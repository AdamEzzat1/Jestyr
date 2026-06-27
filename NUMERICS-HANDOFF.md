# Numerics & `core`/`std` — Handoff (continue in a fresh session)

> Written at the close of the `core`/`std` + numerics workstream. Everything below
> is **on `master`** (head `7cccce7`), **500 tests green** (1 ignored),
> warning-clean. This note is self-contained: it says what exists, where, and
> exactly how to pick up each of the open fronts cold. **Front A (binned finish) and
> Front C `parse_float` are now DONE** — see their sections. Read with
> [`CORE-STD-PHASE3.md`](CORE-STD-PHASE3.md)
> (the full ledger), [`Jestyr-Remaining-And-Numerics-Research.md`](Jestyr-Remaining-And-Numerics-Research.md)
> (Part 3 is the numerics plan), and [`docs/TESTING.md`](docs/TESTING.md) §5.14
> (the test layer for all of this).

## Discipline (unchanged — keep it)

Every increment stays `cargo test`-green and **warning-clean**; default `cargo test`
is toolchain-free (no gcc) and fast. Ship the test layers (unit/golden +
property + the gcc round-trip *example*), **teeth-verify** each new property by
mutation (break it, watch it fail, revert), and **auto-commit each green increment**
(`git commit -F <file>`, one increment per commit, end the message with
`Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>`), then fast-forward
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

### B. Deterministic `par` runtime (parallel reduction that never changes the answer)

The binned accumulator's **merge** is the order-independent combine, so a parallel
reduction is now *expressible*: split the slice across threads, each fills a local
accumulator, then `binned_merge` them → bit-identical regardless of thread count.

What's there: `concurrent { spawn … }` → pthreads + scoped join
(`examples/concurrent.jtr`), atomics (`examples/atomics.jtr`). What's missing
(research doc §2.5 / §3.4.3): task **results**, and the **escape-checker disjointness
proof** for parallel writes. **First slice:** a `par`-style fold that spawns N
workers each summing a chunk into its own `[2048]i64`, joins, merges, finalizes;
assert the result is independent of N (the determinism canary). The hard part is the
join-safety / disjoint-write proof, not the reduction (already solved). Touches
`src/escape.rs` + the concurrency codegen. Bigger than A.

### C. Correctly-rounded float parse/format (Eisel–Lemire + Ryū) — the marquee

The distinctly-Jestyr deliverable (closes CJC's transcendental-determinism gap).

- **`parse_float` (Eisel–Lemire) — ✅ DONE** (commits: array literals `539a3bc`
  enabler, reference `4091488`, port `7cccce7`). `core.parse_float(str) ->
  Result(f64, ParseFloatError)`: a fast-path decimal scanner (≤ 19 significant
  digits) feeding compute_float (one `mul64` against the 1302-entry `POW10_128`
  table, upperbit, subnormal path, in-range round-to-even tie, overflow→±inf).
  Correctly rounded; > 19 digits returns `err(pf_too_many_digits)`. The table is
  generated **and validated end-to-end** by `proptests::lemire` (the same generator
  emits the Jestyr const via the `#[ignore] dump_pow10_table` test) against Rust's
  correctly-rounded `str::parse::<f64>()` over ~1M cases + hard cases; runtime pinned
  by `examples/std/parse_float.jtr`.
- **Remaining on C:**
  1. **`parse_float` slow path** — the > 19-digit / ambiguous cases that the fast
     path bails on (today: `err(pf_too_many_digits)`). A big-integer / AlgorithmM
     fallback. Smaller than format.
  2. **`format_float` (Ryū)** — shortest round-trip — the bigger half (its own
     lookup tables + the algorithm). Do it after the slow path or skip straight to
     it. With both halves, the `parse(format(x)) == x` round-trip property closes.
- **API:** locale-free, into a caller `[]u8` (no-alloc), like `format_i64`.
- **Tests for what remains:** `parse(format(x)) == x` for representable doubles;
  differential vs Rust's `format!`; and the **cross-OS locked-SHA-256 canary** —
  which needs the **`--features c-oracle` gcc-in-test harness** to exist first
  (shared future work; `DROP-ALLOC-PHASE3.md` future item 3 + research §3.6 Step 0).
  Until then, validate via the Rust mirror + the example (the pattern parse_float
  used).

**Recommended order:** ~~A (finishes the accumulator)~~ ✅, ~~C parse_float~~ ✅,
then **C format_float (Ryū)** ← *next* (or the parse_float slow path first, if you
want parse fully correct before starting format), then B (needs the disjointness
proof), then the canary harness.

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
| Demos | `examples/std/{binned,reductions,numbers,float_bits,combinators,slice_algos,parse_float}.jtr`, `examples/{arrays,array_lit}.jtr` |
| parse_float + table | `examples/std/core.jtr` (`parse_float`/`lemire_*`/`POW10_128`); reference + table generator `src/proptests.rs` → `mod lemire` |
| Property/law tests | `src/proptests.rs` → `mod core_props` (mirrors + laws), `mod prop`, `mod fuzz` |
| FP flags | `src/main.rs` → `CC_FLAGS` + `mod fp_contract_tests` |
| Budget canary | `src/main.rs` → `mod budget_canary` |
| `[N]T` arrays | `ast.rs` (`TypeKind::Array`, `ExprKind::ArrayRepeat`), `types.rs` (`Ty::Array`), `parser.rs`, `typeck.rs`, `cgen.rs` (`array_c_name`/`collect_arrays`/`array_struct_defs`/`emit_array_for`) |
| Full ledger | `CORE-STD-PHASE3.md` |
| The plan | `Jestyr-Remaining-And-Numerics-Research.md` Part 3 |
