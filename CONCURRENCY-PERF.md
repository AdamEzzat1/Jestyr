# Concurrency (Workstream N) — Performance Characterization

> Measured performance + determinism verification for Jestyr's concurrency primitives,
> with the architectural ceilings stated plainly. Companion to `CONCURRENCY-HANDOFF.md`
> (what was built) and `ROADMAP.md` §3-N. Numbers below were taken on a **4-core
> Windows box, gcc, the locked `CC_FLAGS = -ffp-contract=off -fno-fast-math`**; they are
> illustrative, not a portable benchmark. Re-measure on the target machine — the SHA
> canary is what pins *correctness* across machines, not these timings.

---

## 1. Determinism — provable, and verified bit-for-bit

The headline holds **unconditionally**: a deterministic parallel reduction is bit-identical
to the serial fold regardless of thread count, schedule, or input size.

| reduction | serial | parallel (4-worker) | identical? |
|---|---|---|---|
| integer sum, 16M × 16 reps | `57344` | `57344` | ✅ bit-for-bit |
| binned `f64` sum, 8M × 8 reps | bits `-536870912` | bits `-536870912` | ✅ bit-for-bit |

This is a **checked** property at three layers, not an observed coincidence:
1. **Compile-time rejection** — `par for … reduce(r)` accepts only *declared deterministic*
   reductions (`core` sum/min/max/xor); a non-deterministic one (naive float `+`) is a
   compile error. "Parallelism that cannot change your answer."
2. **Property test** — `core_props::par_reduce_is_split_independent`: whole-fold ==
   any-chunk-split-then-merge, for every built-in, over arbitrary splits.
3. **Cross-OS SHA canary** — `proptests::c_oracle` pins the demo output bytes across
   OS/compiler, so a determinism break is caught even without re-deriving the digest.

Determinism is by *construction* (disjoint accumulator regions + an order-independent
combine), so it scales to any worker count / input with no extra proof.

---

## 2. Speed & scale — modest, bounded, predictable

| primitive | measured | bound |
|---|---|---|
| `par_reduce`, integer sum (4 workers) | ~1.1× vs serial | **memory-bandwidth** — 4 threads reading 128 MB share the bus |
| `par_binned_sum`, heavy per-element fold (4 workers) | ~1.5–2× (best run 738→364 ms) | compute-heavier ⇒ the 4 workers help more; still part bandwidth/fill-bound |
| dynamic-N `spawn` | **~110–140 µs / task**; 10 000 tasks ≈ 1.3 s | one real OS thread per task, **no pool** |
| Mutex / channel | (low-contention only) | busy-wait test-and-set spinlock |

### The deliberate ceilings (know these)
- **`par_reduce` / `par for` are pinned at 4 workers** (`PAR_WORKERS = 4`, `examples/std/core.jtr`).
  They do not scale past 4 regardless of cores or input. Theoretical max 4×, less in practice.
- **`par for`'s per-element map runs serially** — only the reduction *fold* is 4-way parallel.
  Heavy per-element compute is not parallelized by `par for` today.
- **No thread pool / scheduler.** `concurrent` / dynamic-N `spawn` create raw OS threads, so
  fine-grained tasks are thread-creation-bound. Excellent for a handful of coarse chunks; poor
  for thousands of tiny tasks.
- **Spinlock contention.** Mutex/channel use a busy-wait test-and-set — fine at low contention,
  burns CPU under high; channels also busy-wait on `recv`/`send`.

---

## 3. The honest framing

This is a **correctness-first, structured** concurrency model: OS threads + scoped joins,
optimized for **provable determinism and data-race safety**, explicitly **not** raw throughput
(the handoff rules out a work-stealing scheduler as "a large hidden runtime that fights the
deterministic thesis"). So the headline holds — *"parallelism that cannot change your answer,"*
and it's a compile error to violate it — while absolute speed is *"modest but predictable,"*
bounded by the 4-worker cap and the no-pool tradeoffs.

The two levers that would change the speed story are both **future work, and workstream Q owns
them**: the `with schedule(threads, chunk)` split (configurable worker count, built directly on
the dynamic-N spawn N landed) lifts the 4-worker cap, and a thread pool would kill the per-task
creation cost. **Neither touches the determinism guarantee** — that is the point of the design:
you can make it faster without making it less correct. The full backlog is in
`jestyr-concurrency-future-work.md`.
