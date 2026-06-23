//! Stage ⑤ (prototype): the ownership / escape checker — the heart of Jestyr.
//!
//! It enforces the single rule the whole language thesis rests on (design doc
//! §4.3): a *second-class borrow* (`read`/`mut`/`out` parameter) may be passed
//! **further down** the call stack, but must never **escape** its call frame.
//! Because such borrows provably cannot outlive the frame, they need no lifetime
//! annotations — which is the elegance Jestyr trades Rust's lifetimes for.
//!
//! ## What is a borrow?
//! A parameter with convention `read`/`mut`/`out` (including `read/mut/out self`).
//! `take` transfers ownership; the default (no convention) is treated as owned.
//! Borrow-ness propagates through `let x = <borrow place>` and through pattern
//! bindings when matching on a borrowed scrutinee.
//!
//! A *borrow place* is an expression naming borrowed storage: a borrow binding
//! (`p`, `self`) or a projection of one (`p.field`, `p[i]`, `p.*`).
//!
//! ## Escape routes flagged
//!  1. **return**   — a borrow place in return position, when the return is *by
//!                    value* (not declared `read`/`mut`/`out`).
//!  2. **capture**  — a borrow place stored as a struct-literal field.
//!  3. **store**    — assigning a borrow place into borrowed storage
//!                    (`borrowed.field = borrow`).
//!
//! ## Explicitly allowed (the thesis in action)
//!  * passing a borrow as a **call argument** — second-class borrows flow *down*;
//!  * returning a borrow when the **return convention is a borrow** (`-> read T`)
//!    — the signature opts into handing a borrow back out.
//!
//! ## Copy-refinement (stage ④ now wired in)
//! Each escape route fires only when the value that would escape is **non-`Copy`**
//! (per [`TypeInfo`]). Copying a borrowed scalar/field out (`-> i32 { p.value }`)
//! is no longer flagged — only genuinely *moved* references are. Generic/opaque
//! types are treated as non-`Copy`, which is the correct conservative choice.
//!
//! ## A fourth route, enabled by name resolution
//!  4. **give-away** — passing a borrow to a `take` (owning) parameter of a known
//!     function. `take` consumes ownership, which a second-class borrow cannot
//!     supply; this is how "storing a borrow into a collection" (`vec.push(take
//!     value)`) is caught for free-function calls.

use std::collections::HashMap;

use crate::ast::*;
use crate::diag::Diagnostic;
use crate::span::Span;
use crate::types::TypeInfo;

pub fn check(ast: &Ast, info: &TypeInfo) -> Vec<Diagnostic> {
    let mut ck = Checker { ast, info, diags: Vec::new(), frozen: Vec::new() };
    for item in &ast.items {
        ck.check_item(item);
    }
    ck.diags
}

struct Checker<'a> {
    ast: &'a Ast,
    info: &'a TypeInfo,
    diags: Vec<Diagnostic>,
    /// Collections currently being iterated (by simple name). A `for … in xs`
    /// loop holds a borrow of `xs` for its body, so mutating `xs` there is
    /// forbidden (iterator invalidation — the borrow contract of a loop). A stack
    /// because loops nest.
    frozen: Vec<String>,
}

/// Per-function analysis state: a stack of lexical scopes mapping each in-scope
/// binding to whether it denotes a borrow, plus this function's return mode.
struct FnCtx {
    scopes: Vec<HashMap<String, bool>>,
    ret_is_borrow: bool,
}

impl FnCtx {
    fn push(&mut self) {
        self.scopes.push(HashMap::new());
    }
    fn pop(&mut self) {
        self.scopes.pop();
    }
    fn bind(&mut self, name: &str, is_borrow: bool) {
        self.scopes.last_mut().unwrap().insert(name.to_string(), is_borrow);
    }
    /// Innermost binding wins (handles shadowing).
    fn lookup(&self, name: &str) -> Option<bool> {
        for scope in self.scopes.iter().rev() {
            if let Some(&b) = scope.get(name) {
                return Some(b);
            }
        }
        None
    }
}

impl<'a> Checker<'a> {
    fn error(&mut self, span: Span, message: impl Into<String>) {
        self.diags.push(Diagnostic::new(message, span));
    }

    fn check_item(&mut self, item: &Item) {
        match item {
            Item::Fn(f) => self.check_fn(f),
            Item::Struct { body, .. } => {
                for m in &body.members {
                    if let StructMember::Method(f) = m {
                        self.check_fn(f);
                    }
                }
            }
            Item::Enum(_) | Item::Const(_) | Item::Distinct(_) | Item::Extern(_) | Item::Import(_) => {}
        }
    }

    fn check_fn(&mut self, f: &FnDecl) {
        let ret_is_borrow = matches!(f.ret_conv, Conv::Read | Conv::Mut | Conv::Out);
        let mut ctx = FnCtx { scopes: Vec::new(), ret_is_borrow };
        ctx.push();
        for p in &f.params {
            // MVS (design §4.3): the default convention *is* `read` (an immutable
            // borrow). Only `take` transfers ownership — so every non-comptime,
            // non-`take` parameter is a borrow that may not escape its frame.
            let is_borrow = !p.comptime && p.conv != Conv::Take;
            let name = if p.is_self { "self" } else { p.name.name.as_str() };
            ctx.bind(name, is_borrow);
        }
        // The body is in return position: its tail expression is the result.
        self.check_block(&mut ctx, &f.body, true);
    }

    fn check_block(&mut self, ctx: &mut FnCtx, block: &Block, tail: bool) {
        ctx.push();
        let n = block.stmts.len();
        for (i, stmt) in block.stmts.iter().enumerate() {
            let is_last = i + 1 == n;
            match stmt {
                Stmt::Let { name, init, .. } => {
                    let is_borrow = if let Some(e) = init {
                        self.walk_expr(ctx, *e, false);
                        self.is_borrow_place(ctx, *e)
                    } else {
                        false
                    };
                    ctx.bind(&name.name, is_borrow);
                }
                Stmt::Return { value, .. } => {
                    if let Some(v) = value {
                        // An explicit `return` is always a return position.
                        self.walk_expr(ctx, *v, true);
                    }
                }
                Stmt::Expr(e) => {
                    // Only the block's tail expression inherits return position.
                    self.walk_expr(ctx, *e, is_last && tail);
                }
            }
        }
        ctx.pop();
    }

    /// Walk an expression, reporting capture/store escapes everywhere and (when
    /// `tail` is set) return escapes at the leaves. `tail` is propagated into the
    /// branches of control-flow forms so that every reachable result expression
    /// is checked.
    fn walk_expr(&mut self, ctx: &mut FnCtx, id: ExprId, tail: bool) {
        let ast = self.ast;
        let data = ast.expr_at(id);
        let span = data.span;

        match &data.kind {
            ExprKind::If { cond, then, els } => {
                self.walk_expr(ctx, *cond, false);
                self.check_block(ctx, then, tail);
                if let Some(e) = els {
                    self.walk_expr(ctx, *e, tail);
                }
                return;
            }
            ExprKind::Match { scrut, arms } => {
                let scrut_borrow = self.is_borrow_place(ctx, *scrut);
                self.walk_expr(ctx, *scrut, false);
                for arm in arms {
                    ctx.push();
                    self.bind_pattern(ctx, arm.pat, scrut_borrow);
                    // The guard sees the pattern's bindings; walk it so closures or
                    // nested expressions inside it are checked. It's a boolean, so
                    // nothing escapes *through* it (never a tail position).
                    if let Some(g) = arm.guard {
                        self.walk_expr(ctx, g, false);
                    }
                    self.walk_expr(ctx, arm.body, tail);
                    ctx.pop();
                }
                return;
            }
            ExprKind::Block(b) => {
                self.check_block(ctx, b, tail);
                return;
            }
            ExprKind::Unsafe(b) => {
                self.check_block(ctx, b, tail);
                return;
            }
            ExprKind::StructType(body) => {
                // A struct-type literal is a *type definition*, not part of this
                // function's value dataflow. Check its methods independently.
                for m in &body.members {
                    if let StructMember::Method(f) = m {
                        self.check_fn(f);
                    }
                }
                return;
            }
            ExprKind::StructLit { path, fields } => {
                for fi in fields {
                    self.walk_expr(ctx, fi.value, false);
                    if self.escapes_as(ctx, fi.value) {
                        let name = self.root_name(ctx, fi.value);
                        self.error(
                            ast.expr_at(fi.value).span,
                            format!(
                                "cannot store borrow `{name}` in struct `{}`: a second-class borrow may not outlive its call",
                                path.name
                            ),
                        );
                    }
                }
                return;
            }
            ExprKind::GenStructLit { ctor, fields, .. } => {
                for fi in fields {
                    self.walk_expr(ctx, fi.value, false);
                    if self.escapes_as(ctx, fi.value) {
                        let name = self.root_name(ctx, fi.value);
                        self.error(
                            ast.expr_at(fi.value).span,
                            format!(
                                "cannot store borrow `{name}` in struct `{}`: a second-class borrow may not outlive its call",
                                ctor.name
                            ),
                        );
                    }
                }
                return;
            }
            ExprKind::Assign { target, value, .. } => {
                self.walk_expr(ctx, *target, false);
                self.walk_expr(ctx, *value, false);
                if self.is_borrow_place(ctx, *target) && self.escapes_as(ctx, *value) {
                    let name = self.root_name(ctx, *value);
                    self.error(
                        span,
                        format!(
                            "cannot store borrow `{name}` into borrowed storage: it would outlive its call"
                        ),
                    );
                }
                // Mutating a collection currently being iterated (e.g. `xs[i] = v`
                // or `xs = …` inside `for x in xs`) invalidates the loop's borrow.
                let root = self.root_name(ctx, *target);
                if self.frozen.contains(&root) {
                    self.error(
                        span,
                        format!("cannot mutate `{root}` while iterating it: the `for` loop holds a borrow of it"),
                    );
                }
                return;
            }
            ExprKind::Call { callee, args } => {
                // Passing a borrow *down* is the allowed case — UNLESS the callee
                // is a known function whose matching parameter is `take` (owning):
                // you can't hand ownership of something you only borrowed.
                self.walk_expr(ctx, *callee, false);
                for a in args {
                    self.walk_expr(ctx, *a, false);
                }
                self.check_give_away(ctx, id, *callee, args);
                self.check_loop_mutation(ctx, id, *callee, args);
                return;
            }
            ExprKind::Binary { lhs, rhs, .. } => {
                self.walk_expr(ctx, *lhs, false);
                self.walk_expr(ctx, *rhs, false);
            }
            ExprKind::Unary { rhs, .. } => self.walk_expr(ctx, *rhs, false),
            ExprKind::Range { lo, hi, .. } => {
                if let Some(l) = lo {
                    self.walk_expr(ctx, *l, false);
                }
                if let Some(h) = hi {
                    self.walk_expr(ctx, *h, false);
                }
            }
            ExprKind::Field { base, .. } => self.walk_expr(ctx, *base, false),
            ExprKind::Index { base, index } => {
                self.walk_expr(ctx, *base, false);
                self.walk_expr(ctx, *index, false);
            }
            ExprKind::Deref { base } => self.walk_expr(ctx, *base, false),
            ExprKind::Try { base } => self.walk_expr(ctx, *base, false),
            ExprKind::Cast { expr, .. } => self.walk_expr(ctx, *expr, false),
            ExprKind::Closure { params, body } => {
                // The closure's own parameters shadow outer borrows; check its
                // body for escapes within. Whether the *closure value* escapes is
                // decided by `is_borrow_place` (does it capture a borrow?).
                ctx.push();
                for p in params {
                    ctx.bind(&p.name.name, false);
                }
                self.walk_expr(ctx, *body, false);
                ctx.pop();
            }
            ExprKind::Concurrent(b) => {
                // Structured concurrency: tasks join at the scope's end, so a
                // borrow flowing into a `spawn` does *not* outlive its frame.
                self.check_block(ctx, b, false);
                return;
            }
            ExprKind::Spawn(call) => {
                self.walk_expr(ctx, *call, false);
                return;
            }
            ExprKind::Region { body, .. } => {
                self.check_block(ctx, body, false);
                return;
            }
            ExprKind::For { head, body, els, .. } => {
                ctx.push();
                let mut froze = 0usize;
                match head {
                    ForHead::Infinite => {}
                    ForHead::While(c) => self.walk_expr(ctx, *c, false),
                    ForHead::Iter { binds, sources, .. } => {
                        for s in sources {
                            self.walk_expr(ctx, *s, false);
                        }
                        // A slice-element binding is a borrow *into* the iterated
                        // collection (so it must not escape the loop, enforced by
                        // the existing routes); a range index is a fresh value.
                        // The iterated collections are also frozen: mutating one in
                        // the body is iterator invalidation.
                        let is_range = |c: &Self, s: ExprId| {
                            matches!(&c.ast.expr_at(s).kind, ExprKind::Range { .. })
                        };
                        if sources.len() <= 1 {
                            let src = sources.first().copied();
                            let elem_borrow = src.map_or(false, |s| !is_range(self, s));
                            if let Some(b0) = binds.first() {
                                if b0.name.name != "_" {
                                    ctx.bind(&b0.name.name, elem_borrow);
                                }
                            }
                            if let Some(b1) = binds.get(1) {
                                if b1.name.name != "_" {
                                    ctx.bind(&b1.name.name, false); // the index
                                }
                            }
                            if elem_borrow {
                                if let Some(ExprKind::Name(n)) = src.map(|s| &ast.expr_at(s).kind) {
                                    self.frozen.push(n.name.clone());
                                    froze += 1;
                                }
                            }
                        } else {
                            for b in binds {
                                if b.name.name != "_" {
                                    ctx.bind(&b.name.name, true); // each is an element
                                }
                            }
                            for s in sources {
                                if let ExprKind::Name(n) = &ast.expr_at(*s).kind {
                                    self.frozen.push(n.name.clone());
                                    froze += 1;
                                }
                            }
                        }
                    }
                }
                // The body is never in return position (loops are statements).
                self.check_block(ctx, body, false);
                for _ in 0..froze {
                    self.frozen.pop();
                }
                ctx.pop();
                // The `else` block runs after the loop, in the enclosing scope:
                // the loop bindings are gone and the iterated collections are no
                // longer frozen.
                if let Some(els) = els {
                    self.check_block(ctx, els, false);
                }
                return;
            }
            ExprKind::Invariant(e) | ExprKind::Variant(e) => {
                self.walk_expr(ctx, *e, false);
                return;
            }

            // Leaves: nothing to descend into.
            ExprKind::Name(_)
            | ExprKind::SelfValue
            | ExprKind::SelfType
            | ExprKind::Attr(_)
            | ExprKind::Int(_)
            | ExprKind::Float(_)
            | ExprKind::Str(_)
            | ExprKind::Char(_)
            | ExprKind::Bool(_)
            | ExprKind::Null
            | ExprKind::Break(_)
            | ExprKind::Continue(_)
            | ExprKind::Error => {}
        }

        // Return-position leaf check: a non-Copy borrow place returned by value
        // escapes. (A Copy value is duplicated out, not referenced — so it's fine.)
        if tail && !ctx.ret_is_borrow && self.escapes_as(ctx, id) {
            let name = self.root_name(ctx, id);
            let msg = if matches!(&self.ast.expr_at(id).kind, ExprKind::Closure { .. }) {
                format!("cannot return a closure capturing borrow `{name}`: the borrow would outlive its call")
            } else {
                format!(
                    "cannot return borrow `{name}`: a second-class `read`/`mut`/`out` borrow may not outlive its call \
                     (pass it further down, or declare the return as `read`/`mut`/`out`)"
                )
            };
            self.error(span, msg);
        }
    }

    /// Would moving this expression out be an escape? True when it names borrowed
    /// storage *and* its type is non-`Copy` (a `Copy` value is duplicated, not
    /// referenced, so letting it "escape" is just a copy).
    fn escapes_as(&self, ctx: &FnCtx, id: ExprId) -> bool {
        // A closure that captures a borrow is non-`Copy` by nature (it holds a
        // reference), so the Copy refinement doesn't apply to it.
        if matches!(&self.ast.expr_at(id).kind, ExprKind::Closure { .. }) {
            return self.is_borrow_place(ctx, id);
        }
        self.is_borrow_place(ctx, id) && self.info.is_non_copy(id)
    }

    /// Route 4: passing a borrow to a `take` parameter of a known function —
    /// for both free calls and the desugared method calls resolved by typeck.
    fn check_give_away(&mut self, ctx: &FnCtx, call_id: ExprId, callee: ExprId, args: &[ExprId]) {
        // Method call: the receiver and the explicit arguments map onto the
        // resolved function's runtime (non-comptime) parameters.
        if let Some(mr) = self.info.method_calls.get(&call_id).cloned() {
            let ExprKind::Field { base, .. } = &self.ast.expr_at(callee).kind else { return };
            let base = *base;
            let Some(f) = self.find_fn(&mr.fn_name) else { return };
            let runtime: Vec<(String, Conv)> =
                f.params.iter().filter(|p| !p.comptime).map(|p| (p.name.name.clone(), p.conv)).collect();
            // receiver ↔ runtime[0]
            if let Some((pname, Conv::Take)) = runtime.first() {
                if self.escapes_as(ctx, base) {
                    let borrow = self.root_name(ctx, base);
                    self.give_away_error(base, &borrow, pname, &mr.fn_name);
                }
            }
            // explicit args ↔ runtime[1..]
            for (i, &arg) in args.iter().enumerate() {
                if let Some((pname, Conv::Take)) = runtime.get(i + 1) {
                    if self.escapes_as(ctx, arg) {
                        let borrow = self.root_name(ctx, arg);
                        self.give_away_error(arg, &borrow, pname, &mr.fn_name);
                    }
                }
            }
            return;
        }

        let name = match &self.ast.expr_at(callee).kind {
            ExprKind::Name(n) => n.name.clone(),
            _ => return,
        };
        let Some(sig) = self.info.table.fns.get(&name) else { return };
        let takes: Vec<(usize, String)> = sig
            .params
            .iter()
            .enumerate()
            .filter(|(_, p)| p.conv == Conv::Take)
            .map(|(i, p)| (i, p.name.clone()))
            .collect();
        for (i, pname) in takes {
            if let Some(&arg) = args.get(i) {
                if self.escapes_as(ctx, arg) {
                    let borrow = self.root_name(ctx, arg);
                    self.give_away_error(arg, &borrow, &pname, &name);
                }
            }
        }
    }

    /// Reject passing a *currently-iterated* collection to a parameter that could
    /// mutate or consume it (`mut`/`out`/`take`) — the call-site half of the loop
    /// borrow contract. Covers free calls, qualified calls, and method sugar.
    fn check_loop_mutation(&mut self, ctx: &FnCtx, call_id: ExprId, callee: ExprId, args: &[ExprId]) {
        if self.frozen.is_empty() {
            return;
        }
        let mutating = |c: Conv| matches!(c, Conv::Mut | Conv::Out | Conv::Take);

        // Method sugar: receiver ↔ runtime[0], explicit args ↔ runtime[1..].
        if let Some(mr) = self.info.method_calls.get(&call_id).cloned() {
            let ExprKind::Field { base, .. } = &self.ast.expr_at(callee).kind else { return };
            let base = *base;
            let Some(f) = self.find_fn(&mr.fn_name) else { return };
            let convs: Vec<Conv> = f.params.iter().filter(|p| !p.comptime).map(|p| p.conv).collect();
            if convs.first().copied().is_some_and(mutating) {
                self.flag_frozen_mutation(ctx, base);
            }
            for (i, &arg) in args.iter().enumerate() {
                if convs.get(i + 1).copied().is_some_and(mutating) {
                    self.flag_frozen_mutation(ctx, arg);
                }
            }
            return;
        }

        // Free call (`f(args)`) or module-qualified call (`m.f(args)`): the callee
        // resolves to a bare function name either way.
        let name = match &self.ast.expr_at(callee).kind {
            ExprKind::Name(n) => Some(n.name.clone()),
            ExprKind::Field { .. } => self.info.qualified.get(&call_id).cloned(),
            _ => None,
        };
        let Some(name) = name else { return };
        let Some(sig) = self.info.table.fns.get(&name) else { return };
        let convs: Vec<Conv> = sig.params.iter().map(|p| p.conv).collect();
        for (i, &arg) in args.iter().enumerate() {
            if convs.get(i).copied().is_some_and(mutating) {
                self.flag_frozen_mutation(ctx, arg);
            }
        }
    }

    fn flag_frozen_mutation(&mut self, ctx: &FnCtx, arg: ExprId) {
        let root = self.root_name(ctx, arg);
        if self.frozen.contains(&root) {
            self.error(
                self.ast.expr_at(arg).span,
                format!("cannot mutate `{root}` while iterating it: the `for` loop holds a borrow of it"),
            );
        }
    }

    fn give_away_error(&mut self, arg: ExprId, borrow: &str, param: &str, fn_name: &str) {
        self.error(
            self.ast.expr_at(arg).span,
            format!(
                "cannot give borrow `{borrow}` to owning parameter `{param}` of `{fn_name}`: \
                 `take` consumes ownership, which a second-class borrow cannot provide"
            ),
        );
    }

    /// Find a top-level function declaration by name.
    fn find_fn(&self, name: &str) -> Option<&'a FnDecl> {
        self.ast.items.iter().find_map(|it| match it {
            Item::Fn(f) if f.name.name == name => Some(f),
            _ => None,
        })
    }

    fn bind_pattern(&mut self, ctx: &mut FnCtx, pat: PatId, is_borrow: bool) {
        let ast = self.ast;
        match &ast.pat_at(pat).kind {
            PatKind::Ident(n) => ctx.bind(&n.name, is_borrow),
            PatKind::Variant { subpats, .. } => {
                for sp in subpats {
                    self.bind_pattern(ctx, *sp, is_borrow);
                }
            }
            PatKind::Lit(_) | PatKind::Range { .. } | PatKind::Rest => {}
            PatKind::Or(alts) => {
                for sp in alts {
                    self.bind_pattern(ctx, *sp, is_borrow);
                }
            }
            PatKind::Wildcard | PatKind::Error => {}
        }
    }

    /// Does `id` denote borrowed storage (a borrow binding or a projection of
    /// one) — or a closure that *captures* such a borrow?
    fn is_borrow_place(&self, ctx: &FnCtx, id: ExprId) -> bool {
        match &self.ast.expr_at(id).kind {
            ExprKind::Name(n) => ctx.lookup(&n.name).unwrap_or(false),
            ExprKind::SelfValue => ctx.lookup("self").unwrap_or(false),
            ExprKind::Field { base, .. } => self.is_borrow_place(ctx, *base),
            ExprKind::Index { base, .. } => self.is_borrow_place(ctx, *base),
            ExprKind::Deref { base } => self.is_borrow_place(ctx, *base),
            ExprKind::Closure { .. } => self.captured_borrow_name(ctx, id).is_some(),
            _ => false,
        }
    }

    /// The name of the root binding of a place (or the borrow a closure captures),
    /// for diagnostics.
    fn root_name(&self, ctx: &FnCtx, id: ExprId) -> String {
        match &self.ast.expr_at(id).kind {
            ExprKind::Name(n) => n.name.clone(),
            ExprKind::SelfValue => "self".to_string(),
            ExprKind::Field { base, .. } => self.root_name(ctx, *base),
            ExprKind::Index { base, .. } => self.root_name(ctx, *base),
            ExprKind::Deref { base } => self.root_name(ctx, *base),
            ExprKind::Closure { .. } => {
                self.captured_borrow_name(ctx, id).unwrap_or_else(|| "<closure>".to_string())
            }
            _ => "<borrow>".to_string(),
        }
    }

    /// If a closure captures a *non-`Copy`* borrow from the enclosing scope,
    /// return its name. A captured borrow is a free variable of the body that is
    /// not one of the closure's own parameters and that is a borrow in `ctx`.
    /// Copy captures (e.g. an `i32` read-param) are duplicated, not referenced,
    /// so they do not taint the closure.
    fn captured_borrow_name(&self, ctx: &FnCtx, closure_id: ExprId) -> Option<String> {
        let ExprKind::Closure { params, body } = &self.ast.expr_at(closure_id).kind else {
            return None;
        };
        let params: Vec<&str> = params.iter().map(|p| p.name.name.as_str()).collect();
        let mut refs = Vec::new();
        self.collect_names(*body, &mut refs);
        refs.into_iter().find_map(|(n, id)| {
            let captures_borrow = !params.contains(&n.as_str()) && ctx.lookup(&n) == Some(true);
            (captures_borrow && self.info.is_non_copy(id)).then_some(n)
        })
    }

    /// Gather every value name (and `self`) referenced in a subtree, paired with
    /// the referencing expression's id (so its type/Copy-ness is available).
    fn collect_names(&self, id: ExprId, out: &mut Vec<(String, ExprId)>) {
        match &self.ast.expr_at(id).kind {
            ExprKind::Name(n) => out.push((n.name.clone(), id)),
            ExprKind::SelfValue => out.push(("self".to_string(), id)),
            ExprKind::Unary { rhs, .. } => self.collect_names(*rhs, out),
            ExprKind::Binary { lhs, rhs, .. } => {
                self.collect_names(*lhs, out);
                self.collect_names(*rhs, out);
            }
            ExprKind::Assign { target, value, .. } => {
                self.collect_names(*target, out);
                self.collect_names(*value, out);
            }
            ExprKind::Range { lo, hi, .. } => {
                if let Some(l) = lo {
                    self.collect_names(*l, out);
                }
                if let Some(h) = hi {
                    self.collect_names(*h, out);
                }
            }
            ExprKind::Call { callee, args } => {
                self.collect_names(*callee, out);
                for a in args {
                    self.collect_names(*a, out);
                }
            }
            ExprKind::Field { base, .. } => self.collect_names(*base, out),
            ExprKind::Index { base, index } => {
                self.collect_names(*base, out);
                self.collect_names(*index, out);
            }
            ExprKind::Deref { base } => self.collect_names(*base, out),
            ExprKind::Try { base } => self.collect_names(*base, out),
            ExprKind::Cast { expr, .. } => self.collect_names(*expr, out),
            ExprKind::StructLit { fields, .. } | ExprKind::GenStructLit { fields, .. } => {
                for f in fields {
                    self.collect_names(f.value, out);
                }
            }
            ExprKind::If { cond, then, els } => {
                self.collect_names(*cond, out);
                self.collect_block_names(then, out);
                if let Some(e) = els {
                    self.collect_names(*e, out);
                }
            }
            ExprKind::Match { scrut, arms } => {
                self.collect_names(*scrut, out);
                for a in arms {
                    if let Some(g) = a.guard {
                        self.collect_names(g, out);
                    }
                    self.collect_names(a.body, out);
                }
            }
            ExprKind::Block(b) | ExprKind::Unsafe(b) => self.collect_block_names(b, out),
            ExprKind::Closure { body, .. } => self.collect_names(*body, out),
            _ => {}
        }
    }

    fn collect_block_names(&self, b: &Block, out: &mut Vec<(String, ExprId)>) {
        for s in &b.stmts {
            match s {
                Stmt::Let { init: Some(e), .. } => self.collect_names(*e, out),
                Stmt::Return { value: Some(v), .. } => self.collect_names(*v, out),
                Stmt::Expr(e) => self.collect_names(*e, out),
                _ => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::Lexer;
    use crate::parser::Parser;

    /// Lex, parse, type-check, then run the escape checker with the type info.
    fn escapes(src: &str) -> Vec<Diagnostic> {
        let (tokens, lex_diags) = Lexer::new(src).tokenize();
        assert!(lex_diags.is_empty(), "lex errors: {:?}", lex_diags);
        let (ast, parse_diags) = Parser::new(src, tokens).parse();
        assert!(parse_diags.is_empty(), "parse errors: {:?}", parse_diags);
        let (info, _type_diags) = crate::typeck::check(&ast);
        check(&ast, &info)
    }

    // --- the valid example must stay clean ---

    #[test]
    fn accepts_the_vec_example() {
        let src = include_str!("../examples/vec.jtr");
        assert!(escapes(src).is_empty(), "false positives: {:?}", escapes(src));
    }

    // --- the thesis in action: allowed uses ---

    #[test]
    fn allows_passing_a_borrow_down_as_an_argument() {
        // The whole point: second-class borrows flow *down* freely.
        let d = escapes("fn forward(read p: Node) { use_it(p) }");
        assert!(d.is_empty(), "should allow pass-down: {:?}", d);
    }

    #[test]
    fn allows_returning_a_borrow_when_the_signature_says_so() {
        // NB: `out` is a reserved convention keyword, so the fn is named `reborrow`.
        let d = escapes("fn reborrow(read p: Node) -> read Node { p }");
        assert!(d.is_empty(), "borrow-out via convention should be allowed: {:?}", d);
    }

    #[test]
    fn allows_a_computed_value_return() {
        let d = escapes("fn area(read s: Shape) -> f64 { s.w * s.h }");
        assert!(d.is_empty(), "computing a value from a borrow is fine: {:?}", d);
    }

    // --- escapes that must be caught ---

    #[test]
    fn rejects_returning_a_borrow_by_value() {
        let d = escapes("fn leak(read p: Node) -> Node { p }");
        assert_eq!(d.len(), 1, "{:?}", d);
        assert!(d[0].message.contains("cannot return borrow `p`"));
    }

    #[test]
    fn rejects_capturing_a_borrow_in_a_struct() {
        let d = escapes("fn cap(read p: Node) -> Holder { Holder{ inner: p } }");
        assert_eq!(d.len(), 1, "{:?}", d);
        assert!(d[0].message.contains("cannot store borrow `p` in struct `Holder`"));
    }

    #[test]
    fn rejects_storing_a_borrow_through_another_borrow() {
        let d = escapes("fn stash(read p: Node, mut h: Holder) { h.inner = p }");
        assert_eq!(d.len(), 1, "{:?}", d);
        assert!(d[0].message.contains("into borrowed storage"));
    }

    #[test]
    fn tracks_borrow_through_a_let_binding() {
        // `q` aliases the borrow `p`; returning `q` is still an escape.
        let d = escapes("fn alias(read p: Node) -> Node { let q = p return q }");
        assert_eq!(d.len(), 1, "{:?}", d);
        assert!(d[0].message.contains("cannot return borrow `q`"));
    }

    #[test]
    fn catches_escape_in_a_branch() {
        let d = escapes("fn pick(read a: Node, read b: Node, c: bool) -> Node { if c { a } else { b } }");
        assert_eq!(d.len(), 2, "both branches escape: {:?}", d);
    }

    // --- Copy-refinement (the stage-④ payoff) ---

    #[test]
    fn copy_refinement_allows_returning_a_borrowed_scalar() {
        // The structural-only checker flagged this; with types, `i32` is Copy,
        // so returning it by value is a copy, not an escape.
        let d = escapes("fn copy_out(read n: i32) -> i32 { n }");
        assert!(d.is_empty(), "i32 is Copy — should not be an escape: {:?}", d);
    }

    #[test]
    fn copy_refinement_still_rejects_a_non_copy_borrow() {
        // Same shape, but `Node` (a struct) is non-Copy, so it IS an escape.
        let d = escapes("struct Node { v: i32 } fn move_out(read n: Node) -> Node { n }");
        assert_eq!(d.len(), 1, "{:?}", d);
    }

    // --- route 4: giving a borrow to a `take` (owning) parameter ---

    #[test]
    fn rejects_giving_a_borrow_to_a_take_parameter() {
        // The "store into a collection" case: `store_it` consumes its argument.
        let d = escapes(
            "struct Bin { v: i32 } fn store_it(take item: Bin) {} fn bad(read p: Bin) { store_it(p) }",
        );
        assert_eq!(d.len(), 1, "{:?}", d);
        assert!(d[0].message.contains("give borrow `p` to owning parameter `item`"));
    }

    #[test]
    fn default_param_is_a_read_borrow_that_cannot_escape() {
        // MVS: a default (non-`take`) struct parameter is a borrow; returning it
        // by value moves out of the borrow → escape.
        let d = escapes("struct Node { v: i32 } fn keep(n: Node) -> Node { n }");
        assert_eq!(d.len(), 1, "{:?}", d);
        assert!(d[0].message.contains("cannot return borrow `n`"), "{:?}", d);
    }

    #[test]
    fn take_param_owns_and_can_be_returned() {
        let d = escapes("struct Node { v: i32 } fn consume(take n: Node) -> Node { n }");
        assert!(d.is_empty(), "`take` owns, so it can escape: {:?}", d);
    }

    #[test]
    fn copy_default_param_can_be_returned() {
        // A Copy scalar default is duplicated, not referenced — returning it is fine.
        let d = escapes("fn echo(x: i32) -> i32 { x }");
        assert!(d.is_empty(), "i32 is Copy: {:?}", d);
    }

    #[test]
    fn rejects_giving_a_borrow_to_a_take_method_parameter() {
        // The give-away route now also fires through method-call sugar: `store`
        // consumes its second argument, but `p` is only a borrow.
        let d = escapes(
            "struct Bin { v: i32 } fn store(mut b: Bin, take item: Bin) {} \
             fn bad(mut b: Bin, read p: Bin) { b.store(p) }",
        );
        assert_eq!(d.len(), 1, "{:?}", d);
        assert!(d[0].message.contains("give borrow `p`"), "{:?}", d);
    }

    #[test]
    fn allows_giving_a_borrow_to_a_borrowing_parameter() {
        let d = escapes(
            "struct Bin { v: i32 } fn inspect(read x: Bin) {} fn ok(read p: Bin) { inspect(p) }",
        );
        assert!(d.is_empty(), "passing a borrow to a `read` param is fine: {:?}", d);
    }

    // --- closures: completing the capture route ---

    #[test]
    fn rejects_returning_a_closure_that_captures_a_borrow() {
        let d = escapes("struct C { v: i32 } fn use_c(read c: C) {} fn leak(read c: C) -> H { || use_c(c) }");
        assert_eq!(d.len(), 1, "{:?}", d);
        assert!(d[0].message.contains("closure capturing borrow `c`"), "{:?}", d);
    }

    #[test]
    fn allows_returning_a_closure_capturing_only_owned_values() {
        // `n` is owned (default convention), so capturing it is fine.
        let d = escapes("fn other(n: i32) {} fn ok(n: i32) -> H { || other(n) }");
        assert!(d.is_empty(), "owned capture should be fine: {:?}", d);
    }

    #[test]
    fn allows_a_borrow_capturing_closure_that_stays_local() {
        let d = escapes("struct C { v: i32 } fn use_c(read c: C) {} fn local(read c: C) { let f = || use_c(c) }");
        assert!(d.is_empty(), "a non-escaping closure is fine: {:?}", d);
    }

    // --- loops: a slice-element binding is a borrow that may not escape ---

    #[test]
    fn rejects_returning_a_loop_element_borrow() {
        // `for x in xs` binds each element as a borrow *into* the slice; returning
        // a non-Copy element out of the loop is an escape (the loop-binding half of
        // the loop borrow contract — see docs/loops-spec.md).
        let d = escapes("struct N { v: i32 } fn leak(xs: []N) -> N { for x in xs { return x } return xs[0] }");
        assert!(
            d.iter().any(|m| m.message.contains("cannot return borrow `x`")),
            "the loop element must not escape: {:?}",
            d
        );
    }

    #[test]
    fn rejects_mutating_a_collection_while_iterating_it() {
        // Iterator invalidation: passing the iterated slice to a `mut` parameter.
        let d = escapes("fn grow(mut xs: []i32, x: i32) {} fn bad(mut xs: []i32) { for e in xs { grow(xs, e) } }");
        assert!(
            d.iter().any(|m| m.message.contains("while iterating it")),
            "mutating the iterated collection must be rejected: {:?}",
            d
        );
    }

    #[test]
    fn rejects_element_store_into_a_collection_being_iterated() {
        let d = escapes("fn bad(xs: []i32) { for x in xs { xs[0] = x } }");
        assert!(d.iter().any(|m| m.message.contains("while iterating it")), "{:?}", d);
    }

    #[test]
    fn allows_mutating_the_loop_binding_in_place() {
        // `for mut x` mutates the element via `x`, not via the collection name.
        let d = escapes("fn ok(mut xs: []i32) { for mut x in xs { x = x * 2 } }");
        assert!(d.is_empty(), "in-place element mutation is fine: {:?}", d);
    }

    #[test]
    fn allows_a_copy_loop_element_to_be_returned() {
        // A Copy element (i32) is duplicated out, not referenced — not an escape.
        let d = escapes("fn first(xs: []i32) -> i32 { for x in xs { return x } return 0 }");
        assert!(d.is_empty(), "a Copy element may be returned: {:?}", d);
    }

    #[test]
    fn rejects_storing_a_borrow_capturing_closure_in_a_struct() {
        let d = escapes(
            "struct C { v: i32 } struct Box { h: i32 } fn use_c(read c: C) {} \
             fn stash(read c: C) -> Box { Box{ h: || use_c(c) } }",
        );
        assert_eq!(d.len(), 1, "{:?}", d);
        assert!(d[0].message.contains("store borrow"), "{:?}", d);
    }
}
