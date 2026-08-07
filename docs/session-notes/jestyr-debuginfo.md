> **Internal development log** — kept for provenance. Status lines and counts in this file reflect the moment it was written, not the current state. Start at the repo [README](../../README.md) for current status and verified claims.

# Jestyr — Debug-info workstream: session summary + handoff

**Status: DONE (all planned increments a+b+c landed on `master`, green, warning-clean).**
Debug info (`#line N "file.jtr"` → DWARF + `-g`) was workstream §1 of the
systems-and-verification handoff (`jestyr-systems-handoff.md`). It is the cheapest,
highest-ROI item and is now complete. The remaining systems workstreams are
untouched and described at the bottom.

## What shipped (3 commits on master)

- `eae2d6f` debug-info(a): per-function `#line`→DWARF + `-g`
- `2b9669d` debug-info(b): per-statement `#line` directives
- `d314389` debug-info(c): contract asserts point at the `.jtr` clause

The emitted C now carries `#line N "file.jtr"` directives and the cc is invoked
with `-g`, so gdb/lldb/perf/Valgrind map the binary back to Jestyr source instead
of generated C. Verified end-to-end with gcc on `examples/contracts.jtr`: directives
map to the real `fn`/statement/contract lines and the program output is byte-identical.

## Design / key decisions (so you don't re-derive them)

- **Seam: `TypeInfo.debug: DebugInfo`** (`src/types.rs`). `cgen::emit(ast, info)`
  has ~30 call sites, so threading a `Modules` arg through it was a non-starter.
  Instead the per-region source tables (`paths`/`srcs`/`bases`, copied from
  `Modules`) ride inside `TypeInfo`, which has exactly **one** construction site
  (`src/typeck.rs`, in `check_program`, which already holds `&Modules`).
  `DebugInfo::span_to_file_line(span) -> Option<(&str, u32)>` resolves a global
  span region-first (so an imported module gets its own file+line), then
  `span::line_col` on that region's source.

- **Empty-debug fallback = the byte-identity guarantee.** The single-file
  unit-test path (`typeck::check` → `Modules::single`) leaves `srcs` empty, so
  `span_to_file_line` returns `None` and **no `#line` is emitted**. Every cgen unit
  test, every proptest using `typeck::check`, and the pinned `attest` golden
  (`docs_demo_manifest_is_pinned`) are therefore byte-identical and untouched.
  Only the real loader path (`check_program`, used by build/run/emit-c) emits `#line`.

- **`-g` is SEPARATE from `CC_FLAGS`** (`src/main.rs`: `DEBUG_FLAG` + `cc_base_flags()`).
  `CC_FLAGS` is the FP-determinism seam *and* the `attest` provenance (the manifest
  pins exactly those four flags, and the golden hardcodes them). `-g` is a usability
  flag — it changes the binary's debug sections but not the emitted C or its hash —
  so folding it into `CC_FLAGS` would corrupt the attest provenance. Kept apart;
  `debug_flag_is_carried_and_separate_from_the_determinism_seam` locks this.

- **Path normalization.** `#line` paths are normalized to forward slashes
  (`path.replace('\\', "/")`) so a Windows `C:\a\b.jtr` is not read as C string
  escapes (`\a`, `\b`). Property `emitted_directives_are_well_formed` + the
  Windows-path unit test pin this.

- **Per-statement (b): dedup via `Cgen::dbg_last`.** `mark_line(span)` emits a
  directive only when the resolved `(path, line)` differs from the last one, so a
  run of statements on one physical line costs one directive. `dbg_last` is reset
  to `None` at each function entry so the entry directive always fires.
  **Gotcha handled:** the tail statement of a value-returning body is emitted as a
  `return` directly in `emit_fn_body`/`emit_body` (the `last && ret` branch),
  **bypassing `emit_stmt`** — both branches now `mark_line` the tail too, else the
  tail `return` inherits the previous line. (This was caught by a failing test
  before the fix — the natural teeth.)

- **Contracts (c).** `mark_line` precedes each `requires` assert (`emit_fn_body`)
  and each `ensures` assert (`emit_value_return`), so a contract failure's `assert`
  blames the `.jtr` clause line.

## Reproducibility subtlety you must know

`#line` makes the emitted C a function of the **invocation path**, exactly like every
C toolchain (gcc/clang bake whatever path you pass; reproducible builds add
`-fdebug-prefix-map`). Real builds (`jestyrc build examples/foo.jtr`) store the
relative path as-passed and stay reproducible. This broke four `modules_props`
determinism property tests that wrote each build to a **uniquely-counter-named** temp
dir and compiled twice — the two builds legitimately differed in their baked path.
Fixed *without weakening the contract* (still full byte-identity, not "modulo #line"):
`pipeline_multi_twice` compiles both builds from **one** directory
(`materialize` + `compile_dir` split out of `pipeline_multi`). If a future workstream
wants location-independent output, the right move is a `-fdebug-prefix-map`-style
path-prefix remap, not weakening determinism.

## Tests (all toolchain-free except the manual e2e)

- wiring (`cgen::tests`): `emits_line_directives`,
  `line_directive_points_at_the_imported_file`, `per_statement_line_directives`,
  `line_directives_dedup_within_a_line`, `contract_asserts_point_at_the_clause`,
  `line_directives_are_purely_additive`; (`main.rs`)
  `debug_flag_is_carried_and_separate_from_the_determinism_seam`.
- unit: `span_to_file_line_maps_offsets` (first/last byte, newline boundary,
  second region at nonzero base incl. the loader's `\n` separator, out-of-range,
  empty tables).
- property (`proptests::debuginfo_props`): `debug_info_is_purely_additive`
  (behavioral invariance — the star), `line_numbers_are_in_range`
  (1..=file_line_count), `debug_emit_is_deterministic`,
  `emitted_directives_are_well_formed`.
- bolero fuzz: `fuzz::fuzz_line_directives` (total, never malformed).
- teeth: an off-by-one in `span_to_file_line` fails 4 tests incl. the in-range
  property; the missing tail `mark_line` fails `per_statement_line_directives`.
  Both witnessed then reverted/fixed.

## Files touched

- `src/types.rs` — `DebugInfo` struct + `span_to_file_line`; `TypeInfo.debug` field.
- `src/typeck.rs` — populate `debug` from `modules` at the one `TypeInfo` ctor.
- `src/cgen.rs` — `mark_line`/`stmt_span`/`dbg_last`; per-fn, per-stmt (incl. both
  tail paths), and contract-assert directive emission; the cgen tests above.
- `src/main.rs` — `DEBUG_FLAG`, `cc_base_flags()`, wire `-g`; the seam test.
- `src/proptests.rs` — `debuginfo_props` module, `fuzz_line_directives`,
  `pipeline_multi_twice`/`materialize`/`compile_dir` refactor + 4 determinism tests.

## Anchors (verify — they drift)

- `DebugInfo` / `span_to_file_line`: `src/types.rs` (search `DebugInfo`).
- `mark_line` / `stmt_span` / `dbg_last`: `src/cgen.rs` (search `fn mark_line`).
- per-fn directive: `emit_fn` (search `self.mark_line(f.name.span)`).
- tail paths: the two `if last && ret {` blocks in `emit_fn_body`/`emit_body`.
- contract asserts: `for r in requires` (emit_fn_body) and `for post in
  self.cur_ensures.clone()` (emit_value_return).
- `-g`: `src/main.rs` `DEBUG_FLAG` / `cc_base_flags` / `fp_contract_tests`.

## NOT done — leftovers for this workstream (all optional / future)

- **`-fdebug-prefix-map`-style path remap** for location-independent (reproducible-
  across-machines) debug paths. Only needed if absolute-path provenance becomes a
  problem; real relative-path builds are already reproducible.
- The `#line` path is whatever the loader stored (relative if invoked relative).
  No `--debug`/`--no-debug` toggle exists — debug info is always on for the loader
  path; add a flag if a "no #line" build is ever wanted (the machinery already
  supports it: empty `DebugInfo` ⇒ no directives).

## Remaining systems workstreams (untouched — next in the handoff's ROI order)

From `jestyr-systems-handoff.md`, in dependency/ROI order:
2. **Cross-compilation** — `--target <triple>` via `zig cc` (FP flags preserved);
   generalize `find_c_compiler` → `select_cc`. Note: now that `-g`/`cc_base_flags`
   exists, thread the target into `cc_base_flags`/the cc invocation the same way.
   Touches `main.rs` (the same cc-invocation seam this workstream just used).
3. **Memory-layout pass (workstream L)** — pass large `read` aggregates by `const*`,
   then field reorder for native structs, then enum niche-packing. Pure optimization;
   the whole correctness story is the behavioral-invariance property.
4. **`@verified` (workstream M)** — straight-line-integer WP→SMT(Z3) slice that elides
   proven asserts; load-bearing test is the soundness property. The contract-assert
   `#line` from increment (c) pairs nicely here: a *failed* static proof can point at
   the same `.jtr` clause.

## Discipline reminder (unchanged)

Every increment stays `cargo test`-green + warning-clean; toolchain-gated tests
behind a cargo feature; teeth-verify by mutation; one green commit per increment to
`master` (`git commit -F <file>`, `Co-Authored-By: Claude Opus 4.8`); examples stay
byte-identical (here: guaranteed by the empty-debug fallback on the single-file path).
