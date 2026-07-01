# The Future of Jestyr's Loops

> Closing the breadth gap with Rust **without** spending Jestyr's two advantages over it:
> **transparency** (no hidden adapter machinery, visible cost) and **provability**
> (bounds-check elision, `invariant`, `variant`, `@no_panic`).
>
> _Draft — 2026-06-22. Companion to `docs/loops.md` and `docs/loops-spec.md`._

---

## 1. The trade, stated precisely

Today Jestyr's loops are excellent *within their world* and capped at its edge:

| | Rust | Jestyr (today) |
|---|---|---|
| Iteration domain | **Open-world** — `Iterator` makes *anything* iterable & composable | **Closed-world** — only ranges, slices, byte-strings |
| Expressiveness | loops-as-expressions, pattern bindings, adapter chains | statement-position only, single-name bindings |
| Cost model | **Often hidden** — lazy adapters, `.collect()` allocations, monomorphization blow-up | **Visible** — every form lowers to a C loop you can read |
| Guarantees | optimizer-dependent bounds elision; no termination proofs | **proof-based elision**, `invariant`, `variant`, `@no_panic` |

The clearest one-line framing:

> **Rust = open-world + expressive. Jestyr = closed-world + transparent + provable.**

The goal of this document is **not** "become Rust." It is to take the parts of Rust's
breadth that users genuinely miss — iterating their own types, composing transforms,
yielding values from loops, destructuring as they bind — and deliver them in a form
that *keeps cost visible and keeps the proofs alive*. If a feature can only be had by
hiding cost or by going opaque to the prover, it does not ship in that form.

---

## 2. The invariants we refuse to break

Every proposal below is checked against these. They are the product, not the loops.

1. **No hidden control flow.** Iterating a user type desugars to a loop the user can
   *see* (and the compiler can dump). No lazy thunk graph, no callback soup.
2. **No hidden allocation.** No iterator adapter, and no `for` form, heap-allocates
   unless the user named the destination (an arena, a slice, a builder). `.collect()`'s
   silent `malloc` is exactly what we don't import.
3. **Cost is in the type.** "Does this iterate by borrow or by move? Does it allocate?
   Is it O(1) or O(n) per step?" should be answerable from the signature, like `read`
   vs `mut` already answers ownership.
4. **The prover sees through the abstraction.** Bounds-check elision, `invariant`,
   `variant`, and `@no_panic` must work over a user iterator *exactly* as they work over
   `for i in 0..xs.len`. An abstraction the prover can't see through is a regression.
5. **One way to do a thing.** New iteration power extends the single `for`; it does not
   add a second looping path.

---

## 3. The cornerstone: a *provable, allocation-free* iterator protocol

This is the one feature that moves the completeness score from ~6 to ~9, because it is
what unlocks iterating user-defined types. Everything else is smaller. It is gated on
**traits** (per `HANDOFF`), so it lands after the trait stream — but its *shape* should
be designed now, because the trait design has to carry it.

### 3.1 An explicit protocol, not a magic trait

Rust's `Iterator` is one method, `next() -> Option<Item>`, and everything else (size
hints, fusion, adapters) is convention + optimizer. Jestyr's version is a trait too, but
it is **concrete-by-construction** and **contract-carrying**:

```jestyr
// Sketch — depends on the traits stream.
trait Iter {
    type Item

    // The one required method. `mut self`: advancing is an exclusive borrow of the
    // iterator's *own* state (a stack value), never of the collection behind it.
    fn next(mut self) -> Option(Item)

    // ── Optional, proof-carrying capabilities ──────────────────────────────────
    // Present iff the iterator can honestly provide them. The compiler consumes
    // them to KEEP the guarantees that built-in loops have today.

    // An upper bound on the number of remaining steps. If present, `for x in it`
    // gets an automatic `variant` — termination is proven, and the loop is legal
    // in a `@no_panic` / terminating context.
    fn remaining(read self) -> usize { ... }      // default: absent → "may not terminate"

    // A refinement the loop may assume about the yielded index/element, so that an
    // index derived from this iterator still elides its bounds check.
    fn refine(read self) -> Refinement { ... }    // e.g. "yields i : usize in 0..len"
}
```

The two optional methods are the whole point. A plain Rust iterator is "might yield
forever, prove nothing." A Jestyr iterator can *opt in* to being **bounded** (carries a
termination witness) and **refined** (carries an elision witness). Iterators that can't
prove these simply don't expose them — and then they're correctly *rejected* in
`@no_panic` / terminating positions, with a diagnostic, instead of silently compiling.

### 3.2 Visible desugaring

`for x in it { B }` over a user iterator lowers to *exactly* this, and the compiler can
print it (a `--explain-loop` dump):

```jestyr
// what you wrote                 // what it means (inspectable, no magic)
for x in it {                     var _it = it
    B                             for {
}                                     match _it.next() {
                                          some(x) => { B }
                                          none    => break
                                      }
                                  }
```

If `it` exposes `remaining()`, the lowering additionally injects `variant _it.remaining()`
— so a user-type loop is *proven to terminate* by the same machinery that checks a
hand-written `variant`. If `it` exposes `refine()`, an index pulled from it stays a raw
access. **The borrow rules ride along for free:** `_it` holds the collection's borrow for
the loop, so iterator-invalidation stays a compile error, exactly as for slices today.

### 3.3 Composition without hidden cost — fused, concrete adapters

This is where Rust's breadth lives (`xs.filter(p).map(f).take(k)`) and where its cost
goes dark. Jestyr keeps the chain but pins the cost:

- **Each adapter is a concrete, stack-allocated struct** (`Filter<I, P>`, `Map<I, F>`),
  not a boxed `dyn`. Zero heap. The chain's size is known at compile time.
- **The chain fuses into one loop.** `xs.filter(p).map(f)` is *one* pass with `p` and `f`
  inlined — and the fused loop is what `--explain-loop` shows you. No intermediate
  collections, no per-element indirection.
- **Adapters propagate the contracts.** `map` preserves `remaining()` and (if the
  function is monotone/known) `refine()`; `filter` preserves `remaining()` as an *upper*
  bound (still enough for termination) but drops `refine()`; `take(k)` *tightens*
  `remaining()` to `min(k, …)`. So a chain stays provable as far as it honestly can, and
  the prover knows exactly where a guarantee was lost.
- **Terminal operations that allocate take the destination explicitly.** There is no
  `.collect()` that conjures a `Vec`. You write `into(arena, chain)` or
  `for x in chain { dst[i] = x; … }`. Allocation is a word in the source, never a method
  suffix.

The net: you get `filter`/`map`/`zip`/`take`/`enumerate` ergonomics, but "this allocates"
and "this might not terminate" are both impossible to write by accident.

---

## 4. Loops as expressions — value-yielding `for`

Rust's `let x = loop { … break v };`. Jestyr blocks this today (loops are
statement/return position only), and it's gated on **expression-position control flow**
(the same stream that makes `if`/`match` value sub-expressions).

The transparency-preserving design is small, because **loop-`else` (already shipped) is
the missing half**:

```jestyr
// Search-or-default, as an expression. No sentinel, no found-flag.
let first_even = for x in xs {
    if x % 2 == 0 { break x }     // break carries the found value
} else {
    -1                            // the value when the loop completes (not found)
}
```

- The loop's value is the `break v`'s type; the `else` block supplies the
  ran-to-completion value. The two are unified to one type — concrete, checked, visible.
- An infinite `for { break v }` yields via `break` only (it has no `else` — consistent
  with the rule we already enforce: an infinite loop's `else` is dead code).
- Lowering stays transparent: it's the loop we already emit, with the break value spilled
  into a result temporary — the same shape as `if`-as-expression. No hidden state machine.

This is the highest-ergonomics-per-risk item once expr-position control flow exists,
**and it composes with the iterator protocol** (the `break`/`else` values just flow
through the desugaring in §3.2).

---

## 5. Pattern bindings in the header — `for (x, y) in pairs`

Gated on **destructuring patterns** (which Jestyr also lacks — `let` binds a single
name today). Once patterns exist, this is nearly free and fully transparent:

```jestyr
for (k, v) in entries { … }        // tuple / struct-field destructuring
for Point{ x, y } in points { … }  // nominal destructuring
for x, i in xs { … }               // (today's element+index already prefigures this)
```

- Destructuring is field projection — **zero cost, fully visible**, no effect on elision
  or borrow rules.
- The binding modes reuse the existing `read`/`mut` conventions per sub-binding, so
  `for (read k, mut v) in entries` is well-defined.
- It pairs 1:1 with destructuring `let`; build them in the same stream.

---

## 6. Consuming and parallel iteration (further out)

- **`for take x in xs`** — consuming iteration (each element moved out) needs the
  **owned-iterable** half of the protocol (an iterator that yields by move and leaves the
  collection consumed). Transparent: "this drains the collection" is in the `take`
  keyword. Provable: the escape checker already models move-out; extend it to the
  protocol. Lands with the protocol, not before.
- **`for par x in xs`** — data-parallel map. This is where Jestyr's provability becomes a
  *superpower*, not a tax: the ownership model can prove disjoint writes (race-freedom)
  statically, and — critically for the Motley/numeric story — the reduction must be
  **deterministic** (a fixed, order-independent combine; see the CJC determinism playbook
  in the companion doc). Ship as deterministic map-only, gated on a disjoint-write proof
  and a deterministic-reduce contract. Lowest priority; most valuable to CJC-Lang's
  numerics than to the compiler itself.

---

## 7. The distinctive bet: **provable iterators**

If we do only §3–§6 we've reimplemented Rust iterators with cleaner cost. The thing that
makes this *Jestyr's* and not a port is the contract layer from §3.1 generalized:

> A Jestyr iterator is not just "a thing with `next()`." It can carry, in its type, three
> static witnesses the compiler consumes:
>
> 1. **a bound** (`remaining()`) → termination / `@no_panic` legality,
> 2. **a refinement** (`refine()`) → bounds-check elision survives the abstraction,
> 3. **an effect/purity tag** → eligibility for `par` and for `invariant` reasoning.

No mainstream language ties iteration-over-user-types to *termination and bounds proofs*
this way. Rust can't prove your iterator terminates; Zig's are concrete but carry no
proof; SPARK has the proofs but no open-world iterator protocol. The synthesis —
**open-world iteration that stays provable** — is an actual differentiator, and it falls
out naturally from extending the refinement machinery we already use for range loops.

---

## 8. Sequencing & dependencies

```
traits  ─────────────►  Iter protocol (§3)  ─────►  consuming `take` (§6)
                              │                 └──►  par (§6, + scheduler + det-reduce)
                              └────────────────►  composable adapters (§3.3)

destructuring patterns ─────►  loop pattern bindings (§5)   [parallel track, independent]

expr-position control flow ─►  value-yielding `for` (§4)    [parallel track]
                                   ▲
                          loop-`else` (DONE) ── already the completion-value half
```

- **Nothing here is a loop-syntax problem anymore.** Every item is gated on a bigger
  stream — traits, patterns, or expr-position control flow. The loop work is *done*; the
  loop *breadth* is downstream of those three.
- **Highest leverage:** the **Iter protocol** (it's the whole closed→open-world jump).
  Design it into the traits stream from day one so traits carry the bound/refine/effect
  witnesses, rather than bolting them on later.
- **Cheapest wins:** loop pattern bindings (§5) and value-yielding `for` (§4) — each is a
  thin, transparent layer on top of a capability the language needs anyway.

---

## 9. Anti-goals (what keeps us honest)

We will **not**, in pursuit of breadth:

- ship a `.collect()`-style terminal that allocates without the destination in the source;
- allow `dyn`-boxed/erased iterators in the default path (concrete + fused, or it doesn't
  ship that way);
- accept an iterator abstraction the elision/termination prover can't see through;
- add a second loop keyword or a "fast path / magic path" split;
- hide laziness — an unbounded iterator used where termination matters must carry a bound,
  or be rejected with a diagnostic, not silently compiled.

If a proposed feature forces one of these, the answer is to find the transparent/provable
form of it, or to not ship it — because transparency and provability *are* the reasons to
choose Jestyr over Rust in the first place.
