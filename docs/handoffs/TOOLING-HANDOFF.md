> **Internal development log** — kept for provenance. Status lines and counts in this file reflect the moment it was written, not the current state. Start at the repo [README](../../README.md) for current status and verified claims.

# Tooling (Workstream O) — Handoff (run in a parallel session, commit to master)

> Self-contained cold-start for the **Tooling** workstream (`jestyr` the binary —
> design §15). Designed to run **in parallel** with the modules-v2 session
> ([`MODULES-V2-HANDOFF.md`](MODULES-V2-HANDOFF.md), workstream K) — see the
> **Parallel-safety contract** below; if you respect it the two sessions never touch
> the same lines. Everything lands on `master`, one green increment per commit.
> Companion reading: `ROADMAP.md` workstream O, `jestyr-design.md` §15/§14,
> `docs/TESTING.md` (the test layers), `src/doc.rs` (the template tool), `src/main.rs`
> (CLI dispatch). Conflict tier: **LOW**.

---

## Mission

Grow the single `jestyr` binary's tooling. The high-leverage, *feasible-now* slice is:
1. **Test-runner polish** (`jestyr test`) — the cheap, mostly-built win that proves the
   subcommand wiring.
2. **`jestyr attest`** — the **unique, on-thesis feature**: a reproducible-build +
   machine-checked-guarantee manifest. This is the headline.

Deliberately **deferred** (see "What NOT to build"): a faithful formatter and an LSP —
both are traps in the current compiler.

---

## Parallel-safety contract (read before touching anything)

This session may edit:
- **`src/main.rs`** — new subcommand arms + a small hash/printing util.
- **New files** — e.g. `src/attest.rs` (the manifest builder), `src/testrun.rs` if the
  runner logic outgrows `main.rs`.
- **`src/proptests.rs`** — add `mod prop`/`mod fuzz` cases and golden tests (additive).
- **Read-only** reuse of `src/doc.rs` (`fn_guarantees`, signature reconstruction),
  `src/cgen.rs` (emitted C string), `src/main.rs`'s `CC_FLAGS`.

This session **must NOT** edit `src/typeck.rs`, `src/types.rs`, `src/module.rs`, or
`src/escape.rs` — that is **K's turf** (the modules-v2 session rekeys name resolution
there). The two streams share *zero* lines if O stays in `main.rs`+new-files and K
stays in `module.rs`/`typeck.rs`/`types.rs`. If you find yourself needing a typeck
change, stop and coordinate — it means the increment is mis-scoped.

Standard worktree flow: work on this branch, and after each green increment
**ff-merge to master** (`git -C <repo-root> merge --ff-only <branch>`). If `master`
moved under you (K landed), rebase and re-run the suite — conflicts should be nil by the
contract above; exhaustiveness errors from new AST variants are the only expected
fallout and the compiler names them.

---

## Inspiration — one idea to steal per tool

| Source | The single idea to steal |
|---|---|
| **rustfmt** | **Idempotence as a *tested* invariant** — `fmt(fmt(x)) == fmt(x)`, asserted in the suite. The source-level analogue of the SHA canary. |
| **gofmt** | **Zero config, one canonical form, no knobs.** The absence of options *is* the feature; never ship a `.fmt.toml`. |
| **rust-analyzer (salsa)** | **Memoized query/firewall architecture** — when the LSP eventually comes, model analysis as queries keyed on inputs so an edit re-runs only the affected slice; share that engine with `jestyr build`. (Not now — see traps.) |
| **Biome / Deno** | **One binary, parse once, fan the AST out** to fmt+lint+doc+test. Today each `Mode::*` re-lexes; the eventual win is a single parse per file. |
| **gopls / `go test`** | **Tests discovered by convention, not registration** — walk the tree, run every `@test`, summarize. No manifest. |
| **Unison** | **Content-addressed "already checked, never recheck"** — hash a definition's normalized form and cache "checked + its proven Guarantees." Feeds `attest` and pairs with K's module hashing. |
| **clangd** | **AST-matcher structural lints/fixits** — because Jestyr emits C and has refined params, "match this shape, offer this rewrite" maps cleanly onto contract-aware quick-fixes (later). |

---

## The unique feature — `jestyr attest`

**What it is.** A one-shot subcommand that emits a deterministic *attestation manifest*
for a `.jtr` program: (a) the **SHA-256 of the emitted C**, (b) the exact, locked
**compile command** (`CC_FLAGS = -O2 -std=c11 -ffp-contract=off -fno-fast-math`, plus the
conditional `-pthread`), and (c) the aggregated **machine-checked Guarantees** for every
`pub` item — `requires`/`ensures`, error set `!{…}`, `@no_panic`, refined-param ranges,
and the reference conventions (`read`/`mut`/`out`/`take`). Output is sorted, line-oriented,
and byte-reproducible (fittingly).

**Why it is uniquely a Jestyr feature.** No other language can emit this soundly:
- The **determinism is codegen-locked** (`CC_FLAGS` + the `fp_contract_tests` lock + the
  cross-OS SHA canary), so "same source → byte-identical C" is a *proven invariant*
  (`proptests::compilation_is_deterministic` and the per-feature `*_compile_deterministically`
  properties), not an aspiration — the C hash is a real attestation.
- The **Guarantees are reconstructed from the AST**, not parsed from prose — `doc.rs`'s
  `fn_guarantees` already does this for the doc generator. Rust's `cargo-semver-checks`
  reverse-engineers compatibility from signatures + a hand-maintained lint list and
  *cannot* see `ensures result >= 0` or `@no_panic`; Jestyr's contracts **are** the public
  behavioral ABI.

**Why it's truly useful (not flashy).** It reuses only machinery that already exists and
is already tested — it's *integration*, not new compiler subsystems. It produces the
manifest artifact a future package registry, CI gate, and the `--diff` follow-up all
need. And it is the determinism + "contracts prove" thesis cashed out as a shippable
deliverable.

**The killer follow-up (second increment): `jestyr attest --diff <old> <new>`** — a
*sound* semantic changelog / breaking-change detector: "`parse_u32` added `Overflow` to
its error set" (breaking), "`get` dropped `requires i < len`, now total" (compatible),
"`push` lost `@no_panic`" (breaking), "`scale` widened param `i` from `1..100` to
`0..1000`" (compatible). Classification rules: **breaking** = error added / `requires`
strengthened / `@no_panic` lost / refinement narrowed; **compatible** = error removed /
`ensures` added / refinement widened.

---

## Recommended increment order

1. **Test-runner polish (`jestyr test`)** — proves the subcommand seam, ~all machinery
   exists. `Mode::Test` already: is a recognized subcommand (`main.rs`, the `"test"` arm
   in the subcommand match), dispatches to `cgen::emit_tests`, runs the built binary, and
   is exempt from the "no `main`" check because it synthesizes its own harness `main`
   (covered by `module.rs`'s `tests_demo_example_builds_a_clean_test_harness`, asserting
   `running N test(s)`). **Add:** name filtering (`jestyr test <substr>`), `--list`, and
   per-test pass/fail tally surfaced to the process exit code. Touches only `main.rs` +
   the harness-string shaping in `emit_tests` (additive).
2. **`jestyr attest`** (the unique feature, first slice) — a new `Mode::Attest`:
   run the existing `module::load → typeck::check_program → escape::check → cgen::emit`
   pipeline, `sha256(c_src)` (reuse the dep-free `proptests::sha256` — lift it to a
   non-test module, e.g. `src/sha256.rs`, so both the canary and `attest` share it), print
   `CC_FLAGS` + the hash + the per-`pub`-item Guarantees via a new `attest::manifest()`
   that calls `doc::fn_guarantees`. New file `src/attest.rs` + one `main.rs` arm.
3. **`jestyr attest --diff`** (the killer follow-up) — parse two manifests, classify
   added/removed/changed as breaking vs compatible.

---

## What NOT to build (and why) — save the session

- **A faithful `jestyr fmt` is a trap right now.** The lexer **discards plain comments
  and all whitespace layout**: `skip_trivia` consumes `//` and `/* */` and emits *no
  token and no record* (only `///`/`//!` doc comments survive, into a side `docs` vec the
  parser never sees). And `src/printer.rs` is a **debug-tree printer, not a
  source-faithful one** — it deliberately renders `1 + 2 * 3` as `(1 + (2 * 3))` and
  cannot round-trip to compilable Jestyr. A real formatter needs (a) the lexer to retain
  comments+layout as attached trivia (a wide change — every span consumer assumes the
  current model, and the loader relies on disjoint global span regions) **and** (b) a new
  source-faithful printer. That's multi-week, not an increment. *If you want a formatting
  win now,* ship a **check-only canonical-signature linter** built on `doc.rs`'s existing
  faithful signature reconstruction (no lexer change) — it asserts public signatures are
  in canonical form, operationalizing the "byte-identical examples" discipline without the
  trivia rabbit hole.
- **An LSP is a trap.** No incremental reparse, no stable node IDs, no error-tolerant AST,
  docs stripped at the lexer. "Guarantees on hover" is on-thesis and gorgeous but the
  *unique* value is the real-time part, which is the expensive part — and the static
  version already exists (the doc-gen Guarantees block). Defer until there's an
  incremental query layer.

---

## Rigor — the test layers every increment ships (mirror the existing harness)

Match the discipline used across this repo. For each increment:

1. **Unit tests** — the pure logic in isolation. For `attest`: `manifest()` over a small
   in-memory AST emits the expected sorted records; the breaking/compatible classifier
   over hand-built manifest pairs. For the runner: filter + tally logic.
2. **Wiring tests ("confirm it's plumbed in")** — prove the subcommand is actually
   dispatched and runs the real pipeline, the way `module.rs`'s `*_compiles_clean` /
   `tests_demo_example_builds_a_clean_test_harness` do. E.g. an `attest_subcommand_is_dispatched`
   test that drives the `Mode::Attest` path end-to-end on `examples/docs.jtr` and asserts
   the manifest contains the known guarantees + a 64-hex-char C hash.
3. **Golden tests** — `attest` output on `examples/docs.jtr` (the existing doc-gen demo)
   pinned exactly; the `--diff` report on a hand-edited copy (add an `!{Overflow}`, tighten
   a `requires`) asserts exactly those two flagged, classification correct.
4. **Property tests** (`proptests.rs::mod prop`, via the `arb_*_program` generators) —
   the on-thesis ones: **determinism** (`attest` of the same source twice → identical
   manifest *and* identical C hash — reuse `arb_*_program` + the existing
   `compile_deterministically` pattern); **idempotence** where it applies; **diff
   soundness** (manifest vs itself → zero changes; any single guarantee mutation → exactly
   one change, correctly classified).
5. **Bolero fuzz** (`proptests.rs::mod fuzz`) — a `fuzz_attest` target over `arb_*_program`
   asserting `attest` never panics and always emits a well-formed manifest (and the
   classifier never panics on arbitrary manifest pairs).
6. **Teeth-verify each new property by mutation** — break the thing the test guards (e.g.
   make the classifier call an error-add "compatible"), watch the test fail, revert. A
   property that can't fail isn't a test.
7. **`--features c-oracle`** if an increment runs a built binary (the runner does) — add a
   gcc-oracle case pinning the runner's output on an `examples/*.jtr` with `@test`s.

Every increment stays **`cargo test`-green and warning-clean**; default `cargo test` must
stay toolchain-free (gate any gcc-needing test behind `c-oracle`).

---

## Documentation deliverable (Downloads)

When the workstream's first cut lands, write a **session summary / design doc to the
user's Downloads folder**: `C:\Users\adame\Downloads\jestyr-tooling-attest.md` —
covering what shipped, the manifest format spec, the attest/diff classification rules, the
test matrix, and the deferred items (fmt/LSP) with the reasons above. (This mirrors the
project's convention of dropping session summaries in `~/Downloads`.) Keep the in-repo
`ROADMAP.md` workstream-O entry updated too.

---

## Commit-to-master discipline (do this every increment)

- **One green increment per commit.** `git commit -F <msgfile>` (multi-line). End every
  message with: `Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>`.
- After green + warning-clean, **fast-forward master**:
  `git -C C:\Users\adame\Jestyr merge --ff-only <this-branch>`. Don't push unless asked.
- Teeth-verify before committing. Keep all examples byte-identical (the repo invariant).

---

## Pointers (verify line numbers; they drift — search the symbol)

| Thing | Where |
|---|---|
| CLI subcommand dispatch (add `Mode::Attest`) | `src/main.rs` — the subcommand `match` (`"doc"`/`"build"`/`"run"`/`"test"` arms) |
| Locked compile flags + their lock test | `src/main.rs` → `CC_FLAGS`, `mod fp_contract_tests` |
| Guarantees extractor to reuse | `src/doc.rs` → `fn_guarantees`, `fn_sig`, `ty_str`, `expr_src` |
| Dep-free SHA-256 (lift to a shared module) | `src/proptests.rs` → `mod sha256` |
| Determinism properties to mirror | `src/proptests.rs` → `compilation_is_deterministic`, `*_compile_deterministically` |
| Test-mode harness (runner) | `src/cgen.rs` → `emit_tests`; `src/main.rs` test dispatch; `module.rs` `tests_demo_example_builds_a_clean_test_harness` |
| `@test` attribute | `src/attrs.rs` (the `@test` validation) |
| Test-layer conventions | `docs/TESTING.md`; `src/proptests.rs` (`mod prop`/`mod fuzz`/`arb_*`) |
| Why fmt is a trap | `src/lexer.rs` `skip_trivia` (drops comments); `src/printer.rs` (debug printer, not source-faithful) |

## One-line summary

Ship `jestyr test` polish to prove the seam, then **`jestyr attest`** — a sound
reproducible-build + machine-checked-guarantee manifest (and a `--diff` breaking-change
detector) that only Jestyr can emit, reusing the locked `CC_FLAGS`, the SHA canary, and
the doc-gen's Guarantees. Avoid fmt/LSP (front-end traps). Full test rigor, docs to
Downloads, one green increment per commit to master.
