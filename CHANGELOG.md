# Changelog

All notable changes to Jestyr. This project is pre-1.0 research software;
versions are snapshots, not stability promises.

## Unreleased

### Added

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
