# Jestyr self-hosting — what REMAINS (consolidated checklist)

> One-page map of everything left to self-host the compiler (ROADMAP workstream P), so no context
> is lost between sessions. Pairs with the detailed handoffs:
> - `docs/session-notes/jestyr-selfhost-P3-typeck-handoff.md` (current pass, authoritative)
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
| **P3 typeck** | `src/typeck.rs`, `src/types.rs` (~4.5K) | 🟡 **STARTED** — resolved-type golden 127/127 for the *typed subset so far* (literals + boolean ops) |
| P4 escape | `src/escape.rs` (~1.5K) | ⬜ not started |
| P5 cgen | `src/cgen.rs` (~10.8K) | ⬜ not started (the giant) |
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

1. **A Jestyr `Ty` representation + `ty_display`.** Mirror `Ty` (`src/types.rs`): an arena (or
   compact code+payload) for the recursive forms (`Ptr`, `Slice`, `Array`, `GenRef`, `RegionRef`,
   `GenStruct`/`GenEnum{args}`, `Fn`, `Result`, `Task`, `Named`, `Opaque`; prims + `Unit`/`TypeKw`/
   `Unknown`/`Error` trivial). `ty_display` MUST equal `Ty::display` (the golden's canonical form).
   Fold the current `mark` reachability walk into an `infer` that *returns* a `Ty` and stores it
   per ExprId; dump `ty_display` for the typed subset. **This is the representation unlock.**
2. **Arithmetic / bitwise / shift binaries + `-`/`~`/`&` unaries** — result = operand type (numeric)
   / `GenRef(t)` for `&`. Needs (1). Watch: operator-trait dispatch (`Add`/`Sub`/`Mul`/`Div` on
   user types → the trait method's return) vs primitive native semantics — see `infer`'s Binary arm
   (`resolve_operator_trait`).
3. **`Name` resolution (the bulk).** A lexical `Scope` (name→Ty stack) with locals from
   `let`/params/loop-binds/match-binds, plus the global table (`build_table`: consts, fns, variants,
   types). Mirror `infer`'s `Name` arm. Leniency: unknown named type → `Opaque`, generic param →
   `Opaque` (both non-`Copy`), give-up → `Unknown`. Cross-module qualified access (`mod.x`) needs
   the import map — but the single-file `check` used by the golden resolves imports as
   `Modules::single`, so imported names stay `Opaque`/`Unknown` (match that).
4. **Remaining `infer` arms**, one per increment, each with its golden slice: field access, index,
   deref, `try`/`?`, call (fn / method / variant-ctor / intrinsic return types), cast, struct &
   array literals + `..spread`, `if`/`match`/`block`/`unsafe` result types, ranges, closures
   (`Fn`), `await`/`spawn` (`Task`), `concurrent`/`region`/`select`/`par for`, f-strings, `self`/
   `Self`, attrs. Then P3's golden covers the whole expression set corpus-wide.

## P4 escape (after P3) — `src/escape.rs` ~1.5K, smallest pass

- New `examples/std/escape.jtr` consuming the parser AST (+ the P3 types it needs).
- **Golden = the diagnostic SET** (message + span) equal to the reference `escape::check` on the
  corpus. The escape examples (`examples/escapes.jtr`, `examples/region_escape.jtr`) already pin
  expected error counts — reuse them.
- Same importable-AST + cross-impl-golden toolkit.

## P5 cgen (the giant) — `src/cgen.rs` ~10.8K

- New `examples/std/cgen.jtr` lowering AST + types to C.
- **Golden = BYTE-IDENTICAL C** to the reference, construct by construct. After each construct the
  corpus subset using only ported constructs must match byte-for-byte.
- **Lean on module content-hashing + `attest`** (already in the Rust impl): "same input ⇒ same
  output" is a hash compare, not a full text diff. If the C matches, the gcc build + run behavior
  is identical for free.

## R2 fixpoint — the acceptance criterion

- Gate behind `--features selfhost-fixpoint` (outside the toolchain-free default suite).
- 3-stage: **jc1** (Rust builds the Jestyr compiler) → **jc2** (jc1 rebuilds it) → **jc3** (jc2
  rebuilds it); assert **jc2 ≡ jc3** (byte-identical). Stand it up **early on a subset** once P5
  emits C for a small self-contained program, then grow.

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
P2 done. P3 typeck started (127/127 for literals + bool ops via a body-reachability walk on an
importable parser library). Remaining: P3 (Ty representation → arithmetic → Name/scope → rest of
`infer`) → P4 escape (diagnostic-set golden) → P5 cgen (byte-identical-C golden) → R2 fixpoint
(jc2≡jc3). All committed + pushed to `origin/master`.
