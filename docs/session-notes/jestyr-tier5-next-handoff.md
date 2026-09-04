# Tier 5 — what to do next

Cold-start note, ordered by what should be done FIRST. §1 is the serial work (compiler
defects — one session at a time). §2 is the parallel work (library breadth — fan out).
§3 is the coordination rules that make §2 safe.

```bash
cargo build --release && cargo test --release --features "c-oracle,selfhost-fixpoint"
```

**1354 passed / 0 failed / 3 ignored.** Pushed. **CI is fully green** — all
four jobs — for the first time in over two weeks.

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
failing. Area 10 is complete. The register's remaining entries are a decision (A11), a
measured deferral (B2), and two CI-only items nobody in reach can watch (A10's sanitizer
half, a second C compiler) — see §1.

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

> **THE SERIAL QUEUE IS EMPTY OF ACTIONABLE WORK.** 1.1 (A7), 1.2 (A6), 1.3 (A8), 1.4 (B1)
> and **A11's lowering half** are all done — no `mut` argument leaks a gcc error any more.
> What is left here is **one language QUESTION** (should a `mut` argument that aliases nothing
> be refused? — §1.1b) and **1.5 (B2), deferred on measurement**, bigger than everything above
> it combined. Neither is a "pick it up and go" item.
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

**Still open — a language question, not a lowering one.** Should a `mut` argument that aliases
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

### 1.5 — B2: `extern` binding a C global — MEASURED, then deferred

The principled answer for foreign globals, and no longer needed for anything urgent
(`environ` went through an intrinsic instead). **Measured before deferring:** a new item kind
means 252 `Item::` match sites across nine reference files plus 42 in the port, and it must
also reach `attest` (a global is ABI) and `doc`. Larger than everything above it combined.

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

**NEXT in this chain: the lockfile**, then the content-addressed cache. `sha256` and the
`attest` manifest shape are the precedent for the first; `tar` already exists for the second.
Note that under MVS a lockfile is a *verification* artifact rather than a correctness one,
which should simplify it — it records what was selected and lets a build prove it got the
same answer, rather than being the only reason two builds agree.

### 2.2 — HTTP V2 (area 6)

The parser is hardened and refuses request smuggling; **everything above the message is
absent**. Routing, middleware, streaming bodies, keep-alive, timeouts, static files, access
logs, a test client/server. `sysproc` timeouts and `syspoll` readiness are in place under it.

### 2.3 — Storage V2 (area 9)

KV, compaction, atomic batches, migrations, backup/export on top of `alog.jtr` (which is
CRC'd and crash-recoverable). Note `sysfs` has **no mtime**, deliberately — `struct stat`'s
layout differs per platform.

**Available for a second wave, same rules:**

| work | note |
|---|---|
| Crypto: HMAC, signing, a hash interface | New modules importing `sha256` read-only. These are BINDINGS, not algorithms to write — `csrand` deliberately invents nothing. |
| Trace spans | **Pick another word first.** `Span` is taken three times: `http.Header` spans, `diag` source spans, and the `@span` work-span attribute. Use the fn-pointer vtable shape, not a trait — `@no_alloc` passes vacuously through a trait method. |
| Service supervision / restart policy | The lifecycle is complete; a supervisor over `std/sysproc` is its own module. |
| Sandbox: cwd, process groups, fs capability projection | `sysproc.jtr:113` names all three. `fs.Fs` gates the parent; nothing projects it onto a child. |
| Config: live reload, nesting | `std/syswatch` exists; composing them is the caller's job today. |
| Rewrite `std/plugin` as a server on the pipe transport | Tier 4 leftover. One-process-per-call only because the transport did not exist; it does now (`start_piped`/`capture`). |

### TLS (area 8) — **NOT exclusive of other work.** The recorded blocker was wrong.

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
`spawn` are keywords.

**`cmd.exe /c` strips the outer quotes off a command line that BEGINS with one**, so quoting
a program path — the spelling that looks obviously correct — mangles the rest of the line.
