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

## Extracting a payload — `catch |e| match e { … }`

Error names can carry a value (`docs/error-payloads.md`): `!{ Empty, TooBig(i64),
BadKey(str) }` declares `TooBig` with an `i64` payload, `err(TooBig(n))` creates
it, every `?` hop copies it blind — and **`match` on the bound error is the one
way to read it back**:

```jestyr
let v = hop(n) catch |e| match e {
    Empty      => 0 - 1,
    TooBig(v)  => v,               // the payload, with its declared type
    BadKey(m)  => (m.len as i64),  // a str payload is a str
}
```

* **The match must be the immediate fallback over the binder itself.** That shape
  is what puts the base's *static* error set in hand for exhaustiveness — the
  binder stays the opaque `error` everywhere else, and no payload accessor exists
  outside a match arm.
* **Exhaustive over the static set** (which the type system carries since the
  sets-become-sound increment): a missing name is a compile error listing what is
  uncovered; `_` covers the rest. An arm naming an error outside the set, a
  duplicate arm, an arm after `_`, and a guard are each refused with the reason.
* **A bare arm on a payload carrier is legal** (`TooBig => 7` ignores the value);
  binding a payload on a bare name is not — there is nothing to bind.
* Arm bodies are fallback values: typed against the ok type, one expression each
  (the same value-position rule every `catch` fallback has).
* Lowering: an if-chain on the result's tag inside the error branch, the payload
  bound as a typed `const` from its union member, and the last arm emitted
  unconditionally — exhaustiveness is proven at check time, so C gets a
  totally-assigned result with no dead final branch.

**The intrinsic tag wart is fixed as part of this.** `try_read_file` /
`try_from_utf8` used to hard-code error tag 1, which aliased the first
user-declared error name. Both now construct with the *user* tag whenever the
program declares `IoError` / `Utf8Error` (falling back to the historical 1, so
every existing program is byte-identical), and a match arm looks the tag up the
same way — origin and arm can never disagree. `match e { Parse => …, IoError => … }`
over a propagated `try_read_file` failure picks the right arm, proven by running.

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

## Error sets in trait signatures — fallible trait methods

A **trait method** can declare an error set, and the trait's set is the contract
every call through the trait is typed by:

```jestyr
trait Load { fn get(read self) -> i64 !{ Missing(i64) } }

impl Load for Flaky {
    fn get(read self) -> i64 !{ Missing(i64) } {
        if self.n < 0 { return err(Missing(self.n)) }
        return ok(self.n)
    }
}

let v = f.get()?                                        // propagates { Missing }
let w = f.get() catch |e| match e { Missing(v) => v, _ => 0 }   // extracts the payload
```

* **Conformance is set inclusion** (the same relation `?` enforces): an impl must
  declare a subset of the trait's set. Trait bare + impl fallible stays refused
  (the original rule); trait fallible + impl bare is refused too — the ABI
  returns the tagged result struct, so the body must construct it.
* **Payloads need no trait syntax**: a payload is a property of the error NAME
  (whole-program), so `Missing(i64)` declared in the trait means `Missing`
  carries an `i64` everywhere, and extraction works on a trait call unchanged.
* **Static impl calls and bracket-bound generic calls** carry fallibility (both
  lower to direct calls of the result-returning impl function). Deferred, each
  refused with its reason: **dyn dispatch** of a fallible method (the vtable
  machinery has not learned the result-struct ABI — the refusal is at the
  `dyn` coercion), a **default body** on a fallible trait method, and a
  fallible method in a **blanket `impl[…]`**.

The port mirror had a finding worth the price: the reference's `method_instances` is
**one LIFO worklist for plain and generic methods alike**, and the port's old flat
first-seen scan of plain methods was a **latent order divergence** — three plain
methods called `first/second/third` emit `third/second/first` — that no corpus file
had two instances to expose. Plain methods now route through the same worklist as
generic ones (argc-0 records), and the whole corpus stayed byte-identical.

## Not yet (post-v1)

* **Dyn dispatch of fallible trait methods** (refused at the coercion with the
  reason); default bodies on fallible trait methods; fallible methods in
  blanket impls. Each is a deliberate deferral, not a gap.
* **Owning payloads** (`String` etc. — the drop obligation must be designed
  first), **named sets** (`error FsError = { … }`), match-over-result sugar,
  and multi-statement `catch`/arm bodies (the value-position block rule).
* The **port mirror for trait error sets** — due with the first corpus `.jtr`
  that declares one (the standing trigger; reference side is complete and
  gated on use, so every golden is green without it).
