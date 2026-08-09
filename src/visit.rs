//! Structural traversal: **the one place that knows what an expression contains.**
//!
//! Most analysis passes only care about a handful of node kinds, and the natural
//! way to write one is a `match` with a `_ => {}` arm that recurses into "the
//! rest". That arm is a trap: adding an `ExprKind` variant compiles fine and the
//! pass silently stops visiting the new node's children. For a *report* that is a
//! gap; for `jestyrc unsafe`, which must find every raw-pointer operation, it is a
//! safety report that quietly under-reports.
//!
//! [`child_exprs`] answers only the structural question — "what sub-expressions
//! does this node have?" — with an **exhaustive** match and no `_` arm. Adding a
//! variant is then a compile error in exactly one file, and every pass built on it
//! picks the new node up for free.
//!
//! This is deliberately a function and not a `Visitor` trait. The passes that need
//! it (`provenance`) thread their own context — a lexical `unsafe` flag, an
//! enclosing function name — and a trait would have to model that. Separating
//! "what are the children" from "what does this pass do" keeps the shared part
//! small enough to be obviously right.
//!
//! Passes that need no context should keep scanning the arena flat instead
//! (`for e in ast.exprs.iter()`, as `errsets` and `simd` do): the arena is
//! complete by construction, so a flat scan is immune to this bug class without
//! any helper at all. Reach for this only when the traversal must be *nested* —
//! i.e. when the pass carries information down the tree.

use crate::ast::*;

/// Every expression directly contained in `id`, in source order.
///
/// Types, patterns and identifiers are *not* followed — this is expression
/// structure only. `Cast`'s `TypeId` and `Match`'s `PatId` are therefore absent by
/// design; a pass that needs them should ask the AST directly.
pub fn child_exprs(ast: &Ast, id: ExprId) -> Vec<ExprId> {
    let mut out = Vec::new();
    push_children(ast, id, &mut out);
    out
}

/// [`child_exprs`] without the allocation when the caller already has a buffer.
pub fn push_children(ast: &Ast, id: ExprId, out: &mut Vec<ExprId>) {
    // NO `_` arm, on purpose. If this stops compiling because a variant was added,
    // that is the design working: decide what its children are here, once, and
    // every walker in the tree stays correct.
    match &ast.expr_at(id).kind {
        // Leaves.
        ExprKind::Int(_)
        | ExprKind::Float(_)
        | ExprKind::Str(_)
        | ExprKind::Char(_)
        | ExprKind::Bool(_)
        | ExprKind::Null
        | ExprKind::Name(_)
        | ExprKind::SelfValue
        | ExprKind::SelfType
        | ExprKind::Attr(_)
        | ExprKind::Break(_)
        | ExprKind::Continue(_)
        | ExprKind::Error => {}

        ExprKind::Unary { rhs, .. } => out.push(*rhs),
        ExprKind::Binary { lhs, rhs, .. } => {
            out.push(*lhs);
            out.push(*rhs);
        }
        ExprKind::Assign { target, value, .. } => {
            out.push(*target);
            out.push(*value);
        }
        ExprKind::Range { lo, hi, .. } => {
            out.extend(lo.iter().copied());
            out.extend(hi.iter().copied());
        }
        ExprKind::Call { callee, args } => {
            out.push(*callee);
            out.extend(args.iter().copied());
        }
        ExprKind::Field { base, .. } => out.push(*base),
        ExprKind::Deref { base } => out.push(*base),
        ExprKind::Try { base } => out.push(*base),
        ExprKind::Catch { base, fallback, .. } => {
            out.push(*base);
            out.push(*fallback);
        }
        ExprKind::Index { base, index } => {
            out.push(*base);
            out.push(*index);
        }
        ExprKind::Cast { expr, .. } => out.push(*expr),
        ExprKind::ArrayRepeat { value, count } => {
            out.push(*value);
            out.push(*count);
        }
        ExprKind::ArrayLit { elems } => out.extend(elems.iter().copied()),
        ExprKind::StructLit { fields, spread, .. } => {
            out.extend(fields.iter().map(|f| f.value));
            out.extend(spread.iter().copied());
        }
        ExprKind::GenStructLit { type_args, fields, .. } => {
            // `type_args` are type-valued *expressions* here, so they are children.
            out.extend(type_args.iter().copied());
            out.extend(fields.iter().map(|f| f.value));
        }
        // A `struct { … }` value carries field defaults, which are expressions.
        ExprKind::StructType(body) => push_struct_body(body, out),

        ExprKind::Block(b) => push_block(b, out),
        ExprKind::Unsafe(b) => push_block(b, out),
        ExprKind::Comptime(b) => push_block(b, out),
        ExprKind::Concurrent(b) => push_block(b, out),

        ExprKind::If { cond, then, els } => {
            out.push(*cond);
            push_block(then, out);
            out.extend(els.iter().copied());
        }
        ExprKind::Match { scrut, arms } => {
            out.push(*scrut);
            for arm in arms {
                out.extend(arm.guard.iter().copied());
                out.push(arm.body);
            }
        }
        ExprKind::FString { exprs, .. } => out.extend(exprs.iter().copied()),
        ExprKind::Closure { body, .. } => out.push(*body),
        ExprKind::Spawn(e) => out.push(*e),
        ExprKind::Await(e) => out.push(*e),
        ExprKind::ParFor { iter, reduction, body, .. } => {
            out.push(*iter);
            out.push(*reduction);
            out.push(*body);
        }
        ExprKind::Select(arms) => {
            for arm in arms {
                out.push(arm.chan);
                push_block(&arm.body, out);
            }
        }
        ExprKind::Region { body, .. } => push_block(body, out),
        ExprKind::For { head, body, els, .. } => {
            match head {
                ForHead::Infinite => {}
                ForHead::While(c) => out.push(*c),
                ForHead::Iter { sources, step, .. } => {
                    out.extend(sources.iter().copied());
                    out.extend(step.iter().copied());
                }
            }
            push_block(body, out);
            if let Some(e) = els {
                push_block(e, out);
            }
        }
        ExprKind::Invariant(e) => out.push(*e),
        ExprKind::Variant(e) => out.push(*e),
    }
}

/// Every expression directly contained in a block's statements.
pub fn push_block(b: &Block, out: &mut Vec<ExprId>) {
    for s in &b.stmts {
        match s {
            Stmt::Let { init, .. } => out.extend(init.iter().copied()),
            Stmt::Return { value, .. } => out.extend(value.iter().copied()),
            Stmt::Expr(e) => out.push(*e),
        }
    }
}

/// Field default values inside a `struct { … }` value expression.
fn push_struct_body(body: &StructBody, out: &mut Vec<ExprId>) {
    for m in &body.members {
        if let StructMember::Field { default, .. } = m {
            out.extend(default.iter().copied());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::Lexer;
    use crate::parser::Parser;

    fn ast_of(src: &str) -> Ast {
        let (tokens, _) = Lexer::new(src).tokenize();
        let (ast, d) = Parser::new(src, tokens).parse();
        assert!(d.is_empty(), "parse errors: {d:?}");
        ast
    }

    /// **Reachability.** Walking from every item body must reach every expression
    /// the parser built, apart from the roots themselves. If a variant's children
    /// were forgotten, some node becomes unreachable and this fails — which is the
    /// whole point of the exhaustive match.
    #[test]
    fn every_expression_is_reachable_from_a_root() {
        let src = "\
            struct S { a: i32 = 1 + 1 }\
            enum E { x, y }\
            const K: i32 = 2 * 3\
            fn f(s: []i32, p: *i32, e: E) -> i32 {\
                let a = [1, 2, 3]\
                let b = [0; 4]\
                let c = S{ a: 7 }\
                var t = 0\
                for i in 0..4 { t = t + i }\
                for t < 10 { t += 1 }\
                if t > 1 { t = t - 1 } else { t = t + 1 }\
                match e { x => 1, y if t > 0 => 2, _ => 3 }\
                unsafe { t = p.* }\
                comptime { }\
                concurrent { }\
                region r { }\
                let g = |z| z + 1\
                let h = f\"v={t}\"\
                let k = s[0] as i64 as i32\
                return t\
            }";
        let ast = ast_of(src);

        let mut reached = vec![false; ast.exprs.len()];
        let mut stack: Vec<ExprId> = Vec::new();
        let mut seed = |b: &Block, st: &mut Vec<ExprId>| {
            let mut tmp = Vec::new();
            push_block(b, &mut tmp);
            st.extend(tmp);
        };
        for item in &ast.items {
            match item {
                Item::Fn(f) => seed(&f.body, &mut stack),
                Item::Const(c) => stack.push(c.value),
                Item::Struct { body, .. } => {
                    for m in &body.members {
                        match m {
                            StructMember::Field { default, .. } => stack.extend(default.iter().copied()),
                            StructMember::Method(f) => seed(&f.body, &mut stack),
                        }
                    }
                }
                _ => {}
            }
        }
        while let Some(id) = stack.pop() {
            if reached[id.0 as usize] {
                continue;
            }
            reached[id.0 as usize] = true;
            push_children(&ast, id, &mut stack);
        }

        let missed: Vec<usize> = reached
            .iter()
            .enumerate()
            .filter(|(_, r)| !**r)
            .map(|(i, _)| i)
            .collect();
        assert!(
            missed.is_empty(),
            "unreachable expressions — a variant's children are missing from \
             `push_children`: {:?}",
            missed
                .iter()
                .map(|i| format!("{:?}", ast.exprs[*i].kind))
                .collect::<Vec<_>>()
        );
    }

    /// A leaf has no children, and a compound node reports them in source order.
    #[test]
    fn children_are_in_source_order() {
        let ast = ast_of("fn f() -> i32 { return 1 + 2 * 3 }");
        // Find the `+` node and check its two children come out lhs-then-rhs.
        let plus = ast
            .exprs
            .iter()
            .position(|e| matches!(&e.kind, ExprKind::Binary { op: BinOp::Add, .. }))
            .expect("an addition");
        let kids = child_exprs(&ast, ExprId(plus as u32));
        assert_eq!(kids.len(), 2, "a binary node has exactly two children");
        assert!(kids[0].0 < kids[1].0, "children come out in source order");
    }
}
