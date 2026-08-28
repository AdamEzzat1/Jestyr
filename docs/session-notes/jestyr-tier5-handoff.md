# Tier 5 — reliability, distribution, production operations

Cold-start note. **§0 is what to do next.** Then the baseline story (§1), the three
increments (§2–§4), what the research turned up (§5), what is left (§6), traps (§7).

```bash
cargo build --release && cargo test --release --features "c-oracle,selfhost-fixpoint"
```

**1324 passed / 0 failed / 3 ignored.** It was **1319** at the start of this arc — but see
§1, because on Windows the recorded baseline was never actually green.

| commit | what |
|---|---|
| `8fcdd1f` | the bounded runner's transcript survives a child that speaks CRLF |
| `a883bd7` | a consumer can be told the producer is done, instead of told a count |
| `39c2583` | a wrapper owns what it wraps |

The predecessor note is `docs/session-notes/jestyr-tier4-language-work-handoff.md`.
The research note this arc produced is
`docs/session-notes/jestyr-tier5-systems-language-research.md` — read its §0 shortlist
before picking anything.

---

## §0. START HERE

Tier 5's definition of done opens with *"a service can start, report health, run
background work, **shut down gracefully**, and be tested deterministically"*. §2 removed
the reason the last clause was impossible. The next items, in leverage order:

1. **Signals — and they are BLOCKED on a language increment, which is the finding.** A
   POSIX handler must be async-signal-safe, which in practice means it may only write a
   `volatile sig_atomic_t` at file scope. **Jestyr has nowhere to put one**: `extern` binds
   functions, not globals, and there is no mutable module-level state — swept, and all 274
   corpus files have const-only module scope. So "graceful shutdown on SIGTERM" cannot be
   written today.

   The fix is one of two increments, and **one of them closes two recorded blockers at
   once**: letting `extern` bind a C GLOBAL gives both the signal flag and `environ`, the
   latter being why a POSIX child inherits only `PATH` while a Windows child inherits the
   parent's whole environment (`sysproc.jtr:120`). Do that one.
2. **`select` needs a default arm.** §2 gave receivers an EOF but did not fix `select`,
   which polls `channel_len_i64(ch) > 0` and therefore spins forever over closed, drained
   channels. This is the other half of a terminating worker loop.
3. **Refusing a send on a closed channel** — deferred deliberately in §2, and the reason
   is an ownership question, not effort. See §6.1.
4. ~~**Observability metrics**~~ — **DONE**, see §3b. What is left on top of it is **trace
   spans**, and note the name is taken three times over (`http.Header` spans, `diag` source
   spans, and the `@span` work-span attribute), so pick another word before writing a line.
5. **The config merge layer.** `cli.jtr:38-43` refuses it explicitly, pending a precedence
   decision that is the actual work. `diag.jtr` already supplies source-spanned rendering.
6. **A `Metrics` consumer inside a real service.** `metrics` has a demo, not yet a
   service that reports its own health through one. That is the increment which turns
   §3b from a library into Tier 5's "observability can be injected and asserted".

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

## §4. Comparison suites, rerun at this milestone

* `examples/cpp_compare/verify_all.sh` — **15 matched, 0 failed**, `static_rejections`
  still refused with its 3 errors.
* `benchmarks/rust_vs_jestyr/scripts/check_rejections.ps1` — **all 10 probes refused.**
* `run_all.ps1` **not rerun, deliberately.** No benchmark case imports `sync` or `channel`,
  and §3 touched only `escape.rs` (a checker — it produces diagnostics, not code),
  `escape.jtr`, tests and the seed. `cgen.rs` and `typeck.rs` were not touched, so no
  benchmark case's emission can have moved, and the published noise floor (8.4% median,
  25.2% worst between sessions) exceeds anything a timing rerun could resolve.

---

## §5. The inventory — half the brief already exists

| area | state |
|---|---|
| 1 service runtime | **core exists.** `runtime.jtr` has the loop, timers, cancellation tokens, IO-readiness poller, and structured exit reasons (`RT_FIRED/IDLE/TIMEOUT/READY`). Missing: signals, graceful shutdown, readiness/liveness, supervision |
| 2 observability | **half.** `log.jtr` is structured, leveled, logfmt+JSON, injected `Clock`+`Writer`, no globals by design. `time.manual()` is the deterministic clock. Missing: metrics, spans |
| 3 config | sources exist (`cli`, `env`, `json`, `diag`); the merge does not, and `cli.jtr:38` refuses it pending a precedence decision |
| 4 sandbox | **mostly.** `sysproc` has real spawn, pipes, `wait_timeout`, kill. Missing env control needs a **language** change (`extern` cannot bind a global; `environ`) |
| 5 package | content-addressing + DAG ordering exist (`module.rs` manifest, `buildgraph.jtr`, `tar.jtr`, `sha256.jtr`). Missing: semver, resolver, lockfile, cache |
| 6 HTTP | a hardened `core` parser that refuses request smuggling. Everything above the message is absent |
| 7 crypto | `sha256` + `crc32` only. No secure random, HMAC, constant-time compare |
| 8 TLS | absent entirely |
| 9 storage | `alog.jtr` is a CRC'd crash-recoverable append log. Missing: KV, compaction, batches, migrations |
| 10 compatibility | **largely built.** `src/attest.rs` emits an ABI manifest and does breaking-vs-compatible diffing as a CI gate |

Full detail, with `file:line` citations, is in the research note.

---

## §6. WHAT IS LEFT

### §6.1 — Refusing a send on a closed channel. An ownership question, not effort

`channel_send` takes by `take`, so by the time a refusal is known the callee already owns
the value, and returning `false` leaks it for any `T` whose teardown matters. A correct
refusal hands the value back. Deferred deliberately; the header says so rather than
implying coverage.

### §6.2 — `select` termination

It polls `channel_len_i64(ch) > 0`, false forever once a channel is closed and drained.
Needs a default arm.

### §6.3 — `@copy` over a `@move` field

A contradiction that should be refused at the DECLARATION. New diagnostic, so a port
mirror. Pinned as residue today.

### §6.4 — Sanitizers. NOT shipped, and the reason is the machine

Nothing has ever run ASan/UBSan/TSan — or even `-Wall` — over the emitted C. Verified: no
`fsanitize`, `-Wall` or `-Wextra` anywhere in `.github/workflows` or `src`. The harness is
already there (`selfhost_fixpoint_subset` gcc-builds and RUNS every allowlisted program
comparing stdout and exit code), so a sanitizer leg is a flag list and a second job.

**It was not added, because this machine cannot run it**: mingw gcc 8.3.0 (Strawberry Perl)
has no `libasan`/`libubsan` — `ld: cannot find -lasan`. Shipping a CI-only gate nobody here
can observe is the same untested claim `sysdir.jtr`'s header refuses to make about macOS.
Do it when you can watch the Linux ladder.

### §6.5 — `alog.Cursor` is now move-only, and its header does not say so

§3 makes it move-only by containment rather than by attribute, so a reader of `alog.jtr`
gets the rejection with no explanation in the module. A header note is owed. Batch it with
the next `examples/std` change rather than spending a ladder on a comment.

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

**Never round-trip a `.jtr` file through PowerShell `Get-Content`/`Set-Content`.** It adds
a BOM and mangles every non-ASCII character in the header comments, and the compiler then
reports `unexpected character` at 1:1. Use the editor.

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
correctness, since gcc accepts a non-void function falling off its end.
