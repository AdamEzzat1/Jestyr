# Jestyr Roadmap & Parallel Workstreams

> The forward-looking plan: **everything that remains** to make Jestyr a complete
> low-level language, organized so it can be built across **multiple concurrent
> sessions**. Read with [`HANDOFF.md`](HANDOFF.md) (what exists + how it's built),
> [`jestyr-design.md`](jestyr-design.md) (the vision), and [`MOTLEY.md`](MOTLEY.md)
> (the long-term compiler-infra goal Jestyr is meant to implement).
>
> Each workstream below lists **what's done, what's left, the files it touches, its
> conflict risk against other streams, its size, and its value** to (a) language
> completeness and (b) Motley. Use §1 to decide what to run in parallel.

---

## 0. Current state (snapshot)

Working bootstrap compiler in Rust (~10K LOC, 157 tests, warning-clean) taking
`.jtr` → C → native binary. Pipeline: **load (multi-file) → lex → parse →
resolve+typecheck → ownership/escape → C codegen → cc**. Done: the tiered reference
model (`&T`/`&[r]T`), generics + monomorphization, methods, closures, error sets +
`?`, contracts, slices + bounds-elision, structured concurrency, `extern "c"`,
layout attributes, **modules** (item K), **stdlib + allocator-as-value** (item I),
the full **loop** system (unified `for` + fast-follows), **casts**, **byte-level
string iteration**, and **doc comments + a doc generator** (item C, `jestyrc doc`).
Since this snapshot: traits A–F + `dyn`, fn-pointer types, owned `String`/text ops,
fixed-size arrays, the numerics/determinism stack, and the **self-hosting plumbing** —
**file I/O**, **command-line args**, and a **symbol-table map** (`std/{fs,env,strmap}.jtr`;
see workstream P).

---

## 1. How to run parallel sessions (read this first)

**The hard truth about this codebase:** almost every *language feature* touches the
same six files, in the same order:

```
ast.rs  →  parser.rs  →  typeck.rs  →  escape.rs  →  cgen.rs  →  printer.rs
(node)     (syntax)      (types)       (ownership)   (C)          (AST dump)
```

Add an expression kind and you add a parser arm, an `infer` arm, an escape arm, a
codegen arm, and a printer arm — plus, often, traversal arms in cgen's five
recursive walkers (`find_calls_expr`, `collect_structs_in_expr`, `find_closures_expr`,
`find_spawns_expr`, `collect_refs`). So **two sessions both adding language features
will conflict in these files.**

The good news: the conflicts are almost always **additive** (a new `enum` variant, a
new `match` arm, a new function) rather than edits to the same lines. They resolve
cleanly by hand, and the compiler *tells you what you missed*: Rust's exhaustive
`match` makes a forgotten arm a compile error, and `cargo test` (141 tests, every
feature has one) catches behavior regressions. Nothing merges silently-wrong.

### Strategy
1. **One git worktree/branch per session.** `git worktree add ../jestyr-loops` etc.
   so each session has its own checkout; merge to `main` and run `cargo test` after
   each.
2. **Land AST *shape* changes first, rebase others on them.** The single highest-
   conflict change is altering a struct/variant's *fields* (it breaks every `match`
   on it). If a stream needs to add a field to `ExprKind::For` or `FnDecl`, do that
   first and let other streams pull it in.
3. **Prefer adding new functions/arms over editing shared ones.** New `emit_*`
   helpers and new `match` arms merge far better than rewrites of existing logic.
4. **After every merge: `cargo build` (fix exhaustiveness) then `cargo test`.** Keep
   the build warning-clean and all examples byte-identical (the invariant).

### Conflict-isolation ranking (what's safe to parallelize)

| Risk | Streams | Why |
|---|---|---|
| **LOW** (run freely in parallel) | **Stdlib in Jestyr** (`examples/std/*.jtr` — *zero* Rust-file conflict); **Comments/docs** (lexer + a new doc-gen file); **Diagnostics polish** (`diag.rs`); **Tooling** (formatter / test-runner / doc-gen as new binaries) | touch isolated files or only `.jtr` source |
| **MEDIUM** | **Attributes** (`parse_attrs` + a localized cgen block); **Module v2** (mostly `module.rs`); **Error-handling polish**; **Concurrency polish** | touch the core files but in narrow, well-separated spots |
| **HIGH** (sequence, or expect merges) | **Loops**, **Structs/Enums**, **Traits/dyn**, **CTFE**, **Function-pointer types**, **Strings (real `String`)**, **Numeric/operator work** | each adds AST nodes and threads through all six files |

**For your stated plan** (loops ‖ structs/enums ‖ comments ‖ attributes): *comments*
and *attributes* are safe to run alongside anything. *Loops* and *structs/enums* are
**both HIGH-conflict** and will collide in `ast.rs`/`typeck.rs`/`cgen.rs` — run them
on separate worktrees and merge one, then rebase the other (or sequence them). The
truly-free parallel lane nobody competes on is **growing the stdlib in Jestyr**.

---

## 2. Workstream map

| # | Workstream | Status | Conflict | Size | Completeness | Motley path |
|---|---|---|---|---|---|---|
| A | Loops | ~90% | HIGH | S–M | ✓ | — |
| B | Structs & enums | ~75% | HIGH | M | ✓✓ | — |
| C | Comments & doc-comments | ✅ DONE | LOW | S | ✓ | — |
| D | Attributes | ~50% | MED | S | ✓ | ✓ (provenance hooks) |
| E | Real strings / text | ~25% | HIGH | L | ✓✓ | ✓✓ (self-host) |
| F | Traits / `dyn` | 0% | HIGH | L | ✓✓ | ✓✓ (pass interfaces) |
| G | CTFE + reflection | ~10% | HIGH | L | ✓ | ✓✓ (IR builder) |
| H | Function-pointer types | 0% | HIGH | M | ✓ | ✓ (vtables) |
| I | Error-handling polish | ~70% | MED | S | ✓ | — |
| J | Numeric / operator completeness | ~70% | MED | S–M | ✓ | ✓ (determinism) |
| K | Module system v2 | ~98% | MED | M | ✓ | ✓✓ (build/incremental) |
| L | Memory-layout pass | 0% | MED | M | ✓ | ✓✓ (mem-efficiency) |
| M | `@verified` (SMT) | 0% | HIGH | XL | ✓ | ✓✓ (verify passes) |
| N | Concurrency polish | ~100% | MED | M | ✓ | — |
| O | Tooling (fmt / test / doc / LSP) | test-runner ✅, attest ✅, attest --diff ✅ (all three also SELF-HOSTED ✅); `doc` port open; LSP/fmt deferred | LOW | M | ✓✓ | ✓ |
| P | Self-hosting | ✅ COMPLETE — fixed point, driver, in-language modules, bootstrap seed | — | XL | ✓✓ | ✓✓ (the gate) |
| Q | Parallelism (data-parallel) | ~45% (SOACs + `par for` surface + `@span` cost model ✅) | MED | L | ✓✓ | ✓✓ (cost model) |

---

## 3. Workstreams in detail

### A. Loops — ~90% (HIGH conflict; files: ast, parser, typeck, escape, cgen, printer)
**Done:** unified `for` (range/`0..=`/slice/cond/infinite/`for _`), `read`/`mut`
bindings, bounds-check elision, `invariant`, iterator-invalidation rejection,
element+index, lockstep zip, region scratch, `@no_panic`, labeled break/continue,
step/descending ranges, `variant`. See [`docs/loops.md`](docs/loops.md) +
[`docs/loops-spec.md`](docs/loops-spec.md).
**Left:** `take`-iteration (blocked on F — an owned-iterable protocol); value-yielding
loops (`let x = for { break v }` — blocked on expr-position control flow, same
machinery `if`/`match` need); loop-`else`; custom iterators (blocked on F/H);
Unicode string iteration (`for cp in text.codepoints()` — blocked on E).
**Note:** the cheap remaining loop wins are mostly *blocked* on bigger streams now.

### B. Structs & enums — ~75% (HIGH conflict; files: ast, parser, typeck, cgen)
**Done:** structs (fields, methods, generic + monomorphized, layout attrs), enums
(payload variants, tagged-union lowering, `match` + exhaustiveness + payload
projection).
**Left (high value, mostly independent tasks):**
- **`Self { … }` literals in the C backend** + **fallible methods** (`fn push() !{…}`)
  — the two gaps blocking the flagship `examples/vec.jtr` (HANDOFF §8).
- **Enum methods** and **explicit discriminants** (`enum E { a = 1 }`).
- **Struct field defaults** and **update/spread** (`P{ ..base, x: 1 }`).
- **`distinct` types** (keyword reserved) — newtypes with no implicit conversion.
- **`union`** (keyword reserved) — untagged unions for FFI/bare-metal.
- Opt-in **`Copy`** for user aggregates (today all user types are non-Copy).
**Why:** the single biggest "make the language feel finished" stream; `Self{}` +
fallible methods finally make `vec.jtr` run end-to-end.

### C. Comments & doc-comments — ✅ DONE (LOW conflict; files: lexer + new `doc.rs`)
**Done:** `//` line + nested `/* */` block comments (trivia); the **three doc-comment
tiers** — `///`/`/** */` (outer, document the next item), `//!`/`/*! */` (inner,
document the module) — collected by the lexer as **trivia** (`tokenize_with_docs`),
so docs never reach the parser and can never change parsing (the structural form of
"comments document; contracts prove"). A **doc generator** (`jestyrc doc <file>`,
`--html`) re-lexes, attaches each outer-doc block to the item/method below it
(dangling docs → warnings), takes `//!` as the module doc, splits prose into a
summary + `#`-headed sections + fenced examples, and reconstructs a **Guarantees**
block from the AST (`requires`/`ensures`, error set, `@no_panic`, refined params) —
the Jestyr-specific value-add that keeps machine-checked facts distinct from prose.
Emits Markdown or HTML. Demo `examples/docs.jtr`; reference `docs/comments.md`;
gotcha HANDOFF §5.30. **Left (deferred):** crawl `import`s into one doc site
(today single-file); *run* extracted examples as doctests; richer inline-markdown in
the HTML renderer.

### D. Attributes — ~50% (MED conflict; files: parser `parse_attrs`, ast, cgen)
**Done:** `@packed`, `@align(n)`, `@volatile`, `@address`, `@no_panic`, `@layout(c)`
(parsed; no-op marker). `parse_attrs` framework exists.
**Left:** a **general attribute representation** carried on all item kinds (today
some are read ad-hoc); more attributes — `@inline`/`@cold`/`@section(".x")`/
`@no_mangle`/`@deprecated`; attributes on enums and functions uniformly; validation
+ "unknown attribute" diagnostics. **Motley tie-in:** attributes are where
provenance/optimization hints will live. Fairly isolated → good parallel candidate.

### E. Real strings / text — ~25% (HIGH conflict; files: types, typeck, cgen, stdlib)
**Done:** `str` is `const char*`; byte iteration (`for c in text`) + `text.len`
(`strlen`); string literals.
**Left:** a **length-carrying string/`String` type** (a `{ptr,len}` like a slice,
not `strlen`-based); string **ops** (concat, slice, compare, find); **formatting**
(design §14); a **`StringBuilder`/owned text** type over the allocator; UTF-8 +
`codepoints()`. **This is on Motley's critical path** — a self-hosted compiler
manipulates source text constantly, and `strlen`-based `str` won't cut it.

### F. Traits / `dyn` — 0% (HIGH conflict; files: all six + a resolution pass)
**Design §7.3:** polymorphism via traits, *no inheritance*, **constraints checked at
definition** (the Zig fix). **Left:** trait declarations, `impl`, trait bounds on
generics, `dyn` trait objects (vtables — pairs with H). **Motley:** pass/analysis
interfaces are traits; the real dynamic `Allocator` vtable needs this. The biggest
single language gap.

### G. CTFE + reflection — ~10% (HIGH conflict; files: a new comptime interpreter + typeck/cgen)
**Design §8.** Only the generics slice (type-parameter substitution) exists. **Left:**
a small **comptime interpreter** over the AST; `comptime` blocks/consts; reflection
as comptime calls over type values; comptime codegen. **Motley:** the IR-builder
ergonomics and compile-time pass configuration lean on this.

### H. Function-pointer types — 0% (HIGH conflict; files: ast, parser, types, typeck, cgen)
**Left:** a `fn(T1, T2) -> R` *type*, fn values, indirect calls. **High leverage,
smaller than traits:** unblocks callbacks, a real fn-pointer-vtable `Allocator`
(retiring the enum-dispatch stand-in), and is a stepping stone to `dyn`. **Gotcha:**
calling a fn-ptr *field* (`a.alloc_fn(...)`) collides with method-call sugar in
typeck — needs the field-type to disambiguate.

### I. Error-handling polish — ~70% (MED conflict; files: typeck, cgen)
**Done:** error sets `T !E`, `?`, `ok`/`err`/`is_err`/`unwrap`. **Left:** error return
**traces** (Zig-style); **`catch`** (keyword reserved); fallible **methods** (overlaps
B); errors in more positions; richer error payloads.

### J. Numeric / operator completeness — ~70% (MED conflict; files: typeck, cgen)
**Left:** defined integer-overflow semantics (wrap/saturate/checked — **Motley
determinism** cares); float↔int cast edge cases; bit-width-aware literals; `as`
done; possibly operator methods. Determinism-relevant: no-FMA, compensated ops are a
*Motley* concern but the numeric model starts here.

### K. Module system v2 — ~98% (MED conflict; files: module.rs + typeck)
**Done:** `import`, `pub` visibility, qualified access, cycle detection, multi-file
merge; **per-module namespaces for functions + consts** (increment 1) — resolution
is keyed on `(ModId, name)`, an unqualified name resolves current-module-first and
cross-module access *must* be qualified (a sibling's name unqualified is now an
unresolved-name error), and colliding symbols are disambiguated in cgen via a
canonical name (`make` → `jestyr_make__m<mod>`) that is a **no-op for any
non-colliding program** (single- and multi-module C stays byte-identical — verified).
Two modules may now each define `make`/`get`/`helper` (incl. a *generic* `make`
alongside a *non-generic* one), which **cleared the logged self-host blocker**
(`intern` could not be imported beside `list`/`strmap`). **Qualified type paths
`mod.Type` / `mod.Type(args)`** (increment 2) — a new additive `TypeKind::Path`
arm, parsed in type position and lowered like `Name`/`App`, with a visibility audit
enforcing the type is `pub` in (and owned by) the target module (private / unknown /
unbound-module qualifiers all error). **Directory-as-module** (increment 3, design §9)
— `import "pkg"` where `pkg/` is a directory loads *all* its `.jtr` files as one
shared-namespace module (sorted for determinism); the loader now separates the
*module* id space (namespaces) from the *region* id space (source files) so
diagnostics still point at the exact file while items across a package's files share
one namespace (a private helper in one file is visible to its siblings; two files
defining the same name is a duplicate). **Module content-hashing** (increment 4, the
unique on-thesis feature) — every module gets a sha256 over its *normalized
post-parse form* (the sorted set of its items' pretty-printed renderings, so a
comment/whitespace-only edit or a declaration reordering does **not** change it,
while a semantic edit does) **combined with its imports' hashes** (so the hash is
transitive — a module's identity reflects every module it depends on). Stored in
`Modules.hashes`, exposed via `Modules::hash(m)`. Identical inputs ⇒ identical hash ⇒
the seed of provably-incremental + cacheable builds; pairs with O's `jestyr attest`.
**Manifest hash-verification** (increment 5) — `import "x" = "<sha256>"` pins a
dependency to an exact content hash; the loader verifies the imported module's
computed hash matches and errors on drift (lockfile-lite reproducibility; opt-in,
unpinned imports are never checked). **Collidable types** (increment 6) — two
modules may now each define the same **non-generic** `struct` / `enum` / `distinct`
type: `type_index`, the `variants` table, and `TypeDecl::name` are keyed by a type
`canon` (and a variant `canon`), an unqualified type/variant resolves
current-module-first, `mod.Type` resolves in the target module (the backend gets the
import map), and every `Jestyr_<type>` / enum-tag / variant site canonicalizes —
all a **no-op for any non-colliding program** (output byte-identical, verified). So
two modules' `Slot` become `Jestyr_Slot__m<a>` / `__m<b>` and each `mod.Slot` /
variant construction / `match` hits the right one. **Declarative module manifest**
(increment 8) — `Modules::render_manifest` emits the content-hash DAG (each module's
name + hash, imports tagged with their target's hash) as a deterministic, parseable
artifact, and `verify_manifest` re-checks a committed manifest against a fresh load,
reporting per-module (transitive) drift — the lockfile-lite declarative surface that
tooling reads without executing build code; pairs with O's `attest`. **Generic-enum
collisions** (increment 7) — two modules may each define the same `enum Box(T)`: the
`GenEnum` ctor is canonicalized at resolution so the monomorphized instances mangle
to distinct `Jestyr_Box__m<a>__i32` / `__m<b>__i32` (a shared canon-aware finder
resolves a collided generic enum by its canon key; a misclassified bare instance is
filtered out of struct-instance collection). No-op + byte-identical for the
non-colliding generics (`core`'s `Option`/`Result`). **Left:** the *executable*
`build.jestyr` half (deferred: needs CTFE; lockfile/vendored-deps/effects —
ecosystem-premature). **Also deferred:** *generic-struct* collisions (the
comptime-fn-form ctor lives in the function namespace; its instance mangling would
need the `dup_fns` canon — same pattern, no blocker).
**Motley:** the DAG already enables the parallel/incremental-compilation story;
hashing makes it provable.

### Debug info (`#line`→DWARF) — ✅ DONE (LOW conflict; files: types, typeck, cgen, main)
Systems-handoff §1. Emitted C carries `#line N "file.jtr"` (per-function, per-statement,
and on `requires`/`ensures` asserts) and cc is invoked with `-g`, so debuggers/profilers
map the binary back to `.jtr`. Seam: `TypeInfo.debug` (empty on the single-file path ⇒
no `#line` ⇒ byte-identical there; only the loader path emits). `-g` is separate from the
`CC_FLAGS` determinism/attest seam. Full test rigor + teeth. See `jestyr-debuginfo.md`.
**Next systems items (untouched):** cross-compile (`--target` via `zig cc`), then L, then M.

### L. Memory-layout pass — 0% (MED conflict; files: a new analysis + cgen)
**Design §16 / a Motley principle.** **Left:** a layout pass computing size/align,
**field reordering**, **enum niche-packing**, pass-large-aggregates-by-`const*`
(today `read` params copy). **Directly serves Motley's "memory efficiency at every
layer."** Mostly a new pass + cgen tweaks → reasonably isolated.

### M. `@verified` (SMT) — 0% (HIGH conflict; XL; files: a new verifier + typeck)
**Design §7 ceiling (ATS/Ada).** Turn `requires`/`ensures`/`invariant`/`variant`
from runtime asserts into **static proof obligations** discharged by an SMT backend.
**Motley:** verifying the compiler's own passes. Long-horizon; do after F/G.

### N. Concurrency polish — ~100% (MED conflict; files: ast, parser, typeck, escape, cgen, printer)
**Done:** `concurrent { spawn … }` → pthreads, scoped join; atomics (`__atomic_*`);
`core.par_binned_sum`; **Mutex** (protected object); **move-only channels**; the
generalized **`par_reduce`** library; **task results + `await`**; the headline **`par for …
reduce(r)`** surface with **compile-time non-deterministic-reduction rejection**; **dynamic-N
spawn**; the **`@deterministic`** schedule-independence contract; **`select`** over channels
(below). **Left (nice-to-have):** `spawn` of closures; multi-type / send-arm `select`.

**`select` over channels — ✅ DONE (Crystal/Go CSP ergonomics).** `select { recv(ch) => x { … }
… }` waits on several `Channel(i64)` and runs the arm of whichever has a value ready. New
`ExprKind::Select` + `SelectArm` threaded through all six files; new `select` keyword, contextual
`recv`. Lowering: hoist each channel, then spin with an `else if` chain (exactly one arm per
pass) calling the non-generic `channel_len_i64`/`channel_recv_i64` wrappers added to
`std/sync.jtr`. Single-consumer, recv-only, `Channel(i64)` for now; forbidden in a
`@deterministic` function (its choice depends on the schedule).
- `examples/std/select.jtr` — two spawned producers fill two channels; the main thread drains
  all four via `select` → `66` (order-independent sum, deterministic). `module.rs`/`main.rs`
  untouched.
- Rigor: parser test (`Select` + arms); typeck reject test (a non-`Channel(i64)` arm errors);
  cgen lowering test (the poll loop + i64 wrappers); a c-oracle `select_demo` (×8, pinned `66`).

**`@deterministic` contract — ✅ DONE (the `@verified` tie-in).** A `@deterministic` function is
certified **schedule-independent**: the escape checker forbids the raw concurrency primitives
whose result can depend on the thread schedule — `concurrent`/`spawn` and the `atomic_*` ops —
permitting parallelism only through the *checked* deterministic `par for … reduce(r)`. The
Ada/Ravenscar provable-subset idea fused with the determinism thesis. Mirrors the `@no_alloc`
machinery (a per-function escape flag + per-op rejection; transitive "calls a function that uses
atomics" closure is future work, as for `@no_alloc`). The attribute was already reserved; now
`Active` — schedule-determinism is a facet of "deterministic" that composes with the numerics
allocator-determinism facet (both only *reject* non-deterministic code).
- `examples/std/deterministic.jtr` — a `@deterministic` sum-of-squares (its only parallelism a
  checked `par for`) → `385`. Rigor: escape tests (**accepts** a `par for`; **rejects** raw
  `concurrent`; **rejects** `atomic_*`) + a c-oracle `deterministic_demo`. `attrs.rs` flip to
  `Active`; `module.rs`/`main.rs` untouched.

**Dynamic-N spawn — ✅ DONE (increment 6, the shared `emit_concurrent` change).** A `spawn`
*inside a loop* now launches a **runtime** number of tasks: the `concurrent { … }` nursery
collects them on a **growable handle array** (`_dt`/`_da`) and joins them all at the brace —
structured concurrency with a dynamic worker count, the building block Q's `with
schedule(threads, chunk)` split needs. Each task's arg box is heap-allocated (a stable address
the thread reads, since the arrays may `realloc`-move) and freed after its join. Coexists with
the fixed numbered-handle path (top-level `spawn` / `let h = spawn`); a spawn nested in a loop
*or* an `if` triggers the dynamic path. `module.rs`/`main.rs` untouched.
- `examples/std/dynamic_spawn.jtr` — a runtime worker count (10, then 64) each writing a
  disjoint slot, summed deterministically → `285`, `85344`.
- Rigor: cgen lowering test (growable array + heap arg boxes + join-and-free loop); a
  `--features c-oracle` real-thread `dynamic_spawn_demo` (×8 at up to 64 threads, pinned).
  The existing escape data-race rule still guards each spawn (no shared `mut` slice).

**`par for … reduce(r)` — ✅ DONE (increment 5, THE headline; shared surface with Q).**
`par for x in xs reduce(r) { body }` maps each element through `body` and reduces the results
in **parallel**, with the marquee *checked* guarantee: the compiler **accepts only declared
deterministic reductions** (the `core` built-ins `sum`/`min`/`max`/`xor`) and **rejects a
non-deterministic one at compile time** — "parallelism that cannot change your answer," a
compile error if you try to violate it. New `ExprKind::ParFor` threaded through all six files;
new `par` keyword + contextual `reduce`. Desugars onto `core.par_reduce` (the tested
deterministic engine): a serial element-wise map (always deterministic) into a scratch `[]i64`,
then the parallel reassociation-sensitive reduction — bit-identical to serial for any schedule.
- The check (typeck): the `reduce(r)` constructor name must be in the declared-deterministic
  allowlist; anything else errors with a diagnostic explaining the reassociation hazard. (A
  `@deterministic` attribute admitting *user* reductions is future work; today the trusted set
  is the four `core` built-ins.) `[]i64` element/`i64` body/`i64` result for now.
- `examples/std/par_for.jtr` — a parallel sum-of-squares of 1..=13 (819), bit-identical to the
  serial fold (1), and a parallel max (13). `module.rs`/`main.rs` untouched.
- Rigor: parser test (`ParFor` parses); typeck tests (**accepts** a deterministic reduction →
  `i64`; **rejects** a non-deterministic one — the headline, **teeth-verified by mutation**:
  relax the allowlist → the reject test fails); cgen lowering test (serial map + `par_reduce`
  call); `--features c-oracle` real-thread `par_for_demo` (×8, pinned `819 1 13`). Determinism
  itself is inherited from `core_props::par_reduce_is_split_independent` (the engine).

**Task results + `await` — ✅ DONE (increment 4, the first non-library slice).** `let h =
spawn f(args)` now binds an awaitable handle of type **`Task(T)`** (T = f's return); `await h`
joins the task and yields its result. The first increment to thread a new AST node + a new
`Ty` through all six files. Lowering: the per-site task box gains a `ret` field the trampoline
writes (`_a->ret = f(args)`); `await h` is a statement-expr `({ if(!_jd) {join; _jd=1;}
_ja.ret; })` that joins-once (a `_jd` flag guards against the nursery's safety-net brace-join,
which is now conditional for awaitable handles). Bare `spawn` stays fire-and-forget.
- `await` parses at the **postfix** level so it binds tighter than `as`/binary: `await a +
  await b`, `await t as i32` parse as expected. `Ty::Task(Box<Ty>)` is non-`Copy` and never
  materializes as a runtime value (resolved to thread vars in the `concurrent` scope), so
  `c_type`'s catch-all suffices — only `is_copy`/`display` needed the new arm.
- `examples/std/await.jtr` — two tasks compute disjoint partial sums-of-squares in parallel,
  `await` combines them (385); a single awaited task (14). `module.rs`/`main.rs` untouched.
- Rigor: parser tests (binding+await; **precedence** `await a as i32` = `(await a) as i32`);
  typeck tests (`spawn`→`Task(i64)`, `await` unwraps; **awaiting a non-task is rejected**);
  cgen lowering test (ret field, store, guarded join, `.ret` read) **teeth-verified by
  mutation** (drop the store → test fails); `--features c-oracle` real-thread `await_demo`
  (×8, pinned `385 14`). `cargo test` stays toolchain-free.
- Constraint surfaced: `await` resolves a handle bound in the same `concurrent` scope (a
  handle is await-only — not stored/passed elsewhere); cross-scope handles are future work.

**Task results + `await` — ✅ DONE (increment 4, the first non-library slice).** `let h =
spawn f(args)` now binds an awaitable handle of type **`Task(T)`** (T = f's return); `await h`
joins the task and yields its result. The first increment to thread a new AST node + a new
`Ty` through all six files. Lowering: the per-site task box gains a `ret` field the trampoline
writes (`_a->ret = f(args)`); `await h` is a statement-expr `({ if(!_jd) {join; _jd=1;}
_ja.ret; })` that joins-once (a `_jd` flag guards against the nursery's safety-net brace-join,
which is now conditional for awaitable handles). Bare `spawn` stays fire-and-forget.
- `await` parses at the **postfix** level so it binds tighter than `as`/binary: `await a +
  await b`, `await t as i32` parse as expected. `Ty::Task(Box<Ty>)` is non-`Copy` and never
  materializes as a runtime value (resolved to thread vars in the `concurrent` scope), so
  `c_type`'s catch-all suffices — only `is_copy`/`display` needed the new arm.
- `examples/std/await.jtr` — two tasks compute disjoint partial sums-of-squares in parallel,
  `await` combines them (385); a single awaited task (14). `module.rs`/`main.rs` untouched.
- Rigor: parser tests (binding+await; **precedence** `await a as i32` = `(await a) as i32`);
  typeck tests (`spawn`→`Task(i64)`, `await` unwraps; **awaiting a non-task is rejected**);
  cgen lowering test (ret field, store, guarded join, `.ret` read) **teeth-verified by
  mutation** (drop the store → test fails); `--features c-oracle` real-thread `await_demo`
  (×8, pinned `385 14`). `cargo test` stays toolchain-free.
- Constraint surfaced: `await` resolves a handle bound in the same `concurrent` scope (a
  handle is await-only — not stored/passed elsewhere); cross-scope handles are future work.

**`par_reduce` library — ✅ DONE (increment 3, the headline at library tier).** `core.jtr`'s
`par_reduce(s, r)` generalizes `par_binned_sum`'s disjoint-region shape to any reduction
declared as a value: a `Reduction` carries an `identity`, an `accumulate` (fold one element),
and an order-independent `combine` (merge two accumulators). Workers fold disjoint chunks into
disjoint slots; the slots merge with `combine`; the result is **bit-identical to the serial
fold for any chunk split or thread schedule** — because integer +/min/max/xor are associative
*and* commutative. Built-ins: `sum_reduction`/`min_reduction`/`max_reduction`/`xor_reduction`,
plus `serial_reduce` (the in-program oracle). **Zero compiler change.**
- The accumulator is `i64` so the worker stays monomorphic (`spawn` targets cannot be generic
  — verified: a generic worker fails C lowering). A naive `f64 +` reduction is *deliberately
  not* offered — it reassociates and is the rejection target for the `par for` surface; the
  bit-exact float case remains `par_binned_sum`.
- `examples/std/par_reduce_int.jtr` — sum/min/max/xor of 1..=17 (`153 1 17 1`) + four
  par==serial flags. `module.rs`/`main.rs` untouched.
- Rigor: the determinism **star** `core_props::par_reduce_is_split_independent` (for each
  built-in, whole-fold == chunked-fold-then-merge for any split, mirroring
  `binned_sum_is_chunk_independent`) + a `--features c-oracle` real-thread run
  (`par_reduce_int_demo`, pinned cross-OS). `cargo test` stays toolchain-free.

**Move-only channels — ✅ DONE (increment 2, share by communicating).** `std/sync.jtr`'s
`Channel(T)` is a bounded ring buffer over the spinlock whose `channel_send` takes its value
by **`take`**: ownership *moves into* the channel, so no alias survives in the sender.
Race-freedom then falls out of the *existing* escape analysis — no `Send`/`Sync`, no runtime
detector: the give-away rule forbids handing a *borrow* to a `take` parameter, so you can
only send what you own (Erlang/Pony share-nothing via Jestyr's second-class refs; a `take`
value ≈ Pony `iso`). `channel_recv` reads out under the lock; bounded capacity gives natural
backpressure. Pure library Jestyr — **no compiler change** beyond one escape fix.
- Escape fix (additive): the give-away route (route 4) now resolves **module-qualified**
  callees via `info.qualified`, so a `take` param reached through `mod.f(T, take v)` — e.g.
  every channel `send` — is checked. Previously a qualified generic call silently skipped it.
  Teeth-verified: before the fix, sending a borrow compiled; after, it errors at the arg.
- `examples/std/channel.jtr` (multi-producer fill+drain → 264; cap-2 concurrent
  producer+consumer with real backpressure → 36). `module.rs`/`main.rs` untouched.
- Rigor: toolchain-free wiring (`channel_example_compiles_clean`) + **move-on-send soundness**
  (`qualified_take_of_borrow_is_rejected`, with `…_of_owned_is_accepted` as its teeth) + a
  pure-Rust **ring-buffer model property** (`channel_ring_preserves_every_value`: every sent
  value received exactly once, any capacity/interleaving) + a `--features c-oracle` real-thread
  run (`channel_demo`, ×8, pinned to `264 36`). `cargo test` stays toolchain-free.

**Mutex — ✅ DONE (increment 1, the Ada-style protected object).** `std/sync.jtr`'s
`Mutex(T)` bundles the guarded value, its lock, and the operations as one unit: the only
way to reach the value is `mutex_with`/`mutex_get`, each bracketing the access between lock
acquire/release — so you *cannot forget to lock*, mutual exclusion is structural. The lock
is a **test-and-set spinlock over one atomic `int64`** (one new intrinsic, `atomic_xchg` →
`__atomic_exchange_n`), so the whole primitive is library Jestyr over the existing atomics —
no OS mutex, no special type, portable. Shared by-value across a `concurrent { spawn … }`
nursery (the pointers alias deliberately; the lock serializes), and freed once after the
scope joins.
- New `atomic_xchg` cgen intrinsic + `atomic_intrinsic_ret` (atomics now type as `i64`, so
  the spinlock's `atomic_xchg(lock,1) != 0` test needs no cast); `examples/std/sync.jtr`
  (the protected object) + `examples/std/mutex.jtr` (8 threads → exactly 8). No new syntax;
  `module.rs`/`main.rs` untouched.
- Rigor: cgen unit (xchg → `__atomic_exchange_n`) + toolchain-free wiring (`mutex_example_
  compiles_clean`) + a pure-Rust **mutual-exclusion property** (`tas_lock_serializes_
  increments`: a model of the emitted TAS lock + guarded counter ends at exactly `n*k` for
  *any* interleaving) teeth-verified by `unlocked_increments_lose_updates` + a
  `fuzz_concurrency_pipeline` bolero target + a `--features c-oracle` 8-thread run
  (`mutex_demo`, repeated 8×, pinned to `8`). `cargo test` stays toolchain-free. Teeth:
  breaking `lock_acquire` drops the live demo to a non-deterministic <N (verified, reverted).

### O. Tooling — in progress (LOW conflict; files: new binaries/subcommands)
**Design §15: one `jestyr` binary.** Each tool is a largely new file/subcommand →
**excellent parallel candidates** that barely touch the core.

**Test-runner polish — ✅ DONE (increment 1).** `jestyrc test` gained codegen-time
name filtering and a `--list` mode:
- `jestyrc test <file> <substr>` bakes only the `@test`/`@bench` items whose name
  *contains* the substring (so `running N test(s)` and the pass/fail exit code reflect
  the filtered roster). Filtering is at codegen, not via `argv`, so the unfiltered
  harness is byte-for-byte unchanged (`emit_tests(x) == emit_tests_filtered(x, None)`).
- `jestyrc test <file> --list` prints the runnable test/bench names (one greppable
  `test <name>` / `bench <name>` line each, source order) — toolchain-free, no compile.
- New `cgen::{emit_tests_filtered, list_tests, TestKind}` (the discovery predicate
  mirrors `test_main`'s `runnable` exactly: skips generic/unsupported `@test`s).
- Rigor: unit + wiring + golden + property (`arb_test_program` — discovery
  soundness/completeness, filter soundness, determinism) + a `fuzz_test_runner` bolero
  target + a `--features c-oracle` end-to-end run of the filtered harness; every
  property teeth-verified by mutation. `cargo test` stays toolchain-free.

**`jestyrc attest` — ✅ DONE (increment 2, the headline).** A sound reproducible-build
+ machine-checked-guarantee manifest (`jestyr-attest/v1`): the **SHA-256 of the emitted
C** (a real attestation — codegen is a *proven* deterministic function of the source,
locked by `CC_FLAGS`/the FP seam/the cross-OS canary), the **locked compile command**,
and **every top-level item's machine-checked guarantees** (`requires`/`ensures`/error
set/`@no_panic`/refined params) reconstructed from the AST by the *same*
`doc::fn_guarantees` the doc generator uses — so the attested behavioral ABI can never
drift from the rendered docs. This is what `cargo-semver-checks` structurally cannot do:
the contracts *are* the public ABI.
- New `src/attest.rs` (`manifest`) + `src/sha256.rs` (the canary's dep-free SHA-256
  lifted to a shared non-test module, so both consumers hash with one self-tested copy);
  `Mode::Attest` in main.rs; a behavior-preserving `pub(crate)` bump on the `doc.rs`
  signature/guarantee helpers.
- Rigor: unit + wiring + a **pinned golden** on `examples/docs.jtr` (every byte fixed;
  hash cross-checked against an independent SHA of the emitted C) + property
  (determinism incl. the hash, hash==emitted-C digest, completeness, guarantee fidelity
  vs the doc extractor) + a `fuzz_attest` bolero target; every invariant teeth-verified
  (hash the wrong bytes → 4 fail; drop guarantees → 4 fail; skip an item → completeness
  fails). Fully toolchain-free (the C is *hashed*, not built).

**`jestyrc attest --diff <old> <new>` — ✅ DONE (increment 3).** A *sound* semantic
breaking-change detector: parse two manifests back into structured contracts and classify
every per-item change. **Breaking** = error added / `requires` strengthened / `ensures`
dropped / `@no_panic` lost / refinement narrowed / `pub` item removed or demoted / type
signature changed. **Compatible** = each dual. Sound by construction — only *provably*
compatible changes get the compatible verdict; anything a heuristic can't prove safe
(e.g. a non-literal refinement change) defaults to breaking. Exits non-zero iff any
breaking change (a drop-in CI ABI gate). New `attest::{parse_manifest, diff, DiffReport,
Verdict}`; `run_attest_diff` in main.rs. Rigor: a unit test per rule (one edit at a
time), a pinned multi-edit golden, a self-diff wiring test, properties (reflexivity:
self-diff empty; sharpness+soundness: one edit → exactly one correctly-classified change;
direction asymmetry: swapping old/new flips the verdict) + a two-layer `fuzz_attest_diff`
(parser total on arbitrary bytes; classifier total + reflexive on real manifests); every
rule teeth-verified (flip error-add → 3 fail; neuter `range_widened` → 2 fail).

**Left:** eventually an **LSP** (deferred — needs an incremental query layer). (Deferred:
a faithful `fmt` — the lexer discards comments/layout; `printer.rs` is a debug printer.
See `TOOLING-HANDOFF.md` "What NOT to build".)

### P. Self-hosting — plumbing landed; the port itself remains (the gate; XL)
**Design §19 / Motley prerequisite.** Rewrite the Jestyr compiler in Jestyr. The
language prerequisites are now largely met — E (real strings) ✅, F (traits) ✅, H
(fn-pointer types) ✅ — and the three OS-/stdlib-facing **self-hosting unblockers** a
compiler can't run without are now built (each with a demo + gcc-oracle test):
- **File I/O** ✅ — `read_file`/`write_file`/`file_exists`/`remove_file` intrinsics +
  `std/fs.jtr` (`fs.read_text`/`write`/`exists`/`remove`). Read `.jtr` source, emit output.
- **Command-line args** ✅ — `main(argc, argv)` capture + `arg_count`/`arg` intrinsics
  + `std/env.jtr` (`env.argc`/`env.argv`). Learn *which* file to compile.
- **A symbol-table map** ✅ — `std/strmap.jtr`, an open-addressing `str -> i64` table
  (FNV-1a + SplitMix64, owned keys, RAII), the deliberate cache-friendly/deterministic
  alternative to a chaining hashmap. Every pass past the lexer needs one.
- **A string interner** ✅ — `std/intern.jtr`, str→dense id + id→str (inline
  open-addressing, single-copy, RAII). Makes downstream tables integer-keyed (rustc's
  `Symbol`). Was inlined rather than nesting a `StrMap` because Jestyr used not to
  auto-drop struct fields — **now fixed (B1):** RAII recurses into owned struct fields
  and live enum payloads (reverse declaration order, after the value's own `drop`), so
  a nested `Drop`-having field is freed automatically. A container-of-containers no
  longer leaks; the inline interner stays as a single-copy optimization, not a
  necessity. See `DROP-ALLOC-PHASE3.md` (field/payload drop) + `examples/drop_nested.jtr`.

**Vertical slice — ✅ DONE:** `examples/std/lexer.jtr` — a lexer *written in Jestyr*,
composing `fs` (read source) + `env` (argv) + `intern` (keyword/id classification).
Lexes a built-in sample deterministically and a real file from disk (gcc-oracle tested).
This is the front-end-in-Jestyr proof.

**P1 — full token set: ✅ DONE.** The Jestyr lexer now matches the Rust reference
(`src/lexer.rs`) **token-for-token**: string/char literals with `\` escapes, f-strings,
decimal/hex(`0x`)/binary(`0b`) ints with `_` separators, floats (fraction + `e`/`E`
exponent), nested `/* */` block comments, and every multi-char operator (`->`,`=>`,`::`,
`..`,`..=`,`.*`,`<<`,`>>`,`==`,`!=`,`<=`,`>=`,`&&`,`||`, compound-assigns). The
acceptance test (`jestyr_lexer_matches_reference_on_corpus`, `--features c-oracle`)
cross-checks the Jestyr lexer's lexeme stream against the Rust lexer over the **whole
122-file corpus** — all identical — plus a per-token-class probe.

**P2a — fully-classified token stream: ✅ DONE.** The Jestyr lexer's keyword set is now
complete (all 56 `TokenKind::keyword`s), it distinguishes Int vs Float, and a kind-dump
mode (`lexer.exe <file> kinds`) emits each token's kind label matching
`TokenKind::describe`. `jestyr_lexer_kinds_match_reference_on_corpus` cross-checks the
*token kinds* (keyword vs ident, int vs float, every operator) against the reference over
the whole 122-file corpus — all classified identically. This is the classified token
stream the parser consumes. Next: **P2 parser** (AST-dump golden) → P3 typeck → P4 escape
→ P5 cgen → **R2** fixpoint.

**Surfaced by the slice (gaps, with status):** ~~Jestyr doesn't auto-drop **struct
fields**~~ — **fixed (B1):** RAII now recurses into owned struct fields and live enum
payloads, so containers-of-containers free automatically (no manual frees). ~~`unsafe {}`
isn't a valid `let` initializer~~ — **fixed (B4):** a value-position `unsafe { … }` (and a
plain `{ … }`) yields its tail expression, so `let v = unsafe { p.* }` works inline
(`unsafe` is a compile-time marker — zero runtime effect; `examples/std/unsafe_init.jtr`).
**Per-module
namespaces** (K)
used to bite as soon as two std modules shared a helper name (`make`, `destroy`,
`hash_str`, …) — **fixed (increment 1):** functions/consts are now per-module, so
`intern` imports cleanly beside `list`/`strmap` and shared helper names no longer
collide. (`mod.Type` paths + directory-as-module are the remaining K niceties.)

**Still open before a full self-host:** extend the lexer to the *full* token set
(floats/hex, block comments, strings, all operators) → port the parser → typeck →
escape → cgen (~27K lines); plus qualified type paths + a basic `build.jestyr` (K) for
comfort. ~~**Plumbing follow-up:** a recoverable `read_file -> String !IoError`.~~
**Done (B3):** `try_read_file -> String !IoError` intrinsic + `fs.try_read_text`
wrapper — a missing/unreadable file takes the `err` branch (compose with `?`/`unwrap`),
not a silent empty String (`examples/std/try_read.jtr`). The `try_read_file` runtime
helper + `JestyrResult_String` typedef are emitted only when used (byte-identical
otherwise). **Ergonomic gaps B4 (unsafe/block as `let` initializer) and B5 (inline
`slice(T,…)` typing) are also done** — the three Tier-2 self-host unblockers are cleared;
only the port (P1–P5) + the fixpoint test (R2) remain.

### Q. Parallelism (data-parallel) — ~45% (MED conflict; files: ast, parser, typeck, escape, cgen, printer + new `std/parallel.jtr`)
**Distinct from N (concurrency = task structuring); Q = data parallelism (make one
computation faster across cores / lanes / GPU).** They share exactly one bridge: the
deterministic `par` reduction. **Seed already in-tree:** `core.par_binned_sum` —
bit-identical-to-serial parallel sum via disjoint-region binning. **Headline:**
schedule-independent parallelism as a *checked* guarantee — separate the algorithm from
the schedule (threads/chunk/lane-width) and the compiler **guarantees a bit-identical
result for every legal schedule** (Halide's idea, made general + checked; impossible
elsewhere because IEEE-754 reassociation breaks it — Jestyr's FP contract + binned
reductions + SHA canary make it real). **Increment order:** `par_reduce` library (no
compiler change) → `par_map`/`par_scan` → `par for … reduce(r)` surface +
non-deterministic-reduction rejection → the `with schedule(...)` split (needs dynamic-N
spawn, shared with N) → a **work-span (`W`/`D`) cost model** (`@span(log n)` checked,
+CJC thermal/energy — the Motley tie-in) → far-tier SIMD (`uniform`/`varying` + lane
reductions bit-identical across vector widths) + GPU SOACs. **Coordinate the `core.jtr`
`par_*` region with N.** Full handoff: `PARALLELISM-HANDOFF.md`.

**Built (tier 1 — library, no compiler change):** the three workhorse SOACs, all
deterministic by construction.
- **`core.par_reduce`** (landed via N) — fold a slice to one value over a `Reduction`
  (identity + associative `accumulate`/`combine`; built-ins sum/min/max/xor). Disjoint
  per-worker slots, merge with `combine` — bit-identical to serial for any split.
- **`parallel.par_map`** — element-wise `fn(i64)->i64` across four workers into disjoint
  output regions, no merge (output[i] depends only on input[i] → split-independent by
  construction).
- **`parallel.par_scan`** — inclusive prefix scan via the two-pass algorithm (reduce each
  chunk → exclusive-prefix the chunk-totals → re-scan each chunk seeded with its prefix);
  bit-identical to serial for any worker count *because the op is associative*. Takes the
  op + identity directly (decoupled from `core.Reduction`'s module-private fields) with
  `op_add/min/max/xor` + `par_scan_sum/min/max/xor` wrappers; naive float `+` is non-
  associative and deliberately not offered. New module `examples/std/parallel.jtr` (does
  not touch `core`'s `par_*` region — coordinated with N). Demo `par_soac.jtr`. Tests:
  `parallel_props::{par_scan,par_map}_is_split_independent` (toolchain-free determinism
  stars), `par_soac_example_compiles_clean`, `c_oracle::par_soac_demo` (gcc + real threads,
  8×).

**Built (tier 2-3 — compiler):** the `par for … reduce(r)` surface (`ExprKind::ParFor`,
desugars onto `core.par_reduce`, compile-time rejection of a non-declared reduction) **landed
via the N session** (the N/Q overlap; `par` is a contextual keyword). Q **pinned its
determinism in the cross-OS SHA canary** (an i64 sum/min/max/xor `par for` section in
`numerics_canary.jtr`; the realized values are locked, so a schedule-dependence break flips
the digest — master `a7b9f18`).

**Built (tier 5 — the cost model, Q-distinct, Motley tie-in):** **`@span(<class>)`** — a
*checked* asymptotic bound on a function's parallel **span** (depth). `attrs::validate_fn`
computes the body's span from its loop structure as `n^k·(log n)^j` — a sequential loop ×`n`,
a `par for … reduce(r)` contributes `log n` — and rejects a body whose span exceeds the
declared class (`constant`/`log`/`linear`/`linearithmic`/`quadratic`). So serializing a
reduction (`par for` → `for`, span `log n → n`) is a **compile error, not a silent
regression** — the Cilk/NESL work-span idea as a contract. Intraprocedural v1 (a call is
O(1)); in `attrs.rs` (no new pass — `main.rs` owns the driver). Demo `par_cost.jtr`; tests
`cost_model::*` (rejection-soundness stars) + `c_oracle::par_cost_demo`. **Next (non-
overlapping):** layer CJC **thermal/energy** onto `@span`; the `with schedule(...)` split (now
mostly enabled by N's dynamic-N spawn); far-tier SIMD + GPU SOACs.

---

## 4. Recommended sequencing (priority order)

**To make Jestyr a complete language:** B (structs/enums — finish `vec.jtr`) → E
(real strings) → F (traits) → C/D (comments/attributes, in parallel any time) →
I/J (error/numeric polish) → O (tooling).

**To advance Motley specifically:** E (strings) → F (traits, for pass interfaces) →
G (CTFE, for the IR builder) → L (memory-layout pass) → K-v2 (build/incremental) →
P (self-hosting) → then port CANA/PINN/NSS (MOTLEY Part III) → M (`@verified`).

**The single highest-leverage next item** is **E (real strings)** — it unblocks
self-hosting, finishes the loop story (Unicode iteration), and is on Motley's path.
**F (traits)** is the biggest *language* gap. **H (fn-pointer types)** is the cheapest
high-leverage win (unblocks the real allocator vtable and seeds traits/`dyn`).

---

## 5. Motley critical path (the long game)

From [`MOTLEY.md`](MOTLEY.md): Motley is meant to be **written in Jestyr**, so the
Jestyr items on its path are: **E (strings)**, **F (traits — pass/analysis
interfaces)**, **G (CTFE — IR builder)**, **L (memory layout)**, **K-v2
(build/incremental)**, and **P (self-hosting)** — then the novel work (CANA/PINN/NSS
thermal+energy cost models) ports over from CJC-Lang, mostly *adapted not invented*.
Determinism (J: numeric semantics; no hidden state) is the spine throughout. Keep the
"every feature ships with a demo + tests, build stays warning-clean" discipline — it
is exactly the reproducibility culture Motley needs.
