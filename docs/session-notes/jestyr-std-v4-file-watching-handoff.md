# Std v4 — file watching, and a half-mirrored intrinsic that every byte-comparing gate missed

Cold-start note, and the successor to `jestyr-std-v4-runtime-platform-handoff.md` (which is
still the authoritative record for §1.1–§1.3 and the nine compiler bugs they found). **§0 is
what to do next — read it first.** Then: what was built (§1), what it turned up (§2), what is
open (§3), traps (§4), order (§5).

```bash
cargo build --release && cargo test --release --features "c-oracle,selfhost-fixpoint"
```

**1272 passed / 0 failed / 3 ignored at the start of this work**, matching the previous
note's recorded count exactly; **1273 / 0 / 3 at the end** (+1: the `jwatch` transcript test;
the suite itself is one case of `io_suites_pass`). The 3 ignored are the deliberate slow
numeric sweeps. Record your own count before changing anything; if a later failure appears,
assume it is yours.

---

## §0. START HERE

> **SUPERSEDED for "what to do next" by
> `docs/session-notes/jestyr-std-v4-logging-handoff.md`.** Structured logging, named below as
> the next thing to build, **is done** — `std/log` plus `jlog`. Read the successor's §0 first;
> this note remains the record for file watching, and **§2.6 (a half-mirrored intrinsic no
> byte-comparing gate could see) is the one every future session should read.**

**File watching is done**, with a real consumer. `std/syswatch` is a
directory watcher that registers in the event loop as a `Pollable`, and
`examples/std/syswatch_demo.jtr` is `jwatch` — a debounced rebuild trigger whose transcript
is **identical on Linux and Windows**, which is a design consequence rather than a
coincidence (see §1.3).

**It found one port divergence, and the way it was found is the most useful thing in this
note.** `std/syswatch` derives `struct iovec`'s layout from `@size_of` — the first corpus use
of a layout intrinsic ever — and the port **typed it `?` while its cgen lowered it
correctly**. So the emitted C was right, `jc build` worked, the demo ran, and every
byte-comparing gate agreed. Only `jestyr_typeck_dump_matches_reference`, which runs the whole
corpus with NO allowlist, could see it. Mirrored in `examples/std/typeck.jtr`; seed refreshed
(41,886 → 42,017 lines). See §2.6 — **an intermediate draft of this note claimed "no compiler
change was needed", and the full ladder falsified it.**

### A numbering caveat, so the next reader is not misled

**The previous note's prose and its own table disagree about the brief's section numbers**
— its §0 calls file watching "§1.4" while its table lists §1.4 as TCP sockets and §1.5 as
file watching. This note uses the TABLE's numbering throughout (file watching = §1.5,
structured logging = §1.6), and §3's table is the one to trust. Nothing depends on the
numbers except finding the right row.

### What to build next

**Structured logging (§1.6 in the table below).** It adds no new platform surface at all,
which after four platform-heavy modules is the cheap one; and it has an obvious consumer in
`jwatch` and `jstatus`, both of which currently `print_str`.

Two other candidates, both larger and both now unblocked:

* **§1.10, the append-only log.** `sysfs.rename_replace` is the crash-safe primitive it
  needs and it exists; `syswatch` is what would let a reader follow one.
* **§1.11, the plugin process protocol** — `std/process` plus `std/sysnet` are both there.

### What NOT to do without reading §2 first

* **Do not "just add `linux` to `CFG_WORDS`."** The trigger the vocabulary was left closed
  for has now fired twice, and the increment is still not a word in a list. See §2.2.
* **Do not try to make a Windows `Runtime` serve a socket and a watch together** by wrapping
  pollers. The fix is in `std/sysnet`, not in a composite. See §2.3.
* **Do not add `syswatch_test.jtr` or `syswatch_demo.jtr` to `CGEN_GOLDEN_ALLOWLIST`**
  without redoing the measurement in §2.5. They are excluded for a reason that is written
  down.
* **Do not conclude "the port agrees" from a green `jc build`.** It is evidence about
  emission only. See §2.6, which is this session's own mistake written up.

---

## §1. WHAT WAS BUILT

### §1.1 — `std/syswatch`: a directory watcher (brief §1.5, see the caveat above)

`examples/std/syswatch.jtr` + `syswatch_test.jtr` (7 cases) + `syswatch_demo.jtr`.

One directory per `Watcher`, non-recursively. A handle for `runtime.watch`, the `Poller` that
can wait on it, and a `drain` that reports what changed. Linux `inotify`, Windows
`FindFirstChangeNotification`.

**A change notification is a HINT, not a log, and the API is shaped so a caller cannot forget
that.** This is the one design decision everything else follows from, and it is true on both
platforms for two different reasons:

* Windows reports THAT something under the directory changed and never what — no name, no
  kind.
* Linux names the file and the kind, until its per-watch queue overflows, at which point the
  kernel drops events and sets `IN_Q_OVERFLOW`.

Both arrive as `WATCH_RESCAN` / `WATCH_OVERFLOW`, so a caller writes **one** recovery path
and it is exercised on Linux too rather than being Windows-only code nobody runs.
`names_changes()` says which world you are in, so a caller can skip work it knows is
impossible instead of discovering it at runtime. The alternative — an API that reports names
and lets the Windows branch report none — makes the Linux-shaped program the natural one to
write and the Windows behaviour a surprise found in production.

**`IN_MODIFY` is deliberately absent and `IN_CLOSE_WRITE` is present**, which is the single
most common bug in a first file watcher: `IN_MODIFY` fires on every `write(2)`, so a build
triggered by it runs against a file the compiler is still half way through writing.
`IN_CLOSE_WRITE` fires once, when a writer closes. It is not a guarantee that nobody else
still holds the file open — that is in the header's "what this does not prove" — but it is
the difference between usually right and reliably wrong.

**Rename cookies are deliberately NOT exposed.** `IN_MOVED_FROM`/`IN_MOVED_TO` carry a cookie
correlating the two halves of a rename, and Windows cannot answer the question at all. An API
usable on one platform would be a worse lie than not having it.

**The inotify PARSER is `pub` and NOT `@cfg`-guarded, and that is the most important decision
in the module.** It is where a file watcher is actually wrong — a transposed mask bit or an
off-by-one in the name padding yields plausible events forever — and guarding it would mean
the parser only ever runs on the platform whose CI job is the one nobody is watching. It
needs no operating system: `parse_inotify(buf, avail, evs, names)` is a pure function of a
byte count and a buffer, exactly as `syserr.category` is a pure function of a code and a
numbering system, and for the same payoff. **Two of the seven test cases feed it hand-built
event streams on Windows**, covering mask decoding, the NUL padding, the queue-overflow
marker and the buffer-full marker — on the host that will never compile that branch into a
real program.

That is not a small share of the module: of the code that can be wrong in a way a compiler
will not catch, the parser is most of it, and it is now the part under test on every host.

**A full caller buffer becomes a `WATCH_OVERFLOW`, not a silent truncation**, and it is the
SAME marker the kernel's own queue overflow produces. Deliberately: the data has already left
the kernel and cannot be put back, so a short buffer loses changes exactly as an overflowing
queue does. One recovery path, and it is reachable in a test by passing a small buffer
instead of by putting a kernel under load.

**`Change` is `@copy` and owns no storage.** Names are packed into a caller `[]u8` and a
`Change` carries `(kind, name_off, name_len)`, so a batch of changes costs one allocation the
caller already made and none here. `change_name(c, names)` is a free function rather than a
method because the two live apart on purpose.

**`close_watch`, not `close`.** This module binds libc's `close` as an extern, and an
extern's name is a C symbol reserved *within its own module* — the one arrangement neither
the canonicaliser nor the port's loader can fix. (`std/sysnet` reached the same answer from
the same direction and called its operation `close_socket`.)

### §1.2 — `struct iovec` is DERIVED, not assumed

Binding `readv` costs one foreign structure, and it is the cleanest one in the `sys` tier:

```c
struct iovec { void *iov_base; size_t iov_len; };
```

It is built as `alloc(usize, 2)` — two pointer-sized words in a specified order with no room
for padding — so **there is no offset to get wrong and no endianness to assume.** Compare
`std/sysnet`, which reads `sockaddr_in` at hard-coded byte offsets (defensible, but a
constant that could be wrong), and `std/sysfs`, which refuses `struct stat` outright because
its layout varies by architecture.

Reaching for `@size_of` is also what exposed §2.6: this is its first use anywhere in the
corpus, and the port had never typed it.

The single assumption left is `sizeof(size_t) == sizeof(void *)`, and it is not a comment:
`a_pointer_and_a_usize_are_the_same_width` asserts it with `@size_of` on **every** host,
Windows included. The platform that never compiles the branch is still able to falsify its
premise. That is a strictly better story than an offset table, and it is available because
`@size_of` is a comptime value — worth reaching for the next time a foreign struct comes up.

### §1.3 — The consumer: `jwatch`, a debounced rebuild trigger

`examples/std/syswatch_demo.jtr`. The smallest program that needs the whole tier: a watcher
registered as a pollable, a **debounce** timer sharing that loop and that `CancelToken`, and
a **rescan** that decides what actually changed.

Three things it demonstrates that are easy to get wrong:

* **The callback MARKS and the loop ACTS**, the same discipline `jstatus` follows for a
  socket. A callback runs inside the fire pass, so draining or rescanning there would stall
  every other timer — and would put operations that can fail somewhere with nowhere to
  report a failure.
* **The debounce re-arms rather than throttling.** Every further notification cancels the
  pending timer and schedules a new one, so a burst is over when notifications STOP, not when
  a fixed window since the first one elapses. That is exactly what a hand-rolled
  `while (true) { check(); sleep(50); }` cannot do, and it is why the debounce lives in the
  same `Runtime` as the watch: one loop waits on the directory and the clock at once, and one
  token stops both.
* **It never prints an event's name, though on Linux it could.** It prints a fresh directory
  listing instead. That is the demonstration, not a simplification — it is the only version
  correct on both platforms, and the only version that survives an overflow.

**Its transcript is byte-identical on Linux and Windows, and that is the assertion.**
`jwatch_coalesces_a_burst_and_reports_by_rescanning` compares an exact transcript, the way
`jstatus` does. If that test ever needs a `cfg!(windows)` branch, the design above has been
abandoned — the note is in the test.

The demo prints no notification COUNT, and that is not squeamishness: one `fs.put` is
`IN_CREATE` + `IN_CLOSE_WRITE` on Linux and a differently-coalesced pair on Windows, so a
count is a number the program cannot promise. What it can promise is the debounce's actual
contract — **three edit rounds produce exactly three rescans** — and that is what is asserted.

---

## §2. WHAT THE BUILD TURNED UP

### §2.1 — `read` is a Jestyr keyword, so `read(2)` is unbindable. NOT fixed

Draining an inotify fd needs `read(2)`. **`read` is a Jestyr keyword** (`TokenKind::Read`,
the `read x: T` parameter mode), so `extern "unistd.h" fn read` does not parse.

That is §2.1b's rule arriving from the other side. The previous session established that an
extern's name belongs to the LINKER and must not be renamed; this is the same boundary
saying that the C symbol namespace contains words Jestyr has spent on its own grammar, and
the language currently has no way to spell them.

**Worked around with `readv`**, which is the same call with a vector and is not a keyword —
and which turned out better than a workaround (§1.2). So this is noisy-but-safe rather than
blocking, and it is left open deliberately.

**The increment, and why it is bigger than it looks.** The DECLARATION half is one line:
`parse_extern`'s `self.eat_ident("function name")` (`src/parser.rs:646`) would accept a
keyword token and take its text. The CALL half is the hard part — `read(fd, …)` in expression
position hits the primary-expression parser, which sees `TokenKind::Read` — and "treat a
keyword followed by `(` as a name" is a lexical hack with real ambiguity (`if (x)` becomes a
call). A scoped rule ("only keywords that cannot start an expression") is defensible but
needs the list justified, and the port's parser owes the mirror. The alternative design is a
declared alias — `extern "unistd.h" fn sys_read = "read"(…)` — which is more surface but no
ambiguity, and which would also give `std/file`, `std/sysdir` and `std/sysnet` a way out of
their three separate `close`es. **Either is a real increment; neither is a one-liner.**

### §2.2 — The `@cfg` vocabulary trigger has fired TWICE, and it is still not a word in a list

`src/attrs.rs`'s `CFG_WORDS` says `linux`/`macos` "are deliberately absent until something
needs to tell them apart". `std/sysdir`'s `D_NAME_OFFSET` comment records the first thing
that did. **This module is the second, and a sharper case**: `sysdir` wanted to tell them
apart to pick a NUMBER, this one needs to pick an entirely different API. `inotify` is a
Linux interface; POSIX never standardised file watching; macOS has `kqueue`/`FSEvents`, which
is a different model and not a different name.

**The increment is still not two entries in a table, and the reason is now specific.**
`posix` is a SUPERSET of `linux` and `macos`, and the vocabulary is a closed list of guards
that are **disjoint by construction** — which is what makes "two items may share a name when
their platforms are disjoint" sound. Add two nested names and `@cfg(posix) fn f` and
`@cfg(linux) fn f` both emit on Linux: a duplicate definition. Redefining `posix` as "not
Windows and not one of the finer names" would break every existing `@cfg(posix)` in the tree.

So the real increment is a **specificity rule** — when two items share a name and their
guards nest, the more specific one wins on its platform, which is `#if defined(__linux__) …
#elif !defined(_WIN32) …` — and it interacts with §2.1e (typeck keys its function table on
the bare name, so the second declaration already wins for BOTH branches). That is a typeck
change and an ordered-emission change with a port mirror, not a table edit.

**What this module does instead**: the POSIX branch binds inotify under
`extern "sys/inotify.h"`, so macOS fails with a MISSING HEADER at the line that names it —
the loudest and earliest failure available, rather than a link error or a branch that quietly
does nothing. Stated in the module header.

### §2.3 — A Windows `Runtime` can serve sockets or watches, not both

On Linux a socket and an inotify fd are both file descriptors and one `poll(2)` waits on
both, so **`syspoll.host()` services a watcher with no new code at all** — `syswatch.poller()`
on POSIX simply hands it back. That is the receipt for `runtime.Poller` being a `ctx` +
fn-pointer pair: the integration cost of file watching on Linux is one line.

On Windows they are not the same kind of object. A change-notification HANDLE is waited on
with `WaitForSingleObject`; `WSAPoll` takes sockets and nothing else. A `Runtime` holds
exactly ONE `Poller`, so which of the two it can serve is decided by which poller was
installed.

**The fix is not a composite poller.** Partitioning handles by owner and waiting on each
group with a sliced timeout is a busy loop wearing a design. The Win32 answer is
`WSAEventSelect`, which brings a socket into the object-wait world — at the cost of making it
non-blocking, which is a change to `std/sysnet`'s semantics rather than an addition to
`std/syswatch`. Recorded rather than half-done.

What the Windows poller does keep, and what a test pins: it waits on its own handle for the
whole timeout even when that handle is not registered, so a runtime holding a foreign handle
**idles correctly and merely never fires**, rather than spinning. Never falsely ready, never
spinning — the two properties a poller that cannot see everything can still keep.

### §2.4 — A watch is drained UNTIL EMPTY, and the first test asserted otherwise

The suite's first draft asserted "a second drain with nothing new answers 0". It failed, and
the failure was right.

One logical file operation is **more than one notification on both platforms, and they
coalesce differently**: Linux batches `IN_CREATE` and `IN_CLOSE_WRITE` into a single read, and
Windows re-arms `FindNextChangeNotification` straight back into a signaled state because the
directory genuinely did change twice. So "the second drain answers none" is one platform's
coalescing mistaken for a contract, and a caller who believes it has written code that works
on exactly one of them.

What both platforms owe is **convergence**: with nothing new happening, draining reaches 0 in
a bounded number of calls, and only then does the watch stop waking the loop. The test now
asserts that pair, and asserts them **separately**, so a `drain` that answered 0 without
consuming anything would still be caught. It is written into `drain`'s header, because it is
the one thing a caller must know and the one thing the obvious code gets wrong.

### §2.4a — A capability's FALLBACK value has to survive being used, not just being held

`open_dir(...) catch closed()` is the documented fallback and every other `sys` module has
the same shape. What is new here is that a `Watcher` hands out a **`Poller` whose context is
its own heap cell** — and a failed open never allocated one. A caller who installs the poller
before checking `is_open` (an ordinary thing to do, and the demo is the only reason it did
not happen in this session) dereferenced null **inside the event loop**, which is as far from
the failed open as a bug can get.

Guarded, and the guard is pinned rather than assumed: removing `if ctx == null { return 0 }`
makes `a_denied_capability_refuses_before_touching_the_platform` crash at exactly that
assertion. Nothing-ready is also the honest answer, not just the safe one — a watcher that
was never opened will never become readable.

**The general shape, worth carrying**: `closed()`/`denied()`/`missing()` values are tested
for what they REFUSE, and that is not the same as testing them for what they can be passed
to. The moment a handle exports something derived from its interior — a poller, an allocator,
a callback context — its dead value needs a test that USES the derived thing.

### §2.5 — A new port-divergence category: a slice whose ELEMENT is an unresolved import

`syswatch_test.jtr` and `syswatch_demo.jtr` are the first corpus files to build a `[]T` whose
element is a struct imported from another module (`slice(syswatch.Change, …)`).
`jestyr_cgen_matches_reference` feeds the raw file to both backends with imports UNRESOLVED,
so `syswatch.Change` cannot resolve, and the two sides degrade the unknown element
differently — one typedef:

```
reference:  typedef struct { int* ptr; size_t len; } JestyrSlice_?;
port:       (nothing)
```

**Note what the reference emits: a typedef whose name is not a valid C identifier.** It could
never compile, and that is the clue this is degradation shape rather than a real mangle gap —
§2.1d's `JestyrResult_?` was a missing `Unit` arm reachable from a VALID program, and telling
the two apart is exactly why the previous note insists on measuring.

**Measured**: rebuilt self-contained with the struct declared LOCALLY so the element resolves,
both backends emit `JestyrSlice_Change` and agree byte for byte over the whole file — the
`#line` directives aside, which are the port's separately-recorded gap. So it cannot affect a
program that compiles. Both files are therefore excluded from `CGEN_GOLDEN_ALLOWLIST`, with
the measurement written beside the list, the same as `sysfs_test.jtr`.

The demo is not left unchecked by that exclusion: `jc_build_matrix_matches_expectations`
builds it through the port's own module loader, where the import DOES resolve, and it was
run by hand and printed the same transcript the reference toolchain's build prints. The
matrix is now **52 of 56 BUILD_OK** (was 51 of 55) with the same four isolated failures —
`combinators`, `mutex`, `slice_algos`, `try_read` — so this session added a line and moved
none.

**A gap this leaves, stated rather than discovered later**: `syswatch_test.jtr` is outside
both cgen goldens, so a divergence inside one of its `@test` bodies is invisible to
`jestyr_cgen_test_mode_matches_reference` as well. Same position `sysfs_test.jtr` is in.

### §2.6 — A HALF-MIRRORED INTRINSIC, invisible to every gate that compares bytes

**The one port divergence, and it is worth more than a clean run would have been.**

`@size_of` / `@align_of` / `@offset_of` are layout intrinsics. The reference types them in
`src/typeck.rs` (the `comptime::is_layout_intrinsic` arm → `Ty::Prim("i64")`), sitting
directly below `reflect_intrinsic_ret`. The port had faithfully mirrored the NEIGHBOUR —
`typeck.jtr`'s `reflect_ret` is line-for-line correct — and had no arm for the three below it.

Meanwhile **`cgen.jtr` already lowered `size_of` correctly**: it is in the port's intrinsic
name list and emits `sizeof(<cty>)`. So the port typed the call `?` and still produced the
right C.

Trace what that means for the gates:

| gate | verdict | why |
|---|---|---|
| `jc <file> build`, then run the binary | **passed** | emission was correct |
| `jestyr_cgen_matches_reference` | **passed** | compares emitted BYTES |
| `jestyr_cgen_test_mode_matches_reference` | **passed** | same |
| `jc_build_matrix_matches_expectations` | **passed** | builds; does not compare types |
| `selfhost_fixpoint_*`, self-build | **passed** | the compiler uses no layout intrinsic on itself |
| `jestyr_typeck_dump_matches_reference` | **FAILED** | the only one comparing typed expressions |

**An intermediate draft of this note said "the first `sys` module in this arc that needed no
compiler change at all", citing `jc build` working first try as the evidence. That inference
is wrong and is worth naming: a working `jc build` is evidence about EMISSION, not about
agreement.** A half-mirrored feature — known to one phase and not the other — produces
correct output and a divergent internal state, and only a gate comparing the internal state
can see it. The previous note's advice ("write new modules against the PORT as well as the
reference") is right; this is the refinement: **building against the port is not the same as
checking against it, and the P3 typeck golden is the gate that knows the difference** — it is
the third time it has been the one to notice.

Why it survived until now: **no corpus file had ever used a layout intrinsic.** `@size_of`
has existed since workstream L; `std/syswatch` is its first consumer, because deriving
`struct iovec`'s layout instead of hard-coding offsets is what made it worth reaching for.
The gap was as old as the feature.

Fixed by `layout_ret` in `examples/std/typeck.jtr`, beside `reflect_ret`. `typeck` is in the
fourteen-module closure, so a **seed refresh was owed and paid** (41,886 → 42,017 lines,
purely additive). Emission is unchanged, so there was no golden churn and the build matrix did
not move.

**The generalizable rule**: when a feature threads through several compiler phases, the port
can mirror a subset and stay byte-identical forever. Grep the port for the feature's name in
EVERY phase rather than concluding from a green build —
`grep -n "size_of" examples/std/cgen.jtr` had four hits and
`grep -n "size_of" examples/std/typeck.jtr` had none, which is the whole diagnosis in two
commands.

---

## §3. OPEN

### Tier 4 remaining, in the brief's order

| § | item | state |
|---|---|---|
| 1.1 | `sys/fs` | ✅ |
| 1.2 | deterministic `std/walk` | ✅ Tier 3 |
| 1.3 | event loop V1 | ✅ — cancellation, `poll_for`, `Poller` + `watch` |
| 1.4 | TCP sockets | ✅ |
| 1.5 | file watching | ✅ **this session** — `std/syswatch`, inotify / change notifications |
| 1.6 | structured logging | ✅ — `std/log`, logfmt + JSON. See the successor note |
| 1.7 | HTTP/1.1 | ⬜ |
| 1.8 | tar / reproducible archive | ⬜ |
| 1.9 | Unicode display width | ⬜ — `std/diag`'s `caret_alignment_is_byte_based_and_tabs_expand` is the pin to invert |
| 1.10 | append-only log | ⬜ — `rename_replace` exists; `syswatch` is what follows one |
| 1.11 | plugin process protocol | ⬜ |

### What `std/syswatch` does not do, and what it would cost

* **Recursive watching.** inotify does not do it — you walk the tree and add a watch per
  directory, and handle the race where a directory appears between the walk and the add.
  Windows `FindFirstChangeNotification` takes a `bWatchSubtree` flag and does. The two are
  different enough that a portable `recursive: bool` would mean different things, which is
  why the parameter is not there.
* **Names on Windows.** `ReadDirectoryChangesW` supplies them, at the cost of an `OVERLAPPED`
  structure whose layout differs between x86 and x64 — the `struct stat` hazard `std/sysfs`
  refuses. It would also not change `jwatch`, which rescans on purpose.
* **Write readiness**, inherited from `std/syspoll`, which answers read readiness only.

### Language obligations, unchanged by this session

`§2.1` move-only resources is now owed by a third handle type: a `Watcher` is a struct with
no `Drop` impl, so it is freely copyable and `close_watch` through a copy leaves the other
naming a handle the platform may have reissued. Stated in the module the same way
`std/sysnet` states it for `Socket`. `§2.2` (`@must_use` on a non-union return), `§2.4`
(runtime ownership), `§2.6` (FFI contracts) and `§2.7` (concurrency) are untouched.

---

## §4. TRAPS

**`read` is a keyword and so is `out`.** Neither can name a function, a parameter or a
variable. `read` bites hardest at an `extern`, where the name is a C symbol you do not get to
choose — see §2.1.

**A new `@cfg`-bearing file MUST go in `CGEN_GOLDEN_ALLOWLIST`**, or
`every_cfg_bearing_corpus_file_is_byte_identity_verified` fails on purpose. A file that
merely IMPORTS one does not have to, and should only be added if it is actually
byte-identical — see §2.5 for the shape that is not.

**A module-qualified struct LITERAL does not parse**, still. Any `pub struct` a caller builds
needs an exported constructor: `syswatch.closed()` exists for exactly this, beside
`sysnet.closed()` and `sysfs.missing()`.

**A `match` ARM BODY must be an expression**, still — `stamp` in `syswatch_test.jtr` is the
shape that works, and an empty `catch { }` is not one, which is why `nothing()` has a name.

**`sink.new()`, not `sink.to_slice(buf)`.** A `Sink` carries the write position; the buffer
is passed to every operation. Guessing the other way costs a compile.

**`jc build` writes `<stem>.c` and `<stem>.exe` beside the source.** Remove those two BY
NAME — `git checkout examples/std/` would take your `cgen.jtr` edit with them.

**A feature can be mirrored in ONE phase of the port and stay byte-identical forever.** Before
claiming the port agrees, grep it per phase — `grep -n "<feature>" examples/std/typeck.jtr`
AND `examples/std/cgen.jtr` — rather than trusting a green `jc build`. §2.6 is the instance;
the cost of missing it was one full ladder.

**Reading a foreign struct: know which of three cases you are in.** Derivable from
`@size_of` (`iovec` — best, and check it in a test that runs everywhere); wire-adjacent and
architecture-invariant (`sockaddr_in`, `struct inotify_event` — acceptable, and assert it
end to end); architecture-dependent (`struct stat`, `OVERLAPPED` — refuse).

---

## §5. Suggested order

1. **Structured logging** (the brief's §1.6) — no platform surface, and `jwatch` and
   `jstatus` are both waiting for it.
2. **`@must_use` enforcement** — still cheap, still a degrades-to-gcc row. `FnSig` has no
   attribute field; add `must_use: bool` (two construction sites, `src/typeck.rs` ~595 and
   ~624) and check it at the discarded-statement seam (~2880).
3. **The four `jc_build_matrix` failures** — one def-emission gap plus one inference gap,
   both isolated in the previous note's §2.3.
4. **An extern's name vs. the keyword table** (§2.1) — pick the alias design or the scoped
   keyword rule, and note it also unwinds the three `close`es.
5. **The `@cfg` specificity rule** (§2.2) — the largest of these, and the one with two
   recorded callers waiting.
6. HTTP, tar, Unicode width, append-log, plugins.
