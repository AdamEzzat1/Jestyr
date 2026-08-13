# Case 8 — Structured Concurrency

**Status: planned** (second pass)

## What it tests

Shared read-only data, exclusive mutation, safe handoff, deterministic
reduction. Rust: `std::thread::scope` borrows without `'static`; the
ecosystem track uses `rayon`. Jestyr: `concurrent`/`spawn`, move-only
channels, and the headline `par for … reduce(r)` with compile-time
rejection of nondeterministic reductions plus `@span` — a checked
guarantee Rust does not have (rayon's float reduce is
schedule-dependent and nothing warns).

## Sketch

Parallel sum-of-squares over 20,000,000 i64 (the `heavy_parsum` shape —
reuse its measured discipline, do not rebuild the machinery), plus a
scoped-borrow phase where threads read a shared table while one region
mutates its own partition. Determinism-check asymmetry is the headline
metric, not throughput.
