> **Internal development log** — kept for provenance. Status lines and counts in this file reflect the moment it was written, not the current state. Start at the repo [README](../../README.md) for current status and verified claims.

# Concurrency (Workstream N) — Handoff (run in a parallel session, commit to master)

> Self-contained cold-start for the **Concurrency** workstream (design §10). Branch off
> **current `master`** — it already carries K's per-module namespaces, so your new code
> inherits the right name-resolution API. Safe to run alongside the still-running
> modules-v2 session (its only remaining work, module content-hashing, is `module.rs`-only
> — zero overlap). Everything lands on `master`, one green increment per commit.
> Companion reading: `ROADMAP.md` §3-N, `jestyr-design.md` §10, `NUMERICS-HANDOFF.md`
> (the deterministic-reduction lesson), and the code anchors below. Conflict tier:
> **MEDIUM** (additive AST nodes; the dangerous K rekey already landed).

---

## Mission

Finish Jestyr's concurrency story. Two tiers:
1. **Bread-and-butter sync primitives** — a Mutex (as an Ada-style *protected object*),
   move-only **channels**, **task results + `await`**. Cheap on the existing atomics +
   pthread machinery.
2. **The headline / unique feature** — a **deterministic `par` reduction loop**: parallel
   computation whose result is **bit-identical to serial regardless of thread count or
   schedule**, *enforced by the compiler*. No other systems language can make this a
   *checked* guarantee; Jestyr already has the mechanism (`par_binned_sum`) and the
   FP-determinism contract that make it real.

---

## Where Jestyr is today (confirmed in-tree — build on this, don't reinvent)

- **Structured concurrency exists.** `concurrent { spawn f(args) }` → pthread-per-spawn +
  join-at-`}`, args copied into a per-site struct. Lowering: `cgen::emit_concurrent`
  (`src/cgen.rs:5723`) + `cgen::spawn_runtime` (`src/cgen.rs:5685`). AST node
  `ExprKind::Spawn(ExprId)` (`src/ast.rs:256`). **Limits:** `spawn` works only as a
  *literal statement* in the block (no spawn-in-a-loop → no dynamic N), targets a *direct
  named call*, and has **no result**. Lifting these three is exactly tiers 1–2.
- **Atomics exist.** Seq-cst ops on an `int64` cell via GCC `__atomic_*` builtins —
  `atomic_store`/`atomic_load`/`atomic_add`/`atomic_sub` intrinsics at `src/cgen.rs:4037+`.
  Demo `examples/atomics.jtr`. This is the substrate for Mutex + channel internals.
- **The deterministic parallel reduction is already proven.** `core.par_binned_sum`
  (`examples/std/core.jtr:781`, helper `par_worker:768`, `PAR_WORKERS=4` at `:764`) splits
  a slice across workers, each binning its chunk into a **disjoint** region of one buffer
  (`raw + 0/2048/4096/6144`, `core.jtr:790-793`), then merges by integer add and finalizes
  once — **bit-identical to serial `f64_binned_sum`** for any split. Demo
  `examples/std/par_reduce.jtr`. **This is the template for the headline feature.**
- **The data-race rule is enforced.** `escape::check_spawn_no_shared_mut_slice`
  (`src/escape.rs:610`, dispatched from the `ExprKind::Spawn` arm at `:410`) rejects
  spawning a function that takes a `mut`/`out` **slice** (the one safe-subset race — a
  shared slice's `ptr` aliases). Shared mutable state across tasks must go through a raw
  `*mut T` in `unsafe`, each task a *disjoint region* (as `par_binned_sum` does). The safe
  subset is therefore race-free today.
- **Determinism is the thesis.** Locked `CC_FLAGS = -ffp-contract=off -fno-fast-math`
  (`src/main.rs:510`) + the cross-OS SHA canary (`examples/std/numerics_canary.jtr`) — so a
  "bit-identical across schedules" claim is *testable*, not aspirational.

---

## Parallel-safety contract (read before touching anything)

This session adds new AST nodes (await, task results, sync types, the `par` loop) and
threads them through the six files: `ast.rs → parser.rs → typeck.rs → escape.rs →
cgen.rs → printer.rs`, plus new intrinsics. Keep every change **additive** — a new
`ExprKind`/`TypeKind` variant, a new `infer`/escape/emit arm, a new `emit_*` helper —
never a rewrite of a shared line.

This session **must NOT** edit `src/module.rs` (the modules-v2 session owns it for content
hashing) or `src/main.rs` (the Tooling session owns subcommand dispatch). N's surface and
those two are disjoint, so the streams share zero lines. When you add an `ExprKind`, Rust's
exhaustive `match` will flag every walker you must extend (cgen has five recursive walkers:
`find_calls_expr`, `collect_structs_in_expr`, `find_closures_expr`, `find_spawns_expr`,
`collect_refs`) — follow the errors; nothing merges silently wrong.

Worktree flow: commit on this branch, then ff-merge master
(`git -C C:\Users\adame\Jestyr merge --ff-only <branch>`). `cargo build` after each merge
to clear exhaustiveness, then `cargo test`.

---

## Inspiration — the ONE thing to take from each (surgical, not wholesale)

| Language | The single idea to take | What to reject / note |
|---|---|---|
| **Crystal** | Typed `Channel(T)` + `select` ergonomics; "concurrency should be pleasant" | Reject the fiber/green-thread scheduler (a big hidden runtime) and its *lack of* compile-time race safety. Take the surface, not the engine. |
| **Go** | `select` + "share memory by communicating" | Reject unstructured goroutines (outlive their spawner) — Jestyr already fixed this with scoped `concurrent{}`. |
| **Erlang/BEAM** | **Share-nothing** message passing | Maps onto the escape rule: no shared mutable refs ⇒ message-passing-by-move is the natural safe primitive. Reject per-process GC heaps. |
| **Pony** | **Sendability is a reference-capability property** (`iso`/`val`/`tag`) | A uniquely-owned `take` value ≈ Pony `iso` (sendable mutable); a `mut` borrow is non-sendable — literally the spawn rule. Formalize "sendable = `take`/owned". |
| **Rust** | Compile-time race-freedom + **scoped threads** + move-on-send | Validates the direction (`std::thread::scope` = Jestyr's `concurrent{}`). Take "channel `send` *moves* (`take`) the value". Reject `Send`/`Sync` auto-traits — Jestyr uses second-class refs instead. |
| **Chapel (HPC)** | **Reduction intents** (`forall … with (+ reduce x)`) | The compiler is *told* the reduction and emits a deterministic tree — `par_binned_sum` generalized. The best inspiration for the headline feature. |
| **Ada/SPARK/Ravenscar** | **Protected objects** + the statically-analyzable profile | A protected object bundles data + lock + operations as one unit — a *better* Mutex (can't forget to lock). The Ravenscar "provably race/deadlock-free subset" feeds `@verified`. |
| **Concurrent ML** | Synchronization as **first-class composable values** (events) | `select` as a combinator (`choose`/`wrap`). Elegant, very Jestyr — but advanced; file under "later". |

The thread: **CSP surface** from Crystal/Go, **sendability/share-nothing** from
Erlang/Pony/Rust, **reduction intents** from Chapel, **protected objects + a provable
subset** from Ada.

---

## The unique feature — deterministic `par` reduction loop (the headline)

**What it is.** A parallel loop whose result is **bit-identical to the serial result for
any thread count or schedule**, *checked by the compiler*:

```
let total: f64 = par for x in xs reduce(binned +) { x }    // == serial, bit-for-bit, always
```

The compiler **accepts** declared *deterministic* reductions (the binned superaccumulator —
associative at the *bit* level; integer min/max/sum; xor) and **refuses, at compile time, a
non-deterministic reduction** (naive float `+`, which reassociates under parallelism) with a
clear diagnostic. The lowering reuses the `par_binned_sum` shape: partition the iterable,
give each worker a **disjoint** accumulator region, merge by the reduction's
order-independent combine, finalize once.

**Why it is uniquely a Jestyr feature.** Every other parallel runtime breaks exactly here:
Rayon, OpenMP, and even Chapel give you parallel reductions, but IEEE-754 reassociation
means the answer changes with thread count — so "deterministic" is something you *hope for*,
not something the compiler *guarantees*. Jestyr already has the three things that make it a
checked property: (a) `par_binned_sum` proving a bit-reproducible parallel reduction is
implementable (`core.jtr:781`); (b) the locked `-ffp-contract=off` FP contract
(`main.rs:510`) so the per-element math doesn't drift either; (c) the SHA canary to *test*
bit-identity across runs. The slogan — **"parallelism that cannot change your answer"** — is
literally a compile error if you try to violate it.

**Design sketch (incremental):**
1. *Library first, no syntax:* generalize `par_binned_sum` into a `par_reduce` over a
   declared `Reduction` (a value carrying an identity + an order-independent `combine` +
   a `finalize`), with the binned-sum and integer reductions as the built-ins. Pure
   `core`/`std` Jestyr + the existing `spawn` — **zero compiler change**, lands fast, and
   the determinism property is provable immediately (reuse `binned_sum_is_chunk_independent`).
2. *Then the `par for … reduce(r)` surface:* a new `ExprKind::ParFor` (additive) that
   desugars to the library call; typeck records the reduction; **escape/typeck reject a
   non-deterministic reduction** (the checked guarantee). This is where the new AST node
   threads the six files.
3. *Then dynamic-N spawn* (spawn-in-a-loop with a handle array) so worker count isn't
   pinned at 4 — needs `emit_concurrent` to handle `for { spawn … }`.

**Two more uniquely-Jestyr primitives** (lower tiers, same theses):
- **Move-only channels** — `Channel(T)` whose `send` **consumes** (`take`) the value, so no
  alias survives the send; race-freedom falls out of the *existing* escape analysis (no
  `Send`/`Sync`, no runtime detector). Combined with `concurrent{}`, channels can't leak.
- **`@deterministic` concurrency region** — a block the compiler certifies as
  schedule-independent (only deterministic reductions, disjoint writes, no observable
  ordering). The Ada/Ravenscar "provable subset" fused with the determinism thesis; the
  `@verified` tie-in.

---

## Recommended increment order (cheapest / lowest-overlap first)

1. **Mutex as a protected object** — `Mutex(T)` bundling a value + a lock, with
   `with_lock`-style access, over the existing atomics (a CAS/spin or a pthread mutex
   intrinsic). Touches `cgen.rs` (intrinsics) + a `std` module + `escape.rs`; **no new
   syntax**. Prove mutual exclusion (N threads incrementing → exactly N).
2. **Move-only channels** — `Channel(T)` with `send`(`take`)/`recv`, bounded buffer over a
   mutex+atomics. `cgen.rs` + `std` + the escape "send moves" check.
3. **`par_reduce` library** (headline, tier-1) — generalize `par_binned_sum`; built-in
   deterministic reductions; the determinism property. **No compiler change.**
4. **Task results + `await`** — a `spawn` that returns a value joined back (the reserved
   `await` keyword). New AST + the six-file thread.
5. **`par for … reduce(r)` surface + non-deterministic-reduction rejection** (headline,
   tier-2) — the checked guarantee.
6. **Dynamic-N spawn**, then optionally `select` and the `@deterministic` region.

---

## What NOT to build (and why) — save the session

- **A green-thread / fiber scheduler (Crystal/Go-style M:N).** A large hidden runtime that
  fights the "no ambient machinery / deterministic" thesis. Jestyr's concurrency is
  OS-thread + structured scope; keep it.
- **Effect-polymorphic / colorless async.** Design §10.3 flags this as *explicitly
  unsolved* ("Zig tried, retreated"). Don't open it here — `await` on a joined task result
  is fine; a full async effect system is a separate research workstream.
- **Lock-free general data structures.** Tempting, but the determinism + disjoint-region
  model already covers the parallel-reduction use case race-free; general lock-free
  structures are a deep, separate effort with weak thesis-fit right now.

---

## Rigor — the test layers every increment ships (mirror the existing harness)

1. **Unit tests** — the lowering/typeck in isolation (cgen emits the right pthread/atomic
   calls; typeck types `await`/`par for`; escape accepts/rejects the right shapes).
2. **Wiring tests ("plumbed-in")** — the subcommand-free analogue: a gcc round-trip
   *example* that actually runs threads, like `examples/std/par_reduce.jtr` /
   `examples/concurrent.jtr`, with a `*_example_compiles_clean` test in `module.rs`.
3. **Property tests** (`proptests.rs::mod prop` + `arb_*_program`) — the on-thesis stars:
   **determinism** (a `par_reduce` result is bit-identical to serial across arbitrary
   splits/worker counts — reuse the `binned_sum_is_chunk_independent` pattern); **mutual
   exclusion** (a mutex-guarded counter equals the spawn count); **soundness of the
   rejection** (a non-deterministic reduction *always* errors at compile; a deterministic
   one never does).
4. **Bolero fuzz** (`proptests.rs::mod fuzz`) — `fuzz_concurrency_pipeline` over arbitrary
   small concurrent programs: lowering never panics, the escape checker never accepts a
   shared-mut-slice spawn, `par_reduce` codegen is always well-formed.
5. **`--features c-oracle`** — add the headline `par for`/`par_reduce` demo to the cross-OS
   SHA canary's demo set so its bit-identical output is pinned across OS/compiler (the
   determinism guarantee, productized).
6. **Teeth-verify each property by mutation** — e.g. make `par_worker` write to a shared
   (non-disjoint) region and watch the determinism property fail; or relax the
   non-deterministic-reduction check and watch the rejection test fail; revert.

Every increment stays **`cargo test`-green and warning-clean**; default `cargo test` stays
toolchain-free (gate gcc/thread round-trips behind `c-oracle`). Keep all examples
byte-identical.

---

## Documentation deliverable (Downloads)

When a slice lands (especially the `par` reduction loop), write a session summary to
**`C:\Users\adame\Downloads\jestyr-concurrency.md`** — the sync-primitive APIs, the
reduction-intent design, the determinism guarantee + how it's checked and tested, and the
explicitly-deferred items (schedulers, colorless async) with reasons. Update `ROADMAP.md`
§3-N too.

---

## Commit-to-master discipline (every increment)

- **One green increment per commit.** `git commit -F <msgfile>`.
- After green + warning-clean, **fast-forward master**:
  `git -C C:\Users\adame\Jestyr merge --ff-only <this-branch>`. Don't push unless asked.
- Teeth-verify before committing. Keep examples byte-identical.

---

## Pointers (verify line numbers; they drift — search the symbol)

| Thing | Where |
|---|---|
| Concurrency lowering (extend) | `src/cgen.rs` → `emit_concurrent` (:5723), `spawn_runtime` (:5685) |
| Spawn AST node | `src/ast.rs` → `ExprKind::Spawn(ExprId)` (:256) |
| Atomics intrinsics (Mutex/channel substrate) | `src/cgen.rs` → `atomic_store`/`atomic_load`/`atomic_add` (:4037+) |
| Data-race rule (extend for "send moves") | `src/escape.rs` → `check_spawn_no_shared_mut_slice` (:610), `ExprKind::Spawn` arm (:410) |
| **Deterministic-reduction template** | `examples/std/core.jtr` → `par_binned_sum` (:781), `par_worker` (:768), `PAR_WORKERS` (:764) |
| Determinism property to mirror | `src/proptests.rs` → `binned_sum_is_chunk_independent` |
| Cross-OS canary (add the par demo) | `src/proptests.rs` → `mod c_oracle`; `examples/std/numerics_canary.jtr` |
| FP-determinism contract | `src/main.rs` → `CC_FLAGS` (:510), `mod fp_contract_tests` |
| Concurrency examples to mirror | `examples/concurrent.jtr`, `examples/atomics.jtr`, `examples/std/par_reduce.jtr` |
| The five cgen walkers to extend per new ExprKind | `src/cgen.rs` → `find_calls_expr`, `collect_structs_in_expr`, `find_closures_expr`, `find_spawns_expr`, `collect_refs` |
| Test-layer conventions | `docs/TESTING.md`; `src/proptests.rs` (`mod prop`/`mod fuzz`/`arb_*`) |
| Design intent | `jestyr-design.md` §10; `NUMERICS-HANDOFF.md` (the determinism lesson) |

## One-line summary

Build Mutex (Ada protected-object) → move-only channels → `par_reduce` → task
results/`await` → `par for … reduce(r)`. The headline is a **deterministic parallel
reduction whose result is bit-identical to serial, enforced by the compiler** — reusing
`par_binned_sum` + the FP contract + the SHA canary, a guarantee no other systems language
can make. Additive AST only; don't touch `module.rs`/`main.rs`. Full test rigor (determinism
property is the star), docs to Downloads, one green increment per commit to master.
