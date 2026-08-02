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

use crate::ast::{Ast, Attribute, ExprKind, FnDecl, Item, StructMember, TypeKind};
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
    // `@abi(<word>)` (workstream L, increment 3) — how this function's **large
    // read-only aggregate parameters** are passed. `value` is the default (a copy,
    // which is what `read` has always meant physically); `ref` passes them as
    // `const T*` and dereferences at every use, so a 64-byte record crosses the call
    // boundary as one machine word.
    //
    // It is an *ABI* attribute, not an optimization hint: it changes the emitted C
    // signature, so the compiler has to be able to see every call. A function whose
    // address is taken is therefore refused — a `fn(T) -> R` pointer type records no
    // calling convention, so calling through one would pass a copy to a callee
    // expecting a pointer, which C would compile without complaint.
    //
    // **Free functions only, for now.** A method reaches its callee through more paths
    // than a direct call — method-call sugar, a bound method value, a trait vtable, a
    // `dyn` fat pointer — and every one of them would have to agree on the convention.
    // Until they do, `@abi` on a method is refused by the target list rather than
    // emitting a signature some call sites do not match.
    Spec { name: "abi", targets: &[Target::Fn], args: Args::Word, status: Status::Active },
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
    // `@span(<class>)` (Workstream Q — the work-span cost model; Cilk/NESL) — a
    // **checked** asymptotic bound on the function's parallel *span* (depth: its
    // critical-path length, the time on unbounded processors). The compiler computes
    // the body's span from its loop structure — a sequential loop multiplies by `n`,
    // a deterministic `par for … reduce(r)` contributes only `log n` (a parallel tree
    // reduction) — and rejects a body whose span exceeds the declared class. So a
    // refactor that accidentally *serializes* a reduction (span `log n → n`) becomes a
    // diagnostic, not a silent regression. Classes: `constant`, `log`, `linear`,
    // `linearithmic`, `quadratic`. The Motley cost-model tie-in (see MOTLEY.md); the
    // thermal/energy facet can layer onto the same machinery later.
    Spec {
        name: "span",
        targets: &[Target::Fn, Target::Method],
        args: Args::Word,
        status: Status::Active,
    },
    // `@simd` (Workstream Q — the SIMD frontier) — a **checked** declaration that
    // every `par for` in this function may be evaluated a SIMD lane at a time without
    // changing a bit of the answer. The compiler decides legality itself
    // (`crate::simd`) and rejects a body that is not a total, elementwise integer
    // expression, naming the cause. Like `@span`, it is a *contract*, not a switch:
    // it changes no emitted C, so a refactor that quietly makes a hot loop
    // unvectorizable — a call slipped into the body, a division, a float — becomes a
    // diagnostic instead of a silent performance cliff. The lowering increment will
    // vectorize exactly what this attribute already certifies.
    Spec {
        name: "simd",
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

/// Validate the attributes of a struct/record/union whose **body has been parsed**.
///
/// Split from [`validate`] because these checks need the declaration's shape, and the
/// generic run happens at the item keyword, before the body exists. Everything here
/// concerns `@layout(auto)`, whose contract is "the compiler may choose this struct's
/// field order" — a promise that is either meaningless or actively wrong in three
/// declarations, each refused with its own cause rather than silently ignored.
pub fn validate_struct(ast: &Ast, item: &Item, diags: &mut Vec<Diagnostic>) {
    let Item::Struct { body, attrs, is_union, .. } = item else {
        return;
    };
    for a in attrs.iter().filter(|a| a.name == "layout") {
        // The vocabulary is closed (`c` | `auto`). Checking only that the argument *is*
        // an identifier — which is all `Args::Word` does — let `@layout(packd)` validate
        // clean and do nothing, and an attribute that quietly means nothing reads like a
        // guarantee. Same argument as `@simd` on a function with no `par for`.
        let word = match a.args.first().map(|id| &ast.expr_at(*id).kind) {
            Some(ExprKind::Name(n)) => n.name.clone(),
            // A malformed argument was already reported by `check_args`.
            _ => continue,
        };
        if !crate::layout::LAYOUT_WORDS.contains(&word.as_str()) {
            diags.push(
                Diagnostic::new(format!("unknown layout policy `{word}` in `@layout`"), a.span)
                    .with_help(
                        "expected `c` (declaration order, the default) or `auto` (the compiler minimises padding)",
                    ),
            );
            continue;
        }
        if word != "auto" {
            continue;
        }
        if *is_union {
            diags.push(
                Diagnostic::new(
                    "`@layout(auto)` cannot be applied to a union".to_string(),
                    a.span,
                )
                .with_help(
                    "a union's members all start at offset 0, so there is no order to improve — and C treats the first member as the initially-active one, which makes the declared order meaningful",
                ),
            );
        }
        if attrs.iter().any(|o| o.name == "packed") {
            diags.push(
                Diagnostic::new(
                    "`@layout(auto)` conflicts with `@packed`".to_string(),
                    a.span,
                )
                .with_help(
                    "`@packed` means the byte layout you wrote is the byte layout emitted; `@layout(auto)` means the compiler may choose it — pick one",
                ),
            );
        }
        if body.members.iter().any(|m| matches!(m, StructMember::Field { bits: Some(_), .. })) {
            diags.push(
                Diagnostic::new(
                    "`@layout(auto)` cannot be applied to a struct with bit-fields".to_string(),
                    a.span,
                )
                .with_help(
                    "bit-field packing is implementation-defined in C, so the compiler cannot compute the layout it would be improving — and moving a field between storage units is observable",
                ),
            );
        }
    }
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

    if let Some(a) = f.attr("abi") {
        check_abi_contract(ast, f, a, diags);
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

    // `@span(<class>)` — the checked work-span (depth) contract (Workstream Q).
    if let Some(a) = f.attr("span") {
        check_span_contract(ast, f, a, diags);
    }
    // `@simd` — the checked SIMD-legality contract (Workstream Q).
    if let Some(a) = f.attr("simd") {
        check_simd_contract(ast, f, a, diags);
    }
}

/// Check `@simd`: every `par for` in this function must be vectorizable, and there
/// must be one to vectorize.
///
/// The verdicts come from [`crate::simd`], the *single* classifier `jestyrc simd`
/// also reports through — so the attribute and the report can never disagree about a
/// loop. The empty case is an error rather than a silent success for the same reason
/// `@span` is worth having: an attribute that quietly means nothing is worse than no
/// attribute, because it reads like a guarantee.
fn check_simd_contract(ast: &Ast, f: &FnDecl, a: &Attribute, diags: &mut Vec<Diagnostic>) {
    let sites = crate::simd::sites_in_span(ast, f.body.span);
    if sites.is_empty() {
        diags.push(
            Diagnostic::new(
                "`@simd` is declared but this function has no `par for` loop".to_string(),
                a.span,
            )
            .with_help(
                "`@simd` certifies that every `par for … reduce(r)` in the body may be evaluated a lane at a time; with no such loop it promises nothing",
            ),
        );
        return;
    }
    for s in sites {
        let crate::simd::Verdict::Illegal { reason, at } = s.verdict else { continue };
        diags.push(
            Diagnostic::new(
                format!(
                    "`@simd` is violated: this `par for` cannot be evaluated a lane at a time because {}",
                    reason.describe()
                ),
                at,
            )
            .with_help(
                "a vectorizable body is a total, elementwise integer expression over the loop variable — no calls, no memory access, no division, no floats; `jestyrc simd <file>` reports every loop's verdict",
            ),
        );
    }
}

// ── `@span` — the work-span (depth) cost model (Workstream Q; Cilk/NESL) ──────────
//
// An asymptotic cost expressed as `n^k · (log n)^j` — enough for the classes a
// structured loop/`par for` body produces. Ordered by growth: compare `k` (the
// polynomial degree, which always dominates) first, then `j` (the log power). A
// sequential loop multiplies a cost by `n` (`k += 1`); a `par for … reduce(r)`
// contributes `log n` (`j = 1, k = 0`) — the parallel tree reduction's depth, NOT the
// `n` a sequential fold would cost. That single difference is what lets the contract
// catch an accidental serialization.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Cost {
    k: u32, // polynomial degree (count of nested sequential loops)
    j: u32, // power of log n (parallel-reduction depth)
}

impl Cost {
    const CONST: Cost = Cost { k: 0, j: 0 };
    const LOG: Cost = Cost { k: 0, j: 1 };
    const LINEAR: Cost = Cost { k: 1, j: 0 };

    /// Asymptotic order: `k` dominates (a polynomial factor beats any log), then `j`.
    fn rank(self) -> (u32, u32) {
        (self.k, self.j)
    }
    /// The larger of two costs (sequential statements / branches — the critical path
    /// is the worst arm; asymptotically the max).
    fn max(self, o: Cost) -> Cost {
        if self.rank() >= o.rank() { self } else { o }
    }
    /// The product of two costs (nesting one inside the other — exponents add).
    fn mul(self, o: Cost) -> Cost {
        Cost { k: self.k + o.k, j: self.j + o.j }
    }
    /// A human-readable class name for diagnostics.
    fn describe(self) -> String {
        match (self.k, self.j) {
            (0, 0) => "constant (O(1))".to_string(),
            (0, 1) => "logarithmic (O(log n))".to_string(),
            (0, _) => format!("O((log n)^{})", self.j),
            (1, 0) => "linear (O(n))".to_string(),
            (1, 1) => "linearithmic (O(n log n))".to_string(),
            (2, 0) => "quadratic (O(n²))".to_string(),
            (k, 0) => format!("O(n^{k})"),
            (k, j) => format!("O(n^{k} (log n)^{j})"),
        }
    }
}

/// Map a `@span(<word>)` class name to its cost, or `None` if unrecognized.
/// The calling conventions `@abi(<word>)` accepts.
///
/// * `value` — the default. A `read` parameter is physically a copy.
/// * `ref` — a large read-only aggregate is passed as `const T*` instead.
pub const ABI_WORDS: &[&str] = &["value", "ref"];

/// Validate `@abi(<word>)`: the vocabulary, and the one rule that keeps it sound.
///
/// **The rule:** `@abi(ref)` changes the emitted C signature, so every call has to be
/// compiled against the same convention. That holds for direct calls, which cgen
/// resolves by name — and fails for an *indirect* one, because a `fn(T) -> R` pointer
/// type carries no convention. Taking the function's address and calling through it
/// would pass a by-value copy to a callee reading a pointer: C compiles it, the program
/// reads a struct as an address.
///
/// So a `@abi(ref)` function whose address is taken is refused. That is checkable
/// syntactically, and cheaply: a *call* mentions the name as a callee, anything else
/// that mentions it takes its address.
///
/// A generic function is refused for the same family of reason — its parameter types
/// are not known until instantiation, so "which parameters are large aggregates" is not
/// a question this pass can answer. (`@no_mangle` is refused on generics too, for the
/// mangling analogue.)
fn check_abi_contract(ast: &Ast, f: &FnDecl, a: &Attribute, diags: &mut Vec<Diagnostic>) {
    let word = match a.args.first().map(|id| &ast.expr_at(*id).kind) {
        Some(ExprKind::Name(n)) => n.name.clone(),
        // A malformed argument was already reported by `check_args`.
        _ => return,
    };
    if !ABI_WORDS.contains(&word.as_str()) {
        diags.push(
            Diagnostic::new(format!("unknown calling convention `{word}` in `@abi`"), a.span)
                .with_help("expected `value` (a copy — the default) or `ref` (large read-only aggregates as `const T*`)"),
        );
        return;
    }
    if word != "ref" {
        return;
    }
    if is_generic(ast, f) {
        diags.push(
            Diagnostic::new("`@abi(ref)` cannot be applied to a generic function".to_string(), a.span)
                .with_help(
                    "which parameters are large aggregates is not known until the function is instantiated",
                ),
        );
    }
    // The address-taken rule needs the whole program (a use can come from a later
    // item than the declaration), so it lives in `validate_program`.
}

/// Whole-program attribute checks — the ones a single item cannot answer.
///
/// Today that is exactly one: an `@abi(ref)` function whose address is taken. The
/// declaration is validated as it is parsed, when the rest of the file does not exist
/// yet, so a `let g = f` three items later would slip past. This runs once, over the
/// finished AST. (The same ordering lesson as `validate_struct`, one scope wider.)
pub fn validate_program(ast: &Ast, diags: &mut Vec<Diagnostic>) {
    // Collected once: every expression that is somebody's callee. A call names the
    // function in *callee* position, so a `Name` that is nobody's callee is an
    // address-taking mention — no per-form walker to keep in sync.
    let mut callees = std::collections::HashSet::new();
    for e in &ast.exprs {
        if let ExprKind::Call { callee, .. } = &e.kind {
            callees.insert(*callee);
        }
    }
    for item in &ast.items {
        let Item::Fn(f) = item else { continue };
        let Some(a) = f.attr("abi") else { continue };
        if !matches!(
            a.args.first().map(|id| &ast.expr_at(*id).kind),
            Some(ExprKind::Name(n)) if n.name == "ref"
        ) {
            continue;
        }
        if let Some(span) = fn_address_taken(ast, &f.name.name, &callees) {
            diags.push(
                Diagnostic::new(
                    format!("`@abi(ref)` function `{}` has its address taken here", f.name.name),
                    span,
                )
                .with_help(
                    "a `fn(…)` pointer type records no calling convention, so an indirect call would pass a copy where a pointer is expected — call it directly, or drop `@abi(ref)`",
                ),
            );
        }
    }
}

/// The span where `name` is mentioned as a **value** rather than called, if anywhere.
///
/// A call names the function in *callee* position (`ExprKind::Call { callee, .. }`), so
/// scanning the flat expression arena for a `Name(name)` that is nobody's callee finds
/// exactly the address-taking mentions — a `let g = f`, an argument `run(f)`, a struct
/// field holding it. No walker to maintain: a new expression form that can hold a
/// function value is covered the moment it stores an `ExprId`.
fn fn_address_taken(
    ast: &Ast,
    name: &str,
    callees: &std::collections::HashSet<crate::ast::ExprId>,
) -> Option<crate::span::Span> {
    for (i, e) in ast.exprs.iter().enumerate() {
        if let ExprKind::Name(n) = &e.kind {
            if n.name == name && !callees.contains(&crate::ast::ExprId(i as u32)) {
                return Some(e.span);
            }
        }
    }
    None
}

fn span_class(word: &str) -> Option<Cost> {
    Some(match word {
        // `constant`, not `const` — the latter is a reserved keyword, so it cannot
        // appear as the attribute's identifier argument.
        "constant" => Cost::CONST,
        "log" => Cost::LOG,
        "linear" => Cost::LINEAR,
        "linearithmic" => Cost { k: 1, j: 1 },
        "quadratic" => Cost { k: 2, j: 0 },
        _ => return None,
    })
}

/// Compute the span of a function body and check it against the declared `@span`.
/// Intraprocedural: a call is treated as O(1) here (its own `@span` is its contract),
/// so the model reasons about *this* body's loop structure — exactly what catches a
/// `par for` quietly rewritten into a sequential `for`.
fn check_span_contract(ast: &Ast, f: &FnDecl, a: &Attribute, diags: &mut Vec<Diagnostic>) {
    // The argument shape (single identifier) was already validated by `check_args`;
    // here we only need its spelling. Bail quietly if it wasn't an identifier.
    let word = match a.args.first().map(|id| &ast.expr_at(*id).kind) {
        Some(ExprKind::Name(n)) => n.name.clone(),
        _ => return,
    };
    let Some(declared) = span_class(&word) else {
        diags.push(
            Diagnostic::new(format!("unknown span class `{word}` in `@span`"), a.span)
                .with_help("expected one of `constant`, `log`, `linear`, `linearithmic`, `quadratic`"),
        );
        return;
    };
    let actual = span_of_block(ast, &f.body);
    if actual.rank() > declared.rank() {
        diags.push(
            Diagnostic::new(
                format!(
                    "`@span({word})` is violated: this function's span is {}, which exceeds the declared {}",
                    actual.describe(),
                    declared.describe()
                ),
                a.span,
            )
            .with_help(
                "a sequential loop over the input has span O(n); a deterministic `par for … reduce(r)` has span O(log n) — did a parallel reduction get serialized?",
            ),
        );
    }
}

/// The span of a block: the max over its statements (the critical path runs them in
/// sequence, so asymptotically the worst one dominates).
fn span_of_block(ast: &Ast, b: &crate::ast::Block) -> Cost {
    let mut c = Cost::CONST;
    for s in &b.stmts {
        c = c.max(span_of_stmt(ast, s));
    }
    c
}

fn span_of_stmt(ast: &Ast, s: &crate::ast::Stmt) -> Cost {
    use crate::ast::Stmt;
    match s {
        Stmt::Let { init: Some(e), .. } => span_of_expr(ast, *e),
        Stmt::Return { value: Some(e), .. } => span_of_expr(ast, *e),
        Stmt::Let { .. } | Stmt::Return { .. } => Cost::CONST,
        Stmt::Expr(e) => span_of_expr(ast, *e),
    }
}

/// The span of an expression. Only the control-flow / loop kinds carry asymptotic
/// cost; everything else is O(1) (a call's cost is the callee's own `@span` contract,
/// not counted here). A sequential `for` multiplies its body by `n`; a `par for …
/// reduce(r)` contributes `log n`.
fn span_of_expr(ast: &Ast, id: crate::ast::ExprId) -> Cost {
    use crate::ast::ForHead;
    match &ast.expr_at(id).kind {
        ExprKind::For { head, body, els, .. } => {
            let mut inner = span_of_block(ast, body);
            if let ForHead::While(c) = head {
                inner = inner.max(span_of_expr(ast, *c));
            }
            // The loop runs its body `n` times in sequence → multiply by `n`.
            let mut c = Cost::LINEAR.mul(inner);
            if let Some(e) = els {
                c = c.max(span_of_block(ast, e));
            }
            c
        }
        ExprKind::ParFor { iter, reduction, body, .. } => {
            // A deterministic parallel reduction: its depth is the merge tree's
            // `log n`, independent of how the per-element `body` maps each element.
            Cost::LOG
                .max(span_of_expr(ast, *iter))
                .max(span_of_expr(ast, *reduction))
                .max(span_of_expr(ast, *body))
        }
        ExprKind::If { cond, then, els } => {
            let mut c = span_of_expr(ast, *cond).max(span_of_block(ast, then));
            if let Some(e) = els {
                c = c.max(span_of_expr(ast, *e));
            }
            c
        }
        ExprKind::Match { scrut, arms } => {
            let mut c = span_of_expr(ast, *scrut);
            for arm in arms {
                c = c.max(span_of_expr(ast, arm.body));
            }
            c
        }
        ExprKind::Block(b) | ExprKind::Unsafe(b) | ExprKind::Concurrent(b) => span_of_block(ast, b),
        ExprKind::Region { body, .. } => span_of_block(ast, body),
        _ => Cost::CONST,
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

    /// Diagnostics from a **whole-program** parse — `Parser::parse`, which populates
    /// `ast.items` and therefore runs [`validate_program`]. `diags_of` uses
    /// `parse_module`, whose items stay separate from the arena, so the cross-item
    /// checks would find nothing there.
    fn program_diags_of(src: &str) -> Vec<Diagnostic> {
        let (tokens, lex) = Lexer::new(src).tokenize();
        assert!(lex.is_empty(), "lexing failed: {lex:?}");
        let (_ast, parse) = Parser::new(src, tokens).parse();
        parse
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

    /// The two policies `@layout` accepts, and nothing else.
    ///
    /// The third case is the one with teeth: before this check, `Args::Word` verified
    /// only that the argument *was* an identifier, so a typo validated clean and did
    /// nothing at all — the failure mode where a user believes they opted into a
    /// guarantee they never got.
    #[test]
    fn layout_accepts_only_its_two_policies() {
        assert!(diags_of("@layout(c) struct S { x: i32 }").is_empty());
        assert!(diags_of("@layout(auto) struct S { a: u8, b: u64 }").is_empty());
        let d = diags_of("@layout(packd) struct S { x: i32 }");
        assert!(has_msg(&d, "unknown layout policy `packd`"), "{d:?}");
    }

    /// `@layout(auto)` is refused where the promise "the compiler may choose the field
    /// order" is either vacuous or wrong. Each cause is named separately, because
    /// "cannot reorder" tells the user nothing about which of their three declarations
    /// is the problem.
    #[test]
    fn layout_auto_is_refused_where_it_could_not_mean_what_it_says() {
        // A union: every member is at offset 0, and C's first-member rule makes the
        // declared order observable.
        let d = diags_of("@layout(auto) union U { a: u8, b: u64 }");
        assert!(has_msg(&d, "cannot be applied to a union"), "{d:?}");
        // `@packed` is the exact opposite promise.
        let d = diags_of("@packed @layout(auto) struct S { a: u8, b: u64 }");
        assert!(has_msg(&d, "conflicts with `@packed`"), "{d:?}");
        // Bit-fields: implementation-defined packing, so the model cannot compute the
        // layout it claims to improve — the same gap `layout::bitfield_types` admits.
        let d = diags_of("@layout(auto) struct S { a: u8 : 1, b: u8 : 3 }");
        assert!(has_msg(&d, "bit-fields"), "{d:?}");
        // …and `@align(n)` is orthogonal: a forced alignment says nothing about order.
        assert!(diags_of("@align(16) @layout(auto) struct S { a: u8, b: u64 }").is_empty());
    }

    /// `@abi` accepts `value` | `ref` and nothing else, and refuses `ref` in the two
    /// places it could not be honoured soundly.
    #[test]
    fn abi_accepts_its_conventions_and_refuses_the_unsound_uses() {
        let big = "struct Big { a: i64, b: i64, c: i64 }\n";
        assert!(diags_of(&format!("{big}@abi(ref) fn f(read v: Big) -> i64 {{ return v.a }}")).is_empty());
        assert!(diags_of(&format!("{big}@abi(value) fn f(read v: Big) -> i64 {{ return v.a }}")).is_empty());
        let d = diags_of(&format!("{big}@abi(pointer) fn f(read v: Big) -> i64 {{ return v.a }}"));
        assert!(has_msg(&d, "unknown calling convention `pointer`"), "{d:?}");
        // Generic: which parameters are large aggregates is unknown until instantiation.
        let d = diags_of("@abi(ref) fn f(comptime T: type, read v: T) -> i64 { return 0 }");
        assert!(has_msg(&d, "cannot be applied to a generic function"), "{d:?}");
        // A method is refused by the target list: it reaches its callee through paths
        // (method sugar, a bound value, a vtable, `dyn`) that do not yet agree on the
        // convention, and a signature some call sites do not match is worse than a
        // "not yet".
        let d = diags_of(&format!("{big}struct S {{ x: i64\n  @abi(ref) fn f(read self, read v: Big) -> i64 {{ return v.a }} }}"));
        assert!(has_msg(&d, "cannot be applied to a method"), "{d:?}");
    }

    /// **The soundness rule.** A `fn(T) -> R` pointer type records no calling
    /// convention, so calling an `@abi(ref)` function through one would pass a copy
    /// where the callee reads a pointer — C compiles that without a word.
    ///
    /// The check has to be **whole-program**: the address may be taken from a later
    /// item than the declaration (or another file entirely), so validating the function
    /// as it is parsed would see an empty arena. That was the first version, and it
    /// silently passed the very program it exists to reject.
    #[test]
    fn an_abi_ref_function_may_not_have_its_address_taken() {
        let big = "struct Big { a: i64, b: i64, c: i64 }\n";
        // Taken by a later item — the ordering that broke the first implementation.
        let d = program_diags_of(&format!(
            "{big}@abi(ref) fn total(read v: Big) -> i64 {{ return v.a }}\n\
             fn main() -> i32 {{ let g: fn(Big) -> i64 = total return 0 }}"
        ));
        assert!(has_msg(&d, "has its address taken"), "{d:?}");
        // Passed as an argument — also an address, not a call.
        let d = program_diags_of(&format!(
            "{big}@abi(ref) fn total(read v: Big) -> i64 {{ return v.a }}\n\
             fn run(g: fn(Big) -> i64) -> i64 {{ return 0 }}\n\
             fn main() -> i32 {{ run(total) return 0 }}"
        ));
        assert!(has_msg(&d, "has its address taken"), "{d:?}");
        // …but an ordinary direct call is exactly what the attribute is for.
        let d = program_diags_of(&format!(
            "{big}@abi(ref) fn total(read v: Big) -> i64 {{ return v.a }}\n\
             fn main() -> i32 {{ let b = Big {{ a: 1, b: 2, c: 3 }} print_int(total(b)) return 0 }}"
        ));
        assert!(d.is_empty(), "a direct call must be fine: {d:?}");
        // And a function *without* the attribute may still be used as a value.
        let d = program_diags_of(&format!(
            "{big}fn total(read v: Big) -> i64 {{ return v.a }}\n\
             fn main() -> i32 {{ let g: fn(Big) -> i64 = total return 0 }}"
        ));
        assert!(d.is_empty(), "{d:?}");
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
