# Changelog

All notable changes to Jestyr. This project is pre-1.0 research software;
versions are snapshots, not stability promises.

## Unreleased

### Added

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
