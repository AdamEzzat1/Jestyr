# Case 9 — Unsafe Boundary

**Status: planned** (second pass)

## What it tests

How small, reviewable, and well-fenced the unsafe kernel of a safe
abstraction can be. A bump-allocator wrapper plus a simulated (in-memory,
NOT real hardware) register block: safe callers, unsafe core. Rust:
`unsafe` blocks + invariant comments; count and audit them. Jestyr: the
enforced unsafe ladder (raw pointer ops outside `unsafe` are compile
errors on both toolchains) and contracts around the boundary.

## Sketch

Metrics that matter here: unsafe LOC as a fraction of total, whether the
compiler REQUIRES the fence (Jestyr's ladder vs Rust's `unsafe fn` /
`unsafe {}`), what diagnostics a violating caller gets, and whether safe
misuse of the wrapper is expressible at all on each side.
