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

## Not yet

* `catch |e| …` — binding the error value. Reserved in the design (§7), not implemented.
* Error return **traces** (Zig-style).
* The **port mirror**: `catch` exists in the Rust reference only, so no `examples/**.jtr`
  may use it until `parser.jtr`/`typeck.jtr`/`cgen.jtr` mirror it and the bootstrap seed
  is refreshed (the standing two-sided tax).
