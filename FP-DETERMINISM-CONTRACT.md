# FP / Determinism Contract — Handoff (lock it in a fresh session)

> **Honest status: the contract is *substantially* locked, not *proven*.** The parts
> that are Jestyr's to control — the FP-codegen flags and the determinism-by-
> construction primitives — are locked and tested. The cross-platform *proof* (the
> SHA-256 canary confirmed identical on a second OS/compiler) is **not done**: the
> locked digest has only ever been computed on one machine (Windows + gcc). Read this
> before claiming "cross-OS determinism." Everything below is on `master` (head
> `463624a`), **508 tests green** (+7 under `--features c-oracle`), warning-clean.
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
3. **The canary *mechanism*.** `cargo test --features c-oracle`
   (`proptests::c_oracle`) compiles + runs the `examples/std` demos through gcc — the
   real `jestyrc run` pipeline — and hashes their combined output with a dep-free,
   self-tested SHA-256 (`proptests::sha256`). Currently locked to
   `dfe9f73512629068c28ea3072eb251555cee6f98b2d141e38a23eef16a95a78e`.

---

## ⚠️ NOT yet proven — what "locking the contract" still needs

These are the gaps. Until they're closed, describe this as "flags + construction
locked, single-platform regression canary," **not** "cross-OS determinism proven."

1. **The canary digest is single-platform.** It was computed once on Windows/gcc. The
   whole point is cross-OS identity, which is **unverified**. → **Run `cargo test
   --features c-oracle` on Linux and macOS (and ideally clang).** If the digest
   matches, the contract is *actually* locked — update this note to say so. If it
   differs, triage (see #2) before re-locking.

2. **The canary hashes some `printf`-formatted output — an impurity.** `binned.jtr`
   and `reductions.jtr` print floats via `print_f64` → the runtime `print_float`
   intrinsic → C `printf`, whose float formatting is **not** guaranteed identical
   across libc. So a cross-OS digest difference *might be glibc-vs-msvcrt, not a real
   Jestyr break* — a false alarm baked in. → **Make the canary hash only (a) integer
   output and (b) `format_float` output (our own deterministic formatter), dropping
   the `print_f64`/printf-rendered values.** Then a digest diff can *only* mean a
   genuine determinism break. (Either change the demos to format via `format_float`,
   or have `c_oracle` filter/normalize, or add a dedicated canary demo that only
   prints integers + `format_float` strings.) Re-lock the digest after this change.

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

1. **Purify the canary** (#2) — change what's hashed to integer + `format_float` only;
   re-lock the digest. Cheap, removes the libc false-alarm risk. Do this first so the
   cross-platform run in step 2 is meaningful.
2. **Verify cross-platform** (#1) — run `cargo test --features c-oracle` on Linux
   (and macOS if available). Same digest ⇒ the contract is *proven*; record the
   platforms + compiler versions here. This is the step that turns "locked" from a
   claim into a fact.
3. **Wire it into CI** — a matrix (linux/macos/windows × gcc/clang) running
   `cargo test --features c-oracle`. The canary failing on any cell is then a genuine
   determinism regression signal. (Research §3.6 Step 0.)
4. *(Optional)* `#pragma STDC FP_CONTRACT OFF` in emitted C (#3); 32-bit SSE flags (#4).

---

## Pointers

| Thing | Where |
|---|---|
| FP flags + their lock test | `src/main.rs` → `CC_FLAGS`, `mod fp_contract_tests` |
| Canary harness + locked digest | `src/proptests.rs` → `mod c_oracle` (`--features c-oracle`) |
| SHA-256 (dep-free, self-tested) | `src/proptests.rs` → `mod sha256` |
| Determinism primitives | `examples/std/core.jtr` (`binned_*`, `parse_float`, `format_float`, `par_binned_sum`) |
| Spawn data-race rule | `src/escape.rs` → `check_spawn_no_shared_mut_slice` |
| Demos the canary runs | `examples/std/{binned,reductions,numbers,float_bits,parse_float,format_float,par_reduce}.jtr` |
| Run the canary | `cargo test --features c-oracle` (needs a C compiler on PATH) |

## One-line summary

Flags locked + tested; primitives deterministic by construction + tested; canary
mechanism built and locked **on one platform**. To truly lock the contract: purify
the canary (drop printf-formatted output), then confirm the digest is identical on a
second OS/compiler. That last step is the difference between "deterministic" and
"proven deterministic."
