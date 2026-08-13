# Case 4 — Arena AST

**Status: implemented** (rust/std, rust/idiomatic (typed-arena), jestyr)

## What it tests

A cross-linked node graph: an expression tree with parent back-links,
built bottom-up as a balanced tournament (2^19 leaves → 1,048,575 nodes,
depth 19), folded twice — recursive eval and a weighted path-length scan
that climbs every leaf's parent chain (10M checked hops).

## What each side actually expressed

- **rust-std**: indices into one `Vec<Node>` (`u32` + NIL sentinel). No
  references, no lifetimes — and no protection: a wrong index is
  invisible to the type system. The borrow checker was sidestepped, not
  satisfied.
- **rust-idiomatic**: `typed_arena` with real `&'a Node<'a>` references.
  This is where NAMED LIFETIMES first appear in the suite (`'a` threads
  through the struct and every helper), and back-links force
  `Cell<Option<&'a Node<'a>>>` — interior mutability as the price of
  back-edges among shared references.
- **jestyr**: genref nodes with the niche `@copy enum Link { nil, at(n:
  &Node) }`. Parent back-links are ordinary field writes after
  construction — no Cell concept, because exclusivity is per-call, not
  per-reference. Price: one `gen_new` heap allocation per node and a
  generation check per hop. Lexical `region` allocation cannot carry
  this case (region refs cannot live in struct fields) — that is the
  gap safety-mosaic item 6 exists to close.

## What to look at in the results

The three-way safety story (unchecked indices vs lifetime-proved refs +
Cell vs checked-at-runtime genrefs), the annotation asymmetry (`'a`
count vs `@copy` count), and the allocation-strategy cost: one Vec vs
arena chunks vs a million individual gen_new headers.
