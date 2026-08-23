# Std v4 — the `sys` tier is real, and the self-hosted compiler can build it

Cold-start note. **§0 is what to do next — read it first.** Then: what was built (§1), what
it turned up in the compiler (§2), what is open (§3), traps (§4), order (§5).

```bash
cargo build --release && cargo test --release --features "c-oracle,selfhost-fixpoint"
```

**1265 passed / 0 failed / 3 ignored at the start of this work.** The 3 ignored are the
deliberate slow numeric sweeps, not breakage. Record your own count before changing
anything; if a later failure appears, assume it is yours.

---

## §0. START HERE

> **SUPERSEDED for "what to do next" by
> `docs/session-notes/jestyr-std-v4-file-watching-handoff.md`.** The brief's §1.4 (file
> watching) named below as the next thing to build **is done** — `std/syswatch` plus
> `jwatch`, its debounced-rebuild-trigger consumer. Read the successor's §0 first; this note
> remains the authoritative record for §1.1–§1.3 and for the nine compiler bugs they found,
> and everything in §2 and §4 below is still true.

The brief's **§1.1 (`sys/fs`) and §2.3 (platform error shape) are done**, with a real
consumer. **`JestyrResult_unit` lowering is done too** — a fallible function may now have no
return type at all, which is the natural signature for most of what a `sys` layer does.
**Nine compiler and toolchain bugs** were found and closed on the way, two of them silent
miscompiles, and the gate the 2026-08-20 census audit said it owed now exists — **51 of 55
multi-module programs build under `jc`**, up from 43 of 52.

(Section numbers below are this note's own; the brief's are named as "the brief's §x".)

**The brief's §1.2 (event loop V1) is done too** — cancellation tokens, a waiting `poll`,
and waiting that goes through the CLOCK so the same loop idles in production and runs
instantly under a test. See §1.4.

**The brief's §1.3 (TCP sockets) is done, and `Pollable` landed with it** — which is where
it always belonged, since `epoll`/`kqueue`/IOCP need something to poll. `std/sysnet` is real
loopback TCP in both directions; `std/syspoll` + `runtime.Poller` is the readiness layer; and
`examples/std/sysnet_demo.jtr` is a status server that answers a connection **and** keeps
firing its own timers on one thread. See §1.5–§1.7.

**The next thing to build is the brief's §1.4, file watching.** It is the natural next
consumer of exactly this machinery — a watcher is a `Pollable` over a platform notification
handle (`inotify` on Linux, `ReadDirectoryChangesW` on Windows) — and everything it needs
from the loop now exists. §1.5 (structured logging) is the other cheap one and adds no new
platform surface at all.

Before either, read `std/runtime.jtr`'s header on `Poller`: **the loop reaches the operating
system only through the two handles it is given** (a `Clock` and a `Poller`), and keeping that
true is what makes the whole thing testable with no OS underneath.

### What is NOT what an older note says

* **The `jc build` name-collision bug is FIXED**, and the mechanism recorded for it was
  backwards. See §2.2. The census note's "9 of the 52 runnable multi-module corpus
  programs fail" is now **4 of 53**, and those four are one isolated mechanism.
* **`sys` at Tier 4 builds under `jc`**, sockets included. `examples/std/sysnet_demo.jtr`
  compiles through the self-hosted compiler's own loader and gcc driver — Winsock linked,
  `WSAPoll` declared, headers in the right order — and prints exactly what the reference
  toolchain's build prints.
* **`runtime.poll` is now `runtime.fire_due`.** An `extern` name is a C symbol and is
  globally reserved against any Jestyr function declared in exactly ONE module; POSIX
  readiness polling is spelled `poll(2)`. See §2.1b.

---

## §1. STD TIER 4 — what was built

### §1.1 — `std/syserr`: the platform error shape (brief §2.3)

`examples/std/syserr.jtr` + `syserr_test.jtr` (7 cases). Allowlisted and byte-identical
against the self-hosted backend first try.

```
SysError { pub raw: i32, pub plat: i32 }
```

**Two fields stored, one computed.** The brief asks for a raw code, a portable category and
a platform discriminator. The category is a pure function of the other two, so storing it
would make a `SysError` able to disagree with itself — a record whose `cat` says
`NOT_FOUND` and whose `raw` says `ENOSPC` is representable with three fields and not with
two.

**The mapping tables are pure, and that is the whole trick.** `category(raw, plat)` takes
the numbering system as an ARGUMENT and is not `@cfg`-guarded. `@cfg` buys "both platform
branches are type-checked wherever you build"; this buys the stronger thing — **both
branches are EXECUTED wherever you build**, because deciding what `ENOTEMPTY` means needs
no operating system. Only `last_raw` and one line of `here` are genuinely host-bound.

That ratio is better than it looks: a mapping table is exactly where a module like this is
wrong (a transposed digit yields a plausible category forever), and the tables are now the
part under test on every host.

**The POSIX table is two libcs, and the aliases are deliberate.** POSIX standardises errno
NAMES, not values; Linux and macOS agree from 1 to 34 and diverge above it, which is where
the filesystem errors live:

| | Linux | macOS |
|---|---|---|
| `ENOTEMPTY` | 39 | 66 |
| `ENAMETOOLONG` | 36 | 63 |
| `ELOOP` | 40 | 62 |

Both numbers are in the table for each name, and that is safe rather than sloppy for a
checkable reason: **each alias collides only with a non-filesystem error on the other
libc** (Linux's 66 is `EREMOTE`, its 63 is `ENOSR`; macOS's 39 is `EDESTADDRREQ`, its 36
is `EINPROGRESS`). Within the domain the table is scoped to, the union is unambiguous.

The alternative was `macos` in the `@cfg` vocabulary plus a third table — a branch nobody
in this repository can run, which is the failure `std/sysdir`'s header argues against at
length. A union provably unambiguous in-domain beats an untested branch.

`the_two_libc_aliases_agree_and_stay_out_of_the_shared_range` pins the checkable half: every
alias sits above 34, so none can shadow a code both libcs already spell.

**`errno` is a macro, so the function under it is bound.** `@cfg(posix) extern "errno.h" fn
__errno_location() -> cptr`, narrowed with an explicit `as *mut i32`. That function is
glibc's and musl's, not POSIX's — macOS spells it `__error()` — and it is named in the
header rather than hidden, because a "portable" errno accessor that silently fails to link
is worse and the fix when it matters is one `@cfg` branch.

**Renamed to dodge a known hazard:** `is_failure` rather than `failed`, `last_error` rather
than `last`. `failed` is a struct field in `std/ctfe` and `last` is one in `std/process` and
`std/cgen`, and `std/file`/`std/smallvec` already define functions by those names — the
plain spellings would have added two fresh instances of §2.2's bug. (They would now be
harmless, since §2.2 is fixed. The names are still better.)

### §1.2 — `std/sysfs`: the operations (brief §1.1)

`examples/std/sysfs.jtr` + `sysfs_test.jtr` (10 cases) + `sysfs_demo.jtr`.

`make_dir`, `remove_dir`, `rename_replace`, `metadata`, `exists`, `canonical`, `temp_dir`.
Every one is something ISO C cannot do, or cannot do the same way twice.

**Modification time is deliberately absent, and the reason is a number.** `mtime` is the
field a build daemon wants and the only one here that needs `struct stat` at a hard-coded
byte offset:

| | Linux x86-64 | Linux aarch64 | macOS |
|---|---|---|---|
| `st_mtim.tv_sec` | 88 | 88 | 48 |
| `st_mode` | 24 | 16 | 4 |

`std/sysdir`'s `D_NAME_OFFSET` is wrong on exactly one platform and is asserted by a test
the Linux CI job runs. `st_mode` is wrong on an ARCHITECTURE no CI runner here uses, and a
wrong `st_mode` reads a `uid` and reports a plausible file type. The discipline does not
transfer, so **nothing in this module reads a foreign struct's interior**. When `mtime`
lands it should bring `stat` with it behind a runtime check that validates the offsets
against a portable oracle rather than a comment.

**The cost `metadata` makes visible.** Windows answers "what kind of thing is this" in ONE
`GetFileAttributesA`. POSIX has no equivalent short of `struct stat`, so the POSIX branch
answers by TRYING: `access` for existence, an `opendir` that succeeds only for a directory,
then an `fopen` for the size. Up to three syscalls against one plus an open. Not papered
over — a caller walking ten thousand entries needs to know, and erasing it would be the
"nice API that erases cost" the brief's traps name.

**Errors are `!{ SysFsFailed(i32), SysFsRefused }`, and the split matters.** The payload is
the RAW platform code; the platform discriminator is recovered by `syserr.here`, because an
error produced on this host was produced by this host's numbering. So a single-scalar
payload loses nothing, `?` still works, and the existing discarded-fallible-result rule
already makes it must-use. A capability refusal is a SEPARATE member — conflating it with a
real `EACCES` would make a denied handle indistinguishable from a permission error, which
defeats the capability.

`fs.Fs` is reused rather than a second capability type invented: it is a POLICY value, and
one handle threading through `std/fs`, `std/walk` and `sysfs` is worth more than tier
purity. Refusal is reported by TYPE here rather than by count, because these operations
return an error union (`std/fs` counts because its operations answer `bool`, where a count
is the only discriminator). `fs.refused(f)` therefore does not move when `sysfs` refuses.

**Two operations answer `bool`, and both answers mean something:**

* `make_dir` → true if THIS CALL created it, false if it was already a directory
* `remove_dir` → true if THIS CALL removed it, false if it was already absent

Both make the operation idempotent, which is what callers wanted: an `EEXIST` on a
directory you were creating is information, not a failure.

**`rename_replace` answers nothing, and that is the payoff of §2.1.** It shipped returning
"did the destination exist immediately before" — a report that RACED, and which existed only
because a fallible function was required to declare a return type. With `JestyrResult_unit`
lowered it says exactly what it does and the extra probe is gone. A caller that needs to know
whether it clobbered something asks for itself and **owns the race visibly**, instead of being
handed a possibly-stale answer by a library with no reason to guess; `sysfs_demo.jtr` does
exactly that, and its output is unchanged.

**`already_a_directory` checks rather than assumes**, and that is the subtlest thing here.
`EEXIST` says the NAME is taken, not by what. A `make_dir` that turned every `EEXIST` into
`false` would report success where a regular file sits, and the caller's next write into
"the directory it just made" would fail somewhere far away.
`a_file_sitting_at_the_path_is_not_reported_as_an_existing_directory` pins it, with the
positive control beside it.

**What `rename_replace` proves, precisely.** That the destination NAME refers to the new
contents and no reader saw a partial file under it. NOT that the bytes reached stable
storage — neither platform flushes a file's data as a consequence of renaming it. And not
atomic across filesystems: POSIX gives `EXDEV`, Windows fails without
`MOVEFILE_COPY_ALLOWED`, both surface as `SYSERR_CROSS_DEVICE`, and the copy fallback is
deliberately not taken because a copy is not atomic.

**`canonical` is where the two platforms genuinely disagree.** POSIX `realpath` resolves
every symlink; Windows `GetFullPathNameA` is purely lexical. No wrapping makes them agree —
resolving links on Windows needs `GetFinalPathNameByHandleW` on an open handle, a different
operation with different permissions. What IS made uniform is existence (POSIX requires it,
Windows does not), because leaving that divergent would make the same call succeed on one
platform and fail on the other for a program that never mentions symlinks.

**`temp_dir` goes through `env.Env` on BOTH platforms, on purpose.** Windows has
`GetTempPathA` and it is not used, because what it does is read `TMP`, then `TEMP`, then
`USERPROFILE` — the same environment lookup, performed where a `sealed()` capability cannot
see it. Through the capability, a test seals the environment and gets the documented
fallback deterministically.

### §1.3 — The consumer: `jstage`, atomic publish

`examples/std/sysfs_demo.jtr`. Not an illustration — the three-step publish every tool that
writes an output someone else may be reading has to perform: idempotent `make_dir`, write
to a staging name, `rename_replace` onto the final name. **Step 3 is the one that needs this
module**: ISO C `rename` REFUSES an existing destination on Windows, so the portable
spelling is remove-then-rename, which leaves a window where the final name does not exist.

Its last section removes a non-empty directory on purpose. POSIX says `ENOTEMPTY`, Windows
says `ERROR_DIR_NOT_EMPTY` (145), and the rendered line reads
`directory not empty (windows error 145)` / `directory not empty (posix errno 39)`.
`jstage_publishes_atomically_and_reports_a_portable_category` asserts the category text as a
literal and the raw number as merely present-and-platform-appropriate — the claim
`std/syserr` makes, tested as a claim.

The same split is the point of
`a_non_empty_directory_refuses_with_a_portable_category` in the suite: one assertion against
`SYSERR_NOT_EMPTY`, holding on three platforms, with none of 39 / 66 / 145 appearing
anywhere in the test file.

### §1.4 — Event loop V1 (the brief's §1.2): the first slice is DONE

`std/time` gained waiting; `std/runtime` gained cancellation tokens and a waiting poll.
`runtime_test.jtr` is 5 → 10 cases, and `runtime_demo.jtr` is the consumer.

#### `Pollable` was NOT built, and the reason was measured rather than assumed

The previous note's §5 said to build it next. That was wrong, and checking took two minutes:
**`epoll`/`kqueue`/IOCP need something to poll, and nothing in this tree exposes a file
descriptor or a socket.** `std/file` holds a `FILE*` as an opaque `cptr`. Even reaching for
POSIX `fileno`, a regular file always polls ready (useless), and Windows `WSAPoll` refuses
anything that is not a socket.

So shipping a `Pollable` now would be precisely what `std/runtime`'s own header argues
against — *"adding a `Pollable` that could not poll would be worse than having none"*.
**It belongs with sockets (the brief's §1.3), not before them**, and the brief's §1.2 first
slice never asked for it: handle, timers, cancellation token, task handles, manual runtime,
one hosted runtime. Four of those already existed; the two that did not are below.

#### Waiting belongs to the CLOCK, and that is the whole design

A loop with nothing to do must idle, or a hosted server burns a core. A loop that SLEEPS
makes every test take as long as its longest timer. Both are satisfied at once if waiting
goes through the same handle reading already does:

```
time.wait(mut c: Clock, nanos: i64) -> i64      // returns what it ACTUALLY waited
    host()    — really sleeps
    manual()  — ADVANCES THE CLOCK and returns instantly
```

So the identical code path idles in production and runs at full speed under a test, **and
the test calls no `advance` of its own — waiting IS advancing**. `runtime_test`'s
`poll_for_waits_through_the_clock_and_names_what_happened` never advances the clock, and
still asserts the exact instant every timer fired.

It lives in `std/time` rather than `std/runtime` because sleeping needs a portability
DECISION (`usleep` against `Sleep`), and the clock already WAS the OS boundary for reading.
Nothing new is exposed by making it the boundary for waiting too.

Two details worth keeping:

* **`usleep`, not `nanosleep`.** `nanosleep` takes a `struct timespec*`, and reading a
  foreign struct's interior is exactly what `std/sysfs`'s header refuses to do for `stat`.
  `usleep` takes a scalar, so there is no layout to guess. It is obsolescent in POSIX 2008
  and present everywhere that matters; when that stops being true, `nanosleep` arrives WITH
  a checked layout rather than a hoped-for one.
* **Windows rounds UP to whole milliseconds**, so a sub-millisecond request becomes
  `Sleep(1)` — a yield rather than a spin. Rounding down would turn every short wait into a
  busy loop. The default Windows granularity is coarser still (~15ms unless something has
  called `timeBeginPeriod`), which is why `wait` REPORTS what it actually did instead of
  letting the caller assume.

`time.jtr` now carries `@cfg`, so it (and `time_test.jtr`, and `runtime_demo.jtr`) joined
`CGEN_GOLDEN_ALLOWLIST` — byte-identical first try. **No reseed**: neither `time` nor
`runtime` is in the 14-module self-host closure.

#### `CancelToken` — a group handle, not an id and not a flag

`cancel(rt, id)` already existed and is right when you hold the thing you scheduled. It is
wrong for the case a long-running program actually has — *stop everything this
request/connection/build started* — because the caller would have to keep every id it ever
handed out.

```
token(rt) -> CancelToken     detached() -> CancelToken     // the explicit "no group"
after(rt, nanos, tok, task) -> TaskId
cancel_all(rt, tok) -> i64      is_cancelled(rt, tok) -> bool
```

Four decisions, each with a test that fails if it is reversed:

1. **The token is REQUIRED, and `detached()` is the spelling for "no group".** An optional
   token would make the common case default to ungrouped silently, which is the ambient
   shape the brief says to avoid. The choice is visible at every call site either way.
2. **`cancel_all(detached())` cancels NOTHING.** A wildcard here would let one careless call
   stop every unrelated timer in the loop. Pinned in
   `a_token_cancels_its_whole_group_and_leaves_everything_else_alone`, which would still
   pass without it — that is why the assertion is written separately.
3. **`is_cancelled` exists because a timer list cannot answer it.** Work already in flight
   has no timer left to cancel; the only way it can learn to stop is to ask. That is the
   half that makes this a token rather than a bulk-cancel helper.
4. **Registering under an already-cancelled token is REFUSED** (`-1`). Accepting it would
   let a straggler outlive the cancellation meant to stop it — everything looks cancelled
   and one task still runs, which is the version of this bug that is hard to see.

#### `poll_for` — the loop can finally idle, and says which of three things happened

`poll(rt) -> i64` returned a count, and a count cannot distinguish *nothing was due* from
*there is nothing left to do*. A loop needs that difference: the first means wait, the
second means exit.

```
poll_for(rt, timeout_nanos) -> Event { kind, fired, waited }
    RT_FIRED    at least one timer ran
    RT_IDLE     nothing is scheduled at all; the loop may stop
    RT_TIMEOUT  work is still pending, the budget ran out first
run_for(rt, budget_nanos, rounds) -> Event
```

The wait is clamped to the next deadline, so a large timeout cannot overshoot a timer. A
runtime with nothing scheduled returns `RT_IDLE` immediately and never waits — nothing could
arrive to end the wait, so sleeping would be a hang with extra steps. **That is the line to
invert when `Pollable` lands**: a runtime with no timers may still be waiting on a socket,
and `RT_IDLE` will have to mean "no timers AND no pollables". The note is in the source.

**No error set, and it was measured rather than assumed.** Nothing here can fail: `after`
cannot, and an interrupted or short sleep is not an error — it is a shorter wait, which the
`Event` already reports in `waited`. Inventing `!{ RuntimeError }` would have produced
`std/diag`'s deleted `failed()` in a new costume: a query that can only answer "fine".

#### The consumer: `jheartbeat`

`examples/std/runtime_demo.jtr` — a supervisor that publishes a status file on a timer and
shuts down on its token. It is the smallest program that needs the whole tier: the schedule
(`after` + token), idling (`poll_for`), a status file a concurrent reader can never catch
half-written (`sysfs.rename_replace`), and a shutdown in-flight work can observe.

Two things it demonstrates that are easy to get wrong:

* **The publish happens in the LOOP, not in the callback.** A callback runs inside the fire
  pass, so blocking I/O there stalls every other timer and a "10ms" timer drifts to whatever
  the disk felt like. The timer PACES the work; the loop DOES it.
* **It re-arms before it cancels.** The first draft cancelled first, so `cancel_all` reported
  zero and demonstrated nothing. `jheartbeat_paces_publishes_and_shuts_down_on_its_token`
  asserts `timers killed 1` and separately asserts the output does NOT contain
  `timers killed 0`.

Its output is asserted to the digit — `beat=3 at_ms=300`, `simulated elapsed ms 300` —
which is only possible because `manual()` makes waiting exact. With a host clock the best
that test could say is "roughly 300ms, usually".

### §1.5 — `std/sysnet`: TCP over IPv4 (brief §1.3)

`sysnet.jtr` + `sysnet_test.jtr` (5 cases). Listener, stream, connect / bind+listen / accept
/ send / recv / close, and a `SocketAddr`. IPv4, blocking sockets, no TLS, no DNS.

**Four divergences, all named in the header**: the handle is an `int` on POSIX and a 64-bit
`SOCKET` on Windows; the failure sentinel is `-1` against `INVALID_SOCKET` (equal only as a
signed 64-bit value, so it is *checked*, not assumed); Windows needs the library STARTED; and
closing is `close()` against `closesocket()` — two different functions, not two spellings.

**`WSAStartup` hangs off the CAPABILITY, and that is the design.** It is process-global
initialization — exactly the hidden global this library refuses to have — so `net.host()`
starts Winsock and `net.shutdown()` stops it. The thing you must not forget became the thing
the type system already makes you hold.

**`sockaddr_in` IS read by byte offset, and unlike `struct stat` that is defensible.** It is
wire-adjacent, fixed at 16 bytes since 4.2BSD, and Linux and Windows agree byte for byte;
`std/sysfs` refuses the `stat` equivalent because THAT layout varies by architecture with no
CI runner covering it. macOS splits the first two bytes (`sin_len` + `sin_family`), and
`an_address_round_trips_through_its_wire_bytes` asserts the exact bytes so a BSD gets a red
test rather than a connection to nowhere.

`send`/`recv` are byte-assembled rather than routed through `htons`/`htonl`, which are macros
on several platforms and therefore unbindable — and writing the bytes puts the one fact a
reader must know (port and address are BIG-ENDIAN on the wire) in the code.

### §1.6 — `std/syspoll` + `runtime.Poller`: the loop grows IO (brief §1.2's other half)

`syspoll.jtr` + `syspoll_test.jtr` (3 cases), plus `Poller`, `watch`/`unwatch`, `RT_READY`
and a new idle rule in `std/runtime`.

**`poll_for`'s header note is inverted, exactly as it promised.** `RT_IDLE` now means "no live
timers AND no watched pollables"; a server with a watch and no timers is not finished, it is
waiting.

**The runtime still touches no OS.** Readiness arrives through a `Poller` — a `ctx` +
fn-pointer pair, the `mem.Allocator` shape — so `std/syspoll` supplies the kernel's answer and
a test supplies a scripted one. That is why `epoll`/`kqueue`/IOCP being three models does not
infect the loop: they are three `Poller`s behind one signature and the loop cannot tell them
apart. `a_scripted_poller_drives_the_loop_with_no_sockets_at_all` proves registration, firing,
level-triggering, cancellation and the idle rule **with no operating system involved**;
`a_ready_socket_is_reported_ready` is the one end-to-end check of the per-platform `pollfd`
layout, and it is a real check rather than a layout assertion — a wrong offset reports nothing
ready, the wait runs out, and it fails.

**The wait is the poller's, clamped to the next deadline.** With watches registered a socket
is the only thing that can end the wait early — but a timer still fires on time, which is what
`jstatus`'s `the timer fired, not the socket` asserts. Consequence to know: on that path the
clock is only READ, so **a manual-clock test with watches drives time with `advance`** rather
than by waiting. The timer-only path is the one where waiting IS advancing.

**One token cancels a watch and its timer together** — the reason cancellation is a group and
not an id, now with a second kind of thing in the group.

Watches are LEVEL-TRIGGERED and stay registered: "there is more to read" is the question
`accept` and `recv` are actually asking. One-shot is `unwatch` from inside the task, which is
safe for the same reason cancelling a timer from a callback is — the loop marks, it never
removes.

### §1.7 — The consumer: `jstatus`, a local status server

`sysnet_demo.jtr`. The smallest program that needs all three layers: one thread answering a
connection *and* firing its own timers. **The callback MARKS and the loop ACTS** — the watch's
task records readability and the loop does the accept — which is the same mark-do-not-remove
discipline the runtime uses on timers, and it keeps every operation that can fail out of a
callback with nowhere to report a failure.

---

## §2. WHAT THE BUILD TURNED UP IN THE COMPILER

### §2.1 — A fallible function with NO return type: was a miscompile, now LOWERED

`fn f(x: i32) !{ Bad }` parsed, type-checked, and `jestyrc check` reported **ok**. The C it
produced failed three ways:

| form | what gcc did |
|---|---|
| bare `return` on the ok path | `return;` from a function returning `JestyrResult_unit` — a **warning**, so it compiled and handed back an uninitialized tag |
| `f(x)?` | `error: 'JestyrResult_unit' has no member named 'ok'` |
| `f(x) catch` + rethrow | the same |

The first is the dangerous one: it is the only shape gcc does not refuse, so the *working*
path was the one that miscompiled.

**It is now implemented rather than refused**, because the shape is the natural signature for
most of a `sys` layer (`close`, `bind`, `commit`, `cancel`, `shutdown`) and every alternative
distorts the library — `sysfs.rename_replace` had invented a racy bool purely to have
something to return.

Four emission changes, and **less than half of it was new**: `emit_result_def` already
emitted the ok-member-free typedef for `Ty::Unit`, and BOTH `catch` recovery forms already had
a `Ty::Unit` arm. What was missing:

1. **A valueless `return` in a unit-fallible fn** constructs `{ .is_err = false }` (routed
   through `emit_value_return`, so `ensures` and drops run as for any value return).
   **Scoped to unit deliberately** — a valueless `return` out of a `-> T !E` has no `T` to
   supply, so synthesizing one there would manufacture a zero-valued *success*, strictly
   worse than the pre-existing bare `return;`.
2. **`?` yields nothing** when the base's ok type is unit; the statement-expression's type
   becomes `void`, which is right, since `f(x)?` on a unit-fallible callee is a statement.
3. **The rethrow form (`catch` + `return e`)** — the same, and it had been missed on both
   sides.
4. **Falling off the end is SUCCESS**, and it is now spelled. `fn f(…) !{ E } { if bad {
   return err(E) } }` is the natural way to write "fails early or completes", and it used to
   run off the end of a non-void function. Well-defined ONLY because the ok type is unit —
   there is no value to invent. **A `-> T !E` (or a plain `-> T`) falling off the end is a
   different, pre-existing gap and is untouched: `fn g(x: i32) -> i32 { if x > 0 { return 1 }
   }` still compiles and returns garbage.** That is a real bug and a good next find.

**The port mirror needed one thing the reference did not**: `push_ty_mangle` had no `Unit`
arm, so a unit Result mangled to `JestyrResult_?`. `tyid < 0` ("no type at all") already gave
`unit`; a RESOLVED Unit TyData fell through to the `?` default. Invisible until a corpus file
first had a unit result.

Two structural notes worth carrying: the reference uses a **one-shot `unit_tail` flag** taken
with `mem::take` at each body emitter, because `emit_body` is shared with `if` branches and
match arms; the port uses **`depth == 0`** instead, since all six of its fn-body call sites
pass a literal `0` and every nested body is reached at `depth + 1`.

Seed refreshed. Zero golden churn — no corpus file emitted the shape, and `examples/vec.jtr`'s
`push` (the corpus's only instance) is a method on an uninstantiated `comptime T` factory that
never reaches cgen at all.

### §2.1a — And it immediately exposed one more degrades-to-gcc row

`let b: bool = f(x) catch true` on a unit-fallible `f` passed `check` and failed in gcc with
*void value not ignored as it ought to be*. `assignable` had no rule for `Ty::Unit`, because
until now nothing could produce one. **Unit converts to nothing and nothing converts to
Unit** — symmetric, and pinned with the positive control that the same call in STATEMENT
position is still clean. Reference-only (a diagnostic, no emitted byte), so no mirror.

The general shape is worth remembering: **a new type's first real inhabitant finds every place
the checker had no rule for it.**

### §2.1b — An `extern` name is a C symbol, and TWO things were renaming it

Same root cause, two independent bugs: **an `extern` declaration's name belongs to the
linker, not to Jestyr.**

* **The port's loader renamed it.** `ml_scan_decls` registered every `fn` name as collidable,
  an extern's included — so `std/file` and `std/sysdir` both declaring `close` made
  `std/sysnet`'s `extern "unistd.h" fn close` become `close__m7`, an `undefined reference` at
  link time. SILENTLY, because the loader has no idea the name belongs to someone else's
  object file. Fixed by skipping registration when the token two before `fn` is `extern`.
* **The reference canonicalized it.** `canon_in` produced the same `close__m7` while
  `table.fns` stores externs under the BARE name, so every call inside the extern's own
  module failed with *cannot find `close` in this module; it is defined in module `sysdir`*.
  Fixed with `extern_owned: HashSet<(ModId, String)>`, keyed on `(module, name)` — keying on
  the name alone would have given `file.close` and `sysdir.close` the same C symbol.

**The rule that falls out, worth knowing before binding anything:** an extern name collides
only with a Jestyr function declared in **exactly one** module. `close` is declared by two, so
both are already canonicalized and the bare name is free. `poll` was declared only by
`runtime`, so it genuinely collided — which is why `runtime.poll` is now `runtime.fire_due`.
(A better name regardless: this one fires what is due and returns; `poll_for` is the one that
waits.)

### §2.1c — Three toolchain facts the socket layer needed, two of them silent

* **`-lws2_32` must be linked, and linked LAST.** mingw does not link Winsock by default, and
  a flag that is *present but early* fails identically to a missing one — GNU ld resolves
  libraries against the objects seen so far. A debugging round went on `undefined reference to
  __imp_socket` with the flag visibly in the command. There were FOUR link sites across the
  tree and only three had it; they are one `link_and_finish` helper now.
* **`-D_WIN32_WINNT=0x0600`, or `WSAPoll` is an implicit declaration.** mingw declares it only
  at Vista or later; below that C accepts the call and gives it an `int` return. The
  `int`-fallback silent miscompile, met for the fourth time in this tree.
* **`<winsock2.h>` must precede `<windows.h>`.** `windows.h` pulls in Winsock 1.1, so the
  other order makes `winsock2.h` collide with what it has already seen — mingw downgrades that
  to a `#warning`, MSVC does not. One stable `sort_by_key` in the include emission, mirrored in
  the port as a two-pass scan.

### §2.1d — Two more reference/port divergences the corpus had never reached

Both LATENT: the port had been wrong for a long time and nothing exercised the shape.

* **The `?` temp was numbered before its base's.** `send_bytes(s, buf[off .. buf.len])?` is the
  corpus's first `?` over a call carrying a slice-RANGE argument; the reference numbers the
  range's `_s2` before the try's `_q3`, the port numbered `_q2` before `_s3`. The port's Catch
  arm already carried a comment saying the base must be emitted before the temp is allocated —
  Try had simply never been given the same treatment.
* **`push_ty_mangle` had no `Unit` arm**, so a unit Result mangled to `JestyrResult_?`.
  `tyid < 0` ("no type at all") already gave `unit`; a RESOLVED Unit `TyData` fell through to
  the `?` default. Invisible until a corpus file first had a unit result.

**The pattern across all of this session's port divergences is the same**: the port is
byte-identical on everything the corpus exercises, and each new module finds one more shape it
never did. That is an argument for writing new modules against the PORT as well as the
reference, not just adding them to the allowlist afterwards.

### §2.1e — `@cfg` items sharing a name must share a SIGNATURE. NOT fixed

`@cfg` lets two items share a name when their platforms are disjoint — but typeck keys its
function table on the BARE name, so the **second** declaration wins and BOTH branches are then
checked against ONE signature. `std/sysdir` never met this because its POSIX and Windows
externs have different names (`opendir` against `FindFirstFileA`); sockets have the same names
with different C types, and the POSIX call sites were reported as `expected i32, found u32`
against the Windows extern.

Left open deliberately: it degrades to a type error at the call site rather than a miscompile,
so it is noisy-but-safe. **Worked around by unifying the signatures, which is the better
binding anyway** — these are header-declared externs, so no prototype is emitted and the
Jestyr signature only has to describe something C can implicitly convert from (`i64` covers an
`int` fd and a 64-bit `SOCKET`; `i32` covers `socklen_t`). A diagnostic naming the mismatch
would be a good small increment.

### §2.2 — The `jc build` collision bug is FIXED, and the recorded mechanism was backwards

Recorded as: *"the rename rewrites the FIELD ACCESS too"*. Measured, it is the inverse.

`ml_rewrite` in `examples/std/cgen.jtr` skips renaming a `.`-preceded token, so a field
ACCESS `d.open` was always correctly left alone. What was not skipped was the field
**declaration**:

```
std/sysdir declares `pub fn open` AND a `Dir` field `open: bool`.
std/file also declares `pub fn open` → `open` collides → renamed.
   `d.open`               .-preceded → untouched          (correct)
   `open: bool`           not .-preceded → `open__m3`     (WRONG)
   `Dir{ …, open: false }` not .-preceded → `open__m3`    (WRONG)
```

So the struct became self-consistent under a name none of its seven accessors used, and
`jc` reported seven "type error at this expression" inside a module the user never edited.
`jestyrc` built the same program.

**The fix is one more token-level exclusion**, not a scheme for telling `mod.item` from a
field access — that half already worked:

```jtr
if !prev_dot and (!next_colon or prev_const) {
```

`ident :` is a BINDER — field declarations, struct-literal fields, function parameters,
`let`/`var`, enum payloads — and never a use of a top-level name. **Except `const NAME: T`,
which is the one top-level form spelled that way**, and leaving it out broke `test_demo`
instantly: `std/test` and `std/cli` both declare `BACKSLASH`, so its uses were renamed while
its declaration was not. `distinct Name = Ty` needs no carve-out because it spells `=`.

Measured A/B over every multi-module corpus program:

| | BUILD_OK | FAIL |
|---|---|---|
| before | 43 | 9 (of 52 — reproduces the census audit's number exactly) |
| first attempt | 46 | 7 — **and `test_demo` regressed** |
| after | **49** | 4 (of 53) |

Six programs fixed, zero regressions: `caps_demo`, `census_cli`, `process_demo`,
`sysfs_demo`, `test_fixture_demo`, `writer_demo`.

`cgen.jtr` is a closure module, so **the seed was refreshed** (`bootstrap/jestyr_flat.jtr`
+ `jestyr_seed.c`). No golden churn: the raw-dump golden resolves no imports and never runs
`ml_*`.

### §2.3 — The gate the census audit owed: `docs/jc_build_matrix.txt`

`jc build` is the only path that drives the port's module loader, and nothing covered it.
Both gates that look like they would are blind by construction — `selfhost_fixpoint_subset`
`continue`s on any file containing `import "`, and `jestyr_cgen_matches_reference` feeds
every file to both backends with imports UNRESOLVED. A program could be byte-identity
verified *and* unbuildable, which nine of them were.

`jc_build_matrix_matches_expectations` runs `jc <file> build` over every
`examples/std/*.jtr` with a `fn main()` and an import, and diffs against a committed
expectations file. **An expectations file, not a pass/fail gate, and it fails in both
directions** — four programs still do not build, so green/red would be permanently red or
quietly relaxed.

That earned its keep immediately: it is what caught the `test_demo` regression above.
Regenerate with `JC_BUILD_MATRIX=1`. Runtime ~150 s, `c-oracle` only.

**The four remaining failures are ONE mechanism, now isolated** (the census recorded it as
"mechanism not isolated"): the port emits a generic typedef's NAME into a prototype without
emitting the typedef.

```
combinators  JestyrFn_fn_di32_ret_?          fn-pointer typedef, return mangled empty
mutex        JestyrFn_fn_dptr_i64_ret_unit   fn-pointer typedef never defined
slice_algos  JestyrFn_fn_ri32_ri32_ret_bool  same, with a `read` parameter
try_read     JestyrResult_                   un-annotated `let ok = fs.try_read_text(p)`
```

The first three are one def-emission gap: a fn-pointer instance reached only through the
flattened program is named but never emitted. `try_read` is the other half — the port's
INFERENCE, not its emission. Annotating that `let` would work around it, and that is
deliberately not done, because the workaround would hide the gap the file records.

### §2.4 — `catch |e|` did not bind `e` when the base failed to resolve

Found by the P3 typeck golden, which runs the **whole corpus with no allowlist**.
`sysfs_test.jtr` is the first corpus file to put a `catch |e| match e { … }` on a fallible
call into ANOTHER module; with imports unresolved the base degrades, and the two sides
disagreed about the binder:

```
reference:  e : ?        (the fallback was inferred with no binder scope pushed)
port:       e : error
```

**The port had the better answer and the reference adopted it.** The binder exists because
the syntax says so; whether the base's type could be RECOVERED has nothing to do with
whether the name is in scope. Leaving it out meant a program with one real problem — the
unresolvable callee — reported a second, invented one underneath it.

Four lines in `src/typeck.rs`'s degraded arm: push the scope, insert the binder as
`Ty::Prim("error")`, infer, pop. Reference-only (the port already did this), so no mirror
and no reseed. Pinned by
`catch_binds_its_error_name_even_when_the_base_does_not_resolve`, with a positive control
on the recovered path and an anti-vacuity control that a non-binder `e` is still `i32`.

Worth noting how it was found: **the file that exposed it is a test suite for a library, not
a compiler fixture.** The corpus's coverage of "a fallible call across a module edge, caught
and destructured" was zero until a `sys` module needed one.

---

## §3. OPEN

### The byte-identity exclusion this session added

**`sysfs_test.jtr` is not in `CGEN_GOLDEN_ALLOWLIST`, and the reason was measured.** It is
the first corpus file to put a `catch |e| match e { … }` on a fallible call into ANOTHER
MODULE. With `sysfs.make_dir` unresolvable in the golden's import-unresolved mode, the
catch's ok type degrades and the two sides degrade it differently — one C token:

```
reference:  bool j_made = ({ … int  _cv4; … })
port:       bool j_made = ({ … void _cv4; … })
```

Rebuilt self-contained with the fallible function declared LOCALLY, **both emit `bool _cv1`
and agree byte-for-byte**. So it is a disagreement about how far to degrade an erroneous
program, the same category as `walk.jtr`'s auto-drop divergence, and neither can affect a
program that compiles. `syserr.jtr`, `syserr_test.jtr` and `sysfs.jtr` ARE allowlisted and
were byte-identical first try.

### Tier 4 remaining, in the brief's order

| § | item | state |
|---|---|---|
| 1.1 | `sys/fs` | ✅ this session |
| 1.2 | deterministic `std/walk` | ✅ Tier 3 |
| 1.3 | event loop V1 | ✅ — cancellation, waiting `poll_for`, and now `Poller` + `watch` |
| 1.4 | TCP sockets | ✅ this session — `std/sysnet`, real loopback both directions |
| 1.5 | file watching | ✅ — `std/syswatch`, inotify / change notifications. See the successor note |
| 1.6 | structured logging | ⬜ — **start here** |
| 1.7 | HTTP/1.1 | ⬜ |
| 1.8 | tar / reproducible archive | ⬜ |
| 1.9 | Unicode display width | ⬜ — `std/diag`'s `caret_alignment_is_byte_based_and_tabs_expand` is the pin to invert |
| 1.10 | append-only log | ⬜ — `rename_replace` is the crash-safe primitive it needs, and it now exists |
| 1.11 | plugin process protocol | ⬜ |

### Language obligations, restated against what is now known

* **§2.1 move-only resources** — still owed. `sysdir.Dir`, `file.Reader`/`Writer` are
  freely copyable today. Move-only droppables landed in v3 but only for `take`/rebinding of
  a droppable NAME; a `Dir` has no `Drop` impl, so nothing stops duplicating a live handle.
  This gets sharper the moment sockets exist.
* **§2.2 must-use** — the error-union half is done (v3). `@must_use` on a NON-union return
  is still enforced only by gcc's `warn_unused_result` (`src/cgen.rs`, the
  `"must_use" => gnu.push("warn_unused_result")` row). The front end accepts the attribute
  and never checks it — a degrades-to-gcc instance sitting in plain sight. `FnSig` has no
  attribute field; adding `must_use: bool` there (two construction sites, `src/typeck.rs`
  ~595 and ~624) and checking it at the discarded-statement seam (~2880, beside the existing
  rule) is the shape.
* **§2.3 platform errors** — done, this session.
* **§2.4 runtime ownership** — untouched; the event loop's job.
* **§2.6 FFI contracts** — `extern "<hdr>.h"` emits no prototype and lets the header own the
  declaration, which is what makes `stat`/`__errno_location` bindable at all. `sys/stat.h`
  and `unistd.h` (subdirectory and plain) both work; the rule is only the `.h` suffix.
* **§2.7 concurrency with ownership** — untouched. Still no `Send`-like marker.

---

## §4. TRAPS

**A `match` ARM BODY must be an expression.** A bare assignment cannot be a `catch`
fallback, and a `return` there abandons the caller. The shape that works is a helper that
records into a `mut` slot and returns the ok type — `stamp_bool` / `stamp_str` /
`stamp_meta` in `sysfs_test.jtr`. Worth copying rather than rediscovering.

**A module-qualified struct LITERAL does not parse** (v3 §3.2, still true): `sysfs.Meta{ … }`
from another file is a parse error though `sysfs.Meta` in type position is fine. Any `pub
struct` a caller is expected to build needs an exported constructor — `sysfs.missing()`
exists for exactly this.

**`out` is a keyword.** `var out: []u8` is a parse error with a confusing cascade
(`expected variable name, found 'out'`). It is the `out` parameter convention.

**`git checkout examples/std/` will revert your `cgen.jtr` edit.** Done accidentally in this
session while cleaning up `jc build` artifacts. `jc build` writes `<stem>.c` and
`<stem>.exe` beside the source; remove those two by name.

**Do not add a `pub fn` whose name is also a struct field elsewhere in the tree** — §2.2
made this survivable, not free. The remaining hazard class is anything the token-level
rewriter still cannot distinguish.

**A new `@cfg`-bearing file MUST go in `CGEN_GOLDEN_ALLOWLIST`.**
`every_cfg_bearing_corpus_file_is_byte_identity_verified` scans the corpus and fails
otherwise. It caught this session's first new file within minutes of it being written.

**`errno` is only valid immediately after the failing call.** Neither platform clears its
slot on success. Every operation in `sysfs` reads it on the failure branch and BEFORE
freeing the `CString`, because `free` is not required to preserve `errno` on every libc.

**When a limitation is load-bearing, pin it.** `rename_replace`'s racy bool, the deferred
`mtime` offsets, the symlink divergence in `canonical`, and the byte-identity exclusion above
are all written down with the thing that must change when the blocker moves.

---

## §5. Suggested order

1. ~~`JestyrResult_unit` lowering~~ — **done**, see §2.1. It found two more bugs on the way
   in (the rethrow form, the port's `Unit` mangle) and one on the way out (§2.1a).
2. ~~Event loop V1~~ — **done**, see §1.4. `Pollable` was measured and deferred to sit with
   sockets: nothing in the tree is pollable yet.
3. **TCP sockets** (the brief's §1.3), then a local status server as the consumer — and
   `Pollable` alongside them, since a socket is the first thing worth polling. `RT_IDLE`
   will need to mean "no timers AND no pollables"; the note is in `runtime.poll_for`.
4. **`@must_use` enforcement** (§2.2) — cheap, and it is a degrades-to-gcc row.
5. **The four `jc_build_matrix` failures** — one def-emission gap plus one inference gap,
   both now isolated.
6. Watching, logging, HTTP, tar, Unicode width, append-log, plugins.
