# Case 6 — Observer Registry / Stale Handles

**Status: implemented** (rust/std, rust/idiomatic, jestyr)

## What it tests

Holding handles to objects that get deleted out from under you — the
use-after-free shape, made safe. Every handle ever issued is dereferenced
every round; stale ones must be DETECTED (return none / miss), never
dereferenced into reused memory. This is the case generational indices,
`slotmap`, and Jestyr's genrefs all exist for.

## The workload

100,000 initial objects; 20 rounds of 30,000 delete attempts (victims
drawn from ALL handles ever issued, so some draws are already stale),
15,000 spawns (which reuse freed slots and bump generations), then a full
sweep dereferencing every handle ever issued. The checksum folds live sum,
live count, and stale count — it depends only on the logical deletion
sequence, never on slot numbering, so every implementation's private
slot-reuse policy is invisible. Three output lines, byte-identical.

## What each side must express

- Rust std-only: a hand-rolled generational arena (~45 lines of Registry).
- Rust idiomatic: `slotmap::SlotMap` — the Registry disappears.
- Jestyr: the genref mechanism (or the closest current equivalent); the
  twin records whether stale access is rejected at compile time, misses
  deterministically, or faults deterministically.

## What to look at in the results

Stale-access behavior class (miss vs fault vs UB), how much library each
language needs (std Rust: you write the arena; Jestyr: is it built in?),
and the per-dereference cost of the generation check.
