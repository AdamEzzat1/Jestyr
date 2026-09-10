# Bruchion — the handoff after the rename (2026-09-09)

Cold-start note for whoever picks up the Bruchion copy next. What was done, what was
verified, what was deliberately left, and where the next increment starts. The migration
record itself is `docs/migration-from-jestyr.md`; the prioritized defect list is
`docs/issue-audit.md`; the measured numbers are `docs/performance.md`.

## Where things are

| | |
|---|---|
| directory | `C:\Users\adame\Bruchion` — a full-history `git clone` of the Jestyr repository |
| branch | `bruchion`, from `d5d85ed` (the Jestyr edge-agent branch head, itself master `ba48107` + Pylon-as-jagent) |
| remote | `jestyr` → `https://github.com/AdamEzzat1/Jestyr.git` (the original; **nothing has been pushed** — Bruchion has no remote of its own yet) |
| commits this pass | `a26c8f9` Pylon rename · `130e059` Bruchion identity + docs + identity test · `fb64e97` runtime/kv fixes · then the benchmark/audit commit and this note |
| build | `cargo build --release` → `target/release/bruchionc(.exe)` and the forwarder `jestyrc(.exe)` |
| the ladder | `cargo build --release && cargo test --release --features "c-oracle,selfhost-fixpoint"` — count and time in §"Verification" below |
| line endings | the clone is `core.autocrlf=false`; a Windows checkout with the global default gets CRLF and fails every byte-identity test (`docs/building.md` has the three-command fix) |

## What changed, in one screen

- **Pylon** (`std/pylon`, `pylon_ops`, `pylon_cli`, suites, demo, bench, fixture, docs,
  service notes, `tools/pylon/`) replaced `jagent` everywhere, moved with `git mv`. Old
  names still honoured: `JAGENT_CONFIG` after `PYLON_CONFIG`, `./jagent.ini` after
  `./pylon.ini` (`doctor` says `source=legacy-env|legacy-cwd`), and
  `tools/jagent/build.jestyr` builds the command under the old binary name. No alias for
  the metric labels (`pylon_*`), the stderr prefix, `pylon.db`, `pylon.ini`.
- **Bruchion** replaced Jestyr on the public surface: Cargo package and binary
  (`bruchionc`; `jestyrc` forwards), the compiler's usage text, README, topic docs, site,
  citation, roadmap/status pages, `bruchion-design.md`, `benchmarks/rust_vs_bruchion`
  (tracks `bruchion/`, `bruchion_std/`), the examples' comments and printed output.
- **Kept, with the reason written down** (`docs/migration-from-jestyr.md`): the `jestyr_`
  C prefix and `Jestyr…` C type names, the `.jtr`/`.jestyr` extensions, the bootstrap file
  names, the sixteen compiler-closure modules' comments (they are the seed), the
  `jestyr-manifest/v1` header, the session-note file names, the original URLs, the
  history notes and CHANGELOG entries (banner added).
- **Two fixes with before/after**: `runtime.poll_for` resumes a wait the poller cut short
  (a missed 1 ms timer about one run in twenty → 0 of 60); `kv` keeps the platform's error
  code through the rewrite path (`KvFailed(code)`, `KvDamaged` no longer folded into `-1`),
  with `kv_test` 10 → 11.
- **New pages**: CONTRIBUTING, getting-started, building, language-overview, stdlib,
  performance, known-limitations, migration-from-jestyr, issue-audit, `tools/smoke.ps1`,
  `benchmarks/cpp_heavy_timing.ps1`.
- **The identity test** (`bruchion_identity` in `src/proptests.rs`): the compiler names
  itself; `jestyrc` ≡ `bruchionc` on stdout/stderr/exit across three invocations; Pylon's
  usage; a scan of the public surface for stray old names that prints every hit. It is
  what keeps the next edit from pasting an old name back.

## Verification

Focused, all green on this tree: `pylon_test` 20/20, `pylon_ops_test` 13/13, `kv_test`
11/11, `runtime_test` 10/10; the Pylon demo transcript, both CLI tests, the plan test (both
plans), `jc_build_matrix` (82 BUILD_OK, no FAIL); the four identity tests; the moved
transcripts (`files`, `try_read`, `writer_demo`, `jhttpd`); `jstate` and `jstatus`; all 15
C++ pairs byte-identical with the three static rejections intact; the Rust harness's nine
cases with identical outputs across every track. Mutants watched failing: the legacy
variable fallback disabled (two assertions), the group runner stopping after its first
member (seven).

The full ladder, twice. On `48196c0` (after the examples rename): **1375 passed / 5 failed /
3 ignored** in 652 s — all five reached by the rename (the seed closure's `sha256.jtr`
renamed, a test-only environment variable renamed on one side, the package/lock headers
renamed in the sources but not the pinned transcript, two audit lines without a marker, and
a cross-module `?` the error-set census cannot resolve), each fixed at its root in
`57fe3d3`. On `57fe3d3`: **1381 passed / 0 failed / 3 ignored** in 829 s, fully green. The
count is four above the Jestyr branch's 1377: the four identity tests.

The Linux ladder has not run on any of this — the copy has not been pushed anywhere CI
can see. Every socket suite is a loopback transcript; `tls_test` links OpenSSL.

## Traps met in this pass, so the next one avoids them

- **PowerShell 5.1 mangles UTF-8.** `Get-Content -Raw` reads a BOM-less UTF-8 source as
  code page 1252; writing it back as UTF-8 turns every em-dash and box-drawing character
  into mojibake, and `"…`b…"` in a double-quoted string is a backspace. Every rename
  script here reads with `[IO.File]::ReadAllText($p, UTF8)` and writes with
  `WriteAllText($p, $t, UTF8Encoding(false))`; `[IO.Directory]::SetCurrentDirectory` is
  needed because .NET resolves relative paths against the process directory, not
  `Set-Location`.
- **A word-boundary rename is not enough.** `.jestyr` (the build-plan extension),
  `jestyr-manifest/v1`, `Jestyr_`/`JestyrStr` (C identifiers), `jestyr_` (temp names, test
  names), the session-note file names and the two URLs all had to be protected
  explicitly; each pass over a new file set found one more. The scan test is the net.
- **The seed.** A comment in any of the sixteen closure modules changes
  `bootstrap/jestyr_flat.jtr` and trips the drift guard; that is why their comments still
  say Jestyr. `REFRESH_SEED=1` is the tax if a later pass wants them renamed. The list is
  `SELFHOST_MODULES` in `src/proptests.rs` — it has `sha256` and not `strmap`; a list
  reconstructed from memory had them swapped and cost one ladder.
- **What a rename reaches that a word-boundary grep does not see:** a test-only environment
  variable (`JESTYR_ENVIRON_PROBE`, now `BRUCHION_ENVIRON_PROBE` on both sides) and the
  package/lock format headers (`bruchion-package/v1`, `bruchion-lock/v1`, changed
  consistently in writers, parsers, suites and the pinned transcript;
  `jestyr-manifest/v1` kept because the Rust reference emits it).
- **`catch |e| match` cannot carry a block arm** (`check` accepts it, `cgen` refuses), and
  the error-set census cannot resolve `?` on a cross-module callee — so a code that must
  survive a `catch` needs a code-returning function (`sysfs.rename_replace_code`).
- **`@abi(ref)` is silent on a generic instance.** It checks clean and does nothing;
  `docs/issue-audit.md` 14a.
- **Compaction's rename is not fixed, only diagnosable.** The next sighting of
  `jstate_survives_a_torn_batch` will carry a code; 32 (sharing violation) is the guess.

## The next increment, with the evidence behind it

1. **Push the copy somewhere CI can see it** and let the Ubuntu ladder run: the one
   verification this pass could not do.
2. **The fn-pointer zero-fill** (`docs/issue-audit.md` 3): the one open P1 with a silent
   miscompile behind it; two-sided, seed refresh.
3. **Performance**: `arena_ast` at 1.56× on the `std/list` track is the largest measured
   gap. The emitted C shows the list header copied per recursive call and the node copied
   per hop; the remedy is by-reference lowering of large `read` parameters for
   instantiated generics (`@abi(ref)` today refuses to guess their layout) — two-sided.
4. **Rename the closure modules' comments** with a seed refresh, if the mixed comments
   bother a reader; nothing user-facing depends on it.
5. **Re-render the site** under the new name and decide its address.
