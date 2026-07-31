# Jestyr — next-frontier handoff (post-self-hosting): O-remainder + G/L/Q

> **Cold-start orientation.** Self-hosting (workstream P) and its productization arc are
> COMPLETE on `master` (`b928c4f`, 737 tests green): the fixed point (jc2 ≡ jc1 on the
> compiler's own C, all modes), the driver (`jc <file> build|run|test|list|attest`),
> in-language modules (the `ml_*` flatten loader — **jc compiles itself from its real
> multi-file sources**), exact per-file diagnostics, parse/typeck/escape refusal gates,
> the in-language SHA-256 + attest header, and the committed **bootstrap seed**
> (`bootstrap/` — building Jestyr from scratch needs only a C compiler, never Rust).
> This note scopes what remains: two small O-tooling items, then the three big Rust-side
> workstreams (G CTFE, L memory-layout, Q SIMD).
>
> **Read alongside:** `docs/session-notes/jestyr-selfhost-P5-cgen-R2-handoff.md`
> (increments 1–50, the authoritative history + every recorded trap), `ROADMAP.md`
> workstreams G/L/O/Q (percentages there are stale for F/H — both are DONE — but the
> open-item descriptions are accurate), `MOTLEY.md` (the long game).

---

## 0. Discipline (unchanged — every increment)

- `cargo test` green (685 default) + warning-clean; cross-impl goldens behind
  `--features c-oracle`; the fixpoint/self-build/seed family behind
  `--features selfhost-fixpoint`. **Auto-commit each green increment to `master` +
  `git push origin master`** (`git commit -F <file>`, Co-Authored-By trailer).
- One construct per increment with its golden slice. Never a big drop.
- **THE TWO-SIDED TAX (new since self-hosting — this is the thing that silently breaks):**
  any change to emitted C, an intrinsic, or a pass now has TWO implementations — the Rust
  reference (`src/*.rs`) and the port (`examples/std/*.jtr`). The full gate is:
  1. `jestyr_cgen_matches_reference` — 133 corpus files byte-identical;
  2. `jestyr_cgen_concat_matches_reference` + `jestyr_cgen_test_mode_matches_reference`;
  3. `selfhost_fixpoint_full` + `jestyr_driver_builds_itself` (the compiler must still
     compile ITSELF and must not trip its own refusal gates);
  4. `bootstrap_seed_is_current` — **whenever any `examples/std/*.jtr` source changes,
     rerun `REFRESH_SEED=1 cargo test --features selfhost-fixpoint bootstrap_seed_is_current`
     and commit the refreshed `bootstrap/` pair, else the drift guard fails.**
  New intrinsics/runtime fns must be GATED ON USE (the `uses_try_read` pattern) so every
  non-user program's C is byte-identical — and mirrored at all four port sites (gate
  helper + prelude line in cgen.jtr, helper-table entry, closure marker-name string,
  typeck.jtr return arm).
- **`.jtr` subset traps (all hit in real sessions — do not re-derive):** a `for`
  CONDITION may not start with `(` (parses as the zip-binding head — use
  `for i < n { if … { break } }`); a bare `{` block after a call-initializer parses as
  the `Name(args){…}` generic-ctor form (write flat, no scoping blocks); NEVER chain
  `string_view(x).len` (bind `let v: str` first); `out`/`comptime`/`par`/`select` are
  reserved; author `.jtr` with the Write/Edit tools, never through shell heredocs
  (backslash mangling has produced real newlines inside string literals twice).
- The deep-dive levers: `DUMP_DIVERGE=1` on any cgen golden prints the first differing
  line; `TYPECK_FILE=<basename>` prints the aligned per-expr type stream.

## 1. Current state (what exists, so you don't rebuild it)

- **The self-hosted compiler** `examples/std/cgen.jtr` (~11.5K lines) + its 10 imports
  (`mem, intern, fs, env, list, tokens, parser, typeck, escape, sha256`). CLI:
  `jc <file>` (raw C dump — the UNGATED golden mode; error files must keep emitting
  degenerately), `test [substr]` / `list [substr]`, `build` / `run` (module-loading,
  refusal-gated, gcc-driving product modes), `attest` (manifest header).
- **In-language modules**: the `ml_*` flatten loader (DFS deps-first, token-level
  rewrite, cross-module collision renames `name__<stem>`, `(merged, allsrc)` checkpoint
  map for exact per-file diagnostics).
- **Bootstrap**: `bootstrap/jestyr_flat.jtr` + `jestyr_seed.c` (28,645 lines) + README;
  gcc-only build verified live, seed self-reproduces byte-for-byte.
- **Driver v1 limits already recorded** (P5 handoff, increments 44–50): no
  `-Wl,--stack` in driver gcc invocations; exe suffix fixed `.exe`; cmd.exe wants
  backslash paths for `run`; some item-level parse malformations recover Error-node-free
  and degrade to gcc; refusal messages are generic (location exact).
- **`#line` — the one KNOWN C-emission divergence** (surfaced by increment 52's
  `attest-verify`, recorded here because nothing else exposes it): the reference's
  module path (`module::load` → `TypeInfo::debug` → `mark_line`) emits `#line <n>
  "<file>"` directives, and **the port emits none**. No existing golden sees it — all
  133 corpus goldens, the concat, and the fixpoint use debug-free single-file/merged
  ASTs — but it means `jestyrc attest <f>` and `jc <f> attest` disagree on `c-sha256`
  (the reference hashes the `#line`-bearing C; `examples/api_v1.jtr`-shaped files show
  27 such lines), and port-built binaries carry no source-line mapping. `jc attest` /
  `attest-verify` are self-consistent, so the drift gate works — it's only cross-tool
  hash comparison that can't match. **Its own increment when wanted:** the port already
  has the input it needs (the `Ml.map` checkpoint pairs give per-file line/col), but
  there is NO golden for the module path's C today — build that first (`jestyrc emit-c`
  vs `jc <file> build`'s `.c` over a multi-module fixture), then port `mark_line`'s
  placement + dedup, then refresh the seed (the seed's C would gain the directives).

## 2. O-tooling remainder (SMALL — do these first, they finish the tool story)

### O1. Attest manifest RECORDS + `verify` — **DONE (increments 51–52)**
`jc <file> attest` now emits the FULL manifest — header plus per-item records — byte-equal
to `attest::manifest` (golden `jestyr_driver_attest_manifest_matches_reference`, 10 corpus
files), via the ported `doc::{fn_sig, fn_guarantees, const_sig, extern_sig, ty_str}` family
(`at_*` in cgen.jtr). And `jc <old> attest-diff <new>` / `jc <file> attest-verify
<manifest>` reproduce `attest::diff(…).render()` byte-for-byte, exit code included
(golden `jestyr_driver_attest_diff_matches_reference`, every verdict branch). What follows
is the original plan, kept for its reference pointers:

```
<kind> <name>
  vis: pub|priv
  sig: <reconstructed signature>
  guarantee: <…>          (0..n lines)
```

- The reference collects them in `attest.rs::collect_records`; **`sig` and `guarantees`
  come from `src/doc.rs` (`doc::fn_sig`, `doc::fn_guarantees`) — a signature
  RECONSTRUCTOR from the AST, not a span slice.** Port plan: a `sig`-renderer in `.jtr`
  over the parser's `iar` param 7-tuples + ret TypeId (the port already renders C
  signatures — this is the Jestyr-syntax analogue; `fn_guarantees` reads
  `requires`/`ensures` spans (`far` extras), the error set, `@no_panic`, refined params).
- Records sort by `(kind, name)`; kinds: fn / const / struct / enum / trait / distinct
  (read `collect_records` for the exact set + struct-method handling).
- Then `verify`: `jc <file> attest-verify <manifest>` — re-render and diff (the Rust
  side is `attest --diff` in main.rs; mirror the drift-report format).
- **Golden**: full-manifest byte-compare vs `attest::manifest` over a handful of corpus
  files (contracts.jtr — requires/ensures; records.jtr — methods; errors.jtr — error
  sets), grown file-by-file like every other golden.
- **As built (notes for the next reader):** `collect_records` covers fn/const/enum/
  struct-record-union/extern only — trait/impl/distinct/import are NOT records (the C
  hash attests them), and struct METHODS never get their own record. Sorting is a
  selection sort by (kind, name) emitting each record as its slot settles. The differ
  keeps the manifest text owned by the caller and models `ParsedItem` as spans
  (`struct Am`); the `BTreeSet`/`BTreeMap` fields are re-derived from the guarantee
  lines on demand rather than stored, and set ORDER never matters because the change
  list gets the reference's total `(item, verdict, detail)` re-sort at the end — that
  one observation is what makes a map-free port exact.

### O2. `doc` in-language — **DONE (increments 53–54)**
`jc <file> doc` renders the Markdown API page byte-for-byte identical to
`doc::generate(src, stem, html=false)` over all 134 corpus files (golden
`jestyr_doc_matches_reference`), with the dangling-doc lint located exactly.

- **The blocker was cleared without touching the token stream.** `tokens.jtr` gained
  `pub struct RawDoc` + `pub fn collect_docs`, a SECOND pass that finds comments by
  scanning the **gaps between tokens** — trivia by construction, so it needs none of the
  string/char-literal handling the tokenizer has, and `tokenize` stays byte-identical.
  (The load-bearing case: `"/// not a doc /* nor this */"` inside a string literal is
  never in a gap.) Golden `jestyr_doc_trivia_matches_reference` pins kind, block-ness,
  the comment span and the text span against `Lexer::tokenize_with_docs` corpus-wide,
  plus a fixture for both demotions (`////`, `/***`), `/**/`, nested plain comments and
  a trailing doc. `examples/std/doc_cli.jtr` is its dumper (the `_cli` split again).
- The renderer lives in cgen.jtr beside attest (`dc_*` + the `at_*` sig renderers),
  which is what makes the sharing real: **`at_guarantee_phrases` is now the ONE
  extractor** — attest wraps it as `  guarantee:` lines, doc as the `- ` bullets — so
  the attested ABI can never drift from the rendered docs, exactly the reference's
  design. Added for doc: `at_struct_sig` / `at_enum_sig` (note the reference prints
  `struct` for a `record`/`union` too, and lists fields only, never methods).
- **Not ported** (deliberate, recorded): `--html` mode; the fenced-`Example` extraction
  (collected by the reference for future doctests, never rendered in Markdown — the
  fence STATE is ported, since a `#` inside a fence must not open a section); and the
  reference's snippet decoration on the dangling-doc warning (its message text and
  `file:line:col` are ported, the `|`-gutter source excerpt is not). Doc prose using
  non-ASCII whitespace would trim differently (`char::is_whitespace` vs ASCII).
- **The trap this increment cost time on:** a struct member's tag-1 (method) tuple holds
  its fn `ItemId` in slot **1**, not slot 2 (slot 2 is a field's name-span start). Reading
  the wrong slot indexes the item arena with a source offset — an out-of-bounds read that
  surfaces only as a bare `Assertion failed!` from the generated C.

## 3. Group 3 — the three Rust-side workstreams (each multi-week; nothing blocks on them)

These change the REFERENCE first; the port inherits each construct later as its own
increment chain (same two-sided tax). Recommended order: **G → L → Q** (G unlocks the
most; L wants G's comptime for layout queries; Q builds on both).

### G. CTFE + reflection (~10% — the biggest unlock)
**What exists:** generics/type-param substitution; const-expr DISCRIMINANTS evaluate;
**the comptime interpreter (increment 1 — DONE)**.
**Build (increment chain):**
1. ~~A comptime **interpreter over the AST** (`src/comptime.rs`)~~ — **DONE.**
   `Interp::{eval, eval_usize}` over `Value::{Int, Bool, Str, Unit}`: literals (all
   bases, `_` separators, escapes), unary, checked arithmetic, comparisons,
   short-circuiting `and`/`or`, bitwise/shifts, string concat + compare, `if`/`else`,
   blocks with `let`, `const` references, int-to-int casts, and calls of pure fns with
   recursion. **Totality is the design point** — a compiler that hangs on
   `const A = B; const B = A` is worse than one that errors: a step budget (`FUEL`), a
   call-depth cap, and const-cycle detection make every input terminate with a
   diagnostic. Anything outside the value domain (floats, structs, intrinsics) is a
   clean `EvalError`, never a guess.
   **Its first consumer closed a real silent-miscompile bug:** `typeck::eval_array_len`
   and `cgen::array_len` accepted only an integer LITERAL and silently returned `0`
   otherwise, so `[SIZE]i32` emitted a zero-length array, a type-mismatched
   initialization and `assert(_ix < 0)` on every access — with no diagnostic. Both now
   evaluate, and a length that cannot be evaluated is an error
   (`check_array_len` from `audit_type_id` for signatures, from the `Stmt::Let` arm via
   `check_type_array_lens` for locals — item audits never visit local annotations, which
   is what the first draft got wrong). Zero emission change: a literal folds to the same
   number, so all 134 corpus goldens, the concat (31,658 lines) and the seed are
   untouched, and **no port mirror is needed yet**. Tests:
   `comptime::tests::*` (15, incl. every totality bound) plus
   `comptime_folds_array_lengths_end_to_end` (const / arithmetic / pure-fn-call lengths
   through gcc) and `comptime_rejects_a_non_constant_array_length`.
   **Port note for later:** the moment a corpus `.jtr` uses a non-literal array length,
   the port's `typeck.jtr`/`cgen.jtr` must fold it too (the P3 typeck golden compares
   `[N]T` renderings) — keep such files out of `examples/` until that mirror lands.
2. `comptime { … }` blocks + `comptime` consts (design §8) — new syntax, new golden
   corpus files.
3. Reflection as comptime calls over type values (`@type_of`, field iteration) — the
   IR-builder ergonomics MOTLEY needs.
4. **The executable `build.jestyr`** — closes K's last leftover; the driver gains a
   `jc build.jestyr` mode that evaluates the build description comptime.
**Port impact:** each comptime construct that changes emitted C needs its cgen.jtr
mirror + golden growth; constructs that only FOLD at check time still need typeck.jtr
parity (the P3 golden compares every expression's resolved type).

### L. Memory-layout pass (0% — where systems performance lives)
size/align computation, **field reordering**, **enum niche-packing**, and
pass-large-aggregates-by-`const*` (today `read` params copy).
**The byte-identity constraint is the whole game here:** reordering fields changes the
emitted C for every existing program, which would invalidate 133 golden files, the
concat, the seed, and attest hashes at once. Land it OPT-IN:
1. A pure ANALYSIS pass (`src/layout.rs`) computing size/align/waste + a report mode
   (`jestyrc layout <file>`) — zero emission change, easy first increment.
2. `@layout(auto)` opt-in per struct → reordered emission for annotated types only
   (non-users byte-identical; golden corpus grows annotated files).
3. Niche-packing behind the same attribute; by-`const*` `read` params behind
   `@abi(ref)` or a whole-program flag — each its own increment + port mirror.
**Port impact:** every opt-in construct mirrors into cgen.jtr when its corpus file
lands; the seed refreshes.

### Q. SIMD → GPU (from ~45%)
`par_map`/`par_scan` (`std/parallel.jtr`), the `par for … reduce(r)` surface, and
`@span` (the checked work-span cost model) exist. **Check master before building ANY
parallelism feature — Q has twice discarded duplicate builds** (see the parallelism
memory + `PARALLELISM-HANDOFF.md`).
1. SIMD lowering first: a `@simd`-gated `par for` body → GCC vector extensions
   (`__attribute__((vector_size)))` or plain auto-vectorizable loops with `#pragma omp
   simd`-free portable C — keep the locked `CC_FLAGS` untouched; determinism requires
   the FP seam rules (no reassociation → integer/bitwise SIMD first, FP SIMD only
   where `-ffp-contract=off` semantics hold).
2. SOAC-library growth on `@span` (the CJC thermal/energy hookup is the differentiator).
3. GPU is after SIMD proves the model.

## 4. Sequencing (the one-line plan)

**~~O1 records~~ (DONE, 51–52) → ~~O2 doc~~ (DONE, 53–54) → WORKSTREAM O IS COMPLETE →
G CTFE increments 1–4 (biggest unlock; closes K via build.jestyr) → L layout 1–3 (opt-in,
byte-identity preserved) → Q SIMD.** `#line` (§1) is an independent, optional increment —
take it whenever the port's `build` C needs to match the reference's, not before. Keep
every increment two-sided-green: corpus 134 + concat + test-mode + fixpoint + self-build +
refreshed seed.

## One-line
Self-hosting is finished and productized and **workstream O is now COMPLETE in-language** —
the ported compiler emits the full attestation manifest, the breaking-change diff, and the
documentation page, each byte-for-byte identical to the reference; what's left is the
optional `#line` port and the three reference-side workstreams — CTFE, opt-in memory
layout, SIMD — each landed increment-by-increment under the two-sided golden discipline
with the bootstrap seed refreshed at every `examples/std` change.
