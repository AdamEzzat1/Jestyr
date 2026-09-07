# Tier 5 — what to do next

Cold-start note, ordered by what should be done FIRST. §1 is the serial work (compiler
defects — one session at a time). §2 is the parallel work (library breadth — fan out).
§3 is the coordination rules that make §2 safe.

```bash
cargo build --release && cargo test --release --features "c-oracle,selfhost-fixpoint"
```

**1364 passed / 0 failed / 3 ignored** (full ladder after A13 and the link-rule fold, 510 s;
the new tests are `drop_glue_under_colliding_type_names_finds_the_right_impl_and_only_that_one`,
`jdropcollide_drops_the_right_writer_and_only_that_one` and the three in `main.rs::link_rule`;
before them 1359 after HTTP V2, TLS and the registry layer).
Those commits sit on `claude/tier5-compiler-handoff-75c515`, a fast-forward of the
package-substrate branch; **not merged to master.** CI was fully green — all four jobs — at
the substrate branch's head; the Linux ladder has not run since `std/kv`.

The long-form history is `docs/session-notes/jestyr-tier5-handoff.md`. Read its §5 for the
area inventory (rewritten and current) and §6A for the defect register. Everything below
supersedes that note's §0 queue.

---

## §0. STATE

**Every known two-compiler divergence is closed.** A1 (select ExprIds), A5 (intrinsic
shadowing), A3 (`@copy` struct containment), A3b (`@copy` enum payloads), A2 (`environ`
inheritance, which turned out to need a test rather than a machine). The reference and the
self-hosted compiler now agree on acceptance, emission, and diagnostics wherever a rule
exists on both sides.

**The tier's own definition of done is MET** — *"a service can start, report health, run
background work, shut down gracefully, and be tested deterministically."* What remains is
breadth, not the headline claim.

**The COMPILER-DEFECT queue is now also done.** A7 (a range sub-view as a `mut` argument),
A6 (`Self` in an impl, in every position), A8 (`@deprecated` reaching attest and doc) and
B1 (`select`'s `closed` arm) all landed on both toolchains, each with its mirror watched
failing. Area 10 is complete. A11 has since been decided and enforced, A12 found and closed,
and B2 (`extern … var`) built — so the register's remaining entries are two CI-only items
nobody in reach can watch (A10's sanitizer half, a second C compiler) — see §1.

**Three of those four had a WRONG recorded description**, and in two cases the recorded
"correction" was worse than what it replaced. A7 was not about argument position and not a
parser change; A6 was not only about parameters and had a second half in typeck the entry
never mentioned; B1's real risk was six cgen walkers and a shim, not the parser. The §4 rule
about re-measuring before designing earned its place four more times.

**Four areas done** (service runtime, config, crypto-boundary, compatibility), **three
mostly** (observability, sandbox, and compatibility's tail), **three barely** (package,
HTTP, storage), and **TLS, which is no longer 'undecided'** — the coordination cost that
made it a gate turned out not to exist (see §2). It is now just a large §2 item.

---

## §1. DO THESE FIRST — the serial queue

Every item here touches the compiler's own closure, owes a port mirror, and forces a
reseed. **They cannot run in parallel with each other or with anything else that reseeds**
(see §3). Do them one at a time, in this order.

> **THE SERIAL QUEUE IS EMPTY.** 1.1 (A7), 1.2 (A6), 1.3 (A8), 1.4 (B1), **1.6 (A12,
> `return ok(local)`)**, **both halves of A11** (the lowering, and the language decision — a
> computed value into a `mut` param of an indirection-free type is now REFUSED, both sides)
> and **1.5 (B2, `extern … var`)** are all done, each mirror watched failing. B2 did not
> cost what it was measured at — see §1.5 for why, because the reason generalises.
>
> **So the next session should be fanning out on §2**, and §2 parallelises where §1 could not.
> Read §3 first — the sixteen closure modules are a global lock, and the reseed is the thing
> two concurrent sessions cannot hand-merge.

### ~~1.1 — A7: a range expression may not be a call ARGUMENT~~ — **DONE**

Closed both sides. `examples/slice_range_mut.jtr` is the corpus file, allowlisted, and the
port mirror was **watched failing** without it (the golden named exactly that file).

**Two recorded facts about A7 were wrong, and both cost time — re-measure before designing.**

* **It was never about argument position.** `total(b[0 .. 3])` into a `read []i64` has always
  worked, and `examples/slice_range.jtr` already shipped `from_utf8(b[0 .. 3])`. The Tier 4
  note's original `mut` diagnosis was the correct one; §6A's "correction" of it was the
  error. The boundary is **mutability**, i.e. by-address passing.
* **It was not a parser change**, so the P2-golden argument for the mirror did not apply.
  The parser builds the `Range` node fine and typeck types the sub-view correctly — `check`
  passes. It was one arm of **`cgen::emit_place`**.

**The actual mechanism.** `emit_place` is the lvalue-yielding twin of `emit_expr`, used for
`mut`/`out` arguments because those pass by address. Its `Index` arm assumed the index is a
*scalar element offset*; a range index is not — `xs[a .. b]` computes a whole new
`{ ptr, len }` view. The arm emitted the `Range` node as if it were an offset and hit the
backend's "the C backend does not support ranges yet", **pointing at the range**, which is
what made the symptom look like a missing range feature rather than a missing place case.

The fix parks the computed sub-view in a **compound literal of array type** — `(T[1]){ v }`
decays to `T*`, `(*…)` reads back as a place, and its lifetime is the enclosing block rather
than the statement expression that built it, so `&` of it outlives the call. This is the same
shape `abi_ref_arg` already uses, for the same reason. `str` takes the same route (a
`jestyr_rt_substr` call is a value too) and that half is reachable — a `mut str` parameter
was equally broken, as a raw gcc error rather than the range diagnostic.

The bounds assert survives the wrapping, so a bad sub-range still faults before the callee
sees it. The fixed-size-array refusal is deliberately untouched: typeck does not extend
re-slicing to arrays (that needs the borrowed-projection story, safety-mosaic item 2), so
the guard is restricted to the two bases `emit_expr` actually lowers a range over.

### 1.1b — A11: the LOWERING half is **DONE**; one language question is still open

**Done:** a `mut` argument no longer has to be a place. `f(mut s as Buf)`, `f(mut mk())` and
`f(mut a + b)` compile instead of leaking gcc's *"lvalue required as unary `&` operand"*.
Both sides; `examples/mut_arg_value.jtr` is the corpus file, and the port mirror was watched
failing.

`emit_place` owes its callers an lvalue; its catch-all was handing back whatever `emit_expr`
produced. Values now park in a compound literal of array type, the shape A7 and `abi_ref_arg`
already use. The callee gets a copy **whose indirection is shared**, so element writes through
a slice's `ptr` reach the caller's buffer; only a whole-value reassignment is lost, and a
temporary has nowhere to put one.

The `distinct` cast turned out to be the case that mattered most and the one nobody had
named: `s as Buf` is not a temporary at all — `Jestyr_Buf` *is* `JestyrSlice_i64` — so the
ordinary newtype idiom simply did not work in `mut` position.

**THE MISTAKE THIS ENTRY EXISTS TO RECORD.** The previous version of this note said the guard
had to track `emit_place`'s *rendering* rather than the source form, and that `is_c_lvalue`
was unusable because it answers "never" for `Index`. Half right. The fix uses `is_c_lvalue`,
and the reasoning offered for why that was safe — *"`Field`, `Index` and `Deref` each return
from their own arm, so the only lvalue reaching the catch-all is a `Name`"* — **was wrong**.
The `Field` arm RECURSES into `emit_place` on its base, and a method's `self` is a
`SelfValue`, which `is_c_lvalue` did not list. So `self.seen` in a `mut self` method parked
`(*j_self)`, and `free(&copy.seen)` freed a copy's list while the caller's was never released.

It compiled, it ran, and it produced a wrong answer quietly. **`census_cli` caught it; no
amount of staring at the arm did.** The corpus file now carries that shape permanently: remove
`SelfValue` from `is_c_lvalue` and `examples/mut_arg_value.jtr` prints 5 instead of 14 — a
wrong value, not a crash. `Index` genuinely cannot reach the catch-all; every other kind can,
and the guard has to be right about all of them.

**DECIDED AND DONE: refused, both sides.** A `mut`/`out` argument that is not a place and
whose type holds no indirection is an error in `escape` (`check_mut_value_arg` /
`check_mut_value_arg` in `escape.jtr`): *"cannot pass a computed value to the `mut`
parameter `n` of `twice`: the type `i64` holds no indirection, so the callee's writes would
land in a temporary nothing can read — bind the value to a `var` and pass that"*. The
boundary is the TYPE, exactly as the paragraph below asked: `add(s as Buf)` and `f(mk_slice())`
stay accepted because a slice's `ptr` is shared; a struct with a `*mut T` field stays
accepted; `i64`, `bool`, `P{ x: 1 }` of plain fields are refused. The ARGUMENT's inferred
type is what is tested (a `Ty` on both call shapes, where the declared parameter type is an
AST type on the method path), and `Opaque`/`Unknown`/generic answer "has indirection" so the
rule refuses only what it can prove. Corpus sweep: exactly one file carried the shape
(`mut_arg_value.jtr`, `twice(a + b)`), rewritten to bind the sum first; the corpus is
otherwise clean. `jestyr_mut_value_arg_matches_reference` carries four refused and four
accepted shapes through both toolchains — the port was watched disagreeing on the first
before the mirror landed. Reseed owed and done. **One shared limitation, pre-existing:** the
port's call checks (give-away, slice alias, this) resolve a bare `Name` callee only and defer
qualified `mod.f(...)`, so a computed value into a `mut` param through a QUALIFIED call is
refused by `jestyrc` and not by `jc`. Recorded in §6A; the corpus has no such call.

The question as it stood, kept for the record: should a `mut` argument that aliases
**nothing** be refused? `f(mut a + b)` is unobservable by construction: there is no
indirection to share, so the callee's write cannot be seen. Refusing it fits the language's
character (it already refuses `spawn` with `mut` params, and `@copy` with non-Copy fields).

Three things to know before deciding:

* **The old state was not "these are refused".** It was *"they work if they happen to render
  as a C lvalue"* — `f(mut P{ x: 1 })` compiled all along, because a C compound literal is an
  lvalue. Refusing category 3 is a NEW refusal, not the restoration of one.
* **It cannot be a warning.** The port's `jc build` refuses on any escape diagnostic and has
  no severity model, so a warning would be a program `jestyrc` builds and `jc` will not. Error
  or nothing.
* **A refusal is a rule**, so it must live in `escape` on **both** sides (`typeck.jtr` has no
  diagnostic channel), and the predicate is type-directed and subtle: "not a place, and the
  type transitively contains no indirection" — a struct with a `*mut T` field is *not* in this
  category. That is the whole cost, and it is why the lowering was not made to wait on it.

### ~~1.2 — A6: `Self` in a trait parameter — `check` passes, `run` fails~~ — **DONE**

Closed both sides. `examples/trait_self.jtr` is the corpus file, allowlisted, and **each of the
two mirrors was watched failing on its own**: disabling the `cgen.jtr` half fails the cgen
golden, disabling the `typeck.jtr` half fails the P3 typeck golden (and only that one).

**The register's description was again narrower than the defect** — third time in two items.
It was not "a trait parameter", and it was not one failure:

* **A parameter, a return, a local, and nested (`[]Self`) all failed**, and only when the
  IMPL spells `Self`. A trait declaration's `Self` was always fine (traits are not emitted),
  and an impl spelling the concrete type was always fine — which is why every corpus file
  worked and the gap survived.
* **It was TWO defects wearing two faces.** `check` passes / `run` fails was only the cgen
  half. `mut o: Self` failed the other way — at `check`, with a message about *escape
  analysis* — and nothing connected the two.

**cgen.** `Self` reached neither of cgen's two type doors. `c_ty_ast` (a written-down source
type) refused it: "cannot lower the external type `Self`", fired twice because the impl
emitter runs once for the prototype and once for the definition. `c_type` (an inferred `Ty`)
was worse — `Ty::Opaque("Self")` missed the subst and fell through to **`int`, silently, with
no diagnostic**. Both doors already consult `self.subst`, so the fix is one entry —
`subst.insert("Self", target)` in `emit_impl_method_decl` — not two special cases. That is
also what makes nesting work: the emitters recurse through the map.

**typeck.** `check_fn` lowered a body's `Self` to `Opaque("Self")` and left it. The source
comment called this a deliberate deferral costing nothing, "because `assignable` is lenient on
`Opaque`". **That justification had expired.** Leniency is not the only consumer: the escape
checker's `Unknown`-finalization backstop refuses a *borrow* whose type never resolved, so a
`mut Self` parameter was rejected outright. `read` and `take` slipped through only because
that backstop is about borrows — which is exactly why the hole looked empty. `register_impls`
already built the `{Self → target}` map for recorded return types; `check_fn` now applies it
to parameters and the return too.

**Port shapes differ, and the mirror is not a transcription.** `cgen.jtr`'s `emit_impl_sig`
took no `Cg`, so it had no substitution to bind into, and its substitution map is keyed by
source SPAN with a Ty-triple payload rather than by name. The mirror threads `c`/`g` in, swaps
`emit_c_ty` → `emit_su_ty` (a strict superset — it falls through to `emit_c_ty`), and binds
`Self` as a kind-0 name-span entry, which renders `Jestyr_P` for a struct target and
`int32_t` for a primitive one. `su_slot` matches by TEXT, so any occurrence of `Self` serves
as the key. `typeck.jtr` needed no new machinery — `subst_self` already existed.

**A latent port defect this exposed, now closed with it:** `jc` never refused `Self` at all,
so where `jestyrc` errored, `jc` alone emitted `int` and `JestyrSlice_Self` silently. It was
unreachable only because the reference refused first — a divergence that would have become a
miscompile the moment the reference stopped refusing.

### ~~1.3 — A8: `attest` accepts `@deprecated` and does nothing with it~~ — **DONE**

Closed both sides; `examples/attributes.jtr` already carried a real `@deprecated`, so both
goldens covered it without a new corpus file. **Area 10 is now complete.**

One correction to the entry: `@deprecated` was never doing *nothing* — it reached cgen and
emitted `__attribute__((deprecated("…")))`. What it missed were the two places that
*describe* the API, `doc` and the manifest.

It now has its own manifest line and its own `> **Deprecated**` blockquote. The design
question is the whole item: a deprecation is **not a guarantee** (that block says "checked by
the compiler"; a deprecation is asserted, not proven) and **not part of the signature**
(`diff_item` calls any signature change `Breaking`, which would classify *deprecating* an API
as a break — backwards, and a gate that fires on good behaviour gets switched off). So every
deprecation verdict is `Compatible`, and all of them are still reported.

**Cost note for whoever sizes the next port mirror:** the reference side was ~40 lines. The
port was four times that, and none of it was the extractor — it was *widening two packed
tuples*. `jc`'s parsed-manifest item is a flat `List(i32)` of 7-wide records and its doc
target is 11-wide; adding one optional field to each meant 7→9 and 11→13 plus every stride
site. **The first sweep missed three of them** (`(r * 11)`, `(m2 * 11)`, `(m3 * 11)`, all in
one loop a too-specific regex skipped) and the port compiled fine and then died at runtime on
`Assertion failed!`. A stride change is not done until `grep -n '\* <old>\|/ <old>'` comes
back empty — check, don't sweep.

### ~~1.4 — B1: `select`'s `closed { … }` arm~~ — **DONE**

Closed both sides. `examples/std/select.jtr` grew a Part 3 that uses it, and that file is in
**both** the cgen golden allowlist and the build matrix, so the port mirror is gated
automatically — no new corpus file was needed. Both mirrors were watched failing.

**It is sugar and nothing more, which was the whole design constraint.** The condition was
already computed and already the exit; the arm is somewhere to put a statement. Part 2 of the
example keeps the sentinel it replaces (read a counter before, read it after, infer "nothing
moved"), and Part 3 prints the same two numbers — the point is that the totals must MATCH, so
it is a check rather than a demo.

**`closed` is CONTEXTUAL, and this was the finding the register did not have.** The corpus
already exports `alog.closed()`, `sysnet.closed()` and `syswatch.closed()`, and binds a local
`closed` in two more modules — reserving the word would have broken five files, three of them
public API. Recognised only inside a `select` body, only when a `{` follows. A test pins both
halves (the ordinary name still works; the arm still parses), and a curated P2 snippet pins
`closed(1) + closed(2)` parsing identically on both sides.

**The arm must be last** — `E0025`, with `E0024` for a second one. Not a style rule: readiness
is tested before the closed condition (which is what keeps closing non-destructive), so a
`closed` written first would still run last. The parser owns `E0001`–`E0025` now.

**What the estimate got right and wrong.** "22 reference sites" was about right in spirit —
`ExprKind::Select` became a struct variant and 14 sites moved — but the *risky* ones were not
the parser. Six cgen walkers scan arm bodies for calls, spawns, closures, moves, refs and
structs; each had to learn the closed block, and missing one would have hidden code from the
backend rather than rejected it. The port's `ref_expr_id` shim needed the new block counted
for exactly the reason it counts arm bodies — **that omission is the A1 divergence shape**,
invisible until a select sits between two spawn sites in one function.

The claim about "the no-allowlist P2 golden" was wrong in detail: the corpus-wide P2 coverage
is the cgen golden and the build matrix; the P2 dump goldens are *curated snippet* lists, so
the new arm had to be added to one by hand or nothing would have compared it.

### ~~1.6 — A12: `return ok(local)` drops the local it carries out~~ — **DONE, both sides**

Closed. `cgen.rs` gained `as_returned_name` (a `return`'s bare local, or the one argument
of an `ok(...)`/`err(...)` it returns) and `collect_moved` uses it for both a `return`
statement and a block's tail; `cgen.jtr` mirrors it as `mv_mark_returned`. Corpus file
`examples/return_ok_local.jtr`, allowlisted, with a transcript test
(`return_ok_local_moves_the_local`: three drops, all after `-- end of main --`). **The
mirror was watched failing** — the golden diverged on exactly that file and no other — and
the seed moved by 53 lines. `err(local)` is covered by the same arm although no owning
payload can reach it yet.

What it was: for a struct with a `Drop` impl or `Drop`-bearing fields, `return d` moved
and `return ok(D{ … })` moved, but `return ok(d)` copied `d` into the result and then
emitted the local's drops. The caller got a closed file and freed strings. The probe was
three functions:

```
fn mk_plain() -> D { var d: D = D{ … }  return d }              // moved — fine
fn mk_ok() -> D !{ Nope } { var d: D = D{ … }  return ok(d) }   // DROPPED, then returned
fn mk_lit() -> D !{ Nope } { var t = …  return ok(D{ t: t }) }  // moved — fine
```

Exit code `STATUS_HEAP_CORRUPTION`, stdout lost. Nothing in the corpus tripped it because
every fallible constructor returns a literal. `std/kv` still opens INTO a caller-owned
handle; that shape is fine and stays (changing it now would only churn a green module).

**The cost estimate was right for once**: one predicate on each side, one corpus file, one
reseed. The fix is in `collect_moved`, the move ANALYSIS, not in the `return` lowering —
`emit_value_return` was already correct given a right `cur_moved` set.

### ~~1.5 — B2: `extern` binding a C global~~ — **DONE, both sides; the measurement priced the wrong design**

`extern "errno.h" var errno: i32`, `pub extern "c" var counter = "g_counter": i64`,
`@cfg(posix) extern "unistd.h" var environ: *mut cstr`. A global is a PLACE — read,
assigned, and `&`-taken by its C symbol. `extern "c"` declares `extern T sym;`; a `.h` abi
declares nothing, which for `errno` (a macro on glibc and msvcrt) is the only thing that
works. Corpus file `examples/extern_global.jtr` (cgen allowlist, attest list, transcript
pinned), three P2 item snippets; both port mirrors watched failing (the cgen Name arm →
cgen golden diverges on the corpus file; the parser `var` arm → item dump diverges).

**The 257-site count was right, and it was the price of a design nobody had to choose.** 257
is what a NEW item kind costs. A global is an extern symbol with a type and no parameter
list — the same header, alias, `@cfg` and ABI story as a function — so it rides the EXISTING
`ExternFn` item under an `is_global` flag. On the port it is param count `b == -1`, the one
slot every reader of an extern already bounds its loops by; `(g,h)`, `v` and `e` are read
kind-blind and were NOT usable, which the alias work had found by segfaulting. Nine sites
branch, each one the change concerns: parser, typeck registration (a const plus a `globals`
set), cgen's symbol map + declaration + Name arm + `&name`, `doc::extern_sig` (`var NAME:
T`, attest-hashed, so a `fn`↔`var` swap is a break), the P2 printer, the reference item dump
(an explicit `var`/`fn` atom). Every exhaustive match kept compiling untouched. **Before
sizing a feature by its exhaustive matches, ask whether it is a new KIND or a new FLAG on
one** — the answer is the difference between a session and an afternoon.

**The shadowing gap this first shipped with is closed.** A local named like a global used to
read the global in the backend: typeck resolved the local, but the cgen Name arm decided by
SPELLING, so `var errno: i64 = 40  errno = errno + 2` declared `j_errno` and then wrote the
real `errno` (watched: 2 and 2 where 42 and 0 were owed). The checker now records on the
expression row that a bare Name resolved to a global (`Resolved.global`; the port's
`c.gref` list) and both backends name the symbol from that record and from nothing else.
`shadow()` in the corpus file pins it. `environ` still goes through `env_block`; switching
it is POSIX-only and owed to the Linux ladder.

### Not in this queue, deliberately

- **A4** (Windows: `capture` + print does not round-trip a child's bytes). Making stdout
  binary would change every `\n` the compiler emits on Windows and re-baseline many goldens.
  A recorded decision, not a bug to fix blind.
- **A10's sanitizer half** and **a second C compiler**. Both are CI-only: this machine has no
  `libasan`, and **no clang at all** — measured, not assumed. They inherit the caveat about
  shipping changes nobody in reach can watch. The WARNING half is done and green.

---

## §2. THEN FAN OUT — the parallel queue

None of these touch the compiler's closure, so none of them reseed. They are genuinely
independent: different modules, different tests, different areas of the brief.

**Recommended first wave — three sessions:**

### 2.1 — Package substrate (brief area 5) — THE LOAD-BEARING ONE — **STARTED**

The chain is semver → resolver → lockfile → content-addressed cache. Content-hashing,
`buildgraph.jtr`, `tar.jtr` and `sha256.jtr` already exist underneath it.

**`std/semver` is DONE** — parse, validate, order, and requirement ranges. `@no_alloc @no_os`
(probed: inserting an `alloc` into it is refused), 15 tests, allowlisted so both compilers
agree on its emission. **It needed no reseed**, which is the §3 property working as designed:
a new leaf module outside the sixteen is parallel-safe.

Three things a successor should not re-derive:

* **A `Version` cannot hold `str` views.** A `str` field is a borrow and the escape checker
  refuses storing one that outlives the call. It carries byte OFFSETS and functions take the
  source alongside — the compiler’s own `ExprData`/`src` idiom. `compare` therefore takes TWO
  sources, which is the right general shape anyway (a requirement against a candidate).
* **`@copy` on `Version`/`Req` is load-bearing**, not decoration: without it, reading a
  `Version` out of a borrowed `Req` is a borrow projection and cannot leave the call.
* **Writing the hard rules in the header did not make them true.** The suite caught three
  real defects on its first run — including a scanner that used the single-identifier
  character set on a whole dotted run, rejecting every `1.0.0-rc.1`. Each test is chosen so
  the obvious wrong implementation fails it; that is why they failed.

**`std/resolve` is DONE too — by MINIMAL VERSION SELECTION, and that was a decision.**

Given `^1.2.0` and a registry holding 1.2.0/1.5.0/1.9.0 it picks **1.2.0**, where npm and
Cargo pick the highest. The reason is the property this tree spends its effort on everywhere
else: under maximal selection the answer depends on what the registry held at resolve time,
so reproducibility has to be bought back with a lockfile and the lockfile becomes
correctness-critical rather than a cache. Under minimal selection resolution is a pure
function of the requirement graph — **publishing a release cannot change an existing build**,
which `resolve_test.jtr` asserts by resolving, publishing, and resolving again. The cost is
real: you do not get patch fixes automatically, so upgrading is an explicit act.

One version per package, which is close to forced — the compiler flattens an import closure
into ONE translation unit, so two versions of a package would collide on symbol names.

`^`/`<` add upper bounds, which is what makes this not pure MVS (Go has only lower bounds and
needs no search). It does not backtrack: it iterates a fixpoint with **selection pinned
monotone**, which is what makes it terminate, and REPORTS anything it cannot decide rather
than guessing — `budget_exhausted` distinguishes a non-convergence from a real conflict so a
failure is never mislabelled.

**The constraint set is REBUILT each round, not accumulated**, and that is load-bearing: a
superseded version’s upper bound would otherwise contradict its successor’s floor and report
a conflict in a graph that resolves cleanly.

**A probe caught one of these tests being vacuous, and it is the lesson worth keeping.** All
ten passed on the first run, so each claim was probed by breaking the implementation.
Flipping to maximal selection failed 5 of 10 — correctly, including the reproducibility one.
But making constraints ACCUMULATE failed nothing: the phantom-conflict test set its root to
`b >=2.0.0`, so `b` was 2.0.0 from round one, `b 1.0.0` was never selected, and its upper
bound was never contributed under either policy. **The test asserted the property and pinned
nothing.** Rewritten so the lift arrives LATE (round 1 selects `b` 1.0.0; a dependency raises
it in round 2), it now fails under accumulation. A test for a fixpoint’s behaviour has to
reach the round where the behaviour happens.

**`std/lockfile` is DONE.** `jestyr-lock/v1`, `pkg <name> <version>` sorted by name, with a
`selection-sha256` over the body only. 10 tests.

**It is a WITNESS, not a source of truth**, and that is the payoff of choosing MVS. Under
maximal selection a lock is correctness-critical — delete it and two builds diverge. Here two
builds of the same graph already agree, so the lock records what was decided and lets a later
build prove it decided the same thing. Hence NO "install from lockfile" entry point: there is
nothing it could do that resolving would not. A disagreement means a REQUIREMENT changed.

`resolver mvs` is recorded deliberately — a lock produced under one policy means nothing under
another (the reason `jestyr-attest` writes `cc-flags`), and one naming another resolver is
refused rather than compared.

Two decisions, each **probed by breaking it**: the digest is checked FIRST (a hand-edited body
is a corrupt file, not drift — different responses), and `verify` runs in BOTH directions
(one-way would let a new transitive dependency enter a build unrecorded and still report
agreement). Removing either fails exactly the test that names it.

**`std/cache` is DONE, and the package substrate is COMPLETE**: semver → resolve →
lockfile → cache. 8 tests.

Content-addressed: the key is the SHA-256 of the value, so storing is idempotent, an entry
verifies against its own name without trusting its source, and there is no invalidation
question to answer. That also sidesteps `sysfs` having no mtime — a name-keyed cache would
have been stuck there. Writes go through a temp file and `rename_replace`, because a partial
entry in a content-addressed store is worse than elsewhere: its NAME asserts a hash its bytes
do not have. Sharded two hex characters deep. Gated on `fs.Fs`, with a test that `fs.denied()`
really cannot reach the disk through it.

**The substrate is finished. The remaining §2 items are independent and can fan out:** HTTP V2
(2.2), storage V2 (2.3), and the second-wave table below. TLS is among them and is NOT gated
— see the correction above.

What the substrate does NOT yet have, recorded so nobody assumes otherwise: a registry
protocol (nothing fetches), and a package manifest FORMAT (the `Registry` is built by API
call, not parsed from a file — deliberately, so it is testable without one). Both are the
obvious next layer if the tier wants an end-to-end `jc add`.

### ~~2.2 — HTTP V2 (area 6)~~ — **DONE: `std/httpd` + `std/httpc`**

Routing, middleware, keep-alive with pipelining, read and idle timeouts, chunked streaming
out, static files, an access log, a test client. Eight tests on real loopback sockets, five
mutations watched failing, the `jhttpd` demo pinned. The long note's §3s has the design.
What a successor should not re-derive:

* **One thread, one poll.** `spawn` refuses `mut` params, so a per-connection worker is a
  channel architecture; the readiness loop over `runtime.ask` is smaller and makes the
  suite a transcript. A handler is `fn(*mut u8, mut Exchange) -> i32` plus its context —
  the `runtime.Task` shape, the one a `List` can hold.
* **`httpc.fetch` cannot be used against an in-process server** — it blocks on the answer
  and the caller is the only thread that could produce it. The demo hung on exactly this;
  every in-process exchange is dial → request → `serve_for` → read.
* **`connect`/`close`/`listen` are `sysnet`'s C symbols in every module that links it**;
  the checker says "duplicate definition" one module away from the cause. Hence `dial`/
  `hangup`/`start`/`stop`.
* **A13** (drop glue under colliding type names) fired the moment `sysfs` (→ `file`) and
  `log` (→ `json`/`writer`) shared a closure with a struct holding a `json.Writer`.
  **CLOSED 2026-09-06** (reference-only; the port was already right — see the register).
  `httpd_test`'s dropped `sysfs` import may go back.
* Not built: request-body streaming (a request must fit the connection buffer; 431 and a
  close otherwise), TLS, compression, ranges, ETags.

### ~~2.3 — Storage V2 (area 9)~~ — **DONE: `std/kv`**

KV, atomic batches, compaction, migrations and snapshot/backup on `alog.jtr`. Ten tests,
a pinned demo (`jstate`, `examples/std/kv_demo.jtr`), four mutations watched failing, no
reseed (the drift guard said so). The long note's §3q has the design; what a successor
should not re-derive:

* **The log is durability, not capacity.** Values are memory-resident because
  `file.Reader` has no seek; an offset index could not fetch a value on demand. A store
  larger than memory needs a `seek` in `std/file` first and is a different structure.
* **A batch is one record, so it is atomic for free** and bounded by `KV_MAX_RECORD`.
  There is no begin/commit pair on purpose.
* **Compaction, migration and snapshot are one `rewrite`** — fresh file beside the store,
  `rename_replace`, then REPLAY THE NEW FILE into memory. The index has no removal, so a
  rebuild tombstones every live key by hand first (`forget`); skipping it was one of the
  four mutations.
* **Writers apply their own bytes by parsing them** through the one batch parser, so an
  encoder/parser drift fails at the write.
* **`open` fills a caller-owned handle** (`kv.closed()` then `kv.open(f, a, path, s)`),
  because of §1.6.
* Not built: `[]u8` values, index removal, a text export (values are `str` and may hold
  newlines; the snapshot is the portable form).

**Four suites were gating nothing.** `semver_test`, `resolve_test`, `lockfile_test` and
`cache_test` had no `io_suites_pass` entry. Now registered (15/10/10/8). A new suite is not
done until its count is in that table.

**Available for a second wave, same rules:**

| work | note |
|---|---|
| Crypto: HMAC, signing, a hash interface | New modules importing `sha256` read-only. These are BINDINGS, not algorithms to write — `csrand` deliberately invents nothing. |
| Trace spans | **Pick another word first.** `Span` is taken three times: `http.Header` spans, `diag` source spans, and the `@span` work-span attribute. Use the fn-pointer vtable shape, not a trait — `@no_alloc` passes vacuously through a trait method. |
| ~~Service supervision / restart policy~~ | **DONE: `std/supervise`** — see §2b. |
| Sandbox: cwd, process groups, fs capability projection | `sysproc.jtr:113` names all three. `fs.Fs` gates the parent; nothing projects it onto a child. |
| ~~Config: live reload, nesting~~ | **DONE: `std/livecfg`** — see the section below. |
| Rewrite `std/plugin` as a server on the pipe transport | Tier 4 leftover. One-process-per-call only because the transport did not exist; it does now (`start_piped`/`capture`). |

### ~~Config live reload + nesting~~ — **DONE: `std/livecfg`**

`std/config` + `std/ini` + `std/syswatch`, composed once. Seven tests over a real scratch
file (one through the real watcher, waited for via the runtime loop with a 2s bound), a
pinned demo (`jlivecfg`, `examples/std/livecfg_demo.jtr`, driven by `reload_now` so the
transcript is exact, with the watcher asked afterwards), four mutations watched failing, no
compiler change, no reseed. What a successor should not re-derive:

* **A reload builds a CANDIDATE and swaps it in whole or not at all.** `config.clone(base)`
  + `ini.load` into the clone; any non-shadowed problem drops the candidate. Applying key by
  key and stopping at the fault was the mutation the central test exists to catch.
* **The candidate comes from the BASE, not from `cur`.** `config.apply` can set but never
  unset, so a merge in place keeps stale file values forever; from the base, a key removed
  from the file reverts to its default. `Live` therefore owns TWO `Config`s with ids agreed
  by construction (`declare`/`apply` go to both), and the schema is closed by the first load.
* **A shadowed file value is not validated** — `config.apply` answers `CFG_SHADOWED` before
  it parses. Stated in the header, pinned by a test; a caller who wants the other behaviour
  changes that assertion on purpose.
* **The generation counts reloads that MOVED a value**, decided by `config.same_value` over
  ids (bytes, not origins). `reloads`/`rejections` say whether the file was read.
* **Change reports go through `config.render_value`**, so redaction is the declaration's
  decision in one place; there is no raw-value accessor on `Config`, on purpose.
* **It is `step`, not `poll`**: `poll` is `std/syspoll`'s C symbol and `syswatch` links it.
  `step` drains the watcher bounded (`LIVE_DRAIN_ROUNDS`) and answers `LIVE_QUIET` on a
  closed watcher forever; `reload_now` is the trigger-agnostic form.
* **A `Section` is a prefix VIEW onto the flat key space**, not a tree — `std/ini` already
  flattens `[server.tls]` to `server.tls.port`. It owns its prefix (`String`; a struct may
  not hold a `str`), so it has a `Drop` and is move-only.
* Not built: debounce (the caller's, `syswatch_demo` shows one), watching for the file to
  appear, schema hot-swap, env/cli parsing, a thread.

### ~~The registry layer~~ — **DONE: `std/manifest` + `std/registry`**

The two things the substrate note said were absent — a manifest FORMAT and a registry
PROTOCOL — are a one-rendering text format and a directory layout (`index`, `.manifest`,
`.tar`) that is the same over HTTP through `httpd`'s static route. `load` builds the
solver's table from text alone; `fetch` re-hashes against the index's promise before the
cache sees a byte. The end-to-end `jc add` shape is now a test: publish, load, resolve
minimally, fetch through the cache, and fetch the same package over HTTP. Not built:
signatures (a hash is not authentication), yanking, mirrors, publishing over HTTP.

### ~~TLS (area 8)~~ — **DONE: `std/tls`, by binding OpenSSL**

Contexts, blocking sessions over a `sysnet` descriptor, verification that includes the
HOSTNAME whenever a CA is trusted, an error surface. Four tests over a real handshake with
the client on a spawned thread (`concurrent { spawn … }` is how a handshake gets both ends).
Linking is content-triggered (`openssl/ssl.h` → `-lssl -lcrypto`, both drivers, after the
source); `CC_FLAGS` untouched. The test certificate is a checked-in self-signed
`localhost` pair under `examples/std/fixtures/` (minted with `openssl req -config`, since
Strawberry's `openssl.exe` cannot find its own default config). Not built: schannel,
non-blocking sessions (`sysnet` has no non-blocking mode), resumption, client certs, ALPN.
**The Linux runner needs `libssl-dev`; a host without it fails `tls_test` at link.**

### TLS (area 8) — the original entry, kept for its correction

This entry used to read: *"binding OpenSSL or schannel is a link-flag change, and `CC_FLAGS`
is attest-hashed, so `-lssl` churns every manifest in the corpus. That makes it exclusive of
all other work."* **Every step of that is false, and it was blocking the whole area.**

**A per-program link library never goes near `CC_FLAGS`.** The mechanism already exists and
is *content-triggered*, in `main.rs` at both link sites:

```rust
if c_src.contains("pthread")      { cmd.arg("-pthread"); }
if cfg!(windows) && c_src.contains("winsock2.h") { cmd.arg("-lws2_32"); }
```

TLS would be one more line of the same shape (`openssl/ssl.h` → `-lssl -lcrypto`). Note the
position rule the winsock comment records: GNU ld resolves left to right against objects seen
so far, so the library must come **after** the source file.

**Verified, not assumed.** `examples/std/http_demo.jtr` uses `sysnet`, therefore needs
`-lws2_32` to link, and its manifest is:

```
cc-flags -O2 -std=c11 -ffp-contract=off -fno-fast-math
```

The link library is not in it, and cannot be: `attest::manifest` writes
`crate::CC_FLAGS.join(" ")`, a constant. **Zero manifests move.**

The tree's actual rule is subtler than "link flags are hashed", and `cc_strict_flags` states
it: a flag that changes no emitted byte "has no business churning the provenance", while one
that changes *what gets linked* is exactly why `-D__USE_MINGW_ANSI_STDIO` must never reach
`CC_FLAGS` **in a real build**. Content-triggered link flags sidestep the question entirely by
never entering the constant.

**What remains is ordinary scope, not a gate.** Binding OpenSSL is real work — handshake,
certificate verification, an error surface, and a second implementation for schannel if
Windows is to be served natively rather than through mingw's OpenSSL. Judge it against the
other §2 items on size, not on a coordination cost it does not have. Nothing needs deciding
before anyone starts.

---

## §2b. WHAT IS LEFT — the consolidated register after the breadth pass (2026-09-06)

Everything in §1 and §2's first wave is done: A7, A6, A8, B1, A11, A12, B2; the package
substrate and its registry layer; storage V2; HTTP V2; TLS. **This section is the whole of
what remains**, so nobody has to reconcile §1, §2, the long note's §6A/§6B and the
CHANGELOG to find it.

### Unbuilt — the six second-wave modules (parallel-safe, none reseeds)

| module | what | the one thing to know first |
|---|---|---|
| **Crypto bindings** | HMAC, signing/verification, a hash interface | Bindings over `sha256`/OpenSSL, not algorithms; `std/tls` shows the `extern "openssl/*.h"` shape and the link rule is already in place |
| ~~**Trace spans**~~ | **DONE: `std/trace`** — see the entry below this table | |
| ~~**Service supervision**~~ | **DONE: `std/supervise`** (below) | restart policy, budget window, backoff, `stop_all`, events; no tree |
| **Sandbox** | cwd, process groups, fs capability projection onto a child | `sysproc.jtr:113` names all three; `fs.Fs` gates the parent and nothing projects it |
| ~~**Config live reload + nesting**~~ | **DONE: `std/livecfg`** (§2 above) | candidate-then-swap; a shadowed file value is not validated; `step`, not `poll` |
| **`std/plugin` on the pipe transport** | a plugin as a server on `sysproc.start_piped` | Tier 4 leftover; one-process-per-call only because the transport did not exist |

### ~~Trace spans~~ — **DONE: `std/trace`**

Timed, nested **segments** (the word `span` is taken three times) with attributes, from a
`Tracer` that is GIVEN a `time.Clock` and an exporter — `std/log`'s shape with nesting and
a stopwatch. Seven tests, none touching the OS; a pinned demo (`jtrace`,
`examples/std/trace_demo.jtr`); four mutations watched failing; no reseed (nothing in the
closure changed). What a successor should not re-derive:

* **The exporter is a `@copy` struct of a `*mut u8` and four fn pointers** (`on_open`,
  `on_str`, `on_i64`, `on_close` — a visitor over ONE finished segment), the `Allocator`
  shape. A trait would let `@no_alloc` pass vacuously. A caller's own exporter is four
  functions over a block of counters (`a_caller_can_write_its_own_exporter`).
* **Records arrive in END order with a parent id; the tracer never builds the tree.** The
  demo rebuilds it from the JSON with `json.get` by id — that is the consumer's job.
* **`end(t, id)` names the segment.** Inner segments still open are ABANDONED (discarded,
  counted, never exported) and `id` ends normally; an id that is not open is `unmatched`.
  An `end` that closed "the innermost" could never notice the early-`return` bug.
* **Attributes go on the innermost open segment, and a parent may add more after a child
  ended** (`status` after `db`). That keeps the arena a LIFO stack: names, keys and values
  are released when their segment ends, so a tracer's memory is its `make` size for life.
* **Four counters, because four fixes:** `dropped` (size the tracer), `truncated` (the
  segment still exports what fit; the caller saw `false`), `abandoned` (a mis-nesting),
  `unmatched` (a stale id). A freed tracer refuses without counting.
* **`to_text` and `to_jsonl` share one state and one record scratch**; a record is built
  whole and copied, or withheld and counted in `out_lost` — never a half line. Quoting is
  `log.needs_quoting` + `json.put_str`, so there is ONE answer to "what does a quoted logfmt
  value look like".
* **`to_log` takes the logger and uses two clocks**: `ts` is the logger's, `dur` the
  tracer's; `start` is not repeated in the record. The logger's level threshold still
  applies (a DEBUG tracer through an INFO logger ships nothing, and the logger counts it).
* **Ids are a per-tracer counter from 1; 0 means "no parent" and "refused".** A globally
  unique id is the shipper's job, not the code's.
* Not built: sampling, cross-process propagation, shared trace ids, batching, a wire
  protocol.

Plus the layer the breadth pass stopped short of: **the `jc add` command** over
`manifest` + `registry` + `resolve` + `lockfile` + `cache` (every piece exists and is tested
end to end; the command is the glue), and **request-body streaming** in `httpd` (a request
must fit the connection buffer today).

### ~~Service supervision~~ — **DONE: `std/supervise`**

A `Supervisor` given a `time.Clock`, a `sysproc.Spawner` and an `Allocator`; a bounded
struct-of-arrays child table; `Policy` (never/on_failure/always, N restarts in M ns, fixed
or doubling-capped backoff); `tick`/`poll_for`/`stop_all`; per-child `phase_of`/
`outcome_of`/`last_code_of`/`restarts_of`/`gave_up`/`due_of`; an event callback. Ten tests
over REAL children, a pinned demo (`jsupervise`, `examples/std/supervise_demo.jtr`, which
re-invokes itself as its children), four mutations watched failing, no reseed. What a
successor should not re-derive:

* **Two clocks.** The supervisor's clock is POLICY time; `poll_for(s, blk, budget)` takes
  the clock a caller can SPEND. Tests run policy on `time.manual(0)` and wait on
  `time.host()`; the demo's `t=…ms` lines are all policy time, which is why its transcript
  is exact.
* **The budget counts restarts, not exits**, in a ring of `SUP_HIST_CAP` (32) timestamps;
  `add` refuses `max_restarts` above the ring. N = 0 gives up at the first crash; N < 0 is
  unlimited; `window_nanos <= 0` means "ever".
* **A child changes phase at most once per tick**: reap → `SUP_WAITING` with `due = now +
  delay`, the NEXT tick starts it. A restart is recorded in the window at the time it
  HAPPENS (the start), not when it was decided.
* **A doubling delay resets to its base when the run outlasted the window** — the window
  is the one notion of "stable"; no second number.
* **A refused start is final** (`SUP_GAVE_UP` + `SUP_OUT_START_FAILED`), never retried.
* **`stop_all` is `terminate` + `wait_or_kill(…, 0)`** so the child is reaped and released
  whatever the kill reported; a waiting child is cancelled (`STOPPED` detail 0 vs 1 for a
  kill). `free` calls it — a supervisor cannot leave orphans behind.
* **Supervise with `add_exec`, not `add`.** `sysproc.start_exec(path, args)` (new, at the
  end of `sysproc.jtr`) starts the program DIRECTLY: `TerminateProcess` on `cmd.exe /c
  prog` kills `cmd.exe` and orphans `prog`. `args` split on single spaces, no quoting.
* **`import "core"` beside any fallible-fn module fails to type-check**: `core.Result`'s
  variants `ok`/`err` shadow the intrinsics ("a fallible function must return a result").
  Recorded as an open gap below; the demo renders numbers with its own digit loop.
* **Every probe over child processes needs a timeout**; a driver that `poll_for`s on a
  long-runner spends its whole budget. The test suite's `drive` is for quick children only.
* Not built: a supervision tree (a tree is a supervisor whose children are supervisors),
  graceful signal-then-kill shutdown, dependency order, per-child stdout capture.

### Compiler defects and gaps — OPEN

| id | what | status |
|---|---|---|
| **A13** | **Drop glue under COLLIDING type names.** Three `Writer` types in one closure; a struct holding `json.Writer` emitted `jestyr_impl_Drop__Writer__drop` (bare name, no such fn) → link error; had the names lined up it would have `fclose`d the wrong struct. AND the quiet half, found by the probe: a `file.Writer` local was never dropped at all in such a program (its canonical key found no impl), and `escape`'s consuming rule answered "no Drop" for it. | **CLOSED 2026-09-06, reference-only.** One defect, two sites: `typeck::register_impls` was the one item pass that never set `cur_mod`, so the impl target lowered in a stale module, degraded to `Opaque("Writer")` and was keyed under the BARE name; cgen's `aggregate_drop_fields`/`enum_drop_variants` matched the decl by bare spelling and lowered field types from the EMITTING module (`ast_type_to_ty_in(.., decl_mod)` now). The port was already correct — its loader renames colliding types in the token stream, so its typeck and cgen never see a bare `Writer` — and the two compilers agree byte-for-byte on the probe; **no port change, no reseed** (the drift guard said so). Probe: `examples/std/drop_collide_demo.jtr` (`jdropcollide`: a scope-dropped `file.Writer` proven by the file size read back, a `log.Logger` held from a module that never imports `json`) + `drop_glue_under_colliding_type_names_finds_the_right_impl_and_only_that_one` in `module.rs` + the demo in the port-vs-reference run comparison. `httpd_test`'s route-around (the dropped `sysfs` import) is no longer needed. |
| **A14** | The port's loader renamed a LOCAL sharing a colliding fn's name (`body`, `now`) — token-level rename by spelling. | **CLOSED this pass** (loader tracks the current fn's binders). Residual: a block-local binder is kept until the fn ends, so a fn-valued use of that name after the block would go unrenamed; no corpus program does it. |
| **Link rules in three places** | `-pthread`/`-lws2_32`/`-lssl -lcrypto` were content-triggered in `main.rs` twice and in `proptests::link_and_finish` once; `tls_test` linked under the driver and failed under the harness until the third copy learned OpenSSL. | **CLOSED 2026-09-06.** `main.rs::link_args(exe, cfile, c_src)` is the one rule — a pure function returning the argument tail (`-pthread`, `-o exe cfile`, `-lssl -lcrypto`, `-lws2_32` on Windows) so its ORDER is pinned by a unit test (`main.rs::link_rule`, three tests) with no compiler running. Both driver sites, `build_exe`/`build_and_run`/the test-mode runner, the fixpoint (which had had NO `-pthread`), the subset fixpoint, the inline-probe runner, the layout probe and the port's distinct-members test all call it; hand-written C probes (signal numbers, `windows.h` layouts, the SIMD lane oracle, the warning gate's compile-only probe) do not, on purpose — they are not emitted programs. **The port's driver keeps its own copy by necessity** (a different language); it is the remaining second site, and `jc_build_matrix` is what holds it to the same libraries. |
| **A4** | Windows: `capture` + print does not round-trip a child's bytes (text-mode stdout). | Recorded decision, not a bug to fix blind: binary stdout re-baselines many goldens. |
| **A10 / sanitizers** | No sanitizer has ever run over the emitted C. | CI-only: no `libasan` here. `selfhost_fixpoint_subset` is the harness. |
| **Second C compiler** | Only gcc has ever compiled the output. | CI-only: no clang here; MSVC takes neither `-std=c11` nor `-Werror=`. |
| **`#line` gap** | The port emits no `#line`; `jestyrc attest` and `jc attest` disagree on `c-sha256` for a module-path build. | Invisible to every golden (they strip `#line`). Needs a module-path C golden first. |
| **Multi-bound generics** | `fn f[T: Hash + Eq]` does not parse. | Why `hashmap` stores fn-pointer hash/eq. P2 dump goldens are curated snippet lists — add the snippet by hand. |
| **Generic aliases** | refused; no way to newtype a container. | Why `std/set` is free functions over `HashMap(T, bool)`. |
| **Uninitialized memory** | no facility; containers carry fake defaults (`smallvec.jtr:77`). | The hard part is the destructor rule for partially initialised aggregates. |
| **`\u00XX` below 0x20** | passes through to the emitted C verbatim; C rejects it. | Small; lexer, both sides. |
| **`check` is quiet on an undeclared bare name** | a typo (`eid` for `id`) passes every front-end check and dies in gcc. | Leniency exists for fn-pointer values and extern symbols; a rule refusing a name found in none of scope/consts/variants/fns/externs/globals is a two-sided `escape` item. |
| **`alog.jtr` header debt** | `alog.Cursor` is move-only by containment; the header does not say so. | A comment, owed on the next change to that file. |
| **`std/cstring` has no `cstr` view** | a C string cannot be read back as `str`; `tls.protocol` was dropped for it. | Bind `strlen`, build a slice; small. |
| **`import "core"` shadows the `ok`/`err` intrinsics** | `core.Result`'s variant constructors `ok(v)`/`err(e)` win over the intrinsics in EVERY module linked beside `core`, so a program importing `core` and any module with a fallible fn (`sysproc.start`) fails `check` with "a fallible function must return a result" at the other module's `return ok(…)`. Found by `supervise_demo` reaching for `core.format_i64`. | No corpus program imports `core` beside a fallible module today (`numbers.jtr` imports `core` + `io` only). Either the variants or the intrinsics have to yield; the intrinsic-shadowing rule (A5) refuses a `pub fn ok` but not an enum VARIANT. Two-sided (`escape`). |

### Known flake

`jstatus_serves_a_connection_without_starving_its_timers`: 1ms timer, 500ms budget. Use
repetition on a quiet machine; "passes in isolation" is the wrong discriminator; do not
widen the deadline.

### Verification owed to a machine not in reach

The Linux ladder has not run since `std/kv`. **New this pass:** `tls_test` links
`-lssl -lcrypto` (the runner has `libssl-dev`; a host without it fails at link), and every
socket suite (`httpd_test`, `tls_test`, `registry_test`) is a loopback transcript that has
only ever run on Windows.

## §3. COORDINATION — what makes §2 safe

**The seed is the global lock.** These sixteen modules are the compiler's own closure:

```
mem  intern  fs  env  list  tokens  parser  ctfe  typeck
intrinsics  escape  sha256  sink  width  diag  cgen
```

Editing ANY of them forces `REFRESH_SEED=1`, which rewrites ~74,000 lines across
`bootstrap/jestyr_flat.jtr` and `bootstrap/jestyr_seed.c`. Two sessions reseeding
concurrently produce a conflict nobody can hand-merge.

**Rules for a parallel session:**

1. **Do not edit any of the sixteen.** Import them freely; a new leaf module that imports
   `fs` or `sha256` is fine and needs no reseed.
2. **If you find you need a compiler change, STOP and report it.** Do not reseed. Add it to
   §1 instead. This is the most likely way a parallel session turns serial.
3. **Run the drift guard rather than the heuristic.** The standing rule is "reseed on any
   `examples/std` change"; the guard is the authority and has twice said no reseed was owed:
   ```bash
   cargo test --release --features "c-oracle,selfhost-fixpoint" bootstrap_seed_is_current
   ```
4. **A new corpus file appends one line to `docs/jc_build_matrix.txt`.** Regenerate with
   `JC_BUILD_MATRIX=1` and **read the diff** — it is hand-maintained on purpose. Conflicts
   there are line-local and trivial.
5. **Corpus-size assertions are lower bounds** (`> 100`), so new files break nothing.
6. **Everyone edits the handoff note and CHANGELOG.** Text conflicts, mergeable, but this is
   the real friction cost — which is why 3–4 concurrent sessions is the recommendation and
   8 is not.

**Cross-cutting defects do not parallelise.** The cc-flag duplication found this session
touched four places in two languages. A session that finds one like it must own the whole
fix or leave it fully recorded — a partial de-duplication leaves the hazard exactly where it
is hardest to see, which is what happened and cost an extra CI round trip.

---

## §4. THE RULES THIS TREE KEEPS RELEARNING

Read these before writing a test. Each cost a real failure.

**A rule that changes nothing owes a probe that it CAN fail — and the probe must be watched
failing.** A whole-corpus sweep that is byte-identical before and after means every golden
would pass with the port unmirrored.

**Verification on one platform is often vacuous.** This bit three times in one session: the
`-Werror` gate (1,662 warnings were a mingw false positive, proven only by RUNNING the
program), the platform defines (the missing half was POSIX, invisible from Windows), and a
line-terminator stripper (`cfg!(windows)` is true here, so the broken branch was
unreachable). **When a branch cannot run on your host, make it reachable** — take the
platform as a parameter, or assert on the emitted C where `@cfg` puts both arms.

**The obvious test for a portability fix is often the vacuous one.** A POSIX child used to
get `PATH` alone, so "the child sees `PATH`" passes against the broken and the fixed build. A
fix that WIDENS something needs a value outside the old width.

**"Owed to the Linux ladder" can mean "owed to a test nobody wrote."** A2 sat as
blocked-on-a-machine for an entire arc while the machine ran that code green. Before
recording an item as infrastructure-blocked, check whether the assertion exists at all.

**A recorded COST can be as wrong as a recorded diagnosis, and it blocks more.** TLS sat
as "exclusive of all other work" on the claim that `-lssl` would churn every attest manifest.
It would not: per-program link libraries are content-triggered in `main.rs` (`-pthread`,
`-lws2_32`) and never enter `CC_FLAGS`, which is the constant the manifest prints. One
`jestyrc attest` on a socket-using corpus file disproved it. **A wrong diagnosis costs the
session that hits it; a wrong cost estimate costs every session that reads it and moves on.**
Sanity-check a recorded blocker before treating an area as gated.

**A recorded diagnosis is worth less than a recorded symptom, and can be worth less than
nothing.** Three times in one session the note's *conclusion* was right and its *mechanism*
wrong, sending the fix at something far larger than the real one. Re-measure before
designing around a recorded explanation.

**A7 then made it four, and added a sharper edge: a CORRECTION can be worse than the error it
replaced.** The entry's original `mut` diagnosis was right; the later "actually the boundary
is argument position, not mutability" was wrong, and it cost more than the original ever did
— it pointed at the parser, invoked the no-allowlist P2 golden, and made a two-line `cgen`
fix look like a mandatory-mirror parser change. **Thirty seconds of running the claim would
have killed it**: the corpus file the entry sat next to, `examples/slice_range.jtr`, had
shipped a range sub-view in argument position all along. Before rewriting a defect's
mechanism, run the sentence you are about to delete.

**A defect that wears two faces gets recorded as the smaller one.** A7 and A6 were each filed
by their most visible symptom, and each hid a second failure with a different message, a
different phase, and sometimes a worse consequence. A6's register line was "check passes, run
fails" — true of the cgen half, while the typeck half failed AT check, and while the port
lowered the same construct to a silent `int`. When closing an item, probe every position and
every convention the construct admits (`read`/`mut`/`take`, parameter/return/local/nested,
struct receiver and primitive receiver) before believing the recorded shape is the whole of it.

**"It costs nothing today" is a claim about CONSUMERS, and it goes stale when one is added.**
`check_fn` left `Self` opaque on the reasoning that assignability is lenient on `Opaque`. That
was true and stayed true — but the escape checker's `Unknown` backstop is a second consumer of
the same fact, and it refuses rather than shrugs. A deferral justified by "the only thing that
reads this is lenient" is only as good as the word *only*.

**A grandfathered exception is a claim with a timestamp.** "Those two have not been bitten
yet" was reasonable when written and stayed on the page while the hazard fired twice more.

**Read the code that WRITES a table, not the comment describing it.** The port's header calls
enum payloads "payload TyIds also in `tch`"; they are 3-tuples. Indexing them as a flat run
made a mirror that silently did nothing.

**A comment saying "validated by the reference" is a divergence with a note attached.** Grep
the port for that shape — each one marks a pass the self-hosted compiler is trusting but
does not have.

**Never round-trip a source file through PowerShell `Get-Content`/`Set-Content`.** It reads
UTF-8 as ANSI, so every em-dash becomes mojibake, and it adds a BOM. Use the editor. Note
`python3` is not on PATH here; `python` is.

**`.jtr` subset traps:** a for-condition cannot start with `(`; a bare `{` after a call-init
parses as the ctor form; never chain `string_view(x).len`; `out`, `read`, `take`, `error` and
`spawn` are keywords (a parameter named `out` is thirty-two parse errors, none of which says
"keyword"); a `catch { … }` block may not contain a `return` — set a flag and test it; a
fn-pointer argument is spelled `&name`; `else` must follow `}` on the same line.

**`from_utf8` TRAPS; `try_from_utf8` is the checked one.** A `str` is UTF-8 by construction,
so bytes above 127 cannot pass through a `String` — record framing goes in a `[]u8` buffer.
The trap is a runtime assertion, not a diagnostic, and the message names the runtime, not
your line.

**A file size read while a `file.Writer` is open lags the writes.** They sit in the C
library's buffer until a sync or a close. A test measuring "before" against a live log
measures the buffer, not the file; sync first.

**A suite is not registered until its count is in `io_suites_pass`.** Four landed green and
gated nothing.

**A mutation probe over a SOCKET suite can hang, and the hang is the verdict.** Two of
`httpd`'s five mutations left a client blocked on a response the broken server never sent.
A probe harness needs a per-run timeout that counts as FAILED, and on Windows it must kill
the test binary itself — `subprocess.run(timeout=…)` kills the compiler it launched, then
blocks on the pipe the grandchild still holds. Watched twice before it was understood.

**A test that asserts only through the wire cannot see what happened behind it.** Ignoring
middleware `DONE` passed the first `httpd` suite: the client still saw the 401, because the
handler that ran afterwards had its response refused as a duplicate. Only a handler that
records that it RAN (through its context pointer) made the mutation visible. When a probe
passes, ask what the assertion could not observe.

**`jestyrc check` is QUIET on an undeclared bare name.** A `Name` that resolves to nothing
types as `Unknown` ("a function name or external symbol: stay quiet"), so a typo in a `.jtr`
source — `eid` for `id` — passes every front-end check and dies in gcc with `'j_eid'
undeclared`. In the compiler's own modules that means a green `check` on `typeck.jtr` is not
evidence it builds; run the golden (or `jc build`) before believing an edit to the sixteen.
The leniency exists for fn-pointer values (`&make`) and extern symbols; a rule that refuses
a name found in none of scope, consts, variants, fns, externs and globals would close it
and is a two-sided `escape` item by the usual argument.

**`cmd.exe /c` strips the outer quotes off a command line that BEGINS with one**, so quoting
a program path — the spelling that looks obviously correct — mangles the rest of the line.
