# Error handling — propagate with `?`, recover with `catch`

Jestyr splits failure two ways (design §7). **Errors** are expected outcomes a caller
should handle; **faults** are bugs, and are not catchable as ordinary control flow. This
page is about errors.

A fallible function declares what it can fail with, and returns `ok`/`err`:

```jestyr
fn safe_div(a: i32, b: i32) -> i32 !{ DivByZero } {
    if b == 0 { return err(DivByZero) }
    return ok(a / b)
}
```

## `?` propagates — `catch` recovers

They are the two halves of the same story, and the difference is who handles the error:

```jestyr
let q = safe_div(a, b)?          // hand it to my caller — I stay fallible
let q = safe_div(a, b) catch 0   // handle it here — I do not
```

That is why **`catch` is legal in an infallible function and `?` is not**: recovering is
precisely what turns a fallible call into an ordinary value.

| | `?` | `catch` |
|---|---|---|
| On error | returns it to the caller | evaluates the fallback |
| Requires a fallible enclosing fn | yes | no |
| Emitted C | early `return` of the error | a conditional |

## The fallback runs only on the error path

`catch` supplies a *fallback*, not a default argument:

```jestyr
let cfg = load(path) catch expensive_default()   // expensive_default() is NOT
                                                 // called when load succeeds
```

This is a guarantee, not an optimization — a fallback with side effects must not run on
the success path. It is why the lowering is C's conditional operator rather than two
evaluated branches, and it is checked by running a fallback that prints
(`catch_recovers_and_short_circuits`).

## Chaining

`catch` is **right-associative**, so a chain tries each alternative in turn:

```jestyr
let v = primary() catch secondary() catch 0    // primary, else secondary, else 0
```

Parsed the other way this would apply `0` to an already-recovered value — both parses
compile, so the associativity is pinned by a test rather than left to reading.

## Precedence

`catch` binds **looser than every binary operator and tighter than assignment**, so

```jestyr
let v = read(p) catch 0
```

groups as `v = (read(p) catch 0)`, which is the reading every example assumes.

## What is checked

* The left side must be **fallible**. `catch` on an expression that cannot fail is a
  compile error, not a no-op — it reads as a claim that an error was handled, and a
  claim about nothing is worse than a diagnostic.
* The fallback is inferred **against the ok type**, so a literal picks up the right
  width and a struct literal gets its expected type. A `distinct` ok-type still needs an
  explicit `as`, exactly as a `let` annotation would.

## Both sides

`catch` is implemented in the Rust reference **and** in the self-hosted compiler
(`parser.jtr` kind 45, `typeck.jtr`, `cgen.jtr`), byte-identical across the corpus, and
the bootstrap seed carries it — so the gcc-only from-scratch compiler can build programs
that recover. `examples/error_catch.jtr` is the worked example.

## Debug error traces — `--error-traces`

Tier 4 of the ladder. `jestyrc build/run/emit-c <file> --error-traces` instruments the
error paths with a Zig-style trace:

* **`err(E)`** is the *origin* — it resets the trace and records where the error was born
  (reset at creation, not at print, so a recovered-then-recreated error never shows a
  stale path).
* **each `?`** records itself as a *propagation hop* before its early return, so the
  trace reads as the error's path up the stack.
* **`unwrap` of an error** is the *surfacing point* — the recorded path prints to stderr:

```text
error trace (origin first):
  src/config.jtr:2 (error created here)
  src/config.jtr:6
  src/main.jtr:10
```

Three properties, each tested by running (not by reading emitted C):

* **Opt-in and invisible otherwise.** Without the flag the emitted C is byte-identical —
  not a byte of the runtime appears — so goldens, attest hashes, the fixpoint and the
  seed never see it.
* **stderr only.** stdout is byte-identical with and without the flag, even when a trace
  fires — which is what keeps the flag compatible with every determinism canary.
* **Behaviour-preserving.** Traced `unwrap` yields exactly what untraced `unwrap`
  yields; the fixed-size hop buffer (64 entries, oldest kept — the origin is the entry a
  reader needs most) never allocates, so instrumentation cannot fail.

`catch` deliberately records nothing: recovery *consumes* the error, and the next
`err()` resets the buffer anyway.

Not ported to `jc` (the self-hosted driver): the flag is per-invocation debug tooling,
no corpus file or golden uses it, so no two-sided tax is due. Recorded here rather than
discovered.

## Binding the error — `catch |e|`

The design's second example (§7), now implemented (reference side):

```jestyr
let v = small(n) catch |e| (e as i64)      // recover, with the tag in hand
let v = small(n) catch |e| return e        // explicit propagation — exactly `?`
```

* **`e` is opaque.** It has the `error` type, not `i32` — `catch |e| e` is refused,
  because recovering with the raw tag would silently turn an error code into a success
  value. The explicit cast (`e as i64`) is the sanctioned escape hatch, exactly as it is
  for `distinct`. Runtime representation: the result struct's `int err` tag.
* **`catch |e| return e` is `?`, spelled out.** Same lowering, same tag preservation
  across the hop, same requirement of a fallible enclosing function. Only the binder may
  be returned — anything else would be a general statement-position fallback, which is a
  bigger feature than "re-raise, explicitly".
* **A `|` right after `catch` is always the binder**, never a closure-literal fallback.
  A closure fallback needs parens: `catch (|x| x)`. (Zig's resolution of the same
  surface.)
* **Scoped to the fallback.** The binder is a `const int` in the error branch of the
  lowering; the success path never sees it.

**Mirrored in the port**: `parser.jtr` (the binder's name span + the rethrow flag on
kind 45), `typeck.jtr` (the opaque `error` prim, code 20, bound in a pushed-and-popped
scope over the fallback alone), and `cgen.jtr` (all three lowerings, byte-identical).
`examples/error_catch.jtr` carries the binder forms, and the bootstrap seed carries the
mirror. The increment also fixed a **reference** bug the P3 golden caught: the typeck
arm's early `return`s bypassed `set()`, so a `catch`-expression's recorded type stayed
`Unknown` while the port recorded it faithfully — the rare divergence where the port
was right and the reference was wrong.

## Fallible methods — both sides

A struct method declares an error set exactly as a free function does, and every
consumer works on the call unchanged:

```jestyr
struct Account {
    balance: i32
    fn withdraw(mut self, amount: i32) -> i32 !{ Insufficient } {
        if amount > self.balance { return err(Insufficient) }
        self.balance = self.balance - amount
        return ok(self.balance)
    }
}

let left = a.withdraw(30) catch 0 - 1     // recover
let tag  = a.withdraw(500) catch |e| (e as i64)   // bind the tag
let v    = s.get()?                        // propagate out of a free fn
```

A **generic** struct's method gets one result type per instantiation — `Slot(i32).get`
and `Slot(f64).get` emit `JestyrResult_i32` and `JestyrResult_f64` — exactly as two
monomorphized functions would. `examples/method_errors.jtr` is the worked example.

**Trait-impl methods stay infallible, by rule.** A call through a trait is typed by
the trait's *signature*, which has no error-set syntax — so a fallible impl would be
silently mistyped as infallible at every call site. It is a check-time error with that
reason. Lifting the rule needs error sets in trait declarations, a design item.

The port mirror had a finding worth the price: the reference's `method_instances` is
**one LIFO worklist for plain and generic methods alike**, and the port's old flat
first-seen scan of plain methods was a **latent order divergence** — three plain
methods called `first/second/third` emit `third/second/first` — that no corpus file
had two instances to expose. Plain methods now route through the same worklist as
generic ones (argc-0 records), and the whole corpus stayed byte-identical.

## Not yet (post-v1)

* Error sets in **trait signatures** (which would unlock fallible impls).
* Errors in more positions; richer error payloads (today an error is a tag) —
  **designed, not built**: `docs/error-payloads.md` carries the decisions (payload
  is a property of the name, one whole-program payload union, `catch |e| match e`
  as the extractor, sets made sound first) and the E1–E6 increment chain.
