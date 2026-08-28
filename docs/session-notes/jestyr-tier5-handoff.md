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

1. **Signals** — nothing in the tree installs a handler. `sigaction`/`signal(2)` are bound
   nowhere; `SIGTERM` appears only as prose in `sysproc.jtr:1098`. Without it, "graceful
   shutdown" is programmatic only, which is not what a deployment means by it.
2. **`select` needs a default arm.** §2 gave receivers an EOF but did not fix `select`,
   which polls `channel_len_i64(ch) > 0` and therefore spins forever over closed, drained
   channels. This is the other half of a terminating worker loop.
3. **Refusing a send on a closed channel** — deferred deliberately in §2, and the reason
   is an ownership question, not effort. See §6.1.
4. **Observability metrics** (`Counter`/`Gauge`/`Histogram`) — pure library work; the
   `ctx + fn-pointer` vtable shape is already in `mem.Allocator`, `runtime.Poller`,
   `walk.Visitor`. **Do not put it behind a trait**: `@no_alloc` passes vacuously through
   a trait method, so a `core`-tier metrics interface behind one carries a marker that
   proves nothing.
5. **The config merge layer.** `cli.jtr:38-43` refuses it explicitly, pending a precedence
   decision that is the actual work. `diag.jtr` already supplies source-spanned rendering.

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
