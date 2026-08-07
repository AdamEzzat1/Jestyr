> **Internal development log** — kept for provenance. Status lines and counts in this file reflect the moment it was written, not the current state. Start at the repo [README](../../README.md) for current status and verified claims.

# Jestyr — Session Summary (2026-06-29)

**Workstream:** K (Module system v2) — taken from ~70% to **~98%**, plus a grounded
research pass on the **self-hosting** gate (workstream P).
**Branch:** `elated-tesla-23a2a4` → ff-merged to `master` after every green
increment. **Discipline held throughout:** each increment is `cargo test`-green and
warning-clean (incl. the `dharht-experiment` + `bench-alloc` feature builds), ships
unit + wiring + property + bolero-fuzz tests, is teeth-verified by mutation, and keeps
the emitted C **byte-identical** for every pre-existing program (the repo invariant).

---

## What shipped (modules-v2 / K), in commit order

| Commit | Increment | One-line |
|---|---|---|
| `274e2e4` | 1 — per-module namespaces | `(ModId, name)` keying for fns/consts; unqualified resolves current-module-first; colliding C symbols disambiguated by a `canon` that's a no-op when unique. **Cleared the logged self-host blocker** (`intern` couldn't import beside `list`/`strmap`). |
| `e92e69f` | 2 — qualified type paths | `mod.Type` / `mod.Type(args)`: new additive `TypeKind::Path`, lowered like `Name`/`App`, with a visibility audit (private / unknown / unbound-module → error). |
| `2c2b45d` | 3 — directory-as-module | `import "pkg"` merges a directory's `.jtr` files into **one** shared-namespace module (sorted → deterministic); the loader now separates the **module** id space from the **source-region** id space so diagnostics stay file-accurate. |
| `9e4eaa8` | 4 — module content-hashing | sha256 over each module's **normalized post-parse form** (sorted item renderings; comment/whitespace/reorder-insensitive) folded with imports' hashes (transitive). `Modules.hashes` / `Modules::hash(m)`. The unique, on-thesis feature. |
| `1f4bce5` | 5 — manifest hash-verification | `import "x" = "<sha256>"` pins a dependency; the loader errors on drift. Opt-in; unpinned imports never checked. |
| `7f9f3f3` | (interim) | A clear cross-module type-name collision *diagnostic* (later superseded by real support). |
| `5fc1dde` | 6 — collidable types | Two modules may each define the same **non-generic** `struct`/`enum`/`distinct`: `type_index`/`variants`/`TypeDecl.name` keyed by a type/variant `canon`; `mod.Type` resolves in the target module (backend got the import map); every `Jestyr_<type>`/enum-tag/variant site canonicalizes. |
| `74b16b3` | 8 — declarative module manifest | `Modules::render_manifest` emits the content-hash DAG as a deterministic parseable artifact; `verify_manifest` reports per-module (transitive) drift. The lockfile-lite declarative surface; pairs with O's `attest`. |
| `7e1372c` | 7 — generic-enum collisions | Two modules may each define `enum Box(T)`: the `GenEnum` ctor is canonicalized at resolution → distinct `Jestyr_Box__m<a>__i32` / `__m<b>__i32` instances; a shared canon-aware `find_generic_enum`; a misclassified bare instance is filtered from struct-instance collection. |
| `bc7a0f4` | (note) | Code comments recording the **generic-struct** collision deferral at the `GenStruct` ctor sites. |

Net test delta: ~550 → **607 tests** green; warning-clean across default + both feature
builds.

---

## Key engineering decisions (the "why")

- **`canon(mod, name, dup)` is the identity unless `name` collides across modules.**
  This single property made every collision change *safe*: `dup_*` sets are empty for
  all pre-existing programs, so the emitted C is byte-identical regardless of how much
  `cur_mod` threading was added — a missed `cur_mod` can only mis-resolve a *colliding*
  program (caught by new tests), never perturb existing output.

- **Module id vs source-region id (directory-as-module).** The v1 loader overloaded one
  index for both "which namespace owns this item" and "which file does this span belong
  to." Those coincide only for single-file modules; splitting them let a package's files
  share a namespace while diagnostics still point at the exact file.

- **Store the canonical name in `TypeDecl.name`.** Anything that lowers a resolved
  `Ty::Named(i)` then disambiguates for free; only the *bare-name resolvers* and the
  AST-name-based typedef emitters needed touching.

- **Generic enums vs generic structs are different namespaces.** A generic enum is a
  real type (instances collected from typeck's already-canon `Ty`s → clean fix). A
  generic struct is a comptime *function*; its instances are gathered by walking the AST
  before `cur_mod` is set → deferred (see the handoff).

---

## Deliberately deferred (no logged blocker)

- **Generic-struct collisions** — the comptime-fn ctor lives in the function namespace;
  needs `dup_fns` canon threaded through cgen's AST-walking struct-instance collection.
- **Executable `build.jestyr`** — needs CTFE; the declarative manifest is the
  non-premature half.

---

## Research pass — the self-hosting gate (workstream P)

Concluded with evidence from the live compiler + ROADMAP/HANDOFF. Headline: the
language prerequisites are essentially met; self-hosting is gated by **one genuine
correctness blocker (no auto-drop of owned fields)** plus **the ~27K-line port itself**.
Full breakdown, fix approaches, and the required test scaffolding are in the companion
**`jestyr-selfhost-blockers-handoff.md`**.

Verified live this session:
- Nested struct field of a `Drop` type → **zero** drop calls at scope exit (the gap).
  RAII for plain locals works (`stdlib_demo` proves `List` drops at scope exit).
- `read_file` is non-recoverable (aborts; `String !IoError` is a follow-up).
- Bootstrap compiler is **31,871 lines** of Rust (`cgen.rs` alone ≈ 9.6K) — the scale
  the port must reproduce.

---

## Repo state at session end

- `master` carries all 11 K commits; worktree + main checkout clean.
- ROADMAP.md workstream K updated to ~98%; the in-repo `MODULES-V2-HANDOFF.md` plus the
  running `~/Downloads/jestyr-modules-v2.md` design doc cover increments 1–8.
- Memory index updated (`modules-v2 (K) ~98%`).
