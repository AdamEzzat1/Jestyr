# Case 1 — Transient Borrowing

**Status: implemented** (rust/std, jestyr)

## What it tests

The bread-and-butter ownership pattern: pass a large aggregate to helper
functions immutably to read it, mutably to update it, with no copies and no
reader ever observing a half-updated structure. Nested access (`world →
player → pos → x`) through both kinds of borrow.

## The workload

1,000,000 `Player` structs (two nested `V3`s plus two scalars) inside a
`World`. 40 ticks: `advance(&mut World)` integrates positions and applies
scheduled damage through a per-player `clamp_hp(&mut Player)` helper;
`total_score(&World)` folds a read-only pass after every tick;
`inspect(&World)` folds the final checksum. Deterministic MINSTD LCG init,
output is three lines, byte-identical across languages.

## What each side must express

- Rust: `&World` / `&mut World` / `&mut Player` — the borrow checker
  guarantees the read pass and the write pass cannot overlap.
- Jestyr: `read` / `mut` parameter modes — same call shapes, checked by
  mut-exclusivity rules rather than lifetimes.

## What to look at in the results

Annotation count (lifetimes needed? none expected on either side), LOC
parity, and whether runtime differs once Jestyr's bounds-checked indexing
meets Rust's iterator-based traversal.
