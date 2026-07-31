# Jestyr — next-frontier handoff (post-self-hosting): G/L/Q

> **Cold-start orientation.** Two whole workstreams are finished and on `master`
> (`ebf8397`, **757 tests green** with `--features selfhost-fixpoint`, 700 in the
> toolchain-free default suite):
>
> * **P (self-hosting) + its productization arc** — the fixed point (jc2 ≡ jc1 on the
>   compiler's own C, all modes), the driver, in-language modules (the `ml_*` flatten
>   loader — **jc compiles itself from its real multi-file sources**), exact per-file
>   diagnostics, parse/typeck/escape refusal gates, and the committed **bootstrap seed**
>   (`bootstrap/` — building Jestyr from scratch needs only a C compiler, never Rust).
> * **O (tooling) — now COMPLETE IN-LANGUAGE**: the ported compiler emits the full
>   attestation manifest, the breaking-change diff/verify gate, and the documentation
>   page, each byte-for-byte identical to the Rust reference.
>
> **What this note now scopes is only Group 3** — the three Rust-side workstreams
> (G CTFE, L memory-layout, Q SIMD), of which **G increment 1 is done**. Section 2 is
> kept as a record of how O was closed; §3 is the live worklist.
>
> **Read alongside:** `docs/session-notes/jestyr-selfhost-P5-cgen-R2-handoff.md`
> (increments 1–54, the authoritative history + every recorded trap), `ROADMAP.md`
> workstreams G/L/Q (percentages there are stale for F/H — both are DONE — but the
> open-item descriptions are accurate), `MOTLEY.md` (the long game).

---

## Progress ledger (newest first — what landed since this note was written)

| Increment | Commit | What |
|---|---|---|
| **G6** aggregate values | *(this run)* | `Value::List` — CTFE goes from "compute a number" to "compute a **table**". Array literals/repeats evaluate; `[i]` and `.len` read. A `const` initialised by a comptime block yielding a list becomes an ordinary **static** (`static const JestyrArr_i64_8 jestyr_FIB = { { 0, 1, 1, 2, 3, 5, 8, 13 } };`) — the C compiler gets the answers, not the recursion. Emission detail with teeth: a static **must** be a brace initializer, since it cannot be initialised by the GNU statement-expression an expression-position aggregate uses; both paths exist and the test asserts no `({` reaches a static. Fuel is spent **per element**, so `[0; 10_000_000_000]` (and the nested `[[0; 100000]; 100000]`, where the *product* blows up) is a diagnostic in microseconds. |
| **G5** bounded generation | *(this run)* | `jestyrc plan … --emit`. A build script **computes the bytes of a file**; the plan records it by SHA-256, and the driver writes it only on explicit `--emit`. The evaluator gained no new power — it computed a string — so generation is a pure function whose result the *user* places, never an effect a script performs. Reproducible, attestable (hash in the plan), and bounded literally: size-capped, and an absolute or `..` path is **refused, not normalised**. A generated file can be named as a build target, so a program can be computed and compiled in one invocation. |
| **G4** `build.jestyr` | `29bd2bd` | `src/buildscript.rs` + `jestyrc plan <script> [--build]`. The build described in Jestyr and **evaluated, never run**. The plan is a *pure function of an index* (`const targets` + `fn source(i)`/`fn output(i)`) rather than the imperative `exe(…)`/`test(…)` shape, because that shape needs comptime **effects** — the one thing the ladder exists to forbid. Closes K's last leftover. `Interp::{eval_const, call_fn}` added. `examples/build.jestyr` is the canonical demo (a `.jestyr` extension, so no golden sweeps it). |
| **G3** reflection | `4a85bc6` | `@type_name` / `@field_count` / `@field_name` / `@field_type` over the **declared shape**, answered by this compiler and folded to literals. |
| **G2** comptime blocks | `b063ca4` | `comptime { … }` in user syntax, reusing the existing keyword. Typed as and emitted as the literal it folds to; **the body belongs to the interpreter alone**, so non-users are byte-identical with no gating flag. |
| *(chore)* seed refresh | `458911a` | `bootstrap_seed_is_current` was **already failing on master** — the committed flat was missing the ten import-placeholder blank lines. `jestyr_seed.c` was byte-identical, so it was cosmetic; the gcc-only bootstrap was never broken, only its drift guard. |
| **G1** CTFE interpreter | `ebf8397` | `src/comptime.rs` — a TOTAL comptime interpreter (step budget, call-depth cap, const-cycle detection, checked arithmetic). First consumer closed a silent miscompile: `[SIZE]i32` had been emitting a **zero-length** array with `assert(_ix < 0)` and no diagnostic. Zero emission change → no port mirror yet. |
| **54** `doc` renderer | `1a5ad67` | `jc <file> doc` byte-identical to `doc::generate` over 134 files; `at_guarantee_phrases` became the ONE guarantee extractor shared with attest. |
| **53** doc trivia | `1a5ad67` | `tokens.collect_docs` — finds comments by scanning the **gaps between tokens**, so the golden-pinned `tokenize` is untouched. `examples/std/doc_cli.jtr` added → corpus 134. |
| **52** attest diff/verify | `094bd6e` | `jc <old> attest-diff <new>` / `jc <file> attest-verify <manifest>` reproduce `attest::diff(…).render()` byte-for-byte, exit code included. Surfaced the `#line` divergence (§1). |
| **51** attest records | `e3c5fbe` | `jc <file> attest` emits the FULL manifest; ported `doc::{ty_str, fn_sig, extern_sig, const_sig, fn_guarantees}`. |

**Two findings from the O run worth carrying forward**, both recorded in place below:
the `#line` emission gap (§1 — invisible to every golden, so it hid for 50 increments),
and the fact that a map-free port of an ordered-container algorithm can still be exact
when the reference re-sorts its output by content (§2, O1).

### Findings from the G2–G4 run (read these before continuing CTFE)

1. ~~**`Value::List` — aggregate comptime values — is the single next unlock.**~~ —
   **DONE (G6).** It cost no totality, as predicted, *provided* fuel is spent per
   element — that one line is what separates a bounded evaluator from one that tries
   to allocate ten billion values. What it delivered: comptime **tables** as real
   statics. What it did *not* deliver on its own: tier-3 field iteration still needs
   comptime-only functions (finding 2), and building a table still means spelling out
   `[f(0), f(1), …]` because there is no comptime `for` yet — the *values* are
   computed, the *shape* is not.
2. **Field iteration is emission-blocked, not L-blocked.** The evaluator can already
   walk a struct by recursion (`if i >= @field_count(T) …`), and that works *inside*
   the interpreter. What stops it end-to-end is that a top-level `fn` is also emitted
   as ordinary runtime code, where the index is a parameter and the query cannot fold.
   The fix is **comptime-only functions** (a body instantiated at comptime, never
   emitted) — not the layout pass. Worth knowing before anyone waits on L for it.
3. **`size_of`/`align_of`/`offset_of` are C-deferred**, lowering to `sizeof`/
   `_Alignof`/`offsetof`. This compiler never learns the numbers, so exposing them as
   comptime *values* genuinely does require **L**. That is the real L dependency; the
   declared shape (names, field types, order) needed none of it.
4. **Bare-name intrinsics are a live hazard, and the compiler's own source proves it.**
   G3's first draft put reflection beside `size_of` as ordinary identifiers — but
   `examples/std/typeck.jtr:919` declares `fn field_type(…)`, which a bare-name
   `field_type` intrinsic silently hijacks, breaking the self-hosted build. Reflection
   moved to the **`@` namespace** (`@field_type`), which was already a callable form
   (`@address(0x…)`) and cannot collide. `size_of` and friends carry the same latent
   hazard; prefer `@` for anything new.
5. **Do not add names to cgen's `is_intrinsic` list casually.** That list stops a *bare
   value reference* being read as a closure capture — and the self-hosted compiler has
   several locals named `field_count`. Adding reflection there would have broken real
   code while buying nothing (reflection is only ever called). A comment now says so
   in place.
6. **`comptime` inherited the block-led statement rule for free**, which will look like
   a bug to the next reader: `comptime { comptime { 3 } + 1 }` is a parse error for the
   same reason `unsafe { unsafe { 3 } + 1 }` is — at statement position a block-led form
   parses as the block alone so a trailing operator cannot extend it. Nest in *value*
   position instead. Pinned in `comptime::tests`.
7. **A computed string needs real C escaping.** A source literal is passed through
   verbatim (Jestyr's escapes are C's), but a comptime-*computed* string has no source
   text. Two C rules bite: hex escapes are maximal-munch (`"\x41" "1"` → `\x411`), so
   non-printables use fixed-width three-digit octal; and `-std=c11` still honours
   trigraphs, so a literal `?` must be escaped. `c_string_literal` in cgen.rs, round-
   tripped through gcc. Note the NUL case can only be checked by *length* —
   `printf("%.*s")` stops at a NUL whatever precision it is given.
8. **Unit tests in the binary crate have no `CARGO_BIN_EXE_*`.** That is an
   integration-test variable. To invoke `jestyrc` as a subprocess from `proptests.rs`,
   walk up from `std::env::current_exe()` (`target/<profile>/deps/` → `../`).

---

## 0. Discipline (unchanged — every increment)

- `cargo test` green (**700 default**, 757 with `--features selfhost-fixpoint`) +
  warning-clean; cross-impl goldens behind `--features c-oracle`; the
  fixpoint/self-build/seed family behind `--features selfhost-fixpoint`.
  **Auto-commit each green increment to `master` + `git push origin master`**
  (`git commit -F <file>`, Co-Authored-By trailer).
- One construct per increment with its golden slice. Never a big drop.
- **THE TWO-SIDED TAX (new since self-hosting — this is the thing that silently breaks):**
  any change to emitted C, an intrinsic, or a pass now has TWO implementations — the Rust
  reference (`src/*.rs`) and the port (`examples/std/*.jtr`). The full gate is:
  1. `jestyr_cgen_matches_reference` — **134** corpus files byte-identical;
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

- **The self-hosted compiler** `examples/std/cgen.jtr` (**~12.7K lines**) + its 10 imports
  (`mem, intern, fs, env, list, tokens, parser, typeck, escape, sha256`). Full CLI:
  * `jc <file>` — raw C dump, the UNGATED golden mode (error files must keep emitting
    degenerately, so **never** add a refusal gate here);
  * `test [substr]` / `list [substr]` — the `@test`/`@bench` harness and its listing;
  * `build` / `run` — module-loading, refusal-gated, gcc-driving product modes;
  * `attest` — the FULL manifest (header + per-item records);
  * `<old> attest-diff <new>` / `<file> attest-verify <manifest>` — the breaking-change
    gate, exit 1 on a breaking change;
  * `doc` — the Markdown API page (single-file, typeck-free).
- **In-language modules**: the `ml_*` flatten loader (DFS deps-first, token-level
  rewrite, cross-module collision renames `name__<stem>`, `(merged, allsrc)` checkpoint
  map for exact per-file diagnostics).
- **The `_cli` split**: a module with `main` cannot be imported, so every pass that needs
  both a library and a dump driver has one — `parser_cli`, `typeck_cli`, `escape_cli`,
  `doc_cli`. Reach for it rather than adding a dump mode to `cgen.jtr`.
- **Bootstrap**: `bootstrap/jestyr_flat.jtr` (20,857 lines) + `jestyr_seed.c`
  (**31,658 lines**) + README; gcc-only build verified live, seed self-reproduces
  byte-for-byte.
- **Driver v1 limits already recorded** (P5 handoff, increments 44–50): no
  `-Wl,--stack` in driver gcc invocations; exe suffix fixed `.exe`; cmd.exe wants
  backslash paths for `run`; some item-level parse malformations recover Error-node-free
  and degrade to gcc; refusal messages are generic (location exact).
- **`#line` — the one KNOWN C-emission divergence** (surfaced by increment 52's
  `attest-verify`, recorded here because nothing else exposes it): the reference's
  module path (`module::load` → `TypeInfo::debug` → `mark_line`) emits `#line <n>
  "<file>"` directives, and **the port emits none**. No existing golden sees it — all
  134 corpus goldens, the concat, and the fixpoint use debug-free single-file/merged
  ASTs — but it means `jestyrc attest <f>` and `jc <f> attest` disagree on `c-sha256`
  (the reference hashes the `#line`-bearing C; `examples/api_v1.jtr`-shaped files show
  27 such lines), and port-built binaries carry no source-line mapping. `jc attest` /
  `attest-verify` are self-consistent, so the drift gate works — it's only cross-tool
  hash comparison that can't match. **Its own increment when wanted:** the port already
  has the input it needs (the `Ml.map` checkpoint pairs give per-file line/col), but
  there is NO golden for the module path's C today — build that first (`jestyrc emit-c`
  vs `jc <file> build`'s `.c` over a multi-module fixture), then port `mark_line`'s
  placement + dedup, then refresh the seed (the seed's C would gain the directives).

## 2. O-tooling — ✅ COMPLETE (kept as the record of how, not as a worklist)

> Nothing here is outstanding. It stays because the *how* is reusable: both items were
> ports of a Rust pass into `.jtr` under the two-sided tax, and the notes below name the
> arena layouts, the sharing invariant, and the traps that cost time.

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
2. ~~`comptime { … }` blocks~~ — **DONE (G2, `b063ca4`)**, reference side. Reuses the
   existing `comptime` keyword (it could never start an expression, so no new reserved
   word and no changed parse). Typed as and emitted as the literal it folds to. **No
   `comptime const`** — top-level `const` is already comptime-evaluated, and a second
   way to say it violates §8. The design point: *the body belongs to the interpreter
   alone* — cgen/escape/attrs/dharht never descend into it, which is why non-users are
   byte-identical with no gating flag and why "typechecks" cannot disagree with
   "evaluates". **No corpus file yet, deliberately** (see the port-mirror trigger below).
3. ~~Reflection~~ — **DONE (G3, `4a85bc6`)**, reference side, *declared shape only*:
   `@type_name(T)`, `@field_count(T)`, `@field_name(T, i)`, `@field_type(T, i)`.
   Answered by this compiler and folded to literals; field types render through
   `doc::ty_str`, so reflection cannot drift from the docs. Arguments must be
   compile-time constants — see findings 2–4 above for what that excludes and why.
4. ~~**The executable `build.jestyr`**~~ — **DONE (G4)**, closes K's last leftover.
   `jestyrc plan <script> [--build]`. The plan is a **pure function of an index**
   (`const targets` + `fn source(i)`/`fn output(i)`), not the imperative
   `exe(…)`/`test(…)` shape, because that needs comptime effects. Determinism is
   structural: a non-deterministic build script cannot be *written*, since the
   evaluator has no arm for a clock or an environment read. `examples/build.jestyr` is
   the demo; a `.jestyr` extension means no golden sweeps it.
5. ~~**Tier 5 — bounded generation**~~ — **DONE (G5)**, foundation laid.
   `jestyrc plan … --emit`: a script *computes* an artifact's bytes, the plan records
   it by SHA-256, the driver writes it only on demand. The design point is where the
   boundary sits — **the evaluator gained no new power**; it computed a string, and the
   *driver* places the file. So generation is a pure function the user chooses to
   apply, not an effect a script can perform. Reproducible, attestable, and bounded
   literally (size cap; an absolute or `..` path is refused rather than normalised).
   Deliberately *artifact* generation, not source-string injection: what a script
   produces is a file you can read, diff and hash before anything acts on it.

**The outstanding CTFE work, in the order it should be done:**

| # | Work | Notes |
|---|---|---|
| 1 | **Comptime `for`** | the natural partner to G6: today a table's *values* are computed but its *shape* is spelled out (`[f(0), f(1), …]`). Bounded by the same fuel budget — spend per iteration, exactly as `ArrayRepeat` now does |
| 2 | **Comptime-only functions** | unblocks tier-3 field *iteration* end-to-end (finding 2). Either this or (1) closes the "walk a struct's fields" story |
| 3 | **Port mirrors for G2/G3/G6** | the big one — see below |
| 4 | `@size_of`/`@align_of`/`@offset_of` as comptime values | after **L** (finding 3) |

**On the port mirrors (item 3).** Nothing is broken today: G2–G4 changed no emitted C
for any existing program, so all 134 corpus goldens, the concat, test mode, the
fixpoint, the self-build and the seed are green *without* a mirror. The trigger is
unchanged and still the thing to respect — **the first `comptime`/reflection corpus
`.jtr` file drags in `parser.jtr` + `typeck.jtr` + `cgen.jtr` mirrors and a seed
refresh**, because the P2/P3 goldens sweep every `examples/**.jtr` with no allowlist.
The mirror is genuinely large: it means writing a total comptime interpreter in the
`.jtr` subset (G1's ~700 lines of Rust, plus the tier-2/3 surface) and threading a new
expression kind through the port's integer-tagged AST. Land it as its own increment
chain, and only when a corpus file needs it. `.jestyr` build scripts are exempt — the
goldens key on the `jtr` extension.
**Port impact:** each comptime construct that changes emitted C needs its cgen.jtr
mirror + golden growth; constructs that only FOLD at check time still need typeck.jtr
parity (the P3 golden compares every expression's resolved type).

### L. Memory-layout pass (0% — where systems performance lives)
size/align computation, **field reordering**, **enum niche-packing**, and
pass-large-aggregates-by-`const*` (today `read` params copy).
**The byte-identity constraint is the whole game here:** reordering fields changes the
emitted C for every existing program, which would invalidate 134 golden files, the
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

**Done:** ~~O1 records~~ (51–52) → ~~O2 doc~~ (53–54) → **workstream O complete** →
~~G1 the comptime interpreter~~ (`ebf8397`) → ~~G2 `comptime` blocks~~ (`b063ca4`) →
~~G3 reflection~~ (`4a85bc6`) → ~~G4 `build.jestyr`~~ (`29bd2bd`) → ~~G5 bounded
generation~~ (`bce5456`) → ~~G6 aggregate values / comptime tables~~ — **the CTFE tier
ladder (0–6) is done on the reference side**, and documented in `docs/ctfe-tiers.md`.

**Next:** a comptime `for` (so a table's *shape* can be computed, not just its values)
→ comptime-only functions → the **G2/G3/G6 port mirrors** → L layout 1–3 (opt-in,
byte-identity preserved) → Q SIMD. `@size_of` as a comptime *value* comes after L.

`#line` (§1) is an independent, optional increment — take it whenever the port's `build`
C needs to match the reference's, not before. Keep every increment two-sided-green:
corpus 134 + concat + test-mode + fixpoint + self-build + refreshed seed.

**The port-mirror trigger, restated because it is the thing that will bite:** G2–G4 need
no mirror *yet* only because they added no corpus file and changed no emitted C for any
existing program. The moment a `comptime`/reflection `.jtr` lands in `examples/`, the
P2/P3 goldens sweep it with no allowlist and the port must parse, check and emit it —
which means a comptime interpreter written in the `.jtr` subset. Land the reference side
and its Rust-only tests first, then the mirror, then the corpus file, in that order.
(`.jestyr` build scripts are exempt: the goldens key on the `jtr` extension.)

## One-line
Self-hosting is finished and productized, **workstream O is complete in-language**, and
**the CTFE tier ladder (0–6) is now done on the reference side** — a total comptime
interpreter that closed a silent zero-length-array miscompile, `comptime { … }` in user
syntax, reflection over the declared shape in the collision-proof `@` namespace, a
`build.jestyr` that is *evaluated, never run*, bounded artifact generation, and
aggregate values that make a computed lookup table an ordinary static. The through-line
is that **purity was never traded away to get power**: each tier added a capability
without giving the evaluator an effect, so determinism and reproducibility stayed
properties of the design rather than conventions to police. The ladder is documented in
`docs/ctfe-tiers.md`. What's left is a comptime `for` (so a table's shape can be
computed too), comptime-only functions, the **G2/G3/G6 port mirrors**, opt-in memory
layout, SIMD, and the optional `#line` port — each landed
increment-by-increment under the two-sided golden discipline with the bootstrap seed
refreshed at every `examples/std` change.
