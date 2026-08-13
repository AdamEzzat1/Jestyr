# Case 5 — Doubly Linked List / Graph

**Status: implemented** (rust/std, rust/idiomatic (slotmap), jestyr)

## What it tests

The intentionally hard case: back-edges under churn. 200,000 nodes
pushed back, forward sum, delete-every-3rd mid-walk (tail guarded),
backward sum, insert-after-every-5th mid-walk, final sum + count.

## What each side actually expressed

- **rust-std**: slot indices in a `Vec` (`u32` prev/next + NIL). No
  unsafe, no `Rc<RefCell<_>>` — but deleted slots are unlinked, not
  freed (reuse would need a hand-rolled free list), and a stale index
  silently reads whatever occupies the slot. The bidirectional patch is
  easy precisely because nothing checks it.
- **rust-idiomatic**: `slotmap` keys as links. Removal genuinely frees;
  a stale key deterministically MISSES. Generational safety as a crate.
- **jestyr**: genrefs + `@copy` Link — the `dlist_genref.jtr` shape,
  minus the take-ceremony that enum `@copy` (00dfcee) retired: link
  surgery through read-position bindings, removal is unlink + `gen_free`.
  A stale handle touched after `gen_free` FAULTS deterministically —
  the language-level version of what slotmap does as a library, with a
  harder failure mode (fault vs miss) and a stronger guarantee (the
  std twin has neither).

## Escape hatches used

None on any side: no `unsafe`, no `RefCell`, no `Rc`. The std twin's
"escape hatch" is subtler — it escaped into UNCHECKED indices, which
compile clean and verify nothing. That asymmetry (where did the safety
go?) is the case's real result; see ANALYSIS.
