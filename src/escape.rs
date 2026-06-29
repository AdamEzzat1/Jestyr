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

use std::collections::{HashMap, HashSet};

use crate::ast::*;
use crate::diag::Diagnostic;
use crate::span::Span;
use crate::types::{Ty, TypeInfo};

pub fn check(ast: &Ast, info: &TypeInfo) -> Vec<Diagnostic> {
    let mut ck = Checker {
        ast,
        info,
        diags: Vec::new(),
        frozen: Vec::new(),
        region_depths: Vec::new(),
        no_alloc: false,
    };
    for item in &ast.items {
        ck.check_item(item);
    }
    ck.diags
}

struct Checker<'a> {
    ast: &'a Ast,
    info: &'a TypeInfo,
    diags: Vec<Diagnostic>,
    /// Is the function currently being checked `@no_alloc`? If so, any allocation
    /// (a heap/arena intrinsic, a `region` block, a region-scoped loop) is a
    /// compile error — the enforced allocation-free contract (the `@no_panic`
    /// analog). Saved/restored around nested method bodies.
    no_alloc: bool,
    /// Collections currently being iterated (by simple name). A `for … in xs`
    /// loop holds a borrow of `xs` for its body, so mutating `xs` there is
    /// forbidden (iterator invalidation — the borrow contract of a loop). A stack
    /// because loops nest.
    frozen: Vec<String>,
    /// The scope depth (`ctx.scopes.len()`) at the entry of each active `region`
    /// block. A binding at a shallower scope is *outside* the region, so storing a
    /// region-allocated value into it would let the value outlive its arena.
    region_depths: Vec<usize>,
}

/// Per-function analysis state: a stack of lexical scopes mapping each in-scope
/// binding to whether it denotes a borrow, plus this function's return mode.
struct FnCtx {
    scopes: Vec<HashMap<String, bool>>,
    /// Names bound to a **region-allocated** value (from `region_str`/`region_alloc`/
    /// `region_concat`). Such a value is owned by its arena and may not escape — the
    /// region-safety proof (design §4.4).
    region: Vec<HashSet<String>>,
    ret_is_borrow: bool,
}

impl FnCtx {
    fn push(&mut self) {
        self.scopes.push(HashMap::new());
        self.region.push(HashSet::new());
    }
    fn pop(&mut self) {
        self.scopes.pop();
        self.region.pop();
    }
    fn bind(&mut self, name: &str, is_borrow: bool) {
        self.scopes.last_mut().unwrap().insert(name.to_string(), is_borrow);
    }
    fn bind_region(&mut self, name: &str) {
        self.region.last_mut().unwrap().insert(name.to_string());
    }
    fn is_region(&self, name: &str) -> bool {
        self.region.iter().any(|s| s.contains(name))
    }
    /// The scope index where `name` is bound (innermost wins) — for deciding
    /// whether an assignment target lives *outside* a region block.
    fn scope_depth_of(&self, name: &str) -> Option<usize> {
        self.scopes.iter().enumerate().rev().find(|(_, s)| s.contains_key(name)).map(|(i, _)| i)
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
            // Trait/impl method bodies are escape-checked once their resolution
            // lands (Stage B); a bare signature has nothing to check.
            Item::Trait(_) | Item::Impl(_) => {}
        }
    }

    fn check_fn(&mut self, f: &FnDecl) {
        let ret_is_borrow = matches!(f.ret_conv, Conv::Read | Conv::Mut | Conv::Out);
        let mut ctx = FnCtx { scopes: Vec::new(), region: Vec::new(), ret_is_borrow };
        ctx.push();
        for p in &f.params {
            // MVS (design §4.3): the default convention *is* `read` (an immutable
            // borrow). Only `take` transfers ownership — so every non-comptime,
            // non-`take` parameter is a borrow that may not escape its frame.
            let is_borrow = !p.comptime && p.conv != Conv::Take;
            let name = if p.is_self { "self" } else { p.name.name.as_str() };
            ctx.bind(name, is_borrow);
        }
        // `@no_alloc` is per-function — save/restore so a nested method body does
        // not inherit (or clobber) the enclosing function's contract.
        let saved_no_alloc = self.no_alloc;
        self.no_alloc = f.has_attr("no_alloc");
        // The body is in return position: its tail expression is the result.
        self.check_block(&mut ctx, &f.body, true);
        self.no_alloc = saved_no_alloc;
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
                        // A binding initialized from a region-allocated value is
                        // itself region-tainted, so the taint flows through `let`s.
                        if self.is_region_value(ctx, *e) {
                            ctx.bind_region(&name.name);
                        }
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
            ExprKind::FString { exprs, .. } => {
                // Interpolations are read for formatting — never a tail position.
                for e in exprs {
                    self.walk_expr(ctx, *e, false);
                }
                return;
            }
            ExprKind::StructLit { path, fields, spread } => {
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
                if let Some(s) = spread {
                    self.walk_expr(ctx, *s, false);
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
                // Region-safety (assign-to-outer): storing a region-allocated value
                // into a binding declared *outside* the current `region` block lets
                // it outlive the arena. The taint flows so the outer binding is
                // region-marked too (a later `return` of it is then also caught).
                if let Some(&region_depth) = self.region_depths.last() {
                    if self.is_region_value(ctx, *value) && self.carries_arena_ref(*value) {
                        if let ExprKind::Name(n) = &self.ast.expr_at(*target).kind {
                            let n = n.name.clone();
                            match ctx.scope_depth_of(&n) {
                                Some(d) if d < region_depth => self.error(
                                    span,
                                    format!(
                                        "cannot store region-allocated value into `{n}`: it is declared outside the \
                                         `region` block and would outlive the arena (copy it into an owned `String`)"
                                    ),
                                ),
                                _ => ctx.bind_region(&n),
                            }
                        }
                    }
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
                self.check_no_alloc_call(id, *callee, span);
                self.check_manual_drop(id, span);
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
            ExprKind::ArrayRepeat { value, count } => {
                // A `[v; N]` array literal is a value (it copies `v` N times); walk
                // both sub-expressions, neither in tail/escape position.
                self.walk_expr(ctx, *value, false);
                self.walk_expr(ctx, *count, false);
            }
            ExprKind::ArrayLit { elems } => {
                // `[e0, e1, …]` is a value; walk each element, none escaping.
                for e in elems {
                    self.walk_expr(ctx, *e, false);
                }
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
                // Data-race safety. Structured concurrency already gives join-safety
                // (a task can't outlive its scope, so a borrow into it stays frame-
                // safe). The remaining hazard is two tasks writing the *same* memory.
                // In the safe subset the only shareable mutable handle is a slice
                // (a `mut [N]T` value-array arg is copied into each task, so it can't
                // alias) — so a `mut`/`out` slice parameter on a spawn target is the
                // one way to create a safe data race. Forbid it: shared mutable state
                // across tasks must go through a raw `*mut T` in `unsafe`, where the
                // programmer asserts the regions are disjoint (as `par_binned_sum`
                // does — each worker gets its own region). A general *proof* that two
                // raw pointers don't overlap needs range-aware alias analysis (e.g.
                // `raw+0` vs `raw+2048`), which is out of scope; this rule keeps the
                // safe subset race-free and makes the unsafe boundary explicit.
                self.check_spawn_no_shared_mut_slice(*call);
                self.walk_expr(ctx, *call, false);
                return;
            }
            ExprKind::Region { body, .. } => {
                // A `region` block opens an arena (a heap allocation), so it is
                // forbidden in a `@no_alloc` function.
                if self.no_alloc {
                    self.error(span, "a `region` block allocates an arena — forbidden in a `@no_alloc` function");
                }
                // Record the scope depth at entry: anything bound shallower than
                // this is *outside* the region (a region value stored there escapes).
                self.region_depths.push(ctx.scopes.len());
                self.check_block(ctx, body, false);
                self.region_depths.pop();
                return;
            }
            ExprKind::For { head, body, els, region, .. } => {
                // A region-scoped loop allocates a per-iteration scratch arena.
                if self.no_alloc && region.is_some() {
                    self.error(span, "a region-scoped loop allocates a scratch arena — forbidden in a `@no_alloc` function");
                }
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

        // Region-safety: a region-allocated value is owned by its arena, which is
        // freed at the end of its `region` block — so it can never be returned (it
        // would dangle). This is the static proof behind zero-alloc region text.
        if tail && self.is_region_value(ctx, id) && self.carries_arena_ref(id) {
            let name = self.root_name(ctx, id);
            self.error(
                span,
                format!(
                    "cannot return region-allocated value `{name}`: it is owned by its `region` \
                     arena and does not outlive it (copy it into an owned `String` to return it)"
                ),
            );
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

    /// Does this expression's type carry a *pointer into* the arena (so letting it
    /// escape dangles)? A `str`/`os_str` view, a raw/region pointer, or a slice —
    /// but **not** a scalar projected out of a region value (e.g. `g.len`, which is
    /// a fresh `usize`). `Unknown` is included (the region intrinsics return it).
    fn carries_arena_ref(&self, id: ExprId) -> bool {
        matches!(
            self.info.type_of(id),
            Ty::Prim("str")
                | Ty::Prim("os_str")
                | Ty::Ptr { .. }
                | Ty::Slice(_)
                | Ty::RegionRef(_)
                | Ty::Unknown
        )
    }

    /// Is this expression a region-allocated value — a `region_str`/`region_alloc`/
    /// `region_concat` call, a binding tainted by one, or a projection of either?
    fn is_region_value(&self, ctx: &FnCtx, id: ExprId) -> bool {
        match &self.ast.expr_at(id).kind {
            ExprKind::Call { callee, .. } => matches!(
                &self.ast.expr_at(*callee).kind,
                ExprKind::Name(n)
                    if matches!(n.name.as_str(), "region_str" | "region_alloc" | "region_concat")
            ),
            ExprKind::Name(n) => ctx.is_region(&n.name),
            ExprKind::Field { base, .. }
            | ExprKind::Index { base, .. }
            | ExprKind::Deref { base } => self.is_region_value(ctx, *base),
            _ => false,
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

    /// Reject a `spawn` whose target takes a `mut`/`out` **slice** parameter — a
    /// shared mutable slice across parallel tasks can race (its `ptr` aliases). The
    /// safe way to share mutable state across tasks is a raw `*mut T` in `unsafe`.
    fn check_spawn_no_shared_mut_slice(&mut self, call: ExprId) {
        let ExprKind::Call { callee, .. } = &self.ast.expr_at(call).kind else { return };
        let ExprKind::Name(n) = &self.ast.expr_at(*callee).kind else { return };
        let Some(sig) = self.info.table.fns.get(&n.name) else { return };
        let mut hit: Option<String> = None;
        for p in &sig.params {
            if matches!(p.conv, Conv::Mut | Conv::Out) && matches!(p.ty, Ty::Slice(_)) {
                hit = Some(p.name.clone());
                break;
            }
        }
        if let Some(pname) = hit {
            let span = self.ast.expr_at(call).span;
            let fname = n.name.clone();
            self.error(
                span,
                format!(
                    "`spawn`: `{fname}` takes a `mut` slice `{pname}` — a shared mutable slice can \
                     race across parallel tasks. Share mutable state through a raw `*mut T` in \
                     `unsafe` (each task a disjoint region, as `par_binned_sum` does), or pass it `read`."
                ),
            );
        }
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
            // A module-qualified call (`mod.f(…)`) has a `Field` callee; typeck
            // recorded its resolved (canonical) target in `qualified`, keyed the same
            // as `table.fns`. Without this arm a `take` parameter reached through a
            // qualified call — e.g. the move-only `sync.channel_send(T, ch, take v)` —
            // would silently skip the give-away check, letting a *borrow* be sent.
            ExprKind::Field { .. } => match self.info.qualified.get(&call_id) {
                Some(q) => q.clone(),
                None => return,
            },
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

    /// Reject a manual `value.drop()` call. `drop` is run *automatically* by the
    /// compiler at scope exit (in reverse declaration order); calling it by hand
    /// would double-free (the auto-drop still fires). The destructor is
    /// inspectable (`--show-drops`) but not hand-callable — Rust's rule.
    fn check_manual_drop(&mut self, call_id: ExprId, span: Span) {
        if let Some(ic) = self.info.impl_calls.get(&call_id) {
            if ic.trait_name == "Drop" && ic.method == "drop" {
                self.error(
                    span,
                    "cannot call `drop` manually — it runs automatically at scope exit (see `--show-drops`)",
                );
            }
        }
    }

    /// In a `@no_alloc` function, reject a call to an allocation intrinsic
    /// (`alloc`/`realloc`/`arena_*`/`region_*`/`gen_new`), whether bare or module-
    /// qualified (`mem.allocate` resolves to its bare name). This is the direct,
    /// per-op enforcement that mirrors `@no_panic`'s un-elided-index check; the
    /// *transitive* "calls a function that allocates" closure is future work.
    fn check_no_alloc_call(&mut self, call_id: ExprId, callee: ExprId, span: Span) {
        if !self.no_alloc {
            return;
        }
        // A module-qualified call resolves to the underlying bare function name.
        let name = if let Some(q) = self.info.qualified.get(&call_id) {
            q.clone()
        } else if let ExprKind::Name(n) = &self.ast.expr_at(callee).kind {
            n.name.clone()
        } else {
            return;
        };
        if is_alloc_intrinsic(&name) {
            self.error(
                span,
                format!(
                    "`{name}` allocates — forbidden in a `@no_alloc` function (the proven-allocation-free contract)"
                ),
            );
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
            PatKind::StructVariant { fields, .. } => {
                for (_, sp) in fields {
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
            ExprKind::StructLit { fields, spread, .. } => {
                for f in fields {
                    self.collect_names(f.value, out);
                }
                if let Some(s) = spread {
                    self.collect_names(*s, out);
                }
            }
            ExprKind::GenStructLit { fields, .. } => {
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

/// Does `name` denote a backend allocation intrinsic — a call that obtains fresh
/// memory (heap `malloc`/`realloc`, an arena open, or an arena bump)? These are
/// the operations a `@no_alloc` function may not perform. (`free_ptr`/`arena_close`
/// release memory and are allowed; they don't allocate.)
fn is_alloc_intrinsic(name: &str) -> bool {
    matches!(
        name,
        "alloc"
            | "alloc_i32"
            | "realloc"
            | "realloc_i32"
            | "arena_open"
            | "arena_alloc"
            | "region_alloc"
            | "region_str"
            | "region_concat"
            | "gen_new"
    )
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

    // --- @no_alloc: the enforced allocation-free contract (Phase 3) ---

    #[test]
    fn no_alloc_rejects_a_heap_allocation() {
        let d = escapes("@no_alloc fn f(n: i32) -> i32 { let p = alloc(i32, 4) free_ptr(p) return n }");
        assert_eq!(d.len(), 1, "{:?}", d);
        assert!(d[0].message.contains("@no_alloc"), "{:?}", d);
        assert!(d[0].message.contains("alloc"), "{:?}", d);
    }

    #[test]
    fn no_alloc_rejects_a_region_block() {
        let d = escapes("@no_alloc fn f() -> i32 { region r { let x = region_alloc(r, i32, 1) } return 0 }");
        assert!(!d.is_empty(), "a region block must be rejected: {:?}", d);
        assert!(d.iter().any(|m| m.message.contains("region")), "{:?}", d);
    }

    #[test]
    fn cannot_call_drop_manually() {
        let d = escapes(
            "trait Drop { fn drop(mut self) } struct R { id: i32 } \
             impl Drop for R { fn drop(mut self) { print_int(self.id) } } \
             fn f() { let a = R{ id: 1 } a.drop() }",
        );
        assert!(d.iter().any(|m| m.message.contains("cannot call `drop` manually")), "{:?}", d);
    }

    #[test]
    fn no_alloc_accepts_an_allocation_free_body() {
        let d = escapes("@no_alloc fn f(a: i32, b: i32) -> i32 { let s = a + b return s }");
        assert!(d.is_empty(), "allocation-free body must pass: {:?}", d);
    }

    #[test]
    fn no_alloc_is_per_function_not_inherited() {
        // The plain `g` may allocate even when a `@no_alloc` `f` exists alongside.
        let d = escapes(
            "@no_alloc fn f(n: i32) -> i32 { return n } \
             fn g() -> i32 { let p = alloc(i32, 4) free_ptr(p) return 0 }",
        );
        assert!(d.is_empty(), "only the annotated fn is constrained: {:?}", d);
    }

    // --- the thesis in action: allowed uses ---

    #[test]
    fn allows_passing_a_borrow_down_as_an_argument() {
        // The whole point: second-class borrows flow *down* freely.
        let d = escapes("fn forward(read p: Node) { use_it(p) }");
        assert!(d.is_empty(), "should allow pass-down: {:?}", d);
    }

    #[test]
    fn non_copy_struct_param_cannot_be_returned() {
        // By default a user aggregate is non-Copy: returning a `read` param escapes.
        let d = escapes("struct V { x: i32 } fn id(read v: V) -> V { return v }");
        assert_eq!(d.len(), 1, "{:?}", d);
        assert!(d[0].message.contains("cannot return borrow"), "{:?}", d);
    }

    #[test]
    fn a_fn_pointer_is_first_class_and_escapes_freely() {
        // The split the design memo predicted: a borrow-capturing closure stays
        // second-class, but a *thin* fn-pointer captures nothing — it is Copy and
        // first-class, so returning one is not an escape (contrast the test above,
        // where a non-Copy aggregate borrow cannot be returned).
        let d = escapes("fn pick(f: fn(i32) -> i32) -> fn(i32) -> i32 { return f }");
        assert!(d.is_empty(), "returning a thin fn-pointer is allowed: {:?}", d);
    }

    #[test]
    fn region_value_cannot_be_returned() {
        // The marquee region-safety proof: a region-allocated value can't escape.
        let d = escapes(
            "fn f() -> str { region r { let g: str = region_concat(r, \"a\", \"b\") return g } return \"\" }",
        );
        assert!(d.iter().any(|m| m.message.contains("region-allocated")), "{:?}", d);
    }

    #[test]
    fn region_value_assigned_to_outer_binding_escapes() {
        // Assign-to-outer: storing a region value into a binding declared outside
        // the `region` block lets it outlive the arena.
        let d = escapes(
            "fn f() -> i32 { var saved: str = \"\" region r { saved = region_concat(r, \"a\", \"b\") } return saved.len as i32 }",
        );
        assert!(
            d.iter().any(|m| m.message.contains("declared outside the `region`")),
            "{:?}",
            d
        );
    }

    #[test]
    fn region_value_assigned_to_inner_binding_is_fine() {
        // Assigning to a region-*local* binding (declared inside the block) is OK.
        let d = escapes(
            "fn f() -> i32 { region r { var local: str = \"\" local = region_concat(r, \"a\", \"b\") return local.len as i32 } return 0 }",
        );
        assert!(d.is_empty(), "in-region local assign is fine: {:?}", d);
    }

    #[test]
    fn region_value_used_in_scope_is_fine() {
        // Using a region value *inside* its region (not returning it) is allowed —
        // no false positive.
        let d = escapes(
            "fn f() -> i32 { var n: i32 = 0 region r { let g: str = region_concat(r, \"a\", \"b\") n = g.len as i32 } return n }",
        );
        assert!(d.is_empty(), "in-scope use must stay clean: {:?}", d);
    }

    #[test]
    fn copy_struct_param_can_be_returned() {
        // `@copy` opts the aggregate into being freely copyable — no escape.
        let d = escapes("@copy struct V { x: i32 } fn id(read v: V) -> V { return v }");
        assert!(d.is_empty(), "a @copy aggregate may be returned by value: {:?}", d);
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

    // --- data-race safety: a spawn target may not take a shared mutable slice ---

    #[test]
    fn rejects_spawn_with_mut_slice_param() {
        // A `mut []i64` worker would let two tasks alias the same backing store.
        let d = escapes(
            "fn w(mut s: []i64) { s[0] = 1 } \
             fn main() -> i32 { var p: *mut i64 = alloc(i64, 4) var s: []i64 = slice(i64, p, 4) \
             concurrent { spawn w(s) } free_ptr(p) return 0 }",
        );
        assert!(d.iter().any(|m| m.message.contains("shared mutable slice")), "{:?}", d);
    }

    #[test]
    fn accepts_spawn_with_raw_pointer_and_read_slice() {
        // The `par_binned_sum` shape: a raw `*mut` (the unsafe sharing hatch) plus a
        // `read` slice (shared read, no race) — both fine.
        let d = escapes(
            "fn w(read chunk: []f64, acc: *mut i64) { unsafe { acc.* = 0 } } \
             fn main() -> i32 { var p: *mut f64 = alloc(f64, 4) var s: []f64 = slice(f64, p, 4) \
             var a: *mut i64 = alloc(i64, 1) concurrent { spawn w(s, a) } free_ptr(p) free_ptr(a) return 0 }",
        );
        assert!(!d.iter().any(|m| m.message.contains("shared mutable slice")), "false positive: {:?}", d);
    }
}
