> **Internal development log** — kept for provenance. Status lines and counts in this file reflect the moment it was written, not the current state. Start at the repo [README](../../README.md) for current status and verified claims.

# Numerics & `core`/`std` — Handoff (continue in a fresh session)

> Written at the close of the `core`/`std` + numerics workstream. Everything below
> is **on `master`** (head `bbf426b`), **508 tests green** (3 ignored) + 8 more
> under `--features c-oracle`, warning-clean. This note is self-contained. **Done:
> Front A (binned finish); Front C — the marquee — parse_float (incl. the >19-digit
> slow path, so it is correctly rounded with *no* caveat) + format_float, round-trip
> closed; Front B's deterministic `par_binned_sum` + a sound spawn data-race rule;
> and the gcc-in-test c-oracle harness with a locked SHA-256 cross-OS determinism
> canary.** What remains is research-grade (a general disjoint-write proof) or
> deliberately-skipped polish (a Ryū perf swap — identical output). Read with
> [`CORE-STD-PHASE3.md`](CORE-STD-PHASE3.md)
> (the full ledger), [`Jestyr-Remaining-And-Numerics-Research.md`](Jestyr-Remaining-And-Numerics-Research.md)
> (Part 3 is the numerics plan), and [`docs/TESTING.md`](docs/TESTING.md) §5.14
> (the test layer for all of this).

## Discipline (unchanged — keep it)

Every increment stays `cargo test`-green and **warning-clean**; default `cargo test`
is toolchain-free (no gcc) and fast. Ship the test layers (unit/golden +
property + the gcc round-trip *example*), **teeth-verify** each new property by
mutation (break it, watch it fail, revert), and **auto-commit each green increment**
(`git commit -F <file>`, one increment per commit), then fast-forward
`master`: `git -C <repo-root> merge --ff-only <branch>`. Don't push unless asked.

Verify a numeric change end-to-end with `jestyrc run examples/std/<demo>.jtr` (there
is **no gcc-in-`cargo test` harness yet** — the property layer asserts on emitted C
or a Rust mirror; runtime is pinned by running the example).

---

## What exists (the determinism spine — done & tested)

All library code is in [`examples/std/core.jtr`](examples/std/core.jtr) (the no-alloc
`core`; demos `import "core"`). Symbols, grouped:

- **IEEE-754 / 128-bit primitives:** `f64_bits`/`f64_from_bits` (via an untagged
  `union FU`), `f64_sign`/`f64_raw_exp`/`f64_mantissa`; `U128`/`mul64` (64×64→128
  from 32-bit halves — there is **no `u128`**), `clz64`. Validated by
  `core_props::mul64_matches_u128` (vs Rust `u128`) and `clz64_matches_builtin`.
  Demo `examples/std/float_bits.jtr`.
- **FP-codegen determinism contract (Step 0):** `CC_FLAGS` in
  [`src/main.rs`](src/main.rs) pins `-ffp-contract=off -fno-fast-math` for every
  translation unit (no FMA fusion, no reassociation). Locked by
  `main::fp_contract_tests::fp_determinism_flags_are_locked`.
- **Serial reductions:** `f64_sum` (naive), `f64_kahan_sum` (**Neumaier**
  compensated), `f64_pairwise_sum` (fixed `len/2` split). Run/platform-deterministic.
  `core_props::reductions_*`. Demo `examples/std/reductions.jtr` → `10 10 10 | 0 2 0`.
- **Binned superaccumulator (the headline):** `binned_new`/`binned_add`/
  `binned_merge`/`binned_sum`/`f64_binned_sum`, over 2048 integer exponent bins
  (`[2048]i64`). **Chunk-count-independent** — `binned_add` deposits a value's
  integer significand into its biased-exponent bin, so the bins are order/chunk-
  independent (integer addition), and the finalize folds in fixed ascending order.
  `core_props::binned_sum_is_chunk_independent` (whole == any split-then-merge,
  bit-for-bit). Demo `examples/std/binned.jtr` → `4 4 1`.
- **Integer parse/format:** `parse_i64`/`parse_u64` (defined overflow), `format_i64`/
  `format_u64` into a caller `[]u8`. `core_props` round-trip + differential-vs-Rust.
  Demo `examples/std/numbers.jtr`.
- **Option/Result combinators + slice algorithms** (functor/monad laws; fold/find/
  any/all/sort/binary_search). Demos `combinators.jtr`, `slice_algos.jtr`.

**Compiler features added this session** (all on `master`): generic enums inside
generic functions (subst through construction/match/fn-pointer signatures);
fn-pointer typedefs returning aggregates (`gen_forward_types`); slice-index
assignment lvalue; and **`[N]T` fixed-size arrays** (a value type → C
`struct { T a[N]; }`; `[v; N]` literal, bounds-checked index r/w, `.len`,
`for x in arr`; demo `examples/arrays.jtr`).

Test entry points: `cargo test core_props`, `cargo test --quiet` (all 492),
`cargo run --release -- selfbench` (speed: ~137K lines/s frontend; `cgen` ~47%).

---

## The open fronts (pick one; each is a clean starting point)

### A. Binned accumulator — per-bin carry + correctly-rounded finalize — ✅ DONE

Both former limitations are resolved (commits `8f39ee1`, `6e1ccb8`):

1. **Correctly-rounded finalize** (was: fixed-order FP fold). `binned_sum` now
   reconstructs the bins' **exact** integer value — they are an exact big fixed-point
   number `Y·2^-1074` — as a 36-limb (2304-bit) unsigned bignum (two non-negative
   `pos`/`neg` halves, subtracted, to dodge two's-complement sign handling), finds
   the MSB, extracts the top 53 bits + round + sticky, and rounds **once** to
   nearest-even (overflow → ±inf; values < 2^-1021 exact). Helpers `big_*` in
   `core.jtr`; mirror `m_binned_round` + independent dep-free oracle (exact `i128`
   sum at the finest scale → correctly-rounded `i128→f64` × exact power of two) in
   `core_props::binned_round_is_correctly_rounded`.
2. **Per-bin overflow** (was: ~2¹⁰-same-exponent-adds i64 wrap). `binned_add` now
   **cascades a carry** up the exponents (2:1 between bins, 1:1 across the bin-0/1
   shared ULP) when a bin reaches 2^53 — amortized O(1), keeps bins bounded so merges
   can't wrap either. The carry is value-preserving, so it is sound *only because*
   the finalize reads the exact value, not the bin layout. Property
   `core_props::binned_handles_per_bin_overflow` (n > 2¹⁰ identical adds; asserts
   chunk-independence **and** correctness vs the true total).

**Key design lesson (recorded for B/C):** output bit-identicality requires only the
bins' *exact value* to be chunk-invariant, **not** the bins themselves. The
correctly-rounded finalize is what decouples representation from result — and that
decoupling is exactly what makes a future deterministic parallel reduction (Front B)
free to merge/normalize however it likes.

Not yet done (optional follow-ups, neither blocks B or C): carry-normalize inside
`binned_merge` so an *unbounded* fan-in of accumulators can't wrap (today a single
merge of two is safe; a long sequential merge chain of many could approach the
bound), and an `f64`/runtime SHA canary once the gcc-in-test harness exists.

### B. Deterministic `par` runtime — first slice ✅ DONE; static proof remains

**Done (commit `18a9ad8`):** `core.par_binned_sum(s)` — a *parallel* sum
bit-identical to the serial `f64_binned_sum`, on real OS threads. Splits the slice
across `PAR_WORKERS` (=4) via `concurrent { spawn … }`; each worker bins its chunk
into a LOCAL `[2048]i64` (reusing `binned_add`, so the carry/correct-rounding logic
is never re-implemented) and copies it to its own heap region; after the join the
regions merge by elementwise integer add and finalize once. Disjointness is **by
construction** (region `w` = `[w·2048, w·2048+2048)`), so no race and no atomics on
the hot path. Demo `examples/std/par_reduce.jtr` (par == serial across cancellation,
n=1000, uneven n=7); the contract is property-proven for arbitrary splits by
`core_props::binned_sum_is_chunk_independent`. **Mechanism learned:** a slice / raw
pointer passed *by value* into the spawn arg-struct still aliases the same heap, so
worker writes survive the join — that's what lets per-worker accumulators work
without `mut`-array spawn args (which the spawn lowering does **not** support: it
stores args by value, but a `mut` param is by-address → mismatch; pass `*mut i64` /
slices instead).

**What's there in the compiler:** `concurrent { spawn f(args) }` → pthread-per-spawn
+ join-at-`}`; args copied into a per-site struct (`emit_concurrent`/`spawn_runtime`
in `cgen.rs`). `spawn` only works as a **literal statement** in the concurrent
block (not in a loop → no dynamic N), targets a **direct named call**, and has **no
result**. Atomics via `__atomic_*` (`examples/atomics.jtr`).

**Data-race rule — ✅ DONE (the soundly-deliverable part, commit `bbf426b`):**
`src/escape.rs` `check_spawn_no_shared_mut_slice` rejects spawning a function that
takes a `mut`/`out` **slice** — the one safe-subset data race (a shared slice's `ptr`
aliases; a `mut [N]T` value-array arg is copied, so it can't). Shared mutable state
across tasks must now go through a raw `*mut T` in `unsafe` (each task a disjoint
region, as `par_binned_sum` does). The safe subset is therefore race-free; join-
safety was already enforced by the structured-concurrency walk.

**Remaining (genuinely research-grade / nice-to-have):**
1. **General disjoint-write proof** for the *unsafe* raw-pointer case — proving
   `raw+0` vs `raw+2048` (or `buf` slot 0 vs slot 1) don't overlap needs range-aware
   alias analysis, or a typed split-ownership API (`split_mut` handing each task a
   provably-disjoint subslice). A naive "same base → reject" wrongly flags
   `par_binned_sum`/`concurrent.jtr`. Today: programmer-asserted inside `unsafe`.
2. **Dynamic-N spawn** (spawn-in-a-loop with a handle array) so the worker count
   isn't fixed at 4 — needs `emit_concurrent` to handle `for { spawn … }`.
3. **Task results** (a `spawn` that returns a value joined back).

### C. Correctly-rounded float parse/format (Eisel–Lemire + Ryū) — the marquee

The distinctly-Jestyr deliverable (closes CJC's transcendental-determinism gap).

- **`parse_float` (Eisel–Lemire) — ✅ DONE** (commits: array literals `539a3bc`
  enabler, reference `4091488`, port `7cccce7`). `core.parse_float(str) ->
  Result(f64, ParseFloatError)`: a fast-path decimal scanner (≤ 19 significant
  digits) feeding compute_float (one `mul64` against the 1302-entry `POW10_128`
  table, upperbit, subnormal path, in-range round-to-even tie, overflow→±inf).
  Correctly rounded. The table is generated **and validated end-to-end** by
  `proptests::lemire` (the same generator emits the Jestyr const via the `#[ignore]
  dump_pow10_table` test) against Rust's correctly-rounded `str::parse::<f64>()` over
  ~1M cases + hard cases; runtime pinned by `examples/std/parse_float.jtr`.
  **Slow path — ✅ DONE** (reference `14f23b5`, Jestyr port `79b9d4b`): > 19 digits no
  longer bails — a candidate `g` from the first 19 digits is refined by a
  division-free big-integer comparison of `D·10^E` to the rounding midpoint
  (`slow_parse`/`sp_*` 56-limb bignum; full significand kept, sticky past 768 digits).
  parse_float is now correctly rounded with **no digit caveat**. Validated by
  `proptests::lemire::slow_parse_*` (incl. exact-midpoint teeth + a 2M `#[ignore]`
  sweep).
- **`format_float` (shortest round-trip) — ✅ DONE** (commit `05a57f8`).
  `core.format_float(x, []u8) -> usize` writes the shortest decimal that parses back
  to `x`, as `[-]d[.ddd]e±E`. Implemented as **Dragon4** (Steele-White /
  Burger-Dubois), *not* Ryū: same shortest-round-trip output, but big-integer and
  **table-free** — no precision-coupled lookup tables to get subtly wrong (the right
  trade for a from-scratch auditable `core`; Ryū is a later perf swap with identical
  output). The 40-limb fixed bignum (`d4_*`) needs no division (each digit is ≤ 9
  subtractions). Validated by `proptests::dragon` (round-trips + minimal length +
  all-but-last-digit match vs Rust `{:e}`; 2M-case `#[ignore]` thorough run green);
  runtime pinned by `examples/std/format_float.jtr` (round-trip via parse_float).
  **The `parse(format(x)) == x` contract is now closed.**
- **The cross-OS SHA-256 canary — ✅ DONE + ✅ PURIFIED** (commits `1670498`, then
  purified this session): the `--features c-oracle` gcc-in-test harness
  (`proptests::c_oracle`) compiles + runs demos through gcc (the `jestyrc run`
  pipeline) and locks a SHA-256. The hashed input is now the dedicated
  `examples/std/numerics_canary.jtr`, which prints **only** integers + `format_float`
  strings (no `print_f64`/`printf("%g")`), so the digest can't false-alarm on libc
  formatting — a diff means a *genuine* determinism break. Locked to `886d1b6a…`
  (was `dfe9f735…` pre-purification). A dep-free self-tested SHA-256 lives in
  `proptests::sha256`. This is the artifact a CI matrix runs on Linux/macOS/Windows to
  *prove* byte-identical numeric output. **Still single-platform** — see
  [`FP-DETERMINISM-CONTRACT.md`](FP-DETERMINISM-CONTRACT.md) gap #1 for the cross-OS run.
- **Remaining on C — only a deliberate non-goal:** a **Ryū `format_float`** swap
  (replace Dragon4's per-digit big-integer loop with Ryū's two 128-bit tables). It is
  **pure perf with byte-identical output**, and reproducing Ryū's precision-125 tables
  is the exact from-memory risk Dragon4 was chosen to avoid — so it is **not worth
  doing** unless format throughput becomes a real bottleneck. If ever needed, the
  `proptests::dragon` differential is its ready-made oracle. The Jestyr Dragon4
  already allocates nothing (fixed `d4_*` bignum), so even the perf motivation is weak.

**Recommended order:** ~~A~~ ✅, ~~C parse_float (incl. slow path)~~ ✅,
~~C format_float~~ ✅, ~~B `par_binned_sum` + spawn data-race rule~~ ✅,
~~SHA canary~~ ✅. **The only substantive open item is Front B's general
disjoint-write proof** (range-aware alias analysis or a `split_mut` ownership API —
research-grade). Ryū is an explicit non-goal (see above).

---

## Compiler gotchas found this session (save yourself the debugging)

- **`expr.*` then a line starting with `(`** parses as a call (`x.* \n (y)` →
  `x.*(y)`). Use an intermediate `var p = …` so no statement begins with `(`, or
  reorder. (Bit me in the pointer-swap / format code.)
- **Multiple statements on one line** can mis-parse — one statement per line.
- **Range sub-slicing `buf[0..n]` works for `str` only**, not `[]T`. For a `[]T`
  sub-view use `slice(T, ptr, n)` (exact length).
- **Ambiguous nullary/variant in a generic-call arg** (e.g. `res_and_then(…, ok(20),
  …)` — `ok(20)` pins `T` but not `E`) needs a typed `let` binding first. The fix
  (substitute callee comptime args into param types before inferring the arg) is
  unbuilt.
- **`is_<variant>` function names collide** with auto-generated variant predicates
  (`is_err`, `is_ok`, …). Prefix your helpers (`res_is_err`, etc.).
- **Huge `u64` decimal literals** warn in the emitted C ("constant so large it is
  unsigned"). Compute them instead (`0 - 1` for `u64::MAX`, shifts for powers of two).
  *But* **hex** literals ≥ 2⁶³ are fine (C makes them unsigned, no warning) — that is
  how `POW10_128` holds its top-bit-set entries.
- **`(0 - 1) >> 55` shifts in 32-bit.** Bare integer literals are `i32`, so `0 - 1`
  is `-1:i32` and `>> 55` overflows the type (wrong value + a warning). Force the
  width *before* the shift: bind `let allone: u64 = 0 - 1` then shift, or just write
  the constant (`511`). Same trap for any `1 << k` with `k ≥ 31` — use `let one: u64
  = 1` then `one << k`, or `(1 as u64) << k`.
- **Enum variant constructors are global**, not scoped to their enum. A second enum
  reusing a variant name (`ParseFloatError::empty` vs `ParseIntError::empty`) makes
  the bare `empty` ambiguous and silently miscompiles the *other* enum's `err(empty)`.
  Prefix variants (`pf_empty`, …) to keep them unique across the module.
- **Array list literals** (`[e0, e1, …]`, commit `539a3bc`) now exist alongside
  `[v; N]`. A top-level `const` array lowers to a C **brace initializer**; inside a
  function it is a statement-expression. Indexing a `const` array is `const`-correct.
- **The recurring codegen pattern:** every generic lowering must resolve the inferred
  type through the active monomorphization subst *before* naming a C type —
  `apply_subst(&self.info.type_of(id), &self.subst)`. It was missing in enum
  construction/match, fn-pointer signatures, slice `for`/index, and array `for`/index;
  all fixed. **Latent refactor:** a single `resolve_monomorphized_type(id)` helper
  every lowering path calls, instead of raw `self.info.type_of(id)`.
- **No fixed-size stack arrays** *was* the blocker for the accumulator — now solved
  (`[N]T`), and list literals `[a, b, c]` are now built too (the table enabler).

## Pointers

| Area | Where |
|---|---|
| Numerics + `core` library | `examples/std/core.jtr` |
| Demos | `examples/std/{binned,reductions,numbers,float_bits,combinators,slice_algos,parse_float,format_float,par_reduce,numerics_canary}.jtr`, `examples/{arrays,array_lit,concurrent,atomics}.jtr` |
| par reduction | `examples/std/core.jtr` (`par_binned_sum`/`par_worker`); concurrency lowering `src/cgen.rs` (`emit_concurrent`/`spawn_runtime`); join-safety `src/escape.rs` (`ExprKind::Spawn`) |
| parse_float + table | `examples/std/core.jtr` (`parse_float`/`lemire_*`/`POW10_128`/`slow_parse`/`sp_*`); reference + table generator + slow-path `src/proptests.rs` → `mod lemire` |
| format_float (Dragon4) | `examples/std/core.jtr` (`format_float`/`d4_*`); reference `src/proptests.rs` → `mod dragon` |
| determinism canary | `src/proptests.rs` → `mod c_oracle` (gcc-in-test, `--features c-oracle`) + `mod sha256`; locked digest `886d1b6a…` over the **purified** `examples/std/numerics_canary.jtr` (integers + `format_float` only). Run: `cargo test --features c-oracle` |
| spawn data-race rule | `src/escape.rs` (`check_spawn_no_shared_mut_slice`) |
| Property/law tests | `src/proptests.rs` → `mod core_props` (mirrors + laws), `mod prop`, `mod fuzz` |
| FP flags | `src/main.rs` → `CC_FLAGS` + `mod fp_contract_tests` |
| Budget canary | `src/main.rs` → `mod budget_canary` |
| `[N]T` arrays | `ast.rs` (`TypeKind::Array`, `ExprKind::ArrayRepeat`), `types.rs` (`Ty::Array`), `parser.rs`, `typeck.rs`, `cgen.rs` (`array_c_name`/`collect_arrays`/`array_struct_defs`/`emit_array_for`) |
| Full ledger | `CORE-STD-PHASE3.md` |
| The plan | `Jestyr-Remaining-And-Numerics-Research.md` Part 3 |
