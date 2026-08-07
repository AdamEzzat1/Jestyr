> **Internal development log** — kept for provenance. Status lines and counts in this file reflect the moment it was written, not the current state. Start at the repo [README](../../README.md) for current status and verified claims.

# Jestyr Concurrency (Workstream N) — Future Work

The core concurrency roadmap is **complete and on master** (Mutex, move-only channels,
`par_reduce`, task results + `await`, the headline `par for … reduce(r)`, dynamic-N spawn,
`@deterministic`, `select`). This document collects everything *beyond* that — the performance
levers, the generality limits, and the smaller polish items — so it can be picked up later
without re-deriving the context. Measured numbers and the determinism verification live in the
repo's `CONCURRENCY-PERF.md`.

---

## The honest framing (why these are the right next steps)

> This is a **correctness-first, structured** concurrency model: OS threads + scoped joins,
> optimized for **provable determinism and data-race safety**, explicitly **not** raw throughput
> (the handoff rules out a work-stealing scheduler as "a large hidden runtime that fights the
> deterministic thesis"). So the headline holds — *"parallelism that cannot change your answer,"*
> and it's a compile error to violate it — while absolute speed is *"modest but predictable,"*
> bounded by the 4-worker cap and no-pool tradeoffs.
>
> The two levers that would change the speed story are both **future work, and Q owns them**: the
> `with schedule(threads, chunk)` split (configurable worker count, built directly on the
> dynamic-N spawn N landed) lifts the 4-worker cap, and a thread pool would kill the per-task
> creation cost. **Neither touches the determinism guarantee** — that's the point of the design:
> you can make it faster without making it less correct.

---

## A. Performance levers (change the speed story; never the correctness)

### A1. Lift the 4-worker cap — `with schedule(threads, chunk)`  *(Q owns)*
`par_reduce` / `par for` are pinned at `PAR_WORKERS = 4` (`examples/std/core.jtr`), so they don't
scale past 4 cores. A schedule annotation — `par for x in xs reduce(r) with schedule(threads =
N, chunk = C) { … }` — would partition into a runtime number of chunks. **Builds directly on the
dynamic-N spawn already landed** (spawn-in-a-loop over a growable handle array). The determinism
guarantee is unaffected: the reduction's combine is order-independent, so any chunking is still
bit-identical to serial. This is the single highest-value next step. (Workstream Q;
`PARALLELISM-HANDOFF.md`.)

### A2. A thread pool (kill per-task thread-creation cost)
Today `concurrent` / dynamic-N `spawn` create one raw OS thread per task (~110–140 µs each, no
reuse), so fine-grained parallelism is thread-creation-bound (10 000 tasks ≈ 1.3 s of pure
overhead). A small fixed-size worker pool fed by a work queue would amortize this. **Must stay
within the thesis** — a *static* pool (size chosen up front, no work-stealing), not a Cilk/TBB
M:N scheduler, which the handoff explicitly rejects. Determinism is unaffected (the pool only
changes *where* work runs, not the order-independent combine).

### A3. Parallelize `par for`'s map
`par for` currently runs the per-element **map serially** and only the reduction *fold* in
parallel (`cgen::emit_par_for`: serial map into a scratch `[]i64`, then 4-way `par_reduce`). So
heavy per-element compute (`par for x in xs reduce(sum) { expensive(x) }`) isn't parallelized.
Fusing the map into the parallel workers (each worker maps *and* folds its chunk) would
parallelize the whole loop. Blocked on either A1/A2 or on lifting the spawn-generic limit (B1),
since the fused worker needs the body closure.

---

## B. Generality limits (one root cause unlocks several)

### B1. `spawn` targets cannot be generic — the root constraint
A generic worker fails C lowering ("the C backend cannot lower the external type `A`") because
the spawn arg-struct in `cgen::spawn_runtime` carries **no monomorphization substitution**.
This single limitation is *why*:
- `par_reduce`'s accumulator is fixed to **`i64`** (the worker must be monomorphic),
- `par for` is **`[]i64`-only**,
- `select` calls **non-generic `channel_{len,recv}_i64` wrappers** instead of the generic ops.

**Teaching `spawn_runtime` to carry the active `subst`** (the monomorphization map) would
generalize all three at once — `par_reduce(T)`, `par for` over any `[]T` with a deterministic
reduction, and `select` over `Channel(T)`. Highest-leverage *generality* fix (mirror of A1's role
for speed).

### B2. `spawn` of closures
Today a `spawn` target must be a **direct named call** (`spawn f(args)`). Allowing a closure
(`spawn || { … }`) would remove the boilerplate of declaring a top-level worker per task. Needs
closure-capture lowering into the task box (the captures become arg-struct fields).

### B3. Richer `select`
`select` is currently **recv-only, `Channel(i64)`, single-consumer**. Future:
- **Multi-type** — `Channel(T)` arms (unblocked by B1 or per-type wrappers).
- **Send arms** — `select { send(ch, v) => { … } … }` (Go-style), needing a non-blocking try-send.
- **Multi-consumer** — today the `len > 0` then `recv` is race-free only as the *sole* receiver;
  a true atomic try-recv would make `select` safe under multiple consumers.
- A **default arm** (`else { … }`) for non-blocking poll.

---

## C. The determinism story, deepened

### C1. `@deterministic` as a *block*, and transitively
Today `@deterministic` is a **function attribute** enforced directly: it forbids
`concurrent`/`spawn`/`atomic_*` in the function body, permitting only the checked `par for`.
Two extensions:
- A `@deterministic { … }` **block** (a region, not a whole function) — the design's original
  framing.
- **Transitive enforcement** — currently a `@deterministic` fn that *calls* `channel_recv` or
  `mutex_with` isn't caught (those use atomics inside the library, not in the fn body). A
  call-graph closure ("calls a function that uses atomics/locks/channels") would close this — the
  same future work flagged for `@no_alloc`.

### C2. Use-after-move as a compile error (language-wide)
The channel "no alias survives the send" guarantee currently rests on *"can't send a borrow"*
(the give-away rule) + structured-scope join. The stronger property — *you may not touch a value
after you've sent/moved it* — isn't a compile error yet, because move tracking exists only for
drop-glue in `cgen`, not as a diagnostic. Adding use-after-move detection would make
move-on-send fully airtight. (Language-wide, not channel-specific.)

---

## D. Smaller polish

- **Blocking primitives instead of spinlocks.** Mutex/channel use a busy-wait test-and-set
  spinlock — fine at low contention, burns CPU under high. A futex/condvar-backed blocking path
  would behave better under contention (the spinlock can stay as the fast path).
- **Channel `recv`/`send` backoff.** Same busy-wait concern; a yield/backoff would cut spin burn.
- **Qualified struct literals `mod.Type{…}` don't parse** — hit while writing a `par for`
  rejection test (worked around it). A modules-v2 (**workstream K**) gap, not N's; noted here only
  because it surfaced in N's testing.

---

## Out of scope (deliberately rejected — see the handoff)

- **Green-thread / fiber (M:N) scheduler** — a large hidden runtime that fights the
  no-ambient-machinery / deterministic thesis.
- **Work-stealing scheduler (Cilk/TBB)** — take the *cost model*, not the scheduler; Jestyr stays
  OS-thread + structured scope (a *static* pool, A2, is the allowed middle ground).
- **Effect-polymorphic / colorless async** — design §10.3 flags it as explicitly unsolved.
- **Non-deterministic "fast" reductions** as a default — would reintroduce the schedule-dependent
  answer the headline eliminates; only ever as a loudly-marked opt-out, excluded from the canary.

---

## Pointers
- Repo: `CONCURRENCY-HANDOFF.md` (what was built), `CONCURRENCY-PERF.md` (measured numbers +
  determinism verification), `ROADMAP.md` §3-N, `PARALLELISM-HANDOFF.md` (workstream Q — owns A1).
- Key files: `examples/std/{core,sync}.jtr` (the library), `src/cgen.rs`
  (`emit_concurrent`/`emit_par_for`/`emit_select`/`spawn_runtime`), `src/escape.rs`
  (the give-away rule + `@deterministic`).
