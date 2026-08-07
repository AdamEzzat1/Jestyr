> **Internal development log** — kept for provenance. Status lines and counts in this file reflect the moment it was written, not the current state. Start at the repo [README](../../README.md) for current status and verified claims.

# Jestyr — Structs, Enums & ADTs: Complete Capability Reference

> A reference for everything Jestyr's bootstrap compiler can do with **product types
> (structs), sum types (enums), and algebraic data types (ADTs)** — including pattern
> matching, layout control, and reflection. Every feature below is implemented, has a
> runnable `examples/*.jtr` demo, and is exercised by the test suite (261 tests, green).
> The design plan in `docs/structs-enums-design.md` (§2.1–§2.8) is **complete**.
>
> Governing principle: **representation is explicit and inspectable; every guarantee is
> static and costs nothing at runtime.** Each feature notes how it lowers to C and what
> (if anything) it costs.
>
> Generated from the Jestyr repo. Compiler internals: `HANDOFF.md`; design rationale:
> `docs/structs-enums-design.md`.

---

## Table of contents

1. [Structs](#1-structs)
2. [Immutable records](#2-immutable-records)
3. [Struct update / spread](#3-struct-update--spread)
4. [Enums (tagged unions)](#4-enums-tagged-unions)
5. [Struct-variant syntax](#5-struct-variant-syntax)
6. [Explicit discriminants](#6-explicit-discriminants)
7. [Niche optimization](#7-niche-optimization)
8. [Generic enums + Option/Result](#8-generic-enums--optionresult)
9. [Recursive ADTs (`indirect`)](#9-recursive-adts-indirect)
10. [`distinct` nominal types](#10-distinct-nominal-types)
11. [Pattern matching](#11-pattern-matching)
12. [Exhaustiveness & redundancy (Maranget)](#12-exhaustiveness--redundancy-maranget)
13. [Error sets `T !E` and `?`](#13-error-sets-t-e-and-)
14. [Layout control & reflection](#14-layout-control--reflection)
15. [Reference tiers for ADT fields](#15-reference-tiers-for-adt-fields)
16. [Feature matrix](#16-feature-matrix)

---

## 1. Structs

A `struct` is a product type with named fields. It may carry methods and be generic.

```jestyr
struct Point { x: i32, y: i32 }

// A method (immutable self).
struct Vec2 {
    x: f64, y: f64
    fn len_sq(read self) -> f64 { return self.x * self.x + self.y * self.y }
}

fn main() -> i32 {
    let p = Point { x: 1, y: 2 }
    print_int(p.x + p.y)          // 3
    return 0
}
```

- **Construction:** `Point { x: 1, y: 2 }` (a C compound literal `(Jestyr_Point){ .j_x = 1, .j_y = 2 }`).
- **Methods:** `read self` (immutable borrow), `mut self` (mutable). Method-call sugar `v.len_sq()`.
- **Generics:** a generic struct is a `comptime` function returning a `type`; instances are
  **monomorphized** per type argument (`List(i32)` → `Jestyr_List__i32`). Methods inside a
  generic struct are monomorphized per instance.
- **Cost:** a struct lowers to a plain C `struct`. Zero overhead.

### Field defaults

A field may declare a `= <expr>` default; a struct literal that omits the field falls back to it.

```jestyr
struct Config {
    retries: i32 = 3,
    timeout: i32 = 0,
    verbose: i32 = 1,
}

fn main() -> i32 {
    let a = Config { }                       // all defaults → (3, 0, 1)
    let b = Config { retries: 5, verbose: 9 } // timeout defaults → (5, 0, 9)
    print_int(a.retries + b.timeout)         // 3
    return 0
}
```

- Omitted fields are filled at the construction site via C designated initializers (so the order
  is free). Non-generic structs; defaults should be constant expressions.
- Demo: `examples/defaults.jtr` → `3, 0, 1, 5, 0, 9`.

---

## 2. Immutable records

A `record` is a `struct` whose fields cannot be reassigned — a static guarantee at **zero
representation cost** (it lowers to the identical C struct).

```jestyr
record Point { x: i32, y: i32
    fn norm_sq(read self) -> i32 { return self.x * self.x + self.y * self.y }
}

fn f(mut p: Point) {
    // p.x = 9        // COMPILE ERROR: cannot assign to a field of immutable record `Point`
}
```

- Field assignment `p.x = …` is a **compile error**; a `mut self`/`out self` method is rejected.
- The *binding* may still be rebound (`var p = …; p = Point{…}`) — only **field** mutation is barred.
- Demo: `examples/records.jtr` → `3, 4, 25`.

### Opt-in `Copy`

By default a user aggregate is **non-Copy**: a `read` (borrowed) parameter can't be returned —
it would outlive its call (you'd need `take` to own it). `@copy` marks a small aggregate as freely
copyable.

```jestyr
@copy struct Vec2 { x: i32, y: i32 }

fn identity(read v: Vec2) -> Vec2 { return v }   // legal only because Vec2 is @copy
```

- Escape-checker concept **only** — the representation is unchanged (structs already pass by value).
- Demo: `examples/copy_optin.jtr` → `3, 7, 20`.

### Per-field visibility

Struct fields are **private to their defining module by default**; `pub` exposes them. Within a
module all fields are freely accessible; only *cross-module* access of a non-`pub` field is rejected.

```jestyr
// module geo
pub struct Point {
    pub x: i32,     // public — readable from other modules
    y: i32,         // private to module `geo`
}
pub fn y_of(read p: Point) -> i32 { return p.y }   // controlled access to the private field
```
```jestyr
// module main
import "geo"
let p = geo.make(3, 7)
print_int(p.x)          // OK — `x` is pub
print_int(geo.y_of(p))  // reach `y` via the accessor
// print_int(p.y)       // ERROR: field `y` is private to module `geo`
```

- Demo (2-file): `examples/visibility/main.jtr` + `geo.jtr` → `3, 7`.

### Untagged `union`

All fields **overlap in storage** (a C `union`), so reading a field reinterprets the same bytes —
the classic use is type-punning a float's bit pattern. `size_of` is the *largest* field, not the sum.

```jestyr
union Bits { i: i32, f: f32 }

var a: Bits = Bits { f: 2.5 }
print_int(a.i)              // 1075838976 — the raw bits of 2.5f
print_int(size_of(Bits))   // 4 — max(field sizes)
```

- Reuses the whole struct frontend; only the emitted C keyword differs (`union` vs `struct`).
- Demo: `examples/union.jtr` → `1075838976, 2.5, 4`.

### Bit-fields

A field can declare a **bit width** (`flags: u8 : 3`), lowering to a C bit-field. Several small
fields pack into one storage unit, so `size_of` shrinks. Composes with `@packed`/`@volatile`.

```jestyr
struct Wide   { a: u8,     b: u8,     mode: u8,    rest: u8    }   // 4 bytes
struct Packed { a: u8 : 1, b: u8 : 1, mode: u8 : 3, rest: u8 : 3 } // 1 byte (8 bits)

print_int(size_of(Wide))     // 4
print_int(size_of(Packed))   // 1
```

- Lowers to `uint8_t j_a : 1; …`; reads/writes like any field.
- Demo: `examples/bitfields.jtr` → `4, 1, 1, 5`.

---

## 3. Struct update / spread

Functional update: copy an existing value, override some fields. Pairs naturally with `record`
(immutable update without mutation).

```jestyr
record Point { x: i32, y: i32 }

fn main() -> i32 {
    let p = Point { x: 1, y: 2 }
    let q = Point { x: 9, ..p }     // override x, carry y from p  → (9, 2)
    let r = Point { y: 20, ..p }    // override y, carry x from p  → (1, 20)
    print_int(q.x + r.y)            // 29
    return 0
}
```

- `..base` must be the last element of the literal.
- **Lowering:** a GNU statement-expression — `({ Jestyr_Point t = p; t.j_x = 9; t; })` — copy
  then assign. Stays an expression; zero hidden cost beyond the copy.
- Demo: `examples/spread.jtr` → `1, 2, 9, 2, 1, 20`.

---

## 4. Enums (tagged unions)

An `enum` is a sum type: a tag plus a union payload. Variants may be nullary or carry **named
fields** (positional by default).

```jestyr
enum Shape {
    circle(r: f64),
    rect(w: f64, h: f64),
    none,
}

fn area(read s: Shape) -> f64 {
    match s {
        circle(r)  => 3.14159 * r * r,
        rect(w, h) => w * h,
        none       => 0.0,
    }
}
```

- **Representation:** `struct { enum tag; union { … } u; }` — explicit and inspectable.
- **Construction:** `circle(2.0)` (positional) or `circle { r: 2.0 }` (named — see §5).
- **`match`** dispatches on the tag; payload fields project onto the pattern's bindings.
- Demo: `examples/shapes.jtr` → `12.5664, 12, 0`.

---

## 5. Struct-variant syntax

Construct and match variants by **named field** — the brace counterpart of the positional form.
Pure ergonomics; identical representation.

```jestyr
enum Shape { circle(r: f64), rect(w: f64, h: f64), dot }

fn area(read s: Shape) -> f64 {
    match s {
        circle { r }  => 3.14159 * r * r,     // named binding
        rect { w, h } => w * h,
        dot           => 0.0,
    }
}

fn main() -> i32 {
    print_float(area(circle { r: 2.0 }))      // named construction → 12.5664
    return 0
}
```

- **Construction:** `circle { r: 2.0 }` → a designated tagged-union initializer.
- **Patterns:** `circle { r }` (shorthand for `r: r`), `rect { w: 0.0, .. }` (omit fields with `..`).
- Demo: `examples/struct_variant.jtr` → `12.5664, 12, 0, 9`.

---

## 6. Explicit discriminants

Pin a variant's integer tag value, and read it back with `as`.

```jestyr
enum Color { red = 1, green = 2, blue = 4 }

fn main() -> i32 {
    print_int(red as i32)              // 1
    print_int((red as i32) | (blue as i32))   // 5  (bit flags)
    return 0
}
```

- `EnumVariant = <expr>` sets the tag constant in the emitted C enum.
- `e as i32` extracts `.tag`. (A niche-optimized enum has no tag — see §7.)
- **Note:** variant names can't be language keywords (use `red`, not `read`).
- Demo: `examples/discriminants.jtr` → `1, 2, 4, 7, 2`.

---

## 7. Niche optimization

The flagship "transparent cost" demo. An enum that is *one nullary variant* + *one single-field
variant whose payload is a **thin pointer*** (`*T` or `&[r]T`) is represented as **just the
pointer** — no tag, no padding.

```jestyr
enum Maybe { none, some(p: *mut i32) }
// size_of(Maybe) == size_of(*mut i32) == 8   (a tagged union would be 16)
// some(p)  →  p
// none     →  the null pointer
// match    →  a NULL test, not a tag switch
```

- The provable fact (a pointer has an unused null bit-pattern) and the cheap fact (no tag needed)
  are the **same fact** — Jestyr's thesis in one optimization.
- A *fat* `&T` (generational `{ptr,gen}`) or `[]T` (slice `{ptr,len}`) correctly falls back to the
  tagged union (no single null niche).
- Generic instances inherit it: `Option(*T)` is a bare pointer too (§8).
- Demo: `examples/niche.jtr` → `8, 42, 0`.

---

## 8. Generic enums + Option/Result

Generic enums use direct `enum Name(T) { … }` syntax and are monomorphized per instantiation.
`Option`/`Result` are **ordinary in-language source**, not hardcoded.

```jestyr
enum Option(T) { none, some(v: T) }
enum Result(T, E) { ok(v: T), err(e: E) }

fn first_positive(read a: i32, read b: i32) -> Option(i32) {
    if a > 0 { return some(a) }
    if b > 0 { return some(b) }
    return none
}
```

- **Type:** `Option(i32)` is monomorphized to a tagged union `Jestyr_Option__i32`.
- **Inference:** a payload variant (`some(5)`) recovers `Option(i32)`; a nullary `none` adopts its
  instantiation from the expected type (`let x: Option(i32) = none`, `return none`, and call args).
- **Niche inheritance:** `Option(*T)` / `Option(&[r]T)` collapse to a bare pointer automatically.
- Demo: `examples/option.jtr` → `42, 7, 5, -3, 8`.

---

## 9. Recursive ADTs (`indirect`)

A self-referential field must sit behind an indirection (to break the size cycle). `indirect T`
is the intent-revealing spelling.

```jestyr
enum Tree { leaf(v: i32), node(left: indirect Tree, right: indirect Tree) }

fn sum(t: Tree) -> i32 {
    match t {
        leaf(v)    => v,
        node(l, r) => sum(l.*) + sum(r.*),   // l, r are pointers — deref to recurse
    }
}
```

- `indirect T` is currently sugar for a raw pointer `*T` (the future hook for tier-aware boxing).
- A **by-value** recursive field (`node(left: Tree, …)`) is a **compile error** — "infinitely
  sized … use `indirect`". That error is what gives `indirect` meaning.
- Demo: `examples/recursion.jtr` → `30, 70`.

---

## 10. `distinct` nominal types

A zero-cost nominal wrapper — same bits as the base, but not interchangeable with it.

```jestyr
distinct UserId = i32
distinct AccountId = i32

fn main() -> i32 {
    let u = 1001 as UserId       // construct with `as`
    // let bad: AccountId = u    // COMPILE ERROR: distinct types need an explicit `as`
    print_int(u as i32)          // 1001  (extract with `as`)
    return 0
}
```

- Lowers to a **zero-cost C typedef** (`typedef int32_t Jestyr_UserId;`). `is_copy` follows the base.
- Convert in/out with `as`. A `let` whose annotation is a distinct type rejects a non-matching
  initializer.
- Demo: `examples/distinct.jtr` → `1001, 42, 7`.

---

## 11. Pattern matching

`match` is exhaustive and supports rich patterns. Below, each pattern kind with an example.

### Bindings & wildcards
```jestyr
match s { circle(r) => r, _ => 0.0 }        // bind r; `_` catches the rest
```

### Guards
```jestyr
match reading {
    temp(c) if c < 0   => freezing(c),       // a boolean the arm is gated on
    temp(c) if c > 100 => boiling(c),
    temp(c)            => c,                  // unguarded fallback still required
    pressure(p)        => p,
}
```
A guarded arm **never counts toward exhaustiveness** (the guard may be false). Two arms may share
a variant, differing only by guard. Demo: `examples/guards.jtr`.

### Literal & range patterns (match on integers)
```jestyr
fn classify(read n: i32) -> i32 {
    match n {
        0          => 0,        // exact literal
        1..=9      => 1,        // inclusive range
        100..1000  => 3,        // half-open range
        _          => 9,        // a scalar match needs a catch-all
    }
}
```
The first time `match` dispatches on a **scalar** (integer / `char` / `bool`), not just an enum.
Demo: `examples/ranges.jtr`.

### Or-patterns
```jestyr
match c {
    red | green | blue => 1,     // covers three variants in one arm
    black              => 0,
}
match n {
    0 | 1 | 2          => 7,      // a set of literals
    10..=19 | 30..=39  => 7,      // a union of ranges
    _                  => 0,
}
```
Each alternative covers independently. Demo: `examples/orpat.jtr`.

### `..` rest in variant patterns
```jestyr
match event {
    click(x, ..) => x,           // bind x; ignore the remaining fields
    quit         => 0,
}
```
Trailing `..` only. Demo: `examples/rest_pat.jtr`.

### Nested patterns
```jestyr
fn shape(read t: Tree) -> i32 {
    match t {
        leaf(_)                => 0,
        node(leaf(_), leaf(_)) => 1,   // looks THROUGH both `indirect` children
        node(_, _)             => 2,
    }
}
```
The backend lowers a nested `match` to a decision tree of recursive tests, auto-dereferencing
`indirect`/pointer fields. Demo: `examples/nested_match.jtr` → `0, 1, 2, 99`.

---

## 12. Exhaustiveness & redundancy (Maranget)

Exhaustiveness is a **soundness proof**, not a syntactic check. It uses Maranget's *usefulness*
algorithm over nested patterns.

```jestyr
// NON-EXHAUSTIVE — caught even though both `leaf` and `node` appear at top level:
match t {
    leaf             => 0,
    node(leaf, leaf) => 1,       // error: node(node(..), ..) is unhandled
}

// REDUNDANT ARM — a warning (non-fatal):
match n {
    0..=9 => 1,
    5     => 2,                   // warning: unreachable, 5 ∈ 0..=9
    _     => 0,
}
```

- **Exhaustiveness** (error): the all-wildcard pattern is *not useful* against the arm matrix —
  this catches **nested** gaps a flat name-set check misses.
- **Redundancy** (warning): an arm not useful against the arms above it is unreachable.
- **Scalar interval coverage:** `true | false` covers `bool` and `0..=255` covers `u8`
  *without* a catch-all.
- Guarded arms are excluded from the analysis. Demo: `examples/exhaustive_check.jtr`.

---

## 13. Error sets `T !E` and `?`

A fallible result type with `?` propagation — a sum type at heart. The error set is written
`T !{ E }`.

```jestyr
fn safe_div(a: i32, b: i32) -> i32 !{ DivByZero } {
    if b == 0 { return err(DivByZero) }
    return ok(a / b)
}

fn add_one(a: i32, b: i32) -> i32 !{ DivByZero } {
    let q = safe_div(a, b)?      // `?` returns early on err, unwraps on ok
    return ok(q + 1)
}

fn main() -> i32 {
    print_int(unwrap(add_one(10, 2)))    // 6
    print_bool(is_err(add_one(10, 0)))   // true  (division by zero propagates)
    return 0
}
```

- `T !{ E }` carries the ok type; construct with `ok(v)` / `err(E)`, inspect with `unwrap`/`is_err`.
- `?` lowers to a statement-expression that returns the error or yields the ok value.
- Demo: `examples/errors.jtr` → `6, true`.

---

## 14. Layout control & reflection

Layout is explicit and inspectable.

### Attributes
```jestyr
@packed struct Tight { a: u8, b: i32, c: u8 }   // no inter-field padding → 6 bytes
@align(16) struct Over { x: i32 }                // forced 16-byte alignment
struct Mmio { @volatile status: u32 }            // volatile field (MMIO)
```
`@packed`/`@align(n)` lower to GNU `__attribute__((packed))`/`((aligned(n)))`. Demo: `examples/layout.jtr`.

### Reflection
```jestyr
size_of(Point)            // → sizeof(Jestyr_Point)
align_of(Point)           // → _Alignof(Jestyr_Point)
offset_of(Point, y)       // → offsetof(Jestyr_Point, j_y)
```
Compile-time intrinsics — a type's memory layout, inspectable in-language. Demo: `examples/reflect.jtr` → `12, 4, 0, 4, 8`.

---

## 15. Reference tiers for ADT fields

A recursive or referenced field carries its **allocation strategy in the type**:

| Tier | Syntax | Cost | Safety |
|---|---|---|---|
| Raw pointer | `*T` | a bare pointer | `unsafe` to deref |
| Region reference | `&[r]T` | a bare pointer (zero-cost) | lexical — can't outlive its region |
| Generational reference | `&T` | fat `{ptr, gen}` | runtime-checked — stale deref faults |
| `indirect` | `indirect T` | currently `*T` | breaks recursion size cycles |

The thin tiers (`*T`, `&[r]T`) enable [niche optimization](#7-niche-optimization); the fat `&T`
falls back to a tagged union.

---

## 16. Feature matrix

| Capability | Syntax | Status | Demo |
|---|---|---|---|
| Struct + methods + generics | `struct P { … }` | ✅ | `compute`, `genmethods` |
| Field defaults | `x: i32 = 0` | ✅ | `defaults` |
| Per-field visibility | `pub x` | ✅ | `visibility/` |
| Untagged union | `union U { … }` | ✅ | `union` |
| Bit-fields | `flags: u8 : 3` | ✅ | `bitfields` |
| Opt-in Copy | `@copy struct P { … }` | ✅ | `copy_optin` |
| Immutable record | `record P { … }` | ✅ | `records` |
| Struct update / spread | `P { x: 9, ..p }` | ✅ | `spread` |
| Enum (tagged union) | `enum E { a(x: T), b }` | ✅ | `shapes` |
| Struct-variant construct/match | `a { x: 1 }` / `a { x }` | ✅ | `struct_variant` |
| Explicit discriminants | `red = 1` · `e as i32` | ✅ | `discriminants` |
| Niche optimization | `{none, some(*T)}` → ptr | ✅ | `niche` |
| Generic enums + Option/Result | `enum Option(T) { … }` | ✅ | `option` |
| Recursive ADT | `indirect T` | ✅ | `recursion` |
| `distinct` types | `distinct Id = i32` | ✅ | `distinct` |
| Match guards | `a(x) if x > 0 =>` | ✅ | `guards` |
| Literal/range patterns | `0`, `1..=9` | ✅ | `ranges` |
| Or-patterns | `a \| b` | ✅ | `orpat` |
| `..` rest | `a(x, ..)` | ✅ | `rest_pat` |
| Nested patterns | `node(leaf(_), _)` | ✅ | `nested_match` |
| Maranget exhaustiveness + redundancy | (analysis) | ✅ | `exhaustive_check` |
| Error sets + `?` | `T !E`, `e?` | ✅ | `errors` |
| Layout attributes | `@packed`, `@align(n)`, `@volatile` | ✅ | `layout`, `mmio` |
| Layout reflection | `size_of`/`align_of`/`offset_of` | ✅ | `reflect` |

### Not yet (cross-feature follow-ups — the §2.x plan itself is complete)
The entire struct/enum/ADT plan (§2.1–§2.8) is done. What remains are smaller follow-ups that
span features rather than belong to one:
- Tier-aware `indirect &[r]T` / `indirect &T` with auto-allocation
- A `: <int-type>` tag-width repr (`enum Color : u8`)
- An *optimal* shared-test decision tree for `match` (today's nested if-chain is correct, not minimal)
- Generic-enum *methods*; an auto-imported `Option`/`Result` prelude

---

*All examples are runnable: `jestyrc run examples/<name>.jtr`. The Jestyr bootstrap compiles
Jestyr source to a native executable through a C backend.*
