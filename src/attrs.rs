//! Attribute registry & validation (design §7/§16; roadmap workstream D).
//!
//! An *attribute* is a declarative compiler directive — `@inline`, `@packed`,
//! `@deprecated("…")`. The governing rule (from the language's design notes) is:
//!
//! > Attributes may guide compilation, ABI, optimization, verification, and
//! > tooling — but they must never *silently rewrite program behavior*.
//!
//! This module is the **single source of truth** for which attributes exist,
//! where each may be written, and what arguments it takes. Everything that is
//! unknown, misplaced, mis-argued, or contradictory is rejected *here*, so a
//! typo (`@inlien`) or a misapplied directive (`@packed fn`) can never quietly
//! become a no-op. Inspiration: Rust attributes (compiler-visible metadata),
//! Ada/SPARK aspects (contracts stay real syntax, *not* attributes), and D/C#
//! structured metadata — minus the runtime magic of Python-style decorators.
//!
//! The validator runs in the parser (where every item's attributes are still in
//! hand, before enums/consts/externs discard theirs) and feeds the ordinary
//! diagnostic stream. See [`validate`] and [`validate_fn`].

use crate::ast::{Ast, Attribute, ExprKind, FnDecl, TypeKind};
use crate::diag::Diagnostic;

/// Where an attribute is written — used to reject misplaced attributes with a
/// message that names the wrong host ("…cannot be applied to an enum").
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Target {
    /// A free (top-level) function.
    Fn,
    /// A function declared inside a `struct` body.
    Method,
    Struct,
    /// A `struct` field (`x: @volatile u32`).
    Field,
    Enum,
    Const,
    /// An `extern "c"` declaration.
    Extern,
}

impl Target {
    fn describe(self) -> &'static str {
        match self {
            Target::Fn => "a function",
            Target::Method => "a method",
            Target::Struct => "a struct",
            Target::Field => "a struct field",
            Target::Enum => "an enum",
            Target::Const => "a const",
            Target::Extern => "an extern declaration",
        }
    }
}

/// The argument shape an attribute accepts.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Args {
    /// No arguments — `@inline`.
    None,
    /// Exactly one integer literal, required to be a positive power of two —
    /// `@align(8)`.
    Pow2,
    /// Exactly one bare identifier — `@layout(c)`.
    Word,
    /// Exactly one string literal — `@section(".boot")`.
    Str,
    /// An optional single string message — `@deprecated` or `@deprecated("…")`.
    OptStr,
}

/// Whether an attribute is wired up, or merely reserved for a future feature.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Status {
    /// Implemented end-to-end (parsed, validated, and lowered).
    Active,
    /// A recognized keyword reserved for a feature that does not exist yet. Using
    /// it is an *error* (with the carried explanation) rather than a silent no-op
    /// — promising `@verified` while quietly not verifying would be unsafe.
    Reserved(&'static str),
}

/// One row of the attribute registry.
struct Spec {
    name: &'static str,
    targets: &'static [Target],
    args: Args,
    status: Status,
}

/// The complete, closed set of Jestyr attributes. Anything not listed here is an
/// "unknown attribute" error. Grouped by concern, mirroring the design's intent
/// that attributes describe *compilation / ABI / tooling / verification*.
const SPECS: &[Spec] = &[
    // ── memory layout & ABI (structs; design §16) ──────────────────────────
    Spec { name: "packed", targets: &[Target::Struct], args: Args::None, status: Status::Active },
    Spec { name: "align", targets: &[Target::Struct], args: Args::Pow2, status: Status::Active },
    Spec { name: "layout", targets: &[Target::Struct], args: Args::Word, status: Status::Active },
    // opt-in `Copy` for a small aggregate (design §2.8): freely copied, never moves.
    Spec { name: "copy", targets: &[Target::Struct], args: Args::None, status: Status::Active },
    // ── bare-metal field qualifier (design §16) ────────────────────────────
    Spec { name: "volatile", targets: &[Target::Field], args: Args::None, status: Status::Active },
    // ── safety / verification (functions) ──────────────────────────────────
    Spec {
        name: "no_panic",
        targets: &[Target::Fn, Target::Method],
        args: Args::None,
        status: Status::Active,
    },
    // `@no_alloc` (design Phase 3) — the escape checker must *prove* the body does
    // no allocation (heap or arena), else it is a compile error. The enforced
    // contract for real-time / embedded / kernel paths; the `@no_panic` analog.
    Spec {
        name: "no_alloc",
        targets: &[Target::Fn, Target::Method],
        args: Args::None,
        status: Status::Active,
    },
    // `@deterministic` (design §10 / Phase 3) — the escape checker certifies the
    // function's result is **schedule-independent**: it forbids the raw concurrency
    // primitives whose result can depend on the thread schedule (`concurrent`/`spawn`,
    // the `atomic_*` ops), permitting parallelism only through the *checked*
    // deterministic `par for … reduce(r)`. The Ada/Ravenscar provable-subset idea
    // fused with the determinism thesis (the `@verified` tie-in). (The complementary
    // allocator-determinism facet — "same alloc sequence ⇒ same slot layout" — can
    // layer onto the same attribute later; both are aspects of "deterministic".)
    Spec {
        name: "deterministic",
        targets: &[Target::Fn, Target::Method],
        args: Args::None,
        status: Status::Active,
    },
    // ── optimization intent (functions) ────────────────────────────────────
    Spec {
        name: "inline",
        targets: &[Target::Fn, Target::Method],
        args: Args::None,
        status: Status::Active,
    },
    Spec {
        name: "no_inline",
        targets: &[Target::Fn, Target::Method],
        args: Args::None,
        status: Status::Active,
    },
    Spec {
        name: "hot",
        targets: &[Target::Fn, Target::Method],
        args: Args::None,
        status: Status::Active,
    },
    Spec {
        name: "cold",
        targets: &[Target::Fn, Target::Method],
        args: Args::None,
        status: Status::Active,
    },
    // ── tooling / API hygiene (functions) ──────────────────────────────────
    Spec {
        name: "must_use",
        targets: &[Target::Fn, Target::Method],
        args: Args::None,
        status: Status::Active,
    },
    Spec {
        name: "deprecated",
        targets: &[Target::Fn, Target::Method],
        args: Args::OptStr,
        status: Status::Active,
    },
    // ── ABI / linker placement (export & bare-metal; design §12/§16) ────────
    // `@no_mangle` is the export counterpart to `extern "c"` import — a bare,
    // stable C symbol; allowed on functions and on `const` globals.
    Spec {
        name: "no_mangle",
        targets: &[Target::Fn, Target::Const],
        args: Args::None,
        status: Status::Active,
    },
    // `@section(".name")` places a function or global in a named linker section
    // (boot code, a special RAM region, …) — the systems/bare-metal use case.
    Spec {
        name: "section",
        targets: &[Target::Fn, Target::Method, Target::Const],
        args: Args::Str,
        status: Status::Active,
    },
    // ── testing & benchmarking (workstream O — `jestyrc test`) ──────────────
    Spec { name: "test", targets: &[Target::Fn], args: Args::None, status: Status::Active },
    Spec { name: "bench", targets: &[Target::Fn], args: Args::None, status: Status::Active },
    // ── reserved: recognized, intentionally not yet implemented ────────────
    Spec {
        name: "verified",
        targets: &[Target::Fn, Target::Method],
        args: Args::None,
        status: Status::Reserved("static verification via SMT (design §7; see MOTLEY.md)"),
    },
    Spec {
        name: "doc_hidden",
        targets: &[Target::Fn, Target::Method, Target::Struct, Target::Enum, Target::Const],
        args: Args::None,
        status: Status::Reserved("the documentation generator (roadmap workstream C)"),
    },
];

/// Pairs of attributes that contradict each other; the second is flagged.
const CONFLICTS: &[(&str, &str)] = &[
    ("inline", "no_inline"),
    ("hot", "cold"),
    // `@inline` lowers to `static inline` (internal linkage); `@no_mangle` asks
    // for an externally linkable symbol. They cannot both hold.
    ("inline", "no_mangle"),
];

/// Validate a run of attributes attached to `target`, pushing one diagnostic per
/// problem onto `diags`. Used for everything except the function-only semantic
/// checks (see [`validate_fn`]).
pub fn validate(ast: &Ast, attrs: &[Attribute], target: Target, diags: &mut Vec<Diagnostic>) {
    for a in attrs {
        match SPECS.iter().find(|s| s.name == a.name) {
            None => {
                let mut d = Diagnostic::new(format!("unknown attribute `@{}`", a.name), a.span);
                if let Some(sug) = did_you_mean(&a.name) {
                    d = d.with_help(format!("did you mean `@{sug}`?"));
                }
                diags.push(d);
            }
            Some(spec) => {
                if let Status::Reserved(feature) = spec.status {
                    diags.push(
                        Diagnostic::new(
                            format!(
                                "attribute `@{}` is reserved and not implemented yet",
                                a.name
                            ),
                            a.span,
                        )
                        .with_help(format!("reserved for {feature}")),
                    );
                    continue;
                }
                if !spec.targets.contains(&target) {
                    diags.push(
                        Diagnostic::new(
                            format!(
                                "attribute `@{}` cannot be applied to {}",
                                a.name,
                                target.describe()
                            ),
                            a.span,
                        )
                        .with_help(format!("`@{}` applies to {}", a.name, targets_of(spec.targets))),
                    );
                    continue;
                }
                check_args(ast, a, spec, diags);
            }
        }
    }
    check_duplicates(attrs, diags);
    check_conflicts(attrs, diags);
}

/// Validate a function's (or method's) attributes: the generic checks in
/// [`validate`], plus two semantic checks that need the signature —
/// `@must_use` requires a return value, and `@no_mangle` cannot be generic
/// (its C name carries no type arguments to mangle).
pub fn validate_fn(ast: &Ast, f: &FnDecl, is_method: bool, diags: &mut Vec<Diagnostic>) {
    let target = if is_method { Target::Method } else { Target::Fn };
    validate(ast, &f.attrs, target, diags);

    if let Some(a) = f.attr("must_use") {
        if f.ret_ty.is_none() && f.errors.is_none() {
            diags.push(
                Diagnostic::new(
                    "`@must_use` on a function with no return value".to_string(),
                    a.span,
                )
                .with_help("the attribute warns when a *result* is ignored; give the function a return type or drop it"),
            );
        }
    }

    if let Some(a) = f.attr("no_mangle") {
        if is_generic(ast, f) {
            diags.push(
                Diagnostic::new(
                    "`@no_mangle` cannot be applied to a generic function".to_string(),
                    a.span,
                )
                .with_help("a generic function has one C symbol per instantiation, so it has no single unmangled name"),
            );
        }
    }

    // A `@test` runs standalone: it takes no arguments and reports pass/fail by
    // returning `bool`. A `@bench` likewise takes no arguments (its body is timed).
    let runtime_params = f.params.iter().filter(|p| !p.comptime && !p.is_self).count();
    if let Some(a) = f.attr("test") {
        if runtime_params != 0 {
            diags.push(test_shape_error("test", "take no parameters", a.span));
        }
        let returns_bool = f.errors.is_none()
            && f.ret_ty.is_some_and(|t| {
                matches!(&ast.type_at(t).kind, TypeKind::Name(n) if n.name == "bool")
            });
        if !returns_bool {
            diags.push(
                Diagnostic::new("a `@test` function must return `bool`".to_string(), a.span)
                    .with_help("return `true` on success, `false` on failure"),
            );
        }
    }
    if let Some(a) = f.attr("bench") {
        if runtime_params != 0 {
            diags.push(test_shape_error("bench", "take no parameters", a.span));
        }
    }
}

fn test_shape_error(attr: &str, must: &str, span: crate::span::Span) -> Diagnostic {
    Diagnostic::new(format!("a `@{attr}` function must {must}"), span)
}

/// Does `f` have a `comptime T: type` parameter (i.e. is it a generic template)?
/// Mirrors `cgen`'s notion of genericity without depending on it.
fn is_generic(ast: &Ast, f: &FnDecl) -> bool {
    f.params.iter().any(|p| {
        p.comptime && p.ty.is_some_and(|t| matches!(ast.type_at(t).kind, TypeKind::TypeKw))
    })
}

fn check_args(ast: &Ast, a: &Attribute, spec: &Spec, diags: &mut Vec<Diagnostic>) {
    let kind = a.args.first().map(|id| &ast.expr_at(*id).kind);
    match spec.args {
        Args::None => {
            if !a.args.is_empty() {
                diags.push(Diagnostic::new(
                    format!("attribute `@{}` takes no arguments", a.name),
                    a.span,
                ));
            }
        }
        Args::Pow2 => {
            match kind {
                Some(ExprKind::Int(lit)) if a.args.len() == 1 => match parse_int_literal(lit) {
                    Some(n) if n > 0 && n.is_power_of_two() => {}
                    _ => diags.push(
                        Diagnostic::new(
                            format!("attribute `@{}` expects a positive power of two", a.name),
                            a.span,
                        )
                        .with_help("alignment must be 1, 2, 4, 8, 16, …"),
                    ),
                },
                _ => diags.push(
                    Diagnostic::new(
                        format!("attribute `@{}` expects a single integer", a.name),
                        a.span,
                    )
                    .with_help(format!("e.g. `@{}(8)`", a.name)),
                ),
            }
        }
        Args::Str => {
            if a.args.len() != 1 || !matches!(kind, Some(ExprKind::Str(_))) {
                diags.push(
                    Diagnostic::new(
                        format!("attribute `@{}` expects a single string", a.name),
                        a.span,
                    )
                    .with_help(format!("e.g. `@{}(\".boot\")`", a.name)),
                );
            }
        }
        Args::Word => {
            if a.args.len() != 1 || !matches!(kind, Some(ExprKind::Name(_))) {
                diags.push(
                    Diagnostic::new(
                        format!("attribute `@{}` expects a single identifier", a.name),
                        a.span,
                    )
                    .with_help(format!("e.g. `@{}(c)`", a.name)),
                );
            }
        }
        Args::OptStr => {
            if a.args.len() > 1 || (a.args.len() == 1 && !matches!(kind, Some(ExprKind::Str(_)))) {
                diags.push(
                    Diagnostic::new(
                        format!("attribute `@{}` takes an optional string message", a.name),
                        a.span,
                    )
                    .with_help(format!("e.g. `@{}` or `@{}(\"use parse_v2\")`", a.name, a.name)),
                );
            }
        }
    }
}

/// Flag a repeated attribute (`@inline @inline`) — always a mistake, and for the
/// lowering it would otherwise emit a redundant duplicate GNU clause.
fn check_duplicates(attrs: &[Attribute], diags: &mut Vec<Diagnostic>) {
    for (i, a) in attrs.iter().enumerate() {
        if attrs[..i].iter().any(|b| b.name == a.name) {
            diags.push(Diagnostic::new(
                format!("duplicate attribute `@{}`", a.name),
                a.span,
            ));
        }
    }
}

fn check_conflicts(attrs: &[Attribute], diags: &mut Vec<Diagnostic>) {
    let has = |n: &str| attrs.iter().any(|a| a.name == n);
    for (first, second) in CONFLICTS {
        if has(first) && has(second) {
            if let Some(a) = attrs.iter().find(|a| a.name == *second) {
                diags.push(Diagnostic::new(
                    format!("conflicting attributes `@{first}` and `@{second}`"),
                    a.span,
                ));
            }
        }
    }
}

/// Suggest the closest known attribute name within edit distance 2 (so `@inlien`
/// → `inline`, `@repr` → no suggestion unless close). Reserved names are
/// suggestible too — better to point at `@verified` than leave a typo dangling.
fn did_you_mean(name: &str) -> Option<&'static str> {
    let mut best: Option<(&'static str, usize)> = None;
    for spec in SPECS {
        let d = levenshtein(name, spec.name);
        if d <= 2 && best.map(|(_, bd)| d < bd).unwrap_or(true) {
            best = Some((spec.name, d));
        }
    }
    best.map(|(n, _)| n)
}

/// A human-readable list of an attribute's legal hosts, for the "applies to …"
/// help line. Deduplicated so `Fn`+`Method` reads as "a function or a method".
fn targets_of(targets: &[Target]) -> String {
    let labels: Vec<&str> = targets.iter().map(|t| t.describe()).collect();
    match labels.as_slice() {
        [] => "nothing".to_string(),
        [one] => one.to_string(),
        [head @ .., last] => format!("{} or {last}", head.join(", ")),
    }
}

/// Classic dynamic-programming Levenshtein edit distance over bytes (attribute
/// names are ASCII), for the "did you mean" suggestion.
fn levenshtein(a: &str, b: &str) -> usize {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut curr = vec![0usize; b.len() + 1];
    for (i, &ca) in a.iter().enumerate() {
        curr[0] = i + 1;
        for (j, &cb) in b.iter().enumerate() {
            let cost = if ca == cb { 0 } else { 1 };
            curr[j + 1] = (prev[j] + cost).min(prev[j + 1] + 1).min(curr[j] + 1);
        }
        std::mem::swap(&mut prev, &mut curr);
    }
    prev[b.len()]
}

/// Parse a Jestyr integer literal (the verbatim source text) into a value —
/// decimal or `0x`/`0o`/`0b` radix, with `_` digit separators. Returns `None`
/// on overflow or a malformed literal (the parser already accepted its lexical
/// shape, so this only guards the radix conversion).
fn parse_int_literal(lit: &str) -> Option<u64> {
    let clean: String = lit.chars().filter(|&c| c != '_').collect();
    let (radix, digits) = match clean.get(..2) {
        Some("0x") | Some("0X") => (16, &clean[2..]),
        Some("0o") | Some("0O") => (8, &clean[2..]),
        Some("0b") | Some("0B") => (2, &clean[2..]),
        _ => (10, clean.as_str()),
    };
    if digits.is_empty() {
        return None;
    }
    u64::from_str_radix(digits, radix).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::Lexer;
    use crate::parser::Parser;

    /// Parse a snippet and return its (already attribute-validated) diagnostics.
    fn diags_of(src: &str) -> Vec<Diagnostic> {
        let (tokens, lex) = Lexer::new(src).tokenize();
        assert!(lex.is_empty(), "lexing failed: {lex:?}");
        let (_ast, _items, parse) = Parser::new(src, tokens).parse_module();
        parse
    }

    fn has_msg(diags: &[Diagnostic], needle: &str) -> bool {
        diags.iter().any(|d| d.message.contains(needle))
    }

    #[test]
    fn unknown_attribute_is_rejected_with_a_suggestion() {
        let d = diags_of("@inlien fn f() {}");
        assert!(has_msg(&d, "unknown attribute `@inlien`"), "{d:?}");
        assert!(d.iter().any(|x| x.help.as_deref() == Some("did you mean `@inline`?")), "{d:?}");
    }

    #[test]
    fn misplaced_attribute_is_rejected() {
        let d = diags_of("@packed fn f() {}");
        assert!(has_msg(&d, "`@packed` cannot be applied to a function"), "{d:?}");
    }

    #[test]
    fn struct_layout_attrs_still_validate_clean() {
        let d = diags_of("@packed @align(8) struct S { x: i32 }");
        assert!(d.is_empty(), "expected no diagnostics, got {d:?}");
    }

    #[test]
    fn align_requires_an_integer() {
        let d = diags_of("@align(c) struct S { x: i32 }");
        assert!(has_msg(&d, "`@align` expects a single integer"), "{d:?}");
    }

    #[test]
    fn conflicting_optimization_hints_are_rejected() {
        let d = diags_of("@inline @no_inline fn f() {}");
        assert!(has_msg(&d, "conflicting attributes `@inline` and `@no_inline`"), "{d:?}");
    }

    #[test]
    fn reserved_attribute_errors_rather_than_silently_passing() {
        let d = diags_of("@verified fn f() {}");
        assert!(has_msg(&d, "`@verified` is reserved"), "{d:?}");
    }

    #[test]
    fn must_use_needs_a_return_value() {
        let d = diags_of("@must_use fn f() {}");
        assert!(has_msg(&d, "`@must_use` on a function with no return value"), "{d:?}");
        // …but it is fine on a function that returns something.
        let ok = diags_of("@must_use fn g() -> i32 { return 1 }");
        assert!(ok.is_empty(), "{ok:?}");
    }

    #[test]
    fn no_mangle_rejects_generics() {
        let d = diags_of("@no_mangle fn id(comptime T: type, x: T) -> T { return x }");
        assert!(has_msg(&d, "`@no_mangle` cannot be applied to a generic function"), "{d:?}");
    }

    #[test]
    fn deprecated_accepts_an_optional_message() {
        assert!(diags_of("@deprecated fn f() {}").is_empty());
        assert!(diags_of("@deprecated(\"use g\") fn f() {}").is_empty());
        let d = diags_of("@deprecated(42) fn f() {}");
        assert!(has_msg(&d, "`@deprecated` takes an optional string message"), "{d:?}");
    }

    #[test]
    fn method_attributes_validate_against_the_method_target() {
        // `@no_mangle` is free-function-only → an error on a method.
        let d = diags_of("struct S { @no_mangle fn m(self) {} }");
        assert!(has_msg(&d, "`@no_mangle` cannot be applied to a method"), "{d:?}");
        // `@inline` is allowed on methods.
        let ok = diags_of("struct S { @inline fn m(self) {} }");
        assert!(ok.is_empty(), "{ok:?}");
    }

    #[test]
    fn duplicate_attribute_is_rejected() {
        let d = diags_of("@inline @inline fn f() {}");
        assert!(has_msg(&d, "duplicate attribute `@inline`"), "{d:?}");
    }

    #[test]
    fn align_must_be_a_power_of_two() {
        assert!(diags_of("@align(16) struct S { x: i32 }").is_empty());
        let d = diags_of("@align(3) struct S { x: i32 }");
        assert!(has_msg(&d, "`@align` expects a positive power of two"), "{d:?}");
        let zero = diags_of("@align(0) struct S { x: i32 }");
        assert!(has_msg(&zero, "`@align` expects a positive power of two"), "{zero:?}");
    }

    #[test]
    fn section_requires_a_string_and_works_on_fn_and_const() {
        assert!(diags_of("@section(\".boot\") fn reset() {}").is_empty());
        assert!(diags_of("@section(\".cfg\") const C: i32 = 1").is_empty());
        let d = diags_of("@section(7) fn f() {}");
        assert!(has_msg(&d, "`@section` expects a single string"), "{d:?}");
    }

    #[test]
    fn no_mangle_is_allowed_on_a_const() {
        assert!(diags_of("@no_mangle const VERSION: i32 = 1").is_empty());
    }

    #[test]
    fn test_attribute_requires_no_params_and_a_bool_return() {
        assert!(diags_of("@test fn t() -> bool { return true }").is_empty());
        let no_ret = diags_of("@test fn t() {}");
        assert!(has_msg(&no_ret, "a `@test` function must return `bool`"), "{no_ret:?}");
        let has_param = diags_of("@test fn t(x: i32) -> bool { return true }");
        assert!(has_msg(&has_param, "a `@test` function must take no parameters"), "{has_param:?}");
    }

    #[test]
    fn bench_attribute_requires_no_params() {
        assert!(diags_of("@bench fn b() {}").is_empty());
        let d = diags_of("@bench fn b(x: i32) {}");
        assert!(has_msg(&d, "a `@bench` function must take no parameters"), "{d:?}");
    }
}
