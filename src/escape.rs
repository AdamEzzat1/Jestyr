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
use crate::types::{Ty, TypeInfo, TypeKindG};

pub fn check(ast: &Ast, info: &TypeInfo) -> Vec<Diagnostic> {
    let effects = effect_closures(ast, info);
    let mut ck = Checker {
        ast,
        info,
        diags: Vec::new(),
        frozen: Vec::new(),
        region_depths: Vec::new(),
        no_alloc: false,
        no_os: false,
        deterministic: false,
        allocates: false,
        uses_os: false,
        calls: Vec::new(),
        alloc_via: effects.alloc_via,
        os_via: effects.os_via,
        extern_fns: extern_fn_names(ast),
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

/// The transitive effect closures the two proven-absence contracts read.
///
/// One struct rather than two return values because both are computed from the **same**
/// probe pass and the same call graph; splitting them into two entry points would run
/// the whole checker over every function twice for no gain.
struct Effects {
    /// Functions that allocate transitively → the shortest chain to one that allocates
    /// *directly* (`@no_alloc`).
    alloc_via: HashMap<String, Vec<String>>,
    /// Functions that reach the OS transitively → the shortest chain to one that
    /// reaches it *directly* (`@no_os`).
    os_via: HashMap<String, Vec<String>>,
}

/// For every top-level function that allocates — or reaches the OS — **transitively**,
/// the shortest call chain from it to a function that does so *directly*.
///
/// ## Why this reuses the checker instead of restating "allocates" / "reaches the OS"
/// The direct rules already exist: allocation has three (an allocation intrinsic, a
/// `region` block, a region-scoped loop) and OS access has one ([`is_os_intrinsic`]).
/// Writing a second walker that looked for those would be two definitions of each effect
/// that could drift, and the one that drifted would make its attribute claim a proof it
/// does not have. So this runs the **real checker** over each function with the per-op
/// rules recording into `allocates`/`uses_os`/`calls`, and reads those out. One decision
/// point, several consumers — the rule this codebase applies to `at_ty`,
/// `simd::classify` and `layout::field_order`.
///
/// The diagnostics from those probe runs are discarded: they belong to the main pass,
/// which reports each function once with the right contract flags.
///
/// ## Why one pass for both effects
/// The probe is the expensive part — it walks every function's whole body — and the call
/// graph it yields is shared. So the walk happens once, each effect gets its own `direct`
/// seed set, and [`shortest_chains`] closes over the one graph twice. Adding a third
/// proven-absence contract costs a seed set and a `shortest_chains` call, not another
/// traversal of the program.
///
/// ## What it deliberately does not resolve
/// Only **free functions**, resolved by name. A method, a closure, or a call through a
/// `fn(…)` pointer is not in the graph, so a `@no_alloc`/`@no_os` function that allocates
/// or calls the OS through one is not caught. That is a real limit, not an oversight —
/// closing it needs call-graph resolution the escape checker does not have today — and it
/// is recorded in `docs/attributes.md` rather than left for a user to discover.
fn effect_closures(ast: &Ast, info: &TypeInfo) -> Effects {
    // Per function: which effects does it have directly, and whom does it call?
    let mut alloc_direct: HashSet<String> = HashSet::new();
    let mut os_direct: HashSet<String> = HashSet::new();
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
            no_os: false,
            deterministic: false,
            allocates: false,
            uses_os: false,
            calls: Vec::new(),
            alloc_via: HashMap::new(),
            os_via: HashMap::new(),
            extern_fns: extern_fn_names(ast),
            unresolved: Vec::new(),
        };
        probe.check_item(item);
        if probe.allocates {
            alloc_direct.insert(f.name.name.clone());
        }
        if probe.uses_os {
            os_direct.insert(f.name.name.clone());
        }
        let mut cs: Vec<String> = probe.calls.into_iter().map(|(n, _)| n).collect();
        cs.sort();
        cs.dedup();
        calls.insert(f.name.name.clone(), cs);
    }
    Effects {
        alloc_via: shortest_chains(&alloc_direct, &calls),
        os_via: shortest_chains(&os_direct, &calls),
    }
}

/// Close `direct` over the call graph `calls`, mapping each function that reaches the
/// effect to the **shortest** chain of callees that gets there.
fn shortest_chains(
    direct: &HashSet<String>,
    calls: &HashMap<String, Vec<String>>,
) -> HashMap<String, Vec<String>> {
    // Least fixpoint: a function has the effect if it calls one that does. Iterated to
    // saturation rather than recursed, so a cycle (mutual or self recursion) settles
    // instead of looping — the same totality instinct the comptime interpreter applies.
    // A function with the effect DIRECTLY is reached via an EMPTY chain — it is itself
    // the culprit. Seeding it with its own name instead would duplicate that name in
    // every chain that passes through it.
    let mut via: HashMap<String, Vec<String>> = HashMap::new();
    for d in direct {
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
    // Functions with the effect directly STAY in the map, with their empty chains. The
    // direct rule in `check_effect_call` only recognizes *intrinsics*, so dropping them
    // here would let a one-hop call to a user function that has the effect
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
    /// Is the function currently being checked `@no_os`? If so, a call reaching any
    /// OS-facing intrinsic ([`is_os_intrinsic`]) is a compile error — the enforced
    /// freestanding contract, which turns the `core` tier's "links on a bare-metal
    /// target" from a header comment into a checked property. Independent of
    /// `no_alloc`: a `@no_os` function may allocate (`std/sha256` does).
    /// Saved/restored around nested method bodies.
    no_os: bool,
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
    /// Set when the function being walked calls an OS-facing intrinsic **directly**.
    /// Recorded regardless of `no_os`, for the same reason `allocates` is.
    uses_os: bool,
    /// Every resolved callee name seen while walking, with its span — the call-graph
    /// edges [`effect_closures`] closes over.
    calls: Vec<(String, Span)>,
    /// Functions that allocate **transitively**, each mapped to the shortest chain
    /// reaching a directly-allocating one. Empty during the measuring pass.
    alloc_via: HashMap<String, Vec<String>>,
    /// Functions that reach the OS **transitively**, each mapped to the shortest chain
    /// reaching one that calls an OS intrinsic. Empty during the measuring pass.
    os_via: HashMap<String, Vec<String>>,
    /// Every `extern "c"` function the program declares.
    ///
    /// Calling one leaves Jestyr for the platform's C library, so it is an OS effect
    /// by definition — and it is the one that is *not* on the closed intrinsic list.
    /// `docs/stdlib-roadmap.md` predicted this gap ("when `extern \"c\"` lands, the
    /// intrinsic set stops being the whole platform and `@no_os` needs an `extern`
    /// rule"); `extern "c"` turned out to already work, so the rule was already owed.
    extern_fns: HashSet<String>,
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
/// Why a binding stopped owning its value. Both are moves; they read differently to a
/// person, and a diagnostic that said "given to a `take` parameter" about a plain `let`
/// would send the reader looking for a call that is not there.
#[derive(Clone, Copy, PartialEq, Eq)]
enum MoveCause {
    /// Given to a `take` parameter: the callee owns and drops it.
    TakeArg,
    /// Rebound to a new name: the new binding owns and drops it.
    Rebind,
}

impl MoveCause {
    /// The reason, phrased so it is true for BOTH kinds of resource.
    ///
    /// These used to say "…and will drop it", which was accurate while the rules were
    /// gated on `droppable_ty` alone. A `@move` type need not have a `Drop` — the seven
    /// `sys`-tier handles deliberately do not, because their close is fallible and the
    /// caller is meant to see the verdict — so "the new name owns it" is the part that is
    /// always true and the destructor is the part that is not.
    fn phrase(self) -> &'static str {
        match self {
            MoveCause::TakeArg => "it was given to a `take` parameter: ownership moved at the call",
            MoveCause::Rebind => "it was moved to another binding: the new name owns it now",
        }
    }
}

/// Per-function analysis state: a stack of lexical scopes mapping each in-scope
/// binding to whether it denotes a borrow, plus this function's return mode.
struct FnCtx {
    scopes: Vec<HashMap<String, bool>>,
    /// Names bound to a **region-allocated** value (from `region_str`/`region_alloc`/
    /// `region_concat`). Such a value is owned by its arena and may not escape — the
    /// region-safety proof (design §4.4).
    region: Vec<HashSet<String>>,
    /// Item 5's residue (a), closed lexically: a binding whose initializer is a
    /// pure PLACE CHAIN inherits its root's effective depth (`var alias = h`
    /// inside a region still reaches `h`'s outer storage), transitively. Keyed
    /// per scope beside `scopes`; consulted only by the store-THROUGH-chain
    /// region rule — a bare-Name assign overwrites the alias itself, which is
    /// separate storage, so raw depths stay right for it.
    aliased: Vec<HashMap<String, usize>>,
    /// The consuming rule (ownership: use-after-move): names of DROPPABLE owned
    /// locals given to a `take` parameter, recorded at their binding's scope
    /// depth. The callee owns (and drops) the value now — cgen registers the
    /// `take` param in the callee's drop scope — so any later use of the binding
    /// reads a value whose destructor already ran. `if`/`match` walk branches
    /// with forked copies merged by UNION (both branches may consume; a use
    /// after either errors), like Rust's conservative analysis. A rebind
    /// (`let`) clears the entry — a fresh binding is a fresh value.
    /// The value is WHY it moved, so the diagnostic can name the event: a `take`
    /// argument and a rebinding are the same thing to the value and different things to
    /// the reader, and "given to a `take` parameter" is actively misleading on a `let`.
    consumed: Vec<HashMap<String, MoveCause>>,
    /// Scope depth at entry to each enclosing loop-like construct (`for` loops,
    /// `par for` bodies, closures — anything that may run again). Consuming a
    /// binding declared OUTSIDE the innermost such construct is refused at the
    /// consume site: the second run would consume a value already gone.
    loop_floor: Vec<usize>,
    ret_is_borrow: bool,
}

impl FnCtx {
    fn push(&mut self) {
        self.scopes.push(HashMap::new());
        self.region.push(HashSet::new());
        self.aliased.push(HashMap::new());
        self.consumed.push(HashMap::new());
    }
    fn pop(&mut self) {
        self.scopes.pop();
        self.region.pop();
        self.aliased.pop();
        self.consumed.pop();
    }
    fn bind(&mut self, name: &str, is_borrow: bool) {
        self.scopes.last_mut().unwrap().insert(name.to_string(), is_borrow);
        // A rebind is a fresh value unless the Let arm re-taints it right after.
        self.aliased.last_mut().unwrap().remove(name);
        self.consumed.last_mut().unwrap().remove(name);
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
    /// The depth `name`'s storage actually REACHES: an alias binding answers its
    /// initializer root's inherited depth; every other name answers its own
    /// scope index.
    fn effective_depth_of(&self, name: &str) -> Option<usize> {
        let i = self.scope_depth_of(name)?;
        Some(self.aliased[i].get(name).copied().unwrap_or(i))
    }
    /// Is `name`'s current binding marked consumed? (Innermost binding wins —
    /// a shadowing `let` cleared its own scope's entry at bind.)
    fn is_consumed(&self, name: &str) -> bool {
        self.consumed_cause(name).is_some()
    }
    /// Why `name` stopped owning its value, if it did — so the diagnostic can name the
    /// event rather than guessing at one.
    fn consumed_cause(&self, name: &str) -> Option<MoveCause> {
        let d = self.scope_depth_of(name)?;
        self.consumed[d].get(name).copied()
    }
    /// Record `name`'s binding as consumed, at its BINDING depth — the mark
    /// outlives inner blocks and dies with the binding.
    fn mark_consumed(&mut self, name: &str, why: MoveCause) {
        if let Some(d) = self.scope_depth_of(name) {
            self.consumed[d].insert(name.to_string(), why);
        }
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

/// Union-merge one branch's consumed sets into another, depth-aligned. Only
/// depths still present in `into` matter — a branch's own inner scopes are
/// already popped by the time it merges back.
fn merge_consumed(into: &mut [HashMap<String, MoveCause>], from: Vec<HashMap<String, MoveCause>>) {
    for (i, set) in from.into_iter().enumerate() {
        if let Some(t) = into.get_mut(i) {
            t.extend(set);
        }
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

    /// **"has already dropped it" was removed deliberately.**
    ///
    /// It was true while the rules were gated on `droppable_ty` alone. A `@move` type need
    /// not have a `Drop` — the `sys` tier's handles specifically do not, because their
    /// close is fallible and the caller is meant to see the verdict — so for the commonest
    /// new instance of this diagnostic (a second name for one OS handle) the old help text
    /// described a destructor that does not exist, and pointed the reader at the wrong
    /// question. What is always true is that the value has ONE owner and this name is no
    /// longer it.
    const CONSUMED_HELP: &'static str =
        "the value has one owner and this name is no longer it — a consumed binding cannot \
         be read, reused, or reinitialized; bind the callee's result if you need a value back, \
         or pass a borrow (`read`/`mut`) instead of giving ownership";

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
            // An `impl`'s method bodies. The comment here used to say they were
            // "escape-checked once their resolution lands (Stage B)"; Stage B is
            // `typeck::register_impls`, which reads signatures only, so in fact
            // NOTHING inside an impl body was ever escape-checked — including every
            // `impl Drop` in the corpus, which is where raw pointers get freed and
            // therefore the worst place in the language to have no checker.
            Item::Impl(im) => {
                for m in &im.methods {
                    self.check_fn(m);
                }
            }
            Item::Enum(_) | Item::Const(_) | Item::Distinct(_) | Item::Extern(_) | Item::Import(_) => {}
            // A trait's DEFAULT bodies keep the hole (as in `typeck::check_items`);
            // nothing in the corpus has one. A bare signature has nothing to check.
            Item::Trait(_) => {}
        }
    }

    fn check_fn(&mut self, f: &FnDecl) {
        let ret_is_borrow = matches!(f.ret_conv, Conv::Read | Conv::Mut | Conv::Out);
        let mut ctx = FnCtx {
            scopes: Vec::new(),
            region: Vec::new(),
            aliased: Vec::new(),
            consumed: Vec::new(),
            loop_floor: Vec::new(),
            ret_is_borrow,
        };
        ctx.push();
        for p in &f.params {
            // MVS (design §4.3): the default convention *is* `read` (an immutable
            // borrow). Only `take` transfers ownership — so every non-comptime,
            // non-`take` parameter is a borrow that may not escape its frame.
            let is_borrow = !p.comptime && p.conv != Conv::Take;
            let name = if p.is_self { "self" } else { p.name.name.as_str() };
            ctx.bind(name, is_borrow);
        }
        // The proven-absence contracts are per-function — save/restore so a nested
        // method body does not inherit (or clobber) the enclosing function's.
        let saved_no_alloc = self.no_alloc;
        self.no_alloc = f.has_attr("no_alloc");
        let saved_no_os = self.no_os;
        self.no_os = f.has_attr("no_os");
        let saved_det = self.deterministic;
        self.deterministic = f.has_attr("deterministic");
        // The body is in return position: its tail expression is the result.
        self.check_block(&mut ctx, &f.body, true);
        self.no_alloc = saved_no_alloc;
        self.no_os = saved_no_os;
        self.deterministic = saved_det;
    }

    fn check_block(&mut self, ctx: &mut FnCtx, block: &Block, tail: bool) {
        ctx.push();
        let n = block.stmts.len();
        for (i, stmt) in block.stmts.iter().enumerate() {
            let is_last = i + 1 == n;
            match stmt {
                Stmt::Let { name, init, .. } => {
                    let mut alias_depth = None;
                    let is_borrow = if let Some(e) = init {
                        self.walk_expr(ctx, *e, false);
                        // A binding initialized from a region-allocated value is
                        // itself region-tainted, so the taint flows through `let`s.
                        if self.is_region_value(ctx, *e) {
                            ctx.bind_region(&name.name);
                        }
                        // Item 5's residue (a): a pure place-chain initializer makes
                        // this binding an ALIAS of its root's storage — inherit the
                        // root's effective depth (transitive), computed BEFORE the
                        // bind so `var h = h` self-shadowing taints correctly. Only
                        // a strictly shallower reach is worth recording.
                        if self.place_key(*e).is_some() {
                            let root = self.root_name(ctx, *e);
                            alias_depth = ctx
                                .effective_depth_of(&root)
                                .filter(|&d| d + 1 < ctx.scopes.len());
                        }
                        self.is_borrow_place(ctx, *e)
                    } else {
                        false
                    };
                    // **A droppable moves on rebinding** (brief §2.1 — move-only
                    // resources). `var b: Writer = a` used to leave TWO names for one
                    // handle: both dropped it, and `std/file`'s header had to document
                    // that as a limitation the language could not express.
                    //
                    // The rule reuses `take`'s machinery exactly — the source is marked
                    // consumed, so the existing use-after-move diagnostic fires on the
                    // next mention. That is deliberate: one notion of "moved", one
                    // diagnostic, and a rebinding and a `take` argument are the same
                    // event from the value's point of view.
                    //
                    // Only a BARE NAME initializer moves. A field or index initializer
                    // (`let w = pair.writer`) is already refused by the `take` path's
                    // place-chain arm for the same reason, and a call initializer
                    // (`let w = file.create(..)`) produces a fresh value that nothing
                    // else owns.
                    //
                    // Scoped to droppables so nothing else changes: an `i64`, a `str`, a
                    // `@copy` handle and a plain struct with no `Drop` all still copy,
                    // which is what keeps this a rule about RESOURCES rather than a
                    // borrow checker.
                    if let Some(e) = init {
                        if let ExprKind::Name(n) = &self.ast.expr_at(*e).kind {
                            if ctx.lookup(&n.name) == Some(false)
                                && self.owns_resource(&self.info.type_of(*e).clone())
                            {
                                ctx.mark_consumed(&n.name, MoveCause::Rebind);
                            }
                        }
                    }
                    ctx.bind(&name.name, is_borrow);
                    if let Some(d) = alias_depth {
                        ctx.aliased.last_mut().unwrap().insert(name.name.clone(), d);
                    }
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
                // Branches fork the consumed set and merge by UNION: each branch
                // may consume the same binding (only one runs), but a use after
                // the `if` sees either branch's consumption. A lone `if` (no
                // `else`) keeps its consumption sticky — the branch may have run.
                if let Some(e) = els {
                    let saved = ctx.consumed.clone();
                    self.check_block(ctx, then, tail);
                    let from_then = std::mem::replace(&mut ctx.consumed, saved);
                    self.walk_expr(ctx, *e, tail);
                    merge_consumed(&mut ctx.consumed, from_then);
                } else {
                    self.check_block(ctx, then, tail);
                }
                return;
            }
            ExprKind::Match { scrut, arms } => {
                let scrut_borrow = self.is_borrow_place(ctx, *scrut);
                self.walk_expr(ctx, *scrut, false);
                // Arms fork like `if` branches: each starts from the pre-match
                // consumed set, and the sets merge by union afterwards.
                let saved = ctx.consumed.clone();
                let mut merged = saved.clone();
                for arm in arms {
                    ctx.consumed = saved.clone();
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
                    let from_arm = std::mem::take(&mut ctx.consumed);
                    merge_consumed(&mut merged, from_arm);
                }
                ctx.consumed = merged;
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
                            // root-outside shape). Residue (a) — a root ALIASED inside the
                            // region to outer storage (`var alias = h`, store through
                            // `alias`) — is closed by the lexical alias taint: the root
                            // answers its EFFECTIVE depth, inherited from its initializer's
                            // root at the `let`. The remaining honest limit is a store
                            // performed by a *callee* the checker doesn't look into —
                            // signatures, item 2 territory (`docs/safety-mosaic-next.md`).
                            ExprKind::Field { .. } | ExprKind::Index { .. } | ExprKind::Deref { .. } => {
                                let root = self.root_name(ctx, *target);
                                let raw = ctx.scope_depth_of(&root);
                                if let Some(d) = ctx.effective_depth_of(&root) {
                                    if d < region_depth {
                                        if raw.is_some_and(|r| r >= region_depth) {
                                            // Declared inside; REACHES outside — the alias.
                                            self.error(
                                                span,
                                                format!(
                                                    "cannot store region-allocated value through `{root}`: it aliases \
                                                     storage declared outside the `region` block, so this store outlives \
                                                     the arena (copy the value into an owned `String`, or allocate it in \
                                                     the outer region)"
                                                ),
                                            );
                                        } else {
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
                    self.check_slice_alias(id, *callee, args);
                    self.check_effect_call(id, *callee, span);
                    self.check_deterministic_call(id, *callee, span);
                    self.check_manual_drop(id, span);
                    return;
                }
                // Passing a borrow *down* is the allowed case — UNLESS the callee
                // is a known function whose matching parameter is `take` (owning):
                // you can't hand ownership of something you only borrowed.
                self.walk_expr(ctx, *callee, false);
                // The consuming rule marks each droppable `take` argument's binding
                // AS the arguments are walked, in evaluation order (receiver first) —
                // so `g(d, d)` with two `take` params reports the second `d` as a
                // use-after-consume, and the use half below sees every later read.
                let (recv_take, arg_takes) = self.take_shape(id, *callee, args.len());
                if recv_take {
                    if let ExprKind::Field { base, .. } = &self.ast.expr_at(*callee).kind {
                        self.consume_take_arg(ctx, *base);
                    }
                }
                for (i, a) in args.iter().enumerate() {
                    self.walk_expr(ctx, *a, false);
                    if arg_takes.get(i).copied().unwrap_or(false) {
                        self.consume_take_arg(ctx, *a);
                    }
                }
                self.check_give_away(ctx, id, *callee, args);
                self.check_loop_mutation(ctx, id, *callee, args);
                self.check_slice_alias(id, *callee, args);
                self.check_effect_call(id, *callee, span);
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
                // A closure body may run any number of times, so for the consuming
                // rule it is loop-like: a captured outer droppable must not be
                // given to `take` inside it.
                ctx.loop_floor.push(ctx.scopes.len());
                ctx.push();
                for p in params {
                    ctx.bind(&p.name.name, false);
                }
                self.walk_expr(ctx, *body, false);
                ctx.pop();
                ctx.loop_floor.pop();
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
                //
                // Starting a task needs a **thread runtime**, which a freestanding
                // target does not have — so `spawn` is an OS effect. Reported here and
                // at `par for` rather than at the enclosing `concurrent` block: a
                // `concurrent` scope with nothing spawned in it starts no thread, and a
                // rule on the block would double-report the common nested shape.
                self.uses_os = true;
                if self.no_os {
                    self.error(
                        span,
                        "`spawn` starts a thread — forbidden in a `@no_os` function \
                         (the proven-freestanding contract)",
                    );
                }
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
                //
                // Like `spawn`, a parallel loop needs a thread runtime — an OS effect.
                self.uses_os = true;
                if self.no_os {
                    self.error(
                        span,
                        "a `par for` loop starts threads — forbidden in a `@no_os` function \
                         (the proven-freestanding contract)",
                    );
                }
                self.walk_expr(ctx, *iter, false);
                self.walk_expr(ctx, *reduction, false);
                // The body runs once per element — a loop for the consuming rule.
                ctx.loop_floor.push(ctx.scopes.len());
                ctx.push();
                ctx.bind(&var.name, false);
                self.walk_expr(ctx, *body, false);
                ctx.pop();
                ctx.loop_floor.pop();
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
                // Consuming rule: a binding declared OUTSIDE the loop must not be
                // given to `take` inside it — the next iteration would consume a
                // value already gone. The floor is the depth before the loop's own
                // scopes open; popped before `els`, which runs exactly once.
                ctx.loop_floor.push(ctx.scopes.len());
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
                ctx.loop_floor.pop();
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

            // The consuming rule's use half: this binding was given to a `take`
            // parameter, so the callee owns it and its destructor has already
            // run by the next statement — reading, passing, or reassigning it
            // reads a dropped value. (Falls through: a tail Name still gets the
            // region-return check below.)
            ExprKind::Name(n) => {
                if let Some(why) = ctx.consumed_cause(&n.name) {
                    self.error_help(
                        span,
                        format!("cannot use `{}` after {}", n.name, why.phrase()),
                        Self::CONSUMED_HELP,
                    );
                }
            }

            // Leaves: nothing to descend into.
            ExprKind::SelfValue
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

    /// Reject a `spawn` whose target takes any `mut`/`out` parameter — every task
    /// would hold a writable reference to one binding, which is a data race. The
    /// safe way to share mutable state across tasks is a raw `*mut T` in `unsafe`.
    ///
    /// **The slice restriction was the bug, not the rule.** This checked
    /// `matches!(p.ty, Ty::Slice(_))`, so a `mut` STRUCT walked straight past it and
    /// `spawn bump(b)` for `fn bump(mut b: Box)` was accepted — `jestyrc check`
    /// reported every check passing. What stopped it was gcc, and only by accident:
    /// the spawn thunk stores its arguments by value while the function expects a
    /// `Box* restrict`, so the emitted C does not compile. A racy program held out of
    /// the world by a C type error is the degrades-to-gcc shape this tree keeps
    /// closing, and the race is the part that matters — the C error would disappear
    /// the moment the thunk was fixed.
    ///
    /// Found by writing an ordinary service: a worker that completes units through a
    /// `mut Service` is the obvious first draft, and it is a race.
    ///
    /// A raw `*mut T` parameter is untouched, because it carries no `mut` conv — that
    /// is the sanctioned hatch (`par_binned_sum` gives each task a disjoint region),
    /// and it is `unsafe` precisely so the disjointness is the caller's stated claim.
    fn check_spawn_no_shared_mut_slice(&mut self, call: ExprId) {
        let ExprKind::Call { callee, .. } = &self.ast.expr_at(call).kind else { return };
        // The recorded resolution, not a bare-`Name` match: a QUALIFIED spawn
        // target (`spawn m.fill(s)`) has a `Field` callee and skipped this check
        // entirely — two tasks could share a `mut` slice through exactly the
        // spelling the module system encourages. Same consolidation as the four
        // call checks above (Stage 3), same class of hole.
        let Some(name) = self.resolved_callee_name(call, *callee) else { return };
        let Some(sig) = self.info.table.fns.get(&name) else { return };
        let mut hit: Option<(String, bool)> = None;
        for p in &sig.params {
            if matches!(p.conv, Conv::Mut | Conv::Out) {
                hit = Some((p.name.clone(), matches!(p.ty, Ty::Slice(_))));
                break;
            }
        }
        if let Some((pname, is_slice)) = hit {
            let span = self.ast.expr_at(call).span;
            let fname = name.clone();
            // Two messages, because they are two different reasons for one verdict: a
            // slice races through its aliased `ptr`, a plain binding races because every
            // task writes the one place. Naming the actual hazard is what tells a caller
            // which fix applies.
            let msg = if is_slice {
                format!(
                    "`spawn`: `{fname}` takes a `mut` slice `{pname}` — a shared mutable slice can \
                     race across parallel tasks. Share mutable state through a raw `*mut T` in \
                     `unsafe` (each task a disjoint region, as `par_binned_sum` does), or pass it `read`."
                )
            } else {
                format!(
                    "`spawn`: `{fname}` takes `{pname}` by `mut` — every task would hold a writable \
                     reference to one binding, which is a data race. Share mutable state through a raw \
                     `*mut T` in `unsafe`, or pass it `read` and have each task return what it produced."
                )
            };
            self.error(span, msg);
        }
    }

    /// Item 4 stage 3 — call-site mut-slice exclusivity. The same lexical place in
    /// two writable (`mut`/`out`) **slice** argument positions of one call gives the
    /// callee two views that alias EVERY element — exactly the overlap `split_mut`
    /// exists to make unmanufacturable, freely available one call beside it until
    /// now. Refused lexically: both arguments spell the same place (`g(q, q)`,
    /// `g(s.a, s.a)`). Distinct spellings that still alias (an aliased root
    /// `var alias = q`, a view re-passed through a callee) are the same two dodges
    /// item 5 records, with the same answers. `mut`+`read` overlap at one call is
    /// deliberately allowed — in-place idioms read and write one buffer on purpose
    /// (the design note, `docs/safety-mosaic-next.md` item 4 stage 3).
    fn check_slice_alias(&mut self, call_id: ExprId, callee: ExprId, args: &[ExprId]) {
        // The runtime argument list in parameter order (receiver first for method
        // sugar) with the resolved signature's (conv, is-slice) row per parameter.
        let (fname, params, runtime): (String, Vec<(Conv, bool)>, Vec<ExprId>) =
            if let Some(mr) = self.info.method_call(call_id).cloned() {
                let ExprKind::Field { base, .. } = &self.ast.expr_at(callee).kind else {
                    return;
                };
                let Some(f) = self.find_fn(&mr.fn_name) else { return };
                let rows = f
                    .params
                    .iter()
                    .filter(|p| !p.comptime)
                    .map(|p| {
                        let is_slice = p.ty.is_some_and(|t| {
                            matches!(self.ast.type_at(t).kind, TypeKind::Slice(_))
                        });
                        (p.conv, is_slice)
                    })
                    .collect();
                let mut v = vec![*base];
                v.extend_from_slice(args);
                (mr.fn_name.clone(), rows, v)
            } else {
                let Some(name) = self.resolved_callee_name(call_id, callee) else { return };
                let Some(sig) = self.info.table.fns.get(&name) else { return };
                let rows =
                    sig.params.iter().map(|p| (p.conv, matches!(p.ty, Ty::Slice(_)))).collect();
                (name, rows, args.to_vec())
            };
        let mut seen: Vec<String> = Vec::new();
        for (i, &arg) in runtime.iter().enumerate() {
            let Some(&(conv, is_slice)) = params.get(i) else { break };
            if !(matches!(conv, Conv::Mut | Conv::Out) && is_slice) {
                continue;
            }
            let Some(key) = self.place_key(arg) else { continue };
            if seen.contains(&key) {
                self.error(
                    self.ast.expr_at(arg).span,
                    format!(
                        "cannot pass `{key}` to two writable slice parameters of `{fname}` in one \
                         call: the two views would alias every element — divide the buffer with \
                         `split_mut` instead"
                    ),
                );
                return; // one refusal per call
            }
            seen.push(key);
        }
    }

    /// The lexical spelling of a place chain (`q`, `s.a`, `p.*.buf`), or `None` when
    /// the expression is not a pure place — a call's text can repeat without its
    /// values aliasing (`g(mk(), mk())` is two fresh slices), so only places compare.
    /// Built from AST idents, so `s . a` and `s.a` spell the same key. Index steps
    /// are excluded on purpose: their subscript is an arbitrary expression whose
    /// equality this lexical rule cannot decide.
    fn place_key(&self, id: ExprId) -> Option<String> {
        match &self.ast.expr_at(id).kind {
            ExprKind::Name(n) => Some(n.name.clone()),
            ExprKind::SelfValue => Some("self".to_string()),
            ExprKind::Field { base, name } => {
                Some(format!("{}.{}", self.place_key(*base)?, name.name))
            }
            ExprKind::Deref { base } => Some(format!("{}.*", self.place_key(*base)?)),
            _ => None,
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

    /// Record a call's effects on the enclosing function, and enforce whichever
    /// proven-absence contracts that function declared.
    ///
    /// The recording half runs unconditionally — [`effect_closures`] needs every
    /// function's callees before it knows which functions are annotated — so this is
    /// also where the call-graph edges come from.
    fn check_effect_call(&mut self, call_id: ExprId, callee: ExprId, span: Span) {
        let Some(name) = self.resolved_callee_name(call_id, callee) else { return };
        if is_alloc_intrinsic(&name) {
            self.allocates = true;
        }
        if is_os_intrinsic(&name) || self.extern_fns.contains(&name) {
            self.uses_os = true;
        }
        self.calls.push((name.clone(), span));
        // A fixed order, so a call that breaks *both* contracts always reports
        // allocation first. The port golden compares diagnostics as a sequence, and a
        // set whose order depended on the attribute spelling would be a difference the
        // two implementations had to agree on for no reason.
        if self.no_alloc {
            self.report_effect(Effect::Alloc, &name, span);
        }
        if self.no_os {
            self.report_effect(Effect::Os, &name, span);
        }
    }

    /// One violation of `eff`'s contract at `span`, direct or transitive.
    ///
    /// Both contracts phrase their diagnostic identically because they *are* the same
    /// finding about different effects — a reader who has learned to read one has
    /// learned to read the other. [`Effect`] holds the four words that differ.
    fn report_effect(&mut self, eff: Effect, name: &str, span: Span) {
        // An `extern "c"` callee is the platform boundary made explicit, so it gets
        // its own sentence rather than being described as an "intrinsic" it is not.
        if eff == Effect::Os && self.extern_fns.contains(name) {
            self.error(
                span,
                format!(
                    "`{name}` is an `extern \"c\"` call into the platform's C library — \
                     forbidden in a `@no_os` function (the proven-freestanding contract)"
                ),
            );
            return;
        }
        if eff.is_intrinsic(name) {
            self.error(
                span,
                format!(
                    "`{name}` {} — forbidden in a `@{}` function ({})",
                    eff.verb(),
                    eff.attr(),
                    eff.contract()
                ),
            );
            return;
        }
        // …and the TRANSITIVE rule: calling something with the effect, however
        // indirectly, breaks the same contract. The closure carries the shortest chain,
        // so the diagnostic can name the function that actually has the effect rather
        // than only the one that was called. Cloned to end the borrow before `error`.
        let chain = match eff {
            Effect::Alloc => self.alloc_via.get(name),
            Effect::Os => self.os_via.get(name),
        };
        let Some(chain) = chain.cloned() else { return };
        let culprit = chain.last().cloned().unwrap_or_else(|| name.to_string());
        // Name the whole path only when there is one worth naming — for a direct
        // callee the chain is a single hop and "via `f`; `f` allocates" is noise.
        let detail = if chain.len() > 1 {
            format!("via `{}`; `{culprit}` {}", chain.join("` → `"), eff.culprit_verb())
        } else {
            format!("`{culprit}` {}", eff.culprit_verb())
        };
        self.error(
            span,
            format!("`{name}` {} — forbidden in a `@{}` function ({detail})", eff.verb(), eff.attr()),
        );
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

    /// The `take` shape of a call: is the method RECEIVER consumed, and which
    /// explicit argument positions land on `take` parameters. One resolution —
    /// the same `method_call`/`resolved_callee_name` chain route 4 consolidated —
    /// and the same reach: a struct METHOD's explicit parameters are in neither
    /// `table.fns` nor the top-level items, so only its receiver (recorded in
    /// `MethodRes.recv_conv`) is tracked, exactly as route 4 skips them.
    fn take_shape(&self, call_id: ExprId, callee: ExprId, argc: usize) -> (bool, Vec<bool>) {
        if let Some(mr) = self.info.method_call(call_id) {
            let recv = mr.recv_conv == Conv::Take;
            if let Some(f) = self.find_fn(&mr.fn_name) {
                let runtime: Vec<Conv> =
                    f.params.iter().filter(|p| !p.comptime).map(|p| p.conv).collect();
                let args =
                    (0..argc).map(|i| matches!(runtime.get(i + 1), Some(Conv::Take))).collect();
                return (recv, args);
            }
            return (recv, vec![false; argc]);
        }
        let takes = self
            .resolved_callee_name(call_id, callee)
            .and_then(|n| self.info.table.fns.get(&n))
            .map(|sig| {
                (0..argc)
                    .map(|i| sig.params.get(i).is_some_and(|p| p.conv == Conv::Take))
                    .collect()
            });
        (false, takes.unwrap_or_else(|| vec![false; argc]))
    }

    /// The consuming rule's gate: does this type run drop glue the callee now
    /// owns? v1 answers "has a direct concrete `impl Drop`" — the first branch
    /// of cgen's `drop_key_of`, read from the same `impl_index` so the two
    /// cannot drift on it. **This answers the DIRECT question only**, and that is
    /// now deliberate rather than residue: the transitive walk lives one level up
    /// in `owns_resource_at`, which asks this about every by-value field, so a
    /// wrapper whose FIELD has the impl is gated there. Blanket generic
    /// `impl[T] Drop` instances are handled by the constructor match below.
    /// `a_wrapper_with_a_droppable_field_cannot_be_reused_after_consumption` is
    /// the test that used to pin the gap as accepted.
    /// Drop-free non-Copy values stay unmarked ON PURPOSE: giving one to `take`
    /// is an implicit copy (MVS trivially-copyable semantics — the corpus's
    /// Option combinators reuse their inputs freely), and nothing observable is
    /// left behind in the caller.
    fn droppable_ty(&self, ty: &Ty) -> bool {
        let key = self.info.table.ty_key(ty);
        if !key.is_empty()
            && self.info.table.impl_index.contains_key(&("Drop".to_string(), key))
        {
            return true;
        }
        // **A BLANKET impl is keyed by its own spelling, not by its instances.**
        // `impl[T] Drop for List(T)` registers under `List(T)`, so a concrete
        // `List(i64)` — key `List(i64)` — never matched, and every ownership rule that
        // consults this silently skipped the most-used droppable in the tree: a
        // use-after-`take` of a `List` was NOT diagnosed.
        //
        // That was a pre-existing hole in the `take` rule, found while adding the
        // rebinding one, and it is the same shape as the miscompiles already recorded —
        // a lookup that misses returns "no" rather than failing, so the check just
        // stops happening for that type.
        //
        // Matching on the CONSTRUCTOR closes it: any `Drop` impl whose key names the
        // same generic constructor makes every instance of it droppable. That is exactly
        // what a blanket impl means, and a non-blanket `impl Drop for List(i64)` would
        // have been caught by the exact-key lookup above.
        match ty {
            Ty::GenStruct { ctor, .. } | Ty::GenEnum { ctor, .. } => {
                let prefix = format!("{ctor}(");
                self.info
                    .table
                    .impl_index
                    .keys()
                    .any(|(tr, k)| tr == "Drop" && k.starts_with(&prefix))
            }
            _ => false,
        }
    }

    /// Is `ty` declared `@move` — a RESOURCE that may not be duplicated?
    ///
    /// Read straight off the type declaration, so it is a property of the type and not of
    /// any impl. That is the whole point: the seven OS-handle types in the `sys` tier are
    /// plain structs around an integer with no `Drop`, and `droppable_ty` therefore says
    /// no about every one of them.
    fn move_only_ty(&self, ty: &Ty) -> bool {
        match ty {
            Ty::Named(i) => self.info.table.types.get(*i).is_some_and(|d| d.is_move),
            // A generic instance inherits its constructor's declaration: `@move struct
            // Handle(T)` makes `Handle(i32)` a resource, exactly as a blanket `Drop` impl
            // makes every instance droppable in `droppable_ty` above.
            Ty::GenStruct { ctor, .. } => self
                .info
                .table
                .type_index
                .get(ctor)
                .and_then(|i| self.info.table.types.get(*i))
                .is_some_and(|d| d.is_move),
            _ => false,
        }
    }

    /// Does a value of `ty` own something that must not be duplicated?
    ///
    /// The gate every ownership rule below consults. It is the UNION of two independent
    /// properties, and keeping them separate is deliberate:
    ///
    ///   * `droppable_ty` — teardown runs, so a copy would run it twice.
    ///   * `move_only_ty` — the value names a resource, so a copy is a second name for one
    ///     OS handle whether or not anything automatic happens at scope exit.
    ///
    /// The `sys` tier is entirely the second kind. Its handles close FALLIBLY — the caller
    /// is meant to see whether the bytes landed — so giving them a `Drop` to reach the
    /// first property would have discarded exactly the verdict `@must_use` exists to
    /// preserve, and would have closed them at every scope exit besides.
    /// See `owns_resource_at`. Not a semantic bound — a guard against a cyclic type
    /// table, which a legal program cannot produce.
    const OWNS_RESOURCE_MAX_DEPTH: usize = 64;

    fn owns_resource(&self, ty: &Ty) -> bool {
        self.owns_resource_at(ty, 0)
    }

    /// `owns_resource`, walking BY-VALUE fields and enum payloads.
    ///
    /// Both halves of the gate were declaration-local, and both were wrong in the
    /// same way: a wrapper around an owning thing owns that thing. The two holes
    /// that closes are one hole seen twice —
    ///
    ///   * `alog.Cursor` holds a `file.Reader`, which is `@move`, and `Cursor`
    ///     itself carried no attribute — so it was freely copyable and a copy was a
    ///     second name for one OS file descriptor. `alog.Log` holds a `file.Writer`
    ///     and DID say `@move`, by hand, two hundred lines earlier in the same
    ///     module. That is the convention failing in the one place best placed to
    ///     remember it.
    ///   * `transitively_droppable_reuse_is_v1_residue` pinned the other half: a
    ///     wrapper with a droppable FIELD slipped the gate while cgen's `needs_drop`
    ///     dropped it in the callee, so consume-then-reuse was a use-after-drop that
    ///     escape accepted. That test now expects the rejection.
    ///
    /// Deliberately shaped to match cgen's `needs_drop`, because the two answer the
    /// same question from opposite ends and drifting apart is what produced the
    /// residue above:
    ///
    ///   * **Indirection is not followed.** Only `Ty::Named` aggregates are walked,
    ///     so a `*mut Socket` or a `&Socket` field is a pointer to a resource, not
    ///     ownership of one. Stopping at indirection is also what guarantees
    ///     termination — a by-value aggregate cannot contain itself.
    ///   * **`@copy` still wins.** An aggregate the author declared copyable is left
    ///     alone, exactly as `needs_drop` leaves it alone. A `@copy` struct holding a
    ///     `@move` field is a contradiction the language should reject outright; it
    ///     is not in the corpus, and diagnosing it is its own increment rather than
    ///     something to do silently from in here. Pinned by
    ///     `a_copy_wrapper_around_a_resource_is_pinned_residue`.
    ///
    /// The depth cap is belt-and-braces against a malformed type table rather than a
    /// real bound: nothing legal gets close to it, and a program that did would have
    /// failed the size check in typeck first.
    fn owns_resource_at(&self, ty: &Ty, depth: usize) -> bool {
        if self.droppable_ty(ty) || self.move_only_ty(ty) {
            return true;
        }
        if depth >= Self::OWNS_RESOURCE_MAX_DEPTH || ty.is_copy(&self.info.table) {
            return false;
        }
        let Ty::Named(i) = ty else { return false };
        let Some(decl) = self.info.table.types.get(*i) else { return false };
        match &decl.kind {
            TypeKindG::Struct { fields } => {
                fields.iter().any(|(_, f)| self.owns_resource_at(f, depth + 1))
            }
            TypeKindG::Enum { variants } => variants
                .iter()
                .any(|(_, payload)| payload.iter().any(|f| self.owns_resource_at(f, depth + 1))),
            _ => false,
        }
    }

    /// The consuming rule's marking half: `arg` is being given to a `take`
    /// parameter. A droppable bare-Name OWNED local is marked consumed at its
    /// binding (a borrow already got route 4's give-away error, and a non-local
    /// name has no binding to poison). Inside a loop or closure, consuming a
    /// binding declared outside it is refused at the consume site. A droppable
    /// Field/Index PROJECTION is refused outright: the container still owns
    /// (and will drop) that part, so the callee's drop — which the take-drop
    /// rule in cgen now emits — would be a second drop of the same value.
    fn consume_take_arg(&mut self, ctx: &mut FnCtx, arg: ExprId) {
        let span = self.ast.expr_at(arg).span;
        match &self.ast.expr_at(arg).kind {
            ExprKind::Name(n) => {
                let name = n.name.clone();
                if ctx.lookup(&name) != Some(false) {
                    return;
                }
                if !self.owns_resource(&self.info.type_of(arg).clone()) {
                    return;
                }
                if let Some(&floor) = ctx.loop_floor.last() {
                    if ctx.scope_depth_of(&name).is_some_and(|d| d < floor) {
                        self.error_help(
                            span,
                            format!(
                                "cannot give `{name}` to a `take` parameter here: `{name}` is declared \
                                 outside the enclosing loop or closure, which may run again — and the \
                                 value is already gone the second time"
                            ),
                            "declare the value inside the loop so each run owns a fresh one, or pass \
                             a borrow (`read`/`mut`) so one owner keeps it",
                        );
                        return;
                    }
                }
                ctx.mark_consumed(&name, MoveCause::TakeArg);
            }
            ExprKind::Field { .. } | ExprKind::Index { .. } => {
                let root = self.root_name(ctx, arg);
                if ctx.lookup(&root) != Some(false) {
                    return;
                }
                if !self.owns_resource(&self.info.type_of(arg).clone()) {
                    return;
                }
                self.error_help(
                    span,
                    format!(
                        "cannot give a droppable part of `{root}` to a `take` parameter: `{root}` \
                         still owns it and will drop it — the callee's drop would be a second one"
                    ),
                    "pass the whole value with `take` so ownership moves together, or pass a \
                     borrow (`read`/`mut`) so it stays put",
                );
            }
            _ => {}
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
/// A proven-**absence** contract: an effect an annotated function must not have,
/// together with the four words its diagnostic differs by.
///
/// Two contracts, one sentence shape. Keeping the phrasing in a shared place is not
/// tidiness — it is what stops the two from drifting into differently-structured
/// messages for the same finding, which is the thing that makes a diagnostic style
/// unlearnable.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Effect {
    /// Allocation, heap or arena — `@no_alloc`.
    Alloc,
    /// Any OS-facing intrinsic — `@no_os`.
    Os,
}

impl Effect {
    fn attr(self) -> &'static str {
        match self {
            Effect::Alloc => "no_alloc",
            Effect::Os => "no_os",
        }
    }
    /// What the offending callee does — the sentence's verb.
    fn verb(self) -> &'static str {
        match self {
            Effect::Alloc => "allocates",
            Effect::Os => "reaches the operating system",
        }
    }
    /// The parenthetical on a DIRECT violation: the contract being broken.
    fn contract(self) -> &'static str {
        match self {
            Effect::Alloc => "the proven-allocation-free contract",
            Effect::Os => "the proven-freestanding contract",
        }
    }
    /// What the function at the far end of a transitive chain does.
    fn culprit_verb(self) -> &'static str {
        match self {
            Effect::Alloc => "allocates directly",
            Effect::Os => "calls the OS directly",
        }
    }
    /// Is `name` an intrinsic with this effect — the *direct* rule?
    fn is_intrinsic(self, name: &str) -> bool {
        match self {
            Effect::Alloc => is_alloc_intrinsic(name),
            Effect::Os => is_os_intrinsic(name),
        }
    }
}

/// The OS boundary, as a closed list — every intrinsic that needs a hosted platform
/// underneath it. This is what `@no_os` proves the absence of, and therefore what
/// "`core` links on a freestanding target" actually means.
///
/// Four groups, and each is here for a concrete reason rather than by analogy:
///
/// * **Files** — `read_file`, `try_read_file`, `write_file`, `file_exists`,
///   `remove_file`. A filesystem.
/// * **Process, arguments, environment** — `run_command`, `arg_count`, `arg`,
///   `env_var`. A process model, an `argv`, an environment block.
/// * **The clock** — `mono_nanos`. A monotonic timer the platform has to supply.
/// * **Standard streams** — `print_int`, `print_float`, `print_str`, `print_bool`,
///   `eprint_str`. These are the easy ones to forget, and the reason the attribute is
///   worth having: a debug `print_str` left in a `core` function is invisible in review
///   and fatal on a target with no stdout.
///
/// The list mirrors `typeck::io_intrinsic_ret` plus the print family. It is restated
/// rather than shared because that function answers "what type does this return", not
/// "does this touch the OS", and the two questions will not always have the same answer
/// — but a name that drifted out of one and not the other would make this check
/// silently vacuous *for that intrinsic*, so `no_os_props` exercises every name here
/// against the real checker.
/// The names of every `extern "c"` declaration in the program.
fn extern_fn_names(ast: &Ast) -> HashSet<String> {
    ast.items
        .iter()
        .filter_map(|it| match it {
            Item::Extern(e) => Some(e.name.name.clone()),
            _ => None,
        })
        .collect()
}

fn is_os_intrinsic(name: &str) -> bool {
    matches!(
        name,
        "read_file"
            | "try_read_file"
            | "write_file"
            | "file_exists"
            | "remove_file"
            | "run_command"
            | "arg_count"
            | "arg"
            | "env_var"
            | "mono_nanos"
            | "print_int"
            | "print_float"
            | "print_str"
            | "print_bool"
            | "eprint_str"
            // Signals are an OS effect in both directions: arming installs a handler
            // in the process, and reading observes something only the kernel can have
            // changed. A `@no_os` function must reach neither.
            | "signal_arm"
            | "signal_caught"
            | "signal_raise"
            // Entropy comes from the kernel, so a `@no_os` function must not reach it.
            | "random_fill"
    )
}

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

    // --- item 4 stage 3: call-site mut-slice exclusivity ---

    /// The measured hole beside `split_mut`, closed: the same lexical place in two
    /// writable slice argument positions is refused — `g(q, q)` was two total-alias
    /// `mut` views through any user fn, one call away from the library that exists
    /// to make that overlap unmanufacturable. The chain compares, not just the
    /// root: `g(t.lo, t.lo)` is refused while `g(t.lo, t.hi)` stays legal.
    #[test]
    fn the_same_place_cannot_feed_two_writable_slice_params() {
        let hdr = "fn g(mut a: []i64, mut b: []i64) { a[0] = 1  b[0] = 2 } ";
        let d = escapes(&(hdr.to_string() + "fn m(mut q: []i64) { g(q, q) }"));
        assert_eq!(d.len(), 1, "one refusal: {d:?}");
        assert!(d[0].message.contains("two writable slice parameters"), "{d:?}");
        assert!(d[0].message.contains("split_mut"), "the fix is named: {d:?}");

        let fields = "struct S { lo: []i64, hi: []i64 } ";
        let d2 = escapes(&(hdr.to_string() + fields + "fn n(read s: S) { g(s.lo, s.lo) }"));
        assert!(
            d2.iter().any(|x| x.message.contains("two writable slice parameters")),
            "the whole chain compares: {d2:?}"
        );
    }

    /// What stays legal, on purpose: distinct places (disjoint by declaration),
    /// `read`+`mut` overlap (in-place idioms read and write one buffer), and a
    /// repeated non-place argument (`g(mk(), mk())` is two fresh slices — text
    /// equality means nothing off a place chain).
    #[test]
    fn distinct_places_read_overlap_and_non_places_stay_legal() {
        let hdr = "fn g(mut a: []i64, mut b: []i64) { a[0] = 1  b[0] = 2 } \
                   fn r(read a: []i64, mut b: []i64) { b[0] = a[0] } \
                   struct S { lo: []i64, hi: []i64 } ";
        for tail in [
            "fn n(read s: S) { g(s.lo, s.hi) }",
            "fn m(mut q: []i64) { r(q, q) }",
        ] {
            let d = escapes(&(hdr.to_string() + tail));
            assert!(
                !d.iter().any(|x| x.message.contains("two writable slice parameters")),
                "`{tail}` must stay legal: {d:?}"
            );
        }
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

    // --- @no_os: the enforced freestanding contract ---
    //
    // The `core` tier's central claim, made checkable. Deliberately structured as a
    // near-copy of the `@no_alloc` family above, because the two contracts are the same
    // analysis over different effects — if one grows a case the other should be asked
    // whether it needs the same one.

    #[test]
    fn no_os_rejects_a_direct_os_call() {
        let d = escapes("@no_os fn f(n: i32) -> i32 { print_int(n as i64) return n }");
        assert!(!d.is_empty(), "a printing @no_os body must be rejected");
        assert!(d[0].message.contains("@no_os"), "{d:?}");
        assert!(d[0].message.contains("reaches the operating system"), "{d:?}");
    }

    /// Every name in [`is_os_intrinsic`] must actually be rejected. A typo in that list
    /// would not fail to compile — it would silently make the check **vacuous for that
    /// intrinsic**, which is the worst failure mode an absence-proof can have.
    #[test]
    fn no_os_rejects_every_intrinsic_on_the_list() {
        // Each with a call shape that type-checks, so a diagnostic can only come from
        // the contract and never from a mis-typed probe.
        for call in [
            "print_int(1)",
            "print_float(1.0)",
            "print_str(\"x\")",
            "print_bool(true)",
            "eprint_str(\"x\")",
            "read_file(\"f\")",
            "try_read_file(\"f\")",
            "write_file(\"f\", \"c\")",
            "file_exists(\"f\")",
            "remove_file(\"f\")",
            "run_command(\"c\")",
            "arg_count()",
            "arg(0)",
            "env_var(\"P\")",
            "mono_nanos()",
        ] {
            let src = format!("@no_os fn f() -> i32 {{ let _v = {call} return 0 }}");
            let d = escapes(&src);
            assert!(
                d.iter().any(|m| m.message.contains("@no_os")),
                "`{call}` is on the OS list but was not rejected — the check is vacuous \
                 for it: {d:?}"
            );
        }
    }

    /// Threads are an OS service too. This is the case the first implementation missed:
    /// it certified `core.par_binned_sum` — which spawns four workers — as freestanding.
    #[test]
    fn no_os_rejects_starting_a_thread() {
        let spawned = escapes(
            "fn sq(x: i64) -> i64 { return x * x } \
             @no_os fn f() -> i32 { let h = spawn sq(3) let _v = await h return 0 }",
        );
        assert!(
            spawned.iter().any(|m| m.message.contains("`spawn` starts a thread")),
            "spawning under @no_os must be rejected: {spawned:?}"
        );
        let par = escapes(
            "@no_os fn f(read xs: []i64) -> i64 { return par for x in xs reduce(core_add) { x } }",
        );
        assert!(
            par.iter().any(|m| m.message.contains("`par for` loop starts threads")),
            "a par for under @no_os must be rejected: {par:?}"
        );
    }

    /// A `concurrent` scope that spawns nothing starts no thread, so it is not itself
    /// the effect — and pinning that is what stops the rule from being restated on the
    /// block later and double-reporting every `concurrent { spawn … }`.
    #[test]
    fn no_os_reports_the_spawn_not_the_concurrent_block() {
        let d = escapes(
            "fn sq(x: i64) -> i64 { return x * x } \
             @no_os fn f() -> i32 { concurrent { let h = spawn sq(3) let _v = await h } return 0 }",
        );
        let hits = d.iter().filter(|m| m.message.contains("@no_os")).count();
        assert_eq!(hits, 1, "exactly one diagnostic, at the spawn: {d:?}");
    }

    #[test]
    fn no_os_accepts_a_pure_body() {
        let d = escapes("@no_os fn f(a: i32, b: i32) -> i32 { let s = a + b return s }");
        assert!(d.is_empty(), "pure computation must be accepted: {d:?}");
    }

    #[test]
    fn no_os_rejects_an_os_call_one_hop_away() {
        let d = escapes(
            "fn helper(n: i32) { print_int(n as i64) } \
             @no_os fn f(n: i32) -> i32 { helper(n) return 0 }",
        );
        assert!(d.iter().any(|m| m.message.contains("@no_os")), "{d:?}");
        assert!(d[0].message.contains("`helper` calls the OS directly"), "{d:?}");
    }

    #[test]
    fn no_os_names_the_chain_to_the_real_culprit() {
        let d = escapes(
            "fn deep(n: i32) { print_int(n as i64) } \
             fn middle(n: i32) { deep(n) } \
             fn outer(n: i32) { middle(n) } \
             @no_os fn f(n: i32) -> i32 { outer(n) return 0 }",
        );
        assert!(!d.is_empty(), "must be rejected");
        assert!(
            d[0].message.contains("via `middle` → `deep`; `deep` calls the OS directly"),
            "the chain must name the real culprit: {d:?}"
        );
    }

    #[test]
    fn no_os_terminates_on_recursive_call_graphs() {
        // Mutual recursion must settle, not loop — the same totality property the
        // allocation closure has.
        let d = escapes(
            "fn a(n: i32) -> i32 { if n > 0 { return b(n - 1) } return 0 } \
             fn b(n: i32) -> i32 { return a(n) } \
             @no_os fn f(n: i32) -> i32 { return a(n) }",
        );
        assert!(d.is_empty(), "an OS-free cycle must be accepted: {d:?}");
    }

    /// **An `extern "c"` call is the platform boundary, so `@no_os` must see it.**
    ///
    /// The OS-intrinsic list is closed, and that was sound only while the intrinsics
    /// WERE the whole platform. `extern "c"` works — `examples/extern_c.jtr` calls
    /// libc's `puts` and `abs` — so a `@no_os` function could reach the C library
    /// directly and still be certified freestanding. `docs/stdlib-roadmap.md`
    /// predicted this gap on the assumption `extern "c"` was still to come; it was
    /// already here, so the rule was already owed.
    #[test]
    fn no_os_rejects_an_extern_c_call() {
        let d = escapes(
            "extern \"c\" fn abs(x: i32) -> i32 \
             @no_os fn f(x: i32) -> i32 { return abs(x) }",
        );
        assert!(
            d.iter().any(|m| m.message.contains("extern")),
            "an extern call leaves Jestyr for the platform: {d:?}"
        );

        // Transitively, too — one hop away is still the platform.
        let d = escapes(
            "extern \"c\" fn abs(x: i32) -> i32 \
             fn helper(x: i32) -> i32 { return abs(x) } \
             @no_os fn f(x: i32) -> i32 { return helper(x) }",
        );
        assert!(d.iter().any(|m| m.message.contains("@no_os")), "{d:?}");

        // …and an unannotated function may of course call it.
        let d = escapes(
            "extern \"c\" fn abs(x: i32) -> i32 \
             fn f(x: i32) -> i32 { return abs(x) }",
        );
        assert!(d.is_empty(), "{d:?}");
    }

    /// **The two contracts are orthogonal axes, and both directions matter.**
    ///
    /// `std/sha256` is the living case for the first half — it builds a `String` and
    /// touches no OS — and if `@no_os` ever started refusing allocation, that module
    /// would have to drop a true claim. The second half keeps `@no_alloc` from
    /// acquiring an OS rule by osmosis.
    #[test]
    fn the_two_absence_contracts_do_not_leak_into_each_other() {
        let allocating = escapes(
            "@no_os fn f(n: i32) -> i32 { let p = alloc(i32, n) free_ptr(p) return 0 }",
        );
        assert!(allocating.is_empty(), "@no_os says nothing about allocation: {allocating:?}");

        let printing = escapes("@no_alloc fn f(n: i32) -> i32 { print_int(n as i64) return n }");
        assert!(printing.is_empty(), "@no_alloc says nothing about the OS: {printing:?}");
    }

    /// Both contracts on one function report both violations, in a fixed order — so a
    /// function claiming both proofs and having both effects is told about both rather
    /// than fixing one and rediscovering the other.
    #[test]
    fn a_call_breaking_both_contracts_reports_both_allocation_first() {
        let d = escapes(
            "fn bad(n: i32) -> *mut i32 { print_int(n as i64) return alloc(i32, n) } \
             @no_alloc @no_os fn f(n: i32) -> i32 { let p = bad(n) free_ptr(p) return 0 }",
        );
        let contract: Vec<&str> = d
            .iter()
            .filter(|m| m.message.contains("forbidden in a"))
            .map(|m| if m.message.contains("@no_alloc") { "alloc" } else { "os" })
            .collect();
        assert_eq!(contract, vec!["alloc", "os"], "both, allocation first: {d:?}");
    }

    /// `@no_os` is per-function, exactly as `@no_alloc` is: an unannotated neighbour
    /// may print freely.
    #[test]
    fn no_os_is_per_function_not_inherited() {
        let d = escapes(
            "@no_os fn f(n: i32) -> i32 { return n } \
             fn g(n: i32) { print_int(n as i64) }",
        );
        assert!(d.is_empty(), "{d:?}");
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

    /// Item 5 residue (a), closed: `var alias = h` INSIDE the inner region gives
    /// the chain an inner-declared root that still REACHES `h`'s outer storage —
    /// the recorded dodge around the route-3 depth compare. The alias taint makes
    /// the root answer its inherited depth, so the store is refused with a message
    /// naming the aliasing (not a false "declared outside").
    #[test]
    fn an_aliased_root_cannot_dodge_the_store_through_outer_rule() {
        let src = "struct Holder { p: &[r]str } \
                   fn f() -> i32 { \
                       region outer { \
                           var h: &[outer]Holder = region_alloc(outer, Holder, Holder { p: region_alloc(outer, str, \"ok\") }) \
                           region inner { \
                               var alias = h \
                               alias.*.p = region_alloc(inner, str, \"gone\") \
                           } \
                       } \
                       return 0 }";
        let d = escapes(src);
        assert!(
            d.iter().any(|x| x.message.contains("aliases storage declared outside")),
            "the aliased root is seen through: {d:?}"
        );
    }

    /// The taint must not over-reach: an alias taken in the SAME region as its
    /// root stores region values through it freely (equal depth), and rebinding
    /// the name to a fresh value clears the inherited reach.
    #[test]
    fn a_same_region_alias_stays_legal() {
        let src = "struct Holder { p: &[r]str } \
                   fn f() -> i32 { \
                       region r { \
                           var h: &[r]Holder = region_alloc(r, Holder, Holder { p: region_alloc(r, str, \"ok\") }) \
                           var alias = h \
                           alias.*.p = region_alloc(r, str, \"still fine\") \
                       } \
                       return 0 }";
        let d = escapes(src);
        assert!(d.is_empty(), "a same-region alias is not an escape: {d:?}");
    }

    #[test]
    fn copy_struct_param_can_be_returned() {
        // `@copy` opts the aggregate into being freely copyable — no escape.
        let d = escapes("@copy struct V { x: i32 } fn id(read v: V) -> V { return v }");
        assert!(d.is_empty(), "a @copy aggregate may be returned by value: {:?}", d);
    }

    /// The enum form of the opt-in (`dlist_genref.jtr` datum 2): a `@copy` enum
    /// whose payloads are all Copy — the niche `Link`-over-genref — moves out of a
    /// `read` param as a copy, so link surgery no longer needs `take` params. The
    /// UN-annotated twin stays refused, so the opt-in is doing the work.
    #[test]
    fn copy_enum_of_copy_payloads_can_be_returned() {
        let d = escapes(
            "@copy enum Link { nil, at(n: &i64) } \
             fn next(read l: Link) -> Link { return l }",
        );
        assert!(d.is_empty(), "a @copy enum of Copy payloads is freely copyable: {d:?}");
        let d2 = escapes("enum Link { nil, at(n: &i64) } fn next(read l: Link) -> Link { return l }");
        assert!(
            !d2.is_empty(),
            "without @copy the same return must still be refused (the opt-in is load-bearing)"
        );
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

    // --- the consuming rule: use-after-`take` of a droppable ---

    /// The shared prelude for the consuming-rule probes: a droppable `Device`
    /// plus a consumer and a pass-through.
    const DROPPY: &str = "trait Drop { fn drop(mut self) } \
        struct Device { id: i64 } \
        impl Drop for Device { fn drop(mut self) { print_int(self.id) } } \
        fn consume(take d: Device) -> i64 { return d.id } \
        fn pass_on(take d: Device) -> Device { return d }";

    #[test]
    fn use_after_take_of_droppable_is_rejected() {
        // The rust_vs_jestyr probe: `consume(d)` then `d.id` — the callee has
        // dropped the value by the time the caller reads its stale copy.
        let d = escapes(&format!(
            "{DROPPY} fn main() -> i64 {{ let d = Device{{ id: 7 }} let e = consume(d) print_int(d.id) return e }}"
        ));
        assert_eq!(d.len(), 1, "{:?}", d);
        assert!(
            d[0].message.contains("cannot use `d` after it was given to a `take` parameter"),
            "{:?}",
            d
        );
    }

    #[test]
    fn rebinding_the_result_of_a_take_chain_is_legal() {
        // `pass_on` moves the value on; the caller re-owns it under a fresh
        // binding and may consume that one exactly once.
        let d = escapes(&format!(
            "{DROPPY} fn main() -> i64 {{ let a = Device{{ id: 1 }} let b = pass_on(a) return consume(b) }}"
        ));
        assert!(d.is_empty(), "{:?}", d);
    }

    #[test]
    fn double_consume_in_one_call_is_rejected() {
        // `g(d, d)` with two `take` params: the first argument consumes, the
        // second is already a use-after-consume (arguments mark in order).
        let d = escapes(&format!(
            "{DROPPY} fn pair(take a: Device, take b: Device) {{}} \
             fn main() -> i64 {{ let d = Device{{ id: 1 }} pair(d, d) return 0 }}"
        ));
        assert_eq!(d.len(), 1, "{:?}", d);
        assert!(d[0].message.contains("after it was given to a `take` parameter"), "{:?}", d);
    }

    #[test]
    fn drop_free_reuse_after_take_stays_legal() {
        // MVS: a drop-free non-Copy value given to `take` is an implicit copy —
        // nothing observable is left behind, so reuse stays legal (this is the
        // corpus's Option-combinator shape, and the deliberate gate).
        let d = escapes(
            "struct Plain { v: i64 } fn eat(take p: Plain) -> i64 { return p.v } \
             fn main() -> i64 { let p = Plain{ v: 3 } let a = eat(p) return a + eat(p) }",
        );
        assert!(d.is_empty(), "{:?}", d);
    }

    #[test]
    fn reassignment_after_consume_is_rejected() {
        // No runtime drop flags: a consumed binding cannot be reinitialized —
        // the Assign target's walk reports the use.
        let d = escapes(&format!(
            "{DROPPY} fn main() -> i64 {{ var d = Device{{ id: 1 }} let e = consume(d) d = Device{{ id: 2 }} return e }}"
        ));
        assert_eq!(d.len(), 1, "{:?}", d);
        assert!(d[0].message.contains("cannot use `d`"), "{:?}", d);
    }

    #[test]
    fn both_branches_may_consume_independently() {
        // Only one branch runs: each may consume the same binding.
        let d = escapes(&format!(
            "{DROPPY} fn main() -> i64 {{ let d = Device{{ id: 1 }} \
             if d.id > 0 {{ return consume(d) }} else {{ return consume(d) }} }}"
        ));
        assert!(d.is_empty(), "{:?}", d);
    }

    #[test]
    fn use_after_conditional_consume_is_rejected() {
        // A lone `if` may have run its consume — the use after it errors
        // (branch sets merge by union; Rust rejects the same shape).
        let d = escapes(&format!(
            "{DROPPY} fn main() -> i64 {{ let d = Device{{ id: 1 }} var e: i64 = 0 \
             if d.id > 0 {{ e = consume(d) }} print_int(d.id) return e }}"
        ));
        assert_eq!(d.len(), 1, "{:?}", d);
        assert!(d[0].message.contains("cannot use `d`"), "{:?}", d);
    }

    // --- `@move`: a resource with no `Drop` (the `sys` tier's eight OS handles) ---
    //
    // The ownership rules used to be gated on `droppable_ty` alone, so only a type with a
    // `Drop` impl could be consumed. Every handle in the `sys` tier is a plain struct
    // around an integer, so all of them were freely copyable — and closing through a copy
    // leaves the other name pointing at a descriptor the platform may have reissued.
    //
    // Each test below is paired with the SAME source minus the attribute. That control is
    // the whole point: without it, a rule that rejected all struct rebinding would pass.
    const HANDLE: &str = "@move struct H { fd: i64 } \
                          fn mk() -> H { return H { fd: 3 } } \
                          fn sink(take h: H) -> i64 { return h.fd } \
                          fn peek(read h: H) -> i64 { return h.fd } ";
    const PLAIN: &str = "struct H { fd: i64 } \
                         fn mk() -> H { return H { fd: 3 } } \
                         fn sink(take h: H) -> i64 { return h.fd } \
                         fn peek(read h: H) -> i64 { return h.fd } ";

    #[test]
    fn rebinding_a_move_only_value_consumes_the_old_name() {
        let d = escapes(&format!(
            "{HANDLE} fn f() -> i64 {{ var a: H = mk() let b: H = a return b.fd + a.fd }}"
        ));
        assert_eq!(d.len(), 1, "{d:?}");
        assert!(d[0].message.contains("moved to another binding"), "{d:?}");
    }

    #[test]
    fn giving_a_move_only_value_to_take_consumes_it() {
        let d = escapes(&format!(
            "{HANDLE} fn f() -> i64 {{ var c: H = mk() let n: i64 = sink(c) return n + c.fd }}"
        ));
        assert_eq!(d.len(), 1, "{d:?}");
        assert!(d[0].message.contains("given to a `take` parameter"), "{d:?}");
    }

    /// **The control for both tests above.** Byte-identical source, no attribute. If this
    /// is ever non-empty, `@move` is not what is doing the work.
    #[test]
    fn without_the_attribute_the_same_program_is_legal() {
        let a = escapes(&format!(
            "{PLAIN} fn f() -> i64 {{ var a: H = mk() let b: H = a return b.fd + a.fd }}"
        ));
        assert!(a.is_empty(), "a plain struct still copies freely: {a:?}");
        let b = escapes(&format!(
            "{PLAIN} fn f() -> i64 {{ var c: H = mk() let n: i64 = sink(c) return n + c.fd }}"
        ));
        assert!(b.is_empty(), "giving a plain struct to `take` is an implicit copy: {b:?}");
    }

    /// A BORROW is not a move — and this is the boundary the `sys` tier actually lives on.
    /// Adopting `@move` on all eight handle types broke no corpus file precisely because
    /// every one of them is passed around by `read`/`mut`, never by value.
    #[test]
    fn borrowing_a_move_only_value_does_not_consume_it() {
        let d = escapes(&format!(
            "{HANDLE} fn f() -> i64 {{ var h: H = mk() let x: i64 = peek(h) return x + peek(h) }}"
        ));
        assert!(d.is_empty(), "a `read` borrow must not consume: {d:?}");
    }

    #[test]
    fn consume_of_outer_binding_in_loop_is_rejected() {
        // The second iteration would consume a value already gone.
        let d = escapes(&format!(
            "{DROPPY} fn main() -> i64 {{ let d = Device{{ id: 1 }} var i: i64 = 0 \
             for i < 3 {{ let e = consume(d) i = i + e }} return i }}"
        ));
        assert_eq!(d.len(), 1, "{:?}", d);
        assert!(
            d[0].message.contains("outside the enclosing loop or closure"),
            "{:?}",
            d
        );
    }

    #[test]
    fn consume_of_loop_local_binding_is_legal() {
        // A fresh value per iteration is the fix the loop message suggests.
        let d = escapes(&format!(
            "{DROPPY} fn main() -> i64 {{ var i: i64 = 0 \
             for i < 3 {{ let d = Device{{ id: i }} i = i + consume(d) }} return i }}"
        ));
        assert!(d.is_empty(), "{:?}", d);
    }

    #[test]
    fn moving_a_droppable_field_out_is_rejected() {
        // After the take-drop fix the callee drops its copy while the container
        // still drops the original at scope exit — a projection into `take` is
        // a double drop, refused at the call.
        let d = escapes(&format!(
            "{DROPPY} struct Holder {{ dev: Device }} \
             fn main() -> i64 {{ let h = Holder{{ dev: Device{{ id: 1 }} }} return consume(h.dev) }}"
        ));
        assert_eq!(d.len(), 1, "{:?}", d);
        assert!(
            d[0].message.contains("cannot give a droppable part of `h`"),
            "{:?}",
            d
        );
    }

    #[test]
    fn a_wrapper_with_a_droppable_field_cannot_be_reused_after_consumption() {
        // WAS `transitively_droppable_reuse_is_v1_residue`, which pinned this as
        // ACCEPTED and said in as many words that it would flip to a rejection once
        // the gate learned the transitive walk. It has.
        //
        // What it was pinning is a use-after-drop, not an untidiness: cgen's
        // `needs_drop` DOES recurse, so `eat(w)` drops `w.dev` inside the callee —
        // and then `main` read `w.dev.id`. The two halves of the compiler disagreed
        // about whether a wrapper owns what it wraps, and escape's half was the
        // permissive one.
        let d = escapes(&format!(
            "{DROPPY} struct Wrap {{ dev: Device }} fn eat(take w: Wrap) {{}} \
             fn main() -> i64 {{ let w = Wrap{{ dev: Device{{ id: 1 }} }} eat(w) return w.dev.id }}"
        ));
        assert_eq!(d.len(), 1, "{:?}", d);
        assert!(
            d[0].message.contains("cannot use `w` after it was given to a `take` parameter"),
            "{:?}",
            d
        );
    }

    #[test]
    fn a_wrapper_with_a_move_only_field_cannot_be_reused_after_consumption() {
        // The other half of the same walk, and the one with a live instance in the
        // shipped library: `alog.Cursor` holds a `file.Reader`, which is `@move`,
        // and `Cursor` carried no attribute of its own — so a copy of a Cursor was a
        // second name for one OS file descriptor. `alog.Log` wraps a `file.Writer`
        // and DID say `@move`, by hand, in the same module. That is the convention
        // failing where it was best placed to hold.
        //
        // No `Drop` impl anywhere here: this is reached only through `move_only_ty`,
        // so it fails if the walk consults just the droppable half.
        let d = escapes(
            "@move struct Handle { fd: i64 } struct Session { h: Handle, n: i64 } \
             fn eat(take s: Session) {} \
             fn main() -> i64 { let s = Session{ h: Handle{ fd: 3 }, n: 1 } eat(s) return s.n }",
        );
        assert_eq!(d.len(), 1, "a wrapper around a @move field owns it: {:?}", d);
        assert!(
            d[0].message.contains("cannot use `s` after it was given to a `take` parameter"),
            "{:?}",
            d
        );
    }

    #[test]
    fn a_wrapper_around_nothing_owning_is_still_freely_reusable() {
        // The positive control the rule owes. Same shape, same `take`, and the only
        // difference is that the field owns nothing — so a walk that answered "owns
        // a resource" for every aggregate would fail here and pass everything above,
        // and the two tests together are what distinguish the rule from a blanket.
        let d = escapes(
            "struct Plain { n: i64 } struct Holder { p: Plain, m: i64 } \
             fn eat(take h: Holder) {} \
             fn main() -> i64 { let h = Holder{ p: Plain{ n: 1 }, m: 2 } eat(h) return h.m }",
        );
        assert!(d.is_empty(), "nothing here owns a resource: {:?}", d);
    }

    #[test]
    fn indirection_is_not_ownership() {
        // The walk stops at pointers, exactly as cgen's `needs_drop` does: a field
        // that POINTS AT a resource does not own it, and following indirection is
        // also what would let a by-value walk fail to terminate. A `Drop` impl that
        // frees through the pointer is that impl's business, not the wrapper's.
        let d = escapes(
            "@move struct Handle { fd: i64 } struct ByPtr { h: *mut Handle, n: i64 } \
             fn eat(take b: ByPtr) {} \
             fn main() -> i64 { var x = Handle{ fd: 3 } let b = ByPtr{ h: &x, n: 1 } eat(b) return b.n }",
        );
        assert!(d.is_empty(), "a pointer field is not ownership: {:?}", d);
    }

    #[test]
    fn a_copy_wrapper_around_a_resource_is_pinned_residue() {
        // PINNED RESIDUE, and cited from `owns_resource_at`. An aggregate the author
        // declared `@copy` is left alone by the walk, exactly as `needs_drop` leaves
        // it alone — so `@copy` remains an escape hatch out of the containment rule.
        //
        // This is NOT an endorsement. A `@copy` struct holding a `@move` field is a
        // contradiction and the right answer is to reject the DECLARATION, which is
        // its own increment and a new diagnostic (so a port mirror). Keeping the walk
        // in step with `needs_drop` matters more than closing it from in here: the
        // two drifting apart is what produced the use-after-drop above. No corpus
        // type is shaped this way: all 31 `@copy` declarations were swept and none
        // holds a field of any of the eight `@move` handle types. (The droppable
        // half of the same contradiction is not swept here — `@copy` already
        // suppressed `needs_drop` before this walk existed, so that behaviour is
        // unchanged rather than newly permitted.)
        let d = escapes(
            "@move struct Handle { fd: i64 } @copy struct Sneaky { h: Handle } \
             fn eat(take s: Sneaky) {} \
             fn main() -> i64 { let s = Sneaky{ h: Handle{ fd: 3 } } eat(s) return 0 }",
        );
        assert!(d.is_empty(), "@copy still wins over containment (residue): {:?}", d);
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
    fn rejects_spawn_with_mut_struct_param() {
        // **The hole the slice restriction left.** `jestyrc check` reported every check
        // passing for this program; the only thing that refused it was gcc, because the
        // spawn thunk stores arguments by value while the callee wants a `Box*`. The race
        // is the part that matters — fixing the thunk would have removed the refusal
        // without removing the data race.
        let d = escapes(
            "struct Box { n: i64 } fn bump(mut b: Box) { b.n = b.n + 1 } \
             fn main() -> i32 { var b: Box = Box{ n: 0 } concurrent { spawn bump(b) } return 0 }",
        );
        assert_eq!(d.len(), 1, "{:?}", d);
        assert!(
            d[0].message.contains("takes `b` by `mut`")
                && d[0].message.contains("data race"),
            "{:?}",
            d
        );
    }

    #[test]
    fn accepts_spawn_with_read_struct_param() {
        // The control. Byte-identical but for the conv, and it must stay legal — a rule
        // that rejected every struct crossing `spawn` would pass the test above while
        // making the feature useless.
        let d = escapes(
            "struct Box { n: i64 } fn peek(read b: Box) -> i64 { return b.n } \
             fn main() -> i32 { var b: Box = Box{ n: 0 } concurrent { spawn peek(b) } return 0 }",
        );
        assert!(d.is_empty(), "a `read` struct across `spawn` is fine: {:?}", d);
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

    // --- impl method bodies are escape-checked at all ---

    /// **An `impl` method body reaches the escape checker.**
    ///
    /// `check_item` skipped `Item::Impl` on a comment claiming the bodies were
    /// "escape-checked once their resolution lands (Stage B)". Stage B is
    /// `typeck::register_impls`, which reads signatures only — so no impl body was
    /// ever escape-checked, `impl Drop` included, which is precisely where raw
    /// pointers are freed.
    ///
    /// Closing it changed **nothing** across all 208 corpus files, which is also what
    /// a vacuous change looks like — hence this probe. Each case is a PAIR: the
    /// escaping body must be refused with the message its free-fn twin gets, and the
    /// well-behaved body must stay clean, so the refusal cannot be passing because
    /// the file is rejected for some other reason.
    #[test]
    fn an_impl_method_body_is_escape_checked() {
        let base = "struct Node { value: i32 } struct Holder { inner: Node } \
                    trait Stash { fn keep(read self, read p: Node) -> Holder } ";
        // Storing a second-class borrow in a struct — the free-fn refusal of
        // `rejects_capturing_a_borrow_in_a_struct`, now reached inside an impl.
        let d = escapes(&format!(
            "{base}impl Stash for Node {{ fn keep(read self, read p: Node) -> Holder {{ \
               return Holder{{ inner: p }} }} }}"
        ));
        assert_eq!(d.len(), 1, "{d:?}");
        assert!(
            d[0].message.contains("cannot store borrow `p` in struct `Holder`"),
            "the impl body gets the same refusal as the free fn: {d:?}"
        );
        // The control: an impl body that stores an OWNED value is fine, so the arm
        // is not simply rejecting every impl.
        let d = escapes(&format!(
            "{base}impl Stash for Node {{ fn keep(read self, take p: Node) -> Holder {{ \
               return Holder{{ inner: p }} }} }}"
        ));
        assert!(d.is_empty(), "an owned value may be stored: {d:?}");
        // And the `impl Drop` shape this matters most for: a use-after-consume in a
        // drop body, which until now nothing looked at.
        let dr = "struct Buf { n: i32 } fn consume(take b: Buf) {} \
                  trait Drop { fn drop(mut self) } ";
        let d = escapes(&format!(
            "{dr}impl Drop for Buf {{ fn drop(mut self) {{ var b: Buf = Buf{{ n: 1 }} \
               consume(b) consume(b) }} }}"
        ));
        assert!(
            d.iter().any(|m| m.message.contains("cannot use `b` after it was given to a `take`")),
            "a use-after-consume inside `drop` must be caught: {d:?}"
        );
        let d = escapes(&format!(
            "{dr}impl Drop for Buf {{ fn drop(mut self) {{ var b: Buf = Buf{{ n: 1 }} \
               consume(b) }} }}"
        ));
        assert!(d.is_empty(), "consuming once is fine: {d:?}");
    }
}
