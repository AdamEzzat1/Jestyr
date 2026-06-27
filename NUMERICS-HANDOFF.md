# Numerics & `core`/`std` — Handoff (continue in a fresh session)

> Written at the close of the `core`/`std` + numerics workstream. Everything below
> is **on `master`** (head `f297058`), **490 tests green**, warning-clean. This note
> is self-contained: it says what exists, where, and exactly how to pick up each of
> the three open fronts cold. Read with [`CORE-STD-PHASE3.md`](CORE-STD-PHASE3.md)
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

Test entry points: `cargo test core_props`, `cargo test --quiet` (all 490),
`cargo run --release -- selfbench` (speed: ~137K lines/s frontend; `cgen` ~47%).

---

## The three open fronts (pick one; each is a clean starting point)

### A. Binned accumulator — per-bin carry + correctly-rounded finalize

Two known limitations of the current binned accumulator (both documented in
`core.jtr` and `CORE-STD-PHASE3.md`):

1. **Per-bin overflow.** A bin is one `i64`; ~2¹⁰ same-exponent values can overflow
   it. The `core_props::binned_sum_is_chunk_independent` test currently uses small
   `n` (≤ 96) to stay under that. **Fix options:** (a) two `i64` per bin (a `hi`/`lo`
   limb pair) — widen `[2048]i64` to a `[2048]i64` pair or a struct array; or (b)
   *carry at finalize* — keep `binned_add` a pure `+=` (so bins stay order-
   independent), then in a deterministic pre-finalize pass normalize low→high:
   while `|bins[e]| ≥ 2^53`, move the overflow into the next exponent bin. Carry
   math: a bin-`e` unit is `2^(e-1075)`; an overflow of `2^53` units at bin `e`
   equals `2^52` units at bin `e+1`. **(b) is cheaper and keeps the add hot-path
   trivial — recommended.** Then widen the chunk-independence property's `n` to force
   overflow and confirm it still holds.
2. **Finalize is fixed-order, not correctly-rounded.** `binned_sum` does
   `acc += (bins[e] as f64) * 2^(e-1075)` ascending — deterministic, but the
   `i64→f64` cast and the FP adds round. For a *correctly-rounded* result,
   reconstruct the exact value (the bins **are** an exact big fixed-point number)
   and round once to nearest-even. This is the renormalize-then-round step of
   reproducible BLAS / a Kulisch accumulator. **Oracle for the test:** the exact sum
   is `Σ bins[e]·2^(e-1075)` as a rational/bignum; compare the Jestyr/mirror finalize
   to that rounded to `f64`. (A Rust `i128`/bignum mirror is the easiest oracle.)

Start in `core.jtr` (`binned_add`/`binned_sum`) + `core_props` (extend the mirror
`m_binned_*` and the two properties). Smallest of the three; no new compiler work.

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

The primitives are ready and validated (`mul64`, `clz64`, `f64_bits`/`f64_from_bits`,
field extractors). This is the distinctly-Jestyr deliverable (closes CJC's
transcendental-determinism gap) and the largest.

- **`parse_float` (Eisel–Lemire)** is the smaller half. Reuse the `parse_i64` digit
  loop to read the decimal significand + power-of-ten exponent, normalize, do **one
  `mul64`** against a precomputed power-of-ten table (~`5e-22..5e22`, ≈ 650 `u64`
  hi/lo pairs — generate it and verify each entry against Rust), derive the 53-bit
  mantissa + binary exponent, handle the rounding-tie (the "is the product exactly
  halfway" check) and the slow-path fallback. Reference: Lemire's *fast_float*. The
  table is the bulk of the code; `[N]T` arrays now hold it.
- **`format_float` (Ryū)** — shortest round-trip — is the bigger half (its own lookup
  tables + the algorithm). Do it after parse.
- **API:** locale-free, into a caller `[]u8` (no-alloc), like `format_i64`.
- **Tests:** `parse(format(x)) == x` for representable doubles; differential vs
  Rust's correctly-rounded parse / `format!`; and the **cross-OS locked-SHA-256
  canary** — which needs the **`--features c-oracle` gcc-in-test harness** to exist
  first (shared future work; `DROP-ALLOC-PHASE3.md` future item 3 + research §3.6
  Step 0). Until then, validate via the Rust mirror + the example.

**Recommended order:** A (small, finishes the accumulator), then C parse_float
(marquee, primitives ready), then B (needs the disjointness proof), then C
format_float, then the canary harness.

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
- **The recurring codegen pattern:** every generic lowering must resolve the inferred
  type through the active monomorphization subst *before* naming a C type —
  `apply_subst(&self.info.type_of(id), &self.subst)`. It was missing in enum
  construction/match, fn-pointer signatures, slice `for`/index, and array `for`/index;
  all fixed. **Latent refactor:** a single `resolve_monomorphized_type(id)` helper
  every lowering path calls, instead of raw `self.info.type_of(id)`.
- **No fixed-size stack arrays** *was* the blocker for the accumulator — now solved
  (`[N]T`). List literals `[a, b, c]` are still unbuilt (only `[v; N]` repeat).

## Pointers

| Area | Where |
|---|---|
| Numerics + `core` library | `examples/std/core.jtr` |
| Demos | `examples/std/{binned,reductions,numbers,float_bits,combinators,slice_algos}.jtr`, `examples/arrays.jtr` |
| Property/law tests | `src/proptests.rs` → `mod core_props` (mirrors + laws), `mod prop`, `mod fuzz` |
| FP flags | `src/main.rs` → `CC_FLAGS` + `mod fp_contract_tests` |
| Budget canary | `src/main.rs` → `mod budget_canary` |
| `[N]T` arrays | `ast.rs` (`TypeKind::Array`, `ExprKind::ArrayRepeat`), `types.rs` (`Ty::Array`), `parser.rs`, `typeck.rs`, `cgen.rs` (`array_c_name`/`collect_arrays`/`array_struct_defs`/`emit_array_for`) |
| Full ledger | `CORE-STD-PHASE3.md` |
| The plan | `Jestyr-Remaining-And-Numerics-Research.md` Part 3 |
