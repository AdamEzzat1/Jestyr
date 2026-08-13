# Methodology

How every number in `results/` is produced. If a metric is not measured the
way this file says, the number is wrong — fix the script, not the doc.

## Twin-program discipline

Inherited from `examples/cpp_compare/`: each case is the same algorithm,
same traversal order, same deterministic input (MINSTD LCG,
`state = state * 48271 % 2147483647`, seeds recorded in the source), and
the tracks must print **byte-identical output**. A timing row for a case
whose outputs diverge is invalid and the harness marks it as such.

Checksums are designed to depend only on *logical* program state (object
values, live/stale outcomes), never on internal slot indices, generation
counters, or allocation addresses — otherwise independently-implemented
registries (hand-rolled arena vs `slotmap` vs genrefs) could never be
byte-compared.

All arithmetic stays within `i64` bounds by construction (bounded operands,
modulus folds). This matters because release-mode Rust wraps silently where
Jestyr's arithmetic is checked; staying in-bounds keeps the two sides
running provably identical computations. `%` follows C truncated semantics
in Rust, C, and Jestyr's lowering alike.

## Tracks

- `rust-std` — Rust standard library only.
- `rust-idiomatic` — common crates where a real Rust user would reach for
  one (`slotmap`, `rayon`, `bumpalo`, `typed-arena`). Only implemented
  where it changes the solution; when std IS idiomatic, the track is
  marked N/A rather than duplicated.
- `jestyr` — current Jestyr on the recorded commit, reference compiler
  (`jestyrc`, Rust) driving gcc on emitted C.

## Runtime

- Build once, then time the produced native executable directly — never
  time through `cargo run` or `jestyrc run`.
- **Interleaved min-of-N** (default N=7 per track, first run of each
  binary discarded as cache warm-up): binaries alternate A,B,A,B,… so a
  background-load spike lands on both sides instead of deciding the
  winner. The minimum approaches the true cost. (Both rules exist because
  their absence produced wrong published numbers in `cpp_compare` —
  see its README's measurement notes.)
- Wall time via PowerShell `Stopwatch` around a full process run; process
  spawn overhead (~few ms) is included equally on all tracks and is noise
  at the 100 ms–1 s workloads used here.
- Single-digit-percent differences are code-layout noise. Do not narrate
  them as wins.

## Compile time

Cold single-crate/file compile, measured as wall time of:

- Rust: `cargo build --release` for the one package after `cargo clean`
  of that package only (`cargo clean -p <pkg>` keeps dependency artifacts,
  so the number is the *user's* crate, not slotmap's).
- Jestyr: `jestyrc build <file>.jtr` (includes the gcc invocation on the
  emitted C — that IS Jestyr's backend, not overhead to be excluded).

Median of 3. These toolchains do different amounts of work (rustc+LLVM vs
jestyrc+gcc); the number contextualizes iteration speed, it does not rank
compiler quality.

## Binary size

Bytes of the final executable as produced by the default release pipeline
above. Caveat recorded once here: Rust links the MSVC toolchain runtime
statically-ish; Jestyr's gcc output links the MinGW C runtime. Sizes are
reported raw, not normalized.

## LOC

Non-blank, non-comment lines of the program source only (harness excluded
on both sides; neither side has one). Comments = lines whose first
non-whitespace is `//` (both languages). Measured by the harness script,
not by eye.

## Annotation count

Hand-counted per case (recorded in `results/latest.md` with the count's
itemization so it can be audited). Counts, per language:

- Rust: lifetime parameters/uses (`'a`), explicit borrows at call sites
  (`&x`, `&mut x`), `mut` bindings required only to satisfy the checker,
  derive/attribute lines needed for the ownership story (`#[derive(Clone,
  Copy)]` on a handle), `.as_ref()`/`.clone()` calls inserted purely to
  appease ownership.
- Jestyr: `read`/`mut`/`take` parameter modes, `var` (vs `let`) bindings,
  ownership-relevant attributes (`@copy`, `@abi(ref)`), explicit
  copies/index round-trips inserted because a borrow cannot be expressed.

The two columns are NOT the same units — the point is "how much ceremony
does the safe version carry", itemized so readers can re-weigh it.

## Unsafe count

`unsafe` blocks/fns (Rust) and `unsafe` blocks / raw-pointer ops (Jestyr)
in the program source. Library internals (slotmap's unsafe, Jestyr std's
unsafe) are NOT counted in the per-program number but noted in prose —
that boundary placement is itself a result.

## Allocation count

Where practical, counted analytically from the program (this suite's
cases allocate a knowable O(1) number of buffers) and stated in the case
notes; no allocator instrumentation in the first pass.

## Peak compiler memory

`scripts/measure_compiler_memory.ps1`: peak working set of the compiler
process, polled via `PeakWorkingSet64` every 5 ms until exit. What each
number covers is NOT symmetrical and both footnotes are load-bearing:

- rustc is invoked directly on the case's `main.rs` at `-O` — one
  process containing the whole compiler including LLVM codegen.
- jestyrc runs `emit-c` — parse/check/lower/emit but NOT gcc, which
  jestyrc forks as a child whose memory the parent's counters do not
  include. The gcc pass's memory is unmeasured.
- Idiomatic-track crates are skipped (direct rustc needs extern
  plumbing that would measure the wrong thing).

Results in `results/compiler_memory.md`. Numbers contextualize the
toolchains' working-set scale; they do not rank compilers.

## Accepted / rejected programs

Rejection cases (a program each language must refuse) are separate files,
compiled with the normal check command; the harness records
accepted/rejected plus the diagnostic text verbatim. Diagnostic quality is
reported as the verbatim message — readers judge, the harness does not
score prose.

## Environment

Recorded in every `results/latest.json`: rustc/cargo versions, Jestyr
branch+commit, gcc version, OS, CPU name, and the input sizes (which are
compile-time constants in the sources).

## Fairness rules

1. Modern stable Rust via rustup; no nightly features.
2. Rust std-only is written the way a competent Rust user writes std-only
   code — never strawmanned into bad style.
3. The idiomatic track may use crates real users would choose; the crate
   choice is named in the case file.
4. Jestyr is not penalized for lacking an ecosystem, but every missing
   expressiveness (a borrow it cannot return, a split it cannot type) is
   documented in the twin's header and in results notes.
5. "Language expressiveness" findings and "current implementation
   performance" findings are reported in separate sections and never
   merged into one claim.
6. Design-only Jestyr comparisons (features from the safety-mosaic design
   docs that are not implemented) are clearly marked DESIGN-ONLY and
   excluded from all measured tables.
7. One benchmark supports one sentence of claim. No extrapolation.
