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

The brief's **§1.1 (`sys/fs`) and §2.3 (platform error shape) are done**, with a real
consumer. **`JestyrResult_unit` lowering is done too** — a fallible function may now have no
return type at all, which is the natural signature for most of what a `sys` layer does.
Five compiler bugs were found and closed on the way — one a silent miscompile — and the
gate the 2026-08-20 census audit said it owed now exists.

(Section numbers below are this note's own; the brief's are named as "the brief's §x".)

**The next thing to build is the brief's §1.2, the event loop.** `std/runtime` already has the
half that does not need the OS — an explicit `Runtime` handle, timers, cancellation, a
manual clock, `poll`/`run_until_idle`. What it lacks is `Pollable`, and the reason it was
left out is now *less* true than it was: `sys` is a real tier with a real error shape, so
an `epoll`/`kqueue`/IOCP binding has somewhere to live and something to report with. Read
`std/runtime.jtr`'s header before designing anything; the ownership bug it records (a
`Runtime` takes its clock by `take`, so the caller's copy is not the one that advances) is
the shape of mistake this area produces.

### What is NOT what an older note says

* **The `jc build` name-collision bug is FIXED**, and the mechanism recorded for it was
  backwards. See §2.2. The census note's "9 of the 52 runnable multi-module corpus
  programs fail" is now **4 of 53**, and those four are one isolated mechanism.
* **`sys` at Tier 4 builds under `jc`.** `examples/std/sysfs_demo.jtr` — which imports
  `sysfs`, `syserr`, `sysdir`, `file`, `fs`, `env`, `mem`, `str`, `path`, `sink` — compiles
  through the self-hosted compiler's own loader and gcc driver, and prints exactly what the
  reference toolchain's build prints.

---

## §1. STD TIER 4 — `std/syserr` and `std/sysfs`

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
| 1.3 | event loop V1 | ⬜ `std/runtime` has everything but `Pollable` — **start here** |
| 1.4 | TCP sockets | ⬜ needs the event loop first |
| 1.5 | file watching | ⬜ |
| 1.6 | structured logging | ⬜ |
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
2. **Event loop V1 `Pollable`** (§1.3) — `epoll`/`kqueue`/IOCP behind `@cfg`, reporting
   through `syserr`. The two things that were missing when `std/runtime` deferred it now
   exist.
3. **TCP sockets** (§1.4), then a local status server as the consumer.
4. **`@must_use` enforcement** (§2.2) — cheap, and it is a degrades-to-gcc row.
5. **The four `jc_build_matrix` failures** — one def-emission gap plus one inference gap,
   both now isolated.
6. Watching, logging, HTTP, tar, Unicode width, append-log, plugins.
