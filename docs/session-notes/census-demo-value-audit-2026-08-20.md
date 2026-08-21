# Was the `census` demo worth building? — a readiness-style value audit

**Commit:** `cdf547a`, clean tree. **Host:** Windows 11, gcc 8.3.0 (Strawberry), rustc via
`cargo --release`. **Evidence:** `…/scratchpad/audit/` — `coverage.txt`,
`jc_build_matrix.txt`, `mechanism.txt`, `imports.txt`, `env.txt`.

The question is not "does census work" (it does; gate 1265/0/3). It is what building it
bought, and whether each claimed benefit survives an attempt to kill it.

## Bottom line

**The artifact is worth a little. The measurement it provoked is worth a lot.**

Census's own bugs were census's own fault and taught the project nothing directly. What it
actually did was make someone run `jc <file> build` — a code path with **no systematic gate**
— and that produced a confirmed compiler defect with a four-control mechanism, three
independent instances, and a 17%-of-corpus blast radius that had been sitting unnoticed in
four pre-existing files.

Had I built census and *not* tried to build it with the self-hosted compiler, the honest
value would have been close to zero.

## Role matrix

| Role | Readiness | What this seat found | Evidence |
|---|---|---|---|
| Release engineer | `ENFORCED` | Nothing new. Seed provenance, gcc-only bootstrap and fixpoint are already gated in `.github/workflows/ci.yml:74-88` | — |
| Portability engineer | `N/A` | Not exercised — census adds no new C constructs | — |
| Performance engineer | `N/A` | Deliberately empty. Nothing was timed and no perf claim was made or should be | — |
| **Reliability engineer** | **`ENFORCED` (was `ABSENT`)** | **The finding, and it is now closed.** The only gate that built programs end-to-end, `selfhost_fixpoint_subset`, `continue`s on any file containing `import "`; 52 runnable multi-module corpus programs were outside every gate and 9 did not build. The gate this row asked for exists as `jc_build_matrix_matches_expectations` + `docs/jc_build_matrix.txt`, and the loader bug behind 5 of the 9 is fixed — **49 of 53 build now, and the remaining 4 are one isolated mechanism**. See `jestyr-std-v4-runtime-platform-handoff.md` §2.2/§2.3 | `docs/jc_build_matrix.txt` |
| Supply-chain engineer | `N/A` | No new dependencies; census is pure Jestyr | — |
| **DX / diagnostics engineer** | `CLAIMED` | Two API-shape findings, both low severity, both from census's own bugs: `walk`'s visitor must test `is_dir` with nothing prompting it, and `memprof` read before scope exit reports live arenas as a leak | this session |
| API steward | `VERIFIED`, **constraint LIFTED** | `len` / `refused` / `lost` are simultaneously public function names and struct field names across modules. That overlap was load-bearing for the port; it no longer is — `ml_rewrite` stopped renaming binders, so the overlap is legal again. The mechanism recorded here was backwards (the field ACCESS survived; the DECLARATION was rewritten) | `mechanism.txt`, v4 handoff §2.2 |
| Adversarial reviewer | — | Demoted 2 of 5 claimed benefits; see below | this file |

## The falsification gate, applied to each claimed benefit

### B1 — "the verification loop caught three bugs" → **QUALIFIED, near-zero project value**

All three were bugs in census itself, not in Jestyr: the visitor ignoring `is_dir`, the
profile printed before scope exit, and nine wrong expectations in my own test file. A demo
finding its own bugs is evidence about the demo.

*Salvage:* two of them are weak evidence about API misuse-resistance (the `walk` visitor and
the `memprof` read point), which is why the DX row above is not empty. That is a much smaller
claim than "the loop caught three bugs" and is how it should be stated.

### B2 — "it found a real compiler bug" → **CONFIRMED, but the causal story is wrong**

The bug is real and larger than first recorded. Falsification, however: **it was already
findable before census existed.** Four corpus files — `caps_demo`, `process_demo`,
`test_fixture_demo`, `writer_demo` — fail the same way today and predate this work. Census
was the fifth instance, not the discovery.

What census actually contributed was the *prompt to run the driver*. Real, but it should not
be dressed as finding something new.

**Mechanism (Confirmed — reproduction, control, alternatives, anchors, prior art, consequence):**

> When two modules define a top-level function with the same name, the loader renames them.
> If that name is **also a struct field** accessed as `x.name` inside one of those modules,
> the rename rewrites the *field access* too, and it stops resolving.

Four discriminating controls, each a one-line module imported alongside `list`:

| Colliding name | Is a `list` generic? | Is a `List` field? | `jc … run` |
|---|---|---|---|
| `zzz_unrelated` | no | no | **OK** |
| `cap` | no | yes | **OK** |
| `make` | yes | no | **OK** |
| `len` | **yes** | **yes** | **9 errors inside `list.jtr`** |

Three independent instances in the corpus, all at the same shape:

| Name | Colliding modules | Field access that breaks |
|---|---|---|
| `len` | `list` + `json`/`strmap`/7 others | `list.jtr:29` `l.len == l.cap` |
| `refused` | `fs` + `process` + `walk` | `fs.jtr:93` `f.refused = f.refused + 1` |
| `lost` | `sink` + `test` | `sink.jtr:47` `s.lost = s.lost + 1` |

*Prior art:* the general mechanism **is** recorded — the stdlib memory notes that the
token-level flatten "cannot tell `mod.item` from a field access on a local of the same name",
learned when migrating `cgen.jtr` onto `std/path`. This is the same mechanism in a new
position (function-name vs *field* name rather than module-name vs local-name), and that
position is not recorded anywhere. Reported as a new instance, not a new discovery.

*Correction owed:* `docs/session-notes/jestyr-std-v3-diagnostics-handoff.md` currently says
this is "any program importing `std/json`". That is too narrow — `caps_demo` imports no
`json` and fails identically. The handoff should say what the table above says.

*Second, separate class:* four more files (`combinators`, `mutex`, `slice_algos`,
`try_read`) fail at **gcc**, not at the front end, with undefined types
(`Jestyr_Option__i32`, `JestyrFn_fn_di64_di64_ret_i64`). Different failure stage, mechanism
not isolated → **Suspected**, listed here so it is not lost.

### B3 — "it caught an overclaim" → **REFUTED as a benefit**

The overclaim (`@no_os` coverage) existed *in the post census generated*. No census, no post,
no overclaim. The catch was a human question, not the demo. Counting the repair of a
self-inflicted wound as a benefit is circular.

*Residue that is genuinely positive:* the repair added `@no_os` to 7 functions and a pin that
fails below 29. That would have been worth doing regardless.

### B4 — "a showcase for the standard library" → **VERIFIED as integration, UNTESTED as outreach**

Measured: `census_cli.jtr` directly imports **5 Tier 3 modules**; the next-highest corpus file
imports **2**. It is the only place several of these modules meet in one program, which is a
real integration property and is what surfaced B2.

Whether it works as *outreach* has no metric and is recorded `UNTESTED`, not claimed.

### B5 — "a permanent regression test" → **VERIFIED, modest**

`census_demo_matches_an_independent_recount` (differential recount, determinism, table-vs-JSON,
capability + control, clean profile, diagnostic exit code), `census_is_os_free_even_though_it_allocates`,
9 suite cases, 3 allowlist entries. Ongoing cost: gate time. Ongoing value: real but small —
it guards a demo, not the compiler.

## The gate this audit owes

One job, and it is the actionable output:

```
build-every-multi-module-program:
  # Defends: the `jc build` path, which selfhost_fixpoint_subset skips by construction
  # (it `continue`s on any file containing `import "`). 9 of 52 such programs fail today.
  for each examples/std/*.jtr with `fn main()` and an import:
      jc <file> build   →   record BUILD_OK / TYPE_REFUSAL / SYNTAX_REFUSAL / OTHER
  compare against a committed expectations file; fail on any change in either direction.
```

An expectations file rather than a green/red gate, because 9 are failing now and the useful
property immediately is *"this set does not grow, and shrinks only deliberately"*.

## Checked and fine

- Byte-identity, concat, fixpoint and self-build gates all pass with census in the corpus;
  `@no_os` is checker-only and changed no emitted C.
- Census's own numbers agree with `find`/`wc` exactly on two independent trees.
- No performance claim was made anywhere, so there is nothing to A/A test.
- CI already gates the gcc-only bootstrap and the seed fixpoint; release provenance is not
  where this project is weak.

## Unverified leads

- The four gcc-stage failures above (mechanism not isolated).
- Whether `walk`'s visitor signature could make `is_dir` harder to ignore without
  a language feature. Design question, not a defect.
- Whether the 43 that build also *run correctly* — this audit checked that they build, not
  that their output matches the reference-built binary.
