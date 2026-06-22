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

Working bootstrap compiler in Rust (~10K LOC, 141 tests, warning-clean) taking
`.jtr` → C → native binary. Pipeline: **load (multi-file) → lex → parse →
resolve+typecheck → ownership/escape → C codegen → cc**. Done: the tiered reference
model (`&T`/`&[r]T`), generics + monomorphization, methods, closures, error sets +
`?`, contracts, slices + bounds-elision, structured concurrency, `extern "c"`,
layout attributes, **modules** (item K), **stdlib + allocator-as-value** (item I),
the full **loop** system (unified `for` + fast-follows), **casts**, and **byte-level
string iteration**.

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
| C | Comments & doc-comments | ~60% | LOW | S | ✓ | — |
| D | Attributes | ~50% | MED | S | ✓ | ✓ (provenance hooks) |
| E | Real strings / text | ~25% | HIGH | L | ✓✓ | ✓✓ (self-host) |
| F | Traits / `dyn` | 0% | HIGH | L | ✓✓ | ✓✓ (pass interfaces) |
| G | CTFE + reflection | ~10% | HIGH | L | ✓ | ✓✓ (IR builder) |
| H | Function-pointer types | 0% | HIGH | M | ✓ | ✓ (vtables) |
| I | Error-handling polish | ~70% | MED | S | ✓ | — |
| J | Numeric / operator completeness | ~70% | MED | S–M | ✓ | ✓ (determinism) |
| K | Module system v2 | ~60% | MED | M | ✓ | ✓✓ (build/incremental) |
| L | Memory-layout pass | 0% | MED | M | ✓ | ✓✓ (mem-efficiency) |
| M | `@verified` (SMT) | 0% | HIGH | XL | ✓ | ✓✓ (verify passes) |
| N | Concurrency polish | ~50% | MED | M | ✓ | — |
| O | Tooling (fmt / test / doc / LSP) | 0% | LOW | M | ✓✓ | ✓ |
| P | Self-hosting | 0% | — | XL | ✓✓ | ✓✓ (the gate) |

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

### C. Comments & doc-comments — ~60% (LOW conflict; files: lexer + new doc-gen)
**Done:** `//` line comments and nested `/* */` block comments (lexer trivia).
**Left:** **doc comments** (`///` / `/** */`) captured by the lexer and attached to
the following item; a **doc generator** (a new binary/subcommand emitting Markdown
or HTML from doc comments + signatures — design §15). Mostly isolated: lexer change
+ an `Ast` annotation + a new pass. **Great parallel candidate.**

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

### K. Module system v2 — ~60% (MED conflict; files: module.rs + typeck)
**Done:** `import`, `pub` visibility, qualified access, cycle detection, multi-file
merge (flat namespace). **Left:** **true per-module namespaces** (today names must be
globally unique — the flat-namespace limitation, HANDOFF §5.24); **directory-as-
module**; qualified *type* paths (`mod.Type`); **`build.jestyr`** + manifest +
lockfile + vendored deps (design §9). **Motley:** the DAG already enables the
parallel/incremental-compilation story.

### L. Memory-layout pass — 0% (MED conflict; files: a new analysis + cgen)
**Design §16 / a Motley principle.** **Left:** a layout pass computing size/align,
**field reordering**, **enum niche-packing**, pass-large-aggregates-by-`const*`
(today `read` params copy). **Directly serves Motley's "memory efficiency at every
layer."** Mostly a new pass + cgen tweaks → reasonably isolated.

### M. `@verified` (SMT) — 0% (HIGH conflict; XL; files: a new verifier + typeck)
**Design §7 ceiling (ATS/Ada).** Turn `requires`/`ensures`/`invariant`/`variant`
from runtime asserts into **static proof obligations** discharged by an SMT backend.
**Motley:** verifying the compiler's own passes. Long-horizon; do after F/G.

### N. Concurrency polish — ~50% (MED conflict; files: cgen, escape)
**Done:** `concurrent { spawn … }` → pthreads, scoped join. **Left:** `spawn` of
closures; task **results** + `await` (keyword reserved); sync types (`Mutex`/`Atomic`/
channels); escape-checked join-safety; the `par` loop (design + MOTLEY note: must be
*deterministic*). 

### O. Tooling — 0% (LOW conflict; files: new binaries/subcommands)
**Design §15: one `jestyr` binary.** **Left:** a **formatter**, a **test runner**, a
**doc generator** (pairs with C), eventually an **LSP**. Each is a largely new
file/subcommand → **excellent parallel candidates** that barely touch the core.

### P. Self-hosting — 0% (the gate; XL)
**Design §19 / Motley prerequisite.** Rewrite the Jestyr compiler in Jestyr. **Gated
on:** E (real strings — a compiler is a text processor), probably F or H (dispatch
tables), and arguably the layout/efficiency work. **Recommended first step:** a
*vertical slice* — port the **lexer** to Jestyr; it's small, self-contained, and will
surface exactly which features are still missing.

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
