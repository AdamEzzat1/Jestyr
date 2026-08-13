# Case 3 — Disjoint Mutation

**Status: implemented** (rust/std, jestyr)

## What it tests

Mutating two non-overlapping halves of ONE buffer, plus a
one-writer-one-reader pass (`add_into(dst, src)`) over the same split.
Rust's aliasing rule (no two `&mut` into one slice) forces the split
through `split_at_mut` — a std API that is unsafe inside and safe outside.
The comparison: what does a language whose exclusivity story is different
need for the same program?

## The workload

8,000,000 `i64` (64 MB), 25 rounds of: bump the left half in place, scale
the right half in place, then `add_into(left, right)` — a simultaneous
`&mut left` + `&right` into the same allocation. Rolling checksum plus
three probe elements printed; byte-identical across languages.

## What each side must express

- Rust: `split_at_mut` (std blesses the disjointness with internal
  unsafe), then two independent `&mut [i64]`, then `&mut`/`&` pairing.
- Jestyr: mut-slice arguments with the current exclusivity rules. If
  Jestyr has no safe split-into-two-mut-slices, the twin documents the
  workaround used (index windows over one `mut` slice, or a library
  split) — and whether the aliasing of `add_into`'s two arguments is
  even a concept Jestyr has to defend against.

## What to look at in the results

Whether Jestyr needs the split CONCEPT at all, what (if anything) checks
`add_into`'s disjointness on each side, and the runtime cost of Jestyr's
bounds checks against Rust's iterator-elided ones.
