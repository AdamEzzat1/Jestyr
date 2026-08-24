# Std v4 — structured logging, and a module that needed no new platform surface at all

Cold-start note, successor to `jestyr-std-v4-file-watching-handoff.md` (still the record for
file watching and for the half-mirrored-intrinsic finding). **§0 is what to do next.**

```bash
cargo build --release && cargo test --release --features "c-oracle,selfhost-fixpoint"
```

**1273 passed / 0 failed / 3 ignored at the start of this work.** Record your own count
before changing anything; if a later failure appears, assume it is yours.

---

## §0. START HERE

**Structured logging is done**, with a consumer that checks its own claim. `std/log` is a
`Logger` you name and hold; `examples/std/log_demo.jtr` is `jlog`, which logs a job once,
ships it as logfmt AND as JSON, then **parses its own JSON back** and compares a message
containing a quote, an `=` and a newline against what it logged.

**It needed no new platform surface**, which was the reason it was picked: after four
platform-heavy modules (`sysfs`, `sysnet`, `syspoll`, `syswatch`), this one binds nothing,
has no `@cfg`, and reaches the OS only through a `time.Clock` and a `writer.Writer` it is
handed. Every one of its seven tests runs with both replaced, so the whole module is exercised
with no operating system and every record's bytes are exact — timestamps included.

**The port agreed on all three axes first try**, and this time that was checked properly
rather than inferred from a green build: `jestyr_typeck_dump_matches_reference` and
`jestyr_escape_dump_matches_reference` (both whole-corpus, no allowlist) were run BEFORE
concluding anything, which is the correction the previous note's §2.6 asks for. All three
files are also in `CGEN_GOLDEN_ALLOWLIST` and byte-identical (232 files verified).

### What to build next

The Tier 4 table in §3. **§1.10, the append-only log**, is the natural follow-on: `std/log`
now produces the records, `sysfs.rename_replace` is the crash-safe primitive, and `syswatch`
is what lets a reader follow one. Rotation belongs there rather than in `std/log`, and the
module header says so.

**§1.11, the plugin process protocol** is the other unblocked one (`std/process` +
`std/sysnet` both exist). **§1.7, HTTP/1.1** is the large one and nothing blocks it.

### What NOT to do without reading §2 first

* **Do not add per-level openers (`log.info(lg, m)`).** `error` is a keyword, so the set
  cannot be spelled and every partial version is worse than none. See §2.1.
* **Do not make `std/log` rotate, sample or rate-limit.** Each is a policy with no
  defensible default, and rotation belongs to the append-only log. See §1.1.

---

## §1. WHAT WAS BUILT

### §1.1 — `std/log`

`examples/std/log.jtr` + `log_test.jtr` (7 cases) + `log_demo.jtr`.

A `Logger` holding a `time.Clock`, a `writer.Writer`, a minimum level, a format, and its own
record buffer. `begin` / `kv_str` / `kv_i64` / `kv_bool` / `end`.

**No hidden logger**, the same rule `std/runtime` obeys. There is no `log.info(…)` free
function and no default initialized on first use, because a global logger is a hidden global
with an open handle and a clock attached.

**A record is always exactly one line**, and that is the property everything else serves. A
message carrying a quote, a newline or an `=` cannot end the record early, split it in two or
invent a field. That is the difference between a log a program can read and one it can nearly
read, and it is what hand-rolled loggers break first.

**The escaping is `std/json`'s, in BOTH formats.** A quoted logfmt value *is* a JSON string,
produced by the same `json.put_str` the JSON renderer uses and that `json_test` already
covers. Two escape tables would be two places to be wrong about one question, and the second
would be the untested one. The only new logic is `needs_quoting`, which is `pub` and pure so
the rule that makes the format parseable is tested directly.

**Two formats, one record-building API.** `LOG_LOGFMT` and `LOG_JSON`; the format is a
property of the `Logger`, chosen at `make`, and the calls that build a record are identical
either way. `the_same_record_renders_in_both_formats` builds one record twice and asserts
both — if those two blocks ever have to differ, the separation has failed.

**Filtering happens before formatting, and forgetting the guard is SAFE.** `begin` returns
false below the threshold so `if log.begin(…) { … }` costs one integer comparison for a
filtered record — but a caller who omits the `if` is *correct*, not lucky: the record is
marked closed, every `kv_*` on a closed record is a no-op, and `end` reports false.
`omitting_the_filter_guard_cannot_corrupt_the_next_record` pins it, because a logger where
forgetting the guard corrupts the *next* record is a logger that will corrupt the next record.

**Three counters, because there are three fixes.**

| counter | cause | what the operator does |
|---|---|---|
| `dropped` | below the minimum level | lower the level |
| `truncated` | too big for the record buffer | make the buffer bigger |
| `abandoned` | `begin` with no matching `end` | fix the missing `end` |

A single "lost" number would tell a caller nothing about which to apply. `abandoned` exists
because the realistic bug is an early `return` between `begin` and `end`, and an uncounted
loss is indistinguishable from a record that was never written.

**A truncated record is NOT emitted**, and a valid one announcing the loss takes its place. A
half-written JSON object is unparseable and a half-written logfmt line silently loses its tail,
so shipping one puts corruption into the stream that a reader cannot detect. Announcing the
gap in band is the same choice `std/syswatch` makes with `WATCH_OVERFLOW`: **loss a consumer
can see beats loss it cannot.** `a_truncated_record_is_withheld_and_announced` gives the
logger a 64-byte record buffer, then asserts the notice is present, is parseable JSON, and
that the oversized record's contents never reached the stream.

**Deliberately absent**: rotation, sampling, rate limiting, async shipping, and context
inheritance ("a child logger with these fields pre-set"). Each is a policy wanting a decision
this module cannot make for every caller; rotation in particular belongs with the append-only
log.

### §1.2 — The consumer: `jlog`, and the claim checked in band

`run_job` is written once and shipped twice. It knows no format, no destination, no
timestamps and no thresholds — all four belong to whoever built the logger, which is exactly
what lets the same function serve a test, a terminal and a log pipeline.

Then the demo **parses its own JSON output back** with `std/json` and compares. One record
carries `peer said "no" and quit\nreason=timeout` — a quote, an `=` and a newline, every
piece of punctuation both formats use. A logger that formats by concatenation produces an
unparseable line here, or two lines, or silently loses the tail; all three surface as `false`
rather than as a plausible-looking log.

`jlog_ships_one_routine_two_ways_and_reads_itself_back` asserts the exact transcript (possible
because `time.manual()` makes the timestamps the clock's readings rather than the wall
clock's), that the newline appears only in escaped form, and that the hostile message occurs
exactly twice — once per format, never as a line of its own.

---

## §2. WHAT THE BUILD TURNED UP

### §2.1 — `error` is a keyword, so the conventional logging API cannot be spelled

`pub fn error(…)` does not parse. `trace`, `debug`, `info` and `warn` do.

**This is the second instance this arc of "the obvious API name is already the grammar's"** —
`std/syswatch` hit it with `read`, where `extern "unistd.h" fn read` is unspellable and
`readv` was the way through. There the workaround was a better binding; here there is no
workaround, only a choice between bad options:

* **four openers plus `log_error`** puts the odd spelling on the level a reader most needs to
  get right;
* **a uniformly prefixed `begin_info`/`begin_error` set** is `begin(lg, LOG_INFO, …)` with
  five more names and no more capability;
* **`std/diag`'s dodge** (`sev_error`, `new_error`) works for an enum variant precisely
  because nobody types it forty times a file.

**So `begin` is the only opener**, and it came out better than the convenience set would have:
the level constant sits at the call site where it greps, and it is the *same* vocabulary as
`enabled(lg, LOG_DEBUG)` and `set_min_level(lg, LOG_WARN)` rather than a second one that has
to be kept in step.

Worth noting for whoever takes the keyword/extern increment recorded in the previous note's
§2.1: **it has two consumers now**, and they want different things. `syswatch` wants a C
symbol spelled with a keyword (an alias form, `fn sys_read = "read"(…)`, would do it); `log`
wants a *Jestyr* function named with one, which an extern alias does not help with at all. A
general "a keyword may name a function where no ambiguity arises" rule would serve both, and
is the larger of the two designs.

### §2.1a — "A dead handle must survive being USED", for the second session running

The previous note's §2.4a recorded it from `syswatch.poller(closed())`, which dereferenced
null inside the event loop. `std/log` produced the same shape from the other direction:
**`begin` after `log.free` wrote through a null record buffer.**

The two are worth reading together because the *dead* states are different and the lesson is
the same. `syswatch` had a constructed-but-invalid value (`closed()`); `log` has a
valid-then-freed one. Both are reachable without doing anything exotic — a `catch closed()`
fallback in one case, ordinary shutdown ordering in the other — and in both the crash lands
far from the cause.

Guarded in `begin` only, because that is the one entry point `free` does not already close
off: it clears `open`, and a closed record already makes `kv_*` and `end` no-ops. Pinned by
mutation — removing `if lg.raw == null { return false }` crashes
`a_record_that_is_never_ended_is_counted` at exactly that assertion.

**The generalizable version, now with two instances**: a handle's dead value is always tested
for what it REFUSES, and that is not the same as testing it for what it can be PASSED TO. Ask
what the handle hands out — a poller, a buffer, a callback context — and use that from the
dead value in a test.

### §2.2 — The port agreed, and this time that was CHECKED

The previous note's §2.6 recorded a half-mirrored intrinsic that every byte-comparing gate
missed, and named the correction: *building* against the port is not the same as *checking*
against it. That correction was applied here before any claim was made —
`jestyr_typeck_dump_matches_reference` and `jestyr_escape_dump_matches_reference`, both
whole-corpus with no allowlist, were run first. Both green.

`std/log` was also a good test of the rule for a cheap reason: it uses no intrinsic the corpus
had not already exercised, which is precisely the case where a half-mirror would hide. The
three files are additionally in `CGEN_GOLDEN_ALLOWLIST` and byte-identical (232 files
verified), and `jc_build_matrix` moved to **53 of 57 BUILD_OK** — one line added, the same
four isolated failures (`combinators`, `mutex`, `slice_algos`, `try_read`), none moved.

**A note on what did NOT need excluding**: `syswatch_test.jtr` and `syswatch_demo.jtr` are out
of the allowlist because they build a `[]T` whose element is an unresolved import. `std/log`'s
files store cross-module STRUCTS as fields (`time.Clock`, `writer.Writer`, `json.Writer`,
`Sink` inside `Logger`) and that shape degrades identically on both sides — so the exclusion
is specific to slice ELEMENTS, not to imported types generally. Worth knowing before assuming
the next module owes an exclusion.

---

## §3. OPEN

### Tier 4 remaining

| § | item | state |
|---|---|---|
| 1.1 | `sys/fs` | ✅ |
| 1.2 | deterministic `std/walk` | ✅ Tier 3 |
| 1.3 | event loop V1 | ✅ |
| 1.4 | TCP sockets | ✅ |
| 1.5 | file watching | ✅ |
| 1.6 | structured logging | ✅ **this session** — `std/log`, logfmt + JSON |
| 1.7 | HTTP/1.1 | ⬜ — the large one, nothing blocks it |
| 1.8 | tar / reproducible archive | ⬜ |
| 1.9 | Unicode display width | ⬜ — `std/diag`'s `caret_alignment_is_byte_based_and_tabs_expand` is the pin to invert |
| 1.10 | append-only log | ⬜ — **the natural follow-on**; `std/log` makes the records, `rename_replace` is the crash-safe primitive, `syswatch` follows one |
| 1.11 | plugin process protocol | ⬜ — unblocked |

### What `std/log` does not do, and what it would cost

* **Rotation** — belongs to §1.10, which owns the file's lifecycle. A logger that rotated
  would be deciding when to rename a file it does not own.
* **Context inheritance** (`child(lg, "req_id", id)`) — wants either a heap-owned field list
  per logger or a parent pointer, and both make a `Logger` a resource with a lifetime rather
  than a handle. Worth doing when something has more than one long-lived scope to tag.
* **Async / buffered shipping** — the destination is `writer.Writer`, which writes
  synchronously. Batching belongs behind that interface, not in front of it.
* **A `failed()` on the writer** — `std/writer`'s header explains why it does not exist yet
  (nothing can latch: `print_str` returns nothing). When a fallible write intrinsic lands,
  `end` is where the error set belongs.

### Language obligations

Unchanged by this session. `§2.1` move-only resources is still owed by `Socket`, the file
handles and `Watcher`; `Logger` does NOT join them — it owns a heap buffer freed by
`log.free`, so a copied `Logger` double-frees exactly as any other owning struct would, which
is the general case rather than a handle-specific one.

---

## §4. TRAPS

**`error` is a keyword**, alongside `read` and `out`. Check before naming a function after a
severity, a stream operation, or a parameter direction.

**`contains` / `trim` / `find` / `str_eq` are INTRINSICS, not `std/str` members.**
`str.contains(…)` does not resolve; the bare call does. `std/str` has `eq`, which is not the
same function as the intrinsic `str_eq`.

**`sink.new()` takes no buffer** — the buffer travels to every operation. A `Sink` stored in a
struct is fine (two `usize`); a `[]u8` is not.

**A handle that owns its clock needs its own `advance`.** `runtime.advance` exists for this
and `log.advance` now does too: a test advancing its own `Clock` copy advances one nothing
reads, and every timestamp comes out identical.

**Check the port PER PHASE, not by building.** The previous note's §2.6 is the instance;
`jestyr_typeck_dump_matches_reference` is the gate, and it has no allowlist so a new file
joins it automatically.

---

## §5. Suggested order

1. **The append-only log** (§1.10) — the natural consumer of what just landed.
2. **`@must_use` enforcement** — still cheap, still a degrades-to-gcc row.
3. **The four `jc_build_matrix` failures** — one def-emission gap, one inference gap.
4. **A keyword may name a function** (§2.1) — now with two consumers wanting different halves.
5. **The `@cfg` specificity rule** — two recorded callers waiting.
6. HTTP, tar, Unicode width, plugins.
