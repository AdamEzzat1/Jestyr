# Jestyr Loops — Design & Build Spec

> A self-contained build spec for adding loops to Jestyr: a unified `for`. Read
> with [`../HANDOFF.md`](../HANDOFF.md) (implementation state) and
> [`../jestyr-design.md`](../jestyr-design.md) (vision). Status of the MVP is
> tracked in `HANDOFF.md` §7.

## Goal
Add looping as one keyword, `for`, in a small set of header shapes. The loop
binding carries an ownership convention (`read`/`mut`) exactly like a parameter,
the loop **borrows what it iterates** (so iterator invalidation is a compile
error), and a range loop gives **bounds-check elision for free**. Statement-
position only. Reserved `while`/`loop` become helpful errors, not a second path.

**Influences:** Odin (one `for`), Zig (no-magic concrete iteration), Jestyr's own
ownership model (`read`/`mut` binding), Ada/SPARK (`invariant`). Serves the design
principles: "one way to do a thing," "no hidden control flow," "cost/risk visible."

## Syntax (MVP)
```jestyr
for i in 0..n      { … }   // counted range, exclusive
for i in 0..=n     { … }   // counted range, inclusive
for x in xs        { … }   // read iteration over a slice []T (read = default)
for mut x in xs    { … }   // mutable element iteration, in place
for _ in 0..n      { … }   // wildcard binding — "repeat n times"
for _ in xs        { … }   // wildcard binding — visit each, ignore value
for cond           { … }   // conditional (the "while" job)
for                { … }   // infinite; exit via `break`
// inside any body:  break    continue    invariant <bool-expr>
```

## Semantics & ownership contract
1. **The loop borrows what it iterates, for the whole body's scope.** `for x in xs`
   (= `for read x in xs`) holds an immutable borrow → mutating `xs` in the body
   (e.g. `push`) is a **compile error** (iterator invalidation, killed for free).
   `for mut x in xs` holds an exclusive borrow → no other access to `xs` in the
   body. **This is the headline guarantee — claim it.**
2. **Loop bindings cannot escape.** The `for mut x` pointer (any borrow binding)
   may not be stored or returned out of the body; extend the escape checker's routes.
3. **Bounds snapshotted once.** Range `hi` and slice `.len` evaluated once at entry;
   the index range is frozen. (Required for the elision proof and termination.)
4. **Fresh binding per iteration** (so closures and `spawn` capture the right value).
5. **`continue` still advances the index** (lower so the step isn't skipped).
6. **Index type** inferred from the bounds; `0..n` / `0..xs.len` default to `usize`.
7. **Iterate lengthed things only** — ranges and slices (`[]T` carries `.len`). Raw
   `*mut T` has no extent → not iterable. **Strings not iterable yet** (`str` is bare
   `const char*`, no length) — a dependency on future real-strings work.
8. **`invariant` timing:** asserted at the top of the loop, *including before the
   first body run* (holds at entry and after each iteration).
9. **Composition:** `?` early-returns from the function; `break`/`continue` target
   the nearest *loop*; loops nest inside `concurrent`/`region` normally.

## Parsing
After `for`: `{` → infinite. Else parse an optional conv keyword (`read`/`mut`); if
present → iteration binding. Else parse one primary — if it's a `Name`/`_` followed
by `in` → iteration binding; otherwise it (and the rest) is the conditional
expression. Iteration header is `binding in <expr>`; the `<expr>`'s kind/type
(`Range` vs `Slice`) selects lowering. Set `no_struct` while parsing the header so a
trailing `{` opens the body. New AST node; **statement-position only** — diagnose in
value position, like `if`/`match` (gotcha §9).

## Lowering to C
- `for i in lo..hi { B }` → snapshot `hi`, then `for (T i=lo; i<_hi; i++) { B }`
  (`<=` for `..=`); `T` = inferred index type (default `size_t`).
- `for x in xs { B }` → snapshot the slice, then
  `for (size_t _k=0; _k<_s.len; _k++) { T x=_s.ptr[_k]; B }` (bind by value; Copy
  elements for MVP).
- `for mut x in xs { B }` → `… { T* x=&_s.ptr[_k]; B }`, register `x` in `ptr_params`
  so uses render `(*j_x)`. (Reuses the `mut`-param mechanism.)
- `for _ in …` → omit the element binding; keep the internal counter for ranges.
- `for cond { B }` → `while (cond) { B }`. `for { B }` → `for (;;) { B }`.
- `break`/`continue` → C `break`/`continue`. `invariant e` → `assert(e);`.

## Killer feature — wire the bounds-check elision (required)
Extend the refinement proof (`index_in_range`/`cur_refines`) so a range-loop index
`i` from `for i in 0..xs.len` is treated like a value refined `0..xs.len`; then
`xs[i]` in the body emits a **raw access**, no bounds check. (An *inclusive*
`0..=xs.len` index can equal `len`, so it correctly does *not* elide.) The whole
point of range loops — ship it in the MVP.

## Reserved-keyword errors
`while`/`loop` parse to a diagnostic, not a second syntax:
```
error: Jestyr has one loop keyword — write `for <cond> { … }` (not `while`)
                                    or `for { … }` (not `loop`)
```

---

## Fast-follow features (post-MVP — spec'd)

### F1. Element + index — `for x, i in xs`
Element first, index second (Odin). Document loudly; `help:` note if someone writes
`for i, x in …`. Generalizes the binding side to a comma list (see F3 parse rule).

### F2. Region-scoped loop — per-iteration scratch arena *(the unique one)*
```jestyr
for line in lines region scratch {
    var buf: &[scratch]Token = region_alloc(scratch, Token, 256)
    tokenize_into(buf, line)
}   // arena freed ONCE, after the loop
```
**Syntax:** append `region <ident>` (optional `(<cap-expr>)`) after any loop header,
before the body. **Semantics:** one arena created **before** the loop; **reset to
empty (`off=0`) at the top of each iteration** (previous iteration's allocations
reclaimed O(1)); **freed once** after the loop. `region_alloc(scratch, …)` and
`&[scratch]T` work as in a `region scratch { }` block; a `&[scratch]T` is valid only
within its iteration. **Distinct from** `for … { region scratch { … } }`, which
mallocs+frees every iteration — the loop form reuses one buffer (reset = one
assignment). **Lowering:**
```c
{ JestyrArena j_scratch = jestyr_arena_new(CAP);   // once; CAP defaults to 1<<20
  for (…) { j_scratch.off = 0; <body> }            // reset
  jestyr_arena_free(&j_scratch); }                 // once
```
**Risk:** low — reuses the arena runtime. Full lifetime safety (a `&[scratch]T` can't
escape its iteration) rides on the roadmapped region-escape check (HANDOFF §5.23).

### F3. Length-refined lockstep iteration — checked zip
```jestyr
for x, y in xs, ys { sum += x * y }     // requires xs.len == ys.len
for x, mut y in xs, ys { y += x }
```
**Semantics:** **requires `xs.len == ys.len`** — unlike Rust's `zip` (truncates),
Jestyr **rejects** a mismatch. One shared index; bounds-elision on both. **Parse
disambiguation:** header is `binding (, binding)* in source (, source)*` — 2 bindings
+ 1 source → element+index (F1); 2 bindings + 2 sources → zip; N/N (N>2) deferred.
**Lowering:**
```c
{ Slice _sx=xs; Slice _sy=ys; assert(_sx.len==_sy.len);
  for (size_t _k=0; _k<_sx.len; _k++) { Tx x=_sx.ptr[_k]; Ty* y=&_sy.ptr[_k]; <body> } }
```
Runtime `assert` now; static refinement (`requires xs.len==ys.len`, proven by
`@verified`) later. **Risk:** low — reuses contracts + elision.

### F4. `@no_panic` provably check-free loop *(design-reserved)*
Inside a `@no_panic` function (design §13), every potentially-faulting op must be
**proven fault-free or it's a compile error**. For loops: every `xs[i]` must be
provably in range via the range-loop elision, else:
```
error: indexing may fault in a `@no_panic` function
  help: iterate with `for i in 0..xs.len { … }` so the index is provably in range
```
**Honest scope:** `@no_panic` itself is **not built yet** (design §13), so F4 lands
after it. The MVP's elision proof is the enabling half; F4 reserves the design hook.

---

## DONE since the MVP
All fast-follows (F1–F4), plus: **labeled `break`/`continue`** (`for outer: …` → C
`goto`), **`step`/descending ranges** (negative literal step descends; signed index
avoids underflow; no elision when stepped), **`variant` termination measures**
(hoisted `INT64_MAX` tracker + per-iteration `>= 0` and strict-decrease asserts),
**loop-`else`** (`for … { … } else { … }` — runs iff the loop completes without a
`break`; the `else` is emitted after the loop and a `break` becomes a `goto` past
it; rejected on an infinite loop, whose `else` would be dead code), plus **casts**
(`expr as T`) and **byte-level string iteration** (`for c in text`, `text.len` via
`strlen`). User-facing reference: [`loops.md`](loops.md).

## Still DEFERRED
`take`-iteration (slices are borrows — needs an owned-iterable protocol);
value-yielding `for { break v }` / loop-as-expression; destructuring
bindings; custom iterators; **Unicode-aware** string iteration
(`for cp in text.codepoints()` — byte iteration exists, codepoints are future); a
length-carrying `String` type; `par` parallel loops (`// future: par — deterministic
map only, gated on a disjoint-write proof`).

## Scope notes & risk
Everything in the **MVP** reuses an existing mechanism (ptr-params, contract asserts,
refinement proofs, `no_struct`) **except one:** the borrow-contract enforcement
(iterator-invalidation as a compile error) needs the escape checker to model the
loop *holding a borrow over its body* — a conflicting-access check it doesn't do
today. That's the one genuinely new piece and where the risk lives. **Fallback:**
ship the loop with borrow *semantics documented* and *binding-escape enforced*
(reuses existing routes), and land the full mutate-while-iterating rejection as the
immediate fast-follow. Don't let it block the rest.

## Acceptance criteria
1. **`examples/loops.jtr`** exercising every MVP form (`0..n`, `0..=n`, slice read,
   `mut` in-place, `for _`, conditional, infinite-with-break) + `invariant`, with
   documented output.
2. **cgen unit tests** per form: range → `for (… i < …; i++)`; mut-iter →
   `… * j_x = &`; conditional → `while (`; infinite → `for (;;)`; **elision** → raw
   `…ptr[…]` with **no `assert`** for a range index; `invariant` → `assert(`.
3. **Parser tests** for the header shapes, `for _`, and the `while`/`loop` rejection.
4. **Ownership negative tests:** `for x in xs { xs.push(…) }` rejected; an escaping
   `for mut x` binding rejected. (If the §risk fallback is taken, these move to the
   fast-follow.)
5. **Dogfood:** simplify `examples/std/list.jtr` — replace recursive `copy_each` with
   `for i in 0..n { (dst + i).* = (src + i).* }`.
6. Full suite green, build warning-clean, all existing examples byte-identical.

## Gotchas to respect
Statement boundaries are structural — separate statements with `;`; a local must not
share a nullary-variant name; keep `no_struct` discipline in the header; `usize`/
`.len` lower to `size_t`.
