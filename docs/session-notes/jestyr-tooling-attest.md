# Jestyr Tooling (workstream O) — session summary

Two green increments on `master`, one commit each, full test rigor. This session grew
the single `jestyr` binary's tooling: a polished `jestyrc test` runner, and the headline
**`jestyrc attest`** — a sound reproducible-build + machine-checked-guarantee manifest.

Parallel-safety contract honored throughout: edits only in `main.rs`, new files
(`src/attest.rs`, `src/sha256.rs`), `src/proptests.rs` (additive), the sanctioned
harness-string shaping in `src/cgen.rs`, and a behavior-preserving visibility bump in
`src/doc.rs`. Zero edits to `typeck.rs`/`types.rs`/`module.rs`/`escape.rs` (K's turf).

---

## Increment 1 — `jestyrc test` polish (commit `5636507`)

`Mode::Test` was ~80% built (it emits a `@test`/`@bench` harness `main` and runs it).
Added:

- **`jestyrc test <file> <substr>`** — run only `@test`/`@bench` items whose name
  *contains* the substring. `running N test(s)` and the pass/fail process exit code
  reflect the **filtered** roster.
- **`jestyrc test <file> --list`** — print the runnable test/bench names (one greppable
  `test <name>` / `bench <name>` line each, source order). Toolchain-free: no compile.

### The key design decision: filter at codegen, not at runtime

The "obvious" design is runtime filtering — bake every test, pass the filter as `argv`
to the built binary (à la `go test -run`). But `module.rs:638` (K's turf, off-limits)
pins the literal string `running 2 test(s)` emitted by `emit_tests`; a runtime filter
would force rewriting that `printf` to `running %d`, breaking an un-editable test.

**Codegen-time filtering** sidesteps it: the harness bakes only matching tests, so `N`
is naturally the filtered count, and the unfiltered path stays byte-for-byte identical
(`emit_tests(x) == emit_tests_filtered(x, None)`). The parallel-safety contract didn't
just constrain *where* I edited — it picked the architecture.

New API (cgen): `emit_tests_filtered(ast, info, Option<&str>)`, `list_tests(ast) ->
Vec<(String, TestKind)>` (discovery mirrors `test_main`'s `runnable` predicate exactly —
skips generic/unsupported `@test`s), and the `TestKind` enum. The `is_generic`/
`fn_supported`/`is_type_param` methods were refactored into free functions so the harness
and `list_tests` share one predicate.

---

## Increment 2 — `jestyrc attest` (the headline)

`jestyrc attest <file>` emits a deterministic, line-oriented **attestation manifest**.

### Manifest format (`jestyr-attest/v1`)

```text
jestyr-attest/v1
source <id>
c-sha256 <64-hex SHA-256 of the emitted C>
cc-flags -O2 -std=c11 -ffp-contract=off -fno-fast-math

<kind> <name>
  vis: pub | priv
  sig: <faithful one-line signature>
  guarantee: <phrase>          (zero or more; fns only)
…
```

- **Header** — magic tag, source id, the C hash, and the *locked* compile command.
- **Items** — one block per top-level `fn` / `const` / `struct`|`record`|`union` /
  `enum` / `extern`, sorted by `(kind, name)`. The `<kind> <name>` key line is the
  stable identity the `--diff` follow-up will key on. Each item carries its visibility
  and a faithful one-line signature; **functions** additionally list their
  machine-checked guarantees.
- **Reproducible by construction** — every line `\n`-terminated, items sorted, guarantees
  in `doc::fn_guarantees`'s fixed order. `attest x == attest x`, hash included.

Example (`examples/docs.jtr`, abridged):

```text
jestyr-attest/v1
source examples/docs.jtr
c-sha256 76412c271d369cf2bf262d0c86b725922fd730bc29b53f21d480a8cf88403e61
cc-flags -O2 -std=c11 -ffp-contract=off -fno-fast-math

fn abs
  vis: priv
  sig: fn abs(x: i32) -> i32
  guarantee: `ensures result >= 0`
fn add
  vis: priv
  sig: @no_panic fn add(a: i32, b: i32) -> i32
  guarantee: `@no_panic` — proven free of faulting operations
fn at
  vis: priv
  sig: fn at(xs: []i32, i: usize in 0..xs.len) -> i32
  guarantee: parameter `i` is constrained to `0..xs.len`
```

### Why only Jestyr can emit this soundly

1. **The C hash is a real attestation.** Codegen is a *proven* deterministic function of
   the source — locked by `CC_FLAGS` (`-ffp-contract=off -fno-fast-math`), the
   `fp_contract_tests` lock, and the cross-OS numerics canary. "Same source →
   byte-identical C" is an invariant, not an aspiration, so the hash means something.
2. **Guarantees are reconstructed from the AST, not prose.** `attest` reuses the *same*
   `doc::fn_guarantees` the doc generator uses, so the attested behavioral ABI can never
   drift from the rendered docs. `cargo-semver-checks` reverse-engineers compatibility
   from signatures plus a hand-maintained lint list and *cannot* see `ensures result >=
   0` or `@no_panic`. In Jestyr the contracts **are** the public behavioral ABI.

### Implementation notes

- `src/sha256.rs` — the canary's dependency-free SHA-256 (FIPS 180-4) **lifted to a
  shared non-test module**, so both consumers (the numerics-determinism canary and
  `attest`) hash with one self-tested copy. The NIST vectors that vouch for one now
  vouch for the other. The manifest's `c-sha256` was cross-checked against GNU
  `sha256sum` of the `emit-c` output — they agree.
- `src/attest.rs` — `manifest(source_id, src, ast, info)` (codegen + hash + records) and
  `global_src(modules)` (reconstructs the loader's concatenated span buffer so a
  multi-module program's guarantee/signature text slices correctly).
- `doc.rs` — `fn_guarantees`/`fn_sig`/`const_sig`/`extern_sig`/`expr_src`/`ty_str` bumped
  to `pub(crate)`. **Behavior-preserving** — the 11 doc golden tests stay green.
- `main.rs` — `Mode::Attest`, gated on the same `load → typeck → escape` pipeline as
  codegen (you can only attest a valid program).

---

## Increment 3 — `attest --diff` (shipped)

`jestyrc attest --diff <old> <new>` parses two manifest files back into structured
contracts and classifies each per-item change. Items are matched by their `<kind> <name>`
key (stable across signature changes); the guarantee sets are compared structurally.

| Change | Verdict |
|---|---|
| error added to a fn's `!{…}` | **breaking** |
| `requires` added (precondition strengthened) | **breaking** |
| `ensures` removed (postcondition weakened) | **breaking** |
| `@no_panic` lost | **breaking** |
| refined param narrowed (e.g. `1..100` ⊂ old) or constraint added | **breaking** |
| a `pub` item removed, or demoted `pub` → `priv` | **breaking** |
| the type signature (params/return) changed | **breaking** |
| error removed from `!{…}` | compatible |
| `requires` removed (precondition weakened) | compatible |
| `ensures` added (postcondition strengthened) | compatible |
| `@no_panic` gained | compatible |
| refined param widened (provably ⊇) or constraint removed | compatible |
| a new item added; `priv` → `pub` | compatible |

The verdicts follow Liskov/behavioral subtyping: strengthening a **precondition**
(`requires`, a param refinement) or weakening a **postcondition** (`ensures`,
`@no_panic`) breaks callers; the duals are safe.

**Soundness is asymmetric by design.** A false negative (calling a real break
"compatible") is the dangerous error, so only *provably* compatible changes get the
compatible verdict — anything a heuristic can't prove safe defaults to breaking. Concrete
cases:
- A refinement change is "widened" (compatible) **only** when both ranges are integer
  literals of matching inclusivity and the new range provably ⊇ the old. A non-literal
  change like `0..xs.len → 0..i` is incomparable → breaking.
- `sig_core` strips the structurally-compared bits (`pub`, `@no_panic`, `!{…}`, `in
  <range>`) from the signature before comparing, so e.g. losing `@no_panic` reports
  **once** (as the postcondition loss) rather than also as a spurious "signature changed".

**Exit code:** non-zero iff any breaking change — a drop-in CI ABI gate. A compatible-only
or empty diff exits 0. A malformed manifest exits non-zero with a clear message.

Implementation: `attest::{parse_manifest, diff, DiffReport, Verdict, ParsedManifest,
ParsedItem}` + `run_attest_diff` in main.rs. `parse_manifest` folds the rendered
guarantee phrases back into structured fields (round-trip-tested) and is tolerant of
unknown lines but rejects a non-manifest first line.

Example:

```text
$ jestyrc attest old.manifest.jtr > old && jestyrc attest new.manifest.jtr > new
$ jestyrc attest --diff old new
jestyr-attest diff
old: old (c-sha256 a1f7e8958e6a…)
new: new (c-sha256 6287483204d6…)
note: emitted C differs (a rebuild); per-item ABI verdicts below

compatible  fn get  param `i` widened `0..100` → `0..1000`
BREAKING    fn helper  removed (was pub)
BREAKING    fn parse_u32  error added `Overflow`
compatible  fn parse_u32  `requires s >= 0` removed
BREAKING    fn push  lost `@no_panic`

3 breaking, 2 compatible        # exit 1
```

Note: `--diff` consumes manifest *files* (the output of `jestyrc attest`), not `.jtr`
sources — so a CI job can compare against a stored baseline without the old source tree.

---

## Test matrix (every increment, mirroring `docs/TESTING.md`)

| Layer | Increment 1 (`test`) | Increment 2 (`attest`) | Increment 3 (`attest --diff`) |
|---|---|---|---|
| **unit** | `list_tests` discovery (order, generic-skip, empty); filter over a 3-test program; `None == Some("")` | header shape; 64-hex lowercase hash == emitted-C digest; guarantees reconstructed; visibility; sort order | manifest parse round-trip; one test **per rule** (error ±, requires ±, ensures ±, no_panic ±, refine narrow/widen/non-literal, pub-vs-priv removal, return-type change, no double-report) |
| **wiring** | filter + list through `module::load` on `tests_demo.jtr` | `load → typeck → escape → manifest` on `docs.jtr` | self-diff (identical manifests) → zero changes |
| **golden** | exact filtered/unfiltered harness; `--list` lines | full `docs.jtr` manifest pinned (hash spliced from live C) | pinned multi-edit report (2 breaking, 1 compatible) + gate |
| **property** | discovery soundness/completeness; unfiltered-count == test-count; substring & exact-name selection; determinism | determinism (hash incl.); hash == emitted-C digest; every item attested; guarantee count == doc extractor | reflexivity (self-diff empty); one edit → exactly one correctly-classified change; swapping old/new flips the verdict |
| **fuzz** (bolero) | `fuzz_test_runner` — baked ≤ discovered, filtered ≤ unfiltered, deterministic | `fuzz_attest` — never panics, locked header, hash == digest, deterministic | `fuzz_attest_diff` — parser total on arbitrary bytes; classifier total + reflexive on real manifests |
| **c-oracle** | `test_runner_filters_end_to_end` builds the filtered harness through gcc, asserts stdout + exit tally | n/a (the C is *hashed*, not built — fully toolchain-free) | n/a (diffs manifest text — fully toolchain-free) |
| **teeth** | neutralize filter → 6 fail; drop generic-skip → discovery fails | hash wrong bytes → 4 fail; drop guarantees → 4 fail; skip item → completeness fails | flip error-add → compatible → 3 fail; neuter `range_widened` → 2 fail |

`cargo test` stays toolchain-free (gcc tests gated behind `--features c-oracle`).
Warning-clean; all `examples/*` byte-identical. 563 default tests pass (577 with
c-oracle).

---

## Deferred (and why) — from `TOOLING-HANDOFF.md`

- **A faithful `jestyr fmt` is a trap right now.** The lexer's `skip_trivia` *discards*
  plain comments and all whitespace layout (only `///`/`//!` doc comments survive, into
  a side table the parser never sees), and `printer.rs` is a **debug-tree printer** — it
  renders `1 + 2 * 3` as `(1 + (2 * 3))` and cannot round-trip to compilable Jestyr. A
  real formatter needs (a) the lexer to retain comments+layout as attached trivia (a wide
  change — every span consumer assumes the current model) **and** (b) a new
  source-faithful printer. Multi-week, not an increment. A check-only canonical-signature
  linter (built on `doc.rs`'s faithful signature reconstruction, no lexer change) is the
  available formatting win if one is wanted.
- **An LSP is a trap.** No incremental reparse, no stable node IDs, no error-tolerant
  AST, docs stripped at the lexer. The *unique* value is the real-time part, which is the
  expensive part — and the static "guarantees on hover" already exists (the doc-gen
  Guarantees block, now also the attest manifest). Defer until there's an incremental
  query layer.

---

## Next steps

1. Extend `attest` records with struct/enum **field**-level detail and trait/impl method
   contracts, so `--diff` catches a widened struct field or a changed method `requires`
   (today the C hash catches layout changes but the manifest lists structs/enums as
   kind+name, and traits/impls are attested only via the hash).
2. A locked literal C-hash canary for a flagship example (mirroring the numerics canary)
   once cross-OS C-*source* determinism is separately confirmed — today the tests assert
   `hash == sha256(emit-c)` (robust) rather than a pinned literal (potentially OS-fragile
   at the C-source level).
3. Wire `--diff` into a `build.jestyr`/CI recipe once the build system (workstream K)
   lands — `attest` the `pub` API on each commit, fail the gate on a breaking diff.

All three core Tooling increments (test polish, `attest`, `attest --diff`) are shipped on
`master`. The remaining tools (`fmt`, LSP) are the documented front-end traps, deferred
until the lexer retains trivia and an incremental query layer exists.
