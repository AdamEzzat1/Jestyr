> **Internal development log** — kept for provenance. Status lines and counts in this file reflect the moment it was written, not the current state. Start at the repo [README](../../README.md) for current status and verified claims.

# modules-v2 (Workstream K) — Handoff (run in a parallel session, commit to master)

> Self-contained cold-start for the **module system v2** workstream (design §9).
> Designed to run **in parallel** with the Tooling session
> ([`TOOLING-HANDOFF.md`](TOOLING-HANDOFF.md), workstream O) — see the
> **Parallel-safety contract**; respect it and the two sessions never touch the same
> lines. Everything lands on `master`, one green increment per commit. Companion
> reading: `ROADMAP.md` workstream K (+ P), `jestyr-design.md` §9/§14, `HANDOFF-NEXT.md`
> (the namespace collision that's *already* blocking self-hosting), and **read
> `src/module.rs` carefully** (the loader). Conflict tier: **MEDIUM**.

---

## Mission

Lift Jestyr from a *flat global name pool* to a real module system. In priority order:
1. **Per-module namespaces** — the **blocking** fix (the one feature on the whole design
   panel with a logged, reproduced failure on the critical path). Today two std modules
   can't both define `make`/`destroy`; the self-hosting port can't grow the stdlib past a
   few modules until this lands. *This is the first increment.*
2. **Qualified type paths** (`mod.Type`) — falls out of the same resolution change.
3. **Directory-as-module** (design §9) — cheap once namespaces exist.
4. **Module content-hashing** — the **unique, on-thesis feature** (deterministic build
   inputs → provably-incremental builds), scoped to the cheap non-premature slice.

---

## Where Jestyr is today (the load-bearing facts — confirmed in-tree)

`src/module.rs` is a **whole-program-as-one-translation-unit** loader: it DFS-walks
`import`s from a root, parses every reachable `.jtr` into **one shared arena**,
concatenates sources into **one global span buffer**, memoizes diamonds by
`canonicalize`d path (`loaded: HashMap<PathBuf, ModId>`), and rejects cycles (DAG
invariant). It already records everything per-module namespacing needs but **doesn't use
module identity for name lookup**: `Modules` carries `item_mod: Vec<ModId>`,
`item_pub: Vec<bool>`, and per-module `imports: Vec<HashMap<String, ModId>>`.

**The flatten point (the crux):** `build_owner` in `src/typeck.rs` builds a
`HashMap<String, (ModId, bool)>` keyed on the **bare item name** with
`owner.entry(n).or_insert(...)` — *first-writer-wins on a bare name across all modules*,
so two modules each defining `parse` silently collide. Likewise `GlobalTable`
(`src/types.rs`) keys `fns`/`consts`/`type_index`/`variants`/`traits` purely by `String`.
**That single bare-`String` keyspace is the v1 model's ceiling.**

The good news for parallel-safety and for not breaking everything: per-module namespaces
are a **name-resolution** change (rekey the tables, consult the owning module first). The
**merged-arena / global-span design that escape and cgen depend on stays untouched** —
that is the seam to protect.

Already in place to build on: `cur_mod` is tracked in typeck; `binding_module` resolves an
import binding → `ModId`; `resolve_qualified_call`/`resolve_qualified_const` already gate
on `owner == target_mod && is_pub`; `enforce_visibility` already enforces `pub` (tested in
`module.rs`); codegen mangling (`jestyr_<name>__<targs>`) already distinguishes symbols.

---

## Parallel-safety contract (read before touching anything)

This session may edit:
- **`src/module.rs`** — per-module symbol tables, content hashing, directory-as-module.
- **`src/typeck.rs`** — `build_owner` rekey + unqualified-name resolution path + `mod.Type`
  type-path resolution.
- **`src/types.rs`** — `GlobalTable` map key change (`String` → `(ModId, String)`).
- **`src/cgen.rs`** — *mangling only* (prefix a module discriminator), additive.
- **`src/parser.rs` / `src/ast.rs`** — only if `mod.Type` needs a syntax/AST node (keep
  additive: a new `TypeKind` arm, not edits to existing ones).
- **`src/proptests.rs`** — additive `mod prop`/`mod fuzz` + `module.rs` fixture tests.

This session **must NOT** edit `src/main.rs` (that's **O's turf** — the Tooling session
adds subcommands there) or `src/doc.rs`. Keep K out of `main.rs` and O out of
`typeck.rs`/`types.rs`/`module.rs` and the streams share zero lines.

**Protect the seam:** do **not** rearchitect the merged single-arena / global-span model
(`module.rs` source concatenation + disjoint span regions) — escape and cgen rely on it.
Namespacing is *additive bookkeeping + a resolution change*, nothing more.

Standard worktree flow: commit on this branch, then **ff-merge master**
(`git -C C:\Users\adame\Jestyr merge --ff-only <branch>`). If O landed first, rebase and
re-run — by the contract there should be no real conflicts; Rust's exhaustive `match`
flags any arm you missed after a rebase.

---

## Inspiration — one idea to steal per system

| System | The single idea to steal |
|---|---|
| **Rust modules + paths** | **Resolve against a per-module symbol table, not a global pool, with explicit `pub` re-export to lift a name across the boundary.** This *is* the namespace fix. Skip Rust's deep `mod`-tree ceremony (§9 already rejects it). |
| **Go modules + MVS** | **Minimal Version Selection** — pick the *lowest* version satisfying all constraints, so resolution is a deterministic function of the manifests (no SAT, no "latest" drift). Algorithmically trivial, inherently reproducible — Jestyr's value system. *(Future; behind manifests.)* |
| **Cargo (lockfile)** | **A committed lockfile pinning every transitive dep to an exact hash; apps commit it, libraries don't.** The cheapest reproducibility win — *but premature until there are deps/a registry* (see "What NOT to build"). |
| **Zig `build.zig`** | **The build is written in the language itself (comptime), with a *declarative manifest* beside the script** so tooling reads the dep list without executing build code (§9 splits them — honor it). *(Defer the executable half — needs CTFE/G.)* |
| **Nix** | **Content-address build inputs/outputs by hash → identical inputs provably reuse a cached result; hermetic, nothing implicit leaks in.** The seed of the unique feature. |
| **Unison** | **Hash a definition by its typed content; the hash is the identity, names are metadata.** The radical version: unchanged hash ⇒ artifact + downstream caches provably valid; renames are free. |
| **Bazel** | **Every compile action a pure function of hashed inputs → results shareable across machines via a content-addressed cache.** Turns "bit-for-bit determinism" into a team-scale feature. *(Far future; rides on module hashing.)* |
| **ML functors** | **A module parameterized over an interface and instantiated per use** — the principled "collections take an allocator/capability." Natural home for §14's "`core` allocates nothing behind your back." *(Later.)* |

---

## The unique feature — module content-hashing (deterministic build inputs)

**What it is (scoped to the feasible, non-premature slice).** Give every module a
**content hash** — `sha256` over its *normalized post-parse form* (so comment/whitespace
edits don't invalidate), combined with the hashes of its imports. Store it in `Modules`
and expose it. That hash is the future cache key for a module's compiled C/object output:
an unchanged hash *proves* the artifact is still valid, so **provably-incremental and
parallel compilation fall out of the existing DAG with no timestamps or heuristics**.

**Why it is uniquely a Jestyr feature.** The loader is *already* a DAG with cycle
detection + diamond memoization, and `module.rs`'s own header states the DAG "is what
makes parallel + incremental compilation possible later." Determinism is a stated core
thesis with an existing reproducibility canary and the SHA discipline already practiced
(`proptests::sha256`). So hash-as-identity isn't a bolt-on — it's the thesis cashed out
for the build graph. It also **closes the loop with O's `jestyr attest`**: attest can
surface the module-hash DAG as part of its manifest.

**Scope discipline (heed the skeptic).** Build **only**: (1) compute + store + expose the
per-module hash; (2) a `import "x" = <sha256>` *verification* check in a manifest (second
increment). Explicitly **DEFER**: a lockfile, a registry, network fetch, vendored deps,
remote cache — those serve an ecosystem that doesn't exist yet and a registry/network path
that isn't built. The *hash itself* is the durable primitive; the ecosystem rides on it
later. Caveat to honor: the normalization + hash must be deterministic across platforms —
the same discipline the numerics SHA canary already enforces.

---

## Recommended increment order

1. **Per-module namespaces (the blocking fix).**
   - In `typeck.rs`, change `build_owner` to key on `(ModId, name)` (or add a per-module
     table) instead of first-writer-wins on bare `name`; change `GlobalTable`'s maps
     (`types.rs`) to `(ModId, String)`. This is the bulk: mechanical but **wide** — every
     `owner`/`fns`/`type_index` lookup in `typeck.rs`/`cgen.rs` must thread the current
     module (`cur_mod` already exists).
   - Resolve an **unqualified** name as: lexical scope → **current module's** table →
     (imported modules only via qualified `mod.name`, already the `imports` map +
     `resolve_qualified_*` path). A bare name no longer sees another module's items.
   - Mangling: prefix a module discriminator so two modules' `parse` get distinct C
     symbols (a localized `cgen.rs` mangle change).
   - **Proof tests** (the wiring): two modules each defining `fn make(...)`, both imported,
     both used **qualified** → compiles clean (today's flat loader *cannot*); **plus the
     negative**: an unqualified reference to another module's name is now an
     `unresolved name` error, not an accidental hit. This is exactly the
     `make`/`destroy`/`intern`-can't-coexist-with-`list` pain logged in `HANDOFF-NEXT.md`.
2. **Qualified type paths (`mod.Type`)** — the type-resolution twin of (1). Qualified
   resolution exists today for *calls* and *consts* (values), not for type names in
   `TypeKind::Name`/`App` — add that path.
3. **Directory-as-module** (design §9) — merge several files' per-module tables into one
   module's table (additive, sits on (1)).
4. **Module content-hashing** (the unique feature) — compute `sha256` of each module's
   normalized form where the source is read in `load_file`, store in a new
   `Modules.hashes`, expose it; then manifest verification (`import "x" = <hash>`). Stays
   entirely in `module.rs`.

---

## What NOT to build (and why) — save the session

- **`build.jestyr` (the executable half).** A build script *in Jestyr* needs CTFE
  (workstream G, ~10%) to be more than TOML-in-a-costume, and it's a **bootstrap hazard**
  mid-port (a not-yet-self-hosted compiler executing build code). The *declarative
  manifest* half is fine later; the executable half is years early.
- **Lockfile / content-addressed *deps* / vendored deps / remote cache.** Reproducible-dep
  provenance is on-thesis *as an idea* but pins dependencies you don't have, from a
  registry that doesn't exist, over a network path that isn't built. Zero current
  beneficiaries. (The *module hash* primitive above is the part that's useful now; the
  dep ecosystem is what's premature.)
- **Capability/effect-typed module boundaries.** Genuinely cool and a natural extension of
  `@no_alloc`/`@no_panic`, **but Jestyr has no effect system** (design §10.3 flags async
  coloring as *explicitly unsolved*). It presupposes machinery the language has punted on.
  Defer behind a real effect-summary pass.

---

## Rigor — the test layers every increment ships (mirror the existing harness)

For each increment:

1. **Unit tests** — resolution logic in isolation: `(ModId, name)` lookup picks the owning
   module; qualified `mod.name` hits the target; an unqualified cross-module name fails to
   resolve. For hashing: same normalized AST → same hash; a whitespace/comment-only edit →
   *same* hash; a semantic edit → *different* hash.
2. **Wiring tests ("confirm it's plumbed in")** — mirror `module.rs`'s existing
   `pipeline_is_clean` / `loads_and_merges_an_imported_module` /
   `private_item_is_not_visible_across_modules` fixtures. The headline:
   `two_modules_may_define_the_same_top_level_name` (both compile, qualified calls hit the
   right impl, distinct C symbols emitted) and `unqualified_cross_module_name_is_unresolved`.
3. **Golden tests** — a small multi-module fixture's emitted C pins the per-module mangled
   symbols; a module's hash pinned for a fixed input.
4. **Property tests** (`proptests.rs::mod prop` + `arb_*_program`, extended to generate
   *multi-module* programs) — **resolution soundness** (a qualified name always resolves to
   its declaring module or errors; never silently to another module's item);
   **determinism** (module hash is a deterministic function of normalized content — same
   program twice → identical hashes); **namespace isolation** (two modules' same-named
   private items never alias).
5. **Bolero fuzz** (`proptests.rs::mod fuzz`) — a `fuzz_multimodule_resolution` target over
   arbitrary small multi-module programs: resolution never panics, never resolves a bare
   name across a module boundary, the hash function never panics.
6. **Teeth-verify each property by mutation** — e.g. revert the resolver to bare-`name`
   first-writer-wins and watch `two_modules_may_define_the_same_top_level_name` /
   `unqualified_cross_module_name_is_unresolved` fail; revert and confirm green.
7. Keep **default `cargo test` toolchain-free**; gate any gcc-needing multi-module
   round-trip behind `--features c-oracle`.

Every increment stays **`cargo test`-green and warning-clean**, and all existing examples
stay byte-identical (the repo invariant) — note that fixing the flat namespace must not
change the emitted C of any *single-module* program (the mangle change should be a no-op
when there's one module, or the golden examples will shift — verify).

---

## Documentation deliverable (Downloads)

When per-module namespaces land (and again when hashing lands), write a **session summary /
design doc to the user's Downloads folder**:
`C:\Users\adame\Downloads\jestyr-modules-v2.md` — the resolution model (lexical → current
module → qualified imports), the `(ModId, name)` rekey, the mangling scheme, the module-hash
normalization + format, the test matrix, and the explicitly-deferred items
(build.jestyr/lockfile/effects) with reasons. Update `ROADMAP.md` workstream K (and the P
note that the namespace blocker is cleared) too.

---

## Commit-to-master discipline (do this every increment)

- **One green increment per commit.** `git commit -F <msgfile>` (multi-line). End every
  message with: `Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>`.
- After green + warning-clean, **fast-forward master**:
  `git -C C:\Users\adame\Jestyr merge --ff-only <this-branch>`. Don't push unless asked.
- Teeth-verify before committing. Keep all examples byte-identical.

---

## Pointers (verify line numbers; they drift — search the symbol)

| Thing | Where |
|---|---|
| The loader (read first) | `src/module.rs` → `load`, `load_file`, `Modules` (`item_mod`/`item_pub`/`imports`/`loaded`), the flat-merge item loop |
| **The flatten point to rekey** | `src/typeck.rs` → `build_owner` (`owner.entry(n).or_insert`) |
| Global symbol tables to rekey | `src/types.rs` → `GlobalTable` (`fns`/`consts`/`type_index`/`variants`/`traits`) |
| Qualified resolution already present (extend for types) | `src/typeck.rs` → `binding_module`, `resolve_qualified_call`, `resolve_qualified_const`, `cur_mod`, `enforce_visibility` |
| Mangling (add a module discriminator) | `src/cgen.rs` → the `jestyr_<name>__<targs>` mangler (`mangle`/`ty_mangle`) |
| Existing module fixtures to mirror | `src/module.rs` tests (`loads_and_merges_an_imported_module`, `private_item_is_not_visible_across_modules`, `pipeline_is_clean`) |
| Dep-free SHA-256 (reuse for hashing) | `src/proptests.rs` → `mod sha256` (lift to a shared `src/sha256.rs` if O hasn't) |
| The blocker this clears | `HANDOFF-NEXT.md` (P: `make`/`destroy` collide; `intern` can't be imported with `list`/`strmap`) |
| Test-layer conventions | `docs/TESTING.md`; `src/proptests.rs` (`mod prop`/`mod fuzz`/`arb_*`) |
| Design intent | `jestyr-design.md` §9 (modules/build) + §14 (`core`/`std` layering) |

## One-line summary

Land **per-module namespaces** first — rekey resolution from bare `name` to
`(ModId, name)`, resolve current-module-first, prove two modules can share a name — it's
the logged blocker on the self-hosting port. Then `mod.Type` paths, directory-as-module,
and the unique **module content-hashing** (deterministic build inputs → provably
incremental builds, pairing with O's `attest`). Defer build.jestyr/lockfiles/effects
(ecosystem-premature or unbuilt machinery). Full test rigor, docs to Downloads, one green
increment per commit to master, and **don't touch `main.rs`** (O owns it).
