# Changelog

All notable changes to Jestyr. This project is pre-1.0 research software;
versions are snapshots, not stability promises.

## Unreleased

### Added

- **`census` — a source-tree census, and the Tier 3 standard library's showcase.**
  `examples/std/census.jtr` (the tally), `census_cli.jtr` (the tool), `census_test.jtr`
  (9 cases). Files, bytes, lines and the text/binary split per extension, plus the largest
  file and how much of the 256-value byte space the tree's text actually uses.

  ```
  census scan <dir> [--json] [--hidden] [--depth N] [--profile] [--sandboxed]
  ```

  Seven Tier 3 modules, each doing the job it exists for: `cli` (spec + parse; an unknown
  option is an error, not a silent positional), `walk` + `sysdir` (sorted, so two runs over
  an unchanged tree are byte-identical and `--json` is diffable in CI), `fs` (the
  capability — `--sandboxed` hands the walk `fs.denied()` and reports the refusals), `diag`
  (usage errors with a caret under the offending argument, rendered over the command line
  itself as the "source file"), `json`, `bitset` (the 256-bit byte-value set) and `memprof`
  (`--profile`).

  **The split that makes it testable:** `observe` never touches the filesystem — the caller
  supplies the path and the bytes. So every awkward case is a string literal in the suite
  (no trailing newline, one NUL byte, two dots in a name, an empty file), and the whole
  tally half is `@no_os` — checked rather than asserted. Both renderers are
  `@no_alloc @no_os`.

  **Verified against an independent recount** (`census_demo_matches_an_independent_recount`):
  the fixture is counted again in Rust from the rules re-derived rather than ported, then
  compared — plus determinism across runs, table-vs-JSON agreement, the capability refusal
  with its positive control beside it, a clean `--profile`, and the caret diagnostic's exit
  code and clean stdout.

  Three bugs that loop caught which the output did not show: the visitor counted directories
  as zero-byte files (a perfectly plausible table — only `find | wc -l` disagreed);
  `--profile` reported `live=288` on a run that leaks nothing (the profile was printed from
  inside the scan, before the census's own arenas dropped, so the measurement was in the
  wrong place rather than the memory); and nine test expectations were wrong while the
  library was right every time.

- **The self-hosted driver renders through `std/diag`.** `jc <file> build|run` now prints the
  same caret block the reference does — header, `--> file:line:col`, the offending source
  line, an underline — instead of one `path:line:col: error: <msg>` line, and reports every
  diagnostic in a stage rather than stopping at the first, with the reference's trailing
  `N error(s)`.

  **The reason this was worth doing is not the carets.** The loader already held a source
  map and nothing knew it: `Ml.nb` is every loaded module's name concatenated, `Ml.allsrc`
  every module's source, and `Ml.mods` six offsets per module — a `diag.SourceMap` with
  different field names. Handing those ranges to `diag.add_file` once *deleted* the driver's
  hand-rolled line/column counter, which was a second implementation of `diag.pos_of` and
  the kind of duplicate that drifts silently because nothing compares them. Adding a file per
  module in loader order also means a module index **is** its `FileId`, so no side table.

  **A span end had been computed and discarded for the life of the driver.**
  `escape.Esc.dsp` has always held `(start, end)` pairs; `eprint_diag` took only the start,
  so it could not have drawn anything but a one-column caret. The `^^^…` runs now matching
  the reference are not new information — they are information that stopped being thrown
  away. The golden asserts the multi-column run for exactly that reason.

  **Abbreviation is per stage, because only some diagnostics cascade.** Parse recovery
  cascades (one missing operand yields four Error nodes, since recovery resumes
  mid-expression) and so does an Error type, which propagates into every enclosing
  expression — those get `diag_demo.jtr`'s policy of one caret block then one line each,
  which is the whole reason `render_brief` exists. Escape diagnostics do **not** cascade:
  each is an independent rule violation at an independent site, so all of them render in
  full, as the reference does. A blanket policy would have silently abbreviated real
  findings.

  No error codes on the port's diagnostics. The reference attaches one (`error[E0007]:
  expected an expression, found `)``) because its parser knows which rule failed; these
  messages are generic v1 text recovered from an Error node, so a code here would either
  collide with a reference code under a different meaning or invent a parallel numbering.

  **Cost, named rather than absorbed:** `diag` and its `sink` dependency join the
  self-hosting closure, so `SELFHOST_MODULES` is fourteen modules and the bootstrap seed
  grows 40,715 → 41,886 lines of C. That is what "load-bearing" means here — `std/diag` is
  no longer merely available to programs the compiler compiles, it is part of the compiler.
  The closure absorbed it without incident: the concat build, the `jc2 ≡ jc1` fixed point and
  the self-build through `jc`'s own loader are all green.

- **`@cfg` in the self-hosted back end — `sys` now builds under `jc`.** The Jestyr-written
  `cgen.jtr` emits the same `#if defined(_WIN32)` / `#if !defined(_WIN32)` guards the Rust
  reference does, at all five sites: the header `#include`, the `extern "c"` prototype, the
  non-generic fn prototype and definition, and the monomorphized instance. `cfg_platform.jtr`
  and `sysdir.jtr` join `CGEN_GOLDEN_ALLOWLIST` and are byte-identity verified in dump and
  test mode; the bootstrap seed is refreshed. Verified end-to-end on a multi-module driver:
  `jc` loads `sysdir`'s import closure itself, emits **both** platform branches, drives gcc,
  and the program lists a real directory — with C byte-identical to the reference's over the
  whole closure.

  Nothing is dropped by host, which is the whole point: `attest` hashes the emitted C, so
  host-dependent emission would make the same source attest differently on Linux and
  Windows. The C preprocessor selects; both platforms stay type-checked.

  **`ItemData` grew a dedicated `(xat,xac)` attribute pair for externs** rather than
  overloading an existing slot. `(g,h)` looks free on an extern and is not: it is the
  bracket-generic slice into `gar`, and `typeck.fn_call_ret` reads it off *whatever item a
  call resolves to*, extern included — so attributes parked there crash on the first call to
  an extern. Recorded because the same trap is waiting in every other `ItemData` slot.

  The deferral test `cfg_is_not_yet_in_the_byte_identity_allowlist` is replaced by its
  inverse, `every_cfg_bearing_corpus_file_is_byte_identity_verified`: it scans the corpus
  for a real leading `@cfg(` and asserts each such file is allowlisted, because a dropped
  allowlist entry does not error — it silently stops verifying a file.

- **`examples/cfg_headers.jtr`** — which synthesized `#include` gets a guard, and why the
  rule is *every* declaration naming the header must agree rather than *any* of them being
  guarded. Guarding too little costs a redundant include, which header guards make free;
  guarding too much deletes a header some platform needs unconditionally, which breaks code
  that has nothing to do with `@cfg` — so the fallback is always "unconditional".

  It exists because the rule was corpus-blind: `cfg_platform.jtr` and `std/sysdir.jtr` only
  ever exercise the AGREEING case (one header per platform), so the mixed row and the
  guarded-plus-unguarded row were dead code that still type-checked. Being allowlisted is
  what puts the *port's* copy of the agreement scan under byte-identity rather than under a
  one-off probe. On the reference side,
  `a_header_named_by_two_opposite_platforms_is_unconditional` fills the matching gap: the
  existing mixed test pairs a guarded declaration with an unguarded one, where
  "unconditional" also falls out of "one of them is live everywhere".

- **The IO slice — `sink` + `cursor` (`core`) and `writer` (`std`).** Tier 2 area 3. The
  four design decisions, and the two that implementation corrected, are written up in
  `docs/io-design.md`.

  **The finding that set the architecture:** `@no_alloc` does not see through a trait
  method and **passes vacuously** — it accepts a `@no_alloc` function writing through a
  trait whose impl allocates on every call, while correctly rejecting the direct-call
  control in the same file. So a `core` module built on a trait would carry a marker that
  proves nothing, which is worse than carrying none: it reads as a guarantee. `core`
  therefore gets the concrete `Sink`/`Cursor` (free functions, direct calls, a real
  proof) and the `Writer` trait lives in `std`. The tier line is drawn exactly where the
  proof stops. Pinned by `no_alloc_does_not_see_through_a_trait_method`, which asserts
  the control still fails too — otherwise it would pass for the boring reason.

  `writer_demo.jtr` is the payoff rather than a tour: one `render` routine, called once
  against stdout and once against a buffer, with the buffered result compared
  byte-for-byte. That is what makes program output testable without capturing a
  subprocess.

  **Two things implementation changed, recorded rather than smoothed over.** The
  `failed()` latch was *removed, not shipped*: `print_str`/`eprint_str` return nothing so
  a stream write has no detectable failure, and sink overflow is deliberately the sink's
  business — so `failed()` could only ever answer `false`, and a query that always says
  "fine" invites a caller to believe it checked something. And the `Writer` API needed
  **one** entry point, not two: `write_str` + `write_str_into` forced every formatter to
  open with `if is_buffered(w)`, and a formatter that must know its destination is not
  polymorphic. The sink now travels on every write and is unused for streams — a real
  cost, pinned by `a_stream_writer_leaves_the_scratch_sink_alone`.

  **Reading is not symmetric with writing and was not faked.** There is no partial-read
  intrinsic, no file handle, no `read_line` — only `read_file`, which slurps. `Cursor`
  over that result is the whole honest reader; a streaming `Reader` is blocked on an
  intrinsic, and wrapping the slurp in one would ship an API whose central promise is
  false. Also deferred with reasons: no `BufWriter` (wants an allocator, and stdout
  already buffers in C stdio), no error sets on writes (they want a fallible `flush`).

  19 `@test`s across three sibling suites, 3 toolchain-free structural tests, the demo's
  documented output, and byte-identity with the self-hosted `cgen.jtr` for all seven new
  files, first try. No intrinsic, no closure change, no reseed.

- **`std/str` — the named module in front of the string intrinsics.** `core` tier, zero
  imports, every function `@no_alloc`, every result a view. Two halves: thin wrappers so
  that when `extern "c"` retires an intrinsic one module changes rather than every call
  site (`eq`, `eq_ignore_case`, `has_prefix`, `has_suffix`, `has`, `index_of`, `trimmed`,
  `is_valid_utf8`, `codepoint_count`, `grapheme_count`); and the operations that were
  genuinely missing — `before`/`after`, `before_last`/`after_last`,
  `strip_prefix`/`strip_suffix`, `trim_start`/`trim_end`/`strip_cr`, `last_index_of`,
  `count_of`, `matches_at`, `clamped`, `is_empty`/`is_blank`. `examples/str_ops.jtr` said
  it outright — "with `find` + `substr` you can split by hand" — and that hand-splitting
  is what these replace.

  **It deliberately does not reimplement `split`**, which already exists as a for-loop
  form handing back zero-copy views (`for w in split(s, sep)`,
  `examples/histogram.jtr`). A library cursor beside it would be two ways to do one
  thing, so the module documents `split`'s semantics instead — measured, not guessed:
  empty fields kept, a trailing separator yielding a final empty field, `split("")`
  giving *one* empty field rather than none, multi-byte separators working. `split` and
  `graphemes` being loop forms is also why nothing here is named either.

  Decisions made for reasons rather than symmetry: `before` returns all of `s` on a miss
  while `after` returns nothing, so the pair never both claim the whole string;
  `count_of` counts 0 for an empty needle, because the alternative invites an infinite
  loop in any caller advancing by the needle's length; `last_index_of("")` is `s.len`
  where `index_of("")` is 0, which is what makes `after_last(s, "")` empty.

  Verified at six layers: 10 `@test`s in the sibling `str_test.jtr`, 9 toolchain-free
  properties over an independent Rust oracle, 2 bolero fuzz targets (the needle is
  derived from the fuzz input, so needle-longer-than-haystack and empty-needle cases are
  actually reached), a differential driving the COMPILED module against that oracle over
  nine ops, the demo's documented output, and byte-identity with the self-hosted
  `cgen.jtr`. Two of the Jestyr tests exist to check the author rather than the code: one
  pins the hand-written ASCII whitespace set against the `trim` intrinsic's definition,
  the other pins `split`'s five documented behaviours so the prose cannot drift from the
  language.

  The differential also caught a bug in *itself* rather than the module: its first
  version stripped trailing `\r`/`\n` greedily from stdout, so `after("\r", "")` —
  correctly the whole string — compared as empty and failed a correct implementation. It
  now removes exactly the one terminator `print_str` appends.

  No intrinsic, no closure change, no reseed.

- **All four Tier 2 capability handles — `Fs`, `Clock`, `Env`, `Process`.** `Process`
  landed first; `Fs`, `Clock` and `Env` are added to the existing `fs.jtr`,
  `time.jtr` and `env.jtr` beside the ambient free functions they wrap. One module per
  domain rather than a parallel universe of `*_cap` modules; the free functions stay as
  the documented low-level layer (the shape a `sys` tier will own, and what the
  self-hosted compiler calls directly).

  **Each restricted mode is chosen for a different reason** — they are not four copies
  of one symmetry, and this is the part worth reading:

  - `time.manual(start)` — **determinism**, not permission. A `denied()` clock would be
    useless (code would divide by a zero duration); a clock you `advance` yourself makes
    an elapsed time *assertable* instead of merely bounded. It may also run backward, on
    purpose, so code that must survive a non-monotonic reading can be tested.
  - `env.sealed()` — **proving a negative.** Reports every variable unset while still
    counting lookups, so a test establishes both that a subsystem behaves identically
    with no ambient configuration and that it nevertheless tried to read some. A build
    that silently varies with `CC` or `TMPDIR` becomes a test rather than someone
    else's broken machine.
  - `fs.read_only()` and `fs.denied()` — **three states, because "read but don't
    modify" is a real need**, not the midpoint of a symmetry: a linter, a formatter,
    `jestyrc check`. `denied()` refuses reads too, including existence probes, since a
    probe leaks the shape of a tree the handle exists to hide.
  - `process.denied()` — refusing while recording attempts.

  `caps_demo.jtr` is the argument rather than a tour: a `stamp` function whose
  signature names every effect it performs produces byte-identical output across two
  runs with deterministic handles, and varies with `host()` ones — with nothing about
  `stamp` changing between them.

  **Cost, measured.** `time.jtr` was free. `fs.jtr` and `env.jtr` are self-host closure
  modules and owed a **reseed** (+192 lines of flattened source, +163 of seed C) — but
  **not** a port mirror, because adding library code to a closure module changes the
  flattened source, not the compiler's behavior. The `cgen.jtr` mirror is owed for
  emission changes, and this was not one. Byte-identity held for every file first try.

  Suites live in sibling files (`fs_test.jtr`, `env_test.jtr`, `time_test.jtr`, 15
  tests) — for `fs` and `env` that is not merely convention, since a `@test` inside a
  closure module would be compiled into the flattened compiler itself. Every negative
  result has a positive control beside it: "the write was refused" only means something
  if the same write through a `host()` handle lands.

  **A limitation surfaced and pinned:** `env.argc()` / `argv()` / `program()` read 0 and
  empty **inside a `@test`**, because the harness emits `int main(void)` and the runtime
  never records the arguments. Environment *variables* are unaffected (`getenv` does not
  go through `main`), which is the contrast that identifies the cause.
  `argv_is_invisible_to_the_test_harness` asserts it, so if the harness ever forwards
  `argv` someone decides deliberately what should happen.

- **`std/test_fixture`, `eq_golden_all`, and `diff_count` — the three `std/test`
  gaps closed.** The slice shipped with three named limits; all three now have an
  answer, two of them by building it and one by bounding it honestly.

  **Expected diagnostics** turned out not to need the compiler as a library, which
  was the original reason for deferring them. It needs to *run* the compiler and
  compare text: `test_fixture.capture(p, cmd, out_path, buf)` runs a command with
  both streams redirected into a file, `fs.read_text` reads it, `eq_golden_all`
  judges it. Deliberately no `expect_diagnostics(file, want)` one-liner — it would
  have to invent the compiler's path, and a helper that silently runs the wrong
  binary is worse than no helper, so the caller supplies it.

  **Temp files** are available (`test_fixture.temp_path`), resolving `TMPDIR`, then
  `TEMP`/`TMP`, then `.` — no single spelling is portable, and on this repo's Windows
  box `TMPDIR` is unset while both others are set. It stays deterministic because the
  *caller* names the file. Temp **directories** are still absent: there is no `mkdir`
  intrinsic, and creating one via `process.run("mkdir …")` is refused because it
  would make every caller's test depend on shell quoting for a path it did not
  choose.

  **The diff** now reports every differing line (`eq_golden_all`, capped at 8 then
  summarized) and counts them (`diff_count`), where before only the first difference
  was reported. The limit is stated precisely and *tested*, not just written down:
  this is an **aligned** line comparison, so one inserted line at the top makes every
  following line differ and the count becomes the file length, where a real diff
  would report a single insertion. An LCS table needs O(n·m) storage — an allocator —
  which a `core` module cannot have. `diff_count_is_aligned_not_an_edit_script` pins
  it, so if someone implements LCS in an allocator-taking tier, that test is the one
  that must change deliberately.

  `test_fixture` is `std` and the second file in the slice that performs effects
  (`test.jtr` decides, `test_report.jtr` prints, `test_fixture.jtr` fetches). Every
  capture goes through the `Process` capability, so a `denied()` handle writes no
  file — verified with a `host()` control in the same test.

  One platform detail found by a failing golden and now owned by `capture`: **no
  space before the `>`**. cmd.exe's `echo` treats everything after `echo ` literally
  including the space preceding a redirect, so `echo hi > "f"` writes `"hi "` while
  `echo hi> "f"` writes `"hi"`; POSIX sh tokenizes `>` as an operator either way.

  Layers: 26 `@test` functions in `test.jtr` (up from 22) and 4 in the sibling
  `test_fixture_test.jtr`, a compile-clean test, a c-oracle assertion on the demo's
  documented output (every value a *property*, never a path or a captured message,
  since those are machine-specific), a `diff_count` Rust oracle with a coherence
  property (zero iff `lines_eq`, symmetric, bounded by the longer side), and a new
  `dcount` op wired into the existing differential test so `diff_count` is checked
  against that oracle through the real compiled module.

- **Range-slicing a `[]T`.** `xs[lo .. hi]` narrows a slice to a view of the same
  buffer — `{ ptr, len }` in, `{ ptr + lo, hi - lo }` out — no copy and no
  allocation, the `[]T` twin of `str`'s existing sub-view. It used to be rejected
  with `error: the C backend does not support ranges yet` even though `str` sliced
  fine. All four forms work (closed `a .. b`, open-ended `a ..`, inclusive
  `a ..= b`, empty `a .. a`), and bounds are asserted (`lo <= hi <= len`) so a bad
  range faults deterministically instead of producing a view past the end of the
  buffer. No UTF-8 boundary check, unlike `str`: a `[]T` has no encoding.

  The payoff is that **no raw pointer crosses a stdlib boundary any more**.
  `std/test_report.finish(c, rep)` takes the `[]u8` the checks recorded into and
  narrows it itself, where before it needed the `*mut u8` that `alloc` returned
  purely to reach `slice(u8, raw, n)`.

  Deliberately **not** extended to a fixed-size array: `arr[lo .. hi]` would yield
  a view borrowing the array's inline storage, which is the borrowed-projection
  question (safety-mosaic item 2) rather than a typing one.

  This is the one change in this batch that paid the full two-sided tax — mirrors in
  `examples/std/typeck.jtr` and `examples/std/cgen.jtr` plus a refreshed bootstrap
  seed in the same commit — because it adds a construct the corpus then uses.
  Emitted C went byte-identical between the reference backend and the self-hosted
  one on the first attempt; `examples/slice_range.jtr` is the corpus demo carrying
  that guarantee. One subtlety for the next such mirror: the base expression emits
  *before* the statement-expression's temp is taken and the bounds *after*, so
  nested temps number identically on both sides.

- **`std/process` — running a command behind an explicit capability.** The first
  of the four planned Tier 2 handles (`Fs`, `Clock`, `Env`, `Process`), and free:
  a named wrapper over the existing `run_command` and `eprint_str` intrinsics, no
  new intrinsic and no reseed.

  `run_command` is ambient — any function anywhere can reach it, and no signature
  reveals that it did. A `Process` value makes the authority to spawn something a
  caller must *pass*, so `fn build(mut p: Process, …)` announces in its type that
  it may execute commands. This is explicitly **not** a sandbox: nothing stops
  direct `run_command`, and nothing in a library could. It has the same shape of
  limitation as `@no_alloc`'s call-graph blind spot and is worth having for the
  same reason — the honest path is also the documented one, and the capability is
  visible in the signature.

  `denied()` is the half that earns its keep: a handle that executes nothing while
  still counting every attempt, so a test can assert both that nothing ran and
  that the right number of attempts were made. `process_test.jtr` proves the
  refusal is real rather than cosmetic by running one file-creating command through
  a `host()` handle and finding the file, then the *same* command through a
  `denied()` handle and finding nothing — the host half is present as a control so
  the negative result cannot pass vacuously. Flipping `denied()` to permit kills
  four of the seven tests.

  **`run_ok` is the portable API, and that is forced by a real platform
  difference.** The runtime helper is `return (int32_t)system(cp)` — raw. Windows
  gives the command's exit code; POSIX specifies a *wait status* with the code in
  the high byte, so `exit 3` is 3 on one platform and 768 on the other. Only zero
  means the same thing on both, so `run_ok` (== 0) is portable while `run` is
  documented as platform-specific for any non-zero value. Normalizing it
  (`WEXITSTATUS` in the helper) is a runtime change owing the `cgen.jtr` mirror
  plus a reseed, and is recorded as follow-up 3 in `docs/stdlib-roadmap.md`; it is
  also the clearest argument yet for the `sys` tier.

  Seven `@test` functions in the sibling `process_test.jtr`, two toolchain-free
  compile-clean tests, a c-oracle assertion on the demo's documented output, and
  byte-identity between the reference backend and the self-hosted `cgen.jtr` for
  all three files (first attempt). Deliberately **no** command-string fuzzer:
  feeding fuzzer-generated strings to `system()` would execute arbitrary shell
  commands, which is not something a test suite should do. The property layer is
  in-language instead — `attempts_are_never_lost_or_double_counted` checks
  `runs + refused == n` exhaustively over small `n` against the real compiled
  handle, rather than against a Rust re-description of a four-field struct.

- **`std/test` + `std/test_report` — expectations and golden comparison, split
  across the tier boundary.** The `@test` harness has existed since workstream O
  and had two users in the whole corpus, because writing a test meant hand-rolling
  `if str_eq(a, b) == false { return false }` and getting a bare `false` back when
  it failed. Now: `test.eq_str(c, rep, "base", got, want)` records the check,
  returns the verdict, and appends `FAIL base: got "x" want "y"` — and
  `test_report.finish(c, raw)` is the last line of the test body.

  The split is the point. `test.jtr` is `core`: zero imports, every function
  `@no_alloc` (so "asserting never allocates" is checked, not claimed), and it
  **cannot print**. A `Check` value counts; failure text is rendered into a `[]u8`
  the caller supplies. `test_report.jtr` is the `std` half and the only file in the
  slice that performs an effect. That is what lets one `Check` end up on stdout,
  another end up compared against a golden file, and a third run somewhere with no
  stdout at all.

  `eq_golden` compares line-wise, insensitive to CRLF and to a missing final
  newline and to nothing else — so the same golden file compares equal checked out
  on Windows and on POSIX — and names the line that differs, which is the whole
  value of a golden over `str_eq`. `escaped` renders arbitrary bytes as printable
  ASCII, so a failure message can show you the trailing `\r` that made two
  apparently-identical lines differ; the property that makes it trustworthy is
  that it **round-trips** (an independent decoder recovers the original bytes at
  arbitrary bytes, under the fuzzer), so two different values can never render
  alike. It also means the report is always valid UTF-8, which is why
  `test_report.finish` can hand it to `from_utf8` unconditionally instead of
  carrying a latent abort in the failure path.

  Everything from the caller goes through the escaper — values **and** check
  names. An earlier version wrote the name through the module-authored-text path,
  so a name containing `\n` forged an extra `FAIL` line into the report (log
  injection, in miniature) and a name containing a high byte would have broken the
  printable-ASCII invariant that `from_utf8` call depends on. Pinned by
  `a_check_name_cannot_forge_a_report_line`. Worth recording that the property
  tests could not have caught it: they check the primitives, and the hole was in a
  caller of them.

  Six verification layers, all green: 21 colocated `@test` functions, 3
  toolchain-free compile-clean tests, 11 proptest properties over a Rust oracle, 4
  Bolero fuzz targets, a differential test driving the **compiled Jestyr module**
  against that oracle (five ops × 48 cases), and byte-identity between the
  reference backend and the self-hosted `cgen.jtr` including the emitted test
  harness. The differential test reaches bytes `path`'s could not: `test_demo.jtr`
  takes stand-ins in its arguments (`;` newline, `^` backslash, `#` quote, `!`
  0x01), where `path_matches_the_reference` had to exclude backslash because it
  passed paths through the command line literally.

  No new intrinsic, no closure change, no reseed. Three language gaps recorded
  in `docs/stdlib-roadmap.md` from building it: a capability handle cannot own
  borrowed storage (a borrow is second-class, so `[]u8` cannot be a struct field
  — counters live in the handle, storage stays with the caller); `[]u8` cannot be
  range-sliced, which is why `finish` takes a `*mut u8` at all; and module
  `const`s are emitted **unqualified**, so `std/path`'s `BACKSLASH` and this
  module's collided as `redefinition of 'j_BACKSLASH'` in generated C — note the
  asymmetry with modules-v2, which does let two modules share a struct name.

- **`mono_nanos` intrinsic and `std/time` — Jestyr code can measure elapsed
  time.** Until now it could not ask the clock at all: `@bench` timed a whole
  function from generated C, and every other measurement in the repo timed
  binaries from the outside. `time.now_nanos()` reads `CLOCK_MONOTONIC`, with
  `since_nanos`/`since_micros`/`since_millis` on top.

  Monotonic on purpose, and monotonic only — no calendar, no time-of-day. The
  origin is unspecified, so only DIFFERENCES are meaningful, which is the right
  primitive for durations and immune to the wall clock being adjusted
  mid-measurement. A calendar tier needs its own intrinsic.

  Gated on use (the helper and `<time.h>` are emitted only for programs that
  call it), mirrored in the port, seed refreshed in the same commit, rung 3
  green. `examples/std/time_demo.jtr` records two lessons its own failures
  taught: do not time work the optimizer can delete (gcc -O2 close-formed the
  first version's Gauss-sum loop, making zero elapsed the honest answer), and do
  not assert a clock advanced over a *fixed* amount of work — spin until it is
  observed to tick instead, since granularity varies by platform.

- **`env_var` intrinsic and an expanded `std/env`.** `env.get(name)` reads an
  environment variable as a `str` **view** into OS-owned storage — no
  allocation, nothing to free, the same contract `argv` has — with `has` for the
  set-vs-empty question `get` cannot express, `get_or` for the
  read-a-setting-with-a-default shape, and `program()` for argv[0]. Spelled
  `get` rather than Rust's `var` because `var` is a Jestyr keyword.

  The runtime helper is emitted **only when the program calls it**, so every
  program that does not stays byte-identical. Landed with its port mirror and a
  refreshed seed in the same commit, and went byte-identical on the first
  attempt: `selfhost_fixpoint_full` and `selfhost_fixpoint_subset` both green.
  The eleven edit sites an intrinsic touches are now written down as a checklist
  in [docs/stdlib-roadmap.md](docs/stdlib-roadmap.md).

- **`std/path` — the first slice of the stdlib readiness layer**, and the first
  stdlib module whose allocation behavior is *proven*. Lexical path
  manipulation (`base`, `dir`, `ext`, `stem`, `is_abs`, `dir_len`, `join`,
  `normalize`) with no heap and no syscalls: every function is `@no_alloc`, so
  the escape checker rejects the file if any of it ever reaches for the
  allocator. Queries return `read str` views into their argument; composition
  writes into a caller-supplied `[]u8` and returns the byte count, the
  `core.format_u64` idiom. Both `/` and `\` parse as separators, only `/` is
  ever written, so composed output is byte-identical across platforms.

  It is also the `@test` harness's first real user — the unit tests ship
  beside the code (`jestyrc test examples/std/path.jtr`), where previously
  `examples/tests_demo.jtr` was the only file in the corpus using the
  attribute. Verified at four layers: the in-language `@test` suite, a
  toolchain-free "compiles clean" test, a c-oracle assertion on the demo's
  documented output, and a **differential property test** that drives the
  compiled Jestyr module and requires it to agree with an independent Rust
  oracle, plus bolero totality coverage on that oracle.

  Costs nothing in bootstrap terms: no closure module imports it, so there is
  no port mirror and no reseed. Both files *are* in `CGEN_GOLDEN_ALLOWLIST`
  though — the self-hosted `cgen.jtr` lowers them byte-identically to the
  reference backend, verified rather than assumed. See
  [docs/stdlib-roadmap.md](docs/stdlib-roadmap.md) for the tier model, the
  priority order, and the list of things deliberately staying out of `std`.

- **Two modules may now define the same generic struct** (`fn Box(comptime T:
  type) -> type`), completing collidable names: their monomorphized instances
  get distinct symbols (`Jestyr_Box__m1__i32` vs `__m2__i32`), fields, and
  method instances, on both toolchains, byte-identically. This was the last
  open kind in the modules row — plain fns/consts/types/variants and generic
  enums were already collidable.
- **`jc build|run` emits `#line` directives**, mapping generated C back to the
  original per-file sources exactly as the reference does — the module-path C
  of the two toolchains is now byte-identical *including* debug info, which
  also closes the recorded `jestyrc attest` vs `jc attest` `c-sha256`
  disagreement for module programs.

### Documented

- **A self-host closure module's NAME is a reserved identifier across the whole
  flattened compiler**, recorded in
  [docs/stdlib-roadmap.md](docs/stdlib-roadmap.md) after migrating `cgen.jtr`'s
  loader onto `std/path` was tried and reverted. The flatten concatenates the
  twelve closure modules at the token level and strips module qualifiers, so it
  cannot distinguish `mod.item` from a field access on a local variable of the
  same name. `cgen.jtr` has thirteen `path.` sites — `path.start`, `path.end`,
  `path.len` on locals — and importing a module named `path` rewrote them into
  bare `start`/`end`/`len`, producing a flattened compiler gcc rejected. The
  duplication (`path_dir_len`, ten lines) is cheaper than renaming every local
  named `path` across 15,000 lines, so it stays, documented at both sites.
  Notably this passed the seed refresh and the byte-identity goldens; only
  `selfhost_fixpoint_full` and `jestyr_driver_builds_itself` — the gates that
  actually compile the flattened compiler — caught it.

### Changed — may reject code that previously compiled

- **A borrow whose type never resolved is now refused rather than assumed
  copyable** (the `Unknown` finalization). `Ty::Unknown` is classified `Copy`
  so that inference gaps do not produce cascades of false escape errors; at the
  two points where `Copy`-ness *decides* an outcome that silently meant "let it
  escape". Those now report:

  ```
  error: cannot decide whether borrow `x` escapes: its type was never resolved
  ```

  No file in the 155-file corpus (which includes the self-hosted compiler)
  triggers this, and no corpus diagnostic changed — but out-of-corpus code can
  newly fail. Every case that reaches it is ill-formed code that had never been
  rejected: a field access on an unbounded type parameter (`x.v` where `x: T`),
  a field access on a primitive (`.w` on an `i32`), a genref field reached
  without `.*`. (One well-formed shape briefly hit it — a generic-struct
  ctor-body method returning a field by value — and is now handled properly
  instead; see below.) Rationale in
  [docs/escape-guarantee.md](docs/escape-guarantee.md).

- **Generic-struct ctor-body methods now type `self` as the real instance**
  (`Box(T)` with `T` opaque) instead of an opaque `Self`, so `self.field`
  resolves through the template. Consequence: returning a type-param field *by
  value* (`fn get(read self) -> T { self.v }`) is judged by the same
  conservative rule as every generic — refused with the ordinary "declare the
  return as `read`" message, since `T` may be non-`Copy` — where it was
  previously accepted through the exact typing hole the `Unknown` finalization
  closes. The borrow-return idiom (`-> read T`), used throughout the corpus, is
  unaffected; no corpus file changes its diagnostics or its emitted C.

### Fixed

- **The self-hosted back end read two arbitrary source bytes per top-level item.**
  `emit_extern_protos` tested `(w,u)` as an extern's abi span *before* checking
  `kind == 8`. On a `Fn` those two slots are the body `ExprId` and the attribute offset, so
  the header probe `src[u-2 .. u]` sliced wherever those happened to point. It never
  produced a wrong prototype — the odds of two random bytes spelling `.h` are slim — but it
  aborted the compiler outright the moment an offset landed inside a UTF-8 continuation
  byte, which is what a file with box-drawing characters in its header comment does. Found
  while porting `@cfg`, not by the corpus: every allowlisted file happened to keep those
  offsets pointing at ASCII. The `kind` test now comes first.

- **Two modules could not share a `const` name.** `const BACKSLASH` in both
  `std/path` and `std/test` made gcc reject any program importing both with `error:
  redefinition of 'j_BACKSLASH'` — an odd asymmetry with modules-v2, which already
  allowed two modules to share a non-generic struct name. Consts are now canon'd by
  module, like functions and types.

  Two lines, because only emission bypassed machinery that already existed:
  `build_owner` already notes a `const` in `name_mods` (consts share the *value*
  namespace with functions, so a colliding const was already in `dup_fns`), and
  typeck already recorded the resolved symbol for an unqualified reference via
  `record_call_sym` — after `scope_lookup`, so a local shadowing the name still
  wins. The fixes were to canon the definition in `Cgen::consts` and to consume
  `call_sym` in the value-position `Name` arm; the qualified path (`mathx.TWO`) was
  already correct.

  `canon` renames only on a real collision, so **every collision-free program is
  byte-identical** — the corpus golden did not move, and `std/test` dropped its
  `B_` prefix workaround in the same change. No port mirror and no reseed were
  owed: the port's `ml_*` loader already renames colliding top-level definitions at
  the token level and its scheme coincides with `canon`'s `__m<modid>`, verified by
  emitting byte-identical C from both backends for a two-`SCALE` program. Pinned by
  the fixtures in `jestyr_driver_module_c_matches_reference` (byte equality) and
  `jestyr_driver_builds_multi_module` (the values stay distinct at runtime,
  including one read unqualified from inside its own module).

- **A module's `@test` functions were emitted into every program importing it.**
  A `@test` function is an ordinary function with an attribute, and there is no
  dead-code elimination at the C-backend layer, so `std/path`'s colocated suite
  was compiled into every consumer — 1,087 lines of `path_demo.c`, 11 `malloc`
  calls among them. Converting those tests to `std/test` in place would have made
  it 2,789 lines and added a `printf`, breaking the freestanding-linkable property
  the `core` tier exists to guarantee.

  `std/path`'s suite now lives in the sibling `examples/std/path_test.jtr`, so
  `path_demo.c` is **744 lines with no test code at all** — cleaner than before
  the conversion started. The stdlib convention changed with it: a module with
  non-test consumers keeps its tests in a sibling `*_test.jtr`; a module only ever
  imported *by* tests (`std/test` itself) may colocate. Pinned by
  `path_stays_a_leaf_module`, which asserts `std/path` imports nothing and
  declares no `@test`.

  The underlying compiler fix — stop emitting `@test`/`@bench` items in non-test
  mode, where nothing can reach them — is recorded as future work in
  `docs/stdlib-roadmap.md`. It is a real emission change (it moves the non-test
  golden for the three corpus files carrying `@test` items, so it owes the
  `cgen.jtr` mirror and a reseed) and it is bigger than one predicate, because the
  `uses_*` helper gating, forward declarations, and generic-instance collection all
  scan `@test` bodies too.

- **`jestyrc test <file>` ran the whole import closure's tests, not the named
  file's.** `jestyrc test examples/std/path_demo.jtr` ran `std/path`'s eleven even
  though `path_demo.jtr` has none of its own. Nobody had noticed because until
  `std/test` landed, no *imported* module shipped a suite — and then importing it
  dragged 22 extra tests into every consumer's run. Collection is now scoped to the
  named module (the root file is always module 0, since `Loader::load_file`
  registers it before merging any import), and `--list` is scoped identically so it
  can never name a test the harness would not run.

  `item_mod` defaults to 0 when absent, so every single-module entry point — all
  the `typeck::check` unit tests — is unaffected. Pinned by
  `the_harness_is_scoped_to_the_named_module`, which asserts both directions.

  **No port mirror was owed**, which is unusual enough to write down given that
  "zero C change" normally is not the same as "zero mirror owed":
  `examples/std/cgen.jtr` reaches test mode only through its single-file dump path,
  while its module loader is wired to `build`/`run`, which never emit a harness —
  so every item there is module 0 and the new condition is vacuously true.
  `jestyr_cgen_test_mode_matches_reference` and `bootstrap_seed_is_current` both
  stayed green untouched. The reasoning is recorded at the change site in
  `src/cgen.rs` rather than in `cgen.jtr`, because `flatten_selfhost_concat` edits
  raw source spans and therefore preserves comments: a comment-only edit there
  would force a 28K-line seed regeneration for no behavior change.

- **Struct-variant patterns bound their fields to no type**, so a borrowed
  non-`Copy` field could escape its frame: `match w { one { n, k } => n }`
  returned a borrow out of a `read` parameter, while the positional
  `one(n, k) => n` and the plain projection `h.inner` both rejected it. Fixed on
  both toolchains; emitted C is unchanged corpus-wide.

## v0.1.0-research — 2026-08-07

First public release: the complete bootstrap arc, June 22 – August 7, 2026
(382 commits). Everything below is verified by the CI ladder described in
[README.md](README.md).

### The headline artifacts

- **A self-hosted compiler.** The Jestyr compiler is written in Jestyr
  (`examples/std/*.jtr`, ~25K lines flattened), compiles itself through its
  own module loader and gcc driver, and reaches a verified fixed point:
  the compiler compiled by itself reproduces its own C output byte-for-byte.
- **A gcc-only bootstrap seed** (`bootstrap/`): building Jestyr from scratch
  needs only a C compiler — no Rust. The committed seed is pinned against
  the live sources by a drift-guard test.
- **A dual implementation held byte-identical.** The ~52K-line Rust
  reference and the self-hosted port emit byte-identical C over a 148-file
  corpus (scope and known module-path divergences documented in README.md).

### The language (as of this release)

- Ownership without lifetimes: second-class `read`/`mut` borrows with a
  structural escape checker, generation-checked `genref`s, scope-bounded
  `region` arenas; RAII drops with recursive field/payload auto-drop.
- Generics with monomorphization; traits with `dyn` dispatch; methods,
  closures, function-pointer types; structs/enums with exhaustive `match`
  and payload projection.
- Error sets with payload-carrying errors, sound set checking through `?`
  and trait dispatch, `catch |e| match` payload extraction, error traces.
- The `unsafe` contract, fully enforced: raw-pointer operations outside
  `unsafe` are compile errors on both toolchains.
- Checked cost models: `@span` work-span classes (serializing a `par for`
  is a compile error), `@simd` as checked legality, transitive `@no_alloc`,
  `@deterministic` rejection of non-deterministic reductions.
- Deterministic floating point: locked flags, correctly-rounded parse and
  format, reproducible parallel reductions, a purified SHA-256 canary that
  also pins SIMD lane width.
- Structured concurrency (`concurrent`/`spawn`/`await`, channels, `select`)
  lowered to pthreads; data parallelism (`par for … reduce`, `par_map`,
  `par_scan`, `par_reduce`).
- Compile-time function evaluation (tiers 0–7, both toolchains), comptime
  reflection, contracts (`requires`/`ensures`), refinement-driven
  bounds-check elision, layout attributes and the layout report.
- Modules v2: per-module namespaces, directory-as-module, content hashing
  with manifest verification; a multi-file loader in the self-hosted
  compiler itself.
- Tooling, all of it also self-hosted: `test`, `doc`, `attest` (an API
  attestation manifest with machine-checked guarantees), `attest-diff` /
  `attest-verify` as a breaking-change CI gate.

### Release scaffolding (this week)

- Dual MIT / Apache-2.0 license texts; root README with scoped,
  command-verifiable claims; CI (Ubuntu + Windows test matrix, the full
  gcc-oracle ladder, a bootstrap fixed-point job); internal development
  logs moved to `docs/handoffs/` and bannered as historical.
