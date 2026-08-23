# Std v4 continuation — file watching, and the machinery that is already there

Cold-start note. Everything below is **on `master` at `e236001`**; there is no branch to
chase. Work from `C:\Users\adame\Jestyr`.

```bash
cargo build --release && cargo test --release --features "c-oracle,selfhost-fixpoint"
```

**Record the count before changing anything: 1272 passed, 0 failed, 3 ignored.** The 3
ignored are the deliberate slow numeric sweeps, not breakage. If a failure appears later,
assume it is yours.

**The authoritative progress log is `docs/session-notes/jestyr-std-v4-runtime-platform-handoff.md`
— read §0 first.** This file is only the short version.

---

## What is done

The Tier 4 brief's **§1.1 (`sys/fs`), §1.2 (event loop V1), §1.3 (TCP sockets) and §2.3
(platform error shape)**, each with a real consumer:

| module | what it is | consumer |
|---|---|---|
| `std/syserr` | platform error code + portable category | every `sys` operation |
| `std/sysfs` | mkdir/rmdir/rename-replace/metadata/canonical/temp-dir | `sysfs_demo` = `jstage`, an atomic publish |
| `std/runtime` | timers, cancellation, **`Poller` + `watch` + `RT_READY`** | — |
| `std/sysnet` | TCP over IPv4, blocking, capability-gated | `sysnet_demo` = `jstatus`, a status server |
| `std/syspoll` | `poll(2)` / `WSAPoll` readiness | the `Poller` implementation |

Plus **`JestyrResult_unit` lowering**: `fn f(x: i32) !{ E }` with no return type now works —
`?`, `catch`, a valueless `return`, and falling off the end (which means SUCCESS). That is
the natural signature for most `sys` operations and it is why `sysfs.rename_replace` no
longer returns a racy bool.

**51 of 55 multi-module corpus programs build under the self-hosted `jc`**, gated by
`docs/jc_build_matrix.txt` (an expectations file that fails in BOTH directions).

---

## What to build next

**The brief's §1.4, file watching.** It is the natural next consumer of exactly the
machinery that just landed — a watcher is a `Pollable` over a platform notification handle
(`inotify` on Linux, `ReadDirectoryChangesW` on Windows) — and everything it needs from the
loop already exists: registration, level-triggered firing, group cancellation, and an idle
rule that knows a watch keeps the loop alive.

**§1.5 (structured logging) is the other cheap one** and adds no new platform surface at all,
if you would rather do a no-OS module first.

**Before writing either, read `std/runtime.jtr`'s header on `Poller`.** The loop reaches the
operating system **only** through the two handles it is given (a `Clock` and a `Poller`), and
keeping that true is what makes the whole thing testable with no OS underneath. A four-line
scripted poller in `syspoll_test.jtr` drives registration, firing, level-triggering,
cancellation and the idle rule with no sockets at all — copy that shape.

---

## The traps that will otherwise cost you an hour

**An `extern`'s name is a C SYMBOL and is globally reserved** against any Jestyr function
declared in **exactly one** module. Two or more modules declaring it → already canonicalised
→ the bare name is free (`close` is fine, because `std/file` and `std/sysdir` both declare
one). Declared by exactly one → genuine collision, and `jestyrc` refuses the program. That is
why `runtime.poll` is now **`runtime.fire_due`**. Check before binding: `grep -rn "pub fn <name>" examples/std/`.

**Two `@cfg` items sharing a name must share a SIGNATURE.** typeck keys its function table on
the bare name, so the second declaration wins and BOTH branches get checked against ONE
signature. Unify the signatures — these are header-declared externs, so the Jestyr signature
only has to describe something C can implicitly convert from (`i64` covers an `int` fd and a
64-bit `SOCKET`). Recorded, not fixed; a diagnostic naming the mismatch is a good small
increment.

**Windows link/compile flags are content-triggered and ORDER-SENSITIVE.** A `-l` library must
come AFTER the source file or GNU ld resolves nothing and the link fails exactly as if the
flag were missing. There is one `link_and_finish` helper in `src/proptests.rs`; the driver
sites are in `src/main.rs` and `examples/std/cgen.jtr`. If a new module needs a library,
all three need it.

**`.jtr` shapes that do not exist:** an empty `catch { }` (a fallback must be an expression —
use a named `fn ignore() { }`); a match ARM BODY that is a statement (use a helper that
records into a `mut` slot and returns the ok type); a module-qualified struct LITERAL
(`sysnet.Socket{ … }` — export a constructor); `out` as a variable name (it is a keyword). A
bare function name in a struct literal emits a LOCAL's spelling — write `&name`.

**Reading a foreign struct by byte offset: know which case you are in.** `sockaddr_in` is
fine (wire-adjacent, fixed since 4.2BSD, Linux == Windows byte for byte). `struct stat` is
not (`st_mode` is 24 on Linux x86-64, 16 on aarch64, 4 on macOS — an ARCHITECTURE difference
no CI runner covers). When you do hard-code an offset, assert it in a test that goes RED on
the platform where it is wrong.

---

## The two-sided tax, every time

A change to `examples/std/cgen.jtr` or to emission in `src/cgen.rs` owes:

1. the mirror on the other side,
2. `REFRESH_SEED=1 cargo test --release --features "c-oracle,selfhost-fixpoint" -- bootstrap_seed_is_current`,
3. `JC_BUILD_MATRIX=1 …` if the set of buildable programs moved,
4. the full ladder.

A new `@cfg`-bearing file **must** join `CGEN_GOLDEN_ALLOWLIST` or
`every_cfg_bearing_corpus_file_is_byte_identity_verified` fails on purpose.

**The two cgen goldens see different halves of a suite file**: `@test` bodies are not emitted
in non-test mode, so a divergence inside a `@test` is invisible to
`jestyr_cgen_matches_reference` and only `jestyr_cgen_test_mode_matches_reference` catches it.

**Write new modules against the PORT as well as the reference.** Every new module this
session found one more shape the corpus had never exercised and the port had therefore been
quietly wrong about (the `?` temp order, the `Unit` mangle, the extern rename). `jc <file> build`
early, not just at the end.

---

## Housekeeping

`master` is **4 commits ahead of `origin/master`** and has not been pushed. Push when you are
ready; nothing else is outstanding.
