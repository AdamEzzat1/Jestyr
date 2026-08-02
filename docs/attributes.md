# Jestyr Attributes

> User reference for Jestyr's attribute system (compiler-visible metadata).
> Implementation: [`src/attrs.rs`](../src/attrs.rs) (the registry + validator) and
> [`src/cgen.rs`](../src/cgen.rs) (lowering). Demos:
> [`examples/attributes.jtr`](../examples/attributes.jtr),
> [`examples/tests_demo.jtr`](../examples/tests_demo.jtr).

---

## 1. What attributes are (and are not)

An **attribute** is a declarative directive attached to a declaration with the
`@name` / `@name(args)` syntax:

```jestyr
@inline @hot fn square(x: i32) -> i32 { return x * x }

@packed struct Header { tag: u8, len: i32 }

@deprecated("use parse_v2") fn parse(x: i32) -> i32 { return x }
```

Jestyr deliberately follows **Rust attributes** (compiler-visible metadata),
**Ada/SPARK aspects** (verification intent), and **D/C# metadata** — and
deliberately *avoids* Python-style decorators, which can wrap or replace a
function at runtime. That leads to the governing rule:

> **Attributes may guide compilation, ABI, optimization, verification, and
> tooling — but they must never silently rewrite program behavior.**

So `@inline` changes *how* `square` is compiled, never *what* it returns.
Behavioral specifications that genuinely constrain results — preconditions and
postconditions — stay **real syntax**, not attributes:

```jestyr
fn sqrt(x: i32) -> i32
    requires x >= 0      // a contract — NOT an attribute
    ensures result >= 0
{ … }
```

A second rule follows from Jestyr being a *deterministic* systems language:
**nothing is silently ignored.** A misspelled or misplaced attribute is a hard
compile error, never a no-op — see [§7](#7-validation).

---

## 2. Syntax

| Position | Form | Example |
|---|---|---|
| **Item** (function, struct, const, …) | leading `@name` before the item | `@inline fn f() {}` |
| **Method** (inside a `struct` body) | leading `@name` before the method | `struct S { @inline fn m(self) {} }` |
| **Struct field** | `@name` **after** the field's `:` | `x: @volatile u32` |

Multiple attributes may stack, in any order:

```jestyr
@inline @hot @section(".text.fast") fn kernel(x: i32) -> i32 { … }
```

Argument shapes are fixed per attribute and enforced:

| Shape | Example |
|---|---|
| none | `@inline`, `@packed`, `@must_use` |
| one integer (a power of two) | `@align(8)` |
| one identifier | `@layout(c)` |
| one string | `@section(".boot")` |
| an optional string | `@deprecated` or `@deprecated("use g")` |

---

## 3. Reference

| Attribute | Applies to | Argument | Lowers to | Purpose |
|---|---|---|---|---|
| `@packed` | struct | — | `__attribute__((packed))` | remove inter-field padding |
| `@align(n)` | struct | power-of-2 int | `__attribute__((aligned(n)))` | force a minimum alignment |
| `@layout(c)` | struct | identifier | (marker) | C field order — the default |
| `@layout(auto)` | struct | identifier | reordered declaration | let the compiler minimise padding |
| `@abi(value)` | fn | identifier | (marker) | large `read` aggregates are copied — the default |
| `@abi(ref)` | fn | identifier | `const T*` parameter | …passed by reference instead |
| `@volatile` | field | — | `volatile` qualifier | MMIO — never cache the field |
| `@no_panic` | fn / method | — | (static check) | every faulting op must be provably safe |
| `@inline` | fn / method | — | `static inline __attribute__((always_inline))` | force inlining |
| `@no_inline` | fn / method | — | `__attribute__((noinline))` | forbid inlining |
| `@hot` | fn / method | — | `__attribute__((hot))` | optimize for the fast path |
| `@cold` | fn / method | — | `__attribute__((cold))` | deprioritize (error/slow paths) |
| `@must_use` | fn / method | — | `__attribute__((warn_unused_result))` | warn if the result is ignored |
| `@deprecated` | fn / method | opt. string | `__attribute__((deprecated("…")))` | warn at call sites |
| `@no_mangle` | fn / const | — | bare C symbol (no `jestyr_`) | export to C / the linker |
| `@section(s)` | fn / method / const | string | `__attribute__((section(s)))` | place in a named linker section |
| `@test` | fn | — | (test harness) | a unit test (`jestyrc test`) |
| `@bench` | fn | — | (test harness) | a timed benchmark (`jestyrc test`) |

Three names are **reserved** — recognized, but using them is an error until the
backing feature lands (rather than silently doing nothing):
`@verified` (SMT verification), `@doc_hidden` (the doc generator). See
[§8](#8-reserved-attributes).

---

## 4. Optimization & safety

### `@inline` / `@no_inline`

```jestyr
@inline fn square(x: i32) -> i32 { return x * x }
```
```c
static inline __attribute__((always_inline)) int32_t jestyr_square(int32_t j_x) { … }
```

The `static` is deliberate: a non-`static` `inline` function in C11 supplies only
an *inline definition*, and if the optimizer declines to inline it the linker may
not find an external symbol. `static inline` sidesteps that — which is also why
`@inline` **conflicts with `@no_mangle`** (the latter needs an external symbol).

### `@hot` / `@cold`

Pure branch/placement hints. `@cold` is ideal for error handlers and slow paths;
the optimizer will move them out of line and assume they run rarely.

```jestyr
@cold fn report_oom() { … }
```

### `@no_panic`

A *static guarantee*, not a C attribute: in a `@no_panic` function every
operation that could fault (today: slice indexing) must be **provably** in range,
or it is a compile error. Pair it with a range-`for` so the index is proven safe:

```jestyr
@no_panic fn sum(xs: []i32) -> i32 {
    var t: i32 = 0
    for i in 0..xs.len { t = t + xs[i] }   // i is provably < xs.len → no bounds check
    return t
}
```

---

## 5. Tooling & API hygiene

### `@must_use`

```jestyr
@must_use fn checked_add(a: i32, b: i32) -> i32 { return a + b }

fn main() {
    checked_add(2, 3)            // warning: ignoring return value … [-Wunused-result]
    print_int(checked_add(2, 3)) // fine — result consumed
}
```

Especially valuable on fallible functions (`-> T !{ … }`): it pushes callers to
actually inspect the result instead of dropping a possible error.

### `@deprecated`

```jestyr
@deprecated("use parse_v2") fn parse(x: i32) -> i32 { return x }
```

Every call site of `parse` then draws a real compiler warning that quotes your
message:

```
warning: 'jestyr_parse' is deprecated: use parse_v2 [-Wdeprecated-declarations]
```

The message is optional (`@deprecated` alone works).

---

## 6. ABI & bare-metal

### `@no_mangle` — exporting to C

Jestyr normally emits functions as `jestyr_<name>` and consts as
`static const j_<name>` (collision-free, internal). `@no_mangle` strips that:
the symbol is the bare name, with external linkage — the **export** counterpart
to `extern "c"`'s **import**.

```jestyr
@no_mangle fn jestyr_add(a: i32, b: i32) -> i32 { return a + b }
@no_mangle const VERSION: i32 = 7
```
```c
int32_t jestyr_add(int32_t j_a, int32_t j_b) { … }   // callable from C as `jestyr_add`
const int32_t VERSION = 7;                            // external symbol `VERSION`
```

Restrictions (enforced):
- **not on generics** — a generic function has one symbol *per instantiation*, so
  there is no single unmangled name;
- **not combined with `@inline`** (internal vs. external linkage);
- on `main` it is a harmless no-op (the entry wrapper already exports `main`).

> ⚠️ A local variable that shadows a `@no_mangle` const's name will mis-resolve
> to the const. Top-level names are globally unique by design, so choose exported
> names that won't collide with locals.

### `@section(".name")`

Place a function or global in a named linker section — boot code, a special RAM
region, a custom segment:

```jestyr
@section(".text.boot") fn _reset() { … }
@section(".rodata.cfg") const CFG: i32 = 0x55
```
```c
__attribute__((section(".text.boot"))) void jestyr__reset(void) { … }
static const int32_t j_CFG __attribute__((section(".rodata.cfg"))) = 85;
```

### `@volatile`, `@packed`, `@align(n)` — layout

```jestyr
@packed struct Packet { kind: u8, length: i32, flags: u8 }   // no padding
@align(64) struct CacheLine { data: i64 }                    // 64-byte aligned

struct Uart { @volatile status: u32, @volatile data: u32 }   // MMIO registers
```

`@align(n)` requires a positive power of two — `@align(3)` is rejected by Jestyr
(with a clear message) rather than deferred to a confusing C error.

### `@layout(auto)` — let the compiler choose the field order

C lays a struct out in the order you wrote it, padding before every field that needs a
stricter alignment than the running offset happens to have. That is a promise worth
keeping when a struct crosses an FFI boundary or maps hardware, and pure cost when it
does not.

```jestyr
struct Wasteful { a: u8, b: u64, c: u8, d: i32, e: u16 }               // 32 bytes
@layout(auto) struct Tidy { a: u8, b: u64, c: u8, d: i32, e: u16 }     // 16 bytes
```

The compiler emits an annotated struct's fields in **descending alignment**, which
leaves no interior padding at all — every offset is then a sum of sizes that are already
multiples of alignments at least as strict as the next field's. Only tail padding
remains, and no ordering can remove that.

Nothing else about the program changes. A struct is constructed by field name
(`Tidy { a: 1, … }` lowers to a C designated initializer) and read by field name, so
reordering the *storage* is invisible to the code that uses it — which is exactly why
the compiler is allowed to do it. `size_of`/`offset_of` lower to C's own
`sizeof`/`offsetof`, so they report the new layout without being taught about it, and
their comptime twins `@size_of`/`@offset_of` compute it from the same model — so a
folded constant and a C expression cannot disagree about where a field is.

It is **opt-in per struct**: nothing you did not annotate moves a byte. And the
annotation is **checked** rather than advisory — each of these is a compile error, not a
silent no-op, because in each the byte order is already load-bearing:

| Refused | Why |
|---|---|
| `@layout(auto)` on a `union` | every member starts at offset 0, and C treats the first member as the initially-active one |
| `@layout(auto)` with `@packed` | `@packed` promises the layout you wrote; `@layout(auto)` says the compiler may choose it |
| `@layout(auto)` with bit-fields | bit-field packing is implementation-defined in C, so the compiler cannot compute the layout it would be improving |
| `@layout(other)` | the vocabulary is closed: `c` or `auto` |

`@align(n)` composes fine — a forced alignment says nothing about field order.

`jestyrc layout <file>` reports what a struct costs today: size, alignment, every field
offset, and the padding waste. A reordered struct's fields are listed in **emission**
order and the record is marked `(reordered)`, so the report always describes the bytes
the backend actually produces. See [`examples/layout_auto.jtr`](../examples/layout_auto.jtr).

### `@abi(ref)` — stop copying large read-only parameters

`read` says a parameter is borrowed and will not be mutated. Physically, though, it has
always been a **copy**: a 64-byte record crossing a call boundary is 64 bytes of memcpy,
at every call.

```jestyr
@abi(ref) fn checksum(read frame: Frame) -> i64 { … }
```

Now it crosses as one machine word. The compiler emits `const Frame*` and dereferences at
every use; the `const` is not decoration, it is the C-level statement of what `read`
already promised, so the C compiler enforces the read-only half rather than trusting it.

**Which parameters change, and which deliberately do not.** Only `read` (or default-borrow)
parameters whose type is an **aggregate larger than two machine words**. `mut`/`out`
already pass a pointer; `take` is an ownership transfer whose copy is the point; a scalar
or a small struct is already one or two registers, and a pointer to it would be *slower*
plus an indirection at every use — an ABI attribute that pessimized the small cases would
be a bad attribute. A parameter whose size the compiler cannot know (a generic instance)
is left by value rather than guessed at.

**It is an ABI change, not a hint**, so every call has to be compiled against the same
convention. That holds for direct calls, which the compiler resolves by name — and fails
for indirect ones, because a `fn(T) -> R` pointer type carries no convention:

```jestyr
let f: fn(Frame) -> i64 = checksum    // error: `@abi(ref)` function has its address taken
```

That is refused rather than left to C, which would compile the mismatch without a word
and read a struct as an address. Generic functions are refused too (their parameter types
are not known until instantiation), and for now so are **methods** — a method reaches its
callee through method sugar, bound values, trait vtables and `dyn` fat pointers, and until
all of those agree on the convention, refusing is better than emitting a signature some
call sites do not match.

---

## 7. Validation

The registry in [`src/attrs.rs`](../src/attrs.rs) is the single source of truth.
Every attribute is checked at parse time; problems are **errors**:

| Mistake | Example | Diagnostic |
|---|---|---|
| unknown name | `@inlien fn f() {}` | `unknown attribute @inlien` + *did you mean `@inline`?* |
| wrong target | `@packed fn f() {}` | `@packed cannot be applied to a function` (`applies to a struct`) |
| bad argument | `@align(3) struct S {…}` | `@align expects a positive power of two` |
| wrong arg type | `@section(7) fn f() {}` | `@section expects a single string` |
| duplicate | `@inline @inline fn f() {}` | `duplicate attribute @inline` |
| conflict | `@hot @cold fn f() {}` | `conflicting attributes @hot and @cold` |
| reserved | `@verified fn f() {}` | `@verified is reserved and not implemented yet` |
| unknown policy | `@layout(packd) struct S {…}` | `unknown layout policy packd` (`expected c or auto`) |
| meaningless promise | `@packed @layout(auto) struct S {…}` | `@layout(auto) conflicts with @packed` |

The "did you mean" suggestion uses an edit-distance match against the known set,
so typos point straight at the intended attribute.

---

## 8. Testing & benchmarking — `jestyrc test`

`@test` and `@bench` turn the "every feature ships with a demo + tests"
discipline into a first-class workflow.

```jestyr
// examples/tests_demo.jtr
fn add(a: i32, b: i32) -> i32 { return a + b }

@test fn add_is_commutative() -> bool {   // a test returns true on success
    return add(2, 3) == add(3, 2)
}

@bench fn sum_to_1000() {                  // a benchmark takes no args; its body is timed
    var total: i32 = 0
    for i in 0..1000 { total = total + i }
}
```

```sh
$ jestyrc test examples/tests_demo.jtr
running 2 test(s)
test add_is_commutative ... ok
test doubling_works ... ok
bench sum_to_1000 ... 0.000 ms

result: 2 passed; 0 failed
```

`jestyrc test` ignores any `main` and generates a harness instead. Rules:

- a **`@test`** takes no parameters and returns `bool` (`true` = pass);
- a **`@bench`** takes no parameters; its body is timed with `clock()`;
- the process exit code is `0` iff every test passes — so it drops into CI.

A failing test prints `... FAILED` and flips the exit code:

```
test fails ... FAILED
result: 1 passed; 1 failed     # exit code 1
```

> Note: `@bench` currently times a **single** invocation. A production
> micro-benchmark would iterate and consume the result so the optimizer can't
> elide pure work — that refinement is future work.

---

## 9. Reserved attributes

These parse and are recognized, but using one today is an **error** with an
explanation — deliberately, because silently accepting (say) `@verified` while
not verifying would be unsafe:

| Reserved | Reserved for |
|---|---|
| `@verified` | static verification via SMT (design §7; see `MOTLEY.md`) |
| `@doc_hidden` | the documentation generator (workstream C) |

---

## 10. Design notes & limitations

- **Why errors, not warnings, for unknown attributes?** A misspelled `@inlien`
  that silently does nothing is a latent performance or correctness bug. In a
  deterministic systems language, a directive the compiler can't honor must be
  surfaced loudly.
- **The C backend is leverage.** Because Jestyr lowers to GNU-flavored C, the
  whole optimization/ABI family is a one-line `__attribute__` mapping that
  `gcc`/`clang` already honor — and `-Wdeprecated-declarations` /
  `-Wunused-result` fire by default, with no extra build flags.
- **`@deprecated` names the mangled symbol.** The C-level warning references
  `jestyr_<name>`, not the Jestyr name. A Jestyr-level call-site warning is a
  future refinement.
- **`@no_mangle` const shadowing** — see the warning in [§6](#6-abi--bare-metal).
- **`@bench` single-shot timing** — see the note in [§8](#8-testing--benchmarking--jestyrc-test).

---

*Part of the Jestyr language. See [`HANDOFF.md`](../HANDOFF.md) for the compiler
internals and [`jestyr-design.md`](../jestyr-design.md) for the language vision.*
