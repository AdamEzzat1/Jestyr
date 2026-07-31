# Compile-time evaluation in Jestyr — the tier ladder

Jestyr has **one** metaprogramming mechanism (design §8): compile-time evaluation.
There is no template language and no macro language. That single mechanism is
deliberately exposed as a **ladder**, not as a switch — each rung adds a specific,
named power, and each rung has its own rules, its own diagnostics and its own tests.

The reason for a ladder rather than "comptime can do anything" is the language's
identity: costs stay visible, compilation stays deterministic, and what the compiler
did stays auditable. A comptime facility that can read the clock, touch the network,
or mutate the compiler's own state destroys all three at once.

| Tier | What it gives you | Status |
|---|---|---|
| 0 | `const` values, enum discriminants | **done** (predates workstream G) |
| 1 | Pure comptime expressions where the *compiler* needs a number — array lengths, repeat counts | **done** — G1, `ebf8397` |
| 2 | `comptime { … }` blocks in user syntax | **done in the Rust reference** — G2; port mirror outstanding |
| 3 | Type reflection | not started |
| 4 | `build.jestyr` — the build described in Jestyr | not started |
| 5 | Bounded, attestable code/artifact generation | not started |

---

## Tier 0 — constants

`const` items and enum discriminants. These have always been folded; they are the
reason a comptime evaluator had to exist at all.

## Tier 1 — the compiler needs a number

Some positions cannot be deferred to the C compiler, because the *Jestyr* compiler
needs the value while it is still checking. An array length is the clearest case: it
is part of `Ty::Array { len }` and part of the emitted C type name
(`JestyrArr_i32_4`), so `[SIZE]i32` cannot be lowered without knowing `SIZE`.

Tier 1 is `src/comptime.rs` — `Interp::{eval, eval_usize}` over
`Value::{Int, Bool, Str, Unit}`, covering literals in every base, unary and checked
binary arithmetic, comparisons, short-circuiting `and`/`or`, bitwise ops and shifts,
string concatenation and comparison, `if`/`else`, blocks with `let`, references to
other `const`s, integer casts, and calls of pure functions including recursion.

> Tier 1 landed by *closing a silent miscompile*: before it, these positions accepted
> only an integer literal and silently fell back to `0`, so `[SIZE]i32` emitted a
> zero-length array and `assert(_ix < 0)` on every access, with no diagnostic.

## Tier 2 — `comptime { … }`

Tier 1 folds where the language forces it. Tier 2 is the same evaluator, made
available **where the author asks for it**:

```jestyr
fn main() -> i32 {
    let a = comptime { 2 + 2 }
    let table_size = comptime { 8 * 1024 }
    var xs: [comptime { 3 * 2 }]i32 = [0; 6]
    return a
}
```

Three deliberate design decisions:

**It reuses the existing `comptime` keyword.** `comptime` was already a keyword — the
generic-parameter marker in `fn Box(comptime T: type)`. Because that token could never
*start an expression*, using it for blocks needs no new reserved word and changes no
existing parse.

**There is no `comptime const`.** Jestyr's top-level `const` is already
comptime-evaluated; adding `comptime const` would be a second way to say the same
thing, which §8's "no second meta-language" rule exists to prevent.

**A comptime block is exactly the literal it folds to.** It is *typed* as that literal
(`comptime { 2 + 2 }` is an `i32` like `4` is) and it *emits* that literal. The block
itself never reaches C.

That last point carries the tier's real weight. **The body belongs to the interpreter
alone** — no runtime-semantics pass (cgen, escape, attrs, dharht) descends into a
comptime block, because there is no runtime code in there to reason about. Two
properties follow for free:

- a program that uses no comptime block emits **byte-identical** C, with no gating
  flag to keep in sync; and
- "it typechecks" can never disagree with "it evaluates", because exactly one checker
  sees comptime code.

Note that `comptime`, like `unsafe` and `if`, is a *block-led* form: in **statement**
position it parses as the block alone, so a trailing operator cannot extend it.
`comptime { comptime { 3 } + 1 }` is a parse error for the same reason
`unsafe { unsafe { 3 } + 1 }` is; write the nested block in value position instead
(`comptime { 1 + comptime { 3 } }`).

## Tier 3 — reflection *(not started)*

Reflection over type values: `@type_of`, field iteration, and the metadata an IR
builder needs.

**A dependency worth recording before anyone plans this.** `size_of`, `align_of` and
`offset_of` already exist in Jestyr — but as **C-deferred** intrinsics, lowering to
`sizeof()`, `_Alignof()` and `offsetof()`. The Jestyr compiler never learns the
numbers; it asks the C compiler. So exposing them as *comptime values* is blocked on
**workstream L** (the memory-layout pass), which is where the compiler first computes
size/align/offset itself.

What tier 3 can deliver without L is reflection over what the compiler already knows
— the **declared shape**: type names, field names, field types, and declaration order,
all readable from the AST. Offsets and sizes wait for L.

Iterating fields also needs two evaluator extensions that do not exist yet: aggregate
comptime values (`Value::List`) and a comptime `for`. Both are bounded by the existing
fuel budget, so neither threatens totality.

## Tier 4 — `build.jestyr` *(not started)*

The build described in Jestyr itself, evaluated by the same interpreter. Must stay
deterministic: no wall-clock, no unmodelled environment reads, and the resulting plan
must be reproducible and attestable. Wants tier 3's aggregate values first, so a plan
can be a *list* of targets rather than a single string.

## Tier 5 — bounded generation *(not started)*

Deterministic, attestable artifact generation — the foundation a future IR builder
(MOTLEY) would sit on. Explicitly **not** arbitrary source-string injection.

---

## The effect policy (all tiers)

Compile-time evaluation is **pure**. Allowed: local computation, `let` bindings,
top-level `const`s, calls of pure functions. Rejected: file, environment and process
I/O; allocation; raw pointer dereference; `unsafe`; concurrency and atomics; extern
calls; floats.

The policy is enforced **structurally rather than by an allowlist**: nothing effectful
is in the interpreter's value domain, so each of these is refused by the same rule
that refuses a float — there is no arm for it. There is no list to keep in sync as the
language grows, and no way to bypass it.

## Totality

A comptime interpreter is somewhere a compiler can hang or blow its stack on
ordinary-looking input. Three bounds make every input terminate with a diagnostic:

- a **step budget** (`FUEL`) spent on every expression evaluated,
- a **call-depth cap** (`MAX_DEPTH`), and
- **cycle detection** across `const` references.

Wrapping an expression in `comptime { … }` buys no escape from any of them: the block
is not a separate evaluation mode.

## Diagnostics

Every failure names a reason and points at the sub-expression that caused it, never at
the whole constant — and a required constant is **never** guessed:

```
array length must be a compile-time constant: `n` is not a compile-time constant
`comptime` block: division by zero at compile time
`comptime` block: compile-time evaluation exceeded its step budget (is it non-terminating?)
constant `A` is defined in terms of itself
a `comptime` block must produce a value
```

## Tests

| Layer | Where | What it pins |
|---|---|---|
| Unit — evaluator | `src/comptime.rs::tests` | every value kind, every totality bound, the effect refusals |
| Unit — checking | `src/typeck.rs::tests` | folded typing, array-length acceptance, refusal quality, determinism |
| Properties | `proptests::comptime_props` | arithmetic vs a checked-`i64` oracle, determinism, trivia-insensitivity, no-panic |
| Fuzz | `proptests::comptime_props::fuzz_comptime_eval` | arbitrary comptime bodies through parse → typeck → cgen |
| End-to-end (C) | `proptests::c_oracle` | folded values through a real C compiler, incl. C string re-encoding |

```bash
cargo test comptime
```

```bash
cargo bolero test fuzz_comptime_eval
```

### Why a *computed* string needs its own test

A string literal written in source is passed to C verbatim, because Jestyr's escapes
are already C's. A string **computed** at comptime has no source text, so it must be
re-encoded — and two C rules make the obvious encoder wrong:

- a hex escape is **maximal-munch**, so `"\x41" "1"` reads as one escape `\x411`;
  non-printables therefore use three-digit octal, which has a fixed width;
- `-std=c11` still honours **trigraphs**, so a literal `?` is escaped rather than left
  to turn `??/` into a backslash.

`a_comptime_string_survives_c_escaping` round-trips both through gcc.
