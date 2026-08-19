# Std v3 — structured diagnostics, the error-model rules it forced, and `@cfg`

Cold-start note. **What was built and what it measured (§1), what the build turned up in
the compiler (§2), what is still open (§3), traps (§4), suggested order (§5).**

Branch `claude/std-v3-systems-language-6d8931`. Baseline before anything changed:

```bash
cargo build --release && cargo test --release --features "c-oracle,selfhost-fixpoint"
```

**1228 passed, 0 failed, 3 ignored.** After this work: **1252 passed, 0 failed, 3 ignored**
(+1 module-boundary driver test, +6 must-use, +7 fallible-return, +10 `@cfg`; `io_suites_pass` grew a
`diag_test` entry rather than a new test).

---

## §1. `std/diag` — Tier 3 §1.1, built

`examples/std/diag.jtr` + `diag_test.jtr` (15 cases) + `diag_demo.jtr`. Allowlisted,
byte-identical against the self-hosted backend first try, and green through `jc`'s own
loader and gcc driver.

### What it is

`SourceMap` / `FileId` / `Span` / `Pos` / line-column mapping / `Diagnostic` / `Label` /
`Severity` / `LabelKind` / `Theme`, a caret renderer, and a one-line `render_brief`.

```
error[E0042]: no field `z` on struct `Point`
  --> area.jtr:5:11
   |
 4 |     let p: Point = Point{ x: 1, y: 2 }
 5 |     return p.z
   |              ^ unknown field
   |              - did you mean `y`?
  --> shapes.jtr:1:12
   |
 1 | pub struct Point {
   |            ----- `Point` is declared here
   |
   = note: `Point` has fields `x` and `y`
```

### The consumer is real, and it is the one the driver already needs

`diag_demo.jtr` runs the **ported parser** over a real `.jtr` file, scans `p.ex` for Error
expressions (kind 9) and `p.it` for Error items (kind 99) — the identical two scans
`cgen.jtr`'s `driver_build` performs — and renders them with source, caret and note. That
is exactly the upgrade `cgen.jtr`'s own comment records as a follow-up. The driver today
prints `path:line:col: error: syntax error (unexpected or malformed input)` and stops at
the first; the demo renders the first in full and the cascade as brief lines, which is
strictly more information for the same screenful.

**Wiring `std/diag` INTO `cgen.jtr`'s driver is the obvious next increment and was
deliberately not done here.** `cgen.jtr` is a closure module, so it costs a reseed, and the
driver's stderr text is what several goldens compare. Do it as its own increment with the
gate re-run, not as a rider on the library.

### Four design decisions, with the reasoning that survives review

**1. No `Writer` parameter, and no error set — the spec asked for both and both are wrong.**
The brief specified `render_diagnostic(read writer: file.Writer, …) -> usize !{ DiagnosticWriteFailed }`.
`sink` writes are infallible at the call site *by design* (overflow is counted, not
returned) and `file.Writer`'s one fallible call is `finish`, so a renderer between them has
nothing to fail at: the error set would be `writer.jtr`'s deleted `failed()` in a new
costume — a query that can only ever answer "fine". And `writer.Writer`'s stream targets
are line-oriented and already carry a `Sink`, so a writer-taking renderer needs *two*
sinks. `render` therefore writes into a caller `Sink` and returns a byte count; the
destination stays the caller's and composes with stderr, a file, and the trait.

**2. Two arenas, no per-label ownership.** A `SourceMap` is every file's text concatenated
into one `String`, every name into another, and four `i32`s per file in a `List(i32)` — the
same shape `cgen.jtr`'s `Ml.mods` already uses. A `Diagnostic` is one `String` arena with
each `Label` slicing into it. Not cleverness: a borrow is second-class so a `read str`
cannot be stored, and a `List(String)` would **leak**, because B1's field auto-drop recurses
into droppable fields and `String` is a primitive with a manual `string_free` — the trap
`std/pathbuf` paid for.

**3. `FileId` is a `distinct usize`, and the bill was measured.** A file index and a byte
offset are both `usize` and never interchangeable. **11 casts inside the module, 0 in the
demo.** That distribution is the opposite of the one that shelved typed `Path` (111 of 132
on callers), which is what makes it worth having — and it is the first real consumer of the
`distinct` operation-inheritance work: a `FileId` still compares with `==` and still
arithmetics with another `FileId`.

**4. One `Label` record, four concepts.** Primary / secondary / note / suggestion differ in
exactly two bits — does it point at source, and which character underlines it. Three structs
agreeing on all their fields would be three things to walk in the right interleaving to keep
the caller's order; one record with a `LabelKind` keeps the order for free and keeps
`render` a single loop.

### What the tests pin, and why each earns its place

* **The overflow contract.** `an_unguarded_render_into_a_small_buffer_truncates_mid_diagnostic`
  performs the unguarded render *on purpose*. Every number it asserts is TRUE and the output
  is still ruined: `render` reports 20 bytes, it wrote 20 bytes, and those 20 bytes are a
  header cut in half. Only `sink.overflowed` distinguishes it — so the check is in the
  module's documented recipe, not a footnote. Paired with the positive control that a
  big-enough buffer does not overflow.
* **`a_span_naming_an_unknown_file_is_skipped_not_fatal`**, with its positive control. Losing
  the whole report because one label is unresolvable is the worst failure mode a diagnostic
  renderer has.
* **`caret_alignment_is_byte_based_and_tabs_expand`** — the pin to invert when a display-width
  function exists. Columns are BYTE columns, matching what `--json` already reports; a
  renderer that disagreed with the JSON report about where a diagnostic points would be worse
  than one that is uniformly byte-based. Tabs are the exception and expand in *both* the
  source row and the caret row, computed from the one string so they cannot disagree.
* **`labels_are_grouped_by_file_not_by_call_order`** — a regression pin for a bug this build
  had. Rendering strictly in label order printed `a.jtr`'s source line twice under two `-->`
  headers, which reads as two different places in the file.
* **`jestyr_driver_diag_across_the_module_boundary`** — the gate the corpus golden cannot be.
  `jestyr_cgen_matches_reference` compiles every file with **no import resolution**, so a
  single-file `diag.jtr` never instantiates `List(Label)` at all. This one goes through `jc`'s
  real loader, asserts `Jestyr_List__Label` is present and no `_T` instance survived, asserts
  `typedef size_t Jestyr_FileId` (a `distinct` degrading to `int` would index a file table
  with 32 bits and still compile), and — the claim byte-equality cannot make — that the
  port-built binary renders the same diagnostic character for character.

---

## §2. Two compiler rules, both from §2.2/§2.3, both reference-only

### §2.1 — A discarded fallible result is refused (§2.2 of the brief, DONE)

`file.finish(w)` as a bare statement used to compile and run with **no diagnostic at all** —
`std/file`'s header said so, and named this as "the actual fix" rather than polish.

**Measured before choosing the severity, over all 208 corpus files: FOUR sites, every one of
them `file.finish(…)` in `file_test.jtr`, zero false positives elsewhere.** That is what
justifies an error rather than a warning. The rule is also *structurally* incapable of firing
on handled code: `e?` and `e catch v` both unwrap to the ok type before statement position
sees them, so the two spellings that handle the error are unreachable by it. A block's
trailing expression is skipped — in a fallible body that is the implicit return and discards
nothing.

The four sites now **assert the verdict** (`file.finish(big) catch 0` compared against the
expected byte count) rather than dropping it, which is strictly more coverage: a setup whose
write silently failed used to make the real assertion fail for the wrong reason.

**Half of `std/file`'s language ask is now closed and the halves are worth keeping apart:**
discarding the verdict is a compile error; **not calling `finish` at all is still silent**,
because that is a linear obligation on the handle at scope exit, not a rule about one
expression. The module header now says exactly that instead of the stale claim.

### §2.2 — A `return` in a fallible function must be Result-typed (§2.3 class, DONE)

Found while probing the must-use escape hatches. `cgen` emits `return <value>` verbatim, so a
bare ok value out of a `-> T !E` produced C assigning an `int64_t` to a `JestyrResult_i64`:
`jestyrc check` passed and **gcc** refused. typeck deliberately compared against the *ok*
type; cgen never implemented the sugar that comparison implies.

**The boundary was probed, not reasoned about, and it is not where it looks:**

| form | verdict |
|---|---|
| `return ok(v)` | ✅ legal |
| `return err(E)` | ✅ legal |
| `return other_fallible(x)` | ✅ **legal** — forwarding a whole result |
| `return f(x)?` | ❌ unwraps to `T`, emitted as a bare value |
| `return f(x) catch v` | ❌ same |

So one condition covers all five: the returned expression is a `Result`. A rule demanding a
literal `ok(…)`/`err(…)` would have refused working code. **Zero corpus hits** — no edits
needed anywhere.

### Why neither owes a port mirror or a reseed

Both add diagnostics and change **no emitted byte**. The port has no assignability check at
all either — the int→int rule set that precedent — and neither rule creates an Error *type*,
so `jc` stays permissive where `jestyrc` refuses. That is the checker being ahead of the
bootstrap, not a divergence in what the two backends emit. Verified: `jestyr_cgen_matches_reference`
green, `selfhost_fixpoint_full` green, seed unchanged.

---

## §3. OPEN — with what is known

### §3.1 — `string_view(x).len` emits invalid C. NOT FIXED, fixture below

Recorded in the previous handoff as a `.jtr` **subset trap for closure modules**. It is not:
it is a bug in the **reference** compiler, and it bites any module holding a `String`.

```jtr
fn main() -> i32 {
    var s: String = string_new()
    string_push(s, "abc")
    print_int(string_view(s).len as i64)     // check: ok.  gcc: no member named 'j_len'
    return 0
}
```

A `.len` on a `str` whose base is a **call** rather than a name emits `j_len` — the field is
being resolved as a user struct field instead of the builtin. `std/diag` works around it by
binding `let v: str = string_view(x)` first, four times.

Fixing it is an **emission** change, so it owes a port mirror in `cgen.jtr` plus a reseed —
but **zero golden churn**, because no corpus file uses the shape today (they all avoid the
documented trap). That makes it a cheap, well-bounded increment, and it removes the workaround
from `diag.jtr` and from the closure modules' subset rules.

### §3.2 — A module-qualified struct LITERAL does not parse

`diag.Theme{ gutter: false, … }` from another file is a parse error, though `diag.Theme` in
**type** position is fine. So a `pub struct` a caller is expected to build needs an exported
constructor or it is effectively read-only outside its module — `diag.theme(…)` exists for
exactly this reason. Worth knowing before designing any library type in this tree; found by
writing the test that varies the theme, not by reading the grammar.

### §3.-1 — Tier 3 §1.2/§1.3/§1.4 — three modules, and where §1.4 stops

`std/cli` (11 tests), `std/buildgraph` (10), `std/sysdir` (5), each with a real consumer.
`cli_demo` is `jlint` — parses a file with the ported parser and reports through
`std/diag`; `buildgraph_demo` is `jplan` — orders the manifest `Modules::render_manifest`
actually emits, verified against a real one by
`jplan_orders_a_manifest_the_compiler_rendered`.

**`std/sysdir` is the first `sys`-tier module, and it lists a real directory.** The thing
recorded as "BLOCKED, and not on what anyone expected" now works: `examples/modules` reads
back as exactly `main.jtr` and `mathx.jtr`, `.`/`..` filtered, a missing directory giving a
closed handle, an interior NUL refused with the prefix-opens positive control beside it.

Two things that made it possible and were not obvious:

* **`cptr` narrowing.** POSIX `readdir` returns a `struct dirent*` whose name must be read
  out of it — impossible through an opaque handle. An explicit `e as *mut u8` IS accepted
  (only the implicit direction is refused), and the explicitness is right: the cast is a
  claim about a foreign struct's layout and should look like one. Windows needs no such
  cast, because the OS writes into a buffer Jestyr owns.
* **`d_name`'s offset is the one number that is not portable within POSIX** — 19 on
  glibc/musl LP64, 21 on macOS. It is a plain `const` (a constant needs no guard; it is
  merely unused on Windows) and it is *asserted*, not assumed:
  `every_name_is_nonempty_and_terminated` fails on a platform where 19 is wrong, because a
  wrong offset yields plausible garbage rather than an error. **This is the trigger the
  `@cfg` vocabulary was left closed for** — `linux`/`macos` become worth adding here — and
  they are deliberately NOT added, because neither machine in this session can run the
  branch and an untested branch is the failure this module's header argues against.

**§1.4 IS NOT FINISHED.** `sysdir` is the platform half. The brief's actual ask —
`walk(fs, root, opts, visitor)` with deterministic order, ignore/glob, and a capability —
is unbuilt, and it is `std`, not `sys`. What it needs, precisely:

* **Sorting, because `sysdir` refuses to.** Neither `readdir` nor `FindNextFileA` promises
  an order and NTFS and ext4 genuinely differ, so the module returns OS order and says so.
  Determinism is `walk`'s job, which is where it is actually required.
* **A visitor that is a fn pointer**, not a closure — the `mem.Allocator` shape.
* **`Fs` as the capability**, so a walk can be denied and tested denied, with the positive
  control through `host()` that a refusal test needs to mean anything.

`sysdir.jtr`/`sysdir_test.jtr` are in `io_suites_pass` and deliberately **not** in
`CGEN_GOLDEN_ALLOWLIST`: they use `@cfg`, so byte-identity against the self-hosted backend
is owed together with §3.0's port mirror.

### §3.0 — `@cfg(<platform>)` — BUILT, reference-side. The port mirror is owed

The `sys` blocker recorded as "Jestyr has NO conditional-compilation mechanism at any
level" is closed on the reference toolchain. `examples/cfg_platform.jtr` binds the actual
divergent family — POSIX `opendir`/`readdir`/`closedir` against Windows
`FindFirstFileA`/`FindClose` — and runs.

**The design was forced, not chosen.** `attest` hashes the emitted C and "same source →
byte-identical C" is the invariant that hash commits to. A `cfg` that dropped items before
codegen would make emission a function of the HOST, so the same source would attest
differently on Linux and Windows and the cross-OS canary would go with it. So **`@cfg`
selects at the C preprocessor, not in codegen**: every guarded item is emitted, wrapped in
`#if defined(_WIN32)` / `#if !defined(_WIN32)`, and `cc` keeps the half that applies.

Two consequences, both improvements on a dropping `cfg`:

* **Both platforms are always checked** — a type error or an escape violation in the
  inactive branch is caught on either host. Pinned by `the_inactive_branch_is_still_checked`.
  (The first probe for this used an unknown bare NAME and passed, proving nothing —
  unknown bare names are not an error for any function here. Fix the probe, not the claim.)
* **Two items may share a name when their platforms are disjoint.** Same-platform
  duplicates and unguarded-vs-guarded still collide, each with its own control test.

Five emission sites carry the guard: the `#include` for a header-declared extern (which
**must** be guarded — `<dirent.h>` does not exist on Windows, so an unguarded include fails
before the guarded prototype is reached), `extern "c"` prototypes, non-generic fn
prototypes and definitions, and monomorphized instances. A header named by mixed platforms
falls back to unconditional.

`ExternFn` gained an `attrs` field: extern attributes were parsed, validated against
`Target::Extern`, and then **discarded**, which was invisible while no attribute meant
anything on an extern.

**What is owed, and why it could land anyway.** The port does not understand `@cfg`, so
`cgen.jtr` needs the mirror and the seed needs refreshing. It lands green regardless
because **no corpus file uses `@cfg`**, so no existing emitted byte changes and
`jestyr_cgen_matches_reference` is untouched — verified. `cfg_platform.jtr` is deliberately
NOT in `CGEN_GOLDEN_ALLOWLIST`, and `cfg_is_not_yet_in_the_byte_identity_allowlist` fails
the moment someone adds it without the mirror. **`sys` is therefore unblocked for `jestyrc`
and not yet for `jc`** — build the mirror before writing `sys` itself, or `sys` will be the
first module the self-hosted compiler cannot build.

The vocabulary is `posix` and `windows`, closed on purpose, with `cfg_guard` total over it
and an anti-vacuity test tying the two together. `linux`/`macos` stay out until something
needs to tell them apart.

### §3.3 — Not attempted, and why

| item | why not now |
|---|---|
| Wiring `std/diag` into `cgen.jtr`'s driver | Closure module → reseed, and the driver's stderr text is golden-compared. Own increment, own gate run. See §1 |
| A machine (JSON) renderer | The brief defers it "after serialization exists", and `docs/diagnostics-json.md` already specifies the shape the reference emits. Build the codec first, then make `diag` render into it — not a second hand-written escaper |
| Multi-line span underlining | v1 underlines the first line only and says so. The full form needs rustc's `/ | \` gutter, a second rendering mode; a v1 that ran a caret across a line break would put it in the *wrong* place rather than a simplified one |
| Colour | `Theme` has the seam (`unicode`); colour needs a terminal-capability question that belongs with the CLI kit (§1.2 of the brief), not here |
| Un-`finish`ed `Writer` at scope exit | A linear obligation on a handle, not a rule about an expression — the `@must_use`/move-only work in §2.1 of the brief. The expression half is done; this is the other half |

---

## §4. TRAPS found this session

* **An explicit `impl Drop` does NOT replace the field auto-drop — it runs BEFORE it.**
  Measured in the emitted C: `Drop__SourceMap__drop` (the two `String`s) then
  `Drop__List_i32___drop` on the `ix` field. So an impl covering only the manual-free
  primitives is right, and adding `list.free` to it would be a **double free**. The `pathbuf`
  note recorded the leak half of this and left the composition half unstated.
* **The default parameter convention is a second-class `read` borrow, even for plain data.**
  A 24-byte `Span` could not be stored in a struct. Two fixes: `take` at every call site
  (viral) or `@copy` on the aggregate. `@copy` is right for handles and value types with no
  droppable payload — `Allocator` already carries it. `Span`, `Pos`, `Label`, `Theme`,
  `Severity` and `LabelKind` are all `@copy` here.
* **Enums have no `Eq`.** Every kind question goes through a `match`. That is a feature: an
  exhaustive `match` makes adding a fifth variant a compile error at each site that has to
  decide, where a chain of `==` would silently take the else branch.
* **`error` is a keyword.** `pub fn error(…)` is a parse error with six cascading
  diagnostics; the constructors are `new_error`/`new_warning`.
* **Array range-slicing is still refused** (`buf[0 .. 64]` on a `[64]u8`), which is pinned
  deliberately. Use `alloc` + `slice`, as `sink_test.jtr` does.
* **A theme field named for what it shows must be able to show nothing.** `gutter: false`
  first zeroed the column width and kept printing the number, giving `2 | two` — a gutter
  with no column rather than no gutter. Only the test that varied the theme caught it.
* **Compute spans in a demo, do not hand-count them.** The showcase's first version used a
  literal offset that pointed into the wrong line; `str.index_of` makes it self-correcting.
  A caret pointing at the wrong token is precisely the failure this module exists to prevent.

---

## §5. Suggested order from here

1. **The `@cfg` port mirror** (§3.0) — `cgen.jtr` + reseed. Do it BEFORE writing `sys`, or `sys` is the first module `jc` cannot build.
2. **`string_view(x).len`** (§3.1) — smallest fix with a fixture already written, and it
   retires a workaround in three places.
3. **Wire `std/diag` into `cgen.jtr`'s driver** (§1) — the consumer that makes the module
   load-bearing rather than available. Reseed + golden run; budget it as its own increment.
4. **Must-use for handles** (brief §2.1) — the un-`finish`ed `Writer` half. This is the
   move-only/linear work, and `file.Writer` is the canary with the argument already written
   down in its header.
5. **CLI app kit** (brief §1.2), then the formatter (brief §5 item 4), which is what stresses
   parser spans against `std/diag` before LSP complexity.

Leave the JSON renderer alone until a codec exists; a second hand-written escaper is the
thing `docs/diagnostics-json.md` already warns is only defensible once.
