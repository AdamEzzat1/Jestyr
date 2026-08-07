> **Internal development log** — kept for provenance. Status lines and counts in this file reflect the moment it was written, not the current state. Start at the repo [README](../../README.md) for current status and verified claims.

# Parallelism (Workstream Q) — Handoff (run in a parallel session, commit to master)

> Self-contained cold-start for the **Parallelism** workstream — *data parallelism*
> (making one computation faster across cores / SIMD lanes / eventually GPU), as
> distinct from the **Concurrency** workstream (N, structuring independent tasks).
> The two meet at exactly one bridge: the deterministic `par` reduction, whose
> machinery (`core.par_binned_sum`) this stream **shares with N** — see the
> Parallel-safety contract for the coordination rule. Branch off **current `master`**.
> Everything lands on `master`, one green increment per commit.
> Companion reading: `CONCURRENCY-HANDOFF.md` (the shared `par_binned_sum` template +
> the determinism lesson), `NUMERICS-HANDOFF.md` (the FP contract that makes
> bit-identity real), `ROADMAP.md` §3-N / §5 (the Motley cost-model thesis),
> `jestyr-design.md` §10 (concurrency model) + §14 (stdlib philosophy). Conflict tier:
> **MEDIUM** (additive AST nodes + new `std` modules; shares only `core.jtr`'s
> `par_*` region with N — coordinate that one file).

---

## Copy-paste opener (paste into a fresh session)

```
Read PARALLELISM-HANDOFF.md in the Jestyr repo, then continue Workstream Q
(data parallelism). Start at increment 1 (the `par_reduce` library generalizing
`core.par_binned_sum`) unless a later increment is already on master. The headline
is schedule-independent deterministic data parallelism — a parallel computation
whose result is BIT-IDENTICAL across thread counts, chunk sizes, and (later) SIMD
lane widths, ENFORCED by the compiler, reusing par_binned_sum + the locked FP
contract + the SHA canary. Keep every change additive; do NOT touch src/module.rs
or src/main.rs; coordinate the core.jtr par_* region with the Concurrency (N)
session. One green, warning-clean increment per commit; ff-merge to master each time.
```

---

## Mission

Build Jestyr's **data-parallelism** story — the HPC tradition (make *one* computation
faster), not task structuring. Three tiers, cheapest first:

1. **Deterministic data-parallel SOACs** — `map` / `reduce` / `scan` over a slice, where
   any *reduction* must be a **declared, order-independent** operator (the binned
   superaccumulator; integer sum/min/max/xor). The compiler **refuses** a non-deterministic
   reduction (naive float `+`) at compile time. Built on the existing `spawn` machinery —
   **zero compiler change** for the library tier.
2. **The headline — schedule-independent parallelism as a *checked* guarantee.** Separate
   the *algorithm* (what to compute) from the *schedule* (thread count, chunk size, tiling,
   later vector width), and **guarantee the output is bit-identical for every legal
   schedule** — a compile-time promise, testable by the SHA canary. Halide proved this
   inside an image DSL; Jestyr can make it a *general, checked* property because it already
   has the FP-determinism contract + bit-reproducible reductions.
3. **A static work-span cost model (the Motley tie-in)** — lift Cilk/NESL's **work `W`** and
   **span/depth `D`** into the contracts culture: the compiler computes and checks a parallel
   loop's asymptotic work and span (`@span(log n)` as a machine-verified part of the
   interface), so a refactor that accidentally serializes a reduction is a *diagnostic*, not
   a silent regression. Then extend with CJC's **thermal/energy** estimates.

Research-/far-tier (sequence after the deterministic core lands): **deterministic SIMD**
(`uniform`/`varying` + lane reductions bit-identical across vector widths) and **GPU SOACs**
(Futhark-style). Big backend lifts — design now, build later.

---

## Where Jestyr is today (confirmed in-tree — build on this, don't reinvent)

- **The deterministic parallel reduction is already proven — this is the seed for the whole
  workstream.** `core.par_binned_sum` ([core.jtr:781](examples/std/core.jtr:781); helper
  `par_worker` [:768](examples/std/core.jtr:768); `PAR_WORKERS = 4` [:764](examples/std/core.jtr:764);
  serial reference `f64_binned_sum` [:742](examples/std/core.jtr:742)) splits a slice across
  workers, each binning its chunk into a **disjoint** region of one buffer
  (`raw + 0/2048/4096/6144`, [:790-793](examples/std/core.jtr:790)), merges by integer add,
  and finalizes once — **bit-identical to serial for any split**. Demo
  [`examples/std/par_reduce.jtr`](examples/std/par_reduce.jtr). **This is the template for
  every tier of this workstream.**
- **`spawn` exists** but is constrained: it works only as a *literal statement* in a
  `concurrent { … }` block (no spawn-in-a-loop → no dynamic worker count), targets a *direct
  named call*, and **has no result**. `par_binned_sum` works *within* these limits by pinning
  `PAR_WORKERS = 4` and writing through a raw `*mut i64`. Lifting "dynamic-N spawn" (a worker
  count chosen at runtime) is shared with Concurrency (N) and is the gate to a *tunable*
  schedule. Lowering: `cgen::emit_concurrent` (`src/cgen.rs`, search the symbol),
  `cgen::spawn_runtime`; AST node `ExprKind::Spawn(ExprId)` (`src/ast.rs`).
- **Atomics exist.** Seq-cst ops on an `int64` cell via GCC `__atomic_*` builtins
  (`atomic_store`/`atomic_load`/`atomic_add`/`atomic_xchg`, `src/cgen.rs`). The merge step's
  substrate if a tier ever needs a shared accumulator instead of disjoint regions (prefer
  disjoint regions — they're race-free *and* deterministic).
- **The data-race rule is enforced.** `escape::check_spawn_no_shared_mut_slice`
  (`src/escape.rs`) rejects spawning a function that takes a `mut`/`out` **slice** (the one
  safe-subset race — a shared slice's `ptr` aliases). Disjoint-region writes go through a raw
  `*mut T` in `unsafe`, each worker a *disjoint region* — exactly the `par_binned_sum` shape.
  The safe subset is race-free today.
- **Determinism is the thesis, and it's *testable*.** Locked
  `CC_FLAGS = -ffp-contract=off -fno-fast-math` (`src/main.rs`, search `CC_FLAGS` +
  `mod fp_contract_tests`) forbids FMA-reassociation so per-element float math doesn't drift;
  the cross-OS SHA canary (`examples/std/numerics_canary.jtr`) makes "bit-identical across
  schedules" a *pinned* output, not an aspiration. The determinism property to mirror is
  `binned_sum_is_chunk_independent` (`src/proptests.rs`).
- **Reality check on the test harness** (from the trait epic's hard-won lesson, see
  `HANDOFF-NEXT.md`): the real suite is `src/proptests.rs` (`mod prop` + `mod fuzz` +
  `arb_*_program` generators). A `--features c-oracle` gate **does** exist for gcc/thread
  round-trips (used by the Mutex increment — see `ROADMAP.md` §3-N); confirm the exact module
  name in-tree before assuming its shape. Keep default `cargo test` toolchain-free.

---

## Parallel-safety contract (read before touching anything)

This stream adds new AST nodes (the `par` loop surface, schedule annotations, later the cost
attributes) and threads them through the six files in order: `ast.rs → parser.rs → typeck.rs
→ escape.rs → cgen.rs → printer.rs`, plus new intrinsics and **new `std` modules**. Keep
every change **additive** — a new `ExprKind`/`TypeKind` variant, a new `infer`/escape/emit
arm, a new `emit_*` helper — never a rewrite of a shared line.

- **Must NOT edit `src/module.rs`** (modules-v2 / K owns it) or **`src/main.rs`** (Tooling /
  O owns subcommand dispatch). Read-only reuse of `CC_FLAGS` is fine.
- **Shared file with Concurrency (N): `examples/std/core.jtr`'s `par_*` region**
  ([:757-800](examples/std/core.jtr:757)). N may also generalize `par_binned_sum`. **Coordinate**:
  prefer landing the generalized `par_reduce` in a **new module** (`std/parallel.jtr`) that
  *calls into* `core`'s existing primitive rather than rewriting it in place — so the two
  streams append, never collide. If you must edit the `core.jtr` `par_*` lines, ping the N
  session first (it's the one MEDIUM-risk seam).
- When you add an `ExprKind`, Rust's exhaustive `match` flags every walker you must extend
  (cgen has five recursive walkers: `find_calls_expr`, `collect_structs_in_expr`,
  `find_closures_expr`, `find_spawns_expr`, `collect_refs`) — follow the errors; nothing
  merges silently wrong.
- Worktree flow: commit on this branch, then ff-merge master
  (`git -C C:\Users\adame\Jestyr merge --ff-only <branch>`). `cargo build` after each merge to
  clear exhaustiveness, then `cargo test`.

---

## Inspiration — the ONE thing to take from each (surgical, not wholesale)

| Language / system | The single idea to take | Fit / what to reject |
|---|---|---|
| **Halide** | **Separate the *algorithm* from the *schedule*** — write *what* to compute once; describe *how* to parallelize/tile/vectorize separately, and the result is identical for every schedule. | **The star.** Determinism-as-a-design-principle. Halide proved it for an image DSL; Jestyr makes it a *general, checked* guarantee. Reject the DSL confinement. |
| **Chapel** | **Domains + `forall`** — a first-class iteration space decoupled from how it maps to processors; **reduction intents** (`forall … with (+ reduce x)`). | The cleanest data-parallel-loop surface. The compiler is *told* the reduction and emits a deterministic tree — `par_binned_sum` generalized. The best inspiration for the headline. |
| **Cilk** | **`spawn`/`sync` fork-join + a provably-efficient work-stealing scheduler**, and the **work-span cost model** (work `W`, span/depth `D`). | Take the *cost model* especially — it's a checkable property (tier 3). Reject the runtime work-stealer for now (Jestyr is OS-thread + structured scope). |
| **NESL / Blelloch** | **Nested data parallelism + a static work-span cost model**; flatten nested parallelism. | The theory behind "parallelism with a provable cost." Deeply aligned with the Motley cost-model thesis. |
| **Futhark** | **`map`/`reduce`/`scan`/`filter` (SOACs) that *require associative operators*** and exploit that for safe parallelism + good cost. | Validates "associativity/determinism as a *requirement*." Extend Jestyr's deterministic reduce to `scan`/prefix-sum. The GPU backend is far-tier. |
| **ISPC** | **`uniform`/`varying` type qualifiers** — scalar-looking code, compiler vectorizes across SIMD lanes (SPMD-on-SIMD). | Types that carry *execution* semantics — exactly Jestyr's `read`/`mut`/`out` philosophy applied to SIMD lanes. The best SIMD model to copy. Needs real backend work — research-tier. |
| **Mojo** | **SIMD as a first-class type** in a systems language; explicit `vectorize`/`parallelize`. | Modern proof a systems language can make SIMD ergonomic + typed. Reference, not a dependency. |
| **Rayon / TBB** | **Work-stealing `par_iter` / `parallel_reduce` ergonomics**. | Pragmatic API shape. **Reject their FP-nondeterminism** — that schedule-dependent-result gap is exactly what Jestyr fills. |

The thread: **algorithm/schedule separation** (Halide) + **iteration domains + reduction
intents** (Chapel) + **associativity-required SOACs** (Futhark) + **uniform/varying SIMD**
(ISPC) + **a work-span cost model** (Cilk/NESL).

---

## The unique features — what only Jestyr can do

Jestyr holds the two pieces nobody else combines: a **codegen-locked FP-determinism
contract** and **bit-reproducible reductions** (`par_binned_sum`). That makes three
genuinely-novel-for-a-systems-language features possible.

### 1. Schedule-independent parallelism as a *checked* guarantee — the headline

Write the algorithm once, then annotate a *schedule* (thread count, tiling, vectorization,
chunk size) separately. The compiler **guarantees the output is bit-identical for every legal
schedule** — so you can tune for speed (or for a different machine's core/lane count) and the
answer *cannot* change.

```
// algorithm — written once
let total: f64 = par for x in xs reduce(binned +) { x }

// schedule — tunable, separately; the result is bit-identical across all of these
with schedule(threads = 8, chunk = 4096)
with schedule(threads = 2, chunk = 1024)
```

Everyone else gives you "probably the same" because IEEE-754 reassociation breaks under
reschedule. Jestyr's FP contract + binned reductions make it a *hard* guarantee, testable by
the SHA canary. The compiler **refuses, at compile time, a non-deterministic reduction**
(naive float `+`) with a clear diagnostic. The pitch: **"separate how-fast from
what-it-computes, and make the separation a compile-time promise."** No general-purpose
language offers this; Halide does it only inside an image-processing DSL.

### 2. Deterministic SIMD (`uniform`/`varying` + bit-reproducible lane reductions)

Borrow ISPC's `uniform`/`varying` (a natural extension of Jestyr's execution-carrying
`read`/`mut`/`out` types), but add the thing ISPC and everyone else *lack*: **lane reductions
that are bit-identical regardless of vector width** — an 8-wide vs 16-wide AVX float sum gives
the same answer, because the reduction goes through the binned accumulator, not naive
horizontal adds. "Portable SIMD whose float results don't depend on the ISA" is unclaimed
territory. **Research-tier** — needs real backend work (C intrinsics or an LLVM path);
sequence after the deterministic data-parallel core.

### 3. A static work-span (→ energy/thermal) cost model — the Motley tie-in

Lift Cilk/NESL's **work `W`** and **span/depth `D`** into the contracts culture: the compiler
*computes and checks* a parallel loop's asymptotic work and span, surfaced like
`requires`/`ensures`. `@span(log n)` becomes a machine-verified part of the interface, and a
refactor that accidentally serializes a reduction (span goes `log n → n`) is a *diagnostic*,
not a silent regression. Then extend the cost model with CJC's **thermal/energy** estimates
(the Motley head start, see `MOTLEY.md`) so parallel code carries a checked *energy* cost, not
just a time cost. A systems language where parallel cost *and energy* are part of the proven
contract is something no one has built — and it is squarely the Motley thesis.

---

## Recommended increment order (cheapest / lowest-overlap first)

1. **`par_reduce` library (no syntax, no compiler change)** — generalize `par_binned_sum` into
   a `par_reduce` over a declared `Reduction` value (an identity + an order-independent
   `combine` + a `finalize`), with binned-sum and integer sum/min/max/xor as built-ins. Land
   it in a **new `std/parallel.jtr`** that calls `core`'s existing primitive (avoids the N
   collision). Pure Jestyr + existing `spawn`. The determinism property is provable
   immediately (reuse `binned_sum_is_chunk_independent`). **Ships on emitted C, no backend
   work.**
2. **`par_map` / `par_scan` (still library)** — `map` is embarrassingly parallel (disjoint
   output regions, no merge); `scan`/prefix-sum needs the associative-operator requirement
   (Futhark's lesson) and a two-pass (local-scan → exclusive-prefix → finalize) shape.
   Determinism property per SOAC.
3. **The `par for … reduce(r)` surface + non-deterministic-reduction rejection** (headline,
   tier-2) — a new `ExprKind::ParFor` (additive) that desugars to the library call; typeck
   records the reduction; **escape/typeck reject a non-deterministic reduction** (the checked
   guarantee). This is where the new AST node threads the six files.
4. **The schedule split — `with schedule(threads = …, chunk = …)`** — a schedule annotation
   parsed onto the `par for`, lowered to the worker partition, with the **bit-identical
   guarantee** across schedules added to the SHA canary's demo set. Needs **dynamic-N spawn**
   (spawn-in-a-loop with a handle array) so worker count isn't pinned at 4 — coordinate with N
   (it's the shared `emit_concurrent` change).
5. **The work-span cost model** (tier-3, Motley) — compute `W`/`D` for the structured
   `par`/`forall` subset first; surface `@span(...)` as a checked attribute; diagnose a span
   regression. Then layer CJC's thermal/energy estimate.
6. **Research / far:** typed **SIMD** (`uniform`/`varying` + bit-identical lane reductions —
   real backend work), then **GPU SOACs** (Futhark-style). Big lifts; sequence last.
   **Increment 6 has now STARTED — do not restart it.** `src/simd.rs` + the `@simd`
   attribute + `jestyrc simd <file>` (increment **Q-S1**, on master) decide which
   `par for` bodies may be evaluated a lane at a time, and prove the verdict sound
   against GCC vector extensions at widths 2/4/8 (`simd_lanes_match_scalar_bit_for_bit`,
   `--features c-oracle`). It changes **no emitted C** — `@simd` is a *checked contract*
   in `@span`'s shape, not a lowering switch — so the lowering (Q-S2), the CJC
   thermal/energy facet (Q-S3) and the GPU contract (Q-S4) are what remain. The full
   plan, the seven findings, and the non-negotiables live in
   **`docs/session-notes/jestyr-next-frontier-handoff.md` § "Q. SIMD → GPU"**; read
   that before touching this workstream.

---

## What NOT to build (and why) — save the session

- **Pragma-style bolt-on parallelism** (a `#pragma`-equivalent slapped on an ordinary loop).
  It makes data races trivial and results schedule-dependent — the exact failure Jestyr
  exists to prevent. Parallel-safety + determinism must be **intrinsic** to the construct
  (the `par for` *requires* a declared reduction), never an annotation on unchecked code.
- **A work-stealing runtime scheduler (Cilk/TBB-style).** A large hidden runtime that fights
  the "no ambient machinery / deterministic" thesis. Take Cilk's *cost model*, not its
  scheduler — Jestyr stays OS-thread + structured scope. Tunable-but-static schedules give
  the win without the runtime.
- **Non-deterministic "fast" reductions as an escape hatch.** Tempting for raw speed, but it
  reintroduces the schedule-dependent-answer problem the headline eliminates. If ever needed,
  it must be a *loudly-marked* `unsafe`/`@nondeterministic` opt-out, never the default — and
  excluded from the SHA canary.
- **GPU / SIMD before the deterministic data-parallel core lands.** Both need real backend
  work and both are far more valuable *with* the determinism guarantee already proven on CPU.
  Design now, build after tiers 1–4.

---

## Rigor — the test layers every increment ships (mirror the existing harness)

1. **Unit tests** — the lowering/typeck in isolation (cgen emits the right partition +
   disjoint-region writes; typeck types `par for` and records the reduction; escape
   accepts/rejects the right shapes).
2. **Wiring tests ("plumbed-in")** — a gcc round-trip *example* that actually runs threads,
   like [`examples/std/par_reduce.jtr`](examples/std/par_reduce.jtr), with a
   `*_example_compiles_clean` test.
3. **Property tests** (`src/proptests.rs` `mod prop` + `arb_*_program`) — the on-thesis stars:
   **determinism** (a `par_reduce`/`par for` result is bit-identical to serial across
   arbitrary splits, worker counts, *and chunk sizes* — reuse the
   `binned_sum_is_chunk_independent` pattern); **soundness of the rejection** (a
   non-deterministic reduction *always* errors at compile; a deterministic one never does);
   for tier 3, **cost-model soundness** (computed `W`/`D` match the emitted structure).
4. **Bolero fuzz** (`src/proptests.rs` `mod fuzz`) — over arbitrary small parallel programs:
   lowering never panics, the escape checker never accepts a shared-mut-slice spawn, the SOAC
   codegen is always well-formed.
5. **`--features c-oracle`** — add the headline `par for` / schedule-split demo to the
   cross-OS SHA canary's demo set so its bit-identical output is **pinned across OS, compiler,
   thread count, and chunk size** (the determinism guarantee, productized). Confirm the exact
   c-oracle module name in-tree before wiring (the Mutex increment uses it — see `ROADMAP.md`
   §3-N).
6. **Teeth-verify each property by mutation** — e.g. make a worker write to a shared
   (non-disjoint) region and watch the determinism property fail; relax the
   non-deterministic-reduction check and watch the rejection test fail; corrupt the computed
   span and watch the cost-model test fail; revert each.

Every increment stays **`cargo test`-green and warning-clean**; default `cargo test` stays
toolchain-free (gate gcc/thread round-trips behind `c-oracle`). Keep all examples
byte-identical (the repo invariant).

---

## Documentation deliverable (Downloads)

When a slice lands (especially the schedule-split headline), write a session summary to
**`C:\Users\adame\Downloads\jestyr-parallelism.md`** — the SOAC APIs, the
algorithm/schedule-separation design, the bit-identical guarantee + how it's checked and
tested across thread/chunk/lane variation, the cost-model design, and the explicitly-deferred
items (work-stealing runtime, SIMD, GPU) with reasons. Add a **Workstream Q** row to
`ROADMAP.md` §2's table and a §3-Q detail section (mirror §3-N's format).

---

## Commit-to-master discipline (every increment)

- **One green increment per commit.** `git commit -F <msgfile>` (multi-line).
- After green + warning-clean, **fast-forward master**:
  `git -C C:\Users\adame\Jestyr merge --ff-only <this-branch>`. Don't push unless asked.
- Teeth-verify before committing. Keep examples byte-identical.

---

## Pointers (verify line numbers; they drift — search the symbol)

| Thing | Where |
|---|---|
| **Deterministic-reduction template (the seed)** | `examples/std/core.jtr` → `par_binned_sum` ([:781](examples/std/core.jtr:781)), `par_worker` ([:768](examples/std/core.jtr:768)), `PAR_WORKERS` ([:764](examples/std/core.jtr:764)), serial `f64_binned_sum` ([:742](examples/std/core.jtr:742)) |
| Disjoint-region write pattern (copy this for `map`/`scan`) | `examples/std/core.jtr` → [:790-793](examples/std/core.jtr:790) (`raw + 0/2048/4096/6144`) |
| Existing parallel demo to mirror | [`examples/std/par_reduce.jtr`](examples/std/par_reduce.jtr) |
| Spawn lowering (extend for dynamic-N — coordinate with N) | `src/cgen.rs` → `emit_concurrent`, `spawn_runtime` |
| Spawn AST node | `src/ast.rs` → `ExprKind::Spawn(ExprId)` |
| Atomics intrinsics (merge substrate if ever needed) | `src/cgen.rs` → `atomic_store`/`atomic_load`/`atomic_add`/`atomic_xchg` |
| Data-race rule (the disjoint-region invariant) | `src/escape.rs` → `check_spawn_no_shared_mut_slice`, the `ExprKind::Spawn` arm |
| Determinism property to mirror | `src/proptests.rs` → `binned_sum_is_chunk_independent` |
| Cross-OS canary (add the schedule-split demo) | `src/proptests.rs` → the c-oracle module; `examples/std/numerics_canary.jtr` |
| FP-determinism contract (what makes bit-identity real) | `src/main.rs` → `CC_FLAGS`, `mod fp_contract_tests` |
| The five cgen walkers to extend per new ExprKind | `src/cgen.rs` → `find_calls_expr`, `collect_structs_in_expr`, `find_closures_expr`, `find_spawns_expr`, `collect_refs` |
| Test-layer conventions | `docs/TESTING.md`; `src/proptests.rs` (`mod prop`/`mod fuzz`/`arb_*`) |
| Shared-machinery sibling (the bridge) | `CONCURRENCY-HANDOFF.md` (the `par` reduction + `par_binned_sum`) |
| Cost-model / energy thesis (tier 3) | `MOTLEY.md`; `ROADMAP.md` §5 (Motley critical path) |
| Design intent | `jestyr-design.md` §10 (concurrency), §14 (stdlib); a new **§20 (data-parallelism model)** to author |

## One-line summary

Build deterministic data-parallel SOACs — `par_reduce` → `par_map`/`par_scan` →
`par for … reduce(r)` with **non-deterministic-reduction rejection** → the **schedule split**
(algorithm separated from thread/chunk schedule) → a **work-span (+energy) cost model** → far
tier SIMD/GPU. The headline is **schedule-independent deterministic data parallelism**: a
result that is **bit-identical across thread counts, chunk sizes, and lane widths, enforced by
the compiler** — reusing `par_binned_sum` + the locked FP contract + the SHA canary, a
guarantee no other general systems language can make, and the Motley cost-model thesis made
concrete. Additive AST only; don't touch `module.rs`/`main.rs`; coordinate the `core.jtr`
`par_*` region with the Concurrency (N) session. Full test rigor (determinism property is the
star), docs to Downloads, one green increment per commit to master.
