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
    let alloc_via = alloc_closure(ast, info);
    let mut ck = Checker {
        ast,
        info,
        diags: Vec::new(),
        frozen: Vec::new(),
        region_depths: Vec::new(),
        no_alloc: false,
        deterministic: false,
        allocates: false,
        calls: Vec::new(),
        alloc_via,
        unresolved: Vec::new(),
    };
    for item in &ast.items {
        ck.check_item(item);
    }
    // The `Unknown` FINALIZATION (safety mosaic, item 1). `Ty::Unknown` is `Copy`
    // on purpose, so expressions the checker could not type raise no escapes and
    // no cascades. At the two sites where copy-ness decides an outcome, though,
    // that reads "unresolved" as "copyable" and lets a *borrow* leave the frame.
    // The census that motivated this found exactly one such expression in the
    // corpus — a struct-variant binding, benign only because its field happened to
    // be `f64`; with a non-`Copy` field the same path lost a real escape. That
    // root cause is fixed, so all 155 corpus files yield none of these and no
    // corpus diagnostic moved.
    //
    // Two ill-formed shapes used to reach it — a field on a bracket type
    // parameter (`x.v` on `T`) and a field on a primitive (`p.v.w` where `v` is
    // `i32`). Both used to compile CLEAN, then this gate refused them, and now
    // typeck rejects them AT THE ACCESS with a field-shaped message (the
    // follow-up this comment used to promise): they type `Error`, not `Unknown`,
    // so this gate is silent for them. What remains here is the true backstop —
    // any *other* borrow whose type never resolved (an indexed `T`, a shape
    // typeck cannot yet name) is still refused rather than assumed copyable, and
    // the tests pin one such probe so the gate cannot go vacuous.
    //
    // Sorted by span start for the same reason the unsafe pass below is: the port
    // reaches the same set by its own traversal, and the sort is what makes two
    // collection strategies emit identically.
    //
    // An error, not a warning, and for the same reason the unsafe rung is: the
    // port's `jc build` refuses on any escape diagnostic (it has no severity
    // model), so a warning here would be a program `jestyrc` builds and `jc` will
    // not.
    let mut unresolved = std::mem::take(&mut ck.unresolved);
    unresolved.sort_by_key(|(id, _)| (ast.expr_at(*id).span.start, id.0));
    for (id, name) in unresolved {
        ck.diags.push(
            Diagnostic::new(
                format!("cannot decide whether borrow `{name}` escapes: its type was never resolved"),
                ast.expr_at(id).span,
            )
            .with_help(
                "the escape rule turns on whether the value is copied or moved, so an unresolved \
                 type cannot be checked soundly — annotate the binding, or simplify the expression \
                 it comes from",
            ),
        );
    }
    // The unsafe boundary, ENFORCED (ownership v2, step 4 — the ladder's last rung):
    // every raw-pointer operation outside an `unsafe` block is an error. Reuses
    // `provenance::collect` — the same decision point as the `jestyrc unsafe` report,
    // so the report and the check cannot drift (the `at_ty`/`simd::classify` rule).
    // The corpus was migrated to zero uncovered sites first and is pinned there, so
    // enforcement broke nothing that existed.
    //
    // Error rather than warning also ALIGNS the two drivers: the port's `jc build`
    // refuses on any escape diagnostic (it has no severity model), so as a warning
    // this was a program `jestyrc` built and `jc` refused.
    //
    // Sorted by span start: the port mirror collects the same sites by a *flat
    // arena scan with span containment* rather than by mirroring this walk, and the
    // sort is what makes the two collection strategies emit identically.
    let mut sites: Vec<crate::provenance::Site> = crate::provenance::collect(ast, info)
        .into_iter()
        .filter(|s| !s.covered)
        .collect();
    sites.sort_by_key(|s| s.span.start);
    for s in sites {
        let msg = match s.op {
            crate::provenance::Op::Deref => "a raw-pointer deref belongs in an `unsafe` block",
            crate::provenance::Op::Arith => "raw-pointer arithmetic belongs in an `unsafe` block",
            crate::provenance::Op::IntToPtr => "an int-to-pointer cast belongs in an `unsafe` block",
        };
        ck.diags.push(
            Diagnostic::new(msg, s.span).with_help(
                "the compiler cannot check a raw pointer's validity; `unsafe { … }` marks the \
                 obligation's extent (docs/unsafe-contract.md) — or use a checked form: genref, \
                 slice, region",
            ),
        );
    }
    ck.diags
}

/// For every top-level function that allocates **transitively**, the shortest call
/// chain from it to a function that allocates *directly*.
///
/// ## Why this reuses the checker instead of restating "allocates"
/// The direct rule already exists in three places — an allocation intrinsic, a `region`
/// block, a region-scoped loop. Writing a second walker that looked for those would be
/// two definitions of "allocates" that could drift, and the one that drifted would make
/// `@no_alloc` claim a proof it does not have. So this runs the **real checker** over
/// each function with the per-op rules recording into `allocates`/`calls`, and reads
/// those out. One decision point, two consumers — the rule this codebase applies to
/// `at_ty`, `simd::classify` and `layout::field_order`.
///
/// The diagnostics from those probe runs are discarded: they belong to the main pass,
/// which reports each function once with the right `no_alloc` flag.
///
/// ## What it deliberately does not resolve
/// Only **free functions**, resolved by name. A method, a closure, or a call through a
/// `fn(…)` pointer is not in the graph, so a `@no_alloc` function that allocates through
/// one is not caught. That is a real limit, not an oversight — closing it needs
/// call-graph resolution the escape checker does not have today — and it is recorded in
/// `docs/attributes.md` rather than left for a user to discover.
fn alloc_closure(ast: &Ast, info: &TypeInfo) -> HashMap<String, Vec<String>> {
    // Per function: does it allocate directly, and whom does it call?
    let mut direct: HashSet<String> = HashSet::new();
    let mut calls: HashMap<String, Vec<String>> = HashMap::new();
    for item in &ast.items {
        let Item::Fn(f) = item else { continue };
        let mut probe = Checker {
            ast,
            info,
            diags: Vec::new(),
            frozen: Vec::new(),
            region_depths: Vec::new(),
            // `false`, so the probe never reports: it is measuring, not judging.
            no_alloc: false,
            deterministic: false,
            allocates: false,
            calls: Vec::new(),
            alloc_via: HashMap::new(),
            unresolved: Vec::new(),
        };
        probe.check_item(item);
        if probe.allocates {
            direct.insert(f.name.name.clone());
        }
        let mut cs: Vec<String> = probe.calls.into_iter().map(|(n, _)| n).collect();
        cs.sort();
        cs.dedup();
        calls.insert(f.name.name.clone(), cs);
    }

    // Least fixpoint: a function allocates if it calls one that does. Iterated to
    // saturation rather than recursed, so a cycle (mutual or self recursion) settles
    // instead of looping — the same totality instinct the comptime interpreter applies.
    // A directly-allocating function is reached via an EMPTY chain — it is itself the
    // culprit. Seeding it with its own name instead would duplicate that name in every
    // chain that passes through it.
    let mut via: HashMap<String, Vec<String>> = HashMap::new();
    for d in &direct {
        via.insert(d.clone(), Vec::new());
    }
    loop {
        let mut changed = false;
        // Sorted for determinism: with two chains of equal length the winner must not
        // depend on hash iteration order, or the diagnostic text would vary per run.
        let mut names: Vec<&String> = calls.keys().collect();
        names.sort();
        for f in names {
            if direct.contains(f) {
                continue; // already the shortest possible chain
            }
            let mut best: Option<Vec<String>> = via.get(f).cloned();
            for callee in &calls[f] {
                let Some(sub) = via.get(callee) else { continue };
                let mut cand = vec![callee.clone()];
                cand.extend(sub.iter().cloned());
                let better = match &best {
                    None => true,
                    Some(b) => cand.len() < b.len() || (cand.len() == b.len() && cand < *b),
                };
                if better {
                    best = Some(cand);
                }
            }
            if let Some(b) = best {
                if via.get(f) != Some(&b) {
                    via.insert(f.clone(), b);
                    changed = true;
                }
            }
        }
        if !changed {
            break;
        }
    }
    // Directly-allocating functions STAY in the map, with their empty chains. The
    // direct rule in `check_no_alloc_call` only recognizes allocation *intrinsics*, so
    // dropping them here would let a one-hop call to a user function that allocates
    // (`@no_alloc fn f() { g() }` where `g` calls `alloc`) pass unreported — which it
    // did, until this comment's test caught it.
    via
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
    /// Is the function currently being checked `@deterministic`? If so, the raw
    /// concurrency primitives whose result can depend on the thread schedule —
    /// `concurrent`/`spawn` and the `atomic_*` ops — are compile errors; parallelism
    /// is permitted only through the *checked* deterministic `par for … reduce(r)`.
    /// The schedule-independence contract. Saved/restored around nested bodies.
    deterministic: bool,
    /// Set when the function being walked performs an allocation **directly** — an
    /// allocation intrinsic, a `region` block, or a region-scoped loop. Recorded
    /// regardless of `no_alloc`, because [`alloc_closure`] measures every function
    /// before it knows which ones are annotated.
    allocates: bool,
    /// Every resolved callee name seen while walking, with its span — the call-graph
    /// edges [`alloc_closure`] closes over.
    calls: Vec<(String, Span)>,
    /// Functions that allocate **transitively**, each mapped to the shortest chain
    /// reaching a directly-allocating one. Empty during the measuring pass.
    alloc_via: HashMap<String, Vec<String>>,
    /// Borrow places whose type inference never resolved, with the root binding's
    /// name — the `Unknown` finalization, drained and emitted sorted in [`check`].
    /// Collected by the [`alloc_closure`] probe too, and discarded there with the
    /// rest of its findings: that pass measures, it does not judge.
    unresolved: Vec<(ExprId, String)>,
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

    /// An error carrying a **suggested rewrite** (Diagnostics tier 3).
    ///
    /// The suggestion goes in `help`, never in the message. Two reasons, and the second
    /// is load-bearing: a suggestion is advice and a message is a fact, so they should
    /// render differently; and the P4 escape golden compares the port's diagnostics
    /// against these by **span + message**, so changing a message would diverge the two
    /// implementations while adding a help line does not. Suggestions can therefore be
    /// improved freely on the reference side without owing a port mirror.
    fn error_help(&mut self, span: Span, message: impl Into<String>, help: impl Into<String>) {
        self.diags.push(Diagnostic::new(message, span).with_help(help));
    }

    /// The suggestion for **storing a borrow somewhere that outlives the call** — the
    /// single most common way to hit the escape rule, and the one where "you may not do
    /// that" is least actionable on its own.
    ///
    /// Jestyr's three answers, in the order a user should try them: give the storage
    /// ownership (`take`), put the value in an arena whose lifetime is explicit
    /// (`region`), or store a checked handle instead of a pointer (`genref`). Naming
    /// all three matters — which one is right depends on whether the value is moved,
    /// long-lived, or shared, and the compiler cannot know that.
    const STORE_ESCAPE_HELP: &'static str =
        "a stored value must outlive the call: pass it as `take` to transfer ownership, \
         allocate it in a `region` if it must outlive this frame, or store a `genref` handle \
         instead of a borrow";

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
        let saved_det = self.deterministic;
        self.deterministic = f.has_attr("deterministic");
        // The body is in return position: its tail expression is the result.
        self.check_block(&mut ctx, &f.body, true);
        self.no_alloc = saved_no_alloc;
        self.deterministic = saved_det;
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
            // A `comptime` block never becomes runtime code — it becomes a literal —
            // so it owns nothing, borrows nothing, and cannot make anything escape.
            // Descending would analyse dataflow that will not exist in the binary.
            ExprKind::Comptime(_) => return,
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
                        self.error_help(
                            ast.expr_at(fi.value).span,
                            format!(
                                "cannot store borrow `{name}` in struct `{}`: a second-class borrow may not outlive its call",
                                path.name
                            ),
                            Self::STORE_ESCAPE_HELP,
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
                        self.error_help(
                            ast.expr_at(fi.value).span,
                            format!(
                                "cannot store borrow `{name}` in struct `{}`: a second-class borrow may not outlive its call",
                                ctor.name
                            ),
                            Self::STORE_ESCAPE_HELP,
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
                    self.error_help(
                        span,
                        format!(
                            "cannot store borrow `{name}` into borrowed storage: it would outlive its call"
                        ),
                        Self::STORE_ESCAPE_HELP,
                    );
                }
                // Region-safety (assign-to-outer): storing a region-allocated value
                // into a binding declared *outside* the current `region` block lets
                // it outlive the arena. The taint flows so the outer binding is
                // region-marked too (a later `return` of it is then also caught).
                if let Some(&region_depth) = self.region_depths.last() {
                    if self.is_region_value(ctx, *value) && self.carries_arena_ref(*value) {
                        match &self.ast.expr_at(*target).kind {
                            ExprKind::Name(n) => {
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
                            // A store THROUGH a place chain (`h.*.p = region_alloc(inner, …)`)
                            // whose ROOT binding was declared outside the current `region`
                            // block reaches storage that outlives the arena — the same dangle
                            // as the bare-binding case, one deref deeper. This was a
                            // demonstrated use-after-free that compiled clean (mosaic item 5's
                            // motivating hole, closed lexically — no brands needed for the
                            // root-outside shape). Honest limits, recorded in
                            // `docs/safety-mosaic-next.md`: a root ALIASED inside the region
                            // to outer storage, or a store performed by a callee, still gets
                            // through — that residue is what a type-level mechanism would buy.
                            ExprKind::Field { .. } | ExprKind::Index { .. } | ExprKind::Deref { .. } => {
                                let root = self.root_name(ctx, *target);
                                if let Some(d) = ctx.scope_depth_of(&root) {
                                    if d < region_depth {
                                        self.error(
                                            span,
                                            format!(
                                                "cannot store region-allocated value through `{root}`: it is declared \
                                                 outside the `region` block, so this storage outlives the arena \
                                                 (copy the value into an owned `String`, or allocate it in the outer region)"
                                            ),
                                        );
                                    }
                                }
                            }
                            _ => {}
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
                // Error-payloads E3: `err(Name(p))` RETURNS its payload — the value
                // rides the result out of the frame — so `p` is walked in RETURN
                // position and the existing return-borrow rules do the work (a
                // `str` payload rooted in a borrow place is refused; a literal or
                // owned scalar passes). Fires only for the real error constructor:
                // a user fn or enum variant named `err` shadows it (the corpus's
                // `Result(T, E)` does), and only for a declared payload name.
                if let Some((ic, p)) = self.err_payload_form(*callee, args) {
                    self.walk_expr(ctx, *callee, false);
                    self.walk_expr(ctx, ic, false);
                    self.walk_expr(ctx, p, true);
                    self.check_give_away(ctx, id, *callee, args);
                    self.check_loop_mutation(ctx, id, *callee, args);
                    self.check_no_alloc_call(id, *callee, span);
                    self.check_deterministic_call(id, *callee, span);
                    self.check_manual_drop(id, span);
                    return;
                }
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
                self.check_deterministic_call(id, *callee, span);
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
            // `base catch fallback` — the fallback inherits `tail`, because it really
            // is in the enclosing tail position when the error path is taken, so a
            // borrow escaping through it must still be caught.
            ExprKind::Catch { base, fallback, .. } => {
                self.walk_expr(ctx, *base, false);
                self.walk_expr(ctx, *fallback, tail);
            }
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
                if self.deterministic {
                    self.error(
                        span,
                        "raw `concurrent` is forbidden in a `@deterministic` function — its result \
                         can depend on the thread schedule. Use `par for … reduce(r)` (a checked \
                         deterministic reduction) for schedule-independent parallelism.",
                    );
                }
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
            ExprKind::Await(task) => {
                // `await` joins a task and yields its return value — an owned value the
                // task produced, never a borrow of the awaiter's frame. No escape; just
                // walk the handle operand.
                self.walk_expr(ctx, *task, false);
                return;
            }
            ExprKind::Region { body, .. } => {
                // A `region` block opens an arena (a heap allocation), so it is
                // forbidden in a `@no_alloc` function.
                self.allocates = true;
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
            ExprKind::WithAlive { genref, name, body, els } => {
                // The scrutinee is walked as an ordinary expression (a genref is
                // `Copy`, so no move concerns), and `name` binds as a BORROW for
                // the body's extent — which is the whole safety argument: the
                // ordinary frame rule refuses to let it return, be captured,
                // stored, or given away, so the checked-once-at-entry window is
                // exactly the block. Nothing new is proved here on purpose.
                self.walk_expr(ctx, *genref, false);
                ctx.push();
                ctx.bind(&name.name, true);
                self.check_block(ctx, body, false);
                ctx.pop();
                if let Some(e) = els {
                    self.check_block(ctx, e, false);
                }
                return;
            }
            ExprKind::ParFor { var, iter, reduction, body } => {
                // The loop variable is a *fresh* i64 element value (par_reduce copies
                // each element into a worker), not a borrow into the iterable — so it
                // can't escape. Walk the iterable, the per-element body, and the
                // reduction. No new data-race surface: the reduction's disjoint-region
                // writes live inside the tested `core.par_reduce` engine.
                self.walk_expr(ctx, *iter, false);
                self.walk_expr(ctx, *reduction, false);
                ctx.push();
                ctx.bind(&var.name, false);
                self.walk_expr(ctx, *body, false);
                ctx.pop();
                return;
            }
            ExprKind::Select(arms) => {
                // Each arm receives an owned `i64` (a fresh value moved out of the
                // channel), not a borrow — so the binding can't escape. Walk the
                // channel expression and the arm body.
                for arm in arms {
                    self.walk_expr(ctx, arm.chan, false);
                    ctx.push();
                    ctx.bind(&arm.bind.name, false);
                    self.check_block(ctx, &arm.body, false);
                    ctx.pop();
                }
                if self.deterministic {
                    self.error(
                        span,
                        "`select` waits on channels — its choice depends on the schedule; \
                         forbidden in a `@deterministic` function.",
                    );
                }
                return;
            }
            ExprKind::For { head, body, els, region, .. } => {
                // A region-scoped loop allocates a per-iteration scratch arena.
                if region.is_some() {
                    self.allocates = true;
                }
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
            // A returned *closure* and a returned *borrow* need different advice: the
            // borrow can often just be declared as one in the signature, while a
            // closure has to stop capturing by reference.
            if matches!(&self.ast.expr_at(id).kind, ExprKind::Closure { .. }) {
                self.error_help(
                    span,
                    format!("cannot return a closure capturing borrow `{name}`: the borrow would outlive its call"),
                    format!(
                        "capture `{name}` by value instead — pass it in as `take` so the closure owns it, \
                         or return a plain `fn` pointer that takes `{name}` as a parameter"
                    ),
                );
            } else {
                // This message already carries its own suggestion inline, and the P4
                // golden compares messages, so it stays exactly as it is.
                self.error(
                    span,
                    format!(
                        "cannot return borrow `{name}`: a second-class `read`/`mut`/`out` borrow may not outlive its call \
                         (pass it further down, or declare the return as `read`/`mut`/`out`)"
                    ),
                );
            }
        }
    }

    /// Recognize the payload-carrying error construction `err(Name(p))`
    /// (error-payloads E3), returning the inner callee and the payload expression.
    /// `None` when `err` is shadowed by a user fn or enum variant of that name
    /// (then it is an ordinary call/construction), or when the head is not a
    /// declared payload name.
    fn err_payload_form(&self, callee: ExprId, args: &[ExprId]) -> Option<(ExprId, ExprId)> {
        let ExprKind::Name(n) = &self.ast.expr_at(callee).kind else { return None };
        if n.name != "err" || args.len() != 1 || self.info.err_payloads.is_empty() {
            return None;
        }
        let shadowed = self.info.table.fns.contains_key("err")
            || self
                .info
                .table
                .variants
                .keys()
                .any(|k| k == "err" || k.starts_with("err__m"));
        if shadowed {
            return None;
        }
        let ExprKind::Call { callee: ic, args: ia } = &self.ast.expr_at(args[0]).kind else {
            return None;
        };
        let ExprKind::Name(en) = &self.ast.expr_at(*ic).kind else { return None };
        if !self.info.err_payloads.contains_key(&en.name) {
            return None;
        }
        ia.first().map(|p| (*ic, *p))
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
    ///
    /// This is one of exactly **two** places in the checker where copy-ness decides
    /// anything (`captured_borrow_name` is the other), which is why the `Unknown`
    /// finalization is recorded here rather than as a walk of its own — see
    /// [`Checker::note_unresolved`].
    fn escapes_as(&mut self, ctx: &FnCtx, id: ExprId) -> bool {
        // A closure that captures a borrow is non-`Copy` by nature (it holds a
        // reference), so the Copy refinement doesn't apply to it.
        if matches!(&self.ast.expr_at(id).kind, ExprKind::Closure { .. }) {
            // The capture path's own leniency: a captured borrow whose type is
            // `Unknown` is filtered out by `captured_borrow_name`'s Copy test, so
            // the closure never looks like it captures a borrow at all.
            if let Some(n) = self.captured_unresolved_name(ctx, id) {
                self.note_unresolved(id, n);
            }
            return self.is_borrow_place(ctx, id);
        }
        let place = self.is_borrow_place(ctx, id);
        if place && matches!(self.info.type_of(id), Ty::Unknown) {
            let n = self.root_name(ctx, id);
            self.note_unresolved(id, n);
        }
        place && self.info.is_non_copy(id)
    }

    /// Record that `id` is borrowed storage whose type inference never resolved —
    /// the `Unknown` finalization (safety mosaic, item 1).
    ///
    /// `Unknown` is `Copy` (`types.rs`), deliberately, so that expressions the
    /// checker could not type do not manufacture escape errors. That leniency is
    /// load-bearing for diagnostics and must stay. But at the two sites where
    /// copy-ness *decides* something, "we could not type it" is silently read as
    /// "it is a copy, let it escape" — which is not leniency, it is an unsound
    /// answer. This turns those into a refusal instead.
    ///
    /// Deduped by expression and emitted sorted in [`check`], not pushed inline:
    /// one expression is visited by several rules, and the port collects the same
    /// set by its own traversal. Sorting is what lets two collection strategies
    /// agree — and the key is `(span start, ExprId)`, a *total* order, rather than
    /// span alone with insertion order breaking ties. Expression ids correspond
    /// exactly across the two toolchains (that is what the P2/P3 goldens compare),
    /// so this orders identically without either side matching the other's walk.
    fn note_unresolved(&mut self, id: ExprId, name: String) {
        if !self.unresolved.iter().any(|(i, _)| *i == id) {
            self.unresolved.push((id, name));
        }
    }

    /// The read-only twin of [`Checker::captured_borrow_name`]: the name of a
    /// borrow this closure captures whose **type never resolved**.
    ///
    /// Kept separate rather than folded into `captured_borrow_name` because that
    /// one is reached through `&self` chains (`is_borrow_place`, `root_name`) used
    /// all over the checker; making it `&mut` to record a diagnostic cascades
    /// across the file for no gain.
    fn captured_unresolved_name(&self, ctx: &FnCtx, closure_id: ExprId) -> Option<String> {
        let ExprKind::Closure { params, body } = &self.ast.expr_at(closure_id).kind else {
            return None;
        };
        let params: Vec<&str> = params.iter().map(|p| p.name.name.as_str()).collect();
        let mut refs = Vec::new();
        self.collect_names(*body, &mut refs);
        refs.into_iter().find_map(|(n, id)| {
            let captures_borrow = !params.contains(&n.as_str()) && ctx.lookup(&n) == Some(true);
            (captures_borrow && matches!(self.info.type_of(id), Ty::Unknown)).then_some(n)
        })
    }

    /// Reject a `spawn` whose target takes a `mut`/`out` **slice** parameter — a
    /// shared mutable slice across parallel tasks can race (its `ptr` aliases). The
    /// safe way to share mutable state across tasks is a raw `*mut T` in `unsafe`.
    fn check_spawn_no_shared_mut_slice(&mut self, call: ExprId) {
        let ExprKind::Call { callee, .. } = &self.ast.expr_at(call).kind else { return };
        // The recorded resolution, not a bare-`Name` match: a QUALIFIED spawn
        // target (`spawn m.fill(s)`) has a `Field` callee and skipped this check
        // entirely — two tasks could share a `mut` slice through exactly the
        // spelling the module system encourages. Same consolidation as the four
        // call checks above (Stage 3), same class of hole.
        let Some(name) = self.resolved_callee_name(call, *callee) else { return };
        let Some(sig) = self.info.table.fns.get(&name) else { return };
        let mut hit: Option<String> = None;
        for p in &sig.params {
            if matches!(p.conv, Conv::Mut | Conv::Out) && matches!(p.ty, Ty::Slice(_)) {
                hit = Some(p.name.clone());
                break;
            }
        }
        if let Some(pname) = hit {
            let span = self.ast.expr_at(call).span;
            let fname = name.clone();
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
        if let Some(mr) = self.info.method_call(call_id).cloned() {
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

        // The canonical target, however the call was written — a qualified call
        // records it in `qualified` (without which `sync.channel_send(T, ch,
        // take v)` would silently let a borrow be sent), a bare call to a
        // colliding name in `call_sym` (without which the same give-away slipped
        // through a within-module call — `table.fns` is canon-keyed, so the bare
        // spelling missed it).
        let Some(name) = self.resolved_callee_name(call_id, callee) else { return };
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
        if let Some(ic) = self.info.impl_call(call_id) {
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
    /// The canonical function name a call targets, however it was written:
    /// the recorded resolution (`qualified` for `m.f(…)`, `call_sym` for a bare
    /// call to a colliding name) or the bare `Name` spelling when nothing was
    /// recorded — which is exactly when the spelling IS canonical. `None` for a
    /// callee that is not a plain function reference (an indirect call).
    ///
    /// This is the Stage-3 move: consume typeck's recorded decision instead of
    /// re-deriving it. The four checks below each hand-rolled this chain
    /// WITHOUT the `call_sym` half, so a within-module bare call to a colliding
    /// name looked up `table.fns` under its bare spelling, missed the canonical
    /// key, and silently skipped the check — a borrow could be `take`n through
    /// exactly that shape.
    fn resolved_callee_name(&self, call_id: ExprId, callee: ExprId) -> Option<String> {
        if let Some(sym) = self.info.resolved_call_target(call_id) {
            return Some(sym.to_string());
        }
        match &self.ast.expr_at(callee).kind {
            ExprKind::Name(n) => Some(n.name.clone()),
            _ => None,
        }
    }

    fn check_no_alloc_call(&mut self, call_id: ExprId, callee: ExprId, span: Span) {
        let Some(name) = self.resolved_callee_name(call_id, callee) else { return };
        // Recorded on EVERY call, `@no_alloc` or not, because the transitive pass
        // needs this function's callees before it knows whether anyone cares.
        if is_alloc_intrinsic(&name) {
            self.allocates = true;
        }
        self.calls.push((name.clone(), span));
        if !self.no_alloc {
            return;
        }
        if is_alloc_intrinsic(&name) {
            self.error(
                span,
                format!(
                    "`{name}` allocates — forbidden in a `@no_alloc` function (the proven-allocation-free contract)"
                ),
            );
            return;
        }
        // …and the TRANSITIVE rule: calling something that allocates, however
        // indirectly, breaks the same contract. `alloc_via` carries the shortest
        // chain, so the diagnostic can name the function that actually allocates
        // rather than only the one that was called.
        if let Some(chain) = self.alloc_via.get(&name) {
            let culprit = chain.last().cloned().unwrap_or_else(|| name.clone());
            // Name the whole path only when there is one worth naming — for a direct
            // callee the chain is a single hop and "via `f`; `f` allocates" is noise.
            let detail = if chain.len() > 1 {
                format!("via `{}`; `{culprit}` allocates directly", chain.join("` → `"))
            } else {
                format!("`{culprit}` allocates directly")
            };
            self.error(
                span,
                format!(
                    "`{name}` allocates — forbidden in a `@no_alloc` function ({detail})"
                ),
            );
        }
    }

    /// In a `@deterministic` function, reject a call to an `atomic_*` op — a
    /// sequentially-consistent atomic can *observe* the interleaving (e.g.
    /// `atomic_load` after concurrent `atomic_add`s), so a result built on it can
    /// depend on the schedule. The direct, per-op enforcement (the `@no_alloc`
    /// analog); the *transitive* "calls a function that uses atomics" closure (so a
    /// `Mutex`/`Channel` op is caught) is future work, as for `@no_alloc`.
    fn check_deterministic_call(&mut self, call_id: ExprId, callee: ExprId, span: Span) {
        if !self.deterministic {
            return;
        }
        let Some(name) = self.resolved_callee_name(call_id, callee) else { return };
        if matches!(name.as_str(), "atomic_store" | "atomic_load" | "atomic_add" | "atomic_sub" | "atomic_xchg") {
            self.error(
                span,
                format!(
                    "`{name}` is an atomic op whose result can depend on the thread schedule — \
                     forbidden in a `@deterministic` function. Use `par for … reduce(r)` for \
                     schedule-independent parallelism."
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
        if let Some(mr) = self.info.method_call(call_id).cloned() {
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
        // resolves to a canonical function name either way.
        let Some(name) = self.resolved_callee_name(call_id, callee) else { return };
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

    /// Find a top-level function declaration by its **canonical** name —
    /// mirroring typeck's `find_fn_decl`: each item's bare name is canonicalized
    /// under its owning module before comparing, so a non-colliding name matches
    /// its bare spelling exactly as before, and a colliding one matches only the
    /// disambiguated `name__m<id>` — never the wrong module's definition.
    fn find_fn(&self, name: &str) -> Option<&'a FnDecl> {
        self.ast.items.iter().enumerate().find_map(|(i, it)| match it {
            Item::Fn(f)
                if crate::types::canon(
                    *self.info.item_mod.get(i).unwrap_or(&0),
                    &f.name.name,
                    &self.info.dup_fns,
                ) == name =>
            {
                Some(f)
            }
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
            ExprKind::Catch { base, fallback, .. } => {
                self.collect_names(*base, out);
                self.collect_names(*fallback, out);
            }
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

    // --- struct-variant patterns bind real types (a closed soundness hole) ---

    const WRAP: &str = "struct Node { value: i32 } enum Wrap { one(n: Node, k: i32), dot } ";

    /// A borrowed field bound by a **named** variant pattern must not escape.
    ///
    /// This pins a soundness fix, not a preference. Struct-variant patterns used
    /// to bind `Ty::Unknown` — the global table stores a variant's field types
    /// positionally and dropped the names, so there was nothing to match `n`
    /// against. `Unknown` is `Copy`, so `escapes_as` classified a *borrowed* field
    /// as a copy and let it leave the frame, while the positional spelling of the
    /// identical program was rejected.
    ///
    /// The assertion is deliberately an *equivalence*: two spellings of one
    /// program must reach the same verdict. A test that only checked "named form
    /// errors" would still pass if both forms silently regressed to accepting.
    #[test]
    fn a_named_variant_binding_cannot_escape_its_borrow() {
        let positional = WRAP.to_string()
            + "fn f(read w: Wrap) -> Node { match w { one(n, k) => n, _ => Node{ value: 0 } } }";
        let named = WRAP.to_string()
            + "fn f(read w: Wrap) -> Node { match w { one { n, k } => n, _ => Node{ value: 0 } } }";
        let p = escapes(&positional);
        let n = escapes(&named);
        assert_eq!(p.len(), 1, "the positional form is the baseline: {p:?}");
        assert!(p[0].message.contains("cannot return borrow"), "{p:?}");
        assert_eq!(
            n.len(),
            p.len(),
            "the named form must agree with the positional one, not silently pass: {n:?}"
        );
        assert!(n[0].message.contains("cannot return borrow"), "{n:?}");
    }

    /// The same hole reached through a `..` rest pattern — the spelling the corpus
    /// actually uses (`examples/struct_variant.jtr`), and the one that surfaced it.
    #[test]
    fn a_rest_pattern_binding_cannot_escape_its_borrow() {
        let src = WRAP.to_string()
            + "fn f(read w: Wrap) -> Node { match w { one { n, .. } => n, _ => Node{ value: 0 } } }";
        let d = escapes(&src);
        assert_eq!(d.len(), 1, "a borrow bound through `..` still escapes: {d:?}");
        assert!(d[0].message.contains("cannot return borrow"), "{d:?}");
    }

    /// The other half of the fix: binding a `Copy` field by name must stay clean.
    /// Typing these bindings for real could have turned leniency into false
    /// positives, so pin that it did not.
    #[test]
    fn a_named_binding_of_a_copy_field_is_not_an_escape() {
        let src = WRAP.to_string()
            + "fn f(read w: Wrap) -> i32 { match w { one { k, .. } => k, _ => 0 } }";
        assert!(escapes(&src).is_empty(), "an `i32` field is copied, not escaped");
    }

    // --- the `Unknown` finalization (safety mosaic, item 1) ---

    /// The gate's two original catches are now typeck's: a field on a bracket
    /// type parameter and a field on a primitive are rejected AT THE ACCESS with
    /// a field-shaped message (the follow-up the gate's comment promised). They
    /// type `Error`, not `Unknown`, so the finalization gate stays silent — one
    /// diagnostic per program, at the right pass, naming the right thing.
    #[test]
    fn the_original_gate_catches_are_now_field_shaped_typeck_errors() {
        for (src, needle) in [
            (
                "struct N { v: i32 } fn f[T](read x: T) -> i32 { return x.v }",
                "no field `v` on type parameter `T`",
            ),
            (
                "struct N { v: i32 } fn h(read p: N) -> i32 { return p.v.w }",
                "no field `w` on `i32`",
            ),
        ] {
            let (tokens, _) = Lexer::new(src).tokenize();
            let (ast, _) = Parser::new(src, tokens).parse();
            let (info, type_diags) = crate::typeck::check(&ast);
            assert!(
                type_diags.iter().any(|d| d.message.contains(needle)),
                "typeck rejects `{src}` at the field access: {type_diags:?}"
            );
            let esc = check(&ast, &info);
            assert!(
                !esc.iter().any(|d| d.message.contains("was never resolved")),
                "the finalization gate is silent once typeck has diagnosed: {esc:?}"
            );
        }
    }

    /// The gate itself is not vacuous: a borrow whose type never resolved through
    /// a shape typeck cannot yet name — here an *indexed* type parameter — is
    /// still refused rather than assumed copyable. If a later typeck refinement
    /// catches this shape too, this probe must move to yet another unresolved
    /// shape, not be deleted: the gate is the backstop for whatever remains.
    #[test]
    fn an_unresolved_borrow_is_refused_rather_than_assumed_copyable() {
        let d = escapes("fn f[T](read x: T) -> i32 { return x[0] }");
        assert_eq!(d.len(), 1, "exactly one refusal: {d:?}");
        assert!(d[0].message.contains("was never resolved"), "{d:?}");
        assert!(d[0].help.is_some(), "the refusal must say what to do about it: {d:?}");
    }

    /// The `split_mut` safety contract (safety mosaic, item 4): the two `mut`
    /// slice views a callback receives are SECOND-CLASS — writable inside the
    /// frame, but a callback that tries to hand one back out is rejected by the
    /// ordinary escape rules. This is the "library-first" claim in one test: the
    /// library provides disjointness (by construction, behind its one `unsafe`),
    /// and the checker provides containment, with no new mechanism.
    #[test]
    fn a_split_mut_callback_cannot_leak_its_half()  {
        // Returning the borrowed half by value is a move of a borrow place.
        let d = escapes(
            "fn bad(mut lo: []i64, mut hi: []i64) -> []i64 { return lo }",
        );
        assert!(
            d.iter().any(|x| x.message.contains("escape") || x.message.contains("borrow")),
            "returning a mut slice param must be rejected: {d:?}"
        );
        // Storing it into a struct value that outlives the frame is a capture.
        let d2 = escapes(
            "struct Stash { s: []i64 } \
             fn bad2(mut lo: []i64, mut hi: []i64) -> Stash { return Stash{ s: lo } }",
        );
        assert!(
            d2.iter().any(|x| x.message.contains("escape") || x.message.contains("borrow")),
            "stashing a mut slice param must be rejected: {d2:?}"
        );
        // The intended use — write through both, leak neither — is clean.
        let ok = escapes(
            "fn good(mut lo: []i64, mut hi: []i64) { lo[0] = 1  hi[0] = 2 }",
        );
        assert!(ok.is_empty(), "writing through both halves is fine: {ok:?}");
    }

    /// The gate must stay *silent* on code whose types resolve — it is a
    /// finalization check, not a second escape rule. `ok_borrow_out` hands a borrow
    /// back through a `read` return, which is legal and fully typed.
    #[test]
    fn the_finalization_gate_is_silent_on_well_typed_borrows() {
        let src = "struct N { v: i32 } \
                   fn ok(read p: N) -> read N { p } \
                   fn also_ok(read p: N) -> i32 { p.v }";
        let d = escapes(src);
        assert!(
            !d.iter().any(|x| x.message.contains("was never resolved")),
            "no finalization refusal on typed code: {d:?}"
        );
    }

    /// One expression is visited by several rules, so the refusal is deduped by
    /// span — a repeated shape must not multiply into a cascade. (The probe is an
    /// indexed `T` — the field-on-`T` shape this test used to use is typeck's now.)
    #[test]
    fn the_finalization_refusal_is_reported_once_per_expression() {
        let d = escapes("fn f[T](read x: T) -> i32 { return x[0] }");
        assert_eq!(d.len(), 1, "one expression, one diagnostic: {d:?}");
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

    // --- @deterministic: the schedule-independence contract (workstream N) ---

    #[test]
    fn deterministic_accepts_a_par_for() {
        // The only parallelism inside a `@deterministic` function may be the checked
        // deterministic `par for` — that is accepted with no diagnostic.
        let d = escapes(
            "fn sum_reduction() -> i64 { return 0 } \
             @deterministic fn f(read s: []i64) -> i64 { return par for x in s reduce(sum_reduction()) { x } }",
        );
        assert!(d.is_empty(), "a par for must be allowed in @deterministic: {d:?}");
    }

    #[test]
    fn deterministic_rejects_raw_concurrent() {
        let d = escapes(
            "fn w(c: *mut i64) {} \
             @deterministic fn f(c: *mut i64) { concurrent { spawn w(c) } }",
        );
        assert!(
            d.iter().any(|m| m.message.contains("@deterministic") && m.message.contains("concurrent")),
            "raw concurrent must be rejected in @deterministic: {d:?}"
        );
    }

    #[test]
    fn deterministic_rejects_atomics() {
        let d = escapes("@deterministic fn f(c: *mut i64) -> i64 { return atomic_load(c) }");
        assert!(
            d.iter().any(|m| m.message.contains("@deterministic") && m.message.contains("atomic_load")),
            "an atomic op must be rejected in @deterministic: {d:?}"
        );
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

    // --- suggested rewrites (Diagnostics tier 3) ---

    /// A store escape is the most common way to hit the ownership rule, and "you may
    /// not do that" is the least actionable thing to say about it. The suggestion names
    /// **all three** Jestyr answers, because which one is right depends on whether the
    /// value is moved, long-lived, or shared — and the compiler cannot know that.
    #[test]
    fn a_store_escape_suggests_the_three_real_remedies() {
        let d = escapes("struct N { v: i32 } struct H { i: i32 } fn f(read p: N) -> H { H{ i: p } }");
        assert_eq!(d.len(), 1, "{d:?}");
        let h = d[0].help.as_deref().expect("a store escape must suggest a rewrite");
        for remedy in ["take", "region", "genref"] {
            assert!(h.contains(remedy), "the `{remedy}` remedy must be named: {h}");
        }
    }

    /// A returned closure gets **different** advice from a returned borrow: a borrow can
    /// often just be declared as one in the signature, but a closure has to stop
    /// capturing by reference, and telling it to "declare the return as `read`" would be
    /// useless.
    #[test]
    fn a_returned_closure_gets_its_own_suggestion() {
        let d = escapes("struct N { v: i32 } fn f(read p: N) -> fn() -> i32 { return || p.v }");
        assert_eq!(d.len(), 1, "{d:?}");
        let h = d[0].help.as_deref().expect("a captured borrow must suggest a rewrite");
        assert!(h.contains("by value"), "{h}");
        assert!(!h.contains("genref"), "the store-escape advice does not apply here: {h}");
    }

    /// **The invariant that keeps suggestions free.** The P4 escape golden compares the
    /// port's diagnostics against these by *span + message*; `help` is a separate field
    /// the port has no counterpart for. So suggestions may be added and improved on the
    /// reference side without owing a port mirror — provided no message changes, which
    /// is what this pins.
    #[test]
    fn adding_a_suggestion_does_not_change_any_message() {
        let d = escapes(
            "struct N { v: i32 } struct H { i: i32 } \
             fn a(read p: N) -> N { p } \
             fn b(read p: N) -> H { H{ i: p } } \
             fn c(read p: N, mut h: H) { h.i = p }",
        );
        assert_eq!(d.len(), 3, "{d:?}");
        assert!(d[0].message.starts_with("cannot return borrow `p`"), "{:?}", d[0].message);
        assert!(d[1].message.starts_with("cannot store borrow `p` in struct `H`"), "{:?}", d[1].message);
        assert!(
            d[2].message.starts_with("cannot store borrow `p` into borrowed storage"),
            "{:?}",
            d[2].message
        );
        // The return-escape message already carries its suggestion inline, so it takes
        // no `help` — duplicating it would print the same advice twice.
        assert!(d[0].help.is_none(), "no duplicated advice: {:?}", d[0].help);
        assert!(d[1].help.is_some() && d[2].help.is_some());
    }

    // --- transitive `@no_alloc` ---

    /// **The contract says *proven* allocation-free, so hiding the allocation one call
    /// away must not launder it.** Before this, only a direct allocation *intrinsic*
    /// was caught, so `@no_alloc fn f() { g() }` passed however much `g` allocated.
    #[test]
    fn no_alloc_rejects_an_allocation_one_call_away() {
        let d = escapes(
            "fn g(n: i32) -> *mut i32 { return alloc(i32, n) } \
             @no_alloc fn f(n: i32) -> i32 { let p = g(n) free_ptr(p) return 0 }",
        );
        assert_eq!(d.len(), 1, "{d:?}");
        assert!(d[0].message.contains("`g` allocates"), "{:?}", d[0].message);
        // One hop: naming a path would be noise, so it just names the culprit.
        assert!(!d[0].message.contains("via"), "{:?}", d[0].message);
    }

    /// A longer chain names the **path** and the function that actually allocates —
    /// which is the one the user has to go change.
    #[test]
    fn no_alloc_names_the_chain_to_the_real_culprit() {
        let d = escapes(
            "fn deep(n: i32) -> *mut i32 { return alloc(i32, n) } \
             fn middle(n: i32) -> *mut i32 { return deep(n) } \
             fn outer(n: i32) -> *mut i32 { return middle(n) } \
             @no_alloc fn f(n: i32) -> i32 { let p = outer(n) free_ptr(p) return 0 }",
        );
        assert_eq!(d.len(), 1, "{d:?}");
        let m = &d[0].message;
        assert!(m.contains("`outer` allocates"), "{m}");
        assert!(m.contains("via `middle` → `deep`"), "the chain must be named: {m}");
        assert!(m.contains("`deep` allocates directly"), "the culprit must be named: {m}");
        // Each name appears once — a self-chain on the directly-allocating function
        // used to duplicate the tail (`deep` → `deep`).
        assert_eq!(m.matches("deep").count(), 2, "one path mention + one culprit: {m}");
    }

    /// A `region` block and a region-scoped loop are allocations too, so a function
    /// containing either poisons its callers just as an intrinsic does.
    #[test]
    fn no_alloc_transits_regions_as_well_as_intrinsics() {
        let d = escapes(
            "fn arena() -> i32 { region r { let x = 1 } return 0 } \
             @no_alloc fn f() -> i32 { return arena() }",
        );
        assert_eq!(d.len(), 1, "a `region` must propagate: {d:?}");
        assert!(d[0].message.contains("`arena` allocates"), "{:?}", d[0].message);
    }

    /// **Totality.** The closure is a least fixpoint, so recursion — direct or
    /// mutual — settles instead of looping. Without that this test hangs the suite.
    #[test]
    fn no_alloc_terminates_on_recursive_call_graphs() {
        let d = escapes(
            "fn a(n: i32) -> i32 { if n > 0 { return b(n - 1) } return 0 } \
             fn b(n: i32) -> i32 { return a(n) } \
             @no_alloc fn f(n: i32) -> i32 { return a(n) }",
        );
        assert!(d.is_empty(), "an allocation-free cycle must pass: {d:?}");
        // …and a cycle that *does* allocate is still caught.
        let d = escapes(
            "fn a(n: i32) -> i32 { if n > 0 { return b(n - 1) } let p = alloc(i32, 1) free_ptr(p) return 0 } \
             fn b(n: i32) -> i32 { return a(n) } \
             @no_alloc fn f(n: i32) -> i32 { return b(n) }",
        );
        assert_eq!(d.len(), 1, "{d:?}");
        assert!(d[0].message.contains("`b` allocates"), "{:?}", d[0].message);
    }

    /// The check must not become a blanket "any call is suspect": a chain of purely
    /// computational functions is exactly what `@no_alloc` code is built from, and
    /// rejecting it would make the attribute unusable.
    #[test]
    fn no_alloc_accepts_a_chain_of_allocation_free_calls() {
        let d = escapes(
            "fn add(a: i32, b: i32) -> i32 { return a + b } \
             fn twice(a: i32) -> i32 { return add(a, a) } \
             fn quad(a: i32) -> i32 { return twice(twice(a)) } \
             @no_alloc fn f(a: i32) -> i32 { return quad(a) }",
        );
        assert!(d.is_empty(), "allocation-free calls must pass: {d:?}");
    }

    /// The diagnostic text is deterministic: with two chains of equal length the
    /// winner must not depend on hash iteration order, or the message would vary
    /// between runs of the same compiler on the same input.
    #[test]
    fn no_alloc_chain_selection_is_deterministic() {
        let src = "fn x(n: i32) -> *mut i32 { return alloc(i32, n) } \
                   fn y(n: i32) -> *mut i32 { return alloc(i32, n) } \
                   fn p(n: i32) -> *mut i32 { return x(n) } \
                   fn q(n: i32) -> *mut i32 { return y(n) } \
                   fn both(n: i32) -> *mut i32 { if n > 0 { return p(n) } return q(n) } \
                   @no_alloc fn f(n: i32) -> i32 { let z = both(n) free_ptr(z) return 0 }";
        let first = escapes(src)[0].message.clone();
        for _ in 0..5 {
            assert_eq!(escapes(src)[0].message, first, "the chain choice must be stable");
        }
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

    /// Error-payloads E3: `err(Name(p))` returns its payload, so the existing
    /// return rules apply to `p` — a region-allocated `str` payload would dangle
    /// by the time the caller could read it, and is refused exactly as a plain
    /// `return` of it is.
    #[test]
    fn a_region_str_payload_is_refused_like_a_return() {
        let d = escapes(
            "fn f() -> i32 !{ Bad(str) } { \
               region r { let g: str = region_concat(r, \"a\", \"b\") return err(Bad(g)) } \
               return ok(1) }",
        );
        assert!(d.iter().any(|m| m.message.contains("region-allocated")), "{:?}", d);
    }

    /// The payloads that carry no borrow — a literal `str` and an owned scalar —
    /// pass, so the rule refuses dangling, not payloads.
    #[test]
    fn literal_and_scalar_payloads_pass_the_escape_check() {
        let d = escapes(
            "fn f(n: i64) -> i32 !{ Bad(str), TooBig(i64) } { \
               if n > 9 { return err(TooBig(n)) } \
               if n < 0 - 9 { return err(Bad(\"oops\")) } \
               return ok(1) }",
        );
        assert!(d.is_empty(), "{d:?}");
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
