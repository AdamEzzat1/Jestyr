# Std v4 — TIER 4 IS COMPLETE. What is left is language work.

Cold-start note. **§0 is what to do next.** Then HTTP (§1), tar (§2), what they turned up
(§3), **the language obligations and compiler follow-ups (§4 — read this before picking
anything)**, traps (§5).

```bash
cargo build --release && cargo test --release --features "c-oracle,selfhost-fixpoint"
```

**1279 passed / 0 failed / 3 ignored.** `jc_build_matrix` is **62 of 62**.

---

## §0. START HERE

**Every item in the Tier 4 brief is done.** §1.1–§1.11, all of them, each with a real consumer
and a test that checks its claim rather than describing it.

So there is no obvious next module, and that is the point of this note: **what is left is not
library work.** §4 is the queue, and it is ordered by leverage rather than by brief number:

1. **A `sys` process module** — not in the brief, and the biggest hole in the tier (§4.1).
2. **`@must_use` on a non-union return** — cheap, and a degrades-to-gcc row sitting in plain
   sight (§4.2).
3. **Move-only resources (§2.1 of the brief)** — now owed by SIX handle types and getting
   worse with every module (§4.3).
4. The three compiler follow-ups in §4.5, one of which is now closed.

---

## §1. `std/http` — HTTP/1.1, and the ambiguities it refuses

`http.jtr` + `http_test.jtr` (5 cases) + `http_demo.jtr`.

**The parser is `core`: no allocation, no OS, no sockets.** That split is why it is testable —
the dangerous half of an HTTP implementation needs no network, and only the demo does.

### The module's actual job is refusing ambiguity

Parsing HTTP is easy. What matters is that **a message two implementations read differently is
the entire request-smuggling vulnerability class**, so `HTTP_AMBIGUOUS` is its own answer:

* `Content-Length` **and** `Transfer-Encoding: chunked` — a proxy honours one, the origin the
  other, and one request becomes two, the second attacker-controlled and attributed to the next
  client on the connection.
* Two `Content-Length` headers that **disagree**. Two that agree are merely redundant and are
  accepted — the line is "can two implementations disagree", not "is it tidy".
* A `Content-Length` that is not a plain decimal: `+5`, `0x5`, empty.
* A **bare LF** as a line ending, and an **obs-fold**, and a **space before the colon**. All
  three are historical leniencies, and each is a way for two parsers to split a message
  differently.

Both are a 400 to the client. They are different answers because they are different BUGS, and a
log that distinguishes them tells an operator whether they are looking at a broken client or an
attack.

### Three answers, not two

`HTTP_INCOMPLETE` is the one a naive parser omits, and omitting it makes a streaming server
unable to tell a slow client from a hostile one — with only ok/bad, a half-arrived request is
"bad" and the connection dies on every slow network. `HTTP_TOO_LARGE` is the other: an
unbounded parser is a denial of service, so a maximum line length and header count are
refusals rather than allocations.

### The consumer

`jserve` puts it on real loopback through `std/sysnet`. Three requests; the middle one is a
genuine smuggling attempt whose trailing bytes are `GET /admin HTTP/1.1`. It answers
**400 ambiguous**, the smuggled body is never served, and the chunked request AFTER it is
served normally — so a server that desynchronised would fail the last assertion.

### What it does not do

No TLS, no compression, no HTTP/2 or /3, no keep-alive state machine, no timeouts, no URL
parsing. Chunked bodies are decoded but not encoded.

---

## §2. `std/tar` — an archive that is byte-identical every time

`tar.jtr` + `tar_test.jtr` (4 cases) + `tar_demo.jtr`.

### "Reproducible" is the requirement, so it is what the tests check

Four things make an archive differ from one built a second later. Three are removed **by
default, with no override**: mtime is 0, uid/gid are 0 with empty names, and mode is 0644/0755
rather than whatever a umask produced. Block padding is zeroed rather than left holding
whatever was in the buffer — which is both an irreproducibility and a memory leak into a file.

**The fourth is ENTRY ORDER, and this module cannot fix it**: a directory walk's order is the
filesystem's. The caller must add entries deterministically, `std/walk` already sorts for
exactly that, and it is said out loud because a module that silently depended on its caller for
its headline property would be making a promise it does not keep.

`an_archive_of_the_same_entries_is_byte_identical` builds one twice **from two deliberately
different dirty buffers** and compares every byte. That is the only way the claim can be
believed, and it is the assertion a `time(0)` in the header fails.

The defaults are asserted as BYTES (`mtime 0`, `mode 0000644`) so an "improvement" that writes
real timestamps has to change the test on purpose.

### The checksum quirk

**The header checksum is computed with its own field taken as eight SPACES**, not zeros. A
writer that sums the zeroed field produces an archive every real `tar` rejects — and no
round-trip test would notice, because the reader would agree with the writer. `header_checksum`
is `pub` and pure so the rule is pinned directly.

### Verified against the real `tar`

Recorded rather than run in CI, because a test that depends on which `tar` is installed fails
on someone else's machine for a reason that is not a bug:

```
$ tar -tvf zz_jpack.tar
-rw-r--r-- 0/0    32 1969-12-31 16:00 README.md
drwxr-xr-x 0/0     0 1969-12-31 16:00 src/
-rw-r--r-- 0/0    30 1969-12-31 16:00 src/main.jtr
$ tar -xf zz_jpack.tar -C out      # contents byte-correct
```

Modes, ownership and the epoch mtime are exactly what the module claims to write.

### The refusals

A name over 100 bytes, or a file over 8 GiB, does not fit a USTAR header and is **refused, not
truncated** — a silently shortened path extracts to the wrong place, which is a security bug.
`prefix`, GNU and PAX long-name extensions are not implemented, and the refusal is what makes
that safe.

**There is no extraction to disk, deliberately.** `read_entry` hands back a header and a body
span; deciding whether a path is safe to write is the caller's. A tar extractor that writes
wherever the archive says is the `../../etc/passwd` vulnerability, and not extracting keeps it
out of the blast radius.

---

## §3. WHAT THEY TURNED UP

### §3.1 — A `mut` sub-slice cannot be a call argument

```jtr
fn writeinto(mut b: []u8) -> usize { … }
writeinto(buf[have .. buf.len])     // error: the C backend does not support ranges yet
```

Narrowed by probe: it is **the mutability, not the `catch`** it was first seen under, and not
ranges in general. The `read` equivalent compiles and runs; so does the same range as a `let`
initializer. So the workaround is one line:

```jtr
var win: []u8 = buf[have .. buf.len]
writeinto(win)
```

**This is a proper compile error, not a degrades-to-gcc row** — it is a missing backend feature
rather than a silent miscompile, which is why it is recorded and not fixed here. `jserve`'s read
loop is the shape that wants it.

### §3.1b — A slice of ANOTHER MODULE's struct cannot be cgen-allowlisted

`http_test.jtr` and `http_demo.jtr` take a `[]http.Header`, and the byte-identity golden caught
them. With imports unresolved — which is how that golden runs, by construction — the element
type degrades to `?` and the two sides disagree about whether it still deserves a typedef:

```
reference:  typedef struct { int* ptr; size_t len; } JestyrSlice_?;
port:       (nothing)
```

**Measured, not assumed:** `http.jtr` IS allowlisted and byte-identical, and it declares
`Header` locally and passes `[]Header` through `parse_request`. The same shape agrees exactly
when the element type resolves, so this is a disagreement about how far to degrade an erroneous
program — the same category as `sysfs_test.jtr` and `walk.jtr`'s auto-drop divergence, and it
cannot affect a program that compiles.

**The general rule, now written into the allowlist's comment:** a corpus file taking a slice of
another module's struct cannot be in `CGEN_GOLDEN_ALLOWLIST`. `syswatch_test.jtr`
(`[]syswatch.Change`) is out for the same reason. The MODULE that declares the struct is fine.

That is the third instance of this category, which is what makes it a rule rather than an
exception — and it is worth checking BEFORE adding a file to the allowlist rather than
discovering it from a red ladder.

### §3.2 — Two more keywords met while naming things

`take` and `error` join `read` and `out`. `fn take(...)` does not parse, which cost a
misdiagnosis: a probe written to test the range bug failed on the keyword instead and briefly
looked like a range problem. **When a minimal repro fails in a way that surprises you, check the
identifiers before believing the diagnosis.**

### §3.3 — `-> read str` is the answer to the returned-borrow refusal

`return t[0 .. i]` from a `read str` parameter is refused, and the diagnostic names the fix:
declare the return as `read`. `env.get_or` already does. Worth knowing because the same refusal
appeared three times this session and was worked around twice by restructuring before the
diagnostic was read properly.

---

## §4. WHAT IS LEFT — the queue, ordered by leverage

### §4.1 — A `sys` process module. NOT in the brief, and the biggest hole

`std/process` runs commands through `system()`, which **blocks until the child exits**. That
one fact:

* blocks a long-lived plugin server (`std/plugin` is one-process-per-call because of it),
* blocks **timeouts** anywhere — a hung plugin hangs the host, and `jserve` cannot bound a slow
  client either,
* blocks the loopback-socket design `std/sysnet` would otherwise support, which needs a child
  running CONCURRENTLY with its parent.

Needs `posix_spawn`/`CreateProcess` plus pipes. It unblocks more than anything else on this
list.

### §4.2 — `@must_use` on a non-union return. Cheap, and a degrades-to-gcc row

The error-union half is enforced (v3). A `@must_use` on a NON-union return is still enforced
only by gcc's `warn_unused_result` — the front end **accepts the attribute and never checks
it**, which is a degrades-to-gcc instance sitting in plain sight.

The shape, unchanged: `FnSig` has no attribute field; add `must_use: bool` (two construction
sites, `src/typeck.rs` ~595 and ~624) and check it at the discarded-statement seam (~2880,
beside the existing rule). Owes a port mirror only if emission moves, which it should not.

### §4.3 — §2.1 move-only resources. Owed by SIX handle types now

Every `sys` module makes this worse, and this session added three more:

| type | module |
|---|---|
| `Socket` | `std/sysnet` |
| `Dir` | `std/sysdir` |
| `Reader` / `Writer` | `std/file` |
| `Watcher` | `std/syswatch` |
| `Log` | `std/alog` (holds a `file.Writer`) |
| `Host` | `std/plugin` (holds a `process.Process`) |

All are freely copyable structs with no `Drop` impl, so closing through a copy leaves the other
naming a handle the platform may have reissued. Move-only droppables landed in v3 but only for
`take`/rebinding of a droppable NAME; a struct with no `Drop` is still freely copied. Giving
these a `Drop` is not the fix on its own — it would close them at every scope exit, which is
wrong for a handle that is deliberately passed around.

**This is now the largest correctness debt in the library**, and it is language work.

### §4.4 — §2.4 runtime ownership, §2.7 concurrency with ownership

Untouched. Brief §2.3 (platform errors) and §2.6 (FFI contracts) are closed. **2 of 6.**

### §4.5 — Compiler follow-ups

* ~~**The four `jc_build_matrix` failures**~~ — **CLOSED.** And the recorded description was
  wrong twice over: they were **six** port gaps, not "one def-emission gap plus one inference
  gap", and `try_read` was a **mangle** gap rather than an inference one (typeck agreed with the
  reference on that file all along). See `jestyr-port-monomorphization-handoff.md`. The general
  lesson is in §5.
* **An extern's name vs. the keyword table.** `read`, `take`, `error`, `out` are all keywords, so
  `extern "unistd.h" fn read` does not parse and `std/syswatch` binds `readv` instead. Needs
  either a scoped keyword rule at the extern seam or a declared alias
  (`extern fn sys_read = "read"(…)`). **The alias design would also unwind the three separate
  `close`es** across `std/file`, `std/sysdir` and `std/sysnet`.
* **The `@cfg` specificity rule.** Still the largest, and it now has **three** recorded callers:
  `std/sysdir`'s `D_NAME_OFFSET`, `std/syswatch`'s Linux-only inotify branch, and any future
  macOS work. The increment is NOT "add `linux` to `CFG_WORDS`": `posix` is a SUPERSET of
  `linux` and `macos`, and the vocabulary is a closed list of guards that are disjoint by
  construction, so adding nested names would make `@cfg(posix) fn f` and `@cfg(linux) fn f` both
  emit on Linux. It is a specificity rule about which of two overlapping items wins.
* **A `mut` sub-slice as a call argument** (§3.1) — new this session.

---

## §5. TRAPS

**Re-measure a recorded mechanism before building on it.** Twice this arc the recorded half was
the wrong one: the "one def-emission gap" was six, and `try_read`'s "inference gap" was a mangle
gap. A note is a lead, not a diagnosis.

**A green `jc build` is evidence about EMISSION, not agreement.** A half-mirrored feature —
known to one compiler phase and not another — produces correct output and a divergent internal
state. Only the no-allowlist P3 typeck golden sees it. Grep the port for a feature's name in
EVERY phase.

**BUILD_OK is not correct.** `combinators` reached BUILD_OK while printing 0 for every
combinator, because gcc accepts a non-void function falling off its end.
`jc_built_generics_run_the_same_as_the_reference` runs the binaries and compares bytes.

**A differential harness needs a case whose answer you already know.** The first version of that
comparison reported all six programs as differing, on a line-ending bug in the harness.

**Do not trust a background job that was stopped.** A ladder killed at a session boundary
reported nothing; re-running it caught a real regression (a fixture with a hand-maintained
module list).

**`@no_alloc` is a proven contract, not documentation.** It rejected `std/plugin`'s first wire
format and forced the CRC into a trailer so the checksummed region is contiguous.

**Check a new `pub fn` name tree-wide.** `pub fn ok` in a new module broke `std/file`, which it
has nothing to do with. `bytes` is an intrinsic too.
