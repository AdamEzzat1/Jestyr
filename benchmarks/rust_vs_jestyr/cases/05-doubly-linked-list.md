# Case 5 — Doubly Linked List / Graph

**Status: planned** (second pass)

## What it tests

The intentionally hard case: back-edges. Rust std-only either goes
index-based or pays for `Rc<RefCell<_>>` (runtime borrow flags, leak-able
cycles); the honest unsafe variant is a separate, clearly-marked file if
written at all. The ecosystem track uses a slotmap/arena. Jestyr's
candidates: genref-linked nodes (the `dlist` shape mentioned in the
safety-mosaic notes) and the enum `@copy` niche-Link representation.

## Sketch

Build a 1,000,000-node deque via pushes at both ends, splice and reverse
segments, walk both directions folding a checksum. Score escape hatches
(`unsafe`, `RefCell`, index arithmetic) as first-class results, runtime
second.
