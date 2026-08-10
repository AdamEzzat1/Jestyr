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

**Staging.**

1. Library-only, sequential: `split_mut` + tests that both halves are writable
   and the writes land in the right elements. No compiler change, no port
   mirror.
2. Parallel: hand the halves to two tasks. This is where the value is, and where
   `spawn`'s non-generic limit bites — expect an i32/i64-only first version.
3. Only if 1–2 prove the shape: consider whether the checker should *know* about
   disjointness rather than trusting the library. That is a much larger change
   and should not be assumed.

**Acceptance.** A test that the two halves are simultaneously writable; a test
that an out-of-range `at` is rejected at runtime rather than producing
overlapping slices; and the corpus unchanged.

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

## Item 3 — checked genref scopes (`with alive p as read node { … }`)

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

### 5. Branded region tokens

Give a region a *type-level brand* so a reference cannot be used with the wrong
region. Value: catches cross-region mistakes at compile time. **Why not next:**
brands are the mechanism most likely to fail the lifetime test above — they want
to appear in every signature that touches a region, which is exactly the
"inferred, pervasive, structural" shape being avoided. Design must show ordinary
region code unchanged before any code is written.

### 6. Safe mutable graph cells (`Cell[r, T]`)

The highest-value item on the list, because it addresses the motivating problem
directly: doubly linked lists, parent pointers, observer lists. A region-scoped
cell with interior mutability and index-like handles could make in-place graph
mutation expressible without `unsafe`. **Why not next:** highest risk, and it
interacts with drop, with regions, and with the concurrency story simultaneously.
Wants worked *examples first* — write the doubly linked list you wish you could
write, then design backwards from it. Do that before proposing syntax.

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
