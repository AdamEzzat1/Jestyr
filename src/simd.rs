//! SIMD legality analysis (roadmap workstream **Q**, the SIMD → GPU frontier,
//! increment 1).
//!
//! Answers one question about each `par for … reduce(r)` in a program: **may its
//! per-element body be evaluated several lanes at a time without changing a single
//! bit of the answer?** The verdict is reported by `jestyrc simd <file>` and checked
//! by the `@simd` attribute; it changes **no emitted C at all**.
//!
//! ## Why legality before lowering
//! Jestyr's promise is that a parallel result is bit-identical across thread counts,
//! chunk sizes and — the part this workstream adds — **SIMD lane widths**. A vector
//! lowering that is merely *usually* right would retire that promise quietly, and the
//! goldens could not catch it: emitted C changes for every vectorized program at once,
//! invalidating the corpus, the concatenated build, the bootstrap seed and every
//! attested hash in a single commit. So the first increment decides *what is legal*
//! and proves that decision against the real vector backend, and a later one lowers
//! only what this pass already certifies. (Workstream L took the same route: measure
//! the layout, verified against the C compiler, before reordering a single field.)
//!
//! ## The determinism argument, in full
//! Vectorizing a `par for` splits into two independent claims.
//!
//! 1. **The reduction is lane-count independent.** `par for` already accepts only the
//!    declared deterministic reductions (`sum`/`min`/`max`/`xor` over `i64`), whose
//!    combine is associative *and* commutative **exactly** on machine integers — no
//!    rounding, so no reassociation error. A lane-tree reduction of `k` lanes and a
//!    serial fold therefore agree bit-for-bit for every `k`. This is precisely why
//!    floating point is excluded here and not merely discouraged: a horizontal float
//!    add reassociates, and the FP determinism contract
//!    (`-ffp-contract=off -fno-fast-math`) constrains *contraction*, not *order*.
//! 2. **The body is a total, elementwise integer expression.** That is what this
//!    module decides, by a deliberately small whitelist (see [`classify`]).
//!
//! Both together give the property the later lowering must preserve: for a certified
//! loop, scalar and any lane width compute the same bits.
//!
//! ## Two rules worth reading before extending the whitelist
//!
//! **Short-circuit operators are legal only because the sublanguage is total.**
//! Scalar `a and b` skips `b` when `a` is false; a lane blend evaluates both sides in
//! every lane. That is value-preserving *only* if evaluating `b` cannot fault — and
//! here it cannot, because the whitelist already excludes every faulting form (`/`,
//! `%`, indexing, calls). Admit any partial operation and `and`/`or` stop being safe
//! in the same motion. The same reasoning licenses `if`/`else` as a lane select.
//!
//! **No float can depend on the loop variable.** The loop variable is `i64` (the type
//! checker enforces it), and the only ways to reach a float from an integer — a cast
//! or a call — are both rejected. So any float subexpression that could appear in an
//! otherwise-legal body is *loop-invariant*: broadcast once, never reduced, never
//! reassociated. Float literals are rejected anyway, because a conservative pass
//! should not require a chain of reasoning to be sound. This is the pass's answer to
//! working without type information (see below).
//!
//! ## Syntactic on purpose
//! Attributes are validated in the parser, before types exist, so `@simd` must be
//! checkable from the AST alone — exactly the constraint `@span` works under. The
//! whitelist is therefore syntactic, and the argument above is what makes a syntactic
//! whitelist sufficient rather than merely convenient. One consequence is worth
//! stating out loud: this pass is **conservative, never optimistic** — a body it
//! rejects may still be perfectly vectorizable, and a body it accepts is vectorizable
//! for real. Only the second direction is a soundness claim, and only the second
//! direction is tested.

use std::fmt::Write;

use crate::ast::{Ast, BinOp, Block, ExprId, ExprKind, Stmt, UnOp};
use crate::span::Span;

/// Why a `par for` body cannot be evaluated lane-at-a-time. Each variant is a
/// *distinct* rejection cause with its own explanation, so a diagnostic can say what
/// to change rather than "not vectorizable".
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Reason {
    /// A call. Its body is not in view, so nothing here can promise it is pure,
    /// total, or elementwise — and a lane-at-a-time call is not a call at all.
    Call,
    /// A memory access (indexing, a field, a dereference, an assignment). Reading
    /// `ys[i]` per lane is a *gather*, a genuinely different operation with its own
    /// legality question; writing is an effect and lane order is unspecified.
    Memory,
    /// Floating point. Excluded by the FP determinism contract: a lane reduction over
    /// floats reassociates, and `-ffp-contract=off` does not make addition associative.
    Float,
    /// An operation that can fault (`/`, `%`). A lane that a scalar run would never
    /// have reached is still evaluated in a vector, so a division whose divisor is
    /// zero — or `INT64_MIN / -1` — becomes a fault the scalar program never had.
    Trapping,
    /// A shift whose amount is not a literal in `0..=63`. An out-of-range shift is
    /// undefined behaviour, and "undefined in both forms" is not a promise that the
    /// two forms agree.
    ShiftAmount,
    /// A cast. Changing the element type changes the lane width, so a cast splits or
    /// joins vectors instead of mapping lanes one-to-one. Same-width casts could be
    /// admitted later; the general case cannot.
    LaneWidth,
    /// Control flow that is not lane-uniform (`break`, `continue`, `return`, an `if`
    /// with no `else`). Lanes cannot leave a loop independently.
    Control,
    /// A form the whitelist simply does not cover yet. Carries the syntax's name so
    /// the diagnostic can name it.
    Unsupported(&'static str),
}

impl Reason {
    /// The diagnostic sentence for this rejection — the *cause*, phrased so the next
    /// action is obvious.
    pub fn describe(self) -> String {
        match self {
            Reason::Call => "it calls a function (a call is not an elementwise lane operation, \
                             and its purity and totality are not in view here)"
                .to_string(),
            Reason::Memory => "it accesses memory (indexing a slice per lane is a gather, and a \
                               write has no defined lane order)"
                .to_string(),
            Reason::Float => "it involves floating point (a lane reduction over floats \
                              reassociates, which the FP determinism contract forbids)"
                .to_string(),
            Reason::Trapping => "it divides (`/` or `%`), which can fault; a vector evaluates \
                                 every lane, including one a scalar run would have skipped"
                .to_string(),
            Reason::ShiftAmount => "it shifts by an amount that is not a literal in `0..=63`, \
                                    so scalar and lane forms are both undefined rather than equal"
                .to_string(),
            Reason::LaneWidth => "it casts, which changes the element width and so the lane count"
                .to_string(),
            Reason::Control => "it uses control flow that is not lane-uniform (a lane cannot \
                                `break`, `continue` or `return` on its own)"
                .to_string(),
            Reason::Unsupported(what) => format!("{what} is not part of the vectorizable subset"),
        }
    }
}

/// The verdict for one `par for`.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Verdict {
    /// Vectorizable. `ops` counts the lane operations in the body — the report's
    /// density figure, and the `flops` input a later cost model needs.
    Legal { ops: usize },
    /// Not vectorizable, because of `reason` at `at` (the innermost offending node,
    /// so the diagnostic points at the cause and not at the whole loop).
    Illegal { reason: Reason, at: Span },
}

impl Verdict {
    pub fn is_legal(&self) -> bool {
        matches!(self, Verdict::Legal { .. })
    }
}

/// One `par for` site and what this pass concluded about it.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Site {
    /// The `par for` expression itself — how a consumer (cgen) keys a verdict to the
    /// node it must lower.
    pub id: ExprId,
    /// The loop variable's name (what the report calls the loop by).
    pub var: String,
    /// The whole `par for` expression's span.
    pub span: Span,
    pub verdict: Verdict,
}

/// Analyze every `par for` in the program.
///
/// The expression arena is flat, so this finds every site wherever it lives — a free
/// function, a method, a closure — with no walker to keep in step with new syntax.
/// Arena order is source order, so the report is stable and diffable.
pub fn analyze(ast: &Ast) -> Vec<Site> {
    let mut out = Vec::new();
    for (i, e) in ast.exprs.iter().enumerate() {
        if let ExprKind::ParFor { var, body, .. } = &e.kind {
            out.push(Site {
                id: ExprId(i as u32),
                var: var.name.clone(),
                span: ast.exprs[i].span,
                verdict: classify(ast, *body),
            });
        }
    }
    out
}

/// Decide whether one `par for` body is a total, elementwise integer expression.
///
/// **This is the single classifier**: the `@simd` attribute check and the report both
/// call it, so a loop the report calls vectorizable can never be one the attribute
/// rejects. (The same "one implementation, two consumers" rule that keeps the
/// reflected, documented and attested renderings of a type from drifting apart.)
pub fn classify(ast: &Ast, body: ExprId) -> Verdict {
    match classify_expr(ast, body) {
        Ok(ops) => Verdict::Legal { ops },
        Err((reason, at)) => Verdict::Illegal { reason, at },
    }
}

/// `Ok(lane-op count)` or `Err(why, where)`. The op count is what the report shows
/// and what a cost model will consume; the span is the *innermost* offender, because
/// the useful diagnostic points at the division, not at the loop containing it.
fn classify_expr(ast: &Ast, id: ExprId) -> Result<usize, (Reason, Span)> {
    let e = ast.expr_at(id);
    let span = e.span;
    let no = |r: Reason| Err((r, span));
    match &e.kind {
        // Leaves. A name is either the loop variable (one lane element) or something
        // bound outside the loop — and nothing in this subset can assign, so anything
        // bound outside is loop-invariant and simply broadcast.
        ExprKind::Int(_) | ExprKind::Bool(_) | ExprKind::Name(_) => Ok(0),

        ExprKind::Float(_) => no(Reason::Float),
        ExprKind::Str(_) | ExprKind::Char(_) => no(Reason::Unsupported("a string or char")),
        ExprKind::Null => no(Reason::Unsupported("`null`")),

        ExprKind::Unary { op, rhs } => match op {
            UnOp::Neg | UnOp::Not | UnOp::BitNot => Ok(1 + classify_expr(ast, *rhs)?),
            // `&x` takes an address — memory, and per-lane meaningless.
            UnOp::Ref => no(Reason::Memory),
        },

        ExprKind::Binary { op, lhs, rhs } => match op {
            // Division and remainder fault. Every lane of a vector is evaluated, so a
            // lane the scalar program would never have computed can raise SIGFPE.
            BinOp::Div | BinOp::Rem => no(Reason::Trapping),
            // A shift is only equal-by-construction when the amount is in range, and
            // the only amount this pass can *see* is a literal one.
            BinOp::Shl | BinOp::Shr => {
                if !is_small_shift(ast, *rhs) {
                    return Err((Reason::ShiftAmount, ast.expr_at(*rhs).span));
                }
                Ok(1 + classify_expr(ast, *lhs)?)
            }
            // Everything else is a pure lane operation. `and`/`or` included — see the
            // module docs: blending both sides is value-preserving precisely because
            // every form reachable here is total.
            _ => Ok(1 + classify_expr(ast, *lhs)? + classify_expr(ast, *rhs)?),
        },

        // `if c { a } else { b }` is a lane select: both sides are evaluated and
        // blended. Legal for the same totality reason as `and`/`or`. Without an
        // `else` there is no value to blend.
        ExprKind::If { cond, then, els } => {
            let Some(els) = els else { return no(Reason::Control) };
            let c = classify_expr(ast, *cond)?;
            let t = classify_block(ast, then)?;
            let f = classify_expr(ast, *els)?;
            Ok(1 + c + t + f)
        }

        ExprKind::Block(b) => classify_block(ast, b),

        ExprKind::Cast { .. } => no(Reason::LaneWidth),
        ExprKind::Call { .. } => no(Reason::Call),
        ExprKind::Index { .. } => no(Reason::Memory),
        ExprKind::Field { .. } | ExprKind::Deref { .. } | ExprKind::Assign { .. } => {
            no(Reason::Memory)
        }
        ExprKind::Try { .. } => no(Reason::Control),
        ExprKind::Break(_) | ExprKind::Continue(_) => no(Reason::Control),

        ExprKind::Match { .. } => no(Reason::Unsupported("a `match`")),
        ExprKind::For { .. } => no(Reason::Unsupported("a nested loop")),
        ExprKind::ParFor { .. } => no(Reason::Unsupported("a nested `par for`")),
        ExprKind::Unsafe(_) => no(Reason::Unsupported("an `unsafe` block")),
        ExprKind::Concurrent(_) | ExprKind::Spawn(_) | ExprKind::Await(_) => {
            no(Reason::Unsupported("a concurrency form"))
        }
        ExprKind::Select { .. } => no(Reason::Unsupported("a `select`")),
        ExprKind::Region { .. } => no(Reason::Unsupported("a `region`")),
        ExprKind::Closure { .. } => no(Reason::Unsupported("a closure")),
        ExprKind::FString { .. } => no(Reason::Unsupported("an f-string")),
        // A `comptime` block is folded to a literal long before any backend sees it,
        // so it *would* be legal — but the fold happens in the type checker and this
        // pass runs in the parser, where the block is still a block. Rejecting it is
        // the conservative reading, and it costs nothing today.
        ExprKind::Comptime(_) => no(Reason::Unsupported("a `comptime` block (not yet folded here)")),
        _ => no(Reason::Unsupported("this expression")),
    }
}

/// A block is legal when it is a **single tail expression**.
///
/// A `let` would vectorize perfectly well — the vector half can bind a lane temporary
/// inside a statement-expression, and the certified subset has nothing to drop. But a
/// `par for` also needs a **scalar remainder**, and cgen genuinely cannot lower a
/// multi-statement block in value position: that needs drop-safe spilling, which is why
/// it was deferred rather than overlooked.
///
/// So this narrows rather than asking the backend to move — the opposite resolution from
/// the value-position `if`, and deliberately so. There the refusal was *incidental* (`?:`
/// lowers a select with no drop question, so cgen was fixed); here it is *principled*.
/// **Fix the backend when its refusal is incidental; narrow the pass when it is not** —
/// which is what keeps "conservative, never optimistic" true rather than aspirational.
fn classify_block(ast: &Ast, b: &Block) -> Result<usize, (Reason, Span)> {
    match b.stmts.as_slice() {
        [Stmt::Expr(e)] => classify_expr(ast, *e),
        [] => Err((Reason::Unsupported("an empty block"), b.span)),
        _ => Err((
            Reason::Unsupported("a block with statements (the scalar remainder cannot lower one)"),
            b.span,
        )),
    }
}

/// Is `id` an integer literal in `0..=63` — a shift amount that is defined for `i64`
/// in both the scalar and the vector form?
fn is_small_shift(ast: &Ast, id: ExprId) -> bool {
    let ExprKind::Int(text) = &ast.expr_at(id).kind else { return false };
    // Literal text is kept verbatim (bases and `_` separators survive to cgen), so
    // parse the plain decimal case and refuse anything else rather than guess.
    let cleaned: String = text.chars().filter(|c| *c != '_').collect();
    matches!(cleaned.parse::<u32>(), Ok(n) if n < 64)
}

/// The `par for` sites lexically inside `block`.
///
/// Span containment rather than a bespoke walker: the arena is flat and spans nest, so
/// this needs no maintenance when new expression forms are added — and a form that
/// *contains* a `par for` cannot hide it.
pub fn sites_in_span(ast: &Ast, outer: Span) -> Vec<Site> {
    analyze(ast)
        .into_iter()
        .filter(|s| s.span.start >= outer.start && s.span.end <= outer.end)
        .collect()
}

/// Render the report `jestyrc simd` prints. One line per `par for`, in source order.
pub fn render(src: &str, sites: &[Site]) -> String {
    let mut out = String::new();
    out.push_str("simd v1\n");
    let legal = sites.iter().filter(|s| s.verdict.is_legal()).count();
    let _ = writeln!(out, "par-for {} vectorizable {}", sites.len(), legal);
    for s in sites {
        let lc = crate::span::line_col(src, s.span.start);
        match &s.verdict {
            Verdict::Legal { ops } => {
                let _ = writeln!(out, "{}:{} par for {} vectorizable lanes i64 ops {}", lc.line, lc.col, s.var, ops);
            }
            Verdict::Illegal { reason, at } => {
                let alc = crate::span::line_col(src, at.start);
                let _ = writeln!(out, "{}:{} par for {} scalar-only", lc.line, lc.col, s.var);
                let _ = writeln!(out, "  because {}:{} {}", alc.line, alc.col, reason.describe());
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::Lexer;
    use crate::parser::Parser;

    /// Wrap a `par for` body in a program and classify it. The reduction is spelled
    /// but never inspected here — reduction determinism is the type checker's check
    /// (`DETERMINISTIC_REDUCTIONS`); this pass is about the *body*.
    fn verdict_of(body: &str) -> Verdict {
        let src = format!(
            "fn f(read s: []i64) -> i64 {{\n    return par for x in s reduce(core.sum_reduction()) {{ {body} }}\n}}\n"
        );
        let (tokens, _) = Lexer::new(&src).tokenize();
        let (ast, d) = Parser::new(&src, tokens).parse();
        assert!(d.iter().all(|x| !x.is_error()), "fixture must parse: {d:?}");
        let sites = analyze(&ast);
        assert_eq!(sites.len(), 1, "fixture must hold exactly one `par for`");
        sites.into_iter().next().expect("one site").verdict
    }

    fn reason_of(body: &str) -> Reason {
        match verdict_of(body) {
            Verdict::Illegal { reason, .. } => reason,
            Verdict::Legal { ops } => panic!("expected a rejection, got Legal {{ ops: {ops} }}"),
        }
    }

    fn ops_of(body: &str) -> usize {
        match verdict_of(body) {
            Verdict::Legal { ops } => ops,
            Verdict::Illegal { reason, .. } => panic!("expected Legal, got {reason:?}"),
        }
    }

    #[test]
    fn the_identity_body_is_vectorizable() {
        // The simplest map of all: no operations, one lane element.
        assert_eq!(ops_of("x"), 0);
    }

    #[test]
    fn integer_arithmetic_and_bit_ops_vectorize() {
        assert_eq!(ops_of("x * x"), 1);
        assert_eq!(ops_of("x * x + x"), 2);
        // Bitwise, negation, complement — all lane-for-lane.
        assert_eq!(ops_of("(x & 7) | (0 - x)"), 3);
        assert_eq!(ops_of("~x"), 1);
    }

    #[test]
    fn a_loop_invariant_name_is_a_broadcast() {
        // `k` is bound outside the loop and nothing in this subset can assign, so it
        // is invariant: one broadcast, not a per-lane load.
        let src = "fn f(read s: []i64, read k: i64) -> i64 {\n    return par for x in s reduce(core.sum_reduction()) { x * k }\n}\n";
        let (tokens, _) = Lexer::new(src).tokenize();
        let (ast, _) = Parser::new(src, tokens).parse();
        assert!(analyze(&ast)[0].verdict.is_legal());
    }

    #[test]
    fn an_if_else_is_a_lane_select() {
        // Both arms are evaluated and blended; legal because both arms are total.
        assert!(verdict_of("if x > 0 { x } else { 0 - x }").is_legal());
    }

    #[test]
    fn short_circuit_operators_are_legal_because_the_subset_is_total() {
        // `and` blends rather than skips. Value-preserving only because nothing
        // reachable here can fault — the invariant the whitelist exists to hold.
        assert!(verdict_of("if x > 0 and x < 100 { 1 } else { 0 }").is_legal());
    }

    #[test]
    /// A block is legal only as a single tail expression. A `let` is refused — and the
    /// reason is the **scalar remainder**, not the vector half, which could bind a lane
    /// temporary perfectly well. See `classify_block` for why the pass narrows here while
    /// the value-position `if` was fixed in the backend instead.
    fn a_block_with_statements_is_refused_but_a_tail_is_fine() {
        assert!(verdict_of("{ x * x }").is_legal());
        assert_eq!(
            reason_of("{ let y: i64 = x * x\n y + 1 }"),
            Reason::Unsupported("a block with statements (the scalar remainder cannot lower one)")
        );
    }

    #[test]
    fn division_is_rejected_because_a_masked_lane_would_still_fault() {
        // The rule with teeth: a vector computes every lane, so a divisor that is zero
        // in a lane the scalar run skipped becomes a real fault.
        assert_eq!(reason_of("x / 2"), Reason::Trapping);
        assert_eq!(reason_of("x % 2"), Reason::Trapping);
        // …including when the division hides in an arm the scalar path would not take.
        assert_eq!(reason_of("if x != 0 { 100 / x } else { 0 }"), Reason::Trapping);
    }

    #[test]
    fn a_float_literal_is_rejected() {
        assert_eq!(reason_of("if 1.5 > 0.0 { x } else { 0 }"), Reason::Float);
    }

    /// The float argument the module docs turn on, pinned: no float can be made to
    /// **depend on the loop variable**, because the only two routes from an `i64` to a
    /// float — a cast and a call — are rejected for their own reasons. So a float that
    /// survived into a legal body could only ever be loop-invariant, and a
    /// loop-invariant value is broadcast, never reduced, and so never reassociated.
    /// If either of these ever becomes legal, the float exclusion needs types to hold.
    #[test]
    fn no_float_can_depend_on_the_loop_variable() {
        assert_eq!(reason_of("x as f64 as i64"), Reason::LaneWidth);
        assert_eq!(reason_of("to_float(x)"), Reason::Call);
    }

    #[test]
    fn calls_indexing_and_casts_are_each_rejected_for_their_own_reason() {
        assert_eq!(reason_of("g(x)"), Reason::Call);
        assert_eq!(reason_of("s[0]"), Reason::Memory);
        assert_eq!(reason_of("x as i32 as i64"), Reason::LaneWidth);
    }

    #[test]
    fn an_out_of_range_or_non_literal_shift_is_rejected() {
        assert!(verdict_of("x << 3").is_legal());
        assert_eq!(reason_of("x << 64"), Reason::ShiftAmount);
        // A variable shift amount cannot be checked here, so it is refused, not guessed.
        assert_eq!(reason_of("x << x"), Reason::ShiftAmount);
    }

    #[test]
    fn non_uniform_control_flow_is_rejected() {
        assert_eq!(reason_of("if x > 0 { x }"), Reason::Control);
        // A `return` is refused at the block level now — the block-shape rule catches it
        // before the per-statement rule does, and either way it is not lane-uniform.
        assert!(!verdict_of("{ return x }").is_legal());
    }

    #[test]
    fn the_innermost_offender_is_what_gets_reported() {
        // The diagnostic must point at the division, not at the whole loop — the span
        // is what makes the message actionable.
        let src = "fn f(read s: []i64) -> i64 {\n    return par for x in s reduce(core.sum_reduction()) { x * x + x / 2 }\n}\n";
        let (tokens, _) = Lexer::new(src).tokenize();
        let (ast, _) = Parser::new(src, tokens).parse();
        let Verdict::Illegal { at, .. } = analyze(&ast)[0].verdict.clone() else {
            panic!("expected a rejection")
        };
        assert_eq!(&src[at.start as usize..at.end as usize], "x / 2");
    }

    #[test]
    fn the_report_names_every_loop_and_its_verdict() {
        let src = "fn f(read s: []i64) -> i64 {\n    let a: i64 = par for x in s reduce(core.sum_reduction()) { x * x }\n    let b: i64 = par for y in s reduce(core.max_reduction()) { y / 2 }\n    return a + b\n}\n";
        let (tokens, _) = Lexer::new(src).tokenize();
        let (ast, _) = Parser::new(src, tokens).parse();
        let out = render(src, &analyze(&ast));
        assert!(out.starts_with("simd v1\npar-for 2 vectorizable 1\n"), "{out}");
        assert!(out.contains("par for x vectorizable lanes i64 ops 1"), "{out}");
        assert!(out.contains("par for y scalar-only"), "{out}");
        assert!(out.contains("because"), "{out}");
    }

    #[test]
    fn a_program_with_no_par_for_reports_nothing() {
        let src = "fn f() -> i64 { return 1 }\n";
        let (tokens, _) = Lexer::new(src).tokenize();
        let (ast, _) = Parser::new(src, tokens).parse();
        assert!(analyze(&ast).is_empty());
    }
}
