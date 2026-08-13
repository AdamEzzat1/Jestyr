# Case 4 — Arena AST

**Status: planned** (second pass)

## What it tests

An AST whose nodes cross-link (children + parent links), built in one
phase and traversed in another. Rust std-only reaches for indices into a
`Vec<Node>`; the ecosystem track may use `typed-arena`/`bumpalo` (real
references, arena lifetime) or `slotmap`. Jestyr has lexical `region`
allocation with compile-time escape rejection (see
`examples/cpp_compare/region_arena.jtr` and `static_rejections.jtr`) —
the question is whether region refs can carry the cross-links.

## Sketch

Parse a deterministic arithmetic-expression stream into a tree with parent
pointers, then fold it twice (eval + depth-weighted checksum). Compare:
index plumbing (std) vs arena references (ecosystem) vs region refs
(Jestyr), and where each side's escape/stale protection comes from.
