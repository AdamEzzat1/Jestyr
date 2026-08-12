# The escape checker's guarantee — a precise statement

This page states, in one place, exactly what the escape checker promises,
how the check works, and what it deliberately does not cover. It is a
precise claim with an informal argument, not a mechanized proof; the
checker is `src/escape.rs` in the reference and `escape.jtr` in the
self-hosted port, and both toolchains enforce the same rules.

## The claim

> **For every program the checker accepts, no second-class borrow is
> usable after the frame that introduced it has returned.**
>
> Equivalently: if a `read`/`mut`/`out` binding `b` refers to storage
> owned by (or borrowed into) a call frame `F`, then every use of `b` —
> and of every binding derived from `b` — occurs within the dynamic
> extent of `F`. Use-after-return through a borrow cannot be expressed
> in checked code.

This is the property that replaces lifetime annotations. Rust proves
"this reference outlives that one" with named lifetimes; Jestyr instead
makes references **second-class** — they flow only *down* the call stack
— so their validity is a structural consequence of the call stack's own
discipline: a callee's frame always ends before its caller's.

## Definitions

* A **borrow** is a parameter with convention `read`, `mut`, or `out`
  (including `read`/`mut`/`out self`). `take` transfers ownership; the
  default convention is `read`.
* Borrow-ness propagates through `let x = <borrow place>` and through
  pattern bindings when matching on a borrowed scrutinee.
* A **borrow place** is an expression naming borrowed storage: a borrow
  binding itself (`p`, `self`) or a projection of one (`p.field`,
  `p[i]`, `p.*`).

## The check

The checker walks every function body and flags exactly four **escape
routes** — the only ways a value can outlive a frame:

1. **return** — a borrow place in return position, when the function
   returns *by value* (not declared `-> read T` / `-> mut T`).
2. **capture** — a borrow place stored as a struct-literal field (the
   struct value may outlive the frame).
3. **store** — assigning a borrow place into borrowed storage
   (`borrowed.field = borrow`): the target's owner may outlive this
   frame.
4. **give-away** — passing a borrow to a `take` (owning) parameter:
   ownership cannot be supplied by a borrow. This is how "store a borrow
   in a collection" (`vec.push(borrow)`) is caught
   (`examples/collection.jtr`).

Two things are **explicitly allowed**, and are the thesis in action:

* passing a borrow as a call argument with a borrow convention — flowing
  *down* is always safe, because the callee's frame is strictly inner;
* returning a borrow when the return convention is itself a borrow
  (`-> read T`) — the signature hands the borrow back to a frame that,
  by induction, received its referent from further up.

One refinement: each route fires only for **non-`Copy`** values. Copying
a borrowed scalar out (`fn f(read p: P) -> i32 { return p.value }`)
escapes a *copy*, not the reference; generic/opaque types are treated as
non-`Copy` (the conservative direction).

### The refinement's one hole, and how it is closed

The `Copy` refinement asks the type a question, so it needs there to *be*
a type. `Ty::Unknown` — inference gave up — is classified `Copy`, on
purpose: expressions the checker could not type must not manufacture
escape errors, or every inference gap would become a cascade of false
positives. That leniency is right for diagnostics and wrong here. At the
two places where `Copy`-ness *decides* an outcome, it silently turns
"we could not type it" into "it is a copy, let it escape" — which is not
leniency but a wrong answer.

So a borrow place whose type never resolved is **refused**, not assumed
copyable:

```
error: cannot decide whether borrow `x` escapes: its type was never resolved
```

This is a *finalization*, not a fifth escape route: it does not claim the
value escapes, only that the question cannot be answered soundly, and it
fires nowhere else — `Unknown` remains lenient everywhere it does not
decide an outcome.

**Behaviour change, and where to expect it.** This refuses some programs
that previously compiled. None are in the 155-file corpus (including the
self-hosted compiler), and no corpus diagnostic moved — but the corpus is
not the whole language, so out-of-corpus code *can* newly fail here. Most
cases found are ill-formed code that had simply never been rejected:

```jestyr
fn f[T](read x: T) -> i32 { return x.v }    // field of an unbounded `T`
fn h(read p: N)    -> i32 { return p.v.w }  // `.w` on an `i32`
```

Both used to compile clean, straight through to code generation, because
neither expression has a type and so neither ever received an escape
verdict — the checker's silence was being read as approval. The right
long-term fix is for the type checker to reject both *at the field
access*, with a message about the field rather than about escape; the
finalization would then stop firing for them. Until that exists, refusing
is the sound behaviour.

The one well-formed shape this gate briefly refused — a generic-struct
ctor-body method returning a field by value — no longer reaches it: those
methods now check with `self` typed as the **real** generic-struct
instance (`Box(T)`, `T` opaque) rather than an opaque `Self`, so
`self.field` resolves through the template. The by-value form is judged by
the same conservative rule as every generic (`T` may be non-`Copy`, so
`fn get(read self) -> T { self.v }` gets the ordinary "declare the return
as `read`" message), and the corpus-wide borrow-return idiom (`-> read T`)
checks cleanly on its merits rather than through a hole.

If you hit this on code you believe is well-formed, that is a
type-inference gap worth reporting — the message names the binding whose
type is missing.

A third shape joined the list the first week the gate existed, caught in
freshly written code rather than by a fuzzer or a probe:

```jestyr
fn read_node(n: &Node) -> i64 { return n.v }   // field through a genref
```

Field projection does not auto-deref a generational reference — the
supported spelling is `n.*.v` — and `n.v` infers no type, so it used to
compile silently. The gate refused it with the message above, which is
exactly the intended behaviour: an inference gap surfacing as a clear
refusal at the definition, not as silence. (Auto-deref for genref field
access, or a targeted "did you mean `n.*.v`?" at the field access, is the
matching long-term fix.)

## Why the argument holds

The four routes are exhaustive over the language's value flows: a value
leaves a frame only by being returned, stored into something that
outlives the frame (a struct value, borrowed storage reaching up the
stack), or surrendered to an owner. Closures capture by the same
struct-capture rule; `spawn` bodies are checked under structured
concurrency, whose scoped join ends every task before the spawning
frame's borrows die. With all four routes refused for borrows, every
borrow's use set is confined to frames below its origin — and those
frames all return first.

## What the claim does *not* cover

The guarantee is scoped to checked code and second-class borrows. The
language's other reference forms make **different, explicit** promises:

| Form | Promise | Enforced by |
|---|---|---|
| `genref` (`&T`) | use-after-free is a **deterministic runtime fault** (generation check on every deref), never UB | emitted code |
| `region` refs (`&[r]T`) | cannot leave their region's scope | compile error (`examples/region_escape.jtr`) |
| raw pointers | nothing — the programmer assumes the written obligations | `unsafe` blocks, required on both toolchains ([unsafe-contract.md](unsafe-contract.md)) |
| `extern "c"` | nothing beyond the C side's own contract | the `unsafe` boundary |

The claim is also about *lifetime* safety, not aliasing races: `mut`
borrows are exclusive by convention (which is what lets the backend emit
`restrict`), and shared-memory concurrency goes through the checked
primitives (`Mutex`, channels) rather than through borrows.

## Evidence at scale

The strongest empirical evidence the discipline is livable: **the
compiler itself** — ~25K lines of flattened Jestyr — passes this checker
with zero escape diagnostics and self-hosts to a byte-identical fixed
point. Rejection demos (`examples/escapes.jtr`,
`examples/collection.jtr`, `examples/region_escape.jtr`) show each route
firing. A mechanized formalization of the claim above is future work;
the statement here is what it would prove.
