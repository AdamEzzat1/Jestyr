# The unsafe contract — raw pointers, provenance, and the C boundary (v2, slice 1)

Ownership roadmap **v2**. This page is the *written contract*: what a raw pointer must
satisfy, what the compiler assumes, when `unsafe` is required, and what safe code may
rely on afterwards. The companion tool is `jestyrc unsafe <file>`, which reports every
raw-pointer operation and whether an `unsafe` block covers it.

## The honest starting point

**Today, `unsafe` gates nothing.** A raw deref compiles identically inside and outside
an `unsafe` block; the keyword is documentation the compiler never checks. This page
does not pretend otherwise — the enforcement plan is at the bottom, sized by a
measurement rather than an intention.

What Jestyr already has instead of raw pointers, and what the contract points people
toward first:

| Instead of | Use | Why it's safe |
|---|---|---|
| a stored raw pointer | `genref` | generation-checked on every deref — use-after-free is a deterministic fault, never UB |
| a frame-scoped pointer | `read`/`mut` borrow | provably cannot outlive the frame (the escape checker's theorem) |
| an arena pointer | `region` allocation | scope-bounded by construction; escapes are compile errors |
| a pointer + length | `[]T` slice | bounds-checked (elidable only by refinement proof) |

A raw `*mut T` / `*const T` is for what remains: FFI, MMIO, allocator internals, and
the disjoint-write sharing pattern (`par_binned_sum`).

## The contract

### 1. Validity — what a deref requires

`p.*` (read or write) requires **all** of:

* `p` is non-null and points into an allocation that is **live** — not freed, not out
  of its `region`'s scope, not a dead stack frame;
* the allocation has at least `size_of(T)` bytes at `p`, and `p` is aligned for `T`;
* for a *write*: no `read` borrow of the same memory is live in this frame, and the
  memory is not owned by an immutable (`record`) value.

None of this is checked. Violating any of it is undefined behaviour **in the emitted
C**, which is strictly worse than a Jestyr fault: gcc may reorder, elide, or
miscompile around it. This is the gap between a `genref` deref (checked, faults
deterministically) and a raw one (unchecked, UB) — and the whole reason the safe forms
exist.

### 2. Provenance — where an address may come from

A raw pointer's *provenance* is the allocation it is derived from. The contract:

* **Deriving**: `&x` of an owned value, an allocator return, pointer arithmetic on a
  pointer with provenance — these *carry* provenance. Arithmetic must stay inside the
  allocation (one-past-the-end may be held but not dereferenced), because the emitted
  C inherits C's rule, and gcc optimizes on it.
* **Manufacturing**: an int-to-pointer cast (`0x4000_0000 as *mut u32`) creates an
  address with **no provenance the compiler knows**. This is deliberate — it is the
  MMIO door (`examples/mmio.jtr`) — and it is the operation with the fewest
  guarantees: the compiler assumes such a pointer aliases *nothing* it tracks.
  `@volatile` fields exist precisely because device memory must not be assumed
  ordinary.
* **Pointer-to-pointer casts** (`p as *mut u32`) reuse existing provenance. The new
  type's size/alignment obligations apply from then on. Milder than int-to-ptr, and
  the report counts them differently for that reason.

### 3. Aliasing — what the compiler assumes around unsafe code

The escape checker's model holds *outside* the unsafe operation: a `read` borrow means
the value is not written through other names in this frame; `mut`/`out` means this is
the writing name. Raw-pointer writes inside `unsafe` **must preserve those
assumptions** — an `unsafe` block is permission to do something the checker cannot
verify, not permission to falsify what it already verified elsewhere.

The one sanctioned concurrent-aliasing pattern is **disjoint writes**: tasks may share
a raw `*mut T` provided their write ranges never overlap (`par_binned_sum`). The
spawn checker refuses the *safe* forms of shared mutation exactly so that this hatch is
the only route, and visibly `unsafe`.

### 4. The C interop boundary

* An `extern` function is trusted to match its declared signature — the C side is
  outside every Jestyr guarantee, and a wrong signature is UB at the call.
* Pointers passed to C are assumed **not retained** after the call returns unless the
  binding author says otherwise; a C function that stores the pointer makes every
  later Jestyr free/move a latent use-after-free the checker cannot see.
* Strings: `cstr` is a NUL-terminated borrow; `str` is a `{ptr,len}` view and is NOT
  NUL-terminated — passing `.ptr` of a `str` to a C string function is a bounds bug.

### 5. What safe code may assume after `unsafe`

A well-formed `unsafe` block **restores every invariant it suspends** by its closing
brace: safe code after it may assume, without checking, that borrows still mean what
the checker proved, that no tracked allocation was freed out from under an owner, and
that no checked value was overwritten through a raw alias. That is the definition of a
*sound* unsafe block, and it is the author's obligation — the point of the block is to
make the obligation's extent lexical and reviewable.

## Enforcement plan — sized, not promised

`jestyrc unsafe` over the whole corpus: **156 raw-pointer sites, 42 uncovered** — 73%
of raw-pointer code is already inside `unsafe` voluntarily. So enforcement is a
~40-site migration, not a rewrite (contrast: the `@verified` census found 7 declared
obligations, which is why *that* item is parked).

The steps, each its own increment under the standing gate:

1. ✅ This report + this contract (analysis only, zero emission change).
2. Migrate the ~42 uncovered corpus sites (mostly `examples/std` — the self-hosted
   compiler's own allocator plumbing). Pure `.jtr` churn: seed refresh, no C change
   expected (`unsafe` emits nothing).
3. Turn the uncovered-site report into a **warning** in `check` (with the port mirror
   for the P4 golden in the same increment — warnings are diagnostics, and the golden
   compares them).
4. Turn the warning into an **error**, closing "when is `unsafe` required" with
   "whenever you deref, do arithmetic on, or manufacture a raw pointer".

The census test (`unsafe_census_is_total_over_the_corpus`) pins the uncovered count as
a ratcheting upper bound, so the migration cannot silently grow while unenforced.
