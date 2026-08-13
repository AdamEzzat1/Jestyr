# Case 9 — Unsafe Boundary

**Status: implemented** (rust/std, jestyr; idiomatic N/A — no crate
changes this)

## What it tests

How small, reviewable, and compiler-enforced the unsafe kernel of a safe
abstraction is: a SIMULATED 64-register block (heap memory standing in
for MMIO — no real hardware) behind a safe read/write API, driven by 10M
LCG-drawn operations.

## What each side actually expressed

- **rust-std**: `RegBlock { base: *mut i64, len }` with exactly two
  `unsafe` blocks, one raw op each, each preceded by the `assert!` that
  justifies it. The fence is compiler-required: the same deref outside
  `unsafe` is E0133 (`rejected.rs`).
- **jestyr**: the same two `unsafe` blocks — and the same enforcement
  class: raw-pointer ops outside `unsafe` are refused by the unsafe
  ladder ("a raw-pointer deref belongs in an `unsafe` block",
  `unsafe_boundary_rejected.jtr`). One genuine difference: the bounds
  precondition is a `requires i < rb.n` CONTRACT on the signature —
  visible at the declaration, compiled to a live assert — where Rust
  buries an `assert!` in the body. Rust's `unsafe fn`/doc-comment
  convention carries the same information humanly, not mechanically.

## What to look at in the results

Unsafe surface: 2 blocks / ~2 lines on both sides, safe callers on both
sides, both languages *require* the fence. This case measured parity of
mechanism — the differentiator is where the precondition lives
(signature contract vs body assert), not the unsafe count.
