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

## Enforcement — where it stands

The original census: **156 raw-pointer sites, 42 uncovered** (73% already wrapped
voluntarily) — which is what made enforcement a migration rather than a rewrite
(contrast: the `@verified` census found 7 declared obligations, which is why *that*
item is parked). Two classifier corrections then moved the numbers honestly: casts
whose operand the checker cannot type are **not** int-to-ptr (they were `alloc(…) as
*mut T`, provenance-reusing), and `spawn`/`await`/f-string operands were being missed.
Final: **171 sites**.

The steps, and where they stand:

1. ✅ The report + this contract (analysis only, zero emission change).
2. ✅ **The migration — the corpus is at zero uncovered sites.** Four files carried
   them all: `recursion.jtr` (tree derefs), `core.jtr` and `parallel.jtr` (chunk-slice
   construction and disjoint-destination spawn args — the sanctioned pattern, now
   visibly `unsafe`), `sync.jtr` (channel internals: the atomics and the buffer
   slots). One expectation corrected in passing: the wrap is *not* always
   C-invariant — a statement-position `unsafe` emits a scope block — so the full
   golden gate runs, it is not argued away.
3. ✅ **The warning.** `escape::check` now warns on every uncovered site — reusing
   `provenance::collect`, so the report and the warning cannot drift. The port
   mirrors it (`unsafe_boundary` in `escape.jtr`) by a **flat arena scan with span
   containment** rather than by reproducing the reference's walk; both sides sort by
   span start, and that shared sort is what makes two different collection strategies
   emit identical diagnostics. Verified by a dedicated two-sided probe
   (`jestyr_escape_unsafe_warnings_match_reference`) carrying every site kind covered
   and uncovered — necessary because the migrated corpus emits zero warnings and so
   cannot distinguish a working mirror from a missing one.
4. **The error** — still ahead. The corpus is ready (zero uncovered, pinned at zero by
   `unsafe_census_is_total_over_the_corpus`); flipping severity is deliberately left
   as its own increment so the warning gets real-world time first.
