//! Error-set soundness census (error-payloads **E1** — `docs/error-payloads.md` §6).
//!
//! Error sets are declaration-side decoration today: `Ty::Result(ok)` carries no set,
//! `err(E)` is not checked against the enclosing declared set, and `?` never checks
//! that the caller's set includes the callee's. That is benign while nothing
//! *discriminates* errors — every consumer (`catch`, `unwrap`, traces) is
//! set-agnostic — and becomes load-bearing the moment payload extraction
//! (`catch |e| match e { … }`) needs an exhaustive static set.
//!
//! This module is the measurement that precedes the enforcement, in the shape the
//! unsafe ladder proved (report → contract → migration → diagnostic): it walks a
//! program and classifies every site where a set obligation exists —
//!
//! * **`err(E)`** — is `E` in the enclosing function's declared set?
//! * **`e?`** — is the callee's declared set a subset of the enclosing one?
//! * **`e catch |x| return x`** — the rethrow form propagates exactly as `?` does,
//!   so it carries exactly `?`'s obligation. (Plain `catch` consumes the error and
//!   owes nothing.)
//!
//! Zero emission change: this reads the AST and prints (`jestyrc errsets <file>`).
//!
//! ## Resolution is best-effort, and says so
//! The census resolves a `?`'s callee by name over the same file: free functions
//! exactly, struct methods when every struct's method of that name agrees on its
//! set. A base it cannot resolve (a variable holding a result, an imported
//! function, an ambiguous method name) is counted **unresolved**, never guessed —
//! a census that guessed would mis-size the migration it exists to size.
//! Enforcement (E2/E3) will sit in typeck, where resolution is exact.
//!
//! ## Sites are found by flat-arena scan, not a walker
//! The expression arena is flat and spans nest, so the enclosing function is
//! recovered by span containment (`simd::sites_in_span`'s idiom) — a new
//! expression form added later cannot hide a `?` from this census the way the
//! unsafe census's early walker missed `spawn` and f-string operands. `comptime`
//! bodies are excluded (they belong to the interpreter, which has no fallible
//! calls); closure bodies are included (runtime code like any other), matching
//! the unsafe census's boundary.
//!
//! ## A wart this census surfaces but does not judge
//! The fallible intrinsics (`try_read_file`, `try_from_utf8`) hard-code error tag
//! **1**, and user error tags also start at 1 — so in any program that declares an
//! error set, the intrinsics' `IoError` aliases the first user-declared name.
//! Unobservable today (nothing discriminates); observable the day `match e`
//! lands. Intrinsic sites are counted in their own category so the number is on
//! the record before payloads make it matter.

use std::collections::{BTreeSet, HashMap};
use std::fmt::Write;

use crate::ast::{Ast, ExprId, ExprKind, Item, StructMember};
use crate::span::Span;

/// The fallible intrinsics — resolved by name whatever the call shape
/// (`try_read_file(p)` or `fs.try_read_file(p)`).
const INTRINSICS: [&str; 2] = ["try_read_file", "try_from_utf8"];

/// Which obligation-carrying form a site is.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    /// `err(E)` — membership in the enclosing set.
    Err,
    /// `e?` — inclusion of the callee's set in the enclosing set.
    Try,
    /// `e catch |x| return x` — `?` spelled out; the same inclusion obligation.
    Rethrow,
}

impl Kind {
    fn label(self) -> &'static str {
        match self {
            Kind::Err => "err",
            Kind::Try => "?",
            Kind::Rethrow => "rethrow",
        }
    }
}

/// What the census concluded about one site.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Verdict {
    /// The obligation holds.
    Ok(String),
    /// The obligation is violated — enforcement (E3) would refuse this site.
    Violation(String),
    /// The callee could not be resolved from this file alone; counted, not guessed.
    Unresolved(String),
    /// A fallible intrinsic — its error domain is builtin, not a declared set.
    Intrinsic(String),
}

/// One obligation-carrying site, in source order.
#[derive(Clone, Debug)]
pub struct Site {
    /// The enclosing function (`Type.method` for a method).
    pub function: String,
    pub kind: Kind,
    pub verdict: Verdict,
    #[allow(dead_code)]
    pub span: Span,
}

/// A function that can own obligations: name, declared set (None = infallible),
/// and the body span sites are matched into.
struct Owner {
    name: String,
    set: Option<BTreeSet<String>>,
    body: Span,
}

fn set_of(es: &Option<crate::ast::ErrorSet>) -> Option<BTreeSet<String>> {
    es.as_ref().map(|e| e.names.iter().map(|n| n.name.name.clone()).collect())
}

fn render_set(s: &BTreeSet<String>) -> String {
    let names: Vec<&str> = s.iter().map(String::as_str).collect();
    format!("{{ {} }}", names.join(", "))
}

/// The head name of a call's callee, dug through generic application
/// (`f(i32)(x)` → `f`) and a field tail (`fs.f(x)` / `recv.m(x)` → `f`/`m`).
/// The bool is `true` when the name was a field tail (method or module call).
fn callee_head(ast: &Ast, id: ExprId) -> Option<(String, bool)> {
    match &ast.expr_at(id).kind {
        ExprKind::Name(n) => Some((n.name.clone(), false)),
        ExprKind::Field { name, .. } => Some((name.name.clone(), true)),
        // Generic application: the outer call's callee is itself a call.
        ExprKind::Call { callee, .. } => callee_head(ast, *callee),
        _ => None,
    }
}

/// Collect and classify every obligation-carrying site in `ast`, in source order.
pub fn collect(ast: &Ast) -> Vec<Site> {
    // Owners (free fns + methods), the free-fn map, and the method map. Methods
    // live in three places: a struct ITEM, an `impl` block, and — the one an
    // item-level scan misses — a `struct { … }` EXPRESSION, the comptime-generic
    // factory idiom (`fn Slot(comptime T: type) -> type { return struct { … } }`).
    // The corpus's `method_errors.jtr` and `vec.jtr` are all factory-form.
    let mut owners: Vec<Owner> = Vec::new();
    let mut free: HashMap<String, Option<BTreeSet<String>>> = HashMap::new();
    let mut methods: HashMap<String, Vec<Option<BTreeSet<String>>>> = HashMap::new();
    let push_methods = |body: &crate::ast::StructBody,
                            qual: &str,
                            owners: &mut Vec<Owner>,
                            methods: &mut HashMap<String, Vec<Option<BTreeSet<String>>>>| {
        for m in &body.members {
            if let StructMember::Method(f) = m {
                let set = set_of(&f.errors);
                methods.entry(f.name.name.clone()).or_default().push(set.clone());
                owners.push(Owner {
                    name: format!("{qual}.{}", f.name.name),
                    set,
                    body: f.body.span,
                });
            }
        }
    };
    // `err` is a name before it is a constructor: a user enum variant (the corpus's
    // `Result(T, E) { ok(v: T), err(e: E) }`) or a user function named `err`
    // SHADOWS the error constructor, exactly as cgen resolves variants first. When
    // shadowed, `err(…)` calls are ordinary constructions, not obligation sites.
    let mut err_shadowed = false;
    for item in &ast.items {
        match item {
            Item::Fn(f) => {
                let set = set_of(&f.errors);
                free.entry(f.name.name.clone()).or_insert_with(|| set.clone());
                owners.push(Owner { name: f.name.name.clone(), set, body: f.body.span });
                if f.name.name == "err" {
                    err_shadowed = true;
                }
            }
            Item::Struct { name, body, .. } => {
                push_methods(body, &name.name, &mut owners, &mut methods);
            }
            Item::Impl(im) => {
                // Trait-impl methods cannot be fallible (refused at check time),
                // but their bodies are owners all the same — a `?` inside one must
                // attribute to the method, not fall through to nothing.
                for f in &im.methods {
                    owners.push(Owner {
                        name: format!("{}.{}", im.trait_name.name, f.name.name),
                        set: set_of(&f.errors),
                        body: f.body.span,
                    });
                }
            }
            Item::Enum(e) => {
                if e.variants.iter().any(|v| v.name.name == "err") {
                    err_shadowed = true;
                }
            }
            _ => {}
        }
    }
    // Factory-form methods: `struct { … }` as an expression.
    for e in &ast.exprs {
        if let ExprKind::StructType(body) = &e.kind {
            push_methods(body, "(struct)", &mut owners, &mut methods);
        }
    }

    // `comptime` bodies belong to the interpreter — no runtime error flows there.
    let comptime: Vec<Span> = ast
        .exprs
        .iter()
        .filter_map(|e| match &e.kind {
            ExprKind::Comptime(_) => Some(e.span),
            _ => None,
        })
        .collect();
    let in_comptime =
        |s: Span| comptime.iter().any(|c| s.start >= c.start && s.end <= c.end);

    // Innermost owner whose body contains the site. Owner bodies never nest
    // (items are top-level; methods live inside a struct item, not another fn),
    // but innermost-wins is the robust rule regardless.
    let owner_of = |s: Span| {
        owners
            .iter()
            .filter(|o| s.start >= o.body.start && s.end <= o.body.end)
            .max_by_key(|o| o.body.start)
    };

    // The inclusion check `?` and the rethrow form share.
    let check_inclusion = |callee: &str,
                           callee_set: &Option<BTreeSet<String>>,
                           owner: &Owner|
     -> Verdict {
        let Some(cs) = callee_set else {
            return Verdict::Violation(format!(
                "on a call to `{callee}`, which declares no error set"
            ));
        };
        let Some(os) = &owner.set else {
            // Already a diagnostic elsewhere (`?` outside a fallible function);
            // counted here because it is a set violation all the same.
            return Verdict::Violation(format!(
                "propagates {} from `{callee}` in a function with no declared error set",
                render_set(cs)
            ));
        };
        let missing: BTreeSet<String> = cs.difference(os).cloned().collect();
        if missing.is_empty() {
            Verdict::Ok(format!("propagates {} from `{callee}`", render_set(cs)))
        } else {
            Verdict::Violation(format!(
                "propagates {} from `{callee}` — not declared by the enclosing set {}",
                render_set(&missing),
                render_set(os)
            ))
        }
    };

    // The callee's declared set, resolved by name — or an Unresolved/Intrinsic
    // verdict when this file alone cannot answer.
    let resolve_and_check = |base: ExprId, owner: &Owner| -> Verdict {
        let ExprKind::Call { callee, .. } = &ast.expr_at(base).kind else {
            return Verdict::Unresolved("the base is not a direct call".to_string());
        };
        let Some((head, is_field)) = callee_head(ast, *callee) else {
            return Verdict::Unresolved("the callee has no resolvable name".to_string());
        };
        if INTRINSICS.contains(&head.as_str()) {
            return Verdict::Intrinsic(format!("`{head}` (builtin error domain)"));
        }
        if !is_field {
            if let Some(set) = free.get(&head) {
                return check_inclusion(&head, set, owner);
            }
            return Verdict::Unresolved(format!("unknown callee `{head}`"));
        }
        // A field tail: a method call or a module-qualified call. Methods resolve
        // when every struct's method of that name agrees on its set.
        match methods.get(&head) {
            Some(sets) => {
                let first = &sets[0];
                if sets.iter().all(|s| s == first) {
                    check_inclusion(&head, first, owner)
                } else {
                    Verdict::Unresolved(format!(
                        "method `{head}` is declared with different error sets"
                    ))
                }
            }
            None => Verdict::Unresolved(format!("unknown callee `{head}`")),
        }
    };

    let mut out = Vec::new();
    for e in ast.exprs.iter() {
        if in_comptime(e.span) {
            continue;
        }
        let (kind, verdict) = match &e.kind {
            ExprKind::Try { base } => {
                let Some(o) = owner_of(e.span) else { continue };
                (Kind::Try, resolve_and_check(*base, o))
            }
            ExprKind::Catch { base, rethrow: true, .. } => {
                let Some(o) = owner_of(e.span) else { continue };
                (Kind::Rethrow, resolve_and_check(*base, o))
            }
            ExprKind::Call { callee, args } => {
                let is_err = !err_shadowed
                    && matches!(&ast.expr_at(*callee).kind,
                                ExprKind::Name(n) if n.name == "err");
                if !is_err {
                    continue;
                }
                let Some(o) = owner_of(e.span) else { continue };
                let v = match args.first().map(|a| &ast.expr_at(*a).kind) {
                    Some(ExprKind::Name(n)) => match &o.set {
                        Some(s) if s.contains(&n.name) => {
                            Verdict::Ok(format!("err({})", n.name))
                        }
                        Some(s) => Verdict::Violation(format!(
                            "err({}) — `{}` is not in the enclosing declared set {}",
                            n.name,
                            n.name,
                            render_set(s)
                        )),
                        None => Verdict::Violation(format!(
                            "err({}) in a function with no declared error set",
                            n.name
                        )),
                    },
                    // Today an error is always a bare name; anything else (a
                    // future `err(Parse(n))`, a computed value) is not guessed.
                    _ => Verdict::Unresolved("`err` with a non-name argument".to_string()),
                };
                (Kind::Err, v)
            }
            _ => continue,
        };
        let function = owner_of(e.span).map(|o| o.name.clone()).unwrap_or_default();
        out.push(Site { function, kind, verdict, span: e.span });
    }
    out
}

/// How many functions declare an error set (the census's denominator).
pub fn fallible_fn_count(ast: &Ast) -> usize {
    let mut n = 0;
    for item in &ast.items {
        match item {
            Item::Fn(f) if f.errors.is_some() => n += 1,
            Item::Struct { body, .. } => {
                for m in &body.members {
                    if let StructMember::Method(f) = m {
                        if f.errors.is_some() {
                            n += 1;
                        }
                    }
                }
            }
            _ => {}
        }
    }
    n
}

/// Render the report `jestyrc errsets` prints. Deterministic and diffable —
/// source order, one line per site — like every report this compiler produces.
pub fn render(ast: &Ast, sites: &[Site]) -> String {
    let mut out = String::new();
    out.push_str("error-sets v1\n");
    let _ = writeln!(out, "fallible-fns {}", fallible_fn_count(ast));
    let count = |k: Kind| sites.iter().filter(|s| s.kind == k).count();
    let _ = writeln!(
        out,
        "sites {} (err {}, ? {}, rethrow {})",
        sites.len(),
        count(Kind::Err),
        count(Kind::Try),
        count(Kind::Rethrow)
    );
    let violations =
        sites.iter().filter(|s| matches!(s.verdict, Verdict::Violation(_))).count();
    let unresolved =
        sites.iter().filter(|s| matches!(s.verdict, Verdict::Unresolved(_))).count();
    let intrinsic =
        sites.iter().filter(|s| matches!(s.verdict, Verdict::Intrinsic(_))).count();
    let _ = writeln!(out, "violations {violations}");
    let _ = writeln!(out, "unresolved {unresolved}");
    let _ = writeln!(out, "intrinsic {intrinsic}");
    let mut cur = String::new();
    for s in sites {
        if s.function != cur {
            let _ = writeln!(out, "fn {}", s.function);
            cur = s.function.clone();
        }
        let (tag, detail) = match &s.verdict {
            Verdict::Ok(d) => ("ok", d),
            Verdict::Violation(d) => ("VIOLATION", d),
            Verdict::Unresolved(d) => ("unresolved", d),
            Verdict::Intrinsic(d) => ("intrinsic", d),
        };
        let _ = writeln!(out, "  {} `{}` {}", tag, s.kind.label(), detail);
    }
    out.push_str(
        "note: census only, nothing is enforced yet (error-payloads E1 — docs/error-payloads.md)\n",
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::Lexer;
    use crate::parser::Parser;

    fn sites_of(src: &str) -> (crate::ast::Ast, Vec<Site>) {
        let (tokens, ld) = Lexer::new(src).tokenize();
        assert!(ld.iter().all(|d| !d.is_error()), "fixture must lex: {ld:?}");
        let (ast, pd) = Parser::new(src, tokens).parse();
        assert!(pd.iter().all(|d| !d.is_error()), "fixture must parse: {pd:?}");
        let s = collect(&ast);
        (ast, s)
    }

    #[test]
    fn err_in_the_declared_set_is_ok_and_outside_it_is_a_violation() {
        let (_, s) = sites_of(
            "fn f(b: i32) -> i32 !{ Io, Parse } { \
               if b == 0 { return err(Io) } \
               if b == 1 { return err(Missing) } \
               return ok(b) }",
        );
        assert_eq!(s.len(), 2, "{s:?}");
        assert!(matches!(&s[0].verdict, Verdict::Ok(d) if d == "err(Io)"), "{s:?}");
        assert!(
            matches!(&s[1].verdict, Verdict::Violation(d)
                     if d.contains("`Missing` is not in the enclosing declared set { Io, Parse }")),
            "{s:?}"
        );
        assert!(s.iter().all(|x| x.kind == Kind::Err && x.function == "f"));
    }

    #[test]
    fn try_inclusion_holds_for_a_subset_and_fails_for_an_undeclared_name() {
        let src = "fn inner(a: i32) -> i32 !{ Io } { return ok(a) } \
                   fn wide(a: i32) -> i32 !{ Io, Parse } { let v = inner(a)? return ok(v) } \
                   fn narrow(a: i32) -> i32 !{ Parse } { let v = inner(a)? return ok(v) }";
        let (_, s) = sites_of(src);
        assert_eq!(s.len(), 2, "{s:?}");
        assert!(
            matches!(&s[0].verdict, Verdict::Ok(d) if d.contains("propagates { Io } from `inner`")),
            "{s:?}"
        );
        assert!(
            matches!(&s[1].verdict, Verdict::Violation(d)
                     if d.contains("propagates { Io }") && d.contains("{ Parse }")),
            "{s:?}"
        );
        assert_eq!(s[1].function, "narrow");
    }

    /// `catch |e| return e` is `?` spelled out, so it carries `?`'s obligation —
    /// the census would under-count the migration if it saw only the `?` spelling.
    #[test]
    fn the_rethrow_form_carries_the_same_obligation_as_try() {
        let src = "fn inner(a: i32) -> i32 !{ Io } { return ok(a) } \
                   fn outer(a: i32) -> i32 !{ Parse } { \
                     let v: i32 = inner(a) catch |e| return e return ok(v) }";
        let (_, s) = sites_of(src);
        assert_eq!(s.len(), 1, "{s:?}");
        assert_eq!(s[0].kind, Kind::Rethrow);
        assert!(matches!(&s[0].verdict, Verdict::Violation(_)), "{s:?}");
    }

    /// A plain `catch` (with or without a binder that is not rethrown) CONSUMES the
    /// error — it owes nothing, and counting it would inflate the census.
    #[test]
    fn a_recovering_catch_is_not_a_site() {
        let src = "fn inner(a: i32) -> i32 !{ Io } { return ok(a) } \
                   fn f(a: i32) -> i32 { let v: i32 = inner(a) catch 0 \
                     let w: i32 = inner(a) catch |e| (e as i64) as i32 return v + w }";
        let (_, s) = sites_of(src);
        assert!(s.is_empty(), "{s:?}");
    }

    #[test]
    fn a_method_callee_resolves_when_unambiguous() {
        let src = "struct A { n: i32 \
                     fn get(read self) -> i32 !{ Empty } { return ok(self.n) } } \
                   fn f(read a: A) -> i32 !{ Empty } { let v = a.get()? return ok(v) } \
                   fn g(read a: A) -> i32 !{ Io } { let v = a.get()? return ok(v) }";
        let (_, s) = sites_of(src);
        assert_eq!(s.len(), 2, "{s:?}");
        assert!(matches!(&s[0].verdict, Verdict::Ok(_)), "{s:?}");
        assert!(
            matches!(&s[1].verdict, Verdict::Violation(d) if d.contains("`get`")),
            "{s:?}"
        );
    }

    #[test]
    fn an_unresolvable_base_is_counted_not_guessed() {
        // `r` is a variable holding a result — this census does not track values.
        let src = "fn inner(a: i32) -> i32 !{ Io } { return ok(a) } \
                   fn f(a: i32) -> i32 !{ Io } { let r = inner(a) let v = r? return ok(v) }";
        let (_, s) = sites_of(src);
        assert_eq!(s.len(), 1, "{s:?}");
        assert!(
            matches!(&s[0].verdict, Verdict::Unresolved(d) if d.contains("not a direct call")),
            "{s:?}"
        );
    }

    #[test]
    fn the_fallible_intrinsics_are_their_own_category() {
        let src = "import \"fs\"\n\
                   fn f(p: str) -> i32 !{ IoError } { let t = fs.try_read_file(p)? return ok(1) }";
        let (_, s) = sites_of(src);
        assert_eq!(s.len(), 1, "{s:?}");
        assert!(
            matches!(&s[0].verdict, Verdict::Intrinsic(d) if d.contains("try_read_file")),
            "{s:?}"
        );
    }

    /// `comptime` bodies belong to the interpreter — a call spelled `err(…)` in one
    /// is not a runtime error site (the same boundary the unsafe census draws).
    /// The fixture keeps a REAL site outside the block so the assertion is sharp:
    /// exclusion, not a broken walk.
    #[test]
    fn comptime_bodies_are_excluded() {
        let src = "fn f(a: i32) -> i32 !{ Io } { \
                     let n: i32 = comptime { err(3) } \
                     if a == 0 { return err(Io) } \
                     return ok(a + n) }";
        let (_, s) = sites_of(src);
        assert_eq!(s.len(), 1, "{s:?}");
        assert!(matches!(&s[0].verdict, Verdict::Ok(d) if d == "err(Io)"), "{s:?}");
    }

    /// A user enum variant named `err` (the corpus's `Result(T, E)`) shadows the
    /// error constructor — its constructions are not obligation sites. Without this
    /// rule the census reported 16 false violations in `core.jtr` alone.
    #[test]
    fn a_user_err_variant_shadows_the_error_constructor() {
        let src = "enum R(T, E) { ok(v: T), err(e: E) } \
                   enum ParseErr { empty, overflow } \
                   fn f(a: i32) -> i32 { \
                     if a == 0 { return 0 } \
                     return 1 } \
                   fn g(a: i32) -> i32 { let r = err(3) return a }";
        let (_, s) = sites_of(src);
        assert!(s.is_empty(), "{s:?}");
    }

    /// The comptime-generic factory idiom: methods declared inside a `struct { … }`
    /// EXPRESSION (returned from a `comptime T: type` fn) are owners and resolve as
    /// callees — `method_errors.jtr` and `vec.jtr` are this form.
    #[test]
    fn factory_struct_methods_are_owners_and_resolve() {
        let src = "fn Slot(comptime T: type) -> type { \
                     return struct { \
                       full: bool \
                       v: T \
                       fn get(read self) -> T !{ Empty } { \
                         if !self.full { return err(Empty) } \
                         return ok(self.v) } } } \
                   fn f(s: Slot(i32)) -> i32 !{ Empty } { let v = s.get()? return ok(v) }";
        let (_, s) = sites_of(src);
        assert_eq!(s.len(), 2, "{s:?}");
        assert_eq!(s[0].function, "(struct).get", "{s:?}");
        assert!(matches!(&s[0].verdict, Verdict::Ok(d) if d == "err(Empty)"), "{s:?}");
        assert!(
            matches!(&s[1].verdict, Verdict::Ok(d) if d.contains("propagates { Empty } from `get`")),
            "{s:?}"
        );
    }

    #[test]
    fn the_report_is_deterministic_and_censuses_by_verdict() {
        let src = "fn inner(a: i32) -> i32 !{ Io } { return ok(a) } \
                   fn f(a: i32) -> i32 !{ Parse } { \
                     if a == 0 { return err(Nope) } \
                     let v = inner(a)? return ok(v) }";
        let (ast, s) = sites_of(src);
        let r = render(&ast, &s);
        assert!(r.starts_with("error-sets v1\nfallible-fns 2\n"), "{r}");
        assert!(r.contains("sites 2 (err 1, ? 1, rethrow 0)"), "{r}");
        assert!(r.contains("violations 2"), "{r}");
        assert!(r.contains("fn f\n"), "{r}");
        assert!(r.contains("nothing is enforced yet"), "{r}");
        for _ in 0..3 {
            let (ast2, s2) = sites_of(src);
            assert_eq!(render(&ast2, &s2), r);
        }
    }
}
