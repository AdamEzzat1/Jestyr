> **Internal development log** — kept for provenance. Status lines and counts in this file reflect the moment it was written, not the current state. Start at the repo [README](../../README.md) for current status and verified claims.

# Handoff — Remaining Blockers to Self-Host Jestyr

> Written 2026-06-29. The goal: rewrite the Jestyr compiler in Jestyr (ROADMAP
> workstream **P**, the Motley prerequisite). This note enumerates **every remaining
> blocker**, each with a concrete fix approach and the **full test scaffolding it must
> ship** before it counts as done. Read with `ROADMAP.md` (§P, §B), `HANDOFF.md`,
> `DROP-ALLOC-PHASE3.md`, `jestyr-design.md` (§19 self-hosting, §2.8 drop/move), and
> the companion `jestyr-session-summary-2026-06-29.md`.

---

## 0. Non-negotiable discipline (applies to every item below)

Every increment that fixes a blocker MUST, before it is committed:

1. **Stay `cargo test`-green and warning-clean** — default build *and* the
   `--features dharht-experiment` and `--features bench-alloc` builds.
2. **Ship four test layers** (the repo's house style — see `src/proptests.rs` and the
   `module.rs`/`typeck.rs`/`cgen.rs` `#[cfg(test)]` blocks for the exact shape):
   - **Unit + "plumbed-in"/wiring tests** — the feature does the narrow thing *and*
     is actually reachable end-to-end (parse → typeck → escape → cgen → emitted C, and
     where a runtime is involved, a **gcc-oracle** round-trip asserting the program's
     exit code / output).
   - **Property tests** — generate the feature over a randomized space with a new or
     extended `arb_*_program` strategy in `proptests.rs`; assert the invariant (no
     panic, determinism, type/shape preserved). Determinism props compile twice and
     assert byte-identical C.
   - **Bolero fuzz target** — a `fuzz_*` test under `mod fuzz` that drives the new
     code path on adversarial input and asserts it never panics (totality).
   - **Teeth-verify by mutation** — deliberately break the new logic, confirm a test
     (ideally the negative/wiring one) **fails**, then revert. A test that can't fail
     under mutation isn't a test.
3. **Preserve the byte-identical invariant** — any program that doesn't use the new
   feature must emit identical C (diff against the pre-change binary on the std demos +
   the type/enum-heavy examples). New language surface must be *additive*.
4. **Keep `cargo test` toolchain-free** — the gcc-oracle tests live behind
   `--features c-oracle`; the default suite must not require a C compiler.
5. **Auto-commit each green increment** (`git commit -F <msgfile>`)
   then ff-merge `master`. Rebase if `master` moved.

---

## 1. Status snapshot — what is NOT blocking

These are done + tested and a compiler can lean on them:
- Real `String` + text ops; traits A–F + `dyn`; fn-pointer types; generics +
  monomorphization; recursive ADTs (`indirect`); enums + `match` + exhaustiveness;
  error sets + `?`; allocator-as-value + **RAII for locals**.
- OS/stdlib seam: `std/fs` (file I/O), `std/env` (argv), `std/list` (growable
  `List(T)`), `std/intern` (string interner), `std/strmap` (str→i64 map), `std/core`,
  numbers/float parse-format.
- **Modules-v2 (K, ~98%)** — per-module namespaces, `mod.Type`, directory-as-module,
  content-hashing, manifest hash-verification, collidable types (incl. generic enums),
  declarative manifest. Multi-file std code no longer collides on shared helper names.
- The **front-end-in-Jestyr proof**: `examples/std/lexer.jtr` (a real lexer in Jestyr,
  gcc-oracle tested).

---

## 2. Tier 1 — the one genuine correctness blocker

### B1. Automatic drop of owned **fields** (struct fields *and* enum payloads)

**Problem.** RAII recurses into locals but **not** into aggregate fields. Verified:
`struct Holder { r: Res }` with `impl Drop for Res` emits **zero** drop calls at scope
exit; `enum Node { wrap(r: Res) }` likewise. A compiler is nothing but nested owned
data (AST arenas of `List`s, symbol tables, interner tables), so today every nested
owned field must be freed by hand — which is exactly why `std/intern` is hand-inlined
instead of containing a `StrMap`. **This is the single must-fix.**

**Fix approach.** Make the drop-glue synthesizer recursive over aggregates.
- In **cgen** (the drop-emission path; search `impl_Drop`, `_drop(&`, the scope-exit
  drop walker, and `emit_generic_drop_methods`): when emitting a value's scope-exit
  drop, after calling its own `Drop::drop` (if any), recurse into each owned field of a
  `struct`/`record` and each owned payload of the active `enum` variant, emitting their
  drops in **reverse declaration order**. For enums, switch on the tag and drop only
  the live variant's payload.
- In **escape/ownership** (`escape.rs`): a struct/enum is "needs-drop" if it has a
  `Drop` impl **or transitively contains** a needs-drop field/payload. Propagate this
  so move/borrow checking and the cgen trigger agree (don't double-drop a moved-out
  field; respect `take`/custody).
- Respect `@copy` aggregates (never dropped) and niche-optimized enums (drop the
  payload behind the niche pointer).
- **Watch:** double-drop after a field is moved out; partial moves; generic structs
  (drop glue per monomorphized instance); the existing manual frees in `std/intern`
  become redundant — leave them (idempotent? no — remove or guard them once auto-drop
  lands, and re-test `intern_demo`).

**Tests to ship.**
- *Unit/wiring (cgen):* `nested_struct_field_drops_at_scope_exit` — emit-c asserts the
  field's `_drop(&...)` call is present, in reverse field order; same for a record.
  `enum_payload_drops_only_for_the_live_variant` — the drop is under the variant's tag
  case. `moved_out_field_is_not_dropped` (no double-drop). `copy_aggregate_is_never_dropped`.
- *Wiring/gcc-oracle (`--features c-oracle`):* a program that puts a counter-incrementing
  `Drop` type inside a struct and inside an enum payload, builds a value in a scope, and
  asserts the counter hits the expected number of drops at scope exit (and exactly once).
- *Property:* extend `proptests.rs` with `arb_drop_program` — randomly nest
  `Drop`/non-`Drop` structs/enums to depth N; assert (a) the pipeline never panics, (b)
  the number of emitted `_drop(` calls equals the count of live needs-drop sub-values,
  (c) determinism (compile twice → identical C).
- *Bolero fuzz:* `fuzz_drop_glue` — feed arbitrary source through parse→typeck→cgen and
  assert the drop-glue synthesizer never panics.
- *Teeth:* disable the field-recursion branch → `nested_struct_field_drops_at_scope_exit`
  fails; restore. Disable the variant-tag guard → `moved_out`/`live_variant` fails.
- *Byte-identical:* programs with no needs-drop fields emit identical C.

**Docs.** Update `DROP-ALLOC-PHASE3.md` + `jestyr-design.md §2.8` to state RAII now
recurses into fields/payloads; add an `examples/drop_nested.jtr` demo. Remove the
"no struct-field auto-drop" caveat from `ROADMAP.md` §P/§B and the memory.

---

## 3. Tier 2 — design constraints & ergonomic gaps (workarounds exist)

### B2. Symbol-table values beyond `i64` (`name → Decl/Type`)

**Problem.** `std/strmap` is `str → i64` only; resolve/typeck want `name → arbitrary`.
**Two acceptable resolutions — pick one and commit to it before the port:**
- **(a) Discipline (cheap, already supported):** intern every name → dense id
  (`std/intern`), keep per-table `List(V)` arrays indexed by that id (the rustc
  `Symbol` pattern). No new language feature; just a convention.
- **(b) `StrMap(V)` / generic hashmap:** a generic-value open-addressing map. New std
  code; depends on **B1** (its `V` values may be drop-having) and on generic structs
  with `Drop` glue per instance.

**Tests to ship (if (b)):** unit (put/get/has/len/grow over `i64`, a struct `V`, and a
`Drop`-having `V`); gcc-oracle (insert N, read back, assert sum + exactly-N drops at
teardown); property `arb_strmap_ops` (a model `Vec<(k,v)>` oracle — final state matches
a reference map; determinism); bolero `fuzz_strmap` (random op sequences never panic);
teeth (break the probe sequence → model-mismatch test fails). **Docs:** `CORE-STD-PHASE3.md`
+ a `std/strmap.jtr` doc-comment block + an `examples/std/strmap_generic_demo.jtr`.

> Recommendation: ship **(a)** as the self-host path (zero risk, proven), and treat
> **(b)** as a post-self-host nicety.

### B3. Recoverable file I/O — `read_file -> String !IoError`

**Problem.** `read_file` aborts on failure; a compiler wants to *report* a missing file,
not crash. **Fix:** add a fallible intrinsic variant (the four-point seam: prelude C fn
+ `emit_call` arm + `is_intrinsic` + typeck return type `String !IoError`), wrapped as
`fs.try_read_text`. **Tests:** unit (typeck infers `String !IoError`; cgen lowers the
result struct); gcc-oracle (read an existing file → ok; a missing path → the `err`
branch, no abort); property over arbitrary paths (never panics); bolero `fuzz_fs_try`;
teeth (force the ok-branch unconditionally → the missing-file test fails). **Docs:**
`std/fs.jtr` doc-comment + ROADMAP §P plumbing line flipped to done.

### B4. `unsafe {}` as a `let`/`var` initializer

**Problem.** `unsafe { … }` is only valid in statement/return position, so
`let x = unsafe { … }` is rejected; today you route through a tail-`unsafe` helper fn.
**Fix:** allow `unsafe`-block as an expression in initializer position (parser +
typeck: an `unsafe` block already yields its tail expression's type; just permit it as
an `ExprKind` in initializer context). **Tests:** unit (parse + typeck of
`let x = unsafe { p.* }`); wiring (emit-c lowers it identically to the helper form);
property `arb_unsafe_init`; bolero `fuzz_unsafe_blocks`; teeth (reject it again → the
new positive test fails). **Docs:** `jestyr-design.md` unsafe section + remove the
caveat from ROADMAP §P / memory.

### B5. Inline `slice(u8, buf, n)` into `from_utf8(...)` mis-types as `int`

**Problem.** A `slice(...)` builder passed straight into `from_utf8(...)` mis-infers its
temp as `int`; the workaround is binding a typed `let vs: []u8` first. **Fix:** in
typeck, give the `slice(...)` builtin its proper `[]T` result type in argument position
(don't fall back to `int` when the element type is recoverable from the buffer
expression). **Tests:** unit (typeck of the inline form yields `[]u8`); wiring
(emit-c); property `arb_slice_builder`; bolero; teeth (revert → the inline-form test
fails). **Docs:** note in `HANDOFF.md` / numerics/text notes.

### B6. `Self { … }` literals + fallible methods (`fn push() !{…}`) — comfort, not a blocker

**Problem.** `examples/vec.jtr` is blocked on these two B-stream gaps. **But
`std/list.jtr` already provides a working growable collection**, so the port does not
depend on `vec.jtr`. Do these only if you want the nicer `Vec`. If done, each ships the
standard four layers + the `examples/vec.jtr` gcc-oracle round-trip as the wiring test,
and updates `docs/structs-enums-design.md` + ROADMAP §B.

---

## 4. Tier 3 — the dominant cost: the port itself (~27K lines)

The bootstrap compiler is **31,871 lines** of Rust (`cgen.rs` ≈ 9.6K, `typeck.rs` ≈
4.4K, `parser.rs` ≈ 2.6K, `escape.rs` ≈ 1.4K, `module.rs` ≈ 1.5K). The port is the
gate; stage it the way the compiler is layered, and at **each stage** keep the
Jestyr-written component diff-equal to the Rust one on a shared corpus.

### P1. Finish the lexer (it is currently a *subset*)
`examples/std/lexer.jtr` handles a Jestyr subset. Extend to the **full token set**:
float literals (incl. hex/exponent), hex/binary/octal ints, block comments (nesting),
string + char literals with escapes, and **every** operator/punctuator in `token.rs`.
- *Wiring/golden:* lex each `examples/**/*.jtr` and assert the Jestyr lexer's token
  stream matches the Rust lexer's (a small `token-dump` mode on both, diffed — a
  cross-implementation golden).
- *Property:* `arb_token_soup` — random whitespace-separated lexemes; the Jestyr lexer's
  classification matches the Rust lexer's. *Bolero:* `fuzz_jestyr_lexer` over arbitrary
  bytes — never panics, never loops. *Teeth:* corrupt one token rule → the golden diffs.

### P2. Parser → P3. typeck/resolve → P4. escape → P5. cgen
Port each pass; the wiring test for each is **cross-implementation equivalence** on the
corpus (Jestyr-`<pass>` output ≡ Rust-`<pass>` output: AST dump, then resolved-type
dump, then emitted C). Property tests reuse the existing `arb_*_program` strategies fed
to *both* implementations. Bolero fuzz each pass for totality. Teeth: mutate a lowering
rule → the cross-impl golden diffs.

> This is where **module content-hashing + `attest`** earn their keep: they make
> "same input ⇒ same output" mechanically checkable across implementations and stages.

---

## 5. Tier 4 — risks & the bootstrap/fixpoint validation

### R1. Bootstrap-compiler scaling to a 27K-line input (unknown)
The largest Jestyr program compiled so far is the lexer slice. A 27K-line input will
likely surface codegen/perf/edge bugs small tests never hit. **Mitigation:** grow a
"large input" corpus early (concatenate the std + examples into ever-bigger single
modules) and add a `huge_program_compiles` test; profile cgen if compile time bites.

### R2. The self-host fixpoint test (the acceptance criterion)
Self-hosting is *proven* by a 3-stage fixpoint:
- **stage-1:** Rust compiler builds the Jestyr compiler (`jc1`).
- **stage-2:** `jc1` builds the Jestyr compiler from the same source (`jc2`).
- **stage-3:** `jc2` builds itself (`jc3`).
- **Assert `jc2` ≡ `jc3` byte-for-byte** (the emitted C, and the final binary).

**Ship this as a gated CI test** (`--features selfhost-fixpoint`, outside the default
toolchain-free suite): a script/test that runs the three stages and diffs. Use the
module **content-hash** of the compiler's own source as the cache key, and O's `attest`
to pin the stage-2/stage-3 artifacts. *Teeth:* perturb one byte of a stage input →
the fixpoint diff fails.

---

## 6. Recommended critical path (ordering)

1. **B1 — field/payload auto-drop.** The only true correctness blocker; everything
   downstream is memory-safe once it lands. Do it first, with the full four-layer suite.
2. **B2(a) decision** — adopt the intern+id-indexed-arrays discipline (no code), or
   schedule `StrMap(V)` if you prefer maps. Don't start the port until the table
   strategy is fixed.
3. **B3 / B4 / B5** — small, independent ergonomic fixes that make the port pleasant
   (recoverable IO, `unsafe` initializers, slice typing). Each is a tidy four-layer
   increment.
4. **P1 lexer → P2 parser → P3 typeck → P4 escape → P5 cgen** — the port, each stage
   gated by cross-implementation equivalence on a shared corpus, bootstrapping
   incrementally.
5. **R2 fixpoint test** — stand it up as soon as a stage-1 self-build exists (even
   partial), so regressions are caught from day one.
6. *(Optional, post-self-host)* B6 (`vec.jtr`), `StrMap(V)`, generic-struct collisions,
   executable `build.jestyr`.

---

## 7. Quick reference — where each fix lives

| Blocker | Primary files |
|---|---|
| B1 field/payload drop | `cgen.rs` (drop glue), `escape.rs` (needs-drop propagation), `examples/drop_nested.jtr` |
| B2 generic map | `examples/std/strmap.jtr` (+ generics/Drop), or pure convention |
| B3 recoverable IO | `cgen.rs` (intrinsic seam), `typeck.rs` (return type), `examples/std/fs.jtr` |
| B4 unsafe initializer | `parser.rs`, `typeck.rs` |
| B5 slice typing | `typeck.rs` |
| B6 Self{}/fallible methods | `cgen.rs`, `typeck.rs`, `examples/vec.jtr` |
| P1–P5 the port | `examples/std/*.jtr` (the compiler-in-Jestyr); cross-checked vs `src/*.rs` |
| R2 fixpoint | a gated test/script + `attest` |

All four test layers for every code-bearing item go in `src/proptests.rs` (`arb_*` +
`fuzz_*`) and the relevant module's `#[cfg(test)]` block, mirroring the existing
modules-v2 tests as templates.
