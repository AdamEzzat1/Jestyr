# FP / Determinism Contract — Handoff (lock it in a fresh session)

> **Honest status: the contract is *substantially* locked, not *proven*.** The parts
> that are Jestyr's to control — the FP-codegen flags and the determinism-by-
> construction primitives — are locked and tested, and the canary is now **purified**
> (gap #2 below, closed): it hashes a dedicated demo that emits *only* integers and
> our own `format_float` strings, so no `printf("%g")` output rides in the digest. The
> one remaining blocker to *proof* is mechanical: the cross-platform run (gap #1). The
> SHA-256 canary has still only been computed on one machine (Windows + gcc), so the
> digest's *cross-OS identity* is **unverified**. Read this before claiming "cross-OS
> determinism." Everything below is on `master`, **508 tests green** (+8 under
> `--features c-oracle`), warning-clean.
>
> Companion docs: [`NUMERICS-HANDOFF.md`](NUMERICS-HANDOFF.md) (the whole numerics
> workstream), [`CORE-STD-PHASE3.md`](CORE-STD-PHASE3.md) (ledger),
> `Jestyr-Remaining-And-Numerics-Research.md` Part 3 / §3.3 / §3.6 (the original plan).

---

## What the contract is

Jestyr emits C and builds it with `gcc -O2`, so it does **not** inherently control
floating-point contraction. `a*b+c` can fuse into an FMA (different rounding), and
`-ffast-math` would permit reassociation — either breaks bit-identical results across
machines/SIMD/compilers. The contract: **make no-FMA / no-fast-math a *codegen
invariant*, and make the numeric primitives deterministic by construction**, then
**prove** the whole thing reproduces bit-for-bit across OS/compiler via a locked
digest. CJC handled this as a *runtime* policy; Jestyr must do it at *codegen* time.

---

## ✅ Locked (and tested) — the parts Jestyr controls

1. **FP-codegen flags.** `CC_FLAGS = ["-O2", "-std=c11", "-ffp-contract=off",
   "-fno-fast-math"]` in [`src/main.rs`](src/main.rs), applied to every translation
   unit `jestyrc` builds. Asserted by `main::fp_contract_tests::
   fp_determinism_flags_are_locked` (breaks if a flag is removed). FTZ/DAZ is a
   runtime MXCSR state the emitted program simply never sets.
2. **Determinism by construction** (these barely depend on the flags — they avoid
   order-dependent FP entirely; in `examples/std/core.jtr`):
   - **Binned superaccumulator** — integer exponent bins; chunk/thread-count
     invariant; correctly-rounded finalize; add-time carry. (`binned_*`, tests
     `core_props::binned_*`.)
   - **`parse_float`** — Eisel–Lemire + a division-free big-integer slow path;
     correctly rounded for *any* digit count. (`proptests::lemire`.)
   - **`format_float`** — Dragon4 shortest round-trip, correctly rounded. (`proptests::dragon`.)
   - **`par_binned_sum`** — parallel reduction bit-identical to serial; the escape
     checker forbids `mut`-slice spawn params so the safe subset is race-free.
3. **The canary *mechanism* + a *purified* hash input.** `cargo test --features
   c-oracle` (`proptests::c_oracle`) compiles + runs demos through gcc — the real
   `jestyrc run` pipeline — and hashes output with a dep-free, self-tested SHA-256
   (`proptests::sha256`). The hashed input is now the single dedicated demo
   [`examples/std/numerics_canary.jtr`](examples/std/numerics_canary.jtr), which
   exercises the whole numeric stack (bit primitives, serial + parallel reductions,
   binned superaccumulator, parse/format) but prints **only** `print_i32` integers and
   `format_float` strings — *nothing* through `printf("%g")`. Results are pinned by
   their `format_float` value, not bare 0/1 flags, so the canary stays sensitive.
   Locked to `886d1b6aa0d4e57af37763903f34bcaff000fcc06929f07d3a4d031cc92af7e3`.
   (The seven per-demo regression tests still run the readable `print_f64` demos as a
   single-platform sanity check; they're just no longer part of the hashed digest.)

---

## ⚠️ NOT yet proven — what "locking the contract" still needs

These are the gaps. Until #1 is closed, describe this as "flags + construction
locked, **purified** single-platform regression canary," **not** "cross-OS
determinism proven."

1. **The canary digest is single-platform.** It was computed once on Windows/gcc. The
   whole point is cross-OS identity, which is **unverified**. → **Run `cargo test
   --features c-oracle` on Linux and macOS (and ideally clang).** If the digest
   (`886d1b6a…`) matches, the contract is *actually* locked — update this note to say
   so. If it differs now, it is a **genuine** determinism break (the libc-formatting
   false-alarm risk was removed in #2), so triage the numerics, don't just re-lock.
   *This is now the only blocker to proof.*

2. ~~**The canary hashes some `printf`-formatted output — an impurity.**~~ — ✅ **DONE.**
   Was: `binned.jtr`/`reductions.jtr`/`float_bits.jtr` printed floats via `print_f64`
   → `printf("%g")`, whose formatting isn't identical across libc, so a cross-OS
   digest diff *might* have been glibc-vs-msvcrt rather than a real break. Fixed by the
   dedicated [`examples/std/numerics_canary.jtr`](examples/std/numerics_canary.jtr):
   the canary now hashes **only** that demo, which prints solely integers and
   `format_float` strings (our own deterministic, locale-free, correctly-rounded
   formatter) — zero `printf`-rendered floats. A digest diff can now *only* mean a
   genuine determinism break. Re-locked to `886d1b6a…`; mutation-verified (changing one
   parsed value moves the digest *and* trips the token-assert `numerics_canary_demo`).
   (Also fixed a latent temp-file race in `build_and_run` — two tests building the same
   demo concurrently now get uniquely-named `.c`/`.exe`, disjoint by construction.)

3. **The contract holds only through the `jestyrc` build path.** Emit the C
   (`jestyrc emit-c`) and compile it yourself without `CC_FLAGS` → determinism gone.
   There is no guard on manual builds. → Optional: document loudly, and/or emit a
   `#pragma STDC FP_CONTRACT OFF` into the generated C so the invariant rides with the
   file, not just the build command. (Belt-and-suspenders; `-ffp-contract=off` already
   covers the `jestyrc` path.)

4. **32-bit x86 / x87 extended precision is unaddressed.** x86-64 (SSE) is
   deterministic; legacy 32-bit x87 keeps 80-bit intermediates → not bit-identical.
   → Either declare x86-64+ only, or add `-mfpmath=sse -msse2` to `CC_FLAGS` for
   32-bit targets. Low priority unless 32-bit is a target.

---

## Recommended order to *finish* locking it

1. ~~**Purify the canary** (#2)~~ — ✅ **DONE** (this session). Hashed input is now
   `numerics_canary.jtr` (integers + `format_float` only); re-locked to `886d1b6a…`.
   The libc false-alarm risk is gone, so the cross-platform run below is now meaningful.
2. **Verify cross-platform** (#1) — **← the next step, and the only blocker to proof.**
   Run `cargo test --features c-oracle` on Linux (and macOS if available). Same digest
   (`886d1b6a…`) ⇒ the contract is *proven*; record the platforms + compiler versions
   here. A *different* digest now means a real break (not formatting), so triage it.
3. **Wire it into CI** — a matrix (linux/macos/windows × gcc/clang) running
   `cargo test --features c-oracle`. The canary failing on any cell is then a genuine
   determinism regression signal. (Research §3.6 Step 0.)
4. *(Optional)* `#pragma STDC FP_CONTRACT OFF` in emitted C (#3); 32-bit SSE flags (#4).

---

## Pointers

| Thing | Where |
|---|---|
| FP flags + their lock test | `src/main.rs` → `CC_FLAGS`, `mod fp_contract_tests` |
| Canary harness + locked digest | `src/proptests.rs` → `mod c_oracle` (`--features c-oracle`); `numerics_determinism_canary` (hash) + `numerics_canary_demo` (token assert) |
| SHA-256 (dep-free, self-tested) | `src/proptests.rs` → `mod sha256` |
| **Purified canary demo (the hashed input)** | `examples/std/numerics_canary.jtr` — integers + `format_float` only, no `print_f64` |
| Determinism primitives | `examples/std/core.jtr` (`binned_*`, `parse_float`, `format_float`, `par_binned_sum`) |
| Spawn data-race rule | `src/escape.rs` → `check_spawn_no_shared_mut_slice` |
| Readable per-demo regression tests (not hashed) | `examples/std/{binned,reductions,numbers,float_bits,parse_float,format_float,par_reduce}.jtr` |
| Run the canary | `cargo test --features c-oracle` (needs a C compiler on PATH) |

## One-line summary

Flags locked + tested; primitives deterministic by construction + tested; canary
mechanism built, **purified** (hashes integer + `format_float` output only — no
`printf`), and locked **on one platform** (`886d1b6a…`). The only step left to turn
"deterministic" into "proven deterministic": run `cargo test --features c-oracle` on a
second OS/compiler and confirm the digest is identical.
