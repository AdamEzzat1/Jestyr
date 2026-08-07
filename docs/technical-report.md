# Jestyr: a self-hosted systems language without lifetimes, held byte-identical across two compiler implementations

**Adam Ezzat** · v0.1.0-research · August 2026

*This is the release's companion report: what was built, what is actually
claimed, and how to check each claim yourself. Every number here is
reproducible from the repository; the commands are in the
[README](../README.md) and [docs/TESTING.md](TESTING.md).*

## Summary

Jestyr is a from-scratch low-level systems language built around one
research question: **how much of Rust-grade memory safety and C-grade
performance survives if lifetimes are removed from the language
entirely?** The prototype answers with a working system rather than a
position paper:

* a ~52K-line Rust reference compiler emitting C;
* a ~25K-line **self-hosted port** — the compiler rewritten in Jestyr —
  whose emitted C is held **byte-identical** to the reference's over a
  148-file corpus, with a verified self-hosting **fixed point** (the
  compiler, compiled by itself, reproduces its own compilation exactly);
* a committed **bootstrap seed**: building the whole toolchain from
  scratch needs only a C compiler — two commands, no Rust;
* a floating-point **determinism contract** with a locked SHA-256
  behavior canary;
* **checked cost models**: performance shape (parallel depth, SIMD
  legality, allocation-freedom, determinism) as compiler-verified
  contracts on function signatures.

The rest of this report walks the contributions in decreasing order of
novelty.

## 1. Dual implementation + byte-identity as a compiler-trust methodology

The project maintains **two independent implementations of the same
compiler** — the Rust reference and the Jestyr port — and pins them to
**byte-identical C output**, enforced continuously over a corpus of 148
programs, over a concatenated build of the compiler's own source, and
through the self-hosting fixed point. On top of that sits a committed
seed (`bootstrap/jestyr_seed.c`, the compiler's own C output for its own
source): anyone with gcc can build the compiler and watch it regenerate
the seed byte-for-byte in about thirty seconds.

This is a practical variation on the *diverse double-compiling* answer to
Thompson's trusting-trust problem (Wheeler, 2009): two implementations
written in different languages, one of them bootstrappable from plain C,
agreeing bit-for-bit on every output. The interesting part is the
engineering discipline that made it sustainable for one person:

* **The gated-on-use rule.** Any feature that changes emission must
  leave every program *not using it* byte-identical — the corpus goldens
  enforce this, so features land without invalidating history.
* **The two-sided tax.** Every emission change must land with its port
  mirror and a refreshed seed in the same increment, or the gates fail.
* **Census-then-enforce migrations.** Before `unsafe` became a hard
  boundary, a census tool counted uncovered raw-pointer sites, the corpus
  was migrated to zero, pinned there, and only then did enforcement turn
  on — so the strictness landed with nothing to break.

Byte-identity is claimed **at its true scope**: it holds on the
single-file/concatenated emission path (which the corpus, the fixed
point, and the seed all use). The module-loader path has three known
divergences (no `#line` directives in the port, per-type artifact order,
spawn symbol naming), themselves pinned by a golden test so they cannot
drift silently.

The byte-identity discipline also proved to be a bug-finding instrument
in its own right: porting the type checker exposed a latent
order-divergence bug in the *reference*, and the port's flat-scan
collection strategy forced the reference's diagnostics into a canonical
order both sides could agree on.

## 2. Ownership without lifetimes, validated at self-hosting scale

Jestyr's answer to memory safety is a **tiered reference model** with no
lifetime annotations anywhere:

1. **Second-class borrows** (`read`/`mut`/`out` parameters) may flow
   *down* the call stack but can never escape their frame — not by
   analysis of annotations, but structurally: the checker refuses the
   four escape routes (return-by-value, capture, store into borrowed
   storage, surrender to an owning parameter). The precise claim and its
   argument are stated in [escape-guarantee.md](escape-guarantee.md).
2. **Generational references** (`genref`) for data structures that need
   stored references: every dereference is generation-checked, so
   use-after-free is a deterministic runtime fault — never undefined
   behavior.
3. **Region arenas** for bulk allocation: region references cannot leave
   their region's scope, enforced at compile time.
4. **Raw pointers** for what remains (FFI, MMIO, allocator internals),
   behind an `unsafe` boundary that is a hard compile error on both
   toolchains, with a written contract.

Hylo (formerly Val) argues second-class references ("subscripts and
projections") suffice for safe systems programming; Jestyr contributes
the missing *empirical* data point: **a 25K-line compiler — lexer to C
backend, with arenas, interners, and a module loader — lives entirely
inside this discipline**, passing its own escape checker with zero
diagnostics. Exclusivity of `mut` borrows also hands the C optimizer
`restrict` semantics; a compute-bound microbenchmark sits at 0.985× the
speed of hand-written C.

## 3. Checked cost models

The most transferable design idea in the language: **performance shape as
a checked interface**, not a hope.

* `@span(log)` declares the function's asymptotic parallel *span*
  (critical-path length, the Cilk/NESL measure). The compiler re-derives
  the body's span from its loop structure — a `par for … reduce`
  contributes `log n`, a sequential loop contributes `n` — and **refuses
  a body that exceeds the declaration**. Serializing a parallel
  reduction is a compile error, not a silent 100× regression.
* `@simd` is a checked legality claim: the compiler verifies every
  `par for` in the body is lane-safe (total, elementwise, integer) and
  vectorizes exactly what it certified; an illegal body is refused with
  the cause named. The realized lane width feeds the determinism canary.
* `@no_alloc` is transitive allocation-freedom, checked through calls.
* `@deterministic` certifies the body — including every parallel
  reduction operator — deterministic at compile time.

Types made data shape part of the checked interface; these make *cost*
part of it. See [attributes.md](attributes.md) §4a for the exact
diagnostics.

## 4. The determinism contract

Bitwise-reproducible floating point as an explicit, tested contract
rather than folklore: compile flags locked (`-ffp-contract=off`,
`-fno-fast-math`) and asserted in tests; parsing and formatting done by
the language's own correctly-rounded routines (never the libc's);
parallel reductions reproducible by construction (binned
superaccumulators, fixed reduction trees). The whole surface is pinned by
a **SHA-256 canary** over a dedicated demo that prints only integers and
own-formatter strings — so a digest change can only mean a genuine
determinism break, not a libc formatting difference. The canary's
cross-platform status is tracked honestly in
[FP-DETERMINISM-CONTRACT.md](../FP-DETERMINISM-CONTRACT.md), and CI runs
it on every push on a second OS.

## 5. Sound error sets with payloads

Errors are declared sets (`-> i32 !{ Io, Parse(i32) }`) checked soundly
through `?`, method calls, and trait dispatch; errors can carry values
with no heap allocation and no dynamic dispatch; `catch |e| match`
extracts payloads exhaustively. Incremental over Zig's error sets (which
are payload-free) — solid engineering rather than novelty, but it
completes the language's "no hidden costs" story: the error type is a
stack value whose size is known at compile time.

## Limitations, stated plainly

* One author, one main development platform (Windows/gcc); Linux
  verification is CI-based; macOS/clang is untested.
* The escape-checker guarantee is a precise informal claim, not a
  mechanized proof.
* The module-loader emission path diverges from byte-identity in the
  three pinned ways above.
* No GPU backend, no async, no SMT-backed `@verified`, no LSP; the
  thermal cost-model facet is design only.
* This is research software: the language surface will change.

## Reproducing the claims

```
gcc -O2 -std=c11 -ffp-contract=off -fno-fast-math -o jc bootstrap/jestyr_seed.c
./jc bootstrap/jestyr_flat.jtr > regen.c && diff regen.c bootstrap/jestyr_seed.c
```

An empty diff is the fixed point. Then `cargo test` (the default suite),
`cargo test --features c-oracle` (the gcc oracle + determinism canary),
and `cargo test --features c-oracle,selfhost-fixpoint` (the corpus
goldens + fixed point + seed guard). Full ladder with timings:
[TESTING.md](TESTING.md).

## References

* D. A. Wheeler, *Fully Countering Trusting Trust through Diverse
  Double-Compiling*, PhD dissertation, George Mason University, 2009.
* K. Thompson, *Reflections on Trusting Trust*, CACM 27(8), 1984.
* The Hylo project (formerly Val) — mutable value semantics and
  second-class references: https://www.hylo-lang.org
* G. E. Blelloch, *Programming Parallel Algorithms* (the work/span
  model), CACM 39(3), 1996.
* Zig's error sets: https://ziglang.org/documentation/ — the payload-free
  baseline §5 extends.
