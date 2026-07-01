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
| `@layout(c)` | struct | identifier | (marker) | C field order (the default today) |
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
