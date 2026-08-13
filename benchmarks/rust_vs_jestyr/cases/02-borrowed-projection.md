# Case 2 — Borrowed Projection

**Status: implemented** (rust/std, jestyr — see the gap note)

## What it tests

Returning a reference INTO an argument: `first`, `at(i)`, max-element,
field-of-borrowed-struct. This is the pattern Rust's lifetime system was
built for — `fn first<T>(xs: &[T]) -> &T` — and the first place a
borrow-adjacent language must either match it, copy the element, or fall
back to indices.

## The workload

2,000,000 parser-style `Token { kind, start, len }` records; 2,000,000
LCG-driven lookups through `at`, each dereferencing the returned borrow;
one `first` and one `longest` (max-by-len, first-wins) projection at the
end. Output is four lines, byte-identical across languages.

## What each side must express

- Rust: borrowed returns with elided lifetimes; zero copies.
- Jestyr: whatever the current borrowed-return story is. If a function
  cannot return a borrow into its parameter, the honest fallbacks are
  (a) return the index and re-index at the call site, or (b) return a
  copy of the (24-byte) element. The Jestyr twin documents which it uses
  and why in its header comment.

## What to look at in the results

This is an EXPRESSIVENESS case first: count concepts (lifetimes vs modes),
note the fallback shape, and only then compare runtime — a 24-byte copy vs
a pointer return is usually invisible at -O2, which is itself a finding.
