# Tier 5 — reliability, distribution, production operations

Cold-start note. **§0 is what to do next.** Then the baseline story (§1), the three
increments (§2–§4), what the research turned up (§5), what is left (§6), traps (§7).

```bash
cargo build --release && cargo test --release --features "c-oracle,selfhost-fixpoint"
```

**1336 passed / 0 failed / 3 ignored.** It was **1319** at the start of this arc — but see
§1, because on Windows the recorded baseline was never actually green.

**On `master`** (fast-forwarded, 18 commits). Both comparison suites green: 15/15 C++ pairs
with `static_rejections` still refused, and all 10 Rust-vs-Jestyr rejection probes refused.

| commit | what |
|---|---|
| `8fcdd1f` | the bounded runner's transcript survives a child that speaks CRLF |
| `a883bd7` | a consumer can be told the producer is done, instead of told a count |
| `39c2583` | a wrapper owns what it wraps |
| `51d2a7e` | the Tier 5 arc gets its cold-start note |
| `a8dde2d` | observability a service is given, not one it finds |
| `ad2dcc8` | a spawned task may not hold a writable reference to a shared binding |
| `b970b4c` | precedence is a property of the source, not of when it was applied |
| `a926da3` | a service can be told to stop by the operating system |
| `cdfa1f1` | the breaking-change gate stops passing a removed trait method |
| `bdb318f` | entropy from the OS, and a comparison that does not leak how far it got |
| `62ea1c2` | a refused send hands the value back instead of eating it |
| `9139900` | a select whose channels are all closed and drained stops waiting |
| `ad52e07` | the ExprId divergence is isolated to one node per select arm |
| `bfcf0f6` | a posix child inherits its parent's whole environment |
| `6560c8b` | a config error points at the line that caused it |

The predecessor note is `docs/session-notes/jestyr-tier4-language-work-handoff.md`.
The research note this arc produced is
`docs/session-notes/jestyr-tier5-systems-language-research.md` — read its §0 shortlist
before picking anything.

---

## §0. START HERE

Tier 5's definition of done opens with *"a service can start, report health, run
background work, **shut down gracefully**, and be tested deterministically"*. §2 removed
the reason the last clause was impossible. The next items, in leverage order:

1. ~~**Signals**~~ — **DONE** (§3g), via an intrinsic rather than the extern-global this
   note originally proposed. See §3g for why those are two different mechanisms.

   **`environ` is still open and is now the only thing wanting an extern-bound global.**
   It is why a POSIX child inherits only `PATH` while a Windows child inherits the parent's
   whole environment (`sysproc.jtr:120`).
2. **`select` needs a default arm.** §2 gave receivers an EOF but did not fix `select`,
   which polls `channel_len_i64(ch) > 0` and therefore spins forever over closed, drained
   channels. This is the other half of a terminating worker loop.
3. **Refusing a send on a closed channel** — deferred deliberately in §2, and the reason
   is an ownership question, not effort. See §6.1.
4. ~~**Observability metrics**~~ — **DONE**, see §3b. What is left on top of it is **trace
   spans**, and note the name is taken three times over (`http.Header` spans, `diag` source
   spans, and the `@span` work-span attribute), so pick another word before writing a line.
5. ~~**The config merge layer.**~~ — **DONE**, see §3f. What is left on top is a FILE
   FORMAT: `apply` takes name/value pairs, so a TOML or INI reader handing them over is the
   next increment, and `diag.jtr` is what gives its errors real source spans (this module
   records which SOURCE a value came from, not which line of which file).
6. ~~**A `Metrics` consumer inside a real service.**~~ — **DONE**, see §3d.
7. ~~**Refusing a send on a closed channel**~~ and ~~**`select` termination**~~ — both
   **DONE** (§3g).

**§6 is the complete register of what remains** — §6A everything that is WRONG (defects,
divergences, unverified claims), §6B everything ABSENT. Read 6A first: an absent feature is
a plan, a defect is a liability.

The three at the top of it:

1. ~~**A1 — the ExprId drift and the typeck disagreement behind it.**~~ — **CLOSED (§3j),
   and it was never a typeck problem.** `cgen.jtr`'s `ref_expr_id` is a deliberate shim that
   already reconciles the two parsers' block-node allocation for six constructs; `select`
   was missing. One `else if`, port-only. §3h's diagnosis and its measurement table were
   both wrong — read §3j before trusting anything §3h says.
2. ~~**A5's OPEN HALF — the port accepts a program the reference now refuses.**~~ —
   **CLOSED (§3l).** Both toolchains refuse it, the name set is a shared leaf module
   (`examples/std/intrinsics.jtr`), and the sets are compared differentially rather than by
   eye. The transferable part: `typeck.jtr` HAS NO DIAGNOSTIC CHANNEL, so any rule that
   must be mirrored belongs in `escape` on both sides — which is where the next two-sided
   static rule should go without rediscovering this.
3. **A10 — the WARNING half is DONE (§3i); the SANITIZER half is not.** The item was
   recorded as one gap and was really two: warnings need no runtime library and are now a
   gate (277 emitted programs clean, watched refusing a broken `cgen`). Sanitizers still
   need a `libasan` this machine lacks.
   **CORRECTION to an earlier version of this note**, which called a second C compiler the
   cheapest win here: **there is no clang on this machine** — no `clang`, no `clang-cl`,
   not even `cc`; only `gcc` resolves, so `find_c_compiler`'s first-match-wins never had a
   choice to make locally. A clang leg is therefore a CI-only increment inheriting the A2
   caveat, exactly like the sanitizer, and not the cheap local win it was billed as. MSVC
   is installed but takes neither `-std=c11` nor `-Werror=`.
4. **B3 — the package substrate.** The largest genuinely-absent area, and the one the
   tier's own "distribution" theme names.

**Half of the Tier 5 brief already exists on master.** Before building anything in it,
read the inventory summary in §5 — this repo has twice rebuilt features that were already
there.

---

## §1. The baseline was RED, and had been on Windows since Tier 4

`cargo test` reported **1318 passed / 1 failed** on a clean tree at the exact commit whose
note records 1319/0. The failure was
`jbounded_kills_a_command_that_outlives_its_deadline`, and it was **not** mine and **not**
timing — which is what it looks like, since the test asserts a 200ms deadline and the
first run happened alongside two subagents. Reproducing it in isolation killed that guess.

The child sorted correctly and every boolean in the transcript was true. The only
difference was line endings:

```
left  (actual):   apple\r\nfig\r\npear
right (expected): apple\nfig\npear
```

For a CR to *survive* a `.replace("\r\n", "\n")` the wire bytes have to be doubled, and
they are — measured, `61 70 70 6C 65 0D 0D 0A`. Windows `sort.exe` emits `apple\r\n`, the
pipe carries it verbatim (which is the module's whole claim and is correct), and then
Jestyr's **text-mode stdout** turns the `\n` into `\r\n` while leaving the child's `\r`
alone. The corruption is doubled and the normalization is not.

**The sweep was the work, not the one-line change.** There are ~55 CRLF normalizations in
`proptests.rs` and 54 are correct: their subject prints only its own lines, each carrying
exactly one CR. Only a program that RELAYS another process's bytes can produce a doubled
one, and `examples/std` was swept for `capture`/`read_output` — `sysproc_demo` is the only
one. `sysproc_test.jtr` compares in-process and `test_fixture.jtr` captures into a file.

Fixed in the test rather than in `sysproc_demo.jtr`, because that file is in
`CGEN_GOLDEN_ALLOWLIST` and editing it churns the byte-identity golden for the same
outcome.

**RECORDED, NOT FIXED:** on Windows, `capture` + print does not round-trip a child's
bytes. Making stdout binary would change every `\n` the compiler emits on Windows and
re-baseline many goldens, so it is its own increment and its own decision.

---

## §2. `channel_close` — a consumer can be told the producer is done

Every channel consumer in the tree took an `n`. Both halves of `channel.jtr` did, and the
demo's header presented it as a virtue. It was not a choice: `channel_recv` on an empty
channel spins forever with nothing able to wake it, so a count agreed in advance was the
only thing the language allowed. **"Drain until the producer is done" was unwritable.**

The control word grows `[4]i64` → `[5]i64`, `[4]=closed`. Contained in `sync.jtr` because
the `select` lowering (`cgen.rs:8329`) calls `channel_len_i64`/`channel_recv_i64` by their
C names and knows nothing of the layout — checked before touching anything.

* `channel_close` — idempotent, any holder, under the lock so a receiver cannot observe
  "empty" and "still open" from either side of the store.
* `channel_is_closed` — state, and **not** the same question as "anything left to receive".
* `channel_recv_open(T, ch, slot) -> bool` — blocks while open and empty; `false` only when
  closed AND drained.

**Draining after close is the point.** The buffered-items branch is tested BEFORE the
closed flag, so a producer that closes with values in flight does not destroy them.

An out-pointer rather than an error union or a `Recv(T)` struct, and the reason is the
golden rather than taste: a cross-module `catch` and another module's struct **both**
disqualify a corpus file from `CGEN_GOLDEN_ALLOWLIST`, and `channel.jtr` is in it.
`sysproc.read_output` and `alog.next` already report an outcome as an integer for this
reason.

### Two demo parts, because one does not prove it

Part 3 is the headline — a drain loop with no count, reporting a total (15) and how many it
saw (5), the second being a fact it was never given. But part 3 returns 15 whether the
producer closed before or after the consumer started, so **it does not distinguish "drains
after close" from "was drained before close"**. Part 4 removes the scheduler: three values
sent, then closed, then drained.

Verified by breaking it: testing the closed flag ahead of the buffered-items branch turns
part 4's 24 into 0 and part 3's 15,5 into 0,0.

---

## §3. `owns_resource` walks by-value fields — a wrapper owns what it wraps

Both halves of the gate read only the declaration in hand. Two holes, one missing walk:

* **`alog.Cursor`** holds a `file.Reader`, which is `@move`, and carried no attribute — so
  a copy was a second name for one OS descriptor. **`alog.Log` wraps a `file.Writer` and
  DID say `@move`, by hand, in the same module.** The convention failed where it was best
  placed to hold.
* **`transitively_droppable_reuse_is_v1_residue`** pinned a **use-after-drop**: cgen's
  `needs_drop` recurses, so `eat(w)` dropped `w.dev` in the callee and `main` then read
  `w.dev.id`, accepted. The two halves of the compiler disagreed about whether a wrapper
  owns what it wraps, and escape's half was the permissive one. That test's own comment
  said it would flip once the gate learned the walk. It has.

Shaped to match `needs_drop` deliberately — the two answer the same question from opposite
ends, and drifting apart is what produced the residue. Indirection is **not** followed (a
`*mut Handle` field points at a resource without owning one, and stopping there is also
what makes termination free), and `@copy` still wins.

`@copy` over a `@move` field is a contradiction wanting its own diagnostic **on the
declaration**. Pinned by `a_copy_wrapper_around_a_resource_is_pinned_residue` rather than
closed silently from inside the walk. Swept: all 31 `@copy` declarations, none holds any of
the eight `@move` handle types.

### The order mattered more than the code

The whole-corpus diagnostic sweep is **byte-identical before and after** — 274 files, 18
diagnostic lines, zero diff. Every golden passes with the port missing the rule entirely.
That is the exact shape of the rebinding gap that survived two workstreams.

So `jestyr_move_containment_matches_reference` was written FIRST and run against the
unmirrored port, where it returned `[]` against the reference's rejection. Only then was
`type_name_owns` written in `escape.jtr`, and the probe flipped to passing.

The seed drift guard got the same treatment: run before refreshing, confirmed `STALE`, then
`REFRESH_SEED=1`.

---

## §3b. `std/metrics` — observability a service is GIVEN

Counters, gauges and histograms, following `log.jtr`'s design exactly: **no ambient
registry, no process-wide default, no initialization on first use.** A service takes a
`Registry` the way it already takes a `Logger`, a `Clock` and an `Allocator`, which is what
makes it assertable — a test hands over its own and reads it back, with no global to reset
and no order dependence between tests.

Three claims a naive implementation gets wrong, each verified rather than described:

* **The dump is NAME-ordered**, so registration order never reaches the bytes. Two
  registries built in opposite orders render byte-identically. **Verified by breaking it**:
  a registration-order `render` makes the demo print `false`.
* **A counter saturates, never wraps.** A wrapped i64 reports a rate spike of ~1.8e19 —
  an outage that never happened, which is worse than a stuck maximum because one is
  obviously broken and the other is not. `saturated` counts the adds that hit the ceiling,
  so a stuck counter is discoverable.
* **An observation lands in exactly ONE bucket** (the first whose bound it is `<=`, else
  overflow). `le 1000` staying `0` while `le +Inf` is `1` is the single line that
  distinguishes this from a cumulative implementation; every other line matches either way.

Also: **two metrics may not share a name.** That is a real bug when it happens, and it is
also what lets `render` sort by a selection scan over "smallest name greater than the last
emitted" — no scratch array, so `render` allocates nothing and takes the registry by
borrow.

`metrics.jtr` is in `CGEN_GOLDEN_ALLOWLIST` and **byte-identical between both backends**,
measured before adding rather than discovered from a red ladder. `metrics_demo.jtr` is out:
it holds a `metrics.Registry` as a scope-local, and another module's struct degrades to `?`
with imports unresolved. `jc` builds the demo through its own loader (`BUILD_OK
metrics_demo`) — but BUILD_OK is not correctness, so the claims ride on the transcript test.

### Two recorded traps fired while writing it

* **`pub fn find` shadows a compiler intrinsic.** *"An unqualified call emits the
  intrinsic, not this function."* Same class as `pub fn ok` breaking `std/file`. The front
  end warns, which is why it cost seconds. Renamed `by_name`.
* **A range expression may not be a call ARGUMENT.** §5.3 of the Tier 4 note records this
  as a `mut` sub-slice problem; **that is too narrow** — `bounds[0 .. 3]` passed to a
  `read []i64` parameter fails identically. The boundary is argument position, not
  mutability. The idiom is `alloc` + `slice(T, raw, N)` bound to a named local first, which
  `census_cli.jtr` already uses.

And the containment rule from §3 immediately applied to code written after it:
`metrics.Registry` holds eleven `List` fields, `List(T)` has a blanket `Drop` impl, so the
registry is transitively owning and copying one then freeing both is now refused.

---

## §3c. `spawn` may not take ANY `mut` parameter — a race gcc was hiding

Found by writing the obvious first draft of a service worker: it takes
`mut s: service.Service` and completes units through it. **`jestyrc check` reported every
check passing** — resolution, arity, assignability, visibility, trait-bound, exhaustiveness
and escape. What refused it was gcc, and only incidentally: the spawn thunk stores its
arguments by value while the callee expects a `Service* restrict`.

That is the worst form of degrades-to-gcc. It is not that the diagnostic came from the
wrong tool — **fixing the C bug would have removed the refusal and left the race.**

The rule existed, and its name was the bug: `check_spawn_no_shared_mut_slice` tested
`matches!(p.ty, Ty::Slice(_))`, so it caught the aliasing-`ptr` case it was written for and
let every `mut` struct through. It now stops at the first `mut`/`out` parameter of any
kind, with two messages because they are two reasons for one verdict — a slice races
through its aliased `ptr`, a plain binding because every task writes the one place.

A raw `*mut T` parameter is untouched: it carries no `mut` conv, and that is the sanctioned
hatch (`par_binned_sum` gives each task a disjoint region, under `unsafe`, so disjointness
is the caller's stated claim).

**Swept: exactly ONE new diagnostic across all 274 corpus files, and it was my own first
draft.** The rule catches a real race and breaks nothing that ever worked. The demo now
teaches it — workers accumulate into atomic cells and the service's state advances on the
main thread after the join, where there is exactly one writer.

Carried by `jestyr_spawn_mut_param_matches_reference`, **watched failing against the
unwidened port** (`[]` against a rejection) before the mirror was written.

---

## §3d. `std/service` — the lifecycle, and the two health questions

Readiness and liveness are **not the same question**, and conflating them is an outage:

* **Ready** = "send me work". A load balancer removes a not-ready instance from rotation.
* **Live** = "I am not wedged". An orchestrator restarts a not-live instance.

A DRAINING service is **live but not ready**. A service reporting one `healthy` boolean has
to choose: report unhealthy while draining and be killed mid-drain — losing exactly the work
it was trying to finish — or report healthy and keep being sent traffic it has promised to
refuse. Every rolling deploy runs this path on every instance. `accept` enforces it rather
than documenting it, and the refusal is counted.

The exit reason is computed from state: `SVC_CLEAN`, `SVC_ABANDONED` (stopped with work in
flight — the difference between a deploy that finished its queue and one that dropped it,
which nothing else can reconstruct afterwards) or `SVC_FAILED`, which outranks abandonment
because a bug is more actionable than dropped work. All three appear in the transcript,
because a demo that only reaches the happy verdict does not show the verdict is computed.

**A bug of mine worth keeping:** `ran_for` guarded with `if s.started == 0 { return 0 }`,
and `time.manual(0)` — the clock every deterministic test uses — starts at exactly 0, so a
service that really started at t=0 reported a lifetime of 0 forever. **A sentinel a legal
value can equal is not a sentinel**, and here the legal value was the one all the tests use.
Now gated on the phase.

No signals: see §0 item 1, it is a language blocker. Shutdown is initiated by the program;
everything below the trigger is done.

---

## §3f. `std/config` — precedence is a property of the SOURCE

`cli.jtr:38-43` names config merging as deliberately absent and gives the right reason: it
needs a precedence decision one argument parser cannot make alone. This is that decision.

```
default  <  file  <  env  <  cli
```

`apply` compares the incoming origin against the one already recorded and keeps the
stronger, so **the same sources applied in any order produce the same configuration.** The
obvious implementation — last write wins — is simpler, passes any test that applies sources
in the documented order, and is wrong: it makes the answer depend on the order the program
happened to READ its sources in. That is the config bug that reproduces only on the one
machine where an env var happened to be set, and only after someone reorders two lines of
startup code. **Verified by breaking it**: a last-write-wins merge prints `false`.

**Equal origins DO overwrite** — two `--flag` occurrences on one command line means last
wins, which is what every shell user expects, and is not the same question as a file
silently beating an environment variable.

**Redaction is a property of the DECLARATION**, so no code path leaks a secret by
forgetting a flag at a call site. The test asserts the token's TEXT IS ABSENT rather than
that `****` is present — a renderer printing both would satisfy the weaker claim.

Three refusals as distinct codes rather than a bool, because they are different events for
an operator: an unknown key is a typo in their file, a bad value is a typo in the value, a
shadowed one is not a problem. A refused value provably moves nothing. Precedence is
asserted as the full ladder — each key won by a DIFFERENT source — so an implementation
with the order partly wrong still fails.

No file format (a TOML/JSON/INI reader sits above and hands over pairs; building one in
would tie precedence to one syntax), no nesting, no live reload.

---

## §3e. A load-sensitive test, recorded rather than retuned

`jstatus_serves_a_connection_without_starving_its_timers` failed once in six full-ladder
runs and **passes in isolation in 2.8s**. The mechanism is measured, not guessed:
`sysnet_demo.jtr:119` schedules a **1ms** timer and polls with a **500ms** budget — a 500×
margin. For `RT_FIRED` not to come back, the process must have lost the CPU for over half a
second. The ladder runs dozens of gcc invocations in parallel and this arc added two more
`c-oracle` tests that each compile and link, so **this work plausibly raised the flake
probability without being its cause**.

**Deliberately not retuned.** Nothing survives a half-second deschedule by margin alone, and
silently widening a deadline is how a real starvation bug gets hidden later. The honest fix
is that a wall-clock test should not run beside a compile farm — a harness question. If it
recurs, that is the thread to pull, not the timeout.

Note this is the OPPOSITE diagnosis from §1, and the difference is reproduction: the CRLF
failure reproduced every time and load was a wrong guess; this one does not.

---

## §3g. Signals, attest, entropy, and the two channel items

Five increments, in the order they were asked for.

**Signals (`std/sysignal`, `signal_arm`/`signal_caught`/`signal_raise`).** §0 said
`extern`-binds-a-global would close signals *and* `environ` in one increment. **That was
wrong, and the correction is the useful part**: binding reaches a symbol that already
exists, while a handler needs a flag that must be **defined**, and no standard external
global serves as one. Two mechanisms, not one. A handler may touch only a
`volatile sig_atomic_t`, which Jestyr has no spelling for — so the flag is emitted in the
generated C, where it can have exactly the type the standard requires, and the language
keeps its "no module-level mutable state" rule. `signal()` not `sigaction()`: C89, works on
the Windows CRT, so no `@cfg` split and no `struct sigaction` layout to guess. The service
demo now drains **because the OS asked**. `environ` remains open and is now the *only*
thing wanting an extern-bound global.

**attest's false negative.** Traits and impls emitted no records; the comment said their
effect was "captured by the C hash". Measured: two programs differing only by a removed
`pub` trait method gave different hashes and `no API changes`, **exit 0**. The hash is in
the manifest but `diff` makes no `Change` from it, so it never reaches `has_breaking()` — a
hash says *something moved*, true of every rebuild; only a record classifies. The method
set goes **inside the signature** rather than one record per method, because `diff` calls a
new key `added` → Compatible, and adding a trait method breaks every implementor.

**`std/csrand`.** Binds the platform CSPRNG and provides `ct_eq`; invents nothing.
`random_fill` returns a bool with the value through a pointer, because **0 is a legal
draw** and a `-> i64` shape cannot report failure. `fill` is all-or-nothing so a caller
who ignores the bool never gets a half-random buffer. Windows `rand_s` is hand-declared
(the `_CRT_RAND_S` macro must precede `<stdlib.h>`) and needs no extra lib — which matters
because `cc-flags` is locked and recorded in every manifest.

**A refused send hands the value back.** `send` takes by `take`, so at the moment a
refusal is known the callee already owns the value; returning `false` would leak it for any
`T` whose teardown matters. `channel_try_send` writes it to `back` on both refusal paths.
`FULL` and `CLOSED` stay distinct: a producer retries on one and gives up on the other.

**`select` terminates.** One lowering site per side, checked last so closing stays
non-destructive. A `closed { … }` arm would have changed `ExprKind::Select` across 22
reference sites plus the parser and the no-allowlist P2 golden; it stays open as sugar.

---

## §3h. The two parsers allocate expression IDs differently — **CLOSED, and the diagnosis below was WRONG**

> **RESOLVED in §3j.** Keep reading this section for the symptom, but **do not act on its
> diagnosis or its proposed fix** — both were mistaken, and §3j records the measurement that
> overturned them. Short version: the parsers are *supposed* to allocate differently,
> `cgen.jtr`'s `ref_expr_id` is the deliberate shim that reconciles them, and it was simply
> missing a case for `select`. A one-branch port-only fix. No typeck alignment, no AST
> change, no reference change.


**Found by a cgen golden failure that had nothing to do with the feature under test.** A
second `concurrent` block in `select.jtr` produced `jestyr_task_111` on the port and
`jestyr_task_109` on the reference — a spawn site's C symbol embeds its `ExprId`.

Every parser golden passes, and **correctly so**: they compare tree SHAPE, not id VALUES,
and two parsers can build the same tree while numbering its nodes differently. The drift is
only observable when a file has a `concurrent` block positioned after enough code for the
counters to separate, and no corpus file had a second one until now.

Same shape as the rebinding rule that survived two workstreams — a golden that passes is
evidence about the corpus, not about agreement.

### MECHANISM ISOLATED — one node per `select` arm

Measured by node-count delta per construct, with the construct placed before a single
spawn so the first task id *is* the count:

| construct | delta (port − reference) |
|---|---|
| `if` (0, 1, 2, 3 of them) | **0** |
| `for` (0, 1, 2) | **0** |
| `let` with a bare `Name` init | 0 |
| a cast, a call with args | 0 |
| **`select`, 1 arm** | **+1** |
| **`select`, 2 arms** | **+2** |

Exactly the 111-vs-109 gap on a two-arm select. The cause is a one-field asymmetry in the
REFERENCE, not the port: `SelectArm.body` is an **inline `Block`**, while `parse_if`
already wraps its `else` block as an `ExprKind::Block` *expression* — so a select arm is the
one block in the language the reference does not allocate a node for, and the port (which
allocates for every block it parses) ends up one ahead per arm.

Minimal repro, import-free, in `/tmp` during the session: a locally-declared
`Channel(comptime T)` struct, one `concurrent { spawn … }`, a one-arm `select`, a second
`concurrent`. Reference `jestyr_task_26`, port `jestyr_task_27`.

### The fix was ATTEMPTED and REVERTED, and the reason is a second divergence

Changing `SelectArm.body` to an `ExprId` (wrapping the block exactly as `parse_if` wraps
its `else`) **does converge the ids** — verified: all three repro files agreed afterwards,
and the cgen golden then passed on a `select.jtr` carrying two `concurrent` blocks.

It was reverted because it exposed a **deeper disagreement the id drift was masking: the
two typecks do not agree about a select-arm block as a typed expression.** The whole-corpus
typeck dump compares the type of EVERY expression, and the port types its select-arm block
node while the reference does not. Neither `infer` (which records a type) nor `infer_block`
on the extracted block (which leaves it unrecorded) aligns the streams — the first makes the
reference emit one entry too many, the second still misaligns further along, and the port
types a select-arm block where the reference yields `()`.

So the real fix is not one field: it is agreeing what a select arm's body *is* — an
expression with a type, or a block without one — and changing both compilers together.
That is a two-sided typeck alignment increment, not a parser tidy-up, and it does not
belong inside an unrelated feature.

**Nothing that compiles today is wrong because of this.** The emitted symbol is internal
and each compiler is self-consistent. The live constraint is narrow and worth knowing: **a
corpus file with a `select` between two `concurrent` blocks cannot be byte-identity
allowlisted** until this is closed. `select.jtr` avoids it by filling part 2's channels on
the main thread.

---

## §3j. A1 CLOSED — and the recorded diagnosis was wrong in an instructive way

`select.jtr` can now carry a `select` between two `concurrent` blocks. §3h called that
impossible pending "a two-sided typeck alignment increment". It was one missing `else if`
in one Jestyr file.

### What §3h got wrong

§3h says the port "allocates for every block it parses", making a select arm "the one block
in the language the reference does not allocate a node for". **Both halves are false**, and
the table under them (if/for = 0, select = +1) was measured with a contaminated instrument.

Measured directly this time — reading `ast.exprs.len()` on the reference against
`list.len(ExprData, p.ex)` on the port, on generic-free probes:

| probe | ref nodes | port nodes | port excess over base |
|---|---|---|---|
| bare `fn main` | 2 | 3 | — |
| `{ … }` statement block | 6 | 7 | +0 |
| `if c { … }` | 9 | 11 | **+1** |
| `if c { … } else { … }` | 13 | 15 | **+1** |
| `unsafe { … }` | 6 | 8 | **+1** |
| `for c { … }` | 11 | 13 | **+1** |
| `select`, 1 arm | 7 | 9 | **+1** |
| `select`, 2 arms | 11 | 14 | **+2** |

The port allocates one extra node for **every** AST field typed `Block` rather than
`ExprId` — the `fn` body, `if.then`, `unsafe`, `for.body`, `region`, `with alive`, and one
per select arm. `if.els` and a statement-position block agree because both sides already
wrap those as expressions. **`select` is not special; it is one of six.**

§3h's contrary table came from measuring `jestyr_task_<id>` on probes carrying a generic
prelude. That prelude *also* diverges (the port materializes struct-literal paths and
generic ctors as `Name` nodes), so the measurement was a NET of two independent differences
that happened to cancel for `if` and `for`. **A derived instrument that shows zero may be
showing two errors cancelling.**

### The actual mechanism, and why the real fix is small

The divergence is **designed in and already compensated.** `ref_expr_id` in `cgen.jtr`
translates a port `ExprId` into the reference's numbering by subtracting exactly these
block nodes, and it already enumerated `if`, `for` (body *and* else), `unsafe`,
`concurrent`, `region`, `with alive` (body *and* else), struct-lit paths and FieldInit.

It had no case for kind 42. `select` was added to the language and never added to the shim.

```
} else if e.kind == 42 {              // select: EVERY arm body is a Block struct
    var q: i32 = 0                    // in the reference, so this parser's per-arm
    for q < e.y {                     // block node has no counterpart
        let ab: i32 = list.get(i32, p.par, (e.x + q * 4 + 3) as usize)
        if ab >= 0 and ab < eid { off = off + 1 }
        q = q + 1
    }
}
```

**Port-only.** No reference file, no AST, no parser, no typeck. Nothing about what a select
arm *is* had to be decided — which is why §3h's attempt detonated a typeck disagreement: it
changed `SelectArm.body` to an `ExprId` on the **reference** side, introducing a newly-typed
node into the compared stream. It was fixing the side that was already right.

### Why it hid for so long

An `ExprId` reaches the output through exactly one path: a `spawn` trampoline's symbol
(`jestyr_task_<id>`). So the divergence is observable **only** when a mis-translated
construct sits between two spawn sites in one function. No corpus file did that until
`select.jtr` grew a second `concurrent` block. The other five constructs were translated
correctly, so they never produced a symptom even though the underlying arenas diverge for
them too.

`a_select_between_two_concurrent_blocks_agrees_on_both_backends` holds the shape
permanently, and was **watched failing** against the unfixed `cgen.jtr` before being
believed. Verified separately: the full emitted C for that program is byte-identical
between backends once `#line` directives are stripped — the port still emits none, which is
its own recorded gap and is orthogonal.

**Still open, deliberately:** the six `Block`-vs-`ExprId` representation differences remain.
They are correct-by-compensation, not by construction, and the next id-embedding emission
added to the language will need `ref_expr_id` extended again. Making the two ASTs agree
outright (or deriving the symbol from a per-spawn ordinal instead of an `ExprId`) would
remove the class — the second is a small change on each side but churns every
spawn-bearing golden and attest hash.

---

## §3i. The emitted C, judged by the C compiler — A10's warning half

§6A10 recorded that **nothing had ever run `-Wall` over the backend's output**, and
deferred the whole item because this machine has no `libasan`. That bundling was wrong in
a useful way: sanitizers need a runtime library, **warnings need nothing**. The warning
half was fully closeable here, and is now closed. The sanitizer half is untouched and stays
recorded — as an A2-shaped item, a claim nobody in reach can watch run.

`CC_STRICT_WARNINGS` in `src/main.rs` promotes 23 classes to errors; the
`emitted_c_warning_gate` module in `src/proptests.rs` (feature `c-oracle`, so the existing
CI job picks it up with no workflow change) sweeps every lowerable corpus file through
them. **277 emitted programs, all clean**, in ~70s — the gcc calls fan out across threads
because serially it is 3½ minutes.

Two classes are named for the traps they close. `-Werror=return-type` refutes
`docs/jc_build_matrix.txt`'s own warning — *"BUILD_OK is not correctness, since gcc accepts
a non-void function falling off its end"* — so the matrix can no longer bless a program
that returns garbage. `-Werror=implicit-function-declaration` names the `int`-fallback
miscompile `cc_base_flags` records arriving **four** times.

**Kept out of `CC_FLAGS` deliberately.** That const is the reproducible-build provenance
hashed into every attest manifest — `cgen.jtr:15959` writes it as the `cc-flags` line — so
a flag added there re-baselines the corpus, the same cost that defers TLS (§6B4). These
change no emitted byte, no rounding and no linked symbol, so they ride alongside the seam
exactly as `-g` does. No emission moved: **no golden churn, no seed refresh, no port
mirror owed** (the port hardcodes the same four flags and knows nothing of warnings).

### It went green on the first run, which is exactly why it needed teeth

Three tests carry the claim, and they do different jobs:

* `every_gate_flag_is_a_real_compiler_option` — the cheap guard, and it turned out to be
  **total**. gcc *hard-errors* on an unrecognized `-Werror=` name (`no option -Wxyz`)
  before it reads the source, so one compile of a trivial file proves all 23 names are
  real. This is the opposite of the usual assumption — that an unknown warning name is
  ignored the way an unknown `-f` often is — and being wrong about it changed the design:
  typo-detection is free, so the behavioural probes only have to prove the flags catch
  what they claim.
* `the_dangerous_warning_set_has_teeth` — seven deliberate defects, each checked in
  **both** directions: accepted under the ordinary build flags, refused under the strict
  ones. Only the pair proves the refusal came from the gate rather than from the snippet
  being bad C to begin with.
* `strict_warnings_stay_out_of_the_determinism_seam` — pins the attest-provenance
  boundary, and that `-O2` survives (`-Wmaybe-uninitialized` reports nothing without the
  optimizer, so the gate would silently lose a class).

**The teeth test earned itself on its first run.** The obvious `uninitialized` probe —
`int v; if (c) v = 1; return v;` — was **accepted**: gcc 8.3.0 at `-O2` folds it away and
never reports it. It looks correct and proves nothing, which on an already-clean corpus is
invisible. Replaced with the unconditional form, plus a loop for the `maybe-` half.

And the gate was **watched catching a real backend regression**, not just handwritten C: a
prelude function calling an undeclared helper was temporarily added to `cgen.rs`, and
277 of 277 files refused. (That run is also why the failure report truncates at three —
a prelude defect reproduces in every file, so the untruncated output was 277 copies of one
message.)

### The census — what the corpus actually emits, and why most of it is not a defect

The full `-Wall -Wextra` sweep produces **9,835 warnings**. Almost none are backend bugs,
and the classification is the deliverable as much as the gate is:

| class | count | verdict |
|---|---|---|
| `-Wunused-function`/`-parameter`/`-variable`/`-const-variable` | 8,851 | **Noise by construction.** A flattened module closure emits everything reachable; the linker drops the dead. |
| `-Wformat=` + `-Wformat-extra-args` | 1,662 | **Windows false positive, proven.** See below. |
| `-Wsign-compare` | 64 | **A language question, not a backend defect.** |
| `-Wmissing-field-initializers` | 49 | Valid C — a partial initializer zero-fills the rest by standard. |
| `-Wtautological-compare` | 5 | `examples/distinct_ops.jtr` asserts `a == a` **on purpose**. |
| `-Wunused-label` | 3 | The loop lowering emits `__continue` whether or not anything jumps to it. |
| `-Wmissing-braces` | 1 | Valid C. |
| **every dangerous class** | **0** | Which is why the gate goes green — and why it is about the future. |

Two of those deserve to be carried forward.

**The format warnings are an artifact of the measuring machine.** All 1,662 are mingw
checking format strings against the **pre-C99 MSVCRT `printf`**, which has no `ll` length
modifier — so the backend's entirely correct `printf("%lld", (long long)x)` draws *unknown
conversion type character 'l'*. Measured, not assumed: the compiled program prints
`1234567890123` correctly, and `-std=gnu11` does **not** fix it — only
`-D__USE_MINGW_ANSI_STDIO=1` does. **The largest single finding in the sweep was a
property of the toolchain doing the sweep.** The gate recovers the class anyway by
defining that macro, which is safe *only* because it compiles with `-c -o /dev/null`: in a
real build the macro swaps in mingw's own `printf` and changes what gets linked, so it must
never reach `CC_FLAGS`.

### Known coverage limit

The gate walks `cgen::emit` only. `emit_tests`/`emit_tests_filtered` (the `@test`/`@bench`
harness `main`), `emit_show_drops` and `emit_error_traces` are **separate emission paths**,
so a defect confined to one of them is still invisible. Extending to the test harness is
the obvious next step and roughly doubles the runtime; deliberately not taken in the
increment that introduced the gate, and recorded rather than left to be discovered.

**The 64 `-Wsign-compare` are the OPEN int→int conversion decision, surfacing in the C.**
`list.len` really does return `usize` (`std/list.jtr:61`), the loop counters really are
`i32`, and Jestyr's typeck really does accept `i32 < usize`. Every site is in the
self-hosted compiler's own sources (`cgen.jtr`, `typeck.jtr`, `escape.jtr` and their CLIs).
The gate deliberately does **not** include this class: deciding it is a language change,
and a test flag must not pre-empt it. Recorded here because it is the first time that open
decision has been given a concrete, countable cost.

---

## §3k. Intrinsic shadowing is refused, and both grandfathered cases were weaker than recorded

A `pub fn` named for a cgen intrinsic was replaced at every UNQUALIFIED call **with its
arguments discarded**, while a qualified call reached the user's function. One name, two
meanings, chosen by spelling. No wrong type, so C is happy; the only signal is a wrong
answer at runtime. It is now a compile error.

**The recorded plan was the expensive one, and it was aimed slightly wrong.** The note said
the real fix was an emission change — cgen prefers the user's function — costing a port
mirror, a reseed and golden churn. But that only resolves the ambiguity *inside cgen*:
`f(x)` and `mod.f(x)` would still read differently to a human even once they compiled the
same. Refusing the name resolves it everywhere, and cost one rename and one deletion.

**The two grandfathered cases were the argument for staying advisory, and neither held.**

* `set.contains` — "only ever called qualified", which was true. Renamed `set.has`, matching
  `bitset.has`, whose own comment records it dodging this exact collision at authoring time
  *because the warning fired*. The convention already existed; `set` predated it.
* `lexer.str_eq` — recorded as safe because "its semantics happen to match the intrinsic's".
  It was **dead code from the day it was written**: its single call site was unqualified and
  therefore always reached the intrinsic, and nothing ever called it qualified. Deleted, not
  renamed. "It works" was true. "It runs" was never true, and the two were not distinguished.

The base rate had also moved since the warning was chosen: the hazard fired twice more after
that decision (`std/file`'s `ok`, `std/metrics`'s `find`), each caught only because a human
read a warning. A hazard that keeps firing and is caught by attention is not mitigated.

**Watched failing.** The severity assertion was checked by reverting `error` → `warn`: the
test fails with `severity: Warning`. A message-only assertion — which is what the previous
test did — passes in that state, so the whole change would have been unpinned.

**Two exemptions deleted, not narrowed.** `module.rs`'s `pipeline_is_clean` and
`hashmap_compiles_clean` both stripped this warning by message. Both are gone: an exemption
that outlives its subject is how the next diagnostic gets covered silently.

**No reseed.** The rule says refresh the seed on any `examples/std` change; the drift guard
says otherwise here, because neither `lexer.jtr` nor `set.jtr` is in the compiler's closure.
Running the guard rather than refreshing on the heuristic is the recorded trap working in
the direction it is usually quoted against.

---

## §3l. The mirror, and where a two-sided rule is allowed to live

§3k left the rule on one side. That is worse than it sounds: an unmirrored REFUSAL is an
acceptance divergence, and this one miscompiled rather than merely under-reported — `jc`
built `fn arg_count(x: i64)` into `jestyr_rt_arg_count()` with the argument discarded,
defining the user's function and never calling it. Closed.

**The rule had to MOVE, and typeck could not host it.** The obvious mirror is
"add the check to `typeck.jtr`", and it is impossible: that module has no diagnostic
channel at all. It is a pure resolution pass whose only output is the P3 type dump, and the
self-hosted driver detects front-end failures by scanning for recovery artifacts rather
than by reading a diagnostic list. `escape.jtr` has the channel (`dsp` span pairs +
`dmsg`), and the P4 golden compares the two escape passes **by span + message**. So the
reference's check moved `typeck.rs` → `escape.rs`, and the rule is now pinned
differentially rather than existing on one side.

`escape.rs` was already the home for static rules that are not strictly escape analysis —
`check_spawn_no_shared_mut_slice` is a concurrency rule — so this is the established shape,
not a new exception.

**Free functions only.** A method is called `x.contains(..)`, never the unqualified shape,
so it cannot be captured by the intrinsic. The check therefore sits in the `Item::Fn` arm
of `check_item`, which struct- and impl-methods do not pass through.

**The name set is a leaf module.** `examples/std/intrinsics.jtr` imports nothing, because
the pass that owns the dispatch (`cgen.jtr`) is the LAST link in the chain — cgen imports
typeck, which escape imports too — so a pass earlier in the pipeline cannot import the
owner. Hoisting the set to a leaf lets any pass ask without inverting the dependency.

**`is_intrinsic` became data on the reference side too.** It was a `matches!` pattern, which
cannot be iterated, so the port's copy could only ever have been compared by eye. It is now
`const INTRINSIC_NAMES: &[&str]`, and `intrinsic_name_set_matches_the_reference` generates a
declaration for every entry in one program and compares both escape dumps — so a name added
on one side and not the other fails. **Watched failing** by deleting a single name
(`eq_fold`) from the port's list.

**The negative half is the load-bearing one.** Tier-3 reflection (`field_count`,
`field_name`) is dispatched by cgen but deliberately excluded from the shadowing set: it is
only ever called, never referenced as a value, and listing it would refuse any program with
a local of that name — which the self-hosted compiler has several of. A port list built by
grepping cgen's ~40 dispatch sites would have picked reflection up and quietly refused the
compiler's own sources. The control pins the exclusion.

**Two gates needed teaching about the new module**: `SELFHOST_MODULES` (deps-first, so
`intrinsics` sits immediately before `escape`) and the seed, refreshed only after the guard
was watched reporting `STALE`.

---

## §3m. `@copy` containment, and the predicate the recorded scope got wrong

A3 was recorded as "`@copy` over a `@move` field is a contradiction nobody diagnoses".
Both halves of that sentence needed correcting.

**The predicate is `!is_copy`, not `owns_resource`.** The obvious implementation reuses
`owns_resource_at` — it is right there, it is what the residue comment cited, and it makes
the `@move` probe pass. It also silently misses `@copy struct S { s: String }`: `String` has
no `Drop` impl and is not `@move`, so `owns_resource` returns false, while `@copy` happily
duplicates the heap pointer *and* suppresses the teardown. Caught by writing the droppable
probe before believing the `@move` one, and the fix is not a widening — `is_copy` is the
predicate the **enum** form of this exact contradiction has used all along
(`` `@copy` enum … carries a non-Copy payload ``). The struct form was simply missing.

**The contradiction already existed one level up.** `attrs::validate_struct` refuses `@move`
and `@copy` on the *same* declaration. A3 is the transitive case, so this is a rule the
language had already committed to and enforced only at depth 0.

**The walk was left alone deliberately.** `owns_resource_at` still stops at `@copy`, exactly
as `needs_drop` does. Their drifting apart is what produced the use-after-drop that widened
`owns_resource` in the first place, so the containment hole is closed at the DECLARATION
rather than by teaching one of the two to disagree with the other.

**Nested `@copy` is not followed, and that is not a hole**: an inner `@copy` type is checked
by this same rule at its own declaration — if it is legal it owns nothing, and if it is not,
the error is reported there. Termination is free as a side effect.

**The span is the field NAME's, on both sides.** The reference could report at the field
TYPE's span and initially did; the port's `tch` field 3-tuples store only the name span, and
the P4 golden compares spans. Choosing the one both sides can produce was cheaper than
threading a second span through the port's tables, and the message names the type anyway.

**Three helpers became `pub` in `typeck.jtr`** (`td`, `find_type`, `ty_str`) so `escape.jtr`
could ask the questions the reference asks of its type table.

**And the enum half turned out to be an open divergence — see §6A3b.** `jc` builds
`@copy enum Bad { none, own(s: String) }` that `jestyrc` refuses. Left for its own increment
rather than bundled here: it is a MOVE of an existing diagnostic out of `typeck`, and the A5
move surfaced six broken fixtures nobody predicted.

---

## §3n. The enum half, and the comment that named the bug

A3b was found while closing A3, and the port's own source had been describing the defect
for as long as it existed. `typeck.jtr`'s `ty_is_copy`:

```
if kind == 1 { return td(c, d.x, 9) == 1 }   // enum: @copy (validated by the reference)
```

It TRUSTS the flag. That is true when the reference is in the pipeline and false when `jc`
is the only compiler — so `jc` built `@copy enum Bad { none, own(s: String) }`, exit 0 and a
working binary, that `jestyrc` refuses as a double-drop. A second phase-1 comment said the
same thing (*"the reference validates payload copy-ness — its refusal runs first"*). Both
now say `escape` validates it, on both toolchains.

**The move, not the rule, was the work.** `escape.rs` grew an `Item::Enum` arm on the
containment check written for A3; typeck's copy was deleted and its test MOVED rather than
dropped, with an inverted assertion left behind — typeck must NOT raise this now, and a
test saying so is what stops it drifting back to the unmirrorable pass.

**The message and span are carried over verbatim**, so a user sees no change at all: same
sentence, still reported at the payload's TYPE. That mattered for choosing the span — see
below — because churning an existing diagnostic is a cost the move did not need to pay.

### The span choice, corrected from §3m

§3m recorded the struct rule as reporting at the field NAME's span, with the reason that the
port's `tch` stores a field's name span and not its type's. **That reason was incomplete.**
`tch` is typeck's table; the PARSER's arena (`p.ty`) carries `start`/`end` on every type
node, and the port can read copy-ness from typeck while taking the span from the parser —
the two are indexed by the same declaration position, so they zip. Both halves now report at
the field/payload TYPE, which is where the contradiction is and what the enum half already
did. The struct rule was changed to match rather than the enum bent to fit.

### A tables comment that reads the wrong way

The port's header describes an enum row as *"payload TyIds also in `tch`"*, which reads as a
flat run of ids. They are **3-tuples** (field-name span, lowered TyId) — the name is kept so
struct-variant patterns can bind by name. Indexing it as a flat run reads a name span as a
type id, `ty_is_copy` answers about nonsense, and the rule silently never fires. It cost one
debug cycle and was found only by reading the code that WRITES the table rather than the
comment that describes it.

---

## §3o. `-std=c11` hides POSIX, and ten helpers that each rebuilt the cc command

The first Linux run of the §3i warning gate refused **35 of 278** corpus programs, with one
class and nothing else: `-Werror=implicit-function-declaration`, over five functions —
`fileno` (11), `ftruncate` (11), `usleep` (10), `clock_gettime` (9), `kill` (1).

**The includes were never missing.** `#include <unistd.h>` is right there in the emitted C,
and the source declares the header properly (`@cfg(posix) extern "unistd.h" fn ftruncate`).
The cause is `-std=c11`: it defines `__STRICT_ANSI__`, which switches glibc's
`_DEFAULT_SOURCE` OFF, and every one of those five is POSIX rather than ISO. The
declarations were not absent — they were **switched off**, and gcc fell back to implicit
`int`. This is the exact mirror of the mingw `__USE_MINGW_ANSI_STDIO` finding in §3i: one
`-std=c11` strictness consequence per platform, found six weeks apart on opposite OSes.

**`_DEFAULT_SOURCE`, and NOT `_POSIX_C_SOURCE=200809L`.** The POSIX macro is the obvious
choice and is wrong: `usleep` was REMOVED in POSIX.1-2008, so glibc guards it behind
`__USE_MISC`. Defining `_POSIX_C_SOURCE=200809L` fixes four of the five and hides the fifth
more firmly than before. `_DEFAULT_SOURCE` is simply what glibc enables by default and what
`-std=c11` took away.

### The real defect was duplication, and it is the reason this went unseen

`cc_base_flags()` had carried the Windows half of this for a while (`-D_WIN32_WINNT=0x0600`,
same shape, same reason). But **ten helpers in `proptests.rs` assembled their own cc command
from `CC_FLAGS` directly**, and each was expected to re-add the baseline by hand. Six did.
Four did not. Nobody noticed, because the half that went missing was the POSIX one and the
corpus is only ever compiled on Windows locally.

Both platforms' defines now live in one place, `cc_platform_defines()`, and all ten helpers
route through it. `every_cc_invocation_carries_the_platform_defines` is the guard — a
SOURCE-TEXT check, like `extern_signature_agreement`, because what went wrong was
duplication rather than logic. It requires the baseline on the line IMMEDIATELY after
`CC_FLAGS`, since "a few lines later" is where the four that lost it went wrong.

That guard needed its needle SPLICED (`concat!("args(crate::CC_", "FLAGS)")`) so no line of
the file contains it whole — a literal spelling makes the scan match its own source and
report itself. It did that twice before the splice, which at least demonstrated it can fail.

**Kept out of `CC_FLAGS`, for the usual reason.** A `-D` that changes which prototypes a
header exposes moves no emitted byte, so it has no business churning the attest provenance.
It rides alongside, exactly as `-g` and the Windows baseline do.

**What this does NOT explain.** The same CI run failed 13 other tests. Several are plausibly
the same root cause (`time_demo` needs `clock_gettime`; `log_demo` showed the implicit
declaration by name), but `time_demo` reported gcc *failing* rather than warning, and the
runner is gcc **13.3.0**, where an implicit declaration is still a warning — so at least one
other cause is unaccounted for. The previous run's logs are purged, so there is no baseline
to diff against and no evidence those 13 are new. Do not assume this fix closes them.

---

## §3o. The first Linux run, and the flag list assembled in four places

The push was the first time the `-Werror` gate, and four rules landed this arc, ran on a
second compiler. It refused **35 of 278** corpus programs with one class and nothing else:
implicit declarations of `fileno` (11), `ftruncate` (11), `usleep` (10), `clock_gettime` (9)
and `kill` (1). That the other ~20 gate flags stayed silent on a different gcc is its own
result — no version-specific false positives.

**The includes were never missing.** `#include <unistd.h>` is in the emitted C and the
source declares its header properly (`@cfg(posix) extern "unistd.h" fn ftruncate`).
`-std=c11` defines `__STRICT_ANSI__`, which switches glibc's `_DEFAULT_SOURCE` OFF, and all
five are POSIX rather than ISO. The declarations were not absent; they were switched off.
Exact mirror of the mingw `__USE_MINGW_ANSI_STDIO` finding — **one `-std=c11` strictness
consequence per platform, found weeks apart on opposite OSes.**

`_DEFAULT_SOURCE`, deliberately **not** `_POSIX_C_SOURCE=200809L`: `usleep` was REMOVED in
POSIX.1-2008, so glibc guards it behind `__USE_MISC` and the POSIX macro fixes four of the
five while hiding the fifth more firmly.

### The defect was DUPLICATION, and it is why nobody saw it

The flag list is assembled in **four** places, and each was supposed to re-add the platform
baseline by hand:

| site | had Windows half | had POSIX half |
|---|---|---|
| `cc_base_flags()` (`main.rs`) | yes | **no** |
| six helpers in `proptests.rs` | yes | **no** |
| four more helpers in `proptests.rs` | **no** | **no** |
| `cgen.jtr`'s `jc build` driver | yes | **no** |

Invisible from here because the half that went missing was the POSIX one and the corpus is
only ever compiled on Windows locally. All four now route through one definition
(`cc_platform_defines()` / its `cgen.jtr` mirror).

**Two guards, and the second exists because the first could not have caught the fourth
site.** `every_cc_invocation_carries_the_platform_defines` scans `proptests.rs` and demands
the baseline on the line IMMEDIATELY after `CC_FLAGS` — "a few lines later" is where the
four went wrong. It cannot see a command built in Jestyr, so
`the_self_hosted_driver_carries_both_platform_defines` checks `cgen.jtr` separately. A
source-scanning test also finds its OWN needle: the literal is spliced
(`concat!("args(crate::CC_", "FLAGS)")`) after the scan reported itself twice.

### Result, measured: 14 failures → 5

Nine closed, including the gate itself and `time_demo` — which this note had flagged as
*unattributable* because it reported gcc FAILING on a runner where implicit declarations are
warnings. The caution was right to hold and the cause turned out to be the same one; the
honest lesson is that "I cannot attribute this" was the correct state to record, not a
reason to guess either way.

**The five that remain are three distinct causes, all diagnosed, none of them the flags:**

1. **`temp_path` returns nothing on POSIX** (`test_fixture_demo`, and half of
   `capability_suites_pass`). A real portability bug: POSIX says an unset `TMPDIR` means
   `/tmp`, and Ubuntu runners set none of `TMPDIR`/`TEMP`/`TMP`.
2. **`env_test.host_sees_a_temp_dir` asserts a non-guarantee** — it requires one of those
   three variables to be SET. The library is correct; the assertion is wrong. Distinct from
   (1) and needs the opposite fix: relax the test, do not "fix" `env`.
3. **The `str` proptest's line-terminator stripper is Windows-only reasoning.** Its own
   comment documents the failing case and then reasons from text-mode doubling: it pops
   `
`, then one `
`, because "a payload ending in CR arrives as `…
` + `
`" —
   true only on Windows. On POSIX the terminator is a bare `
`, so the `
` it pops is
   PAYLOAD. `~` in the test alphabet decodes to CR, so the generator produces this input by
   design, and the comment claiming the alphabet "guarantees" no stray CR is wrong.

---

## §3p. The A2 claim, and why the ladder could never have found it

`sysproc` has claimed since `bfcf0f6` that a child inherits its parent's whole environment
on both platforms. The register said that was "unverified -- owed to the Linux ladder". The
ladder has since run green over that code many times and verified nothing, because **no
test anywhere asked**. It was never blocked on a machine; it was blocked on a file nobody
had written. Same shape as the recorded `sysnet` trap: a cited test that did not exist.

**The obvious test is the vacuous one.** Before the fix a POSIX child received `PATH` and
nothing else, so "the child sees `PATH`" passes identically against the broken and the
fixed behaviour. Proving the fix needs a variable that is NOT `PATH` -- and `std/env` reads
only, with no setter anywhere in the tree, so it cannot come from the program. It comes
from the program's PARENT: the `c-oracle` test sets `JESTYR_ENVIRON_PROBE`.

**The child is the demo re-invoked.** A shell one-liner would need `echo $VAR` on POSIX and
`echo %VAR%` on Windows -- a `@cfg` split inside the very test whose job is to show the two
platforms now agree. Self-spawning asks it in one spelling on both.

**`parent-has-probe` is printed FIRST, and that line is the test.** If the harness ever
stops setting the variable, the child inherits nothing, "child saw nothing" looks exactly
like a regression, and the transcript would agree with a broken build. Watched failing: the
run without `.env()` reports `parent-has-probe 0`.

### `cmd.exe` strips the quotes off a command line that begins with one

The first draft quoted the program path, which is the spelling that looks obviously correct:
`"<path>" child> "<out>" 2>&1`. `cmd.exe /c` re-reads that as `<path>" child> "<out> 2>&1`
and reports *"The filename, directory name, or volume label syntax is incorrect"*. Unquoted
works. The cost is that a built path containing a space would break it -- acceptable here
because the driver builds into the system temp directory, and the alternative is fighting
cmd's quote-stripping from inside a string `test_fixture.capture` owns.

---

## §4. Comparison suites, rerun at this milestone

Run twice this arc — after §3 and again after §3c, since both changed compiler semantics.
Both green both times.

* `examples/cpp_compare/verify_all.sh` — **15 matched, 0 failed**, `static_rejections`
  still refused with its 3 errors.
* `benchmarks/rust_vs_jestyr/scripts/check_rejections.ps1` — **all 10 probes refused.**
* `run_all.ps1` **not rerun, deliberately.** No benchmark case imports `sync` or `channel`,
  and §3 touched only `escape.rs` (a checker — it produces diagnostics, not code),
  `escape.jtr`, tests and the seed. `cgen.rs` and `typeck.rs` were not touched, so no
  benchmark case's emission can have moved, and the published noise floor (8.4% median,
  25.2% worst between sessions) exceeds anything a timing rerun could resolve.

---

## §5. The inventory — WHERE THE TIER ACTUALLY STANDS

**This table was stale for most of the arc** and is now rewritten against the tree. It used
to read "half the brief already exists"; five areas have moved since, and leaving it as it
was is how a session rebuilds something that already landed — which this repo has done
twice.

| # | area | state |
|---|---|---|
| 1 | service runtime | **DONE.** `runtime.jtr` (loop, timers, cancellation, IO-readiness, exit reasons) + `sysignal.jtr` (signals via intrinsics) + `service.jtr` (lifecycle, readiness-vs-liveness, computed exit reason). Missing: **supervision / restart policy** |
| 2 | observability | **mostly.** `log.jtr` (structured, injected `Clock`+`Writer`, no globals) + `metrics.jtr` (counters/gauges/histograms, name-ordered dump, saturating counters). Missing: **trace spans** — and `Span` is taken three times already, so pick another word first |
| 3 | config | **DONE.** `config.jtr` (precedence is a property of the SOURCE, order-independent) + `ini.jtr` as a file format over it. Missing: live reload, nesting |
| 4 | sandbox | **mostly.** `sysproc` spawn/pipes/`wait_timeout`/kill, and environment inheritance now matches on both platforms **and is finally tested** (§3p). Missing: **cwd, process groups, fs capability projection** |
| 5 | package | **the big one, barely started.** Content-addressing + DAG ordering exist (`module.rs` manifest, `buildgraph.jtr`, `tar.jtr`, `sha256.jtr`). Missing: **semver, resolver, lockfile, cache** |
| 6 | HTTP | **parser only.** `http.jtr` refuses request smuggling. Missing: **routing, middleware, streaming bodies, keep-alive, timeouts, static files, access logs, test client/server** |
| 7 | crypto | **boundary done.** `sha256`, `crc32`, and `csrand.jtr` (platform CSPRNG + constant-time compare). Missing: **HMAC, signing/verification, a hash interface** — bindings, not algorithms to write |
| 8 | TLS | **absent**, and wants a DECISION before effort — see §6B4 |
| 9 | storage | **log only.** `alog.jtr` is CRC'd and crash-recoverable. Missing: **KV, compaction, atomic batches, migrations, backup/export** |
| 10 | compatibility | **DONE.** `src/attest.rs` emits the ABI manifest and gates breaking-vs-compatible in CI, now including trait/impl records. Missing: `@deprecated` reaches nothing (A8) |

**Scoring it honestly: 4 areas done (1, 3, 7-at-the-boundary, 10), 3 mostly (2, 4, and 10's
tail), 3 barely or not at all (5, 6, 9), 1 undecided (8).** The tier's own definition of done
— *"a service can start, report health, run background work, shut down gracefully, and be
tested deterministically"* — **is met**. What is left is breadth, not the headline claim.

Full detail, with `file:line` citations, is in the research note.

---

## §6. EVERYTHING REMAINING — the register

Two lists. **§6A is things that are WRONG** — defects, divergences and unverified claims,
which is what to read before trusting any part of this. **§6B is things that are ABSENT** —
features nobody has built yet. An absent feature is a plan; a defect is a liability, so 6A
comes first even though 6B is where the brief's remaining scope lives.

Nothing in 6A makes a program that compiles today wrong, except where it says so.

---

### §6A. DEFECTS AND OPEN DIVERGENCES

#### A1. ~~The two parsers allocate ExprIds differently~~ — **CLOSED (§3j)**

Fixed by adding the missing `select` case to `ref_expr_id` in `cgen.jtr` — the deliberate
shim that already translated the other six block-node differences. Port-only; no reference,
AST, parser or typeck change. **There was no typeck disagreement to resolve**: §3h's
attempted fix created one by changing `SelectArm.body` on the reference side, which was the
side that was already right.

The live constraint is lifted: a corpus file may now hold a `select` between two
`concurrent` blocks, pinned by
`a_select_between_two_concurrent_blocks_agrees_on_both_backends` (watched failing first).

**What remains, as a latent trap rather than a defect:** the two ASTs still represent six
inline blocks differently (`fn` body, `if.then`, `unsafe`, `for.body`, `region`,
`with alive`, select arms). They agree only because `ref_expr_id` compensates, so **any new
emission that embeds an `ExprId` in output must extend that shim** — and will be silently
wrong until some program puts the construct between two spawn sites. Removing the class
means either making the ASTs agree, or deriving spawn symbols from a per-spawn ordinal
instead of an `ExprId` (small on each side, but churns every spawn-bearing golden and
attest hash).

#### A2. `environ` on POSIX — **CLOSED: the test exists now (§3p)**

`examples/std/env_inherit_demo.jtr` + `a_spawned_child_inherits_the_parents_environment`.
The harness sets `JESTYR_ENVIRON_PROBE`, the demo spawns ITSELF, and the child must print
the value back. Watched failing without the variable. Windows passes; the Linux runner is
answering the question for the first time.

The original entry is kept because the diagnosis is the reusable part.

<details>

#### A2 (historical). `environ` on POSIX is unverified — and NOT for the reason recorded here

`bfcf0f6` makes a POSIX child inherit the parent's full environment. Verified: the emitted C
carries `extern char** environ;`, the Windows half is unchanged and still passes, both
compilers agree byte-for-byte. **Not verified: that a POSIX child actually inherits it.**

**The recorded reason was "owed to the Linux ladder", and that is wrong.** The Linux ladder
has since run green over this code and still verifies nothing, because **no test anywhere
asserts that a child sees a variable its parent set** — swept for it. The claim is not
waiting on a runner; it is waiting on a test nobody has written, which is the same shape as
the recorded trap about `sysnet`'s cited test that did not exist.

That also makes it cheap and LOCAL, not blocked: `sysproc.capture` can run a child that
prints one variable, and the assertion works on either platform. Windows would pass it
today (its half was never in doubt) and Linux would be answering the actual question for
the first time. Whoever picks this up should write the test before touching anything.

</details>

#### A3. `@copy` over a non-Copy field — **CLOSED, both sides (§3m)**

A `@copy` struct holding a non-Copy field is refused at the DECLARATION, on both toolchains,
pinned by `jestyr_copy_containment_matches_reference`. The walk in `owns_resource_at` is
unchanged — keeping it in step with `needs_drop` is what stops the two drifting apart, which
is what produced the original use-after-drop — so the contradiction is caught one level
earlier instead, where it actually lives.

**The recorded scope was too narrow, in the predicate.** The note said "`@copy` over a
`@move` field". The rule uses **`!is_copy`**, not `owns_resource`, because `owns_resource`
catches only `@move` and droppable types and would miss a plain `String` field — heap
storage that `@copy` both duplicates *and* stops dropping. `is_copy` is also the predicate
the ENUM form of this contradiction has always used, so this is the struct form of something
the language already committed to rather than a new rule.

Reported at the field NAME's span on both sides: the port's `tch` field 3-tuples store the
name span and not the type's, and the P4 golden compares spans.

Swept clean before landing: 31 `@copy` declarations across 285 corpus files, plus the `.rs`
test fixtures.

#### A3b. `@copy` ENUM with a non-Copy payload — **CLOSED (§3n)**

Was: the reference refused it and `jc` built it — exit 0, working binary. The rule moved
`typeck.rs` → `escape.rs` and is mirrored in `escape.jtr`, so both toolchains now refuse it
with the same message at the same span. Verified end to end: `jc … build` exits 1 and emits
no binary. Under `jestyr_copy_containment_matches_reference` alongside the struct half.

The original entry is kept below because its DIAGNOSIS is the reusable part.

<details>

#### A3b (historical). `@copy` ENUM with a non-Copy payload — the reference refuses it, `jc` BUILDS IT

**Found while closing A3, pre-existing, and not fixed here.** The enum form of the same
contradiction is checked in `typeck.rs` (`` `@copy` enum … carries a non-Copy payload ``) and
the port has no mirror — so this is the same class as A1 and the A5 port gap. Verified:

```
@copy enum Bad { none, own(s: String) }
fn main() -> i32 { return 0 }
```

`jestyrc` refuses it. `jc … build` **exits 0 and produces a working binary.** The port's own
`ty_is_copy` says why, in a comment: `// enum: @copy (validated by the reference)` — it
TRUSTS the flag, which is correct when the reference is in the pipeline and false when `jc`
is the only compiler.

The fix is the one A5 established: the rule belongs in `escape` on both sides, not in
`typeck` (which has no diagnostic channel and therefore cannot mirror anything).

</details>

#### A4. Windows: `capture` + print does not round-trip a child's bytes

Recorded in §1 and NOT fixed. Text-mode stdout turns a relayed `\n` into `\r\n` while
leaving the child's `\r`, so bytes read from a subprocess and printed come out doubled.
Making stdout binary would change every `\n` the compiler emits on Windows and re-baseline
many goldens — its own increment and its own decision.

#### A5. Intrinsic shadowing — **CLOSED, both sides (§3k, §3l)**

A `pub fn` named for a cgen intrinsic was replaced at every UNQUALIFIED call with arguments
discarded, while a qualified call reached the user's function — so the two spellings
disagreed silently, with no wrong type for C to catch. **Now a compile error.**

**The recorded plan was the expensive one.** It said the real fix was an emission change
(cgen prefers the user's function) costing a port mirror, a reseed and golden churn. But
"the user wins" only resolves the ambiguity *inside cgen*; `f(x)` and `mod.f(x)` still read
differently to a human. Refusing the name resolves it everywhere, and cost one rename plus
one deletion. No emission change, and the seed guard confirmed no reseed was owed.

**Both grandfathered cases were weaker than recorded.** `set.contains` was only ever called
qualified → renamed `set.has`, matching `bitset.has`, which dodged this same collision at
authoring time. `lexer.str_eq` was recorded as safe because "its semantics match the
intrinsic's" — in fact it was **dead code from the day it was written**: its one call site
was unqualified and therefore always reached the intrinsic. It was deleted, not renamed.
"It works" was true; "it runs" was not.

**THE PORT MIRRORS IT — the acceptance divergence is CLOSED (§3l).** Briefly it did not,
and the gap was a real miscompile rather than a missing message: `jc` accepted
`fn arg_count(x: i64)` and emitted `jestyr_rt_arg_count()` **with the argument discarded**,
defining the user's function and never calling it. Both toolchains now refuse it, and
`jc_refuses_a_program_that_shadows_an_intrinsic` asserts the DRIVER's exit code, not just
that a diagnostic exists.

**The rule lives in `escape`, not `typeck`, and that is a port constraint.** `typeck.jtr`
has no diagnostic channel at all — it is a pure resolution pass feeding the P3 type dump —
so a rule there can never be mirrored. `escape.jtr` has one (`dsp` spans + `dmsg`), and the
P4 golden compares the two escape passes by span + message, so the rule is now pinned
differentially instead of living on one side only.

**The name set is a leaf module** (`examples/std/intrinsics.jtr`, no imports) because
`cgen.jtr` owns the dispatch but cannot be imported by a pass earlier in the pipeline —
cgen imports typeck, which escape also imports. On the reference side `cgen::is_intrinsic`
became a `const INTRINSIC_NAMES` slice rather than a `matches!`, purely so the set can be
ENUMERATED: `intrinsic_name_set_matches_the_reference` generates a declaration per name and
compares both dumps, so a name added on one side and not the other fails loudly. Watched
failing by deleting one name from the port's list.

It was not mirrored because the port has **no `is_intrinsic` list to reuse** — cgen.jtr
dispatches intrinsics inline and scattered (`if str_eq(nm, "str_eq")`, ~40 sites), and
typeck.jtr cannot import cgen.jtr (cgen imports typeck). A mirror therefore means authoring
a second copy of a list that already exists in fragments, in a module that cannot see the
original. The honest fix is to hoist the name list into a module both can import — a small
refactor of cgen.jtr, worth doing before anything else needs to ask "is this an intrinsic".

#### A6. `Self` as a type inside a trait impl — **CLOSED** (both sides)

The recorded line — "in a trait parameter; `check` passes, `run` fails" — named one position
and one of the **two** failures. `Self` failed as a parameter, as a return, as a local's type
and nested inside `[]Self`; and only when the IMPL spells it (a trait declaration's `Self` was
always fine, since traits are not emitted, and an impl spelling the concrete type was fine
too — which is why no corpus file ever tripped it).

**Half 1, cgen — `check` passes, `run` fails.** `Self` reached neither of cgen's two type
doors. `c_ty_ast` refused it outright ("cannot lower the external type `Self`", emitted twice
because the impl emitter runs for the prototype and again for the definition); `c_type` was
worse, missing the subst and falling through to **`int`, silently, with no diagnostic at all**.
Both doors already consult `self.subst`, so the fix is a single binding —
`subst.insert("Self", target)` in `emit_impl_method_decl` — rather than two special cases, and
that is also what makes the nested forms work, since the emitters recurse through the map.
`impl_ok_ty` (which resolved `Self` by hand for a fallible return, at top level only) survives
as the vestige of the missing general binding; it stays because its consumer wants a `Ty`.

**Half 2, typeck — `check` FAILS, with a message about escape.** `check_fn` lowered a body's
`Self` to `Opaque("Self")`. The source comment called that a deliberate deferral costing
nothing "because `assignable` is lenient on `Opaque`". That justification expired: the escape
checker's `Unknown`-finalization backstop refuses a *borrow* whose type never resolved, so
`mut o: Self` was rejected. `read` and `take` slipped through only because the backstop is
about borrows. `register_impls` already built the `{Self → target}` map for recorded returns;
`check_fn` now applies it to parameters and the return as well.

Corpus: `examples/trait_self.jtr` (parameter, return, local, `[]Self`, `mut Self`, and a
PRIMITIVE receiver where `Self` is `i32`), allowlisted. Each mirror was watched failing
separately — the `cgen.jtr` half against the cgen golden, the `typeck.jtr` half against the P3
typeck golden.

**A latent port defect closed alongside it:** `jc` had no `Self` refusal of its own, so where
`jestyrc` errored, `jc` alone emitted `int` and `JestyrSlice_Self`. Unreachable only because
the reference refused first — it would have become a live miscompile the moment it stopped.

#### A7. A range sub-view may not be a `mut` argument — **CLOSED** (both sides)

**The two descriptions this entry carried were both wrong**, in opposite directions, and the
correction above was the worse of the two. Kept in full because the failure mode — a recorded
*diagnosis* outliving the *symptom* it explained — is the one this tree keeps paying for.

* The claim that "`bounds[0 .. 3]` into a `read []i64` parameter fails identically" **does not
  reproduce**, and could not have: `examples/slice_range.jtr` had shipped
  `from_utf8(b[0 .. 3])` — a range sub-view in argument position — since the file was written.
* The Tier 4 note's original `mut` diagnosis was right. The boundary is by-address passing.
* The inferred consequence — "parser change → the P2 golden has no allowlist" — was therefore
  also wrong. Nothing in the parser or typeck was involved; `check` passed throughout.

The real defect was one arm of `cgen::emit_place`, which assumed an `Index`'s index is a
scalar element offset. A range index computes a new `{ ptr, len }` view instead, so the arm
emitted the `Range` node as if it were an offset and tripped "the C backend does not support
ranges yet" — a diagnostic pointing at the range, which is precisely what disguised a missing
*place* case as a missing *range* feature for two tiers.

Fixed by parking the sub-view in a compound literal of array type (`(T[1]){ v }` → `T*` →
`(*…)` is a place, block lifetime), the same shape `abi_ref_arg` uses. Corpus:
`examples/slice_range_mut.jtr`, allowlisted in `CGEN_GOLDEN_ALLOWLIST`, so it is covered by
both `jestyr_cgen_matches_reference` (byte identity) and `selfhost_fixpoint_subset` (gcc-built
and RUN on both compilers, stdout compared). The port mirror was watched failing without it.

The residual hole is recorded as **A11** in `jestyr-tier5-next-handoff.md` §1.1b: a `mut`
argument that is a *value* rather than a place (`bump(mk())`) still degrades to a raw gcc
"lvalue required". That one is a semantics decision, not a lowering bug, and is deliberately
not folded in here — the obvious shared fix silently discards a callee's writes through a
checked index.

#### A8. `attest` accepts `@deprecated` and does nothing with it — **CLOSED** (both sides)

The register's description was accurate this time, with one correction worth keeping: it was
**not** true that `@deprecated` "did nothing". It reached cgen and emitted
`__attribute__((deprecated("…")))`, so callers already got a C-level warning. What it did not
reach was the two places that *describe* the API — `doc` and the manifest.

`@deprecated` now gets its **own manifest line**, `  deprecated:` (bare) or
`  deprecated: <msg>`, and its own `> **Deprecated**` blockquote ahead of the prose in `doc`.

**The design question this turned on, and it is the whole entry:** a deprecation is neither a
guarantee nor part of the signature, and putting it in either would have been wrong.

* Not a **guarantee** — that block is titled "checked by the compiler". A deprecation is
  proven nothing; it is a status the author asserts. Folding it in makes "checked" false for
  one entry.
* Not the **signature** — `diff_item` classifies any signature change as `Breaking`. A
  deprecation in `sig:` would therefore report *deprecating an API* as a breaking change,
  which is backwards: every existing call still compiles and still works. A gate that fires
  on the one action an author takes to AVOID breaking people is a gate that gets switched off.

So every deprecation verdict is `Compatible` — added, removed, or message changed — and all
three are still *reported*, because "this is going away" is exactly what a contract diff is
read for. Pinned by `deprecating_an_api_is_reported_and_is_never_breaking` and
`a_deprecation_stays_out_of_the_signature_and_the_guarantees`.

The extractor is shared between `doc` and `attest` on both sides (`doc::fn_deprecated`;
`at_dep_attr`/`at_dep_msg` in the port), for the same reason `at_guarantee_phrases` is: the
documented deprecation and the attested one cannot drift. Non-fn records are always `None` —
`attrs.rs` declares the attribute's targets as `Fn` and `Method`, so that is its declared
surface, not a hole.

`examples/attributes.jtr` already carried a real `@deprecated("use parse_v2")`, so both
goldens covered the change from the first run without a new corpus file.

#### A9. Smaller pre-existing language gaps, each recorded elsewhere

* Multi-bounds `fn f[T: Hash + Eq]` do not parse — a parser change plus a **mandatory** port
  mirror (the P2 golden has no allowlist). Why `hashmap` stores fn-pointer hash/eq.
* Generic aliases refused → no way to newtype a container; why `std/set` is free functions
  over `HashMap(T, bool)`.
* **No uninitialized-memory facility at all**, so containers carry fake defaults
  (`smallvec.jtr:77`). The hard part is the destructor rule for partially initialized
  aggregates.
* A `\u00XX` escape below 0x20 passes through to the emitted C verbatim; C rejects it.
* `alog.Cursor` is move-only by containment now, and `alog.jtr`'s header does not say so.
  A comment, owed on the next `examples/std` change rather than its own ladder.

#### A10. VERIFICATION GAPS — not defects, but the reason a defect could hide

* ~~**Nothing has ever run a sanitizer, or even `-Wall`, over the emitted C.**~~ — the
  **warning half is DONE**, see §3i. The gap was recorded as one item and is really two:
  warnings need no runtime library and were fully closeable here; **sanitizers still are
  not** (mingw gcc 8.3.0 has no `libasan`), and remain open as a CI-only increment — which
  is the A2 shape, a claim nobody in reach can watch. `selfhost_fixpoint_subset` is still
  the right harness for it: it already gcc-builds and RUNS every allowlisted program
  comparing stdout and exit code, so a sanitizer job is that loop plus `-fsanitize`.
* **One C compiler, ever.** `find_c_compiler` is first-match-wins; on `ubuntu-latest` `cc`
  resolves to gcc, so clang is never exercised. **This now costs more than it did**: the
  §3i gate is only as good as the analysis behind it, and clang's `-W` set finds shapes
  gcc's does not.

  **But it is CI-only from here, and that is the A2 shape.** Measured on this machine:
  there is no `clang`, no `clang-cl`, and no `cc` — only `gcc` resolves, so
  `find_c_compiler`'s first-match-wins has never had a choice to make locally. Visual
  Studio 2022 is installed, but `cl.exe` accepts neither `-std=c11` nor `-Werror=`, so it
  is a different job rather than a second entry in the same matrix. Adding a clang leg
  means writing a job nobody in reach can watch run — worth doing, but with the same
  caveat A2 carries, and **not** the cheap local win it looks like from the register.
* `jstatus_serves_a_connection_without_starving_its_timers` is load-sensitive (§3e): a 1ms
  timer with a 500ms budget, so a failure means a half-second deschedule. Failed 1 of ~8
  full-ladder runs. **Do not widen the deadline** — a wall-clock test should not run beside a
  compile farm, which is a harness question.

  **"Passes in isolation" is the WRONG discriminator, and this entry used to give it.** It
  failed twice for me when run alone — both times immediately after a 12-minute ladder, while
  the machine was still draining. Run alone on an otherwise IDLE machine it then passed 8/8.
  So the test that tells you whether a failure is real is *repetition on a quiet box*, not a
  single isolated run; one isolated failure on a busy one proves nothing either way. Anyone
  triaging this by the old wording would have concluded it was a genuine regression, which is
  exactly the wrong call.

---

### §6B. ABSENT — unbuilt work, in leverage order

#### B1. Cheap and adjacent to what just landed

| item | size | note |
|---|---|---|
| ~~`select` `closed { … }` arm~~ | **DONE** | Both sides. `ExprKind::Select` is a struct variant carrying `closed: Option<Block>`; `examples/std/select.jtr` Part 3 uses it and is gated by the cgen golden **and** the build matrix. Three corrections to the estimate: **(1)** `closed` had to be a CONTEXTUAL keyword — `alog.closed()`, `sysnet.closed()`, `syswatch.closed()` plus two `let closed` bindings meant reserving it would break five corpus files, three of them public API; **(2)** the risk was never the parser but the **six cgen walkers** that scan arm bodies (calls, spawns, closures, moves, refs, structs) — each had to learn the closed block, and a miss hides code from the backend rather than rejecting it, plus `ref_expr_id` needed the new block counted, which is the **A1 divergence shape** exactly; **(3)** "the no-allowlist P2 golden" was wrong — the P2 dump goldens are *curated snippet lists*, so the arm had to be added to one by hand or nothing would have compared it. Arm required last (`E0025`; a duplicate is `E0024`) because readiness is tested before the closed condition. |
| Trace spans | small | **`Span` is taken three times** (`http.Header`, `diag`, the `@span` attribute) — pick another word first. `@no_alloc` passes vacuously through a trait method, so use the fn-pointer vtable shape, not a trait. |
| Config: live reload, nesting | small | `std/syswatch` exists; composing them is the caller's today. |
| Service: restart policy, supervision tree | medium | The lifecycle is complete; a supervisor over `std/sysproc` is its own module. |
| Sandbox: cwd, process groups, fs capability projection | medium | `sysproc.jtr:113` names all three. `fs.Fs` gates the parent; nothing projects it onto a child. |
| attest: corpus minimizer, benchmark history | medium | `@bench` emits timings; nothing records them across runs. |

#### B2. `extern` binding a C global — the language feature, SIZED

Still the principled answer for foreign globals, and no longer needed for anything urgent
(`environ` went through an intrinsic). **Measured before deferring**: a new item kind means
252 `Item::` match sites across nine reference files plus 42 in the port, and it must also
reach `attest` (a global is ABI) and `doc`. Larger than the select AST change in A1.

#### B3. The brief's remaining areas — a session each

* **Package substrate** (brief area 5) — semver → resolver → lockfile → content-addressed
  cache. The largest genuinely-absent area and the one the tier's "distribution" theme
  names. Content-hashing, `buildgraph`, `tar` and `sha256` are already underneath it.
* **HTTP V2** (area 6) — routing, middleware, streaming bodies, keep-alive, timeouts,
  static files, access logs, a test client/server. The parser is hardened and refuses
  request smuggling; everything above the message is absent. `sysproc` timeouts and
  `syspoll` readiness are now in place under it.
* **Storage V2** (area 9) — KV, compaction, atomic batches, migrations, backup/export on
  top of `alog`. Note `sysfs` has **no mtime**, deliberately (`struct stat` layout differs
  per platform).
* **Crypto beyond the boundary** (area 7) — HMAC, signing/verification, a hash *interface*.
  `csrand` deliberately invents nothing; these are bindings, not algorithms to write.

#### B4. TLS (area 8) — wants an explicit decision, not just effort

Absent entirely, and **different in kind**: it means binding OpenSSL or schannel, which is
a link-flag change — and `cc-flags` is LOCKED and recorded in every attest manifest, so
adding `-lssl` churns every manifest in the corpus. Worth deciding whether Tier 5 claims
TLS at all or whether it is its own arc.

#### B5. Tier 4 leftovers still open

* **Rewrite `std/plugin` as a server on the pipe transport.** It is one-process-per-call
  only because the transport did not exist; it does now (`start_piped`/`capture`).
* **Adopt the extern alias**: `std/syswatch` still binds `readv(2)` with a one-element
  `iovec` to reach `read(2)`, and there are four separate `close`es across `std/file`,
  `std/sysdir`, `std/sysnet`, `std/sysproc`. One-line changes, deliberately not made here —
  those POSIX branches only run on the Linux runner (same rule as A2).
* **Adopt `@cfg(linux)`/`@cfg(macos)`**: `sysdir`'s `D_NAME_OFFSET` and `syswatch`'s inotify
  branch both still decline, now because a macOS branch nobody can run is an untested claim
  rather than because the language cannot say it.
* Brief §2.4 (runtime ownership) and §2.7 (concurrency with ownership) remain untouched.

---

## §7. TRAPS THIS ARC BOUGHT

**A recorded green baseline may be a different machine's.** 1319/0 was a Linux
measurement; Windows had been red since the pipe half landed. Record the count you
actually observe before believing you caused a later failure.

**Reproduce before diagnosing.** The CRLF failure looked exactly like a timing flake, and
the first ladder really had run alongside two subagents. Loosening the deadline would have
papered over a line-ending bug and destroyed a real assertion.

**A rule that changes nothing owes a probe that it CAN fail — and the probe must be
watched failing.** §3's corpus sweep was byte-identical before and after, so every golden
would have passed with the port unmirrored. Writing the differential probe first, and
seeing `[]` against a rejection, is what turned "a mirror is probably owed" into a fact.

**Run a drift guard before refreshing it.** `bootstrap_seed_is_current` was confirmed to
report `STALE` before `REFRESH_SEED=1`. A refresh that runs unconditionally makes a guard
that has silently stopped working look identical to one that passed.

**Search the whole crate for a cited test, not the file you expect.**
`transitively_droppable_reuse_is_v1_residue` is in `escape.rs`, not `proptests.rs`, and a
too-narrow grep nearly produced a false "the cited test does not exist" claim.

**Two parts, because one does not prove it.** §2's concurrent demo returns the same answer
under either interleaving, so it cannot distinguish the ordering rule it appears to test.
The deterministic part is what carries the claim.

**Fix a normalization at the smallest correct scope.** ~55 sites shared the pattern; only
one had the relay shape. The sweep that established "one" is the evidence; changing all 55
would have been unmeasured churn.

**A recorded gap may be recorded too narrowly.** The Tier 4 note describes the range
limitation as a **`mut`** sub-slice problem. It is not: `bounds[0 .. 3]` passed to a `read
[]i64` parameter fails identically. The boundary is a range expression in ARGUMENT
position. Re-measure the shape of a gap before designing around the recorded version of it.

**Never round-trip ANY source file through PowerShell `Get-Content`/`Set-Content`.** It
reads UTF-8 as ANSI and re-encodes, so every em-dash becomes `â€"` — and it adds a BOM.
Recorded here first for `.jtr` (where it produces `unexpected character` at 1:1) and then
walked into again on three `.rs` files, where it corrupted 1,400 lines and **still
compiled**, so only `git diff` caught it. Use the editor. If it happens, `git checkout` the
files rather than trying to repair the mojibake.

**`out` is a keyword.** `mut out: []u8` does not parse. Fifth on the recorded list with
`read`, `take`, `error`, `spawn` — and reached for anyway while writing a buffer parameter,
which is exactly the name the list predicts you will want.

**Write the NAIVE version first.** §3c's race was found because the obvious first draft of
a worker takes `mut s: Service` — which is what anyone would write, and what the front end
accepted. A library written only in its final careful form never walks into the hole its
users will.

**A sentinel a legal value can equal is not a sentinel.** `ran_for` used `started == 0` to
mean "never started", and `time.manual(0)` — the clock every deterministic test uses —
starts at exactly 0. The failing case was not an edge; it was the default.

**Reproduction is what separates the two timing diagnoses.** §1's CRLF failure reproduced
every time, so "load flakiness" was a wrong guess to discard. §3e's poll failure passes in
isolation and failed once in six runs, so it is one. Same symptom class, opposite verdicts,
and only rerunning tells them apart.

**Do not retune someone else's timing assertion to make your run green.** A 500× margin
that fails is a saturated machine, not a tight deadline, and widening it hides the real
starvation bug that shows up later.

**`jc_build_matrix` failing on a new corpus file is the harness working.** It is
hand-maintained on purpose so someone looks at the diff: the byte-identity goldens run with
imports UNRESOLVED and never exercise the module-resolving `build` path, which is how nine
programs were once byte-identity-verified and unbuildable at the same time. Read the moved
line before regenerating with `JC_BUILD_MATRIX=1` — and remember BUILD_OK is not
correctness, since gcc accepts a non-void function falling off its end. **(That last
clause is now false by construction: §3i's `-Werror=return-type` refuses it. The trap it
described is closed; the reason it existed — BUILD_OK asks a weaker question than
correctness — still stands.)**

**A DERIVED instrument that reads zero may be showing two errors cancelling.** §3h measured
per-construct node divergence via `jestyr_task_<id>` on probes carrying a generic prelude —
but that prelude diverges too, in the opposite direction, and the net was zero for `if` and
`for`. The conclusion drawn ("only select arms diverge") was false, and it made the fix look
like a language-semantics decision. Reading the two arenas' lengths directly took one
temporary print on each side and overturned the whole section. **Prefer the instrument that
measures the quantity itself over the one that measures a consequence of it.**

**A recorded diagnosis is worth less than a recorded symptom, and may be worth less than
nothing.** §3h's symptom (`jestyr_task_111` vs `109`) was real and reproducible. Its
diagnosis sent the next session at a two-sided typeck alignment; the actual fix was one
`else if` in one file. **Re-derive a recorded mechanism before building on it** — and note
this is the third time this tree has recorded that rule (`environ`, the range limitation,
now this).

**Look for an existing compensation layer before concluding two implementations disagree.**
`ref_expr_id` had reconciled six block-node differences for as long as the port has existed.
Nothing in §3h mentions it, so the drift read as a newly-discovered disagreement rather than
a known one with a missing case. **When two implementations agree everywhere except one
construct, suspect an enumeration with a gap, not a semantic split.**

**A recorded gap may be two gaps wearing one sentence.** A10 read *"nothing has ever run a
sanitizer, or even `-Wall`"* and was deferred whole, because the blocker cited — no
`libasan` on this machine — is real. It is real for **sanitizers**. Warnings need no
runtime library at all, and that half was closeable in an afternoon. Before accepting a
recorded blocker, check it applies to every clause it was written across.

**The biggest finding may be a property of the machine doing the measuring.** The
`-Wall -Wextra` sweep's largest non-noise class was 1,662 format-string errors, which look
exactly like a code generator emitting bad `printf` calls. They are mingw checking against
the pre-C99 MSVCRT `printf`; the emitted program prints the right digits. Falsify a
finding against the platform before believing it — and note the first fix guessed
(`-std=gnu11`) did nothing, so *"a plausible fix failed"* is not evidence the diagnosis
was right.

**A probe that looks obviously correct can prove nothing.** `int v; if (c) v = 1; return v;`
is the textbook uninitialized-read, and gcc 8.3.0 at `-O2` folds it away and stays silent.
It was caught only because the teeth test asserts the refusal actually *mentions the flag*
rather than just that a refusal happened. On a corpus that is already clean, a vacuous
probe is indistinguishable from a working gate.

**gcc hard-errors on an unknown `-Werror=` name.** Assumed the opposite (that it would be
ignored, like an unknown `-f` often is) and designed around it; measuring took one command
and made typo-detection free and total. Worth knowing before writing elaborate machinery
to defend against a failure the tool already forbids.

**"It works" and "it runs" are different claims, and only one of them was checked.**
`lexer.str_eq` sat on a grandfathered-exception list for two arcs, justified by "its
semantics match the intrinsic's". Nobody asked whether it *executed*. It never had: its one
call was unqualified, so every call reached the intrinsic and the function was dead the day
it was written. An exception justified by behavioural equivalence should be asked which of
the two implementations is actually running.

**A grandfathered exception is a claim with a timestamp.** "Those two have not been bitten
yet" was true and reasonable when written. It stayed on the page while the hazard fired
twice more. Re-derive the cost of an exception before renewing it — the argument for the
warning was a base-rate argument, and base rates move.

**Assert the SEVERITY, not the message.** The whole of A5 is warning → error. The previous
test matched on message text, which is identical in both states — so it passed against the
unfixed compiler, and would have kept passing if the promotion were reverted. Verified by
reverting: the new test fails with `severity: Warning`, the old one would not have.

**Delete an exemption, do not narrow it.** Two tests stripped this warning by message. Once
its subject was gone, keeping either "for safety" would have left a message filter sitting
in the ladder ready to swallow the next diagnostic that happened to share the phrasing.

**Run the drift guard instead of the heuristic — in BOTH directions.** The standing rule is
"reseed on every `examples/std` change", and the guard said no reseed was owed here because
neither changed file is in the compiler's closure. The recorded trap warns against skipping
the guard before refreshing; it is equally worth running before refreshing *unnecessarily*.

**A recorded "next step" can be wrong about the machine it will run on.** An earlier version
of §0 called a second C compiler the cheapest remaining win in A10. There is no clang on
this machine at all — checking took one command, and it moved the item from "cheap local
win" to "CI-only, inherits the A2 caveat". Verify the tool exists before ranking the work
that needs it.

**An unmirrored REFUSAL is worse than an unmirrored warning.** The recorded rule is that
"diagnostics owe no two-sided tax", and for a warning that is right. The moment a rule
refuses, it stops being a diagnostic and becomes part of the language: one compiler now
rejects what the other compiles, and in this case the accepting one MISCOMPILED. Check
which kind of rule you are adding before invoking the no-tax exemption.

**Ask where the mirror can physically live BEFORE choosing where the rule goes.**
Intrinsic shadowing went into `typeck` because that is where the reference does name
resolution. `typeck.jtr` has no diagnostic channel at all, so that choice made the rule
unmirrorable, and the fix was to move it rather than to build one. For any rule that owes
a port mirror, `escape` is the side with the channel and the differential golden.

**A `matches!` pattern cannot be compared to anything.** The intrinsic set was a
`matches!` arm, so the port's copy could only ever be checked by reading both lists. Making
it a `const` slice cost nothing and turned "two lists that ought to agree" into a test that
generates a probe per name. If two implementations must agree on a SET, store it as data on
at least one side.

**A list scraped from dispatch sites is not the same list.** `cgen.jtr` matches ~40
intrinsic names inline, plus attribute names and the atomics; the shadowing set is 74 and
deliberately EXCLUDES tier-3 reflection, which the compiler's own sources use as local
names. Deriving the port's list by grepping the dispatch would have looked reasonable and
refused the self-hosted compiler. The negative control is what pins that.

**`examples/` is not the whole corpus — inline test fixtures are a second one.** The A5
refusal was swept against all 285 `.jtr` files and came back clean, and the ladder then
failed on THREE escape tests whose fixtures declare `fn ok(..)` inside Rust string
literals. Six such fixtures existed across `escape.rs`, `typeck.rs` and `module.rs`. No
`examples/` sweep can see them, and no golden covers them. When a new rule refuses a NAME,
sweep the `.rs` test sources too, not just the corpus.

**Six independent authors reached for `ok`, and that is the rule's real cost.** `ok` is a
Result constructor intrinsic AND an obvious name for "the function that should be fine" —
which is exactly why `std/file` was bitten by `pub fn ok` in the first place. The refusal is
still correct (all six fixtures were dead-calling nothing, the same "works because nothing
runs it" that `lexer.str_eq` relied on), but the cost is measurable and worth stating rather
than hiding: this rule will make people rename things they did not expect to rename, and the
short intrinsic names -- `ok`, `err`, `arg`, `find`, `trim`, `split`, `bytes`, `contains` --
are where it will happen.

**Reuse the predicate the OTHER half of the rule already uses.** A3's obvious implementation
reuses `owns_resource_at` — it is adjacent, the residue comment cites it, and it makes the
`@move` probe pass. It misses `@copy struct S { s: String }` entirely. The correct predicate
was not something to invent: the ENUM form of the identical contradiction had used
`!is_copy` all along. When a rule has two halves and only one is implemented, read the
implemented half before choosing how to write the other.

**A probe that passes on the case you thought of proves the least.** The `@move` probe went
green immediately and would have shipped a rule with a hole in it. The droppable probe was
written because the residue comment said that half had never been SWEPT — the note flagged
its own gap, and reading that sentence was worth more than the code around it.

**Check whether the rule you are adding already exists at depth 0.** `@move` + `@copy` on one
declaration was already refused in `attrs.rs`. Knowing that reframed A3 from "a new rule" to
"an existing rule that stops at depth 0", which is a much easier thing to justify, scope, and
word the diagnostic for.

**Read the code that WRITES a table, not the comment that describes it.** The port's tables
header calls an enum row's payloads "payload TyIds also in `tch`". They are 3-tuples (name
span + TyId). Indexing them as a flat run reads a name span as a type id, the copy-ness
predicate answers about nonsense, and the rule silently never fires — no error, no
diagnostic, just a mirror that does nothing. The writing code said so plainly.

**A comment that says "validated by the reference" is a divergence with a note attached.**
`ty_is_copy` in the port carried exactly that, and it was accurate: the flag IS validated by
the reference, and by nothing at all when `jc` runs alone. Grep the port for phrases of that
shape — "the reference does X", "X runs first", "checked elsewhere" — because each one is a
place where the self-hosted compiler is trusting a pass it does not have.

**Move a test, do not delete it, and leave an inverted one behind.** The `@copy` enum check
moved from typeck to escape. Its typeck test moved with it, and typeck kept a test asserting
it does NOT raise this any more — which is what stops the rule drifting back into the pass
that cannot mirror it.

**`-std=c11` is STRICT ISO, and each libc hides everything else behind its own macro.** It
cost two separate findings on two platforms: mingw checks `printf` against the pre-C99
MSVCRT unless `__USE_MINGW_ANSI_STDIO` is set, and glibc declares no POSIX function unless
`_DEFAULT_SOURCE` is. Neither is a missing include; both are declarations SWITCHED OFF by
the standard flag. If a call is not ISO C, ask what the platform wants defined before
assuming the header is wrong.

**`_POSIX_C_SOURCE=200809L` is not the safe superset it looks like.** `usleep` was REMOVED
in POSIX.1-2008, so naming that version hides it. `_DEFAULT_SOURCE` is what glibc turns on
when nothing asks otherwise, and is the right way to undo `-std=c11`'s strictness.

**A flag list that is assembled in more than one place will drift, and the drift is
invisible from one platform.** Ten cc invocations each rebuilt the command from `CC_FLAGS`;
six re-added the Windows baseline and four did not, and nobody could see it because the
missing half only mattered on the OS nobody compiles on locally. The fix is one function
and a source-text guard requiring it ADJACENT to `CC_FLAGS`.

**A source-scanning test finds its own needle.** Twice. Splice the literal
(`concat!("args(crate::CC_", "FLAGS)")`) so no line of the file contains it whole.

**Count the places a command is ASSEMBLED before fixing one of them.** The platform baseline
was built in four places — `cc_base_flags`, six proptest helpers, four more proptest helpers,
and `cgen.jtr`'s own `jc build` driver. Fixing the first three still left the Linux runner
reporting `FAIL caps_demo`, because the self-hosted compiler drives gcc itself and no
Rust-side guard can see a command built in Jestyr. `grep` for the flags, not for the helper.

**A guard written in one language cannot cover a duplicate written in another.** This is why
there are two tests and not one widened test. Widening the `proptests.rs` scan would have
looked thorough and still missed `cgen.jtr` entirely.

**A source-scanning test finds its own needle.** The scan reported itself twice — first on a
bare mention of the constant, then on the narrowed `args(...)` form, because both spellings
appear in its own body. Splice the literal (`concat!("args(crate::CC_", "FLAGS)")`) so no
line of the file contains it whole.

**"I cannot attribute this" is a result worth recording, not a gap to fill with a guess.**
`time_demo` was flagged as unexplained because it reported gcc FAILING where implicit
declarations are only warnings. It turned out to have the same cause as the rest — but
recording the uncertainty cost nothing and guessing either way would have been wrong-shaped:
the honest note is what let the next run settle it as evidence rather than as confirmation.

**A comment that says a test alphabet "guarantees" something is a claim to check.** The `str`
proptest's alphabet is documented as guaranteeing no stray CR; `~` decodes to CR by design,
two functions above. The stripper built on that false guarantee is correct on Windows and
eats payload on POSIX.

**"Owed to the Linux ladder" can mean "owed to a test nobody wrote".** A2 sat as blocked-on-
a-machine for an entire arc. The machine had been running the code green the whole time; no
test asked the question. Before recording an item as blocked on infrastructure, check
whether the assertion exists at all — the ladder cannot verify a claim nothing states.

**The obvious test for a portability fix is often the vacuous one.** A POSIX child used to
get `PATH` alone, so "the child sees `PATH`" passes against both the broken and the fixed
build. When a fix WIDENS something, the test must use a value outside the old width, or it
is testing the part that never changed.
