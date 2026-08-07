> **Internal development log** — kept for provenance. Status lines and counts in this file reflect the moment it was written, not the current state. Start at the repo [README](../../README.md) for current status and verified claims.

# Jestyr — Workstream Q (Data Parallelism), Tier 1: the deterministic SOAC library

*Session summary. Increment landed on `master`. Companion: `PARALLELISM-HANDOFF.md`.*

## Context: a coordination story

This session started on increment 1 (the `par_reduce` library). Mid-flight, the
**Concurrency (N)** session independently landed the *same* tier-1 `par_reduce` —
`core.par_reduce`, generalizing `core.par_binned_sum` to a `Reduction` value
(identity + associative `accumulate`/`combine`, built-ins sum/min/max/xor), using
**function pointers** passed through `spawn`. The Parallelism handoff anticipated exactly
this overlap ("N may also generalize `par_binned_sum` — coordinate").

Rather than ship a duplicate, this session **advanced Q to the next unbuilt increment**:
the other two workhorse SOACs, **`par_map`** and **`par_scan`**, built on top of N's
primitive and the now-proven fn-pointer-through-`spawn` capability. Net result: all three
tier-1 SOACs (reduce, map, scan) are in-tree and deterministic.

## What landed (this session)

New stdlib module **`examples/std/parallel.jtr`** — `par_map` + `par_scan`, library tier,
**zero compiler change** (reuses `concurrent`/`spawn`).

### `par_map` — embarrassingly parallel element-wise map

```jtr
pub fn par_map(read s: []i64, dst: *mut i64, f: fn(i64) -> i64)
pub fn serial_map(read s: []i64, dst: *mut i64, f: fn(i64) -> i64)   // oracle
```

Four workers each map their input chunk into the **aligned, disjoint** output region.
No merge. `output[i]` depends only on `input[i]`, so the result is identical to serial
**by construction**, for any split — the simplest possible determinism story.

### `par_scan` — deterministic inclusive prefix scan (two-pass)

```jtr
pub fn par_scan(read s: []i64, dst: *mut i64, identity: i64, op: fn(i64,i64) -> i64)
pub fn serial_scan(read s: []i64, dst: *mut i64, identity: i64, op: fn(i64,i64) -> i64)
pub fn op_add / op_min / op_max / op_xor (a, b: i64) -> i64          // built-in ops
pub fn par_scan_sum / par_scan_min / par_scan_max / par_scan_xor(read s, dst)  // wrappers
```

The classic **two-pass** parallel scan:
1. **Pass 1 (parallel):** each worker reduces its chunk to a single total → disjoint slot.
2. **Sequential middle (constant work):** exclusive-prefix the four totals into per-chunk
   seeds (a 3-step fold).
3. **Pass 2 (parallel):** each worker scans its chunk seeded with its prefix, writing its
   own output region.

`dst[i] = identity ⊕ s[0] ⊕ … ⊕ s[i]`. The result is **bit-identical to the serial scan
for any worker count** — and this rests *entirely* on the operator being **associative**:
`(a⊕b)⊕c == a⊕(b⊕c)` is precisely what lets the stream be regrouped into chunks without
changing the answer. A non-associative reduction (naive float `+`) would be schedule-
dependent and is **deliberately not offered**.

**Design note — decoupling:** `par_scan` takes the op + identity *directly* rather than
N's `core.Reduction`, because that struct's fields are **module-private** to `core` and
exposing them would mean editing N's `par_*` region (the one MEDIUM-risk shared seam).
Taking `(identity, op)` keeps Q fully in its own lane while documenting the same
associativity precondition. The `par_scan_*` wrappers supply the built-in op + identity so
callers never write `&`.

Demo **`examples/std/par_soac.jtr`** → `1 1 1 1 1 500500` (two maps, two scans via the
general API, the `par_scan_sum` wrapper — each compared bit-for-bit to its serial oracle —
then the prefix sum of 1..=1000 ending at the triangular number 500500).

## How it's tested (mirrors the existing harness)

| Layer | Test | Toolchain |
|---|---|---|
| **`par_scan` determinism (the star)** | `parallel_props::par_scan_is_split_independent` — two-pass chunked scan == serial inclusive scan, for *every* partition and every associative built-in op | none (default `cargo test`) |
| **`par_map` determinism** | `parallel_props::par_map_is_split_independent` — chunked map == whole-slice map for any partition | none |
| **Compile-clean** | `par_soac_example_compiles_clean` — load + typeck + escape (spawned workers with a `read` slice + raw `*mut i64` + `fn` pointer accepted; disjoint writes race-free) | none |
| **End-to-end thread run** | `c_oracle::par_soac_demo` — gcc-compiled, real OS threads, run 8× to shake out races; output pinned | `--features c-oracle` (gcc) |

Full suite: **609 passed / 0 failed** (default), `par_soac_demo` green under c-oracle,
warning-clean for the new code.

**Teeth:** the scan property breaks immediately if the mirror's `op` is made
non-associative — confirming the test actually constrains the associativity guarantee.

## Findings worth recording

- **Top-level `const`s share one C namespace across imported modules.** `core.PAR_WORKERS`
  and an `I64_MIN` defined in two files both collide at link time. Worked around by
  inlining the literal; the underlying gap is a **modules-v2 (K)** item (cgen doesn't
  mangle const names per module), out of this lane.
- **Struct fields are module-private by default** — `core.Reduction`'s fields can't be read
  from another module, which drove `par_scan`'s decoupled `(identity, op)` signature.
- **`out` is a reserved keyword** (the `out` parameter mode) — can't name a local/param
  `out`; used `dst`/`pre`.
- **`spawn` passes `fn` pointers** (proven by N's `par_reduce`), which is what makes a
  function-parameterized `par_map`/`par_scan` possible without a compiler change.

## Explicitly deferred (and why)

- **The `par for … reduce(r)` surface + non-deterministic-reduction rejection** — next, and
  the **first compiler change** (a new additive `ExprKind::ParFor`): the compiler *refuses*
  a non-deterministic reduction at compile time, turning the library's "not offered" into a
  checked guarantee.
- **The `with schedule(threads=…, chunk=…)` split** — needs **dynamic-N spawn**
  (spawn-in-a-loop), shared with N; gates a *tunable* schedule whose result stays
  bit-identical (to be added to the SHA canary's demo set).
- **The work-span (`W`/`D`) cost model** + CJC thermal/energy — the Motley tie-in.
- **SIMD (`uniform`/`varying` + width-independent lane reductions)** and **GPU SOACs** —
  real backend work, far more valuable with the CPU determinism guarantee already proven.

Tier 1 deliberately ships on emitted C with no compiler change, proving the determinism
story for all three core SOACs end-to-end on real threads first.
