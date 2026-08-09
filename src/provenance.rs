//! The unsafe-boundary report (ownership roadmap **v2** — unsafe/provenance, slice 1).
//!
//! `jestyrc unsafe <file>` lists every **raw-pointer operation** in a program and
//! whether an `unsafe` block covers it. Analysis only — nothing here can change a
//! build — because the first honest step of the v2 model is a *measurement*, not an
//! enforcement.
//!
//! ## Why measure before enforcing
//! Today `unsafe { … }` exists in the surface language and gates **nothing**: a raw
//! deref compiles identically inside and outside it, so the keyword is documentation
//! the compiler never checks. The obvious fix — "require `unsafe` around every raw
//! deref" — has a cost nobody had counted: the self-hosted compiler and much of the
//! corpus deref raw pointers in ordinary code, so enforcement without a migration
//! breaks the self-host build on day one. This report produces the number (how many
//! uncovered sites, where), exactly as `jestyrc obligations` sized the SMT question
//! and `jestyrc layout` sized reordering.
//!
//! ## What counts as a raw-pointer operation
//! * a **deref** (`p.*`), read or write — the operation whose validity the contract
//!   is about;
//! * **pointer arithmetic** (`p + i`), which manufactures a new address whose
//!   validity is the arithmetic's precondition;
//! * an **int-to-pointer cast** (`0x1000 as *mut u32`), which manufactures
//!   *provenance* itself — the MMIO door, and the operation with the fewest
//!   guarantees of all.
//!
//! Taking a pointer's value, passing it, storing it, comparing it: not counted.
//! Holding an address is safe; the contract is entirely about *using* one.
//!
//! The written contract this report serves is `docs/unsafe-contract.md`.

use std::fmt::Write;

use crate::ast::{Ast, BinOp, Block, ExprId, ExprKind, FnDecl, Item, Stmt, StructMember};
use crate::span::Span;
use crate::types::{Ty, TypeInfo};

/// The kind of raw-pointer operation at a site.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Op {
    /// `p.*` — read or write through a raw pointer.
    Deref,
    /// `p + i` / `p - i` — address arithmetic on a raw pointer.
    Arith,
    /// `<int> as *T` — manufacturing provenance from an integer.
    IntToPtr,
}

impl Op {
    pub fn label(self) -> &'static str {
        match self {
            Op::Deref => "deref",
            Op::Arith => "ptr-arith",
            Op::IntToPtr => "int-to-ptr",
        }
    }
}

/// One raw-pointer operation, and whether an `unsafe` block covers it.
#[derive(Clone, Debug)]
pub struct Site {
    pub function: String,
    pub op: Op,
    pub covered: bool,
    pub span: Span,
}

/// Collect every raw-pointer operation in `ast`, in source order.
///
/// Runs after the type checker (like `layout`), because "is this a raw pointer" is a
/// type question — `p.*` is also how a `genref` reads, and a genref deref is checked,
/// not raw, so flagging it here would teach users to wrap safe code in `unsafe`.
pub fn collect(ast: &Ast, info: &TypeInfo) -> Vec<Site> {
    let mut out = Vec::new();
    for item in &ast.items {
        match item {
            Item::Fn(f) => collect_fn(ast, info, f, &f.name.name, &mut out),
            Item::Struct { name, body, .. } => {
                for m in &body.members {
                    if let StructMember::Method(f) = m {
                        let qual = format!("{}.{}", name.name, f.name.name);
                        collect_fn(ast, info, f, &qual, &mut out);
                    }
                }
            }
            _ => {}
        }
    }
    out
}

fn collect_fn(ast: &Ast, info: &TypeInfo, f: &FnDecl, name: &str, out: &mut Vec<Site>) {
    walk_block(ast, info, &f.body, name, false, out);
}

fn walk_block(
    ast: &Ast,
    info: &TypeInfo,
    b: &Block,
    name: &str,
    in_unsafe: bool,
    out: &mut Vec<Site>,
) {
    for s in &b.stmts {
        match s {
            Stmt::Expr(e) => walk_expr(ast, info, *e, name, in_unsafe, out),
            Stmt::Let { init, .. } => {
                if let Some(e) = init {
                    walk_expr(ast, info, *e, name, in_unsafe, out);
                }
            }
            Stmt::Return { value, .. } => {
                if let Some(e) = value {
                    walk_expr(ast, info, *e, name, in_unsafe, out);
                }
            }
        }
    }
}

fn is_raw_ptr(info: &TypeInfo, id: ExprId) -> bool {
    matches!(info.type_of(id), Ty::Ptr { .. })
}

fn walk_expr(
    ast: &Ast,
    info: &TypeInfo,
    id: ExprId,
    name: &str,
    in_unsafe: bool,
    out: &mut Vec<Site>,
) {
    let e = ast.expr_at(id);
    let span = e.span;
    match &e.kind {
        // The unsafe permission is *lexical*: it covers exactly the block's extent,
        // which is what makes coverage decidable by a walk.
        ExprKind::Unsafe(b) => walk_block(ast, info, b, name, true, out),
        ExprKind::Deref { base } => {
            // Only a RAW deref counts. `genref.*` is generation-checked and `p.*` on
            // a region ref is arena-scoped — both are the safe alternatives the
            // contract points people toward, so counting them would be exactly wrong.
            if is_raw_ptr(info, *base) {
                out.push(Site { function: name.to_string(), op: Op::Deref, covered: in_unsafe, span });
            }
            walk_expr(ast, info, *base, name, in_unsafe, out);
        }
        ExprKind::Binary { op: BinOp::Add | BinOp::Sub, lhs, rhs } => {
            if is_raw_ptr(info, *lhs) || is_raw_ptr(info, *rhs) {
                out.push(Site { function: name.to_string(), op: Op::Arith, covered: in_unsafe, span });
            }
            walk_expr(ast, info, *lhs, name, in_unsafe, out);
            walk_expr(ast, info, *rhs, name, in_unsafe, out);
        }
        ExprKind::Cast { expr, .. } => {
            // An int-to-pointer cast manufactures provenance: the result type is a
            // pointer while the operand is a **known integer**. Requiring the operand
            // to be a known integer — rather than merely "not a pointer" — matters
            // because an intrinsic's return type can be `Unknown` to the checker, and
            // `alloc_i32(8) as *mut Tree` is a ptr-to-ptr cast (provenance reused, the
            // milder conversation), not a manufactured address. A measurement pass
            // must not overcount: a census with false positives sizes a migration
            // that does not exist.
            let to_ptr = matches!(info.type_of(id), Ty::Ptr { .. });
            let from_int = matches!(
                info.type_of(*expr),
                Ty::Prim(
                    "i8" | "i16" | "i32" | "i64" | "isize" | "u8" | "u16" | "u32" | "u64" | "usize"
                )
            );
            if to_ptr && from_int {
                out.push(Site { function: name.to_string(), op: Op::IntToPtr, covered: in_unsafe, span });
            }
            walk_expr(ast, info, *expr, name, in_unsafe, out);
        }
        // Everything else: recurse structurally.
        ExprKind::Block(b) | ExprKind::Concurrent(b) | ExprKind::Region { body: b, .. } => {
            walk_block(ast, info, b, name, in_unsafe, out)
        }
        // A `comptime` body is deliberately NOT walked: it runs in the interpreter,
        // which has no pointers at all, so nothing in it is a runtime raw-pointer op.
        // (The port's flat-scan mirror excludes comptime spans for the same reason.)
        ExprKind::Comptime(_) => {}
        // A closure body is runtime code like any other — a deref inside one is a
        // site. Coverage does NOT cross the boundary in either direction: an `unsafe`
        // wrapping the closure *literal* covers the body lexically, which is exactly
        // what the lexical rule says it should.
        ExprKind::Closure { body, .. } => walk_expr(ast, info, *body, name, in_unsafe, out),
        ExprKind::Spawn(e) | ExprKind::Await(e) => walk_expr(ast, info, *e, name, in_unsafe, out),
        ExprKind::FString { exprs, .. } => {
            for e in exprs {
                walk_expr(ast, info, *e, name, in_unsafe, out);
            }
        }
        ExprKind::If { cond, then, els } => {
            walk_expr(ast, info, *cond, name, in_unsafe, out);
            walk_block(ast, info, then, name, in_unsafe, out);
            if let Some(e) = els {
                walk_expr(ast, info, *e, name, in_unsafe, out);
            }
        }
        ExprKind::For { body, els, head, .. } => {
            if let crate::ast::ForHead::While(c) = head {
                walk_expr(ast, info, *c, name, in_unsafe, out);
            }
            if let crate::ast::ForHead::Iter { sources, step, .. } = head {
                for s in sources {
                    walk_expr(ast, info, *s, name, in_unsafe, out);
                }
                if let Some(s) = step {
                    walk_expr(ast, info, *s, name, in_unsafe, out);
                }
            }
            walk_block(ast, info, body, name, in_unsafe, out);
            if let Some(e) = els {
                walk_block(ast, info, e, name, in_unsafe, out);
            }
        }
        ExprKind::Call { callee, args } => {
            walk_expr(ast, info, *callee, name, in_unsafe, out);
            for a in args {
                walk_expr(ast, info, *a, name, in_unsafe, out);
            }
        }
        ExprKind::Binary { lhs, rhs, .. } => {
            walk_expr(ast, info, *lhs, name, in_unsafe, out);
            walk_expr(ast, info, *rhs, name, in_unsafe, out);
        }
        ExprKind::Unary { rhs, .. } => walk_expr(ast, info, *rhs, name, in_unsafe, out),
        ExprKind::Field { base, .. } | ExprKind::Try { base } => {
            walk_expr(ast, info, *base, name, in_unsafe, out)
        }
        ExprKind::Catch { base, fallback, .. } => {
            walk_expr(ast, info, *base, name, in_unsafe, out);
            walk_expr(ast, info, *fallback, name, in_unsafe, out);
        }
        ExprKind::Index { base, index } => {
            walk_expr(ast, info, *base, name, in_unsafe, out);
            walk_expr(ast, info, *index, name, in_unsafe, out);
        }
        ExprKind::Assign { target, value, .. } => {
            walk_expr(ast, info, *target, name, in_unsafe, out);
            walk_expr(ast, info, *value, name, in_unsafe, out);
        }
        ExprKind::StructLit { fields, spread, .. } => {
            for fi in fields {
                walk_expr(ast, info, fi.value, name, in_unsafe, out);
            }
            if let Some(s) = spread {
                walk_expr(ast, info, *s, name, in_unsafe, out);
            }
        }
        ExprKind::GenStructLit { fields, .. } => {
            for fi in fields {
                walk_expr(ast, info, fi.value, name, in_unsafe, out);
            }
        }
        ExprKind::ArrayLit { elems } => {
            for i in elems {
                walk_expr(ast, info, *i, name, in_unsafe, out);
            }
        }
        ExprKind::ArrayRepeat { value, count } => {
            walk_expr(ast, info, *value, name, in_unsafe, out);
            walk_expr(ast, info, *count, name, in_unsafe, out);
        }
        ExprKind::Match { scrut, arms } => {
            walk_expr(ast, info, *scrut, name, in_unsafe, out);
            for arm in arms {
                if let Some(g) = arm.guard {
                    walk_expr(ast, info, g, name, in_unsafe, out);
                }
                walk_expr(ast, info, arm.body, name, in_unsafe, out);
            }
        }
        // Everything else recurses structurally. This used to be `_ => {}`, which
        // silently stopped the walk at any node kind not named above — so a raw
        // pointer operation nested inside a newly-added expression form would
        // vanish from a report whose whole job is to find every one of them.
        // `visit::child_exprs` is exhaustive, so a new variant is a compile error
        // there rather than a hole here.
        //
        // The `unsafe` flag is not propagated specially: `Unsafe` is handled above
        // and is the only construct that grants the permission, so a generic
        // descent correctly carries the *enclosing* state.
        _ => {
            for child in crate::visit::child_exprs(ast, id) {
                walk_expr(ast, info, child, name, in_unsafe, out);
            }
        }
    }
}

/// Render the report `jestyrc unsafe` prints — deterministic, diffable, pinnable.
pub fn render(sites: &[Site], src: &str) -> String {
    let mut out = String::new();
    out.push_str("unsafe v1\n");
    let uncovered = sites.iter().filter(|s| !s.covered).count();
    let _ = writeln!(out, "sites {} uncovered {}", sites.len(), uncovered);
    let mut cur = String::new();
    for s in sites {
        if s.function != cur {
            let _ = writeln!(out, "fn {}", s.function);
            cur = s.function.clone();
        }
        let lc = crate::span::line_col(src, s.span.start);
        let _ = writeln!(
            out,
            "  {} {}:{} {}",
            s.op.label(),
            lc.line,
            lc.col,
            if s.covered { "unsafe" } else { "UNCOVERED" }
        );
    }
    // Enforcement landed (v2 step 4): an uncovered site is now a compile error from
    // the escape checker; this report remains the *survey* view of the same facts.
    out.push_str("note: uncovered sites are compile errors (see docs/unsafe-contract.md)\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::Lexer;
    use crate::parser::Parser;

    fn sites_of(src: &str) -> Vec<Site> {
        let (tokens, _) = Lexer::new(src).tokenize();
        let (ast, d) = Parser::new(src, tokens).parse();
        assert!(d.iter().all(|x| !x.is_error()), "fixture must parse: {d:?}");
        let (info, _) = crate::typeck::check(&ast);
        collect(&ast, &info)
    }

    #[test]
    fn an_uncovered_raw_deref_is_reported_and_a_covered_one_is_not_uncovered() {
        let s = sites_of(
            "fn f(p: *mut i32) -> i32 { p.* = 1 return unsafe { p.* } }",
        );
        assert_eq!(s.len(), 2, "{s:?}");
        assert_eq!((s[0].op, s[0].covered), (Op::Deref, false), "{s:?}");
        assert_eq!((s[1].op, s[1].covered), (Op::Deref, true), "{s:?}");
    }

    /// A `genref` deref is generation-checked — the SAFE alternative the contract
    /// points people toward — so counting it would teach exactly the wrong lesson.
    #[test]
    fn a_genref_deref_is_not_a_raw_site() {
        let s = sites_of(
            "fn f(n: i32) -> i32 { let g = gen_new(i32, n) let v = g.* gen_free(i32, g) return v }",
        );
        assert!(s.is_empty(), "a checked deref must not be flagged: {s:?}");
    }

    #[test]
    fn pointer_arithmetic_and_int_to_ptr_casts_are_sites() {
        let s = sites_of(
            "fn f(p: *mut i32, i: i64) -> i32 { let q = p + i return unsafe { q.* } }",
        );
        assert!(s.iter().any(|x| x.op == Op::Arith && !x.covered), "{s:?}");
        let s = sites_of("fn f() -> i32 { let r = 0x1000 as *mut u32 return 0 }");
        assert!(s.iter().any(|x| x.op == Op::IntToPtr && !x.covered), "{s:?}");
        // A pointer-to-pointer cast reuses provenance — a different, milder thing,
        // deliberately not counted as manufacturing it.
        let s = sites_of("fn f(p: *mut i32) -> i32 { let q = p as *mut u32 return 0 }");
        assert!(!s.iter().any(|x| x.op == Op::IntToPtr), "ptr-to-ptr is not int-to-ptr: {s:?}");
    }

    /// Coverage is lexical: an `unsafe` block covers its extent and nothing more —
    /// the property that makes "is this covered" decidable by a walk.
    #[test]
    fn unsafe_coverage_is_lexical_not_function_wide() {
        let s = sites_of(
            "fn f(p: *mut i32) -> i32 { unsafe { p.* = 1 } p.* = 2 return 0 }",
        );
        assert_eq!(s.len(), 2, "{s:?}");
        assert!(s[0].covered && !s[1].covered, "coverage leaked past the block: {s:?}");
    }

    #[test]
    fn the_report_is_deterministic_and_admits_non_enforcement() {
        let src = "fn f(p: *mut i32) -> i32 { p.* = 1 return 0 }";
        let (tokens, _) = Lexer::new(src).tokenize();
        let (ast, _) = Parser::new(src, tokens).parse();
        let (info, _) = crate::typeck::check(&ast);
        let r = render(&collect(&ast, &info), src);
        assert!(r.starts_with("unsafe v1\nsites 1 uncovered 1\n"), "{r}");
        assert!(r.contains("UNCOVERED"), "{r}");
        assert!(r.contains("compile errors"), "the report must state the enforcement it now makes: {r}");
        for _ in 0..3 {
            assert_eq!(render(&collect(&ast, &info), src), r);
        }
    }
}
