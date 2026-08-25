# Tier 4 is done. The next work is LANGUAGE work, and the tier is what proves it is needed.

Cold-start note. Everything below is **on `master` at `49cc171`**; there is no branch to
chase. Work from `C:\Users\adame\Jestyr`.

```bash
cargo build --release && cargo test --release --features "c-oracle,selfhost-fixpoint"
```

**Record the count before changing anything: 1279 passed, 0 failed, 3 ignored.** The 3 ignored
are the deliberate slow numeric sweeps, not breakage. If a failure appears later, assume it is
yours.

**The authoritative progress log is
`docs/session-notes/jestyr-std-v4-tier4-complete-handoff.md` — read §0 and §4 first.** This
file is only the short version.

---

## What is done

**Every item in the Tier 4 brief, §1.1 through §1.11**, each with a real consumer and a test
that checks its claim rather than describing it:

| module | what it is | consumer |
|---|---|---|
| `std/syserr` `std/sysfs` | platform error shape; mkdir/rename-replace/metadata | `jstage`, an atomic publish |
| `std/runtime` `std/syspoll` | timers, cancellation, `Poller` + `watch` | — |
| `std/sysnet` | TCP over IPv4, blocking, capability-gated | `jstatus`, a status server |
| `std/syswatch` | inotify / change notifications, as a `Pollable` | `jwatch`, a debounced rebuild trigger |
| `std/log` | structured records, logfmt **and** JSON from one API | `jlog`, which parses its own output back |
| `std/width` | display width in cells | the caret in `std/diag` |
| `std/alog` | crash-safe append-only log | `jledger`, which crashes itself on purpose |
| `std/plugin` | out-of-process plugins, four-way failure taxonomy | `jhost` + `plugin_echo`, two programs |
| `std/http` | HTTP/1.1 that refuses ambiguity | `jserve`, refusing a real smuggle on loopback |
| `std/tar` | reproducible USTAR archives | `jpack`, which rebuilds and compares |

`jc_build_matrix` is **62 of 62** — the self-hosted compiler builds every multi-module program
in the corpus, and `jc_built_generics_run_the_same_as_the_reference` checks that the binaries
BEHAVE the same rather than merely building.

---

## What to build next

**There is no obvious next module, and that is the finding.** The queue is ordered by leverage,
not by brief number.

### 1. A `sys` process module — NOT in the brief, and the biggest hole

`std/process` runs commands through `system()`, which **blocks until the child exits**. That one
fact:

* forces `std/plugin` to be one-process-per-call — a long-lived plugin server cannot be built,
* makes **timeouts impossible anywhere**: a hung plugin hangs the host, and `jserve` cannot
  bound a slow client either,
* blocks the loopback-socket design `std/sysnet` would otherwise support, which needs a child
  running CONCURRENTLY with its parent.

Needs `posix_spawn`/`CreateProcess` plus pipes. It unblocks more than anything else here.

### 2. `@must_use` on a non-union return — cheap, and a degrades-to-gcc row

The error-union half is enforced. A `@must_use` on a NON-union return is enforced only by gcc's
`warn_unused_result` — **the front end accepts the attribute and never checks it.** `FnSig` has
no attribute field; add `must_use: bool` (two construction sites, `src/typeck.rs` ~595 and
~624) and check it at the discarded-statement seam (~2880, beside the existing rule).

### 3. Move-only resources (brief §2.1) — owed by SIX handle types

`Socket`, `Dir`, `Reader`/`Writer`, `Watcher`, `alog.Log`, `plugin.Host`. All are freely
copyable structs with no `Drop`, so closing through a copy leaves the other naming a handle the
platform may have reissued. **Giving them a `Drop` is not the fix on its own** — it would close
them at every scope exit, which is wrong for a handle deliberately passed around.

This is the largest correctness debt in the library and it gets worse with every `sys` module.
Brief §2.4 and §2.7 are untouched; §2.3 and §2.6 are closed. **2 of 6.**

### 4. The two compiler follow-ups

* **An extern's name vs the keyword table.** `read`, `take`, `error` and `out` are all keywords,
  so `extern "unistd.h" fn read` does not parse (`std/syswatch` binds `readv` instead). Needs a
  scoped keyword rule at the extern seam, or a declared alias
  (`extern fn sys_read = "read"(…)`) — **the alias design would also unwind the three separate
  `close`es** across `std/file`, `std/sysdir` and `std/sysnet`.
* **The `@cfg` specificity rule**, the largest, with **three** waiting callers now
  (`sysdir`'s `D_NAME_OFFSET`, `syswatch`'s Linux-only inotify branch, any macOS work). It is
  **NOT** "add `linux` to `CFG_WORDS`": `posix` is a SUPERSET of `linux` and `macos`, and the
  vocabulary is a closed list of guards disjoint by construction — so nested names would make
  `@cfg(posix) fn f` and `@cfg(linux) fn f` BOTH emit on Linux. It is a specificity rule about
  which of two overlapping items wins.

---

## The traps that will otherwise cost you an hour

**A green `jc build` is evidence about EMISSION, not agreement.** A half-mirrored feature —
known to one compiler phase and not another — produces correct output and a divergent internal
state, and only the no-allowlist P3 typeck golden sees it. That is exactly how the port's
`@size_of` gap survived: `cgen.jtr` lowered it, `typeck.jtr` typed it `?`. **Grep the port for a
feature's name in EVERY phase**, not just the one you changed.

**BUILD_OK is not correct.** `combinators` once reached BUILD_OK while printing 0 for every
combinator, because the port emitted an empty body and gcc accepts a non-void function falling
off its end. And **a differential harness needs a case whose answer you already know** — the
first version of the run-and-compare test reported all six programs as differing, on a
line-ending bug in the harness itself.

**A slice of ANOTHER module's struct cannot go in `CGEN_GOLDEN_ALLOWLIST`.** That golden runs
with imports unresolved by construction, so `[]mod.T` degrades to `[]?` and the two sides
disagree about the typedef. Third instance of the degrade-an-erroneous-program category; the
MODULE that declares the struct is fine. Check before adding, not from a red ladder.

**The two cgen goldens see different halves of a suite file** — `@test` bodies are not emitted
in non-test mode, so a divergence inside a `@test` is invisible to
`jestyr_cgen_matches_reference` and only the test-mode golden catches it.

**Two modules cannot bind the same C symbol.** `duplicate definition of fopen` killed
`std/alog`'s first design outright; durability moved into `std/file`, where it belonged anyway.

**A NEW module's `pub fn` can break an OLD one.** `pub fn ok` shadowed the Result intrinsic and
broke `std/file`, a module it has nothing to do with. `bytes` is an intrinsic too. Check a
proposed public name tree-wide: `grep -rn "fn <name>" examples/std/`.

**`@no_alloc` is a proven contract, not documentation.** It rejected `std/plugin`'s first wire
format and forced the CRC into a trailer so the checksummed region would be contiguous. If it
refuses a function, ask whether the DESIGN can avoid allocating before dropping the attribute.

**`.jtr` shapes that do not exist:** a `mut` sub-slice as a call argument (`f(buf[a..b])` where
the param is `mut []u8` — the `read` form and a `let` initializer both work, so hoist into a
local); returning a borrow (`return t[0..i]` — declare the return `-> read str`); an empty
`catch { }`; a match ARM BODY that is a statement; a module-qualified struct LITERAL; `\u{...}`
escapes. And `read`, `take`, `error`, `out` are keywords.

**Re-measure a recorded mechanism before building on it.** Twice this arc the recorded half was
the wrong one: the "one def-emission gap" was six, and `try_read`'s "inference gap" was a mangle
gap. A note is a lead, not a diagnosis.

**Do not trust a background job that was stopped.** A ladder killed at a session boundary
reported nothing; re-running it caught a real regression.

---

## The two-sided tax, every time

A change to `examples/std/cgen.jtr` or to emission in `src/cgen.rs` owes:

1. the mirror on the other side,
2. `REFRESH_SEED=1 cargo test --release --features "c-oracle,selfhost-fixpoint" -- bootstrap_seed_is_current`,
3. `JC_BUILD_MATRIX=1 …` if the set of buildable programs moved,
4. the full ladder.

**A closure-module change also makes `selfhost_fixpoint_full` and `jestyr_driver_builds_itself`
non-optional** — the closure is 15 modules now (`width` joined it when `diag` imported it), seed
~42.5K lines.

A new `@cfg`-bearing file **must** join `CGEN_GOLDEN_ALLOWLIST` or
`every_cfg_bearing_corpus_file_is_byte_identity_verified` fails on purpose.
