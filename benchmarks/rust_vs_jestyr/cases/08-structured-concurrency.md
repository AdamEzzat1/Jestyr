# Case 8 — Structured Concurrency

**Status: implemented** (rust/std (thread::scope), rust/idiomatic
(rayon), jestyr (`par for … reduce`))

## What it tests

Shared read-only data + deterministic parallel reduction: sum of squares
over 20,000,000 i64, cross-checked against a serial pass in-program.

## What each side actually expressed

- **rust-std**: `std::thread::scope` — workers borrow the slice without
  `'static`, fixed 4-way chunking (q = n/4, remainder to the last
  worker), in-order merge matching Jestyr's `core.par_reduce` grouping
  exactly. ~20 lines of chunk/spawn/join.
- **rust-idiomatic**: the rayon one-liner (`par_iter().map().sum()`).
- **jestyr**: `par for x in s reduce(core.sum_reduction()) { x * x }` —
  one line, plus `@span(log)` on the signature. Reuses the fused-worker
  lowering measured in `examples/cpp_compare/heavy_parsum`; nothing
  rebuilt.

## The determinism asymmetry (the case's point)

All three programs are deterministic — but for different reasons. Both
Rust versions are deterministic because i64 `+` HAPPENS to be
associative; change the element to f64 and both silently become
schedule-dependent, and nothing warns. Jestyr's `par for` accepts only
declared-deterministic reductions — the f64 version is a COMPILE ERROR
(the rejection lives in `examples/cpp_compare/static_rejections.jtr`) —
and `@span(log)` additionally makes accidental serialization of the
parallel path a compile error. Neither guarantee has a Rust analogue,
in std or in rayon.
