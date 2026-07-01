# Jestyr modules-v2 (workstream K) — Increment 1: per-module namespaces

*Session summary / design note. Landed on `master`, one green increment.*

## What shipped

Jestyr is lifted from a **flat global name pool** to **per-module namespaces for
functions and consts** — the one feature on the design panel with a logged,
reproduced failure on the self-host critical path (`intern` could not be imported
beside `list`/`strmap` because both define `make`, `get`, `len`, and a pile of
private helpers `byte_at`/`hash_str`/`key_eq`/`copy_key`/`place`/`slot_at`).

After this increment:

- Two modules may each define a top-level `make` (or any name), including a
  **generic** `make` (à la `list`, `comptime T`) coexisting with a **non-generic**
  `make` (à la `intern`/`strmap`). Both compile; qualified calls hit the right one.
- An **unqualified** reference to a name the current module does not define is now
  an **unresolved-name error** (“cannot find `f` in this module; it is defined in
  module `a` — call it qualified as `a.f`”), not a silent cross-module hit.
- Every non-colliding program — single-module *and* collision-free multi-module —
  emits **byte-identical C** (verified by diffing the emitted C of every shipped
  example against the pre-change compiler). The disambiguation only fires for a
  genuine collision, which the flat pool could not express at all.

## The resolution model

A name is resolved in this order:

1. **lexical scope** (locals/params) — unchanged;
2. **the current module’s own items** — an unqualified `name` resolves *only* if
   `(cur_mod, name)` is owned by the current module;
3. **qualified `mod.name`** — resolves against `(target_mod, name)` via the
   import binding (the existing `imports` map + `resolve_qualified_*` path),
   gated on `pub`.

A bare name that the current module doesn’t own never falls back to another
module. If some *other* module owns it, that is the new unresolved-name error;
if *no* module owns it, it stays an opaque/builtin/intrinsic name (unchanged
leniency).

## The two tables (the key design decision)

Rather than thread a `ModId` through the hundreds of bare-name lookups in the
checker and backend, the increment uses **two complementary structures**:

1. **`owner: HashMap<(ModId, String), bool>`** (rekeyed from the v1
   `HashMap<String, (ModId, bool)>`). This is the *namespace* — it drives
   ownership, visibility, and resolution isolation. Two modules owning `make` are
   two distinct entries, not a collision.

2. **A canonical symbol name, disambiguated *only on collision*.** A shared free
   function

   ```rust
   pub fn canon(modid: ModId, name: &str, dup: &HashSet<String>) -> String {
       if dup.contains(name) { format!("{name}__m{modid}") } else { name.to_string() }
   }
   ```

   where `dup` is the set of fn/const names defined in **more than one** module.
   The `GlobalTable.fns`/`consts` maps are keyed by this canonical name, and the
   backend emits `jestyr_<canon>`. Because `canon == name` for every
   non-duplicated name, all existing lookups, mangling, and monomorphization are
   **unchanged** for collision-free programs — the byte-identical guarantee falls
   out for free, and disambiguation is localized to the genuinely-new case.

`canon` is shared verbatim by the type checker (table keys + resolution) and the
backend (symbol emission), so the two layers can never disagree on a symbol’s
name — the same discipline `unify_tp` already uses.

## The mangling scheme

| case | C symbol |
|---|---|
| name unique program-wide (the common case) | `jestyr_<name>` *(unchanged)* |
| name defined in modules `m`, `n`, … (non-generic) | `jestyr_<name>__m<modid>` |
| ditto, generic instance | `jestyr_<name>__m<modid>__<targs>` |

`<modid>` is the loader’s DFS-assigned module index (root = 0), which is a
deterministic function of the import graph.

## How the backend stays in sync

`TypeInfo` gained three additive fields the backend reads:

- `item_mod: Vec<ModId>` — the owning module of each `Ast::items[i]`, so a
  definition’s canonical symbol is computable while emitting protos/defs.
- `dup_fns: HashSet<String>` — the collision set, for `canon` and for recognising
  a colliding name in capture analysis.
- `call_sym: HashMap<ExprId, String>` — the canonical target name of an
  *unqualified* call/value-ref, recorded **only** when it differs from the bare
  name (i.e. a within-module reference to a colliding name). The backend prefers
  it over the AST’s bare name; its absence ⇒ the bare name ⇒ byte-identical output
  for everything else. (Qualified calls already flow through the existing
  `qualified` map, which now stores the canonical name.)

`find_fn` (cgen) and `find_fn_decl` (typeck) became canon-aware (they match each
item’s canonical name); the `generics` set and monomorphization `instances` are
keyed by canonical name. None of this changes output when `dup` is empty.

## Files touched

- `src/types.rs` — `canon` helper; `TypeInfo::{item_mod, dup_fns, call_sym}`;
  `GlobalTable.fns`/`consts` keyed by canonical name.
- `src/typeck.rs` — `build_owner` → `(owner keyed (ModId,name), name_mods, dup)`;
  canonical table keys; current-module-first unqualified resolution + the
  cross-module unresolved-name error; qualified resolution records the canonical
  name; field-visibility lookup uses the new `(module, name)` owner.
- `src/cgen.rs` — **mangling/symbol selection only** (canon-aware definition
  emission, call emission, monomorphization, `&fn`, capture analysis). The merged
  single-arena / global-span model is untouched.
- `src/module.rs`, `src/proptests.rs` — tests (below).
- `ROADMAP.md` — K to ~70%; P note that the namespace blocker is cleared.

**Untouched (parallel-safety contract):** `src/main.rs` (workstream O), the
merged-arena/global-span seam, types/variants/traits resolution (still global —
that is increment 2, `mod.Type`).

## Test matrix (all green; 538 tests, warning-clean incl. both feature builds)

- **Wiring (`module.rs`)**: `two_modules_may_define_the_same_top_level_name`
  (distinct symbols + qualified calls resolve to distinct canonical names),
  `unqualified_cross_module_name_is_unresolved` (the negative),
  `a_generic_and_a_nongeneric_same_name_coexist` (the real blocker shape),
  `a_unique_name_keeps_its_bare_symbol` (byte-identical guarantee).
- **Property (`proptests::modules_props`, real loader, N modules)**: distinct
  symbols / namespace isolation, multi-module determinism, and unqualified-sibling
  calls never resolving.
- **Fuzz (`proptests::fuzz::fuzz_multimodule_resolution`)**: in-memory split of an
  arbitrary parsed program across two modules → resolution + lowering never panic;
  `canon` never panics on arbitrary names.
- **Byte-identical**: emitted C of every shipped example diffed against the
  pre-change compiler — 0 diffs.
- **Teeth (mutation)**: disabling collision disambiguation (`dup = ∅`) fails the
  two positive proof tests; disabling the cross-module diagnostic fails the
  negative test. Reverted → green.

---

# Increment 2 — qualified type paths (`mod.Type`)

The type-position twin of increment 1's qualified calls. You can now write a
module-qualified type — `mod.Type` and `mod.Type(args)` — in any type position
(signatures, fields, nested types like `[]mod.Type` / `*mod.Type`), with
cross-module **type visibility** enforced.

## What shipped

- **Syntax + AST**: a new *additive* `TypeKind::Path { module, name, args }` arm,
  produced only when a `.` follows the head identifier in type position. `Name`/`App`
  are untouched.
- **Resolution** (`lower_type`, `&self`): a `Path` lowers like `Name`/`App` — by the
  type's name — because types are globally unique today. So `mod.Type` resolves to
  the same `Ty` (struct/enum/generic instance) it would unqualified.
- **Visibility audit** (`audit_type_paths`, a `&mut self` pass run after the global
  table is built): walks every type annotation reachable from each item (signatures,
  fields, enum payloads, distinct bases, impl targets/methods, nested types) and, for
  each `mod.Type`, checks the head is an import binding of the *referencing* module
  and the target exposes the type as `pub`. Three error shapes mirror
  `resolve_qualified_call`: *not an imported module* / *private to module* / *no
  public type*. (The audit is a separate pass because `lower_type` is `&self` and
  can't report — it recurses over `self.ast` borrows; cloning each node's kind avoids
  the alias.)
- **Backend**: `TypeKind::Path` wired through both cgen type lowerers
  (`ast_type_to_ty` + `c_ty_ast`), the printer, and the doc renderer — all additive,
  resolving by name, so **programs that don't use `mod.Type` emit byte-identical C**
  (re-verified).

## Why types stay globally unique (the scoping call)

Increment 1 deliberately scoped namespacing to functions + consts. Increment 2 adds
the `mod.Type` *path* and *visibility* without yet making types per-module, so two
modules still can't both define `Slot`. Making types collidable is the same
mechanical pattern as increment 1 (canon-key `type_index`/`variants`, disambiguate
struct/enum C symbols — the elegant lever is storing the canonical name in
`TypeDecl.name`, which auto-flows through every `Ty::Named` emission), but it touches
generics-by-name (`GenStruct`/`GenEnum` ctor strings), variant names, impl coherence
keys, and diagnostic display, with **no logged blocker** driving it. It is recorded
as a clean follow-up rather than bundled here.

## Tests (555 green, warning-clean incl. both feature builds)

- **Wiring (`module.rs`)**: `qualified_type_path_resolves_across_modules` (a
  `lib.Point` parameter lowers to `Jestyr_Point`, gcc-style round-trip returns the
  field), plus the negatives `…_to_a_private_type_is_an_error`,
  `…_to_an_unknown_type_is_an_error`, `…_with_unbound_module_is_an_error`.
- **Property (`proptests::modules_props`)**: `qualified_type_paths_resolve_and_lower`
  — N modules each export a distinct `pub struct`, the root references each via
  `m<k>.T<k>`; clean compile, every type lowers, output deterministic.
- **Fuzz**: covered by the existing `fuzz_pipeline` (parser totality on arbitrary
  text now including `ident.Ident` type syntax) and `fuzz_multimodule_resolution`
  (which runs the audit pass on arbitrary split programs).
- **Teeth**: disabling `audit_one_path` fails all three negatives; reverted to green.
- **Byte-identical**: emitted C of existing examples unchanged.

---

# Increment 3 — directory-as-module (§9)

`import "pkg"` where `pkg/` is a directory now loads **all** its `.jtr` files as a
single shared-namespace module (Odin's "directory = package"). A file is still
preferred when both `p.jtr` and `p/` exist.

## The key structural change: module-id vs region-id

The v1 loader had a 1:1 file↔module mapping — `names`/`paths`/`srcs`/`bases` shared
one index that was *both* the module id and the source-file id. A directory-module
breaks that 1:1 (one module, several files). So the loader now separates two index
spaces over the same `Modules` struct:

- **Module** (`ModId`): a namespace — `names`, `imports`, and `item_mod` are
  module-indexed.
- **Region**: a source file — `paths`, `srcs`, `bases` are region-indexed; the
  span→file lookup for diagnostics (`region_of`, used by `render`) walks regions.

For a program of only single-file modules the two spaces coincide, so nothing
changes (verified byte-identical). A directory-module is one `names`/`imports` entry
with several `srcs`/`paths`/`bases` entries — so **diagnostics still point at the
exact file** while a package's files **share one namespace** (a private helper in
one file is callable unqualified from its siblings; two files defining the same name
is a duplicate-definition error pointing at the offending file).

## Loader shape

- `load_import(from_dir, p)` resolves `p.jtr` (→ `load_file`) or `p/` (→ `load_dir`).
- `load_file` / `load_dir` each create one module (`new_module`), then call
  `add_file_to_module(mid, …)` per file — which records the region, lexes+parses into
  the shared arena, routes items to `mid`, and merges that file's imports into
  `mid`'s bindings.
- `load_dir` reads the directory, keeps only `.jtr` files, and **sorts them** before
  merging — the merged module is a deterministic function of its contents, never of
  the filesystem's enumeration order (the determinism thesis). It memoizes the
  directory early so a diamond (or an intra-package self-import) resolves to the same
  module; cycle detection pushes the directory key onto the visiting stack.

The merged single-arena / global-span model and `import` cycle-as-DAG invariant are
untouched — this is additive bookkeeping plus a resolution of file-vs-directory.

## Tests (559 green, warning-clean incl. both feature builds)

- **Wiring (`module.rs`)**: `a_directory_is_one_shared_namespace_module` (cross-file
  unqualified calls within `pkg`; 2 modules / 3 regions; all package items share one
  `ModId`; gcc-style result 42), `same_name_in_two_files_of_a_directory_is_a_duplicate`
  (shared-namespace duplicate, error rendered against the offending file),
  `a_missing_import_reports_cleanly`.
- **Property (`proptests::modules_props`)**: `directory_is_a_deterministic_shared_namespace`
  — a `pkg/` of `k` files each calling the previous file's `f` unqualified; clean,
  bare symbols, output deterministic.
- **Teeth**: disabling the directory branch fails all three dir tests; reverted to
  green.
- **Byte-identical**: existing multi-file programs unchanged.

---

# Increment 4 — module content-hashing (the unique feature)

Every module now carries a **content hash** — the on-thesis primitive that turns
the loader's existing DAG into the foundation for *provably-incremental* and
cacheable builds (Nix/Unison's "hash is identity" idea, scoped to the feasible
non-premature slice). It pairs with O's `jestyr attest` (which content-addresses
the whole emitted C).

## What the hash is

For each module: `sha256` over its **normalized post-parse form** — the **sorted
set** of its items' pretty-printed (`printer::print_item`) renderings — **combined
with the hashes of the modules it imports**, each tagged by its import binding.

Three properties fall out, and are the test contract:

- **Comment/whitespace-insensitive.** The hash is over the AST's printed form;
  comments and layout are trivia never in the AST, so a comment- or whitespace-only
  edit leaves the hash unchanged.
- **Order-independent.** Item renderings are sorted before hashing, so reordering
  order-independent top-level declarations (design §9) does not change the hash.
- **Semantics-sensitive + transitive.** A changed literal changes the hash; and
  because each module folds in its imports' hashes, **changing a dependency changes
  every dependent's hash** while unrelated modules are untouched. Identical inputs ⇒
  identical hash ⇒ the compiled artifact is provably reusable.

## Implementation

- `printer::print_item(ast, item)` — a new public entry point rendering one item to
  its normalized tree form (reuses the existing, already-tested printer; comments
  excluded by construction).
- `module::compute_hashes(...)` — gathers each module's item renderings (filtered by
  `item_mod`), sorts them, then `module_hash` does a memoized DFS over the import
  DAG (the loader already rejects cycles), folding `import <binding> = <hash>` lines
  in sorted binding order, and `sha256::hex`-es the result.
- Stored in `Modules.hashes` (per module), exposed via `Modules::hash(m)`. Computed
  in both `load()` and `Modules::single()`. Entirely within `module.rs` +
  one printer entry point — no typeck/cgen/escape entanglement.

## Tests (587 green, warning-clean incl. both feature builds)

- **Wiring (`module.rs`)**: `module_hashes_are_deterministic` (same program → same
  64-char digests), `comment_or_whitespace_edit_does_not_change_the_hash`,
  `a_semantic_edit_changes_the_hash`, `reordering_declarations_does_not_change_the_hash`,
  `changing_a_dependency_changes_the_dependents_hash` (lib changes ⇒ lib's *and*
  main's hash change; the unrelated `other` module's is unchanged).
- **Property (`proptests::modules_props`)**: `module_hash_is_normalized_deterministic_and_semantic`.
- **Fuzz (`proptests::fuzz::fuzz_module_hash`)**: hashing an arbitrary parsed program
  never panics and reproduces.
- **Teeth**: dropping the import-fold fails the transitivity test; dropping the item
  sort fails the reordering test. Reverted to green.

## Deferred (the second hashing sub-increment)

Manifest **hash verification** (`import "x" = <sha256>`) — checking a committed
expected hash against the computed one — is the natural follow-up; it needs a
manifest surface (adjacent to `build.jestyr`) and is recorded, not built here. The
hash *primitive* is the durable, useful-now part.

---

# Increment 5 — manifest hash-verification (`import "x" = "<sha256>"`)

The second hashing sub-increment: the content hash becomes *enforceable*. An import
may pin its dependency to an exact hash, and the loader verifies it — a lockfile-lite
reproducibility guarantee built directly on increment 4's primitive.

## What shipped

- **Syntax + AST**: `import "x" = "<sha256>"` (and `import "x" as y = "<hash>"`) — an
  additive `expected_hash: Option<String>` on `ImportDecl`, parsed as an optional
  `= <string>` after the path/alias.
- **Verification** (loader): when an import carries a pin, the loader records
  `(target module, expected hash, span)`; after `compute_hashes` runs, it compares
  each pin against the dependency's computed hash and, on mismatch, emits
  *"module `x` hash mismatch: pinned `<a>`, but its content hashes to `<b>`"* at the
  import. Opt-in — an unpinned import is never checked.

The pin verifies the **transitive** hash (increment 4 folds in imports' hashes), so
pinning a module also pins, in effect, the entire subgraph it depends on — a change
anywhere beneath it trips the pin.

## Tests (590 green, warning-clean incl. both feature builds)

- **Wiring (`module.rs`)**: `a_pinned_import_hash_verifies_and_a_wrong_one_errors`
  (learn the real hash from an unpinned load, then a correct pin verifies clean and a
  bogus pin errors naming the actual hash); `an_unpinned_import_is_not_hash_checked`.
- **Property (`proptests::modules_props`)**: `pinning_the_computed_hash_verifies_else_errors`
  — over varied dependencies, the computed hash always verifies and any other pin errors.
- **Fuzz**: `import "x" = "..."` parsing is covered by `fuzz_pipeline` (parser totality).
- **Teeth**: disabling the comparison fails the wrong-pin test; reverted to green.

---

# Increment 6 — collidable types (full type namespacing)

Two modules may now each define the **same non-generic** `struct` / `enum` /
`distinct` type. This is the type-side twin of increment 1: the same `canon`
mechanism, applied across the type-lowering core.

> *History:* an interim slice shipped first — a clear cross-module-collision
> *diagnostic* (`type_first_mod` + an actionable "defined in both module `a` and
> `b`" message) in place of an opaque "duplicate definition" — while full support
> was scoped out. This increment **supersedes** that: the collision is now
> *supported*, and the diagnostic + `type_first_mod` are removed (distinct canon
> keys make any clash a genuine same-module redefinition again).

## Resolution (typeck)

- `build_owner` derives **`dup_types`** (non-generic struct/enum/distinct names in
  >1 module) and **`dup_variants`** (enum-variant names in >1 module). Generic
  enums are excluded — their monomorphized-instance mangling is the deferred case.
- `type_index`, the `variants` table, and **`TypeDecl::name`** are keyed by a type
  `canon` / variant `canon` (`name__m<mod>` only when duplicated, else the bare
  name). Because `TypeDecl::name` *is* the type's identity, everything that lowers a
  resolved `Ty::Named(i)` disambiguates for free.
- An unqualified type/variant resolves **current-module-first** (`cur_mod` is now
  set in `build_table` phase 2, not just phase 1); a `mod.Type` path resolves in the
  **target** module via the import map. A bare cross-module type name no longer
  leaks across the boundary.

## Codegen (cgen)

- New `cur_mod` field, set per item across **every** emission pass
  (`forward_types`, `struct_defs`, `enum_defs`, `fn_protos`, `fn_defs`, `consts`,
  `impl_protos`, `impl_defs`); `canon_type` / `canon_variant` helpers over
  `info.dup_types` / `dup_variants`.
- Every `Jestyr_<type>` site canonicalizes: the forward/struct/enum/distinct
  typedefs, enum tag + variant table (keys *and* `enum_name`), struct-literal
  construction, and the bare-name resolvers `ast_type_to_ty` / `c_ty_ast` /
  `eval_type_arg` / `is_global_name`.
- `TypeInfo` now carries the **per-module import map** so the backend can resolve a
  `mod.Type` path to the right module's (possibly colliding) type — the one piece
  cgen lacked.

So two modules' `Slot` become `Jestyr_Slot__m<a>` / `__m<b>`, and each `mod.Slot`,
variant construction, and `match` hits the right one. **It is a no-op for any
non-colliding program** — `canon` is the identity when a name isn't duplicated (and
`dup_types`/`dup_variants` are empty for every pre-existing program), so the emitted
C is byte-identical there (verified across the std demos + the type-heavy examples).

## Tests (603 green, warning-clean incl. both feature builds)

- `two_modules_may_define_the_same_struct` (distinct symbols, no bare `Jestyr_Slot`,
  each `mod.Slot` resolves to its own struct), `two_modules_may_define_the_same_enum`
  (distinct tags + per-module variant construction/`match` dispatch),
  `a_same_module_type_duplicate_stays_a_plain_duplicate`.
- Property `same_named_types_across_modules_get_distinct_symbols` — *k* modules each
  defining `struct T`, distinct `Jestyr_T__m<id>`, output deterministic.
- Teeth: neutralizing `dup_types` collapses the keys → every collision test fails;
  reverted to green. End-to-end gcc runs (`Slot`→42, `Color`→12) confirm behavior.

## Explicitly deferred (with reasons)

- **Generic type-name collisions** (two modules each defining the same `enum Box(T)`
  or generic struct) — the monomorphized-instance mangling (`Jestyr_<ctor>__<args>`)
  overlaps the function-ctor canon; left globally keyed (no logged blocker).
- **`build.jestyr` executable half / lockfile / registry / vendored deps /
  remote cache / capability-typed boundaries** — ecosystem-premature or needing
  machinery Jestyr doesn’t have (CTFE, an effect system). The *module hash*
  primitive is the durable, non-premature slice; the ecosystem rides on it later.
