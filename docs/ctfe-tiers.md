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
| 2 | `comptime { … }` blocks in user syntax | **done on BOTH sides** — G2 (reference); M1–M3 (port: parse, fold, emit). `examples/comptime_block.jtr` is in the corpus and byte-identical across both compilers |
| 3 | Type reflection over the declared shape | **done in the Rust reference** — G3 (sizes/offsets wait on **L**) |
| 4 | `build.jestyr` — the build described in Jestyr | **done in the Rust reference** — G4 (`jestyrc plan`) |
| 5 | Bounded, attestable artifact generation | **done in the Rust reference** — G5 (`--emit`) |
| 6 | Aggregate values — comptime **tables** | **done in the Rust reference** — G6 (`Value::List`) |
| 7 | Comptime `for` + mutation — computed table *shape* | **done in the Rust reference** — G7 |

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
`Value::{Int, Bool, Str, List, Unit}`, covering literals in every base, unary and checked
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

## Tier 3 — reflection over the declared shape

Four intrinsics let a program ask the compiler what a type *is*:

```jestyr
struct Point { x: i32, y: f64, label: str }

fn main() -> i32 {
    print_str(@type_name(Point))              // "Point"
    print_int(@field_count(Point))            // 3
    print_str(@field_name(Point, 2))          // "label"
    print_str(comptime { @field_type(Point, 0) })  // "i32"
    return 0
}
```

| Intrinsic | Result |
|---|---|
| `@type_name(T)` | the type's declared name (works for primitives too) |
| `@field_count(T)` | number of declared fields |
| `@field_name(T, i)` | the i-th field's name, in **declaration order** |
| `@field_type(T, i)` | the i-th field's type, rendered in Jestyr syntax |

The first argument is a **type**, named directly — it is not evaluated as a value,
the same way `offset_of(Point, y)`'s second argument is a bare field name. `record`
and `union` share the struct item and reflect identically; methods are not fields.
Field types are rendered by the same `doc::ty_str` the documentation generator uses,
so a reflected type name cannot disagree with the documented one.

These are answered by the *Jestyr* compiler and reach C as literals — unlike
`size_of`/`align_of`/`offset_of`, which are handed to the C compiler. An unanswerable
query is a diagnostic, never a default: an out-of-range index is not clamped, a
non-struct is not silently zero.

### Why `@`, when `size_of` has no sigil

The first design put reflection beside `size_of(T)` as ordinary identifiers, for
consistency. That was wrong, and the compiler's own source proved it: `examples/std/
typeck.jtr` declares `fn field_type(…)`, so a bare-name `field_type` intrinsic would
have silently hijacked a real function and broken the self-hosted build. Any
bare-name intrinsic carries that hazard; `size_of` and friends simply have not been
unlucky yet.

`@name(…)` was already a callable form in the grammar (`@address(0x…)`), so the `@`
namespace costs no new syntax — and nothing in it can ever collide, because a user
cannot declare `fn @field_type`. It also makes "this is a compiler query, not a
call" visible at the use site, which is the same instinct as the rest of the
language: no hidden anything.

### What tier 3 does *not* yet do, and why

**Sizes, alignments and offsets are absent — blocked on workstream L.** `size_of`,
`align_of` and `offset_of` exist today only as **C-deferred** intrinsics: they lower
to `sizeof()`, `_Alignof()` and `offsetof()`, so the Jestyr compiler never learns the
numbers, it asks the C compiler. Making them comptime *values* requires the compiler's
own layout pass. Tier 3 therefore reflects what the compiler already knows without it
— the declared shape.

**Field iteration — resolved by tier 7, not by comptime-only functions.** Arguments to
a reflection query must be compile-time constants, and originally that made the *walk*
inexpressible: the natural way to write one is a helper `fn`, but a top-level `fn` is
also emitted as ordinary runtime code, and there the index is a parameter rather than a
constant, so the query cannot fold. That looked like it needed **comptime-only
functions**.

It did not. A comptime `for` binding is not a function parameter — it lives in the
interpreter's own environment — so the loop form folds where the function form could
not, and because typeck never descends into a `comptime` body, the query inside one is
the interpreter's alone:

```jestyr
struct Point { x: i32, y: f64, label: str }

const SHAPE: str = comptime {
    var acc = ""
    for i in 0..@field_count(Point) {
        acc += @field_name(Point, i)
        acc += ": "
        acc += @field_type(Point, i)
        if i + 1 < @field_count(Point) { acc += ", " }
    }
    acc
}
```

That reaches C as `JSTR("x: i32, y: f64, label: str")` — design §8's "iterate fields,
read type info, generate serializers", in ordinary Jestyr, with no macro language.
Collecting the metadata into a table (`var t = [""; @field_count(Rec)]`) works the same
way. Comptime-only functions remain a reasonable convenience later, but they are no
longer a blocker for anything.

## Tier 4 — `build.jestyr`

The build described in Jestyr itself (design §11), **evaluated, never run**:

```jestyr
// build.jestyr
const targets: i64 = 2

fn stem(i: i64) -> str {
    if i == 0 { return "greet" }
    return "count"
}

fn source(i: i64) -> str { return stem(i) + ".jtr" }
fn output(i: i64) -> str { return "built_" + stem(i) }
```

```bash
jestyrc plan build.jestyr
```

prints a deterministic, diffable plan:

```text
build-plan v1
targets 2
target greet.jtr -> built_greet
target count.jtr -> built_count
```

and `jestyrc plan build.jestyr --build` compiles each target to the executable it
names. An explicit subcommand rather than magic on the filename: nothing changes
meaning because a file happens to be called `build.jestyr`.

### Why a pure description, not a build DSL

The shape every build system reaches for is imperative:

```text
pub fn build() { exe("app", "src/main.jtr"); test("tests/main.jtr") }
```

That needs compile-time **effects** — `exe(…)` has to record something — and an
effectful comptime evaluator is exactly what this ladder exists to prevent. Allowing
it would mean a build script could read the clock or the environment, and
reproducibility would become a convention rather than a property.

So the plan is **data the evaluator produced**, not a sequence of calls it made.

Determinism is structural, not policed: a script that tries to be non-deterministic
cannot be *written*, because the interpreter has no arm for a clock or an environment
read. Totality reaches here too — a non-terminating script is a diagnostic, not a hung
build — and a target count is bounded, so a typo'd `const targets: i64 = 100000000`
is an error rather than an apparent hang.

### Two forms, selected by type

`const targets` decides the script's shape by *its own type*: an integer is a count
(answer questions about an index), a list is the targets themselves.

The **index form** above is what tier 4 shipped, and it shipped alone for a reason —
before a comptime `for` there was no way to *build* a list, so writing one out entry by
entry bought nothing over answering questions about an index.

Tier 7 changed that, so the **list form** is now generally the one to reach for
(`examples/build_list.jestyr`):

```jestyr
const names: [3]str = ["hello", "shapes", "array_lit"]

const targets = comptime {
    var t = [["", ""]; 3]
    for i in 0..3 {
        t[i][0] = "examples/" + names[i] + ".jtr"
        t[i][1] = "jestyr_demo_" + names[i]
    }
    t
}
```

A target is a two-element `[source, output]` list because the comptime value domain has
no struct — the honest representation rather than a chosen one. (Two parallel lists,
`sources` and `outputs`, would read better and would be worse: they can disagree in
length. A pair cannot.)

The index form stays supported, and stays the better shape when a target's fields are
genuinely *derived* per index rather than listed. Both describe the same builds and
render byte-identical plans. Artifacts (tier 5) take the same two forms: `const
artifacts` is either a list of `[path, text]` pairs or an integer count paired with
`artifact_path(i)` / `artifact_text(i)`.

## Tier 5 — bounded artifact generation

A build script may *compute the bytes of a file*. The plan records each artifact by
its **SHA-256**, and `--emit` writes it:

```jestyr
const artifacts: i64 = 1
fn artifact_path(i: i64) -> str { return "gen/table.jtr" }

fn rows(i: i64) -> str {
    if i >= 3 { return "" }
    return "    print_int(10)\n" + rows(i + 1)
}
fn artifact_text(i: i64) -> str {
    return "// generated at compile time -- do not edit\n" +
           "fn main() -> i32 {\n" + rows(0) + "    return 0\n}\n"
}
```

```text
artifacts 1
artifact gen/table.jtr 132 sha256 33135a25f790…
```

A generated file can then be named as a build target, so a program can be computed
at compile time and compiled in the same invocation.

**Where the boundary sits — this is the whole design.** The evaluator gained *no new
power*: it computed a string, exactly as it computes any other comptime value, and it
still cannot touch a file. The **driver** places the file, and only under an explicit
`--emit`. Generation is a pure function whose result the user chooses to write, never
an effect a script can perform.

Three properties follow:

- **Reproducible.** Same script, same bytes, same digest — the artifact is a pure
  function of the source.
- **Attestable.** The plan carries the hash, not the content, so a generated file can
  be pinned and drift-checked in CI exactly the way `attest`'s manifest pins the
  emitted C — and a plan diff shows *that* an artifact changed without drowning the
  reviewer in the change.
- **Bounded, literally.** Artifacts are size-capped, and a path that is absolute or
  contains `..` is **refused rather than normalised** — a script that wants to write
  outside the project is not a script whose intent should be guessed at.

This is deliberately *artifact* generation, not source-string injection into the
program being compiled: what a script produces is a file the user can read, diff and
hash before anything acts on it.

See also **tier 6** below, which makes the *direct* form available: a `[N]T` static
computed in place, with no generated source file in between.

## Tier 6 — aggregate values, and comptime tables

`Value::List` turns CTFE from "compute a number" into "compute a **table**". Array
literals and repeats evaluate at compile time, and lists are read with `[i]` and
`.len`:

```jestyr
fn fib(n: i64) -> i64 {
    if n < 2 { return n }
    return fib(n - 1) + fib(n - 2)
}

const FIB: [8]i64 = comptime { [fib(0), fib(1), fib(2), fib(3), fib(4), fib(5), fib(6), fib(7)] }
const ZEROS: [4]i64 = comptime { [0; 4] }
```

emits, as ordinary statics:

```c
static const JestyrArr_i64_8 jestyr_FIB = { { 0, 1, 1, 2, 3, 5, 8, 13 } };
static const JestyrArr_i64_4 jestyr_ZEROS = { { 0, 0, 0, 0 } };
```

The C compiler is handed the *answers*, not the recursion that produced them — and
the result is indistinguishable from a table typed out by hand. A comptime aggregate
types as `Ty::Array { elem, len }`, the same type a written `[a, b, c]` produces, so
every later pass sees an ordinary array.

**The emission detail with teeth.** A `const` must be a **brace initializer**: a C
static cannot be initialised by a GNU statement-expression, which is the shape an
expression-position aggregate uses. Both paths exist, and the end-to-end test asserts
no `({` appears in a static's initializer — if the `const` path ever fell through to
the expression path, gcc would reject the output.

**Totality over aggregates.** Producing an element spends a step from the fuel budget,
so `[0; 10_000_000_000]` is a diagnostic in microseconds rather than an attempt to
allocate ten billion values — including the nested case (`[[0; 100000]; 100000]`),
where it is the *product* that would blow up. Without the per-element spend the test
suite hangs, which is exactly why the property test exists.

Lists compare with `==`/`!=` structurally. They deliberately do **not** order: a rule
for comparing aggregates would have to be invented (lexicographic? by length?), and
inventing rules is what this evaluator does not do. Indexing out of range is an error,
never a clamp; a nested aggregate with no annotation to say what it is is an error,
never a guess.

Tier 6 makes a table's *values* computable. **Tier 7** below makes its *shape*
computable too.

## Tier 7 — comptime `for`, and mutation

Loops and `var` assignment run at compile time, so a table is *built* rather than
written out:

```jestyr
fn crc_entry(n: i64) -> i64 {
    var c = n
    for k in 0..8 {
        if c % 2 == 1 { c = 3988292384 ^ (c / 2) } else { c = c / 2 }
    }
    return c
}

const CRC: [256]i64 = comptime {
    var t = [0; 256]
    for i in 0..256 { t[i] = crc_entry(i) }
    t
}
```

That emits a 256-entry `static const JestyrArr_i64_256 j_CRC = { { 0, 1996959894, … } };`
— the loop, the mutation and the recursion all happened in the compiler, and nothing
of them reaches C. Before tier 7 this table had to be typed out by hand, which is why
tier 6 alone was not enough.

**Loops are statements, so mutation comes with them.** A `for` yields no value in
Jestyr, at compile time exactly as at runtime — so a loop earns its keep by writing to
a `var`, and tier 7 is really "loops *and* assignment" rather than either alone.
Supported: all three loop heads (`for {}`, `for <cond> {}`, `for … in …`), ranges
(exclusive, inclusive, `step`, descending), iterating a list, the element+index form,
`break`/`continue` including **labelled** ones, and `for … else` (which runs iff
nothing broke). Assignment reaches locals and elements at any depth (`g[1][0] = 7`),
and compound assignment (`s += i`) uses the same **checked** arithmetic as everything
else — so a comptime `+=` overflows into a diagnostic rather than wrapping.

**Totality, a third time.** Fuel is spent **per iteration**, and this is the case where
nothing else would charge anything at all: `for i in 0..1_000_000_000 { }` has an empty
body, so no sub-expression is evaluated. A `step` of `0` is refused outright rather
than spun on. Four unit tests and a property test exist purely to hang the suite if
that per-iteration `spend` is ever removed.

Scoping matches runtime: a loop variable belongs to its iteration and is not visible
after the loop.

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
| Properties | `proptests::comptime_props` | arithmetic vs a checked-`i64` oracle, determinism, trivia-insensitivity, reflection order, no-panic |
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
