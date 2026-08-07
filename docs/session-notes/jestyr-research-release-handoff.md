> **Internal development log** — kept for provenance. Status lines and counts in this file reflect the moment it was written, not the current state. Start at the repo [README](../../README.md) for current status and verified claims.

# Jestyr — research-release handoff (what stands between the repo and its first public release)

> **Provenance.** On 2026-08-07, with the error workstream closed on both sides
> (E1–E5 + T1/T2, corpus 148, seed current, master `1e32dfa`), a five-role review
> panel evaluated the repo for a **first research release** — making it public and
> announced to a PL research audience, not a production 1.0. The roles: a PL
> researcher, a build/reproducibility engineer (who verified the cold-clone path
> live), a documentation/onboarding reviewer, an adversarial soundness/claims
> skeptic, and a release manager.
>
> **The verdict was unanimous: `not_ready` — and unanimously for the same
> reason: the SUBSTANCE is release-grade; every blocker is release scaffolding
> or claim scoping.** Total estimated effort to ready: **about two focused
> days.** This note is the complete worklist.

## ▶ START HERE — the shape of the release

The panel converged on the form: scrub + LICENSE + README, flip the private
GitHub repo (`AdamEzzat1/Jestyr`) public, cut an annotated tag
**`v0.1.0-research`**, and pair it with a short technical report/blog post
distilling the two genuinely novel artifacts. The announcement's single best
hook, per every role that mentioned it: **the two-command gcc-only bootstrap
demo** (compile `bootstrap/jestyr_seed.c`, watch the compiler regenerate its own
seed byte-for-byte). The build engineer verified it cold: 21 seconds, fixed
point holds.

**The contribution ranking for the report** (the PL researcher's ordering — use
it to structure the announcement):

1. **The dual-implementation byte-identity methodology + the gcc-only bootstrap
   seed** as a compiler-trust story — cite alongside Wheeler's *Diverse
   Double-Compiling* (the trusting-trust literature).
2. **The second-class-reference / genref / region escape ladder validated at
   28K-line self-hosting scale** — an empirical answer to "does no-lifetimes
   ownership scale?" that Hylo has not published.
3. **Checked cost models** (`@span` work-span classes, `@simd` legality,
   transitive `@no_alloc`) — the most novel pure language-design idea in the
   repo, currently invisible to anyone not reading session notes.
4. The determinism contract with compile-time rejection of non-deterministic
   reductions.
5. Sound error sets with payload-carrying errors (solid engineering,
   incremental over Zig).

Also worth a paragraph in any writeup (PL researcher): the **migration
discipline itself** — census-then-enforce (the E1 census found zero violations,
licensing strict enforcement with no migration) and the gated-on-use rule that
keeps every golden byte-identical until a corpus file exercises a feature.

---

## 1. The unanimous blockers (every role found these independently — all MUST)

| # | Blocker | Fix effort |
|---|---|---|
| 1 | **No LICENSE file.** `Cargo.toml` declares `MIT OR Apache-2.0` but no license texts exist anywhere in git — a public repo without them is legally unusable (nobody can build on, cite-with-artifact, or redistribute it). Add `LICENSE-MIT` + `LICENSE-APACHE` at root. | 15 minutes |
| 2 | **No root README.** GitHub would render a bare file listing; the de-facto entry docs are internal session handoffs whose stats contradict each other — **five different test counts** across HANDOFF.md (~9,300 lines / 294 tests / "86 tests" in its own quick-start), docs/TESTING.md (17.4k / 285 / 77 examples), ROADMAP.md §0 (~10K / 157), FP-DETERMINISM-CONTRACT.md (508), docs/error-payloads.md (~900) — all stale against the real state (909 default tests, 148-file corpus, ~28K-line self-hosted compiler, ~52K lines of Rust). | half a day |
| 3 | **The cross-OS determinism claim is unproven by the repo's own admission.** `FP-DETERMINISM-CONTRACT.md` states the canary digest (`4389bf83…`, still locked at its proptests site) was computed on exactly ONE machine (Windows + gcc) and literally says "read this before claiming cross-OS determinism" — while NUMERICS-HANDOFF.md and TOOLING-HANDOFF.md say "cross-OS SHA canary ✅ DONE". **One Linux/WSL run of `cargo test --features c-oracle` either upgrades the headline claim to proven or saves the release from a public falsification.** The contract doc itself calls this "the only blocker to proof". | one CI/WSL run |
| 4 | **Tracked personal data.** `docs/claude-memory-snapshot/` ships the author's machine paths, OneDrive backup layout, private-repo inventory across three projects, and laptop battery-health details; `docs/README.md` documents restoring AI-session memory with machine-specific paths. Scrub or delete from HEAD before going public. | an hour |
| 5 | **Self-contradicting doc headers.** `docs/unsafe-contract.md` opens "Today, `unsafe` gates nothing" above its own completed ladder (step 4 ✅: compile error on both toolchains); `docs/error-payloads.md` opens "design, pre-implementation … nothing here is built yet" above the closed E-chain. A skimming reviewer mis-cites the project in whichever direction they happen to read — or concludes the docs can't be trusted. | under an hour |
| 6 | **No CI** (no `.github/` at all). Every headline claim is verified only on the author's machine; the central byte-identity evidence is unverifiable by outsiders. A matrix (ubuntu + windows, `cargo test`, plus a gcc `c-oracle` job and a bootstrap job that compiles the seed and diffs the regenerated one — ~30 s) **doubles as blocker #3's fix**. | 2–3 hours |

## 2. Sharper findings that cut across roles (SHOULD before announcing)

* **The byte-for-byte claim needs SCOPING in the README** (skeptic): all 148
  goldens and the self-hosting fixed point run the `#line`-free single-file /
  concat emission path; the real module-loader path has **three admitted
  divergences** (no `#line` in the port, per-type artifact order, offset-derived
  spawn symbol names), documented only in session notes and pinned by
  `jestyr_module_cgen_matches_reference_except_line_directives`. State the scope
  precisely; the claim is honest once scoped, misleading unscoped.
* **The `jc` driver has three POSIX stumbles** (build engineer, with line
  numbers in `examples/std/cgen.jtr`): it hard-codes `gcc` (~line 12596) where
  the Rust driver probes cc/gcc/clang; it always names output `<stem>.exe`
  (~12590) even on POSIX; and `run` executes via C `system()` with the bare
  quoted path (~12610–12613), so a separator-free path won't execute on POSIX
  shells. Fix + `REFRESH_SEED=1` (the two-sided tax applies), or at minimum
  document the limitation.
* **`examples/cpp_compare/` is untracked** (release manager; matches the
  long-standing memory note), so HANDOFF.md's "0.985× hand-written C" figure is
  unreproducible — **commit the suite and the `restrict` microbenchmark, or
  delete the number** from every doc that repeats it.
* **A real code bug the skeptic found in passing**: duplicate enum-variant
  names are a silent last-wins `variants.insert` in `src/typeck.rs` (~334–337) —
  NUMERICS-HANDOFF.md (~232–235) already documents the consequence as a silent
  miscompile of the other enum's `err(...)`. A same-module duplicate-name
  diagnostic is a few lines and check-only (no emission change, no port tax).

## 3. Unique per-role findings (each role's items nobody else raised)

### The PL researcher
* **A one-page precise statement of the escape checker's guarantee** is a
  skeptical reviewer's first ask — not a formalization, just the theorem
  statement ("second-class borrows cannot outlive their frame") and the check
  algorithm, since `docs/unsafe-contract.md` already calls it "the escape
  checker's theorem".
* **A design-doc status map**: `jestyr-design.md` says "draft v0.1, not a
  specification" but never marks implemented vs planned — genrefs, regions,
  contracts, CTFE, error sets are real, while `import c`, async/effects,
  `@verified`/SMT, refinement types beyond bounds-elision, and `trusted` blocks
  are not. Add a status column or a `DESIGN-STATUS.md`; the raw material already
  exists in ROADMAP's workstream table.
* **Promote the checked cost models into `docs/attributes.md`** with the
  compile-error examples (`@span` serialization refusal, `@simd` legality) —
  the repo's most novel design idea should not live only in
  PARALLELISM-HANDOFF.md.
* Replace the six drifting hard-coded test counts with a pointer to CI, so no
  document states counts except one authoritative place.

### The build engineer
* **Cold-clone verification results** (usable in the announcement): `cargo
  build` clean; `cargo run -- run examples/hello.jtr` works; the exact
  `bootstrap/README.md` command built a working compiler in 21 s and the fixed
  point held. The Rust driver is genuinely cross-platform already
  (`EXE_SUFFIX`, `temp_dir`, cc/gcc/clang probing, FP flags as tested consts).
* **`docs/TESTING.md` is six weeks stale** (dated pre-self-hosting): it never
  mentions the `selfhost-fixpoint` feature, the corpus goldens, the seed drift
  guard, or expected wall-clock runtimes. The documented how-to-verify surface
  does not cover the claims the release rests on — rewrite it as the
  verification ladder (`cargo test` ~minutes; `--features c-oracle` needs gcc on
  PATH; `--features c-oracle,selfhost-fixpoint` reproduces corpus + fixed
  point), with runtimes.
* **Curate the ~40 session-notes files**: move under `docs/history/` (or
  `docs/handoffs/`) with a one-line disclaimer, so the signal docs stand out.

### The documentation reviewer
* **There is no language tour or tutorial at all** — the 98 root examples plus
  `examples/std` are the only way to learn the language, unindexed and
  unordered, with teaching demos mixed into the self-hosted compiler's own
  source. Minimum: an `examples/README.md` index (feature → file → expected
  output, grouped: ownership/escapes, errors, generics, traits, concurrency/par,
  comptime, layout, unsafe), noting explicitly that `examples/std/` IS the
  compiler. Better: a short `docs/tour.md`.
* **Internal/external triage via banners**: one line atop every internal doc
  ("Internal development log — kept for provenance; start at README.md") on
  HANDOFF.md, HANDOFF-NEXT.md, ROADMAP.md, the seven `*-HANDOFF.md` files,
  docs/TESTING.md, docs/session-notes/*. An hour of mechanical edits that
  converts the staleness problem from misleading to archival.
* **Kill the §0 status snapshots** — replace each with "see README for current
  status" so exactly one document owns the numbers.

### The soundness skeptic
* **Emit `#pragma STDC FP_CONTRACT OFF` into the generated C** (the contract
  doc's own gap #3), so the determinism invariant rides with the emitted file
  rather than only with `jestyrc`'s `CC_FLAGS` — `bootstrap/README.md`'s manual
  gcc line already carries the flags, but any other consumer of `emit-c` output
  silently loses them. (Emission change → full two-sided tax: port mirror,
  corpus, seed.)
* The four self-contradicting front-door surfaces enumerated for the fix pass:
  the two doc headers (blocker #5) plus HANDOFF.md's TL;DR numbers and
  ROADMAP §0 (which still lists Traits at 0%).
* An **implemented-vs-designed one-page matrix** (independently converged with
  the PL researcher's status-map ask — do it once, satisfy both).

### The release manager
* **Release plumbing**: annotated tag `v0.1.0-research` (git has no tags today),
  a `CHANGELOG.md` seeded from the milestone commits already in `git log`, and
  confirm `Cargo.toml` version matches the tag.
* **`src/dharht.rs` provenance**: it calls itself "a vendored copy of the
  D-HARHT blueprint" with no source or license attribution — one paragraph
  naming its origin (the author's CJC ecosystem) closes the repo's only
  vendoring hazard; alternatively drop the feature-gated file for the release.
* **Root declutter**: move the nine `*-HANDOFF.md` / `*-PHASE3.md` docs into
  `docs/handoffs/` so the public root reads README → jestyr-design.md →
  ROADMAP → docs/. Pure file moves.
* **`CITATION.cff`** + a "How to verify the claims" README section enumerating
  the three reproducible checks (gcc-build the seed; verify the fixed point;
  run the corpus goldens) — turning the byte-identity story into something a
  reviewer can *do*, not just read.
* **Declare out-of-scope explicitly in the announcement**: GPU backend, the
  thermal facet, production use.

## 4. The ordered plan (MUST → SHOULD → LATER)

**Day 1 — MUST (before flipping public):**
1. `LICENSE-MIT` + `LICENSE-APACHE` (15 min).
2. Scrub `docs/claude-memory-snapshot/` (at minimum `backup-and-remotes.md`);
   fix or banner `docs/README.md` (1 h).
3. Root `README.md`: thesis; claims **with scopes** (byte-identity =
   single-file/concat path, three module-path divergences pinned by golden;
   determinism = locked flags + construction, digest verified on N platforms);
   the three-command bootstrap demo front and center; honest numbers; doc map
   (half day).
4. Fix the four self-contradicting headers (blocker #5 + HANDOFF TL;DR +
   ROADMAP §0) (1 h).
5. Banner the internal docs; move handoffs to `docs/handoffs/` (1 h).

**Day 2 — MUST (before announcing):**
6. CI: ubuntu + windows `cargo test`, a gcc `c-oracle` job, and the bootstrap
   fixed-point job. **The Linux c-oracle run IS the cross-OS canary
   verification** — record the digest in FP-DETERMINISM-CONTRACT.md, or scope
   the announcement wording down if it diverges (2–3 h + run time).
7. Tag `v0.1.0-research`, `CHANGELOG.md`, `Cargo.toml` version check (1 h).
8. `examples/README.md` index (2–3 h).
9. Commit `examples/cpp_compare/` + the microbenchmark, or delete the 0.985×
   figure everywhere (1 h either way).
10. `src/dharht.rs` attribution paragraph (15 min).

**SHOULD (within the month, fine to land after the announcement):**
* The escape-checker guarantee page; the design-status matrix; rewrite
  docs/TESTING.md as the verification ladder; `CITATION.cff`; the
  duplicate-variant-name diagnostic; promote the cost models into
  docs/attributes.md; the `jc` POSIX fixes (+ seed refresh); the technical
  report/blog post itself.

**LATER (post-release backlog, unchanged):**
* `#pragma STDC FP_CONTRACT OFF` emission (two-sided tax); the `#line` port +
  loader unification (closes the three module-path divergences and tightens the
  scoping caveat away); macOS/clang canary beyond the Linux run; dyn-fallible
  dispatch, owning payloads, named sets, multi-statement arm bodies.

## Standing rules that apply to this work

The two-sided tax does NOT apply to docs, LICENSE, CI, or tags — which is why
two days suffices. It DOES apply to the `jc` POSIX driver fixes and the
`FP_CONTRACT` pragma (port mirror + corpus + `REFRESH_SEED` in the same
increment). The duplicate-variant diagnostic is check-only (no emission change,
no mirror due until enforcement affects a corpus file). And the release-day rule
from the skeptic, worth pinning: **every claim in the README states its scope
and its verification command** — the repo's credibility asset is that its
internal docs never overclaim; the public docs must inherit that property, not
dilute it.

## One-line

The panel's unanimous read: the science is release-grade and unusually
well-evidenced — a 148-file byte-identical dual implementation, a self-hosting
fixed point, and a gcc-only bootstrap anyone can verify in two commands — and
what stands between the repo and a public `v0.1.0-research` is two days of
scaffolding: a license, a front door that states the claims at their true
scope, one Linux run that turns the determinism headline from single-platform
to proven, a scrub of personal data, four stale doc headers, and CI that lets
strangers check the evidence themselves.
