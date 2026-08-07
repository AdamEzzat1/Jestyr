> **Internal development log** — kept for provenance. Status lines and counts in this file reflect the moment it was written, not the current state. Start at the repo [README](../../README.md) for current status and verified claims.

# Jestyr self-hosting — P3 typeck kickoff (cold-start handoff)

> Continues ROADMAP workstream **P**. **P2 is COMPLETE** (the Jestyr parser matches the Rust
> reference item-for-item on all 125 corpus files; module golden 125/125). This note covers the
> **P3 type-checker** start: the architecture, the resolved-type golden, what's typed so far, and
> the path through P3 → P4 → P5 → R2.
>
> Read with `src/typeck.rs` / `src/types.rs` (the reference), `examples/std/typeck.jtr` (the
> Jestyr pass), and the P2→R2 master plan `docs/session-notes/jestyr-selfhost-port-P2-P5-R2.md`.

---

## 1. Architecture (the load-bearing setup — reused for P4/P5)

- **`parser.jtr` is now a pure LIBRARY** (no `main`). Its old `main` became `pub fn run`; a thin
  **`parser_cli.jtr`** (`import "parser"` + `fn main { return parser.run() }`) is the entry the
  parser goldens build. **Why:** a module with its own `main` can't be imported (duplicate-`main`
  link error), so consumer passes (`typeck.jtr`) need the parser main-free. The four P2 parser
  goldens now `build_exe("examples/std/parser_cli.jtr")` and still pass byte-identical.
- **`pub fn parse_source(src, it, kwcount, a) -> Parser`** parses a whole file and records
  top-level item ids in the new `pub roots` field. A consumer pass calls it and reads the arenas.
- **`pub` surface on the parser** (grown as the consumer needs it): structs `ExprData`,
  `StmtData`, `ItemData` (all fields `pub`); `Parser` fields `ex`, `it`, `roots`, `ar`, `st`,
  `sar`, `par`, `mar`, `lar`. Add more `pub` as later increments read more arenas (`ty`/`tar` for
  types, `pt`/`par` for patterns, `iar` for fn params, `ear`/`aar`/`gar` for enum/attr/generics).
- **`list.set`** added to `std/list.jtr` (random-access update; complements `get`).

## 2. The P3 golden — a resolved-type dump (mirrors the P2 pattern)

`jestyr_typeck_dump_matches_reference` (`src/proptests.rs`, `--features c-oracle`), whole corpus:
- **Reference** `rust_typeck_dump(src)`: `Parser::new(src, tokens).parse()` (NB **`parse()`**, not
  `parse_module` — `parse()` populates `ast.items`, which `check` iterates; `parse_module` leaves
  it empty so nothing is inferred) → `typeck::check(&ast)` → for each `ExprId` in order, if its
  kind is in the compared subset, emit `info.expr_types[id].display(&info.table)`.
- **Jestyr** `typeck.jtr`: `parser.parse_source` → the reachability walk → dump the type per
  compared expr, in ExprId order.
- **Staged by (kind, op)** via `is_typed` (Jestyr) / `typeck_dump_kind` (reference) — **these two
  MUST stay in lockstep**: an expr is compared iff both include it. Grow both together per
  increment; the golden then auto-covers the new kind corpus-wide.
- ExprId order matches across impls (both parsers create nodes in the same recursive-descent
  order), so the compared subsequence aligns atom-for-atom. `DUMP_DIVERGE=1` prints the first
  differing line window per file — the fast diagnosis tool.

## 3. The key insight — reachability, not an arena sweep

The reference populates `expr_types[id]` **only for expressions `infer` visits** — those reachable
from a **body** (fn / const / struct-method / struct-VALUE-method) via *expression-child* edges.
It never visits **const-eval** positions (array sizes `[N]T`, enum discriminants `= 1`, attribute
args, bit-field widths — all in the type/item arenas), **match-pattern literals**, **struct-field
defaults**, **for-loop `step`**, **array-repeat `count`** (`[v; n]`), or **impl/trait method
bodies** (`check_items` skips those). Those stay `Unknown` (`?`).

So `typeck.jtr` does the **same body walk** (`mark` / `walk_items`, mirroring `check_items` +
`infer`'s traversal): it follows only expression children from the same roots, recording a
per-ExprId `reached` flag. A compared expr prints its concrete type iff reached, else `?`. Pinning
this byte-exact against the reference is what took the iteration (see the gotchas above — each was
a real divergence the golden caught). **`unsafe`/`concurrent`/`region` carry their block in `a`**
(not the node's own stmt slice); **struct-VALUE `struct { … }` method bodies ARE inferred** (via
`infer`'s `StructType` arm → shared `mark_methods`), which is how the generic
`fn Vec(T) -> type { return struct { fn … } }` pattern's methods get typed.

## 4. Typed so far (the compared subset)

- **Literal leaves** — Int→`i32`, Float→`f64`, Str→`str`, Char→`char`, Bool→`bool`, Null→`*?`.
- **Boolean operators** — Unary `not` and the comparison/logical binaries (`== != < <= > >= and
  or`) → `bool` (primitives fall through to bool; user operands go through `Eq`/`Ord` operator
  traits, which also return bool). Keyed on `(kind, op)`.

Everything else is out of the compared subset (both dumps skip it) until typed.

## 5. Next increments (the road through P3)

The next step needs the pass to **store a computed `Ty` per expr** (not just a reached flag),
because these result types depend on operands/context:
1. **A `Ty` representation in Jestyr** — mirror the `Ty` enum (`src/types.rs`): a `Ty` arena (or a
   compact code+payload) + a `ty_display` matching `Ty::display` exactly (that's the golden's
   canonical form). Prims are trivial; the recursive forms (`Ptr`, `Slice`, `Array`, `GenRef`,
   `RegionRef`, `GenStruct`/`GenEnum` with args, `Fn`, `Result`, `Task`, `Named`, `Opaque`) need
   the arena. Compute bottom-up during the `mark` walk (fold `mark` into an `infer` that returns a
   `Ty`), store per ExprId, dump `ty_display`.
2. **Arithmetic/bitwise/shift binaries + `-`/`~`/`&` unaries** — result = operand type (numeric) /
   `GenRef(t)` for `&`. Needs (1).
3. **`Name` resolution** — the big one: a `Scope` (lexical stack of name→Ty), locals bound by
   `let`/params/loop-binds/match-binds, plus the global table (consts, fns, variants). Mirror
   `infer`'s `Name` arm and `build_table`. This is most of typeck's bulk.
4. Then: field access, index, call (fn/method/variant-ctor return types), casts, struct/array
   literals, `if`/`match`/`block` result types, ranges, `try`/`?`, closures, `await`/`spawn`
   (`Task`), etc. — one `infer` arm per increment, each with its golden slice.

The leniency rules must match exactly (unknown named type → `Opaque`, generic param → `Opaque`,
both non-`Copy`; inference gives up → `Unknown`).

## 6. Past P3

- **P4 escape** (`src/escape.rs`, ~1.5K — smallest pass): golden = the **set of diagnostics**
  (message + span) equal to the reference's on the corpus. Reuse the same importable-AST +
  golden toolkit; the escape examples already pin expected error counts.
- **P5 cgen** (`src/cgen.rs`, ~10.8K — the giant): golden = **byte-identical C** to the reference,
  construct by construct. Lean on module content-hashing + `attest` (hash compare, not text diff).
- **R2 fixpoint** (`--features selfhost-fixpoint`): jc1→jc2→jc3, assert `jc2 ≡ jc3`. The
  acceptance criterion; stand it up early on a subset.

## 7. Discipline (unchanged)
`cargo test`-green (685 default) + warning-clean; cross-impl goldens behind `--features
c-oracle`. Keep non-users byte-identical. Auto-commit each green increment to `master` +
`git push origin master`. One construct per increment, each with its golden slice; teeth-verify
by mutation.

## 8. Anchors
| Thing | Where |
|---|---|
| Jestyr typeck | `examples/std/typeck.jtr` (`is_typed`/`mark`/`walk_items`/`print_expr_ty`) |
| Jestyr parser (library) | `examples/std/parser.jtr` (+ `parser_cli.jtr` driver) |
| Reference typeck / types | `src/typeck.rs` (`check`/`infer`/`check_items`), `src/types.rs` (`Ty`/`Ty::display`/`TypeInfo`) |
| P3 golden | `src/proptests.rs` `mod c_oracle`: `rust_typeck_dump`, `jestyr_typeck_dump`, `typeck_dump_kind`, `jestyr_typeck_dump_matches_reference` (+ `TYPECK_GOLDEN_DENYLIST`, empty) |
| Master plan | `docs/session-notes/jestyr-selfhost-port-P2-P5-R2.md` |
| P2 parser handoff | `docs/session-notes/jestyr-selfhost-P2-parser-handoff.md` |

## One-line summary
P2 parser COMPLETE (125/125). **P3 typeck started**: `parser.jtr` is now an importable library
(+`parser_cli.jtr` driver), `typeck.jtr` consumes its AST and emits a resolved-type dump matching
`typeck::check` + `Ty::display` over the **whole corpus (127/127)**, via a body-reachability walk
that mirrors `infer` exactly. Typed so far: literal leaves + boolean operators. **Next:** a `Ty`
representation + `ty_display`, then arithmetic, then `Name`/scope resolution (the bulk) → rest of
P3 → P4 escape → P5 cgen → R2 fixpoint.
