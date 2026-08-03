//! Proof-obligation extraction (roadmap Correctness tier 5 — the **planning** slice).
//!
//! `@verified` promises static proof. This module does not prove anything; it answers
//! the question that must come first: **what would have to be proven?** It collects the
//! obligations a program already states — preconditions, postconditions, loop invariants
//! and termination measures, and parameter refinements — and renders them as a report
//! (`jestyrc obligations <file>`).
//!
//! ## Why extraction before solving
//! An SMT backend is the single largest unbuilt item on the roadmap, and its cost is
//! dominated by a question nobody has measured: how many obligations does a real Jestyr
//! program actually carry, and of what shapes? A report answers that from the corpus
//! today, so the decision to build (or not build) a solver is made against a number
//! rather than an impression. It is the same discipline `layout.rs` and `simd.rs`
//! opened with — **measure first, change emission never**.
//!
//! It is also useful on its own. "What does this function assume, and what does it
//! promise?" is a code-review question, and the answer is currently scattered across a
//! signature, a loop body, and a parameter list.
//!
//! ## What counts, and what deliberately does not
//! Only **declared** obligations: the ones a human wrote down. The implicit ones — every
//! index is in bounds, every arithmetic op does not overflow — are real proof
//! obligations too, and there are vastly more of them, but they are *discovered* by a
//! pass rather than stated by a programmer. The report counts declared obligations
//! exactly and says plainly that implicit ones are not included, because a number that
//! quietly omitted them would badly under-size the work it exists to size.
//!
//! Zero emission change: this reads the AST and prints. Nothing here can alter a build.

use std::fmt::Write;

use crate::ast::{Ast, Block, ExprId, FnDecl, Item, Stmt, StructMember};
use crate::span::Span;

/// What kind of thing must be proven.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    /// `requires <expr>` — must hold on entry. The **caller's** obligation.
    Precondition,
    /// `ensures <expr>` — must hold on return. The **callee's** obligation.
    Postcondition,
    /// `invariant <expr>` — must hold on entry to every loop iteration.
    Invariant,
    /// `variant <expr>` — must be `>= 0` and strictly decrease each iteration. The
    /// termination obligation, and the only one whose statement is a *pair* of facts.
    Variant,
    /// `i: usize in 0..len` — a refinement narrowing a parameter's domain. An
    /// obligation on every caller, like a precondition, but attached to one argument.
    Refinement,
}

impl Kind {
    pub fn label(self) -> &'static str {
        match self {
            Kind::Precondition => "requires",
            Kind::Postcondition => "ensures",
            Kind::Invariant => "invariant",
            Kind::Variant => "variant",
            Kind::Refinement => "refine",
        }
    }

    /// Who has to discharge it. The split matters for a solver: a precondition is
    /// assumed inside the body and proven at each call site, and a postcondition is
    /// the reverse.
    pub fn owner(self) -> &'static str {
        match self {
            Kind::Precondition | Kind::Refinement => "caller",
            Kind::Postcondition | Kind::Invariant | Kind::Variant => "callee",
        }
    }
}

/// One thing that would have to be proven.
#[derive(Clone, Debug)]
pub struct Obligation {
    /// The function it belongs to (a method is rendered `Type.method`).
    pub function: String,
    pub kind: Kind,
    /// The obligation's source text, sliced from the program rather than re-rendered.
    /// A proof obligation is a claim about *what the user wrote*, so echoing their
    /// own words is both exact and reviewable — the rule `doc`/`attest` already follow.
    pub text: String,
    /// Where it was written. Not printed by [`render`] — the report is meant to be
    /// diffed, and line numbers make it churn — but carried because it is the field a
    /// solver needs: an obligation that fails to discharge has to be reported *at the
    /// contract*, exactly as a type error is reported at the expression.
    #[allow(dead_code)]
    pub span: Span,
    /// For a refinement, the parameter it constrains.
    pub subject: Option<String>,
    /// `true` when the enclosing function is `@verified` — i.e. someone has asked for
    /// this to be *proven*, not merely checked at runtime.
    pub demanded: bool,
}

/// Slice an expression's source text, collapsing internal whitespace so a multi-line
/// contract stays one report line.
fn text_of(src: &str, span: Span) -> String {
    let (a, b) = (span.start as usize, span.end as usize);
    let raw = src.get(a..b).unwrap_or("");
    raw.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Collect every declared obligation in `ast`, in source order.
pub fn collect(ast: &Ast, src: &str) -> Vec<Obligation> {
    let mut out = Vec::new();
    for item in &ast.items {
        match item {
            Item::Fn(f) => collect_fn(ast, src, f, &f.name.name, &mut out),
            Item::Struct { name, body, .. } => {
                for m in &body.members {
                    if let StructMember::Method(f) = m {
                        // Qualified, so two types' `len` methods are distinguishable —
                        // a report that merged them would be useless for review.
                        let qual = format!("{}.{}", name.name, f.name.name);
                        collect_fn(ast, src, f, &qual, &mut out);
                    }
                }
            }
            _ => {}
        }
    }
    out
}

fn collect_fn(ast: &Ast, src: &str, f: &FnDecl, name: &str, out: &mut Vec<Obligation>) {
    let demanded = f.has_attr("verified");
    let push = |kind: Kind, id: ExprId, subject: Option<String>, out: &mut Vec<Obligation>| {
        let span = ast.expr_at(id).span;
        out.push(Obligation {
            function: name.to_string(),
            kind,
            text: text_of(src, span),
            span,
            subject,
            demanded,
        });
    };
    // Refinements first: they constrain the inputs the rest is stated about.
    for p in &f.params {
        if let Some(r) = p.refine {
            push(Kind::Refinement, r, Some(p.name.name.clone()), out);
        }
    }
    for r in &f.requires {
        push(Kind::Precondition, *r, None, out);
    }
    for e in &f.ensures {
        push(Kind::Postcondition, *e, None, out);
    }
    collect_block(ast, src, &f.body, name, demanded, out);
}

/// Walk a body for loop contracts. They can nest arbitrarily (a loop inside a loop
/// inside an `if`), so this recurses through every statement form that carries a block
/// rather than scanning the top level.
fn collect_block(
    ast: &Ast,
    src: &str,
    b: &Block,
    name: &str,
    demanded: bool,
    out: &mut Vec<Obligation>,
) {
    for s in &b.stmts {
        match s {
            Stmt::Expr(e) => collect_expr(ast, src, *e, name, demanded, out),
            Stmt::Let { init, .. } => {
                if let Some(e) = init {
                    collect_expr(ast, src, *e, name, demanded, out);
                }
            }
            Stmt::Return { value, .. } => {
                if let Some(e) = value {
                    collect_expr(ast, src, *e, name, demanded, out);
                }
            }
        }
    }
}

fn collect_expr(
    ast: &Ast,
    src: &str,
    id: ExprId,
    name: &str,
    demanded: bool,
    out: &mut Vec<Obligation>,
) {
    use crate::ast::ExprKind as E;
    let push = |kind: Kind, id: ExprId, out: &mut Vec<Obligation>| {
        let span = ast.expr_at(id).span;
        out.push(Obligation {
            function: name.to_string(),
            kind,
            text: text_of(src, span),
            span,
            subject: None,
            demanded,
        });
    };
    match &ast.expr_at(id).kind {
        E::Invariant(e) => push(Kind::Invariant, *e, out),
        E::Variant(e) => push(Kind::Variant, *e, out),
        // `for` is Jestyr's *unified* loop — infinite, condition ("while"), and
        // iteration are all `ForHead` shapes of one expression, so one arm covers
        // every loop a contract can be attached to.
        E::For { body, els, .. } => {
            collect_block(ast, src, body, name, demanded, out);
            if let Some(e) = els {
                collect_block(ast, src, e, name, demanded, out);
            }
        }
        E::If { then, els, .. } => {
            collect_block(ast, src, then, name, demanded, out);
            if let Some(e) = els {
                collect_expr(ast, src, *e, name, demanded, out);
            }
        }
        E::Block(b) | E::Unsafe(b) | E::Concurrent(b) | E::Region { body: b, .. } => {
            collect_block(ast, src, b, name, demanded, out)
        }
        _ => {}
    }
}

/// Render the report `jestyrc obligations` prints.
///
/// Deterministic and diffable — source order, one line per obligation — so it can be
/// pinned in CI like every other artifact this compiler produces.
pub fn render(obs: &[Obligation]) -> String {
    let mut out = String::new();
    out.push_str("obligations v1\n");
    out.push_str(&format!("total {}\n", obs.len()));

    // A per-kind census: this is the number the SMT sizing decision actually rests on.
    for k in [Kind::Refinement, Kind::Precondition, Kind::Postcondition, Kind::Invariant, Kind::Variant] {
        let n = obs.iter().filter(|o| o.kind == k).count();
        if n > 0 {
            let _ = writeln!(out, "{} {} ({})", k.label(), n, k.owner());
        }
    }
    let demanded = obs.iter().filter(|o| o.demanded).count();
    let _ = writeln!(out, "verified-demanded {demanded}");

    let mut cur = String::new();
    for o in obs {
        if o.function != cur {
            let _ = writeln!(out, "fn {}", o.function);
            cur = o.function.clone();
        }
        match &o.subject {
            Some(s) => {
                let _ = writeln!(out, "  {} {} in {}", o.kind.label(), s, o.text);
            }
            None => {
                let _ = writeln!(out, "  {} {}", o.kind.label(), o.text);
            }
        }
    }
    // Said out loud, every time. A count that silently omitted the implicit
    // obligations would badly under-size the very work this report exists to size.
    out.push_str("note: declared obligations only; implicit ones (bounds, overflow) are not counted\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::Lexer;
    use crate::parser::Parser;

    fn obs_of(src: &str) -> Vec<Obligation> {
        let (tokens, _) = Lexer::new(src).tokenize();
        let (ast, d) = Parser::new(src, tokens).parse();
        assert!(d.iter().all(|x| !x.is_error()), "fixture must parse: {d:?}");
        collect(&ast, src)
    }

    #[test]
    fn collects_a_functions_pre_and_postconditions() {
        let o = obs_of(
            "fn div(a: i32, b: i32) -> i32 requires b != 0 ensures result * b == a { return a / b }",
        );
        assert_eq!(o.len(), 2, "{o:?}");
        assert_eq!(o[0].kind, Kind::Precondition);
        assert_eq!(o[0].text, "b != 0");
        assert_eq!(o[1].kind, Kind::Postcondition);
        assert_eq!(o[1].text, "result * b == a");
        assert!(o.iter().all(|x| x.function == "div"));
    }

    /// The caller/callee split is the one a solver needs: a precondition is *assumed*
    /// inside the body and *proven* at every call site; a postcondition is the reverse.
    #[test]
    fn each_kind_knows_who_must_discharge_it() {
        assert_eq!(Kind::Precondition.owner(), "caller");
        assert_eq!(Kind::Refinement.owner(), "caller");
        assert_eq!(Kind::Postcondition.owner(), "callee");
        assert_eq!(Kind::Invariant.owner(), "callee");
        assert_eq!(Kind::Variant.owner(), "callee");
    }

    /// Loop contracts nest arbitrarily, so the walk has to recurse rather than scan a
    /// function's top level — a `variant` two loops deep is exactly the one a
    /// termination proof needs.
    #[test]
    fn finds_loop_contracts_nested_inside_other_blocks() {
        let o = obs_of(
            "fn f(n: i32) -> i32 { var t: i32 = 0 \
             if n > 0 { \
               for i in 0..n { invariant t >= 0 \
                 for j in 0..n { variant n - j t = t + 1 } } } \
             return t }",
        );
        let kinds: Vec<Kind> = o.iter().map(|x| x.kind).collect();
        assert_eq!(kinds, vec![Kind::Invariant, Kind::Variant], "{o:?}");
        assert_eq!(o[1].text, "n - j");
    }

    #[test]
    fn a_refinement_names_the_parameter_it_constrains() {
        let o = obs_of("fn at(xs: []i32, i: usize in 0..xs.len) -> i32 { return xs[i] }");
        assert_eq!(o.len(), 1, "{o:?}");
        assert_eq!(o[0].kind, Kind::Refinement);
        assert_eq!(o[0].subject.as_deref(), Some("i"));
        assert_eq!(o[0].text, "0..xs.len");
    }

    /// A method is reported as `Type.method`. Two types with a `len` method would
    /// otherwise merge into one heading, which makes the report useless for review.
    #[test]
    fn a_method_is_qualified_by_its_type() {
        let o = obs_of(
            "struct V { n: i32 fn get(read self) -> i32 requires self.n >= 0 { return self.n } }",
        );
        assert_eq!(o.len(), 1, "{o:?}");
        assert_eq!(o[0].function, "V.get");
    }

    /// A multi-line contract collapses to one report line — the report is meant to be
    /// diffed, and a wrapped obligation would produce spurious hunks.
    #[test]
    fn a_multiline_contract_renders_on_one_line() {
        let o = obs_of("fn f(a: i32) -> i32 requires a > 0\n    and a < 100\n { return a }");
        assert_eq!(o.len(), 1, "{o:?}");
        assert_eq!(o[0].text, "a > 0 and a < 100");
    }

    #[test]
    fn the_report_censuses_by_kind_and_admits_what_it_omits() {
        let src = "fn div(a: i32, b: i32) -> i32 requires b != 0 ensures result * b == a { return a / b }";
        let r = render(&obs_of(src));
        assert!(r.starts_with("obligations v1\ntotal 2\n"), "{r}");
        assert!(r.contains("requires 1 (caller)"), "{r}");
        assert!(r.contains("ensures 1 (callee)"), "{r}");
        assert!(r.contains("fn div\n"), "{r}");
        // The omission is stated every time, not just in the docs.
        assert!(r.contains("implicit ones (bounds, overflow) are not counted"), "{r}");
        // Deterministic — it is meant to be pinned.
        for _ in 0..3 {
            assert_eq!(render(&obs_of(src)), r);
        }
    }
}
