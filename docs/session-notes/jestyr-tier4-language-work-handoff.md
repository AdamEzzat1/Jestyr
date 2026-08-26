# Tier 4's language queue — three of four items, and what the fourth costs

Cold-start note. **§0 is what to do next.** Then the four increments (§1–§4), what is left
(§5), and the traps this arc bought (§6).

```bash
cargo build --release && cargo test --release --features "c-oracle,selfhost-fixpoint"
```

**1307 passed / 0 failed / 3 ignored**, and `jc_build_matrix` is **63 of 63**
(`sysproc_demo` joined it). The 3 ignored are the deliberate slow numeric sweeps. Record
the count before changing anything; if a failure appears later, assume it is yours.

It was 1279 at the start of this arc — the arc's four commits are `3cd0126` (`@must_use`),
the `std/sysproc` commit, `6233869` (`@move`), and the `@cfg` specificity commit.

The predecessor note is `docs/session-notes/jestyr-std-v4-tier4-complete-handoff.md`; its
§4 is the queue this session worked, in its own leverage order.

---

## §0. START HERE

The Tier 4 note's queue was four items. **Three are done and the fourth is half done:**

| § | item | status |
|---|---|---|
| §4.2 | `@must_use` on a non-union return | **DONE** |
| §4.1 | a `sys` process module | **DONE** — `std/sysproc`, spawn + bounded wait + kill |
| §4.3 | move-only resources (brief §2.1) | **DONE** — `@move`, all eight handle types |
| §4.5 | the `@cfg` specificity rule | **DONE** — `linux`/`macos` exist |
| §4.5 | the extern declared-alias | **NOT STARTED** — see §5.1, it is the expensive one |

**What to do next is §5.1 or §5.2**, and they are very different sizes. Read §5 before
picking.

---

## §1. `@must_use` stops being a suggestion the C compiler enforces

The attribute was accepted by `attrs.rs`, lowered to
`__attribute__((warn_unused_result))`, and then **never looked at again**. Whether
ignoring the value was diagnosed depended on which C compiler built the emitted C and at
what warning level; `jestyrc check` said nothing at all.

`FnSig` carries `must_use`; the call-resolution paths record it into a dense side table
beside `expr_types`; the discarded-statement seam reads it, ordered AFTER the fallible arm
so a `@must_use fn f() -> T !E` produces ONE diagnostic and it is the one naming the error
set.

### FOUR resolution paths, and the number was measured

The rule was written against the three that hold a `FnSig` — unqualified, `mod.f(…)`, UFCS
method. Then a probe of the two method forms the attribute's own target list advertises
found `@must_use` on a **struct-body method** doing nothing at all: those never get a
`FnSig`, so `resolve_struct_method` reads the `FnDecl` directly.

**Trait methods stay uncovered on purpose.** `wrap_trait_ret` records the principle — a
call through a trait is typed by the TRAIT's signature, whichever impl answers — so an
impl-side `@must_use` would make one trait call must-use for some receivers and not
others. The attribute belongs on the trait, and `TraitMethod` has **no `attrs` field**, so
it is an AST + parser increment. `a_trait_impl_method_is_not_covered_yet` asserts the
current behaviour so the day someone closes it, the test says so.

Reference-only, like the fallible rule: no Error type, no C change, so no port mirror and
no reseed.

---

## §2. `std/sysproc` — a child that runs beside its parent

`std/process` goes through `system()`, which blocks until the child exits. That one fact
was the ceiling: no timeouts anywhere, no long-lived plugin server, no concurrent loopback
child.

`start` / `try_wait` / `wait` / **`wait_timeout`** / `terminate` / `release`, plus
`wait_or_kill` which composes the three — because a `terminate` without a following `wait`
leaves a zombie on POSIX. `wait_timeout` reports "still running" as a STATE rather than an
error, so a caller that wanted to give up now knows it may.

### The claim is measured, not asserted

`jbounded` (`sysproc_demo.jtr`) starts a ~2000ms child and times `start` itself: **29ms**.
Under `system()` that number IS the child's runtime, so the boolean it prints is the whole
subject of the program. A `print_str("the parent kept running")` would have passed before
the module existed.

### The bounded wait polls on BOTH platforms

Windows has a native timed wait and POSIX has none. Polling on both makes the loop one
piece of code driven by one `time.Clock`, so `time.manual()` exercises the timeout logic in
microseconds instead of seconds — and only "is it alive" and "what did it exit with" are
`@cfg`-split. `wait` (unbounded) does NOT poll; it hands off to the platform's blocking
wait.

### Three things it turned up

* **`spawn` is a KEYWORD.** The concurrency task form, so the process module cannot spell
  its central verb — it is `start`. Fifth on the list after `read`/`take`/`error`/`out`,
  and the first that the declared-alias design (§5.1) would NOT fix: this is a Jestyr name,
  not a C symbol.
* **`GetExitCodeProcess` cannot answer "is it running".** `STILL_ACTIVE` is **259** and a
  child may legitimately exit with 259. Liveness is `WaitForSingleObject(h, 0)`, which has
  no in-band value to collide with. Pinned against the real header along with the
  `STARTUPINFOA`/`PROCESS_INFORMATION` offsets — and the constants are **parsed out of the
  shipped source**, because a test that hard-codes 104 beside a module that hard-codes 104
  proves only that someone typed it twice.
* **A null environment means two different things.** `CreateProcessA` INHERITS the
  parent's; `posix_spawn` hands the child an EMPTY one, because it reaches `execve`
  unchanged. "Pass null on both" is not symmetry, it is an asymmetry that happens to
  typecheck. POSIX forwards `PATH`; the residual difference is documented, and closing it
  needs an `extern` that can bind a GLOBAL (`environ`).

### A new corpus-wide guard

`one_c_symbol_has_one_signature_across_the_whole_std_corpus`. `syswatch` and `sysproc` both
need `WaitForSingleObject`, and **typeck keys its function table on the BARE name**, so a
second declaration with a different signature type-checks the OTHER module's call sites
against this one's idea of it. `sysnet`'s header records that being measured
(`expected i32, found u32`). Verified by BREAKING it.

---

## §3. `@move` — the eight OS handles stop being freely copyable

Brief §2.1. The ownership rules were gated on `droppable_ty`, so only a type with a `Drop`
impl could be consumed by `take` or by rebinding. Every handle in the `sys` tier is a plain
struct around an integer — `Socket`, `Dir`, `Reader`, `Writer`, `Watcher`, `alog.Log`,
`plugin.Host`, `sysproc.Child` — so all eight were freely copied. `Socket` was `@copy`,
under a header that stated the hazard and then said the language could not express it.

### The recorded obstacle was right, and it is not an obstacle to `@move`

"Giving them a `Drop` is not the fix" is true, and the resolution is that **duplication and
automatic teardown are two separate properties**. Only the first is unsound. A `Drop`
closes at every scope exit, which is wrong for a handle deliberately passed around — and
these handles close FALLIBLY, so an implicit drop would discard exactly the verdict
`@must_use` exists to preserve. `@move` grants "may not be duplicated" alone.

`escape.rs`'s three ownership sites now consult
`owns_resource = droppable_ty | move_only_ty`. Two diagnostics were corrected because they
had become false: "the new name owns it and will drop it" and "has already dropped it" both
describe a destructor a `@move` type need not have.

### Adoption broke nothing, and that is the dangerous part

Every corpus use of all eight passes them by BORROW. So the whole-corpus escape golden
would pass with the port missing the rule entirely — the rule is therefore carried by
differential probes and by a registration test that goes through `check_program` rather
than grepping for the attribute text.

### And the differential immediately found a pre-existing half-mirror

**The port has never had the REBINDING rule at all** — "moved to another binding" appears
nowhere in `escape.jtr`. The reference has had it since v3; nothing in the corpus rebinds a
droppable; so the two sides agreed by producing the same empty answer for different
reasons, and every golden was blind to it. Same shape as the recorded `@size_of` gap.

Implemented, with the move CAUSE threaded through the port's consumed-marks
(`ECONS_TAKE` / `ECONS_REBIND`) so both messages match character for character.

---

## §4. The `@cfg` specificity rule — `linux` and `macos` exist

The predecessor note said this was "NOT `add linux to CFG_WORDS`", and it was exactly
right about why: **`posix` is a SUPERSET of both**, and the vocabulary was a closed list of
guards disjoint by construction, so two nested names would make `@cfg(posix) fn f` and
`@cfg(linux) fn f` BOTH emit on Linux — a duplicate definition.

The increment is the specificity rule. The narrower item keeps its own guard; the wider one
is emitted with **every narrower sibling of the same name subtracted**:

```
@cfg(linux)   →  #if defined(__linux__)
@cfg(macos)   →  #if defined(__APPLE__)
@cfg(posix)   →  #if !defined(_WIN32) && !defined(__linux__) && !defined(__APPLE__)
@cfg(windows) →  #if defined(_WIN32)
```

* `cfgs_are_disjoint` → **`cfgs_may_share_a_name`**. The body is still `x != y`, but that
  is no longer *because* they are disjoint — the name stopped claiming something untrue.
* `cfg_siblings` is keyed the way the C symbol is: a fn by its CANONICAL name, an extern by
  its bare name. Cross-module same-named fns become different symbols and must not subtract
  from each other.
* **A `#include` gets no specificity key.** Two headers guarded by overlapping `@cfg`s do
  not collide the way two definitions do — `<sys/inotify.h>` under `@cfg(linux)` and
  `<unistd.h>` under `@cfg(posix)` must BOTH be included on Linux.

### The proof is the real preprocessor

`exactly_one_definition_survives_on_every_platform` lifts the guards cgen ACTUALLY emitted
out of its output and hands them to `cc -E` under four macro sets — windows, linux, macos,
and **a POSIX that is neither**, which is the case an over-eager subtraction silently
deletes. Exactly one definition must survive each.

`a_disjoint_pair_emits_exactly_what_it_always_did` is the other half: nothing about the old
vocabulary moved, which is what let this land without re-baselining every golden.

### Nobody adopted it, and that is deliberate

`sysdir`'s `D_NAME_OFFSET` and `syswatch`'s inotify branch were the two recorded callers.
Both headers are updated, and **both still decline** — for a reason that has changed. It is
no longer that the language cannot say it; it is that a macOS branch nobody can run on
either machine this project builds on is an untested claim, which is the failure mode both
headers argue against. `examples/cfg_platform.jtr` carries the demonstration instead, which
is what puts the PORT's copy under byte-identity.

---

## §5. WHAT IS LEFT

### §5.1 — The extern declared-alias. The expensive one, and still worth it

```jtr
extern "unistd.h" fn sys_read = "read"(fd: i64, buf: cptr, n: usize) -> i64
```

`read`, `take`, `error`, `out` and now `spawn` are Jestyr keywords, so an extern cannot
bind a C symbol that shares one of those spellings — `std/syswatch` binds `readv` instead.
The alias would also **unwind the four separate `close`es** now across `std/file`,
`std/sysdir`, `std/sysnet` and `std/sysproc`'s `CloseHandle`.

**Cost, honestly:** parser + typeck + cgen, so it owes the P2 (parse dump), P3 (typeck) and
P5 (cgen) goldens plus a reseed. That is the full three-golden tax, and it is why this
session did not start it — the other three items were each one or two goldens.

Note it does NOT fix `spawn`: that is a Jestyr function name, not a C symbol. Scoped
keywords are a separate question.

### §5.2 — Pipes for `std/sysproc`, and the plugin server they unblock

The module deliberately stops before capturing a child's stdout. That is the increment that
turns `std/plugin` from one-process-per-call into a server, and it needs `CreatePipe` /
`pipe(2)`, inherited handles, and on POSIX a `read(2)` this language cannot bind by name —
so **§5.1 is its prerequisite**, or `readv` is again the way through.

### §5.3 — A `mut` sub-slice as a call argument

Unchanged from the predecessor note. `writeinto(buf[a .. b])` where the parameter is
`mut []u8` is a proper compile error ("the C backend does not support ranges yet"); the
`read` equivalent and a `let` initializer both work. The `[]T` sub-view lowering already
exists — the `mut` argument path takes a different route that reaches `emit_expr` with a
bare `Range`.

### §5.4 — `environ`, or an environment intrinsic

§2's recorded asymmetry. An `extern` binds functions; inheriting a POSIX child's full
environment needs a global.

### §5.5 — Brief §2.4 and §2.7

Runtime ownership and concurrency-with-ownership. Untouched. **§2.1 is now closed**, so the
brief's language column is 3 of 6.

---

## §6. TRAPS THIS ARC BOUGHT

**A rule that changes nothing owes a probe that it CAN fail.** `@move` broke no corpus file
and `one_c_symbol_has_one_signature…` passed on its first run. Both were then verified by
deliberately breaking them. Without that, "it passes" and "it is vacuous" are the same
observation.

**A golden that passes is evidence about the CORPUS, not about agreement.** The port's
missing rebinding rule survived every golden for two workstreams because no corpus file
rebinds a droppable. Whole-corpus diagnostic-set comparison cannot see a rule neither side
fires.

**Check the identifiers before believing the diagnosis.** `pub fn spawn` does not parse;
`var out: String` does not parse. The second cost a confusing parse-error cascade in the
middle of a cgen change that had nothing to do with names.

**A cited test must exist.** `sysnet.jtr`'s header says a relied-upon sentinel coincidence
is "CHECKED" by `the_invalid_socket_sentinel_is_the_same_on_both` — **that test is nowhere
in the tree.** Found by grepping for it while writing an analogous pin. A claimed guarantee
with no test is worse than an acknowledged gap. (Flagged as a separate task; still open.)

**Two modules must not bind one C symbol with two signatures.** Typeck keys on the BARE
name, so the second declaration wins and the FIRST module's call sites are then checked
against it. Now swept corpus-wide.

**A layout constant is a claim about a foreign struct.** `sysproc`'s Windows offsets were
measured with a C probe, and the test re-measures them against `<windows.h>` while parsing
the constants out of the shipped source — so a wrong number is a red test rather than a
`CreateProcess` that mysteriously starts nothing.

**Do not edit `examples/**` while a ladder is running.** The corpus tests read those files
at run time, so an edit underneath a running suite corrupts it. `src/**` is safe once the
test binary is built.
