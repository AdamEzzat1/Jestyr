# Jestyr self-hosting — what REMAINS (consolidated checklist)

> One-page map of everything left to self-host the compiler (ROADMAP workstream P), so no context
> is lost between sessions. Pairs with the detailed handoffs:
> - **`docs/session-notes/jestyr-selfhost-P5-cgen-R2-handoff.md` (P5 cgen + R2 — the remaining work, authoritative)**
> - `docs/session-notes/jestyr-selfhost-P3-typeck-handoff.md` (P3, done)
> - `docs/session-notes/jestyr-selfhost-P2-parser-handoff.md` (P2, done)
> - `docs/session-notes/jestyr-selfhost-port-P2-P5-R2.md` (master plan)
>
> **Discipline (every increment):** `cargo test`-green (685 default) + warning-clean; cross-impl
> goldens behind `--features c-oracle`; keep non-users byte-identical; **auto-commit each green
> increment to `master` + `git push origin master`**; one construct per increment with its golden
> slice; teeth-verify by mutation.

## Status at a glance

| Pass | Reference | State |
|---|---|---|
| P1 lexemes / P2a kinds | `src/lexer.rs`, `src/token.rs` | ✅ DONE (token-for-token, whole corpus) |
| **P2 parser** | `src/parser.rs`, `src/ast.rs` (~2.6K) | ✅ **COMPLETE** — module golden 125/125, denylist empty |
| **P3 typeck** | `src/typeck.rs`, `src/types.rs` (~4.5K) | 🟢 **expression coverage COMPLETE** — the FULL-STREAM resolved-type golden (every expression, `is_typed`=true) passes **all 127 files, denylist EMPTY**. Remaining P3-adjacent: the aux resolution maps cgen reads (method_calls/impl_calls/dyn_coercions/call_sym), diagnostics parity, multi-module — fold into P5 prep as needed |
| **P4 escape** | `src/escape.rs` (~1.5K) | 🟢 **COMPLETE** — `escape.jtr`; diagnostic-set golden (span + message byte-exact) passes **all 129 files**, empty denylist |
| P5 cgen | `src/cgen.rs` (~10.8K) | 🟡 **STARTED** — `examples/std/cgen.jtr` + golden `jestyr_cgen_matches_reference`; increment 1 emits `hello.jtr` **byte-identical** (prelude, `emit_program` section order/blank-skeleton locked, `print_str` intrinsic, `JSTR` literal, `return`). Golden uses a growing allowlist (`hello.jtr`). NEXT: params/locals/binops → grow the corpus subset |
| R2 fixpoint | `--features selfhost-fixpoint` | ⬜ not started (acceptance criterion) |

Roughly **~16.8K reference lines still to port** (typeck ~4.5K remaining-ish, escape 1.5K, cgen
10.8K) + the fixpoint harness. Multi-session.

## What EXISTS to build on (reusable toolkit)

- **Importable parser library:** `examples/std/parser.jtr` (no `main`; `pub fn run`, `pub fn
  parse_source(src,it,kwcount,a) -> Parser`, `pub` structs `ExprData`/`StmtData`/`ItemData`, `pub`
  arenas `ex`/`it`/`roots`/`ar`/`st`/`sar`/`par`/`mar`/`lar`). Thin driver `parser_cli.jtr`.
  → **Add more `pub` arenas as later passes read them** (`ty`/`tar` types, `pt` patterns, `iar`
  fn-params, `ear`/`aar`/`gar` enum/attr/generic).
- **Consumer pass template:** `examples/std/typeck.jtr` — `import "parser"`, `parse_source`, walk,
  dump. Copy this shape for `escape.jtr` and `cgen.jtr`.
- **Cross-impl golden template:** `src/proptests.rs` `mod c_oracle` — `build_exe`, per-file corpus
  loop, `DUMP_DIVERGE=1` first-diff printer, staged-subset predicate kept in lockstep on both
  sides. `jestyr_typeck_dump_matches_reference` is the current model.
- `std/list.jtr` has `make/push/get/set/len/free`.

## P3 typeck — remaining increments (in order)

1. ✅ **Ty representation + `ty_str`** (`786269a`) — the TyData arena (+ `tya` child slices,
   well-known ids 0–9), `ty_str` field-for-field vs `Ty::display`, `lower_type` (all structural
   forms; array lens const-eval'd dec/0x/0b), **casts** compared corpus-wide. Key display insight:
   `Named(i)` ≡ `Opaque(name)` in display, so unresolved named types stay Opaque until the table.
2. ✅ **Name/scope resolution — the locals half** (`391d24b`) — flat-arena scopes
   (`scn`/`scst` + `list.truncate`), ALL binder forms (params, let/var, for-iter binds via
   `iter_elem_type`, match binds via `bind_pattern_types`, closure params, select/par-for binds),
   block tail types, array/struct/gen-struct literal + fstring/spawn/await/unsafe/if result types.
   **Name is in the compared subset**: 99/127 full-stream. Two representation shims (structlit-path
   Names skipped Jestyr-side; fstring-interp Names skipped reference-side).
3. ✅ **The GLOBAL TABLE** (`72b8a3b`) — two-phase `build_table` (tdecl/tch/fns/cst/vmap flat
   arenas): Named types + struct fields/method sigs + enum variants w/ payload projection +
   fn sigs (fallible → `T !E`, comptime-return monomorphization) + consts + intrinsic tables;
   Call (method sugar: fn-ptr field / free method / struct method), Field/Index/Deref/Try,
   `self`/`Self{..}` via `selfty`, variant ctors w/ payload unification. **121/127.**
   Shim: a generic lit's ctor Name is NOT skipped (the reference leaves the same orphan).
4. ✅ **cur_expected/cur_ret + generic-decl substitution** (`adad164`) — expected-type
   propagation at every reference site (fn-ret seed → tail arms, let annotations, call args
   vs declared params — fns rows 9→12 w/ full param-type slice, struct-lit/gen-lit fields,
   array elem adoption); nullary-variant adoption; closure→fn-pointer coercion; generic
   struct-value field/method lookup (`find_gen_struct_node` + `subst_ctp_args`, Fn arm in
   `subst_fn_ret`); **fresh scope stack per fn body** (`scfloor` — a struct-value method must
   not see the enclosing comptime type-fn's bindings).
5. ✅ **THE FULL STREAM** (`f6bd996`) — `is_typed`/`typeck_dump_kind` = true: EVERY expression
   compared, all 127 files, denylist empty. Landed: the TRAIT system (trait/impl tables,
   `subst_self`, impl/bound/dyn method dispatch in the reference's fallback order, operator
   traits Add/Sub/Mul/Div/Eq/Ord before native semantics, Error on missing impls),
   monomorphization by UNIFICATION (`unify_names`/`subst_by_names`/`fn_tp_names` — free-method
   receiver+arg unification, plain-call bracket generics), and the block/FieldInit
   representation shims (the reference embeds fn/then/loop/select-arm blocks as structs, only
   bare-`{}` and `else` blocks are arena exprs). Deep-dive harness: `TYPECK_FILE=<basename>`
   prints the aligned per-expr id/kind/span/want/got stream.
   **→ P3's defined golden is COMPLETE. Next: P4 escape.**

## P4 escape — ✅ DONE (`ce9416b`)

`examples/std/escape.jtr` imports the parser + the typeck LIBRARY; the diagnostic-set golden
(`jestyr_escape_dump_matches_reference`, span + full message byte-exact, emission order) matches
the reference on all 129 files. All routes ported (return/capture/store/give-away, region escape,
loop mutation, `@no_alloc`/`@deterministic`, manual drop, shared-mut-slice spawn) + closure
capture analysis. **Enabler:** typeck.jtr is now a library (`typeck.check_parsed` returns the
Checker; `typeck.ty_is_copy`; `mcalls`/`icalls` resolution records; `@copy` -> tdecl slot 9);
thin `typeck_cli.jtr` drives the P3 golden. Pattern to copy for cgen: `import "typeck"`,
`check_parsed`, walk.

## P5 cgen + R2 fixpoint — the remaining work

**See `jestyr-selfhost-P5-cgen-R2-handoff.md` for the full, concrete plan** (emit-program order,
the construct-by-construct increment list, the instance-collection/mangling/match/drop machinery,
what to `pub`-expose from typeck, and how to stand R2 up early). In brief:
- **P5:** `examples/std/cgen.jtr` (`import parser`+`typeck` → `check_parsed` → walk items emitting
  C). Golden = **byte-identical C** vs `cgen::emit`, staged construct-by-construct from `hello.jtr`
  outward; lean on the `attest` C-SHA-256 so equality is a hash compare. Build a
  `CGEN_FILE=<basename>` deep-dive printer first.
- **R2:** gate `--features selfhost-fixpoint` (add to Cargo.toml). Stand up jc1→jc2→jc3 **early on
  a subset** (`hello.jtr` first); assert **jc2 ≡ jc3** (byte-identical C = the fixed point = the
  proof).

## Known constraints / lessons (carry forward)

- **Parser can't store borrowed `src`** (escape checker) → text tests are hard. Positional
  contextual keywords (`step`/`recv`/`reduce`) are safe; `par` needed a driver-precomputed
  `parw[i]` bool mask. Reuse the driver-precompute pattern if a later pass needs a text test.
- **`parse()` vs `parse_module()`:** the P3 reference golden uses `parse()` (fills `ast.items`,
  which `check` iterates); `parse_module` leaves `ast.items` empty ⇒ nothing inferred.
- **Reachability, not arena sweep:** the reference types an expr iff `infer` visits it (body-
  reachable). Const-eval slots, pattern literals, defaults, `step`, array-repeat `count`, and
  impl/trait bodies stay `Unknown`. `typeck.jtr`'s `mark`/`walk_items` already encodes this.
- **A module with `main` can't be imported** (duplicate-`main` link error) → every pass that other
  passes consume must be a library (like `parser.jtr`), with a thin `*_cli.jtr` driver for its
  golden.

## One-line
P2 + **P3 (full-stream resolved-type golden)** + **P4 (diagnostic-set escape golden)** all pass
the whole corpus with empty denylists. Parser, typeck, and escape checker are written in Jestyr,
cross-checked byte-for-byte against the Rust reference. **Next: P5 cgen** — `examples/std/cgen.jtr`
lowering AST + types to C, golden = byte-identical C (lean on module content-hashing + `attest`),
construct by construct → then R2 fixpoint (jc2≡jc3). All committed + pushed to `origin/master`.
