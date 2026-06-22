# Jestyr Loops — What Works Now

> A reference for Jestyr's loop feature **as implemented**, with runnable examples.
> For the design rationale and the roadmap, see [`loops-spec.md`](loops-spec.md).
> Every example here compiles and runs through `jestyrc run`.

Jestyr has **one loop keyword: `for`** (Odin-style — one keyword, several header
shapes). The reserved words `while` and `loop` are errors that point you back to
`for`. Loops are statement-position constructs.

---

## 1. Range loops — `for i in 0..n`

```jestyr
fn sum_to(n: i32) -> i32 {
    var total: i32 = 0
    for i in 0..n { total = total + i }   // 0,1,2,…,n-1
    return total
}
// sum_to(5) == 10
```

- `0..n` is **exclusive** (does not include `n`); `0..=n` is **inclusive**.
- The index type follows the bound's type (here `i32`; `0..xs.len` gives `usize`).
- The upper bound is evaluated **once**, before the loop.

```jestyr
fn sum_to_incl(n: i32) -> i32 {
    var total: i32 = 0
    for i in 0..=n { total = total + i }   // includes n
    return total
}
// sum_to_incl(5) == 15
```

### Free bounds-check elision (the headline)
When you index a slice with a range index proven in-bounds, the bounds check is
**elided** — the access is as fast as unchecked C, with no loss of safety:

```jestyr
fn sum_slice(xs: []i32) -> i32 {
    var total: i32 = 0
    for i in 0..xs.len {
        total = total + xs[i]   // RAW access — no bounds check emitted
    }
    return total
}
```
The compiler proves `i < xs.len` from the loop's range, so `xs[i]` lowers to a plain
`xs.ptr[i]`. (An *inclusive* `0..=xs.len` index could equal `len`, so it correctly
does **not** elide.)

---

## 2. Slice iteration — `for x in xs`

Iterate the elements of a slice directly. The binding carries an ownership
convention, exactly like a function parameter:

```jestyr
// read (the default): each `x` is a copy/borrow of the element
fn sum(xs: []i32) -> i32 {
    var total: i32 = 0
    for x in xs { total = total + x }
    return total
}

// mut: `x` is a mutable handle into the slice — writes land in place
fn double_all(mut xs: []i32) {
    for mut x in xs { x = x * 2 }
}
```

After `double_all`, the original slice is modified (`[1,2,3]` → `[2,4,6]`).

**Ownership guarantee:** a slice-element binding is a *borrow into the slice*, so it
**cannot escape the loop**. Returning it out of a function is a compile error:

```jestyr
struct N { v: i32 }
fn leak(xs: []N) -> N {
    for x in xs { return x }   // error: cannot return borrow `x`
    return xs[0]
}
```
(A `Copy` element like `i32` is duplicated, not referenced, so returning it is fine.)

---

## 3. Wildcard binding — `for _ in …`

When you don't need the element or index, bind `_`:

```jestyr
fn main() -> i32 {
    var count: i32 = 0
    for _ in 0..3 { count = count + 1 }   // just repeat 3 times
    print_int(count)                      // 3
    return 0
}
```

---

## 4. Conditional loop — `for cond` (the "while" job)

```jestyr
fn countdown(n: i32) {
    var k: i32 = n
    for k > 0 {        // no separate `while` keyword
        print_int(k)
        k = k - 1
    }
}
```

Writing `while k > 0 { … }` is an error: *"Jestyr has one loop keyword — write
`for <cond> { … }` (not `while`)."*

---

## 5. Infinite loop — `for {}`

```jestyr
fn first_even(xs: []i32) -> i32 {
    var i: usize = 0
    for {
        if i >= xs.len { break }
        if xs[i] % 2 != 0 { i = i + 1; continue }
        return xs[i]
    }
    return 0 - 1
}
```

Writing `loop { … }` is an error pointing you to `for { … }`.

---

## 6. `break` and `continue`

- `break` exits the nearest enclosing loop.
- `continue` skips to the next iteration (and still advances a range/slice index).
- A `return` inside a loop returns from the **function** (as in `first_even` above).

---

## 7. Loop invariants — `invariant <expr>`

A loop invariant (Ada/SPARK influence) is checked each iteration. Today it lowers to
a debug `assert` (active in debug builds, elided under `-DNDEBUG`); it becomes a
static proof obligation once `@verified` lands.

```jestyr
fn sum_slice(xs: []i32) -> i32 {
    var total: i32 = 0
    for i in 0..xs.len {
        invariant total >= 0   // asserted every iteration
        total = total + xs[i]
    }
    return total
}
```

---

## 8. Iterator invalidation is a compile error

Iterating a collection **borrows** it for the loop body, so mutating that
collection while iterating is rejected at compile time — no runtime surprises:

```jestyr
fn grow(mut xs: []i32, x: i32) {}
fn bad(mut xs: []i32) {
    for e in xs { grow(xs, e) }   // error: cannot mutate `xs` while iterating it
}
fn also_bad(xs: []i32) {
    for x in xs { xs[0] = x }     // error: cannot mutate `xs` while iterating it
}
```
Mutating the *binding* (`for mut x in xs { x = … }`) is fine — that's in-place
element mutation, not structural mutation of the collection.

---

## 9. Element + index — `for x, i in xs`

> ⚠️ **Order: element first, index second.** Jestyr follows Odin (`for value, i`),
> **not** Python/C (`for i, value`). `for x, i in xs` binds `x` to the element and
> `i` to the position (a `usize`). If you write `for i, x in xs` you get the element
> in `i` and the index in `x` — almost certainly not what you meant. The rule is
> consistent everywhere a binding list appears (including the zip form below).

```jestyr
for x, i in xs {
    if i == 0 { print_int(x) }   // x = first element, i = 0
}
```

---

## 10. Lockstep zip — `for x, y in xs, ys`

Iterate several slices in lockstep. Their lengths **must be equal** (checked) —
unlike languages that silently truncate to the shortest:

```jestyr
fn dot(xs: []i32, ys: []i32) -> i32 {
    var d: i32 = 0
    for a, b in xs, ys { d = d + a * b }   // requires xs.len == ys.len
    return d
}
```

---

## 11. Region-scoped scratch arena — `for … region name`

Give each iteration a fresh scratch arena that is reset in O(1) per iteration and
freed once after the loop — zero-cost per-iteration scratch with no leaks:

```jestyr
for line in lines region scratch {
    var buf: &[scratch]Token = region_alloc(scratch, Token, 256)
    tokenize_into(buf, line)
}   // arena freed once, here
```
This reuses one buffer (the reset is a single pointer bump) — strictly better than
allocating and freeing each iteration.

> **The safety rule: a `&[scratch]T` cannot escape its iteration.** Because the
> arena is reset at the top of every iteration, anything allocated from `scratch`
> is only valid *within* that iteration. Storing such a reference into an outer
> collection, returning it, or carrying it to the next iteration is a use-after-
> reset — and is rejected by the region-escape rule (the same lexical rule that
> governs `region` blocks; see HANDOFF §5.23). Treat `scratch` allocations as
> strictly iteration-local.

---

## 12. `@no_panic` loops — proven fault-free

A `@no_panic` function (design §13) must have every potentially-faulting operation
proven safe, or it's a **compile error**. For loops this means indexing must be
provably in range:

```jestyr
@no_panic fn sum(xs: []i32) -> i32 {
    var t: i32 = 0
    for i in 0..xs.len { t = t + xs[i] }   // OK — index proven < len
    return t
}

@no_panic fn at(xs: []i32, i: usize) -> i32 {
    return xs[i]   // error: indexing may fault in a `@no_panic` function
}
```

---

## 13. String iteration — `for c in text` (BYTES, not characters)

> ⚠️ **`for c in text` iterates BYTES (`u8`), not Unicode characters.** Each `c` is
> one byte (`u8`), and `text.len` is the **byte** length (via `strlen`), not a count
> of characters. For ASCII this is the same thing; for any multi-byte UTF-8 text it
> is **not** — a 3-byte `€` yields three iterations. This is the right default for a
> systems language (you usually want the bytes), but do not mistake it for
> character iteration.

```jestyr
fn byte_sum(s: str) -> i32 {
    var t: i32 = 0
    for c in s { t = t + (c as i32) }   // each `c` is a u8 byte
    return t
}
// byte_sum("AB") == 131   (65 + 66)
```

Unicode-aware iteration is **explicitly named future work** and will be a *separate,
opt-in* form so the byte/char distinction is never ambiguous:

```jestyr
// FUTURE (not yet implemented):
for cp in text.codepoints() { … }   // one Unicode scalar (char) per iteration
```

---

## 14. Labeled `break` / `continue` — escaping nested loops

Label a loop with `name:` and target it from an inner loop:

```jestyr
fn first_pair(xs: []i32, target: i32) -> i32 {
    for outer: i in 0..xs.len {
        for j in 0..xs.len {
            if xs[i] + xs[j] == target { break outer }     // exit BOTH loops
            if xs[j] == 0 { continue outer }                // next i, skip rest of j
        }
    }
    return 0 - 1
}
```
`break outer` leaves the labeled loop entirely; `continue outer` jumps to its next
iteration. Plain `break`/`continue` (no label) target the nearest loop. (Lowered to
C `goto` with `<label>__break:` / `<label>__continue:` targets.)

---

## 15. Step and descending ranges — `for i in lo..hi step n`

```jestyr
for i in 0..10 step 2 { … }     // 0, 2, 4, 6, 8
for i in 5..0 step -1 { … }     // 5, 4, 3, 2, 1   (descending)
```
A **negative literal** step descends: the loop compares with `>` instead of `<`, and
the index uses a **signed** type so an unsigned `size_t` can't underflow and run
forever. (A stepped/descending index is not a plain `0..len`, so it does **not** get
bounds-check elision — only a plain exclusive range index does.)

---

## 16. Termination measures — `variant <expr>`

A loop *variant* (Ada/SPARK) is a quantity that must be `>= 0` and **strictly
decrease** every iteration — a machine-checkable termination argument. Pairs
naturally with `invariant`:

```jestyr
fn drain(n: i32) {
    var k: i32 = n
    for k > 0 {
        invariant k <= n     // what stays true
        variant k            // what strictly decreases (proves termination)
        k = k - 1
    }
}
```
Today it lowers to runtime asserts (the value is checked `>= 0` and `< ` its previous
value each iteration); like `invariant`, it becomes a static proof obligation once
`@verified` lands.

---

## 17. Loop `else` — search-or-default

Attach an `else` block to any *finite* loop. It runs **exactly once, iff the loop
finishes without a `break`** (Python's loop-`else`) — the "search, else default"
idiom with no sentinel value and no found-flag:

```jestyr
fn first_even(xs: []i32) -> i32 {
    var ans: i32 = 0
    for x in xs {
        if x % 2 == 0 { ans = x; break }   // hit → break skips the `else`
    } else {
        ans = 0 - 1                         // ran off the end → not found
    }
    return ans
}
```

- A `break` (plain *or* labeled) skips the `else`; running off the end of a
  range/slice — or a `for <cond>` going false — runs it.
- The `else` runs in the **enclosing scope**: the loop bindings are already out of
  scope, and the iterated collection is no longer frozen.
- `return` inside the body leaves the *function*, so it bypasses the `else` too.
- It composes with everything else — labels, `region`, zip, `step`. With a
  `region`, the `else` runs after the loop body but before the arena is freed (so
  it must not touch `scratch` allocations — they are iteration-local).

**An infinite `for { … }` may not have an `else`** — its only exit is `break`, so
the `else` could never run. It is a compile error:

```jestyr
for { if done() { break } } else { cleanup() }
// error: an infinite `for { … }` only exits via `break`, so its `else` can
//        never run — remove the `else`, or give the loop a condition or range
```

(Lowered to C with the `else` block emitted after the loop; a `break` becomes a
`goto` whose target sits *after* the `else`, so it jumps past it.)

---

## Related: casts — `expr as T`

Loops over typed data lean on explicit conversions. `as` performs numeric and
pointer casts (binds tighter than binary operators, chains left):

```jestyr
var u: usize = n as usize
var b: u8    = c as u8
var q: *mut i32 = p as *mut i32
print_int((c as u8) as i32)
```

---

## Full runnable examples

```sh
jestyrc run examples/loops.jtr           # → 10, 15, 10, 14, 2, 3   (the MVP forms)
jestyrc run examples/loops_advanced.jtr  # → 32, 6, 1, 102, 203, 30, 131, 2, 20, 0
jestyrc run examples/loops_else.jtr      # → 4, -1, 7               (loop-`else`)
```
[`examples/loops.jtr`](../examples/loops.jtr) ·
[`examples/loops_advanced.jtr`](../examples/loops_advanced.jtr) ·
[`examples/loops_else.jtr`](../examples/loops_else.jtr)

---

## Not available yet (see [`loops-spec.md`](loops-spec.md) for the plan)

- `take`-iteration (`for take x in xs`) — needs an owned-iterable protocol.
- Value-yielding loops (`let x = for { break v }`).
- Custom iterators (`for x in map.keys()`).
- Unicode-aware string iteration (`for cp in text.codepoints()`).
- `par` parallel loops.
