# The safety mosaic: what to build next, and what it costs

Companion to `docs/escape-guarantee.md`, which states what the checker proves
today. This file is about the gap that remains, and it is deliberately a
*design* document: item 1 of the plan is built, item 4 is the next thing that
could be, and items 2–3 and 5–8 are written down here precisely so that nobody
implements them before the design is argued.

Ranking, rationale and the cost model live in
`docs/handoffs/SAFETY-MOSAIC-AND-FRONTEND-HANDOFF.md` §2. This file is the
design detail that handoff defers to.

---

## The problem, stated once

Jestyr's thesis is second-class borrows: `read`/`mut`/`out` may flow *down* the
stack and never escape the frame. That is provably frame-bounded, needs no
lifetime annotations, and costs ordinary code nothing — a function taking
`read xs: []i32` carries no ceremony at all.

The cost of that simplicity is that **complex aliasing has nowhere to go**. Any
program whose shape is a doubly linked list, a parent pointer, an observer list,
a B-tree, or in-place graph mutation collapses into one of three unsatisfying
outcomes:

1. **rejected** by the escape checker,
2. **re-expressed** with region/genref/index handles, or
3. **dropped into `unsafe`**, where the compiler stops helping entirely.

There is no middle. The goal of everything below is to fill that middle with
small, *named*, opt-in capabilities — not with a general lifetime system.

### The test that keeps this from becoming lifetimes

Rust's lifetimes are *inferred, pervasive and structural*: every reference
carries one whether or not the programmer writes it. Every mechanism below is
**named, local and opt-in** — a projection names its source parameter, a genref
scope is a lexical block, a region token is a value you pass. Ordinary code
never mentions any of them.

> **The check:** if a mechanism starts appearing in signatures that do not need
> it, it has become a lifetime and should be redesigned.

Apply that test to each item before writing code, not after.

---

## What is already true (verify before designing against it)

These are the load-bearing facts about the current implementation. Each was
checked against the code, not recalled:

- **The escape checker flags exactly four routes** — return, capture (into a
  struct literal), store (into borrowed storage), give-away (to a `take`
  parameter). `docs/escape-guarantee.md` argues they are exhaustive.
- **Each route fires only for non-`Copy` values**, and a borrow whose type never
  resolved is now *refused* rather than assumed copyable (item 1, done).
- **Copy-ness decides an outcome in exactly two places** — `escapes_as` and
  `captured_borrow_name`. This is why item 1 turned out to be one predicate
  rather than a taxonomy of positions, and it is the first place to look for any
  future soundness question.
- **Borrowed returns already exist**: `FnSig::ret_conv` is `Read`/`Mut`/`Out`,
  and `-> read T` is accepted as a borrow re-export. What is missing is the
  *source* relationship — the checker knows the return is a borrow, not which
  input it projects from. **Item 2 is a refinement of something present, not a
  new concept.**
- **Slices are built from raw pointers** (`slice(T, ptr, len)`); there is no
  `xs[a..b]` sub-slicing form. Any splitting primitive therefore contains
  pointer arithmetic and needs `unsafe` internally.
- **`spawn` targets cannot be generic**, which is why `par_reduce`/`par_for` are
  i64-only. Any mechanism intended to serve parallelism inherits that limit.
- **A struct value holding a borrow is escape route 2.** So a function *cannot*
  return two borrows in a pair. This single fact shapes item 4's entire design.

---

## Item 4 — disjoint borrowing (`split_mut`) — **the next implementable one**

**Why it is next.** It is the only remaining item ranked implementable rather
than design-first, it may need no port mirror (library-first), and it removes
the most common reason working code reaches for `unsafe` today: giving two
parallel workers disjoint halves of one buffer. `par_binned_sum` does exactly
this with a raw `*mut T` because there is no safe way to say it.

**The design is forced by the escape rule.** A splitting function cannot
*return* its two halves — a pair holding two borrows is escape route 2, capture.
So the halves must be handed *down*:

```jestyr
// Both halves flow DOWN into `f` and never outlive this frame — the second-class
// thesis applied literally, rather than an exception to it.
fn split_mut(mut xs: []i32, at: usize, f: fn(mut []i32, mut []i32))
```

This is continuation passing, and it is not a workaround: it is the shape the
language's own rule selects. A `fn`-pointer type already carries a `Conv` per
parameter (`Ty::Fn { params: Vec<(Conv, Box<Ty>)> }`), so `fn(mut []i32, mut
[]i32)` is expressible today with no syntax change.

**What is unsafe, and what the safety argument is.** Internally the two slices
are built by pointer arithmetic, so the body contains one `unsafe` block. The
obligation it discharges is exactly one sentence, and it is checkable by eye:

> `at <= xs.len`, so `[0, at)` and `[at, xs.len)` are non-overlapping ranges of
> one allocation; therefore the two slices alias no common element.

That is the "unsafe implementation, safe interface" pattern the unsafe contract
already contemplates (`docs/unsafe-contract.md`). The `at <= xs.len` precondition
must be *checked at runtime*, not assumed — otherwise the safe interface is a
lie, and this becomes a way to manufacture overlapping `mut` slices without
writing `unsafe`.

**The design is validated, and it is BLOCKED on a compiler bug.** The shape below
was prototyped and it type-checks and passes the escape checker unchanged —
`mut []T` parameters and fn-pointer types carrying conventions (`fn(read T, read
T) -> bool` already appears in `core.sl_sort`) both exist:

```jestyr
fn split_mut(mut xs: []i64, at: usize, f: fn(mut []i64, mut []i64)) {
    var mid: usize = at
    if mid > xs.len { mid = xs.len }          // the precondition, CHECKED
    let lo: []i64 = unsafe { slice(i64, xs.ptr, mid) }
    let hi: []i64 = unsafe { slice(i64, xs.ptr + mid, xs.len - mid) }
    f(lo, hi)
}
```

But the emitted C does not compile. **`cgen` emits fn-pointer typedefs before the
slice typedefs they reference:**

```c
typedef void (*JestyrFn_fn_mslice_i64_mslice_i64_ret_unit)(JestyrSlice_i64*, …);
typedef struct { int64_t* ptr; size_t len; } JestyrSlice_i64;   /* two lines LATER */
```

The ordering is deliberate for aggregates — `gen_forward_types` forward-declares
structs and enums so a `JestyrFn_…` can name them — but a slice typedef is a
typedef of an *anonymous* struct and cannot be forward-declared, so it must be
*emitted* earlier instead. `slice_struct_defs` participates in the topological
aggregate flush (`def_begin`), so this is not a one-line reorder, and it changes
emitted C, so it owes a `cgen.jtr` mirror and a reseed.

No corpus file hits it: it needs a fn-pointer type whose parameter is a slice,
which nothing currently writes. **That bug is the real prerequisite for item 4**,
and it is a self-contained cgen increment worth doing on its own.

**Staging.**

0. **DONE** — the fn-pointer typedef fix grew into the dependency graph
   (`dep_of_anon_typedef` / `dc_dep_anon`, both toolchains): a fixed order
   cannot work, because struct defs need fn typedefs (vtable fields) while fn
   typedefs need array defs need struct defs (by-value). It also surfaced a
   *silent* sibling: by-value genref/array params in fn typedefs were K&R
   identifier lists — unprototyped pointers, no argument checking.
   `examples/fn_slice_param.jtr` pins all three shapes, and is allowlisted into
   the P5 golden **and** the fixpoint subset (the goldens run an allowlist, not
   a glob — an example the emission gates skip reads as coverage it isn't).
1. **DONE** — `parallel.split_mut`, i64-only like `par_reduce` before it (a
   generic `fn(mut []T, …)` needs monomorphized fn-typedef contributions the
   port does not emit yet). The `unsafe` lives inside the library behind the
   clamp; containment is the checker's ordinary second-class rule.
   `a_split_mut_callback_cannot_leak_its_half` pins the contract from both
   sides; the par_soac demo asserts the boundary pair and whole-parent sum.
   One deliberate deviation from the acceptance line below: an out-of-range
   `at` is **clamped**, not faulted — `mid ≤ len` makes overlap inexpressible
   and the function total (`hi` is simply empty at the boundary), which is
   strictly stronger than a runtime rejection.
2. Parallel: hand the halves to two tasks. This is where the value is, and where
   `spawn`'s non-generic limit bites — expect an i32/i64-only first version.
3. Only if 1–2 prove the shape: consider whether the checker should *know* about
   disjointness rather than trusting the library. That is a much larger change
   and should not be assumed.

**Acceptance (stage 1, met).** Both halves simultaneously writable, writes land
in the right elements (the 16/48 demo pair), overlap inexpressible for any `at`
(by clamp, see above), and the corpus byte-identical through the full gate.

**Do not** generalise this to a `split_at` returning a pair "just for
symmetry" — that is escape route 2 and the checker will (correctly) reject it.

---

## Item 2 — borrowed projections (`-> read T from xs`)

**The gap.** `-> read T` says "the return is a borrow". It does not say *of
what*. So the checker must be conservative about every borrowed return in the
same way, and a caller learns nothing about which argument the result aliases.

**Minimal mechanism.** Name the source parameter in the signature:

```jestyr
fn first(read xs: []i32) -> read i32 from xs
```

`from xs` is a *name*, not an inferred lifetime — it points at a parameter that
is already written in the signature. Ordinary functions never write it; a
function that returns a borrow of one specific input does.

**Why it is "large" despite being one keyword.** It changes what a signature
*means*, so it owes: parser + AST + `FnSig` in both toolchains, a P2 parse
mirror, a P3 signature-rendering mirror (`doc::fn_sig` renders signatures, and
`attest` hashes them — a signature change moves attest hashes), and a seed
refresh. The syntax is small; the two-sided tax is not.

**Design questions to settle before writing code.**

- What does `from` mean for a *method* — is `self` nameable?
- Is `from` checked (the returned expression must actually project from that
  parameter) or merely declared? Checked is the only version worth having, and
  it is most of the work.
- Does the caller gain anything today, or only once item 4/6 exist? If nothing
  consumes the extra precision yet, this is a signature change with no current
  payoff — **that is an argument for deferring it, and it should be answered
  honestly before it is built.**

---

## Item 3 — checked genref scopes — REFERENCE SIDE LANDED; the port ladder remains

**Status (2026-08-12).** The reference implements the full design, both forms:

```jestyr
with alive r as read n { … }                 // stale genref → FAULTS at entry
with alive r as read n { … } else { … }      // stale genref → the else arm
```

One generation check at block entry (the exact test every deref emits:
`((uint64_t*)ptr)[-1] == gen`), then `n` is a plain pointer for the block — no
per-deref checks inside. The design questions resolved: failure is BOTH forms
(`else` optional; absent = fault, mirroring deref semantics); the binding is
`read`-only in v1 (a `mut` variant needs an exclusivity story first); and the
block claims **checked once at entry, nothing more** — nothing in the language
today prevents another holder freeing the object inside the block, and the AST
doc + this file say so plainly. Containment needed no new machinery: the
binding is a second-class borrow, so `return n.s` from inside the block gets
the ordinary "cannot return borrow" refusal — verified, pinned.

Settled while implementing, worth knowing for the port:
- `with` is a full keyword (zero identifier uses corpus-wide); **`alive` is
  contextual** (`examples/life.jtr` uses it as a local).
- **The scrutinee parses at POSTFIX level**, not `parse_expr` and not
  `parse_unary`: the ladder is unary → cast → postfix, and anything at or above
  the cast level eats the construct's `as` (`r as read` → "expected a type").
  A place chain is all a scrutinee can usefully be anyway.
- Lowering: `{ JestyrRef_T _wa<n> = (expr); assert/if(check); __auto_type
  j_<name> = _wa<n>.ptr; body }` — `_wa<n>` consumes one number from the global
  temp counter (LOCKSTEP with the port), and the binding joins `ptr_params` for
  the block so uses deref through it.

**The remaining two-sided tax, in order** (the feature is reference-only until
this is paid; no corpus/golden file uses the syntax yet, deliberately):

1. Port mirror: `tokens.jtr` keyword, `parser.jtr` (new expr kind + dump arm),
   `typeck.jtr` (kind arm: GenRef check + binding), `escape.jtr` (bind-as-
   borrow + walk), `cgen.jtr` (both lowering forms, temp-counter lockstep).
2. P2/P3 golden dump arms on BOTH sides for the new node kind.
3. A corpus example (`examples/with_alive.jtr`), added to the ALLOWLISTED gates
   (`CGEN_GOLDEN_ALLOWLIST` — the recorded trap) only once the port emits it
   byte-identically; runtime output pinned via the c-oracle demo pattern.
4. Grammar doc (`docs/frontend-grammar.md`) + the conformance table row +
   `REFRESH_SEED=1` + the full gate.

## Item 3 — the original design (kept for the record)

**The gap.** A genref (`&T`) is checked at *every* deref, and a stale deref is a
deterministic runtime fault. That is the right default. But code that derefs the
same genref in a loop pays the check every time, and — more importantly — has no
way to say "I have established this is alive; treat it as a plain borrow for the
extent of this block".

**Minimal mechanism.** A lexical block that performs the generation check *once*
and binds a second-class borrow for its extent:

```jestyr
with alive p as read node {
    // `node` is an ordinary `read` borrow here; no per-deref check.
}   // the borrow dies with the block, by the existing frame rule
```

The safety argument is entirely lexical and reuses machinery that exists: the
bound name is a second-class borrow, so the escape checker already refuses to
let it leave the block. Nothing new has to be proved about it — which is what
makes this *medium* rather than *large*.

**Design questions.** What happens on failure — is `with alive` a conditional
(an `else` arm) or a fault? A conditional is more useful and more honest; a
fault is smaller. What prevents the underlying object being freed *inside* the
block by other code holding the same genref? (Answer must be: nothing in the
language today, so the block must not claim more than "checked once at entry" —
and the documentation must say so plainly, or this becomes a mechanism that
*looks* checked and is not, which §2.6 forbids.)

---

## Items 5–8 — design only, with the reason each is not next

### 5. Branded region tokens — the hole is real, its KERNEL is closed, brands stay design-only

**Measured (2026-08-12).** The premise was tested before designing: cross-region
confusion was not hypothetical but a **demonstrated use-after-free that
compiled clean** —

```jestyr
region outer {
    var h: &[outer]Holder = region_alloc(outer, Holder, …)
    region inner {
        h.*.p = region_alloc(inner, str, "gone")   // stored through outer storage
    }
    print_str(h.*.p.*)                              // reads the freed inner arena
}
```

The assign-to-outer rule checked only bare `Name` targets; a store *through a
place chain* rooted outside the region escaped it. **Closed lexically** — the
same depth compare applied to the chain's root — with zero syntax, both
toolchains, pinned as escape route 3 in `examples/region_escape.jtr` (the P4
golden holds both sides to it). Ordinary region code is unchanged *by
construction*, which was this item's design constraint.

**What brands would still buy — the honest residue:** (a) a root ALIASED inside
the region to outer storage (`var alias = h` inside `inner`, then store through
`alias`); (b) a store performed by a *callee* the checker doesn't look into.
Both are now written down as the actual value proposition, replacing the
original hypothesis. Brands remain design-only: they still want to appear in
signatures (the lifetime test), and the residue is narrow enough that the next
step, if any, is taint-tracking the alias case lexically — not a type system.

### 6. Safe mutable graph cells — the worked example EXISTS; design from its data

**The prescribed method was followed** (2026-08-12): the doubly linked list is
written, working, and pinned — `examples/dlist_genref.jtr` (push, traverse,
unlink+free the middle, traverse again; byte-identical across both toolchains
and runtime-pinned). It is the list you CAN write today, and it produced five
concrete data for the design to beat:

1. **No self-referential initialization exists** — a sentinel ring cannot be
   constructed at all (struct init requires every field; there is no two-phase
   form). Every link is therefore `enum Link { nil, at(n: &Node) }` and every
   hop is a `match`.
2. **Enums are never `Copy` and there is no opt-in** — so the niche `Link` over
   a Copy genref is conservatively non-Copy, and pointer surgery through `read`
   params trips escape route 3.
3. **The fix is `take` on genref params** — semantically free (genrefs are
   Copy), syntactically load-bearing, and non-obvious. Ceremony that a cell
   mechanism should erase.
4. **Genref-field WRITES had never been emitted by any program** — the checked
   deref was an rvalue statement expression; the first write found the gap
   (fixed both toolchains: the `emit_place` genref arm, `(*({ …; ptr; }))`).
5. **`break` inside a switch-lowered `match` inside a loop miscompiles** (exits
   the C switch, not the loop — an infinite loop from correct-looking code).
   Tracked as its own fix; the example carries the flag workaround.

Plus the run-cost profile the design must beat: one heap allocation + generation
header per node, a checked deref per hop, per-node frees.

**The design, derived backwards:** what the example actually suffered from was
not checking cost — it was ceremony (1–3) and tooling gaps (4–5). A `Cell[r, T]`
arena would buy: arena locality + one-shot drop (vs per-node malloc/free),
handle = index (no generation header), and — if handles are region-scoped —
zero per-hop checks *inside the region's lexical extent*, the same argument
`with alive` makes for one object generalized to an arena. The open questions
that remain before syntax: what a dangling *index* means after `remove` (an
index is not generation-checked — either handles carry generations again, which
is the genref tier re-invented, or removal invalidates nothing and stale
indices read *wrong-but-live* data, which must be said plainly); and whether
`nil` links stay an enum or the cell type carries a null index. Wants item 7's
linear-capability thinking nearby before committing. Still design-only — but
now design-with-data rather than design-from-scratch.

### 7. Linear capabilities (`linear File`)

A value that must be consumed exactly once — the "you forgot to close it" class
of bug, caught statically. Jestyr already has move semantics and RAII drop, so
the increment is "must be used" rather than "may be moved". **Why not next:**
staged design; the interesting question is what happens on an early `return`,
and the answer interacts with the error-set machinery (`?` propagation) in ways
that need writing down before syntax is chosen.

### 8. Reference capabilities for concurrency

Per-reference capabilities (isolated / immutable / shared) as a concurrency
discipline. **Why not next, and a warning:** this is the item most likely to
turn into a full capability *lattice* that every programmer must learn — the
opposite of the "named tiers, opt-in" design the mosaic is built on. §2.6's
constraint applies with full force. If the design cannot be stated as two or
three named, opt-in capabilities, it should not be built.

---

## Item 9 — the formal mini-model

**Write it alongside items 2–3, not after.** A small operational semantics —
frames, borrows, the four escape routes — with a stated soundness property and a
hand proof for the core rule. No port mirror, no emitted-C change, so it costs
only writing time.

Its real value is not the proof. It is that **a mechanism which cannot be stated
in the mini-model is a mechanism whose rule is not yet clear enough to
implement** — which makes it the cheapest possible filter on items 5–8.

---

## The standing constraints (repeat before every item)

From §2.6, unchanged and non-negotiable:

- No Rust-style lifetime syntax. Ordinary borrows stay second-class.
- Do not weaken the escape checker.
- Do not make raw pointers *look* checked.
- Do not let `Unknown` pass through safety-sensitive code silently. *(Enforced
  as of item 1.)*
- No large mechanism without tests **and** documentation.
- Do not break the byte-identical reference-vs-self-hosted gates.
- If a feature needs mirroring in `examples/std/*.jtr`, either implement the
  mirror or keep the feature design-only. **Do not land it half-mirrored.**

And two lessons this workstream paid for, which apply to every item above:

- **Zero emitted-C change does not imply zero port mirror owed.** The P3 golden
  compares a rendered type for *every* expression against the self-hosted
  typeck. Making an inferred type more precise diverges there even when
  `emit-c` is byte-identical corpus-wide.
- **A rule deliberately silent on the corpus cannot be guarded by the corpus
  goldens.** They would pass with the port missing the rule entirely. Write a
  differential test on inputs that *do* trigger it, and assert they still
  trigger, so a later improvement cannot render it vacuous.
