# Jestyr — Handoff

A handoff for continuing the **Jestyr** language bootstrap in a fresh session. Read
this together with [`jestyr-design.md`](jestyr-design.md) (the full language
vision). This file describes **what exists, how it's built, the non-obvious
decisions, and exactly what's left** with file-level pointers.

---

## 0. TL;DR

Jestyr is a low-level systems language (design doc: §1). What exists today is a
**working bootstrap compiler written in Rust (~9,300 lines, zero runtime deps)**
that takes Jestyr source all the way to a **native executable via a C backend**.

The full pipeline runs: **load (multi-file) → lex → parse → resolve+typecheck →
ownership/escape check → C codegen → gcc → binary**. ~35 example programs compile
and run (or are correctly rejected). 276 tests pass, including `proptest` property
tests and `bolero` fuzz tests. Build is warning-clean.

**Now also done — items K and I:** a **module/package system** (`import`,
`pub` visibility, qualified access `mem.allocate`, cycle detection, multi-file
merge into one translation unit — design §9) and a **standard library with
allocator-as-value** (`core`/`std` split, an `Allocator` *value* every allocating
API takes, `system` + bump-`arena` strategies, and a generic growable `List(T)`
collection — design §14). Both are written/exercised in `examples/modules/` and
`examples/std/`. See §5 gotchas 24–27 and §7 items K/I.

The **tiered-safety model is complete** (item D): both runtime-checked generational
references (`&T`) and zero-cost region references (`&[r]T`). See also the long-term
**Motley** compiler-infrastructure vision and its CJC-Lang connections in
[`MOTLEY.md`](MOTLEY.md).

Also done (design-doc features beyond the lettered backlog): **layout attributes**
`@packed` / `@align(n)` (→ GNU `__attribute__`) with `size_of(T)`; **slices `[]T`**
(fat pointer) with **bounds-checked indexing**; **match exhaustiveness + enum-payload
projection**; **bare-metal** `@volatile` fields and `@address(0x…)`; **MVS** — the
default parameter convention is now `read` (an immutable borrow; only `take` owns);
**the complete reference model** (item D — runtime-checked generational `&T` *and*
zero-cost arena-backed region `&[r]T`); and **refinement-driven bounds-check
elision** (item E — `s[i]` raw when `i in 0..s.len`). See §5 gotchas 15–23.

The backend now handles **method-call sugar** (`xs.push(x)`), **executable
closures** (lambda-lifted to fn-ptr + env), and **methods inside generic structs**
(monomorphized per instance) — backlog items A, B, C (§7). Since then it also
gained: **`restrict` on exclusive (`mut`/`out`) borrows** — the ownership model's
non-aliasing guarantee handed to the C optimizer as Rust-`noalias`-grade latitude
(a microbenchmark puts compute-bound code at **0.985× hand-written C**);
**teaching-quality diagnostics** (source snippet + caret underline, item L);
**design-by-contract** `requires`/`ensures` lowered to debug `assert`s (item F);
**real C interop** via `extern "c"` (item H); and **structured concurrency** —
`concurrent { spawn … }` lowered to pthreads with a scoped join (item J).

**Start here in a new session:**
```sh
cargo test                              # 86 tests, incl. property + fuzz
cargo run -- run examples/genmethods.jtr # generic List(T) with methods: 10, 30, 3, 2.5
cargo run -- run examples/closure_run.jtr # closures that execute: 105, 42, 102
cargo run -- run examples/contracts.jtr  # requires/ensures → asserts: 5, 5
cargo run -- run examples/extern_c.jtr   # calls libc puts/abs directly
cargo run -- check examples/escapes.jtr  # ownership errors (the language thesis)
cargo run -- run examples/modules/main.jtr # multi-file program + qualified access: 49, 27, 14, 2
cargo run -- run examples/std/demo.jtr   # stdlib: allocator-as-value + generic List(T): 5, 10, 50, 40
```

---

## 1. Quick start

**Prerequisites:** Rust/Cargo (built on 1.91), and a **Unix-style C compiler on
PATH** (`gcc`, `clang`, or `cc`) for the `build`/`run` commands. MSVC `cl.exe`
is *not* supported by the backend (it emits gcc/clang-flavored C with a couple of
GNU extensions — see §5).

Source files use the extension **`.jtr`**.

**CLI (`src/main.rs`):**
```
jestyrc <file.jtr>          parse and print the AST          (default)
jestyrc parse  <file.jtr>   same, explicitly
jestyrc tokens <file.jtr>   stop after lexing, dump tokens
jestyrc check  <file.jtr>   resolve, type-check, ownership-check
jestyrc emit-c <file.jtr>   lower to C and print it
jestyrc build  <file.jtr>   lower to C and compile a native binary (needs cc)
jestyrc run    <file.jtr>   build, then execute
jestyrc test   <file.jtr>   build & run the `@test`/`@bench` harness (workstream O)
jestyrc doc    <file.jtr>   render the file's API docs as Markdown (--html for HTML)
```
(Run via `cargo run -- <args>`.)

---

## 2. Repository layout

```
jestyr-design.md     The language design document (vision, all 19 sections).
HANDOFF.md           This file.
Cargo.toml           bin crate `jestyrc`; dev-deps: proptest, bolero.
examples/*.jtr       single-file demo programs (see §4).
examples/modules/    multi-file module demo (main.jtr + mathx.jtr) — item K.
examples/std/        the standard library + its demos — item I.
src/
  span.rs             Span {start,end:u32}, LineCol, line_col().
  token.rs            Token, TokenKind (big enum), keyword(), describe().
  diag.rs             Diagnostic { message, span, code, help }; render().
  doc.rs              Doc comments (DocKind/RawDoc) + the `doc` generator (C):
                      attach docs to items, extract AST guarantees, Markdown/HTML.
  lexer.rs            Hand-written lexer; new()/new_slice() (slice = one module's region).
                      Collects `///`/`//!`/`/** */` docs as trivia (tokenize_with_docs).
  ast.rs              Arena AST: ExprId/TypeId/PatId handles, all node enums, Ast.
                      Item::Import, `is_pub` on decls (item K); `attrs` on FnDecl/Struct.
  attrs.rs            Attribute registry + validation (workstream D): the closed set,
                      legal targets, arg shapes, conflicts, "did you mean". §5.30.
  module.rs           Module loader (item K): import resolution, cycle detection,
                      shared-arena multi-file merge, per-module bookkeeping. Modules.
  parser.rs           Recursive descent + Pratt. parse()/parse_module()/resume().
  printer.rs          print_ast(&Ast) -> String (tree + inline rendering).
  types.rs            Ty, GlobalTable, FnSig, TypeInfo (+ qualified), MethodRes, is_copy().
  typeck.rs           Stages 3+4: resolution + types + methods; check_program() is
                      module-aware (visibility + qualified access).
  escape.rs           Stage 5: ownership/escape checker. check(&Ast, &TypeInfo).
  cgen.rs             Stage 6: C backend; monomorphization, closures, methods,
                      qualified calls, arena intrinsics.
  main.rs             CLI driver; routes check/emit-c/build/run through module::load.
  proptests.rs        Property (proptest) + fuzz (bolero) tests. #[cfg(test)].
```
Unit tests live **in-crate** as `#[cfg(test)] mod tests` inside each module
(the crate is a `bin`, so `tests/` integration files can't see internals).

---

## 3. The compiler pipeline

Data flows through six stages; each stage's output is the next stage's input.

| # | Stage | Module | Signature | Produces |
|---|---|---|---|---|
| ⓪ | Load (multi-file) | `module.rs` | `module::load(root)` | `Program { ast, modules, diags }` |
| ① | Lexer | `lexer.rs` | `Lexer::new_slice(&src, lo, hi).tokenize()` | `(Vec<Token>, Vec<Diagnostic>)` |
| ② | Parser | `parser.rs` | `Parser::resume(&src, tokens, ast).parse_module()` | `(Ast, Vec<Item>, Vec<Diagnostic>)` |
| ③④ | Resolve+Typeck | `typeck.rs` | `typeck::check_program(&ast, &modules)` | `(TypeInfo, Vec<Diagnostic>)` |
| ⑤ | Escape check | `escape.rs` | `escape::check(&ast, &info)` | `Vec<Diagnostic>` |
| ⑥ | C codegen | `cgen.rs` | `cgen::emit(&ast, &info)` | `(String /*C*/, Vec<Diagnostic>)` |

**Stage ⓪ — the loader (item K).** `module::load` follows `import`s from the root
file, parsing **every** reachable module into *one shared arena* and concatenating
their sources into *one buffer* (so spans are globally unique → cross-file
diagnostics render against the right file). It is a "linker" merge: a flat global
namespace, one C translation unit — stages ⑤/⑥ never learn there were many files.
`Modules` carries each item's owning module, its `pub` flag, and each module's
import bindings; the type checker uses these for visibility + qualified access.
The unit-test entry points (`typeck::check`, single file) bypass it via
`Modules::single`.

**Key data structures:**

- **AST (`ast.rs`)** — flat arenas + integer handles, *not* `Box`/`&`. `Ast`
  holds `exprs: Vec<ExprData>`, `types`, `pats`; nodes reference each other by
  `ExprId(u32)` / `TypeId` / `PatId`. Later passes *annotate* via parallel
  vectors keyed by the same id (e.g. `TypeInfo::expr_types`) rather than mutating
  the tree. Add a node by `ast.expr(kind, span) -> ExprId`.

- **`Conv` (passing conventions)** — `Default | Read | Mut | Take | Out`. This is
  the ownership model surface (design §4.3). `read`/`mut`/`out` are second-class
  borrows; `take` consumes; `Default` (no keyword) is currently treated as
  owned-by-value (see gotcha in §5).

- **`Ty` (`types.rs`)** — the checker's type: `Unit | Prim(&str) | Ptr | Named(idx)
  | Opaque(name) | Result(Box<Ty>) | GenStruct{ctor,args} | TypeKw | Unknown |
  Error`. `Opaque` = unresolved/external name OR generic type parameter (treated
  non-`Copy`). `is_copy(&GlobalTable)` is the predicate the escape checker uses.

- **`TypeInfo`** — `{ table: GlobalTable, expr_types: Vec<Ty>, method_calls:
  HashMap<ExprId, MethodRes> }`. `table` has `types` (struct/enum decls), `fns`
  (`FnSig`), `consts`, `variants`. `type_of(ExprId)` and `is_non_copy(ExprId)` are
  the escape checker's interface to types. `method_calls` records, for every
  `base.name(args)` call, how it resolved (free fn vs. struct method, the inferred
  comptime type args, and the receiver convention) — see §5 gotcha 12.

---

## 4. What works today (demos = proof)

All `examples/*.jtr` either run natively or are correctly rejected:

| Demo | `jestyrc run` output | Exercises |
|---|---|---|
| `hello.jtr` | greeting | string literal, entry point |
| `compute.jtr` | `16`, `120` | structs, value-returning fns, recursion, if |
| `shapes.jtr` | `12.5664`, `12`, `0` | **enums + `match`** (tagged union + switch) |
| `generic.jtr` | `7`, `3`, `2.5` | **generic functions** (monomorphization) |
| `errors.jtr` | `6`, `true` | **error sets `T !E` + `?`** (tagged result) |
| `container.jtr` | `10`, `50`, `5` | concrete growable vec (malloc/realloc, raw ptrs) |
| `genlist.jtr` | `10`, `30`, `3`, `2.5` | **generic `List(T)`** (struct monomorphization) |
| `methods.jtr` | `10`, `30`, `3`, `2.5` | **method-call sugar** (`xs.push(x)` → free fn) — item A |
| `closure_run.jtr` | `105`, `42`, `102` | **executable closures** (lambda-lift to fn-ptr + env) — item B |
| `genmethods.jtr` | `10`, `30`, `3`, `2.5` | **methods inside a generic struct** (monomorphized) — item C |
| `contracts.jtr` | `5`, `5` | **`requires`/`ensures`** → debug `assert`s (checked at every return) — item F |
| `extern_c.jtr` | greeting, `42` | **`extern "c"`** — calls libc `puts`/`abs` by bare name — item H |
| `bench_fib.jtr` | `102334155` | speed microbenchmark (0.985× hand-C); also shows `restrict` on borrows |
| `concurrent.jtr` | `0`, `4`, `9` | **structured concurrency** — `concurrent { spawn … }` → pthreads, joins at scope — item J |
| `layout.jtr` | `12`, `6`, `16` | **layout attributes** `@packed`/`@align(n)` + `size_of(T)` — memory efficiency (§7/§16) |
| `slices.jtr` | `10`, `30`, `3` | **slices `[]T`** (fat pointer) + **bounds-checked** `s[i]` + `s.len` (§7; foundation for E) |
| `mmio.jtr` | `7`, `1` | **bare-metal**: `@volatile` fields + `@address(0x…)` fixed-address pointer (§16) |
| `refine.jtr` | `10`, `30` | **refinement → bounds-check elision** — `i: usize in 0..s.len` makes `s[i]` raw (E, §7.2) |
| `genref.jtr` | `42`, `99` | **generational references `&T`** — `{ptr,gen}`; stale deref faults (D, §4.4) |
| `region.jtr` | `10`, `20`, `30` | **region references `&[r]T`** — zero-cost (plain ptr, raw deref); arena freed at block end (D, §4.4) |
| `modules/main.jtr` | `49`, `27`, `14`, `2` | **modules (K)** — `import`, qualified call/const, a pub fn calling a private one (§9) |
| `std/alloc_demo.jtr` | `60`, `60` | **allocator-as-value (I)** — one code path, two allocator strategies (system/arena) |
| `std/demo.jtr` | `5`, `10`, `50`, `40` | **stdlib (I)** — `mem`+`list`+`core`+`io`: a generic growable `List(T)` over an allocator value |
| `loops.jtr` | `10`, `15`, `10`, `14`, `2`, `3` | **unified `for`** — range/inclusive/slice-read/`mut`-in-place/infinite+break/`for _`; bounds-check elision + `invariant` |
| `loops_advanced.jtr` | `32`, `6`, `1`, `102`, `203`, `30`, `131`, `2`, `20`, `0` | loop fast-follows — zip, `@no_panic`, element+index, region scratch, strings, casts, labeled break, step, `variant` |
| `docs.jtr` | `5`, `7`, `20`, `6` | **doc comments (C)** — `//!`/`///`/`/** */` tiers, sections, examples; also `jestyrc doc examples/docs.jtr` renders its API (signatures + prose + machine-checked **Guarantees**) |
| `attributes.jtr` | `25`, `7`, `12`, `9`, `8` | **compiler attributes (D)** — `@inline`/`@hot`/`@cold` opt hints, `@must_use`, `@deprecated("…")`, `@no_mangle` export; registry-validated (§5.30) |
| `tests_demo.jtr` | (via `jestyrc test`) | **`@test`/`@bench` runner (O)** — `jestyrc test` harness: runs `bool`-returning tests + timed benches; exit≠0 on failure (§5.30) |
| `records.jtr` | `3`, `4`, `25` | **immutable `record` (B)** — struct/record split; field assignment is a static error; lowers to a plain struct (§5.32) |
| `niche.jtr` | `8`, `42`, `0` | **niche optimization (B)** — `enum {none, some(*T)}` lowers to a bare pointer (`none`=NULL); `size_of`==pointer size; match→null-test (§5.33) |
| `option.jtr` | `42`, `7`, `5`, `-3`, `8` | **generic enums + in-language `Option`/`Result` (B)** — monomorphized per instantiation; inference from args/expected-type; `Option(*T)` inherits niche-opt (§5.34) |
| `discriminants.jtr` | `1`, `2`, `4`, `7`, `2` | **explicit enum discriminants (B)** — `red = 1`; `e as i32` reads the tag; `match` still works by name (§5.35) |
| `recursion.jtr` | `30`, `70` | **recursive ADTs via `indirect` (B)** — `node(left: indirect Tree, …)`; by-value recursion is a compile error (§5.36) |
| `distinct.jtr` | `1001`, `42`, `7` | **distinct nominal types (B)** — `distinct UserId = i32`; zero-cost typedef; `as` to convert; mixing without `as` errors (§5.37) |
| `guards.jtr` | `5`, `20`, `37`, `50`, `5`, `3`, `0` | **match arm guards (§2.4)** — `pat if <bool> => …`; two arms share a variant via guard; guarded arms don't count for exhaustiveness; lowers to an if-chain (§5.38) |
| `ranges.jtr` | `0`, `1`, `2`, `3`, `9`, `-1`, `1` | **literal + range patterns (§2.4)** — `match` on integers; `0`, `1..=9`, `100..1000`; scalar match needs a catch-all; if-chain on the value (§5.39) |
| `orpat.jtr` | `1`, `1`, `0`, `7`, `7`, `7`, `0` | **or-patterns `a \| b` (§2.4)** — `red \| green \| blue`, `0 \| 1 \| 2`, `10..=19 \| 30..=39`; each alt covers independently; stacked cases / ORed tests (§5.40) |
| `rest_pat.jtr` | `1`, `2`, `7`, `0` | **`..` rest in variant patterns (§2.4)** — `click(x, ..)` binds `x`, ignores the rest; trailing-only; the binding loop just skips it (§5.41) |
| `nested_match.jtr` | `0`, `1`, `2`, `99` | **nested pattern dispatch (§2.4)** — `node(leaf(_), leaf(_))`, `leaf(99)`; recursive `pat_test` if-chain, auto-deref through `indirect` (§5.43) |
| `reflect.jtr` | `12`, `4`, `0`, `4`, `8` | **layout reflection (§2.7)** — `align_of(T)`→`_Alignof`, `offset_of(T, f)`→`offsetof`; compile-time intrinsics next to `size_of` (§5.44) |
| `struct_variant.jtr` | `12.5664`, `12`, `0`, `9` | **struct-variant syntax (§2.3b)** — named construct `circle { r: 2.0 }` + named match `circle { r }`, `rect { w, .. }`; designated init / by-name dispatch (§5.45) |
| `spread.jtr` | `1`, `2`, `9`, `2`, `1`, `20` | **struct update / spread (§2.8)** — `Point { x: 9, ..p }` functional update; copy-then-override statement-expr (§5.46) |
| `defaults.jtr` | `3`, `0`, `1`, `5`, `0`, `9` | **field defaults (§2.8)** — `x: i32 = 0`; omitted fields filled from defaults at construction (§5.47) |
| `copy_optin.jtr` | `3`, `7`, `20` | **opt-in Copy (§2.8)** — `@copy struct`; a freely-copyable aggregate may be returned by value (escape-checker only) (§5.48) |
| `visibility/main.jtr` | `3`, `7` | **per-field visibility (§2.8)** — `pub x` exposes a field cross-module; private fields need a pub accessor (§5.49) |
| `union.jtr` | `1075838976`, `2.5`, `4` | **untagged `union` (§2.8)** — overlapping fields (C `union`); float bit-punning; `size_of` = largest field (§5.50) |
| `bitfields.jtr` | `4`, `1`, `1`, `5` | **bit-fields (§2.8)** — `flags: u8 : 3` → C `uint8_t j_flags : 3`; four fields pack 4 B → 1 B (§5.51) |
| `strings.jtr` | `13`, `72`, `3`, `5`, greeting | **length-carrying `str` view + `cstr` (strings E)** — `{ptr,len}`, O(1) `.len`, `"café".len==5`; `.cstr` FFI bridge (§5.52) |
| `codepoints.jtr` | `5`, `4`, `233`, `1` | **cost-visible views (strings E)** — O(1) `.len` vs O(n) `count_codepoints`; `for cp in codepoints(s)` decodes (§5.53) |
| `utf8_validate.jtr` | `1`, `2`, `2`, `0` | **validate-at-boundary (strings E)** — `from_utf8([]u8)→str` (validity as a type-state); `is_utf8` recoverable check (§5.54) |
| `owned_string.jtr` | `5`, `12`, 2 greetings | **owned `String` (strings E)** — heap-owned growable buffer; `string_view` borrows it (owned/view split) (§5.55) |
| `builder.jtr` | `9`, `[1, 2, 3]` | **iolist / `Builder` (strings E)** — collect `str` fragments zero-copy, flatten once into a `String` (§5.56) |
| `fstring.jtr` | message, `25` | **f-strings (strings E)** — `f"{name} x = {x} ({ok})"` typed interpolation → owned `String` (§5.57) |
| `region_string.jtr` | `Hello, region!`, `14`, `5`, `4` | **region strings + `bytes` (strings E)** — arena-allocated text, freed at scope end; `bytes`↔`from_utf8` round-trip (§5.58) |
| `substr.jtr` | `ell`, `Hello`, `lo`, `3` | **substring / slicing (strings E)** — `s[i..j]`/`substr` boundary-checked zero-copy sub-view (§5.59) |
| `str_ops.jtr` | `true`, `true`, `false`, `7`, `foo`, `bar` | **string operations (strings E)** — `str_eq`/`starts_with`/`contains`/`find`/`trim` (view-based) (§5.60) |

| Demo | `jestyrc check` | Exercises |
|---|---|---|
| `match_check.jtr` | 1 error | **match exhaustiveness** (missing `blue`) — payload projection runs in shapes.jtr (§7) |
| `exhaustive_check.jtr` | 1 error + 1 warning | **Maranget usefulness (§2.4)** — nested non-exhaustiveness (error) + a redundant/unreachable arm (warning) (§5.42) |
| `mvs.jtr` | 1 error | **MVS default = `read`** — returning a default (borrowed) param escapes; `take` to own (§4.3) |

| Demo | `jestyrc check` | Exercises |
|---|---|---|
| `escapes.jtr` | 3 errors | return / struct-capture / store escape routes |
| `collection.jtr` | 1 error | give-away-to-`take`-param route |
| `closures.jtr` | 1 error | closure-captures-borrow escape route |
| `typeerr.jtr` | 3 errors | unknown field, arity, duplicate def |
| `vec.jtr` | backend-unsupported notes | the design flagship (does NOT fully run — see §7) |

**Feature coverage by stage:**
- **Lexer:** full token set, nested block comments, **doc comments** (`///`/`//!`/
  `/** */`/`/*! */`, collected as trivia — item C), all literals (incl `0xFF`,
  `0b1010`, `1_000`, floats w/ exponent), error recovery, total on any input.
- **Parser:** structs/enums/fns/consts; if/match/blocks/unsafe; Pratt expressions;
  closures `|x| e`; type application `List(i32)`; generic struct literals
  `List(T){…}`; error sets `!{…}`; refinements `i: usize in 0..n` (parsed); `?`.
- **Typeck:** fused name resolution; struct fields, enum variants, fn signatures;
  `Copy` predicate; generic-call return substitution; `Result`/`GenStruct` types.
  **Method-call resolution** (`base.name(args)` → free fn (A) or struct method (C),
  with comptime type args *recovered from the receiver* by unification).
  Reports: unknown field on known struct, call arity, duplicate definition.
  Lenient elsewhere (unknown names → `Opaque`, no error — no stdlib yet).
- **Escape checker (the thesis):** 4 routes — (1) return a borrow, (2) capture a
  borrow in a struct literal, (3) store a borrow through borrowed storage, (4)
  give a borrow to a `take` param (now also through method-call sugar). Copy-refined
  (only non-`Copy` borrows escape). Borrow-ness propagates through `let`. Closures
  that capture a borrow are themselves borrow-tainted, so the 4 routes catch them.
- **C backend:** primitives, structs, functions, control flow, **enums→tagged
  unions + match→switch**, **generic functions + generic structs (monomorphized)**,
  **error sets + `?`**, raw pointers + heap alloc, passing conventions
  (`read`/`take`/default → by value; `mut`/`out` → by pointer), **method-call sugar**
  (item A), **closures** (lambda-lifted to fn-ptr + env, item B), and **methods
  inside generic structs** (monomorphized per instance, `self` by pointer for
  `mut self`, item C).

---

## 5. Design decisions & gotchas (read before editing)

These are the non-obvious things that will bite if you don't know them.

1. **Borrow-decoupling trick.** Throughout `typeck`/`escape`/`cgen`, methods do
   `let ast = self.ast;` *before* iterating arena nodes, then fetch via the local
   `ast`. Because `self.ast: &'a Ast` is `Copy`, this yields `&'a` node refs that
   are **independent of the `&mut self` borrow**, so you can read nodes and push
   to `self.out`/`self.diags` in the same loop. If you get "cannot borrow `self`"
   errors, this is the fix.

2. **The C backend uses two GNU extensions** (so it targets gcc/clang, not MSVC):
   - `?` lowers to a **statement-expression** `({ R _t = e; if (_t.is_err) return …; _t.ok; })`.
   - exhaustive `match` in return position appends `__builtin_unreachable();`.
   `main.rs::find_c_compiler()` probes `cc`/`gcc`/`clang` via `--version`.

3. **Intrinsics stand in for the stdlib / C interop.** `cgen::emit_call`
   special-cases these *by name*: `print_int`/`print_float`/`print_str`/`print_bool`,
   `alloc`/`realloc`/`free_ptr` (generic, type-arg first), `alloc_i32`/`realloc_i32`
   (concrete), `ok`/`err`/`is_err`/`unwrap` (result construction/inspection). Real
   C interop (`extern "c"`, `import c`) would replace these — see §7 item H.

4. **Emitted-C naming (collision-free by construction):**
   - types: `Jestyr_<Name>`; generic struct instance: `Jestyr_<Ctor>__<args>`
   - functions: `jestyr_<name>`; generic fn instance: `jestyr_<name>__<args>`
   - values & struct fields: `j_<name>`
   - result structs: `JestyrResult_<okmangle>`; runtime: `jestyr_rt_*`

5. **Monomorphization (`cgen.rs`).** A worklist (`collect_instances`) walks from
   non-generic roots, finds calls to generic fns, and stamps an instance per
   `(fn, type-args)`; instances can discover more instances (a `seen` set keyed on
   the mangled name guarantees termination). Generic **struct** instances are
   collected separately (`collect_struct_instances`) by scanning each
   (monomorphized) function's **signature types** and generic-struct literals
   under its substitution. The `subst: HashMap<String,Ty>` is threaded through
   `c_ty_ast`/`c_type`; comptime type params are *erased* from runtime signatures.
   A generic struct is `fn List(comptime T: type) -> type { return struct {…} }` —
   a comptime function whose return value is a type.

6. **`no_struct` flag (parser).** Disambiguates `Ident {` as struct-literal vs.
   control-flow body. Set `true` while parsing `if`/`while` conditions and `match`
   scrutinees, reset inside `(`/`[`/blocks. This is why `match s {` reads `{` as
   the match body, not a struct literal.

7. **The `Copy` seam.** The escape checker gates each route on
   `info.is_non_copy(expr)`. `Unknown` types are treated as `Copy` (suppress —
   avoid false positives on un-inferred things); `Opaque`/`Named`/`GenStruct`/
   `Result` are non-`Copy`. This is *the* integration point between stages 4 and 5.

8. **MVS default = `read` (done).** The escape checker now treats a parameter
   with *no* convention keyword as an immutable **borrow** (design §4.3): every
   non-`comptime`, non-`take` param is a borrow that may not escape; only `take`
   owns. cgen still passes default/`read` **by value** — a sound implementation of
   an immutable borrow for the bootstrap (zero-copy-by-`const*` for large
   aggregates is a future memory-efficiency optimization). Consequence: a function
   that *returns* its parameter must declare it `take` (see `generic.jtr`'s
   `max`/`min`); closure capture is Copy-aware (a captured Copy borrow doesn't
   taint the closure). Demo `mvs.jtr`.

9. **Statement-vs-expression lowering.** Jestyr is expression-oriented; C is not.
   `cgen` threads a `ret: bool` ("am I in return position?") through
   `emit_body`/`emit_if`/`emit_match`/`emit_return`, pushing `return` into branch
   tails. `match`/`if` are only supported in **statement or return** position, not
   as nested value sub-expressions (those emit a diagnostic).

10. **Parser statement boundaries are structural**, not newline-aware (the lexer
    discards newlines). So `f` on one line then `(x)` on the next parses as a call
    `f(x)`. A real language would add newline significance; fine for now.

11. **Lenient, no-stdlib typeck.** Unknown type/value names become `Opaque`/
    `Unknown` with *no* error (there's no prelude to resolve against). Only
    high-confidence errors are reported.

12. **Method calls are resolved once, in typeck, and recorded in
    `TypeInfo::method_calls`.** Both later passes read that side table rather than
    re-deriving the receiver→function match (annotate-don't-mutate, like
    `expr_types`). `MethodRes { fn_name, recv_ctor, type_args, recv_conv }`:
    `recv_ctor = None` → free-function method (item A); `Some(ctor)` → a method
    *inside* the struct `ctor` (item C). The comptime `type_args` are **recovered
    from the receiver** by `unify_tp` (`List(T)` vs `List(i32)` ⇒ `T=i32`), since a
    method call carries no explicit type arguments. cgen pushes the active
    monomorphization `subst` through them (`apply_subst`) for method calls that
    appear inside a generic body.

13. **Closures are lambda-lifted, collected from non-generic bodies only.** Each
    closure expr `C` becomes `JestyrEnv_C` (captured values), `JestyrClosure_C`
    (`{fn-ptr, env}`), and `jestyr_lam_C(JestyrEnv_C*, params)`. Inside the lifted
    body, captured names render as `j__env->j_<name>` (driven by `capture_set`);
    invocation `f(args)` → `f.call(&f.env, args)` (the trigger is `info.type_of` ==
    `Opaque("closure")`). Captures = free vars that are neither params nor *global*
    names (`is_global_name` / `is_intrinsic`), typed via a reference site inside the
    body. **Limitation:** a closure inside a *generic* fn still diagnoses (its
    capture types depend on the monomorphization); capturing a `mut`-pointer param
    is by-value of the pointer. Closure types are emitted before `fn_protos`; lifted
    fn *definitions* after `consts`, so sites (in `fn_defs`) see both.

14. **Generic functions and generic-struct methods share one monomorphization
    worklist** (`collect_all_instances` → `Work::Fn | Work::Method`), because each
    can call the other. Methods are emitted as free C functions
    `jestyr_<Ctor>__<args>_<method>` with an explicit `self` param (`self_cty` /
    `self_is_ptr`); `mut self` → by pointer, so `self.f` lowers to `(*j_self).j_f`.
    Method bodies are checked by typeck/escape via the `StructType` arm with
    `self_ty = Opaque("Self")`.

15. **Contracts lower to `assert`, checked at every return.** `requires` asserts
    run at the top of `emit_fn_body`; `ensures` run in `emit_value_return`, which
    spills the return value to **`j_result`** first (so `result` in the postcondition
    resolves to it via the ordinary `j_<name>` rule — no special binding). Because
    *every* leaf value-return funnels through `emit_value_return` (including those
    inside `if`/`match`/`block` tails), all return points are covered. Contracts are
    parsed but **not** type-checked or escape-checked (lenient, like §5.11); they
    are not yet supported on methods/closures (those `cur_ensures.clear()` to be safe).

16. **`extern "c"` calls bypass name mangling.** `Item::Extern` is a bodyless decl;
    its signature is registered in `table.fns` (so calls type/arity-check), and
    `cgen` keeps an `extern_fns` set. In `emit_call` an extern name emits **bare**
    (`puts(...)`, not `jestyr_puts(...)`); `extern_protos` emits a C prototype the
    linker resolves. Re-declaring a libc function with a *compatible* prototype
    (`int32_t == int`, `str == const char*`) is legal C, so no header is needed.
    Extern params get **no** `restrict` (a foreign fn makes no aliasing promise).

18. **Slices `[]T` are fat pointers, built/indexed via runtime.** `TypeKind::Slice`
    / `Ty::Slice(Box<Ty>)`. cgen emits one `JestyrSlice_<elem>` struct (`{T* ptr;
    size_t len;}`) per element type, collected by scanning the flat arenas
    (`collect_slices`) for `[]T` annotations and `slice(T,…)` calls. `slice(T,p,n)`
    is an intrinsic constructor; `s[i]` lowers to a bounds-checked statement-expr
    (`assert(_ix < _s.len)`, aborts on OOB); `s.len`/`s.ptr` are real fields (not
    `j_`-prefixed). **Limitation:** slices with a *generic* element (`[]T` inside a
    generic fn) aren't monomorphized (collected with empty subst). Demo `slices.jtr`.

19. **Match: payload projection + exhaustiveness.** `bind_pattern_types` projects an
    enum variant's payload field types onto its sub-patterns (`circle(r)` → `r:f64`),
    via `variant_field_types`. `check_exhaustive` reports a `match` on an enum that
    misses variants and lacks a `_`/binding catch-all. Demo `match_check.jtr`
    (exhaustiveness), `shapes.jtr` (projection at runtime).

20. **Bare-metal attributes.** `@volatile` on a struct field (parsed as a field-level
    attribute after the `:`) sets `StructMember::Field.volatile` → C `volatile`
    qualifier. `@address(0x…)` is an `Attr`-callee call lowered in `emit_call` to
    `((void*)(addr))` — a fixed-address pointer for MMIO. Demo `mmio.jtr`.

17. **`concurrent`/`spawn` lower to pthreads.** `ExprKind::Concurrent(Block)` +
    `ExprKind::Spawn(call)`. Per spawn site (keyed by the inner call's expr id) cgen
    emits `struct _jsp_<id>` (the task's args, by value) and `jestyr_task_<id>` (a
    `void*` trampoline that unpacks and calls `jestyr_<fn>`). `emit_concurrent`
    spills each task's args to a stack struct, `pthread_create`s it, and
    `pthread_join`s all before the block exits — so the arg structs (and any
    borrowed data) provably outlive the tasks. `prelude` includes `<pthread.h>` only
    when spawns exist; `main.rs` adds `-pthread` only when the C mentions `pthread`.
    Spawned callee must be a non-generic, non-`self`, by-value-param user fn.
    **Gotcha:** a `var x = alloc_i32(n)` is typed `Unknown` (intrinsic) → C `int`;
    annotate (`var x: *mut i32 = …`) for the right pointer type.

21. **Generational references are a `{ptr, gen}` fat pointer with a checked deref.**
    `gen_new(T,v)` lays out the allocation as `[uint64_t gen | T]` and returns a ref
    snapshotting the generation; the object pointer is `base+8`, so the live gen is
    always at `((uint64_t*)ptr)[-1]`. `r.*` asserts it equals the snapshot →
    use-after-free traps. `gen_free` increments the gen (invalidating every ref) but
    **leaks** (so the stale deref reads valid, bumped memory rather than UB). Element
    types collected like slices (`collect_genrefs` over `&T` annotations + `gen_new`
    calls). `&` is a *type* prefix only in `parse_type`; as an expression it's still
    address-of/bit-and.

22. **Refinements drive bounds-check elision, structurally.** `index_in_range`
    matches the AST shape of a param's refinement — `Range{ hi: Field{Name(s),"len"},
    inclusive:false }` whose `s` is the slice being indexed — and the `Index` arm
    skips the `assert`. It is *purely structural* (no value analysis); the refinement
    is currently a **caller promise** (no narrowing check inserted at the call yet),
    so an out-of-range argument to a refined param is UB — the soundness companion is
    call-site enforcement.

23. **Region refs are zero-cost; gen-refs are checked — same `&`, different `Ty`.**
    `&[r]T` (`TypeKind::RegionRef`) lowers to a bare `T*`; the `Deref` arm gives it
    the raw `(*p)` path because it isn't `Ty::GenRef` (whose deref asserts the
    generation). `region r { … }` lowers to `{ JestyrArena j_r = …new(1MiB); <body>;
    …free(&j_r); }`; `region_alloc(r,T,v)` bump-allocates from `j_r`. In `parse_type`,
    `&` followed by `[` is a region ref, else a gen-ref. Safety is currently *lexical*
    (the arena outlives the block's refs by construction) — a real escape check that a
    `&[r]T` can't leave its region is future work.

24. **Modules are a "linker" merge with a *flat* namespace (item K).** `module.rs`
    parses every `import`ed file into one shared arena and merges all items into one
    program — so **top-level item names must be globally unique** across modules (a
    clash is the ordinary "duplicate definition" error; cgen would otherwise emit two
    `jestyr_<name>`). This is the bootstrap's deliberate simplification; true
    per-module namespacing (so two modules can each have a private `helper`) is the
    main deferred refinement. `pub`/visibility and qualified access give back most of
    the ergonomics. Spans are **global** (offsets into the concatenated buffer); the
    loader re-bases them per file when rendering diagnostics (`Modules::render`).

25. **Qualified access (`mem.allocate`) is resolved in `typeck`, not the parser.**
    `mem.allocate(x)` parses as a normal `Call{ callee: Field{ Name("mem"), … } }`.
    The checker recognizes `mem` as an import binding **only if it isn't a local in
    scope** (so a variable named `mem` always wins) and records the resolution in
    `TypeInfo::qualified` (call-id → bare fn name; field-id → bare const name). cgen
    reads that map (`emit_named_call` / the `Field` arm). Generic qualified calls
    (`mem.allocate(i32, …)`) are discovered by the monomorphization worklist via the
    same map (see `find_calls_expr`).

26. **Types resolve by *bare* name across modules; only fns/consts are import-gated.**
    There is no qualified-type syntax (`mem.Allocator` doesn't parse). In the flat
    namespace a type defined anywhere is visible by its bare name (`Allocator`), so
    std types are written unqualified. Per-module *type* privacy + dotted type paths
    are future work. Visibility is enforced for **function calls** and **qualified
    access**; bare type references are not visibility-checked.

27. **`pub` lives on the decls; the loader reads it.** `FnDecl`/`EnumDecl`/`ConstDecl`/
    `ExternFn`/`Item::Struct` each carry `is_pub` (set by `parse_item`). Methods are
    never `pub` (their visibility follows the struct). `import "p"` binds `p`'s last
    path segment (or an `as` alias); the path is resolved relative to the importing
    file with `.jtr` appended. Cycles are a hard error; diamonds load once.

28. **Loops are one keyword, statement-position only (`ExprKind::For`).** Header
    shape lives in `ForHead` (`Infinite` / `While` / `Iter{conv,binding,iter}`).
    Range-vs-slice is decided by whether `iter` is an `ExprKind::Range` (parse-time),
    not by type. `cgen::emit_for` routes from `emit_stmt`/`emit_return` (a loop in
    value position diagnoses, like `if`/`match`, §5.9). **Bounds-elision reuses the
    refinement machinery**: a range-for inserts its index into `cur_refines` (keyed by
    the loop's `Range` expr id) around the body, so `index_in_range` proves `xs[i] <
    xs.len` — *exclusive ranges only* (an inclusive index can equal `len`). `for mut x`
    registers the element in `ptr_params` (so `x` renders `(*j_x)`), reusing the
    `mut`-param path. **Full borrow contract (both halves):** the escape checker
    binds a slice element as a borrow (so it can't escape the loop) *and* keeps a
    `frozen: Vec<String>` of iterated collection names — mutating one in the body
    (reassign, element store, or passing it to a `mut`/`out`/`take` param via
    `check_loop_mutation`) is rejected (iterator invalidation). `ForHead::Iter` now
    holds `binds: Vec<LoopBind>` + `sources: Vec<ExprId>`: 1 bind/1 source = simple;
    2 binds/1 source = element+index; 2 binds/2 sources = zip (length-checked via a
    runtime `assert`). `For` also carries `region: Option<Ident>` (scratch arena;
    `emit_for` wraps + arms `scratch_reset`, consumed at the top of `emit_loop_body`);
    `uses_arena()` must report `true` for it. `@no_panic` lives on `FnDecl`; cgen's
    `cur_no_panic` makes an un-elided index in the `Index` arm a diagnostic. The
    recursive cgen walkers (`find_calls_expr`, `collect_structs_in_expr`,
    `find_closures_expr`, `find_spawns_expr`, `collect_refs`) all descend into
    `For`/`Invariant`/`Variant`/`Cast` — miss one and generic calls / closures /
    spawns inside a loop or cast silently vanish from codegen.
    **Labels/step/variant:** `For` carries `label: Option<Ident>` (`for outer: …`);
    labeled `break`/`continue` are `ExprKind::Break(Option<Ident>)`/`Continue(...)`
    lowering to C `goto <l>__break;` (target after the loop) / `goto <l>__continue;`
    (target armed via `cont_label`, emitted at the bottom of every body emitter).
    `ForHead::Iter` has `step: Option<ExprId>`; a *negative literal* step descends
    (`>` compare, `int64_t` index so `size_t` can't underflow), and a stepped index
    does **not** elide. `variant <e>` (`ExprKind::Variant`, keyword `variant`) hoists
    one `int64_t _vt<id> = INT64_MAX;` per loop (`hoist_variant_trackers`) and asserts
    `_vv >= 0 && _vv < _vt` each iteration. `step` is a *contextual* keyword (an
    `Ident` named `step`), not reserved.

29. **Casts and string iteration (self-hosting enablers).** `expr as T`
    (`ExprKind::Cast`) parses tighter than binary ops (`parse_cast` between unary and
    postfix), types as its target, lowers to a C cast `(T)(e)` — numeric and pointer.
    `str` is iterable: `for c in text` byte-iterates (`emit_str_for`, each `c` a
    `u8`), `text[i]` → a byte. **Byte iteration, not Unicode.** *(Superseded by §5.52:
    `str` is now a length-carrying `{ptr,len}` view, so `text.len` is O(1) — no more
    `strlen` — and iteration/indexing go through the view's `ptr`.)*

30. **Doc comments are *trivia* with a side table — they never reach the parser
    (item C).** The lexer classifies `///`/`/** */` as *outer* docs and `//!`/`/*! */`
    as *inner* docs (an extra marker char — `////`, `/***` — demotes them to plain
    comments, like Rust), records each as a `RawDoc { kind, block, span, text }` in
    `Lexer::docs`, and **still skips it**. So the token stream is identical with or
    without docs and *a comment can never change how code parses* — the structural
    enforcement of "comments document; contracts prove". `tokenize_with_docs` exposes
    the side table; the compiler proper calls plain `tokenize` and ignores docs.
    The generator (`doc.rs`, `jestyrc doc`) re-lexes, **attaches** each outer-doc
    block to the nearest item/method below it (a dangling doc is a *warning*),
    treats all `//!` blocks as the module doc, splits prose into a summary +
    `#`-headed sections + fenced examples, and — the Jestyr-specific part —
    reconstructs a **Guarantees** block straight from the AST (`requires`/`ensures`,
    error set, `@no_panic`, refined params), so machine-checked facts are never
    confused with prose. Renders Markdown (default) or HTML (`--html`); single-file
    for now (doesn't crawl `import`s). User reference: [`docs/comments.md`](docs/comments.md);
    demo `examples/docs.jtr`.

31. **Attributes are registry-validated; `attrs.rs` is the single source of truth.**
    `parse_attrs` collects raw `@name`/`@name(args)` onto items; `FnDecl` now keeps
    its **full** `attrs: Vec<Attribute>` (not just the `no_panic` bool — that field is
    a cached projection; `FnDecl::has_attr`/`attr` read the vector). Every item's
    attributes are checked against the closed registry in `attrs.rs` *at parse time*
    (so enum/const/extern attrs are validated before being discarded): a row lists an
    attribute's legal `Target`s, its `Args` shape, and a `Status` (`Active` vs
    `Reserved`). **Anything unknown, misplaced, mis-argued, contradictory, or
    reserved-but-unimplemented is a hard error** (with a Levenshtein "did you mean"
    for typos) — a deterministic language must never silently ignore a directive. The
    backend (`cgen::fn_attr_prefix`) lowers function attributes to GNU declaration
    clauses: `@inline` → `static inline __attribute__((always_inline))` (the `static`
    dodges C11's inline-linkage pitfall, and is why `@inline` conflicts with
    `@no_mangle`), `@no_inline`/`@hot`/`@cold` → the matching `__attribute__`,
    `@must_use` → `warn_unused_result`, `@deprecated("m")` → `deprecated("m")` (the
    message literal already carries its quotes). `@no_mangle` (`cgen::c_fn_name` +
    a `no_mangle` set) emits the function under its **bare** C symbol and calls it
    bare — the export mirror of `extern "c"`; rejected on generics (no single name to
    mangle) and a no-op on `main` (already exported by the entry wrapper). These are
    all *hints/ABI*, never behavior — the design's hard rule. Demo `attributes.jtr`.
    **Also active:** `@section(".name")` (→ `__attribute__((section…))` on fns,
    methods, *and* consts — bare-metal placement); `@no_mangle` **on consts** (a
    bare external `const <name>` instead of `static const j_<name>`, referenced
    bare via `no_mangle_consts` in the `Name` arm — *caveat:* a local shadowing the
    name mis-resolves, since cgen has no scope map). **Bug-fix watch-outs:** the
    `@no_mangle` bare-name must be threaded everywhere a callee name is built —
    `emit_named_call`, the direct `Name`-call arm, the **free-method-call** path
    (`emit_method_call`, item A), and the **spawn trampoline** (`jestyr_task_*`)
    all route through `c_fn_name` now; miss one and a `@no_mangle` target link-errors.
    Validation extras (`attrs.rs`): duplicate attributes, `@align` must be a positive
    power of two, conflicts include `@inline`+`@no_mangle`. Doc: `docs/attributes.md`.
    **`@test`/`@bench` runner (workstream O):** `cgen::emit_tests` (vs `emit`) swaps
    the `main` wrapper for `test_main` — a harness that calls each `@test` (a no-arg
    `bool` fn; `true`=pass) tallying pass/fail and times each `@bench` with `clock()`
    (prelude pulls `<time.h>` in `test_mode`); exit≠0 on any failure. Driven by
    `jestyrc test` (`Mode::Test` in `main.rs`); demo `tests_demo.jtr`.
    **Reserved (recognized, error-on-use until built):** `@verified` (SMT),
    `@doc_hidden` (doc gen, workstream C).

32. **`record` is an immutable `struct` — a static guarantee with zero representation
    cost (CJC-inspired struct/record split).** A `record` is parsed exactly like a
    `struct` (new `Record` keyword → `parser::parse_named_struct(.., is_record=true)`),
    carries `is_record` on `Item::Struct` (`ast.rs`) and `TypeDecl` (`types.rs`), and
    lowers to the **identical** C struct — cgen never branches on it. The immutability is
    enforced purely in the checker: `typeck`'s `Assign` arm calls `record_name(base_ty)`
    (which looks through one level of `*`/`&`/`&[r]`) and rejects `r.field = …` with
    *"cannot assign to a field of immutable record"*; and `parse_named_struct` rejects a
    `mut self`/`out self` method on a record. The whole *binding* may still be rebound
    (`var p = …; p = Rec{…}`), like a `let` value — only **field** assignment is barred.
    Design + the sequenced plan for the rest of the struct/enum/ADT work (niche-optimized
    `Option`, struct-variant enums, Maranget exhaustiveness, recursive `indirect` ADTs,
    `distinct`, layout reflection): [`docs/structs-enums-design.md`](docs/structs-enums-design.md).
    Demo `examples/records.jtr`.

33. **Niche optimization — a two-variant `{none, some(thin-ptr)}` enum *is* its
    pointer (CJC §1.3 / Rust niche; the flagship "transparent cost" demo).** When an
    enum has exactly one nullary variant and one single-field variant whose payload is a
    **thin pointer** (`*T` or `&[r]T` — both have a `null` niche; a *fat* `&T` genref or
    `[]T` slice does not), cgen represents the whole enum as just that pointer: `some(p)`
    → `p`, `none` → `((T*)0)`, and `size_of` == the pointer size. It is a pure
    *representation* swap — typeck still sees an ordinary 2-variant enum (exhaustiveness,
    construction, projection unchanged); only cgen branches. Pieces (`cgen.rs`):
    `NicheInfo` + `niche_enum_at`/`niche_enum_named` (detect, reading the type table);
    `c_type`/`c_ty_ast` return the payload pointer; `enum_defs` + `forward_types` skip the
    enum (no `Jestyr_<E>` struct/tag); `emit_variant_construct` handles `some`/`none`;
    `emit_niche_match` lowers `match` to an `if (p != NULL)` test instead of a tag
    `switch`. Demo `examples/niche.jtr` (`8, 42, 0`). Generic instances inherit this
    via `niche_enum_instance` (§5.34), so `Option(*T)` is a bare pointer too.

34. **Generic enums + in-language `Option`/`Result` — fully lowered (design §2.2b).**
    `enum Option(T) { none, some(v: T) }` uses **direct `enum Name(T)` syntax** (not the
    comptime-`fn -> type` pattern generic structs use), so the enum stays a registered
    `TypeDecl` and reuses variant/match/exhaustiveness/niche machinery; it's
    monomorphized per instantiation like a generic struct. Pieces:
    - **Type:** `Ty::GenEnum { ctor, args }` (distinct from `GenStruct`). `lower_type`/
      `ast_type_to_ty` of `App` produce it when the ctor is a generic enum.
    - **Inference (`typeck::variant_ctor_type`):** a payload variant `some(5)` recovers
      `Option(i32)` via `unify_tp` against the variant's template fields; a nullary
      `none` takes its instance from a *targeted* expected type — `cur_expected`/`cur_ret`,
      set only around `let`-annotations and `return` (not a general bidirectional pass).
    - **Monomorphization (`cgen`):** `collect_enum_instances` scans every expr type +
      fn signatures for concrete `GenEnum`s; `gen_enum_defs`/`emit_enum_instance` emit one
      `Jestyr_Option__i32` tagged union per instance (type-params substituted, mangled by
      `gen_struct_c_name`). `enum_defs`/`forward_types` still skip the *template*.
    - **Construct/match:** `emit_variant_construct` reads the construction expr's inferred
      `GenEnum` type; `emit_match` carries `(tag_prefix, subst)` so payload bindings get
      their concrete C type. `niche_enum_instance` runs the niche rule on the substituted
      templates, so `Option(*T)`/`Option(&[r]T)` inherit §5.33 niche-opt (a bare pointer).
    Demo `examples/option.jtr` (`42, 7, 5, -3, 8`). Inference covers `let`/`return` **and
    call arguments** (`or_else(none, 5)` resolves `none` from the param type). **Limitations
    (follow-ups):** generic enums used only *inside a generic function body* aren't
    collected; generic-enum *methods* and a true auto-prelude are future work.

35. **Explicit enum discriminants (design §2.3).** `EnumVariant.discriminant:
    Option<ExprId>` (the AST-shape change), parsed as `= <expr>` after a variant
    (`enum Color { red = 1, green = 2 }`). cgen emits `Jestyr_<E>_<v> = <value>` in the
    tag enum — for both plain enums (`enum_defs`) and generic instances
    (`emit_enum_instance`). Reading a discriminant: `e as i32` extracts the tag —
    `cgen`'s `Cast` arm emits `(int)((e).tag)` when `is_tagged_enum(src)` (a non-niche
    Named/`GenEnum`); a niche enum has no tag so it keeps the plain pointer cast. The
    `match` switch is unchanged (it dispatches on the named tag constant, which now just
    has an explicit value). **Gotcha:** a variant name can't be a language keyword
    (`read`/`mut`/`in`/… are reserved) — use `red`, not `read`. Demo
    `examples/discriminants.jtr` (`1, 2, 4, 7, 2`). **Not yet:** a `: <int>` tag-width
    repr (`enum Color : u8`), and brace struct-variant *syntax* (`V { x }` named
    construct/match — named *fields* already exist via `V(x: T)`).

36. **Recursive ADTs via `indirect` (design §2.5).** Key fact: **recursion already
    worked** — `*T`/`&T`/`&[r]T` all lower to a pointer, so `enum List { cons(tail: *List) }`
    has always compiled (a forward typedef `typedef struct Jestyr_List Jestyr_List;` is
    emitted by `forward_types`). New this step: the **`indirect` keyword** (`token.rs`),
    parsed in `parse_type` as **sugar for `TypeKind::Ptr{Default}`** (so *zero* new
    type/cgen machinery — `indirect T` ≡ `*T`), the readable spelling for a self-reference;
    and a **by-value-recursion guard** (`typeck::check_no_value_recursion`, run in phase 2
    when lowering struct/enum fields) that rejects a field whose type is the *enclosing
    type by value* (`cons(tail: List)`, `next: Node`) with "infinitely sized … use
    `indirect`/`*`". The guard is what gives `indirect` meaning. Demo
    `examples/recursion.jtr` (`30, 70`). **Future:** tier-aware `indirect &[r]T` +
    auto-allocation on construction; the guard catches only *direct* self-reference (mutual
    / generic-by-value cycles fall to the C compiler).

37. **`distinct` nominal types (design §2.6).** `distinct UserId = i32` →
    `Item::Distinct(DistinctDecl)` + `TypeKindG::Distinct { base }` (registered in
    phase 1, base lowered in phase 2; `is_copy` follows the base). It lowers to a
    **zero-cost typedef** — `typedef int32_t Jestyr_UserId;` emitted in
    `forward_types`; `c_type`/`c_ty_ast` for a `Named` distinct already return
    `Jestyr_<Name>` (so no other cgen change), and the struct/enum/etc. passes skip it
    (they match `Item::Struct`/`Item::Enum` specifically). Construct/extract with `as`
    (`5 as UserId`, `uid as i32`). Enforcement: `typeck::distinct_mismatch` — a `let`
    whose annotation is a distinct type rejects a non-matching initializer (suggest `as`);
    scoped to fire *only when a distinct type is involved*, so the lenient checker is
    untouched elsewhere. Demo `examples/distinct.jtr` (`1001, 42, 7`). **Limitation:**
    enforcement covers `let` annotations only — call args / returns aren't type-checked
    yet (lands with general arg-vs-param checking). **Adding a new `Item` variant** touches
    ~8 exhaustive `match item` sites (typeck phases + build_owner + check_items, escape,
    module `item_is_pub`, printer, doc) — the AST-shape tax, all additive.

38. **Match arm guards (`pat if <bool> => …`, design §2.4, step 1 of "match power").**
    `MatchArm.guard: Option<ExprId>` (the AST-shape change), parsed in `parse_match` as
    an optional `if <expr>` between the pattern and `=>` (the `if` is a *contextual
    marker*, not an if-expression — we just `parse_expr` what follows). **The one
    soundness-relevant rule:** a guarded arm proves *nothing* about coverage, so
    `typeck::check_exhaustive` **`continue`s past any guarded arm** — even `_ if c =>`
    or `circle(r) if c =>` leaves that case potentially unhandled, so an unguarded
    fallback is still required. typeck infers the guard with the pattern's bindings in
    scope (it may reference them); escape walks it (a boolean — never a tail/escape
    route). **cgen:** a C `switch` can't put two `case`s on one tag (arms differing only
    by guard) nor fall through when a guard fails, so **any guarded arm flips the whole
    match to an ordered if-else-if chain** (`emit_guarded_match`; the niche path gets
    `emit_guarded_niche_match`) — `if (tag matches) { bind; if (guard) { body } }`, a
    failed guard falling to the next arm. A fired arm `goto`s a shared `jm_end_N` label
    in statement position, or the body `return`s in return position (then a trailing
    `__builtin_unreachable()` iff no *unguarded* catch-all). **No-guard matches keep the
    existing `switch`/null-test lowering byte-for-byte** (zero risk to the tests that
    assert on it) — the chain is only used when `arms.iter().any(|a| a.guard.is_some())`.
    **Watch-out (the §5.28 lesson again):** the guard is a *new sub-expression*, so every
    cgen walker that descends into `arm.body` (`find_calls_expr`, `collect_structs_in_expr`,
    `find_closures_expr`, `find_spawns_expr`, `collect_refs`) **and** escape's
    `collect_names` now also descend into `arm.guard` — miss one and a generic call /
    closure / spawn used *only inside a guard* silently vanishes from codegen. Demo
    [`examples/guards.jtr`](examples/guards.jtr) (`5, 20, 37, 50, 5, 3, 0`). **Next match-power
    steps (still §2.4):** or-patterns (`a | b`), range patterns (`0..=9`), `..` rest, then
    replacing name-set exhaustiveness with a Maranget usefulness matrix (which also flags
    redundant arms — now that guarded arms are skipped, an unguarded arm shadowed by an
    earlier one is the first redundancy case to catch).

39. **Literal + range patterns → `match` on integers (design §2.4, step 2).** Two new
    `PatKind`s: `Lit(ExprId)` (`0`, `-3`, `'a'`, `true` — the literal is kept as an expr so
    cgen re-emits it verbatim) and `Range { lo, hi, inclusive }` (`0..=9` / `0..9`). Parsed
    in `parse_pattern` (a `parse_pat_lit` helper handles a leading `-`; **floats are
    excluded** — equality is a footgun). **This is the first time `match` dispatches on a
    non-enum scrutinee.** cgen: `emit_match` routes a `Ty::Prim` scrutinee whose name passes
    `typeck::is_scalar_match_ty` (integer/char/bool, *not* float) to **`emit_scalar_match`**
    — an ordered if-chain on the *value*: `Lit` → `tmp == (lit)`, `Range` → `tmp >= (lo) &&
    tmp <(=) (hi)`, `Wildcard`/`Ident` → catch-all (on a scalar, *every* `Ident` is a
    binding — no variant can match an int). Guards compose (same `emit_guarded_arm`).
    **Exhaustiveness:** `check_exhaustive` gains a scalar branch — the integer domain can't
    be enumerated, so a scalar `match` **requires an unguarded `_`/binding catch-all** (true
    interval coverage like `0..=255` over `u8`, and `true|false` over `bool`, arrive with
    the Maranget pass — §2.4 step 4). The new `PatKind`s also forced no-op/diagnostic arms in
    every exhaustive `match pat` site (the AST-shape tax): `bind_pattern_types`/`check_exhaustive`
    (typeck), `bind_pattern` (escape), `pat_str` (printer), and the two enum loops in cgen
    (`emit_match` switch path + `emit_guarded_match`) where a scalar pattern on an enum
    scrutinee diagnoses. **Limitation:** *nested* literal patterns inside a variant
    (`some(0)`) still bind-or-ignore like a wildcard in the enum path — proper nested-pattern
    dispatch lands with the Maranget decision tree (step 4). Demo
    [`examples/ranges.jtr`](examples/ranges.jtr) (`0, 1, 2, 3, 9, -1, 1`).

40. **Or-patterns `a | b` (design §2.4, step 3).** `PatKind::Or(Vec<PatId>)`. `parse_pattern`
    now parses one atom (`parse_pattern_atom`, the old body) then folds `|`-separated atoms
    into an `Or` — so or-patterns nest inside variant subpatterns too. An arm matches if
    **any** alternative matches; each alternative counts **independently** for coverage, so
    `red | green | blue` over the remaining variants is exhaustive with no catch-all.
    Exhaustiveness recurses through `Or` via two new typeck helpers — `cover_pattern`
    (variants covered + catch-all, for enums) and `pat_is_irrefutable` (for the scalar
    catch-all check). **cgen, three paths:** (1) **scalar** — `scalar_pat_cond` recurses and
    OR-joins the alternatives' value tests (`(n==0) || (n==1) || …`); (2) **enum no-guard
    switch** — an or-pattern stacks `case` labels (`case red: case green: …` → one body);
    (3) **enum guarded if-chain** — an OR-ed tag test (`tag==red || tag==green`). Both enum
    paths use `or_variant_names`, which only accepts **nullary** variant alternatives —
    payload bindings can't be shared across or-alternatives in the bootstrap (a mismatch
    diagnoses, it doesn't miscompile). Or-patterns on a **niche** enum diagnose
    ("not supported yet") rather than being silently dropped by the niche classifier's
    catch-all. Demo [`examples/orpat.jtr`](examples/orpat.jtr) (`1, 1, 0, 7, 7, 7, 0`).

41. **`..` rest in variant patterns (design §2.4, step 3b).** `PatKind::Rest`, parsed from a
    bare `..` (the `DotDot` token at the start of a pattern atom — unambiguous since open-
    ended ranges aren't supported). It's only meaningful as the **last** field of a variant
    pattern (`rect(w, ..)` binds `w`, ignores the rest); the parser **rejects a non-trailing
    `..`** ("`..` may only appear as the last field pattern"). Almost free to implement: the
    variant binding loop in cgen already binds *only* `Ident` subpatterns, so a trailing
    `Rest` is simply skipped — no field is bound for it. Everywhere else `Rest` is a no-op
    (`bind_pattern_types`/`cover_pattern`/`bind_pattern` bind/cover nothing; `pat_str` →
    `".."`; the two cgen enum loops treat a *whole-arm* `..` as a no-op). Demo
    [`examples/rest_pat.jtr`](examples/rest_pat.jtr) (`1, 2, 7, 0`). **Limitation:**
    *trailing* `..` only (a middle `..` would need positional remapping of later bindings) —
    fine until named struct-variant patterns (§2.3b) land.

42. **Maranget usefulness — real exhaustiveness + redundant-arm warnings (design §2.4, the
    capstone).** `typeck::check_exhaustive` is rebuilt on Maranget's usefulness algorithm
    ("Warnings for pattern matching", 2007). Patterns lower (`lower_pat`) to a structural IR
    `Pat = Wild | Var(name, args) | Int(i128) | Range(lo,hi) | Or(vec)` (bindings/`_`/`..`/
    un-evaluable literals → `Wild`; a trailing `..` pads a variant's args with `Wild`s to its
    arity). **`useful(matrix, q)`** decides whether a pattern vector matches a value no prior
    row does, via `specialize_var`/`specialize_value`/`default_matrix` + a `col_kind`
    completeness check. **Exhaustiveness** = the all-`Wild` vector is *not* useful against the
    arm matrix (this finds **nested** gaps the old name-set check missed — e.g.
    `node(leaf, leaf)` doesn't cover `node(node(..), ..)`). **Redundancy** = an arm not useful
    against the rows above it → a **warning** (`unreachable match arm`). **Guarded arms are
    excluded** from the matrix (a guard may be false → no coverage, never flagged).
    **Scalars** use a dedicated interval engine (`check_scalar_match`): exhaustiveness via
    `intervals_cover` over the type's `scalar_bounds` (so `true|false` covers `bool` and
    `0..=255` covers `u8` *without* a catch-all — a real improvement), plus interval-subsumption
    redundancy (`5` after `0..=9`); in the *matrix*, scalar columns are treated as never
    complete (a wildcard sibling is required) — sound, and matches the dedicated check.
    **Warnings plumbing:** `Diagnostic` gained a `severity` field (+`Diagnostic::warning`,
    `is_error`); `main.rs` reports warnings but only *errors* block codegen / fail the build
    (`report_program` counts errors; the Build/Run gate is `diags.iter().any(is_error)`).
    **Frontend/backend gap (important):** the *checker* now understands nested patterns, but
    cgen still lowers via the flat switch/if-chain and can't **dispatch** a nested non-wildcard
    subpattern (`node(leaf, leaf)`, `some(0)`) — so the two cgen variant-binding loops now
    **diagnose** ("nested patterns aren't supported by the backend yet …") instead of silently
    miscompiling. The optimal **decision-tree lowering** that closes this gap is the remaining
    §2.4 work. Check-demo [`examples/exhaustive_check.jtr`](examples/exhaustive_check.jtr)
    (1 error + 1 warning).

43. **Decision-tree backend — nested pattern *dispatch* (design §2.4, closing §5.42's gap).**
    cgen now lowers a `match` with **nested** sub-patterns (`node(leaf(_), leaf(_))`, `some(0)`,
    `v(0..=9)`). The core is **`pat_test(subject, subject_ty, pat) -> (C bool test, binding
    stmts)`** — a recursive compiler that ANDs a variant's tag test with its fields' tests and
    collects bindings along full C paths (`tmp.u.node.j_l`). **`pat_test_auto`** auto-dereferences
    a *plain* pointer field (`pointer_pointee`: `*T`/`&[r]T`, **not** a fat `&T`) when matching a
    constructor against it — so a nested pattern looks *through* an `indirect Tree`
    (`(*tmp.u.node.j_l).tag == …`). It's **niche-aware**: `variant_tag_test`/`variant_field` use a
    null test and treat the payload pointer as the lone field for a niche enum. **`emit_nested_match`**
    is an ordered if-chain (`if (<pat_test>) { <binds>; <body> }`), guards composing via
    `emit_guarded_arm`, with the same `goto`/`return` + `__builtin_unreachable` discipline as the
    other chains. **Routing:** `emit_match` checks `pat_needs_nesting` (a variant field that isn't
    a binding/`_`/`..`) and routes there; **flat matches keep their optimized switch/scalar/niche/
    guarded paths unchanged** (so `recursion.jtr`'s flat `node(l, r)` still emits a `switch` — zero
    test churn). The two variant-binding loops' "nested patterns aren't supported" diagnostics are
    now **dead safety nets** (nesting is intercepted upstream). **Limitations:** or-pattern
    alternatives still can't *bind* in a nested position (diagnosed); a fat `&T` field isn't
    structurally looked through. Demo [`examples/nested_match.jtr`](examples/nested_match.jtr)
    (`0, 1, 2, 99`). **This completes §2.4 end-to-end** (analysis *and* dispatch); the only deferred
    polish is an *optimal* shared-test decision tree (the current if-chain re-tests the tag per arm
    — correct, not minimal).

44. **Layout reflection — `align_of(T)` / `offset_of(T, f)` (design §2.7).** Two new cgen
    intrinsics alongside `size_of`, in `emit_call`'s name-keyed dispatch: `align_of(T)` →
    `_Alignof(<c_type T>)` (a C11 keyword — no header), `offset_of(T, f)` →
    `offsetof(Jestyr_<T>, j_<f>)` (`<stddef.h>`, already in the prelude). The first arg is a
    **type** (resolved by `eval_type_arg`, like `size_of`); for `offset_of` the **second arg
    is a bare field name** — an `ExprKind::Name` whose identifier is read directly (never
    emitted as a value; a non-name diagnoses), and the field's C symbol is `j_<name>` (§5.4).
    Both are added to `is_intrinsic` (so a reference isn't mistaken for a closure capture).
    Makes a type's layout inspectable in-language — a seed for CTFE/reflection (workstream G).
    Demo [`examples/reflect.jtr`](examples/reflect.jtr) (`12, 4, 0, 4, 8`).

45. **Struct-variant syntax — named construct + match (design §2.3b).** The brace counterpart
    of the positional variant forms. **Construction** `circle { r: 2.0 }` reuses the existing
    `ExprKind::StructLit` (it already parses): typeck's `StructLit` arm now checks `path` against
    `table.variants` and types it via `variant_ctor_type` (source order taken as field order —
    exact for non-generic, best-effort for generic); cgen's `StructLit` arm routes a variant
    path to **`emit_struct_variant_construct`**, which emits a **designated** tagged-union literal
    (`(Jestyr_Shape){ .tag = …_circle, .u.circle = { .j_r = 2.0 } }`) — handling niche/generic
    like `emit_variant_construct`. **Patterns** `circle { r }` / `rect { w: 0.0, .. }` are a new
    `PatKind::StructVariant { name, fields: Vec<(Ident, PatId)>, has_rest }` (shorthand `r` ≡
    `r: r`, synthesized at parse time; `..` → `has_rest`), parsed from `Ident {` in pattern
    position. They always **route through `emit_nested_match`** (`pat_needs_nesting` → true), where
    `pat_test` resolves each field by name via the new `variant_field_by_name`. **Key gotcha:**
    the typeck table stores enum-variant field *types* but **not names**, so named bindings are
    typed **`Unknown`** in the checker (lenient — cgen projects the real field type from
    `VariantInfo`), and exhaustiveness lowers a `StructVariant`'s fields as **wildcards**
    (`Pat::Var(name, [Wild; arity])`) — coverage is by variant, named-sub-pattern nesting is
    cgen-only. **AST-shape tax:** the new `PatKind` touched ~10 exhaustive `match pat` sites
    (parser, typeck `bind_pattern_types`/`cover_pattern`/`lower_pat`/`collect_scalar_intervals`,
    escape, printer, cgen `pat_test`/`pat_needs_nesting`/`pat_is_constructor` + two dead loop
    arms). Demo [`examples/struct_variant.jtr`](examples/struct_variant.jtr)
    (`12.5664, 12, 0, 9`).

46. **Struct update / spread `Point { x: 9, ..p }` (design §2.8, the first substrate win).**
    `ExprKind::StructLit` gains a `spread: Option<ExprId>` (the `..base` source). Parsed in
    `parse_struct_lit` (a leading `..` ends the field list). cgen lowers a spread to a GNU
    **statement-expression** — `({ Jestyr_Point jss_0 = <base>; jss_0.j_x = 9; jss_0; })` —
    copying the base then assigning the listed fields, so it stays an expression. Pairs with
    immutable `record`: the synthesized field assignments live only in cgen, so they never trip
    the typeck record-mutation check (the user wrote a *construction*, not an assignment).
    **Watch-out (the §5.28 lesson):** `spread` is a new sub-expression, so the StructLit
    destructures in escape (`walk_expr` + `collect_names`), printer, typeck, and the **four**
    cgen walkers (`collect_structs_in_expr`, `find_closures_expr`, `collect_refs`,
    `find_calls_expr`) all had to descend into it — and the combined `StructLit | GenStructLit`
    walker arms were **split** (only `StructLit` carries `spread`). Demo
    [`examples/spread.jtr`](examples/spread.jtr) (`1, 2, 9, 2, 1, 20`). **Remaining §2.8:** field
    defaults (`x: i32 = 0`), per-field visibility (`pub x`), untagged `union`, bit-fields, opt-in
    `Copy`.

47. **Field defaults `x: i32 = 0` (design §2.8).** `StructMember::Field` gains a `default:
    Option<ExprId>`, parsed in `parse_struct_body` as an optional `= <expr>` after the field
    type. cgen fills omitted fields at each construction site: after emitting the literal's
    explicit fields, it appends `.j_<f> = <default>` for every declared field not present (via
    `struct_field_defaults`, which scans `ast.items` for the struct's decl). C designated
    initializers are order-independent, so the appended defaults compose cleanly with the
    explicit ones. **Applies to non-generic `StructLit` only** (not generic `GenStructLit`, and
    not the spread path — a `..base` already supplies every field). **Caveat:** a default is
    emitted at *each* construction site, so it should be a **constant expression** — a default
    containing a generic call wouldn't be seen by the monomorphization walkers (which scan fn
    bodies, not struct decls). Demo [`examples/defaults.jtr`](examples/defaults.jtr)
    (`3, 0, 1, 5, 0, 9`). **Remaining §2.8:** per-field visibility (`pub x`), untagged `union`,
    bit-fields, opt-in `Copy`.

48. **Opt-in `Copy` — `@copy struct` (design §2.8).** Marks a small aggregate as freely
    copyable, so the escape checker never treats it as a move/borrow that could escape (e.g.
    a `read` struct param can now be **returned by value** without `take`). Almost no new code:
    `TypeDecl.is_copy` and `Ty::is_copy()` **already existed** (the field's comment literally said
    "an explicit opt-in lands later"), structs already pass by value in cgen, so this is
    **escape-checker-only**. Wiring: a `@copy` row in the `attrs.rs` registry (`Target::Struct`),
    and one line in `build_table` — `self.table.types[i].is_copy = attrs.iter().any(|a| a.name ==
    "copy")` (the struct's attrs are still on `Item::Struct`, even though §5.31 discards them for
    cgen). No cgen/representation change. Demo [`examples/copy_optin.jtr`](examples/copy_optin.jtr)
    (`3, 7, 20`). **Scoped to structs** (enums could follow the same one-liner at the enum
    registration site). **Remaining §2.8:** per-field visibility (`pub x`), untagged `union`,
    bit-fields.

49. **Per-field visibility — `pub x` (design §2.8).** Struct fields are **private to their
    defining module by default**; `pub` exposes them. `StructMember::Field.is_pub` (parsed as an
    optional `pub` before the field name). Enforced in `typeck::field_type`: when accessing
    `base.f` on a `Ty::Named` **struct** whose owning module (`self.owner[sname]`) differs from
    `self.cur_mod` and the field isn't `pub` (`field_is_pub`, an AST scan), it's an error —
    `field \`f\` is private to module \`m\``. **Same-module access is always free** (so all
    single-file programs and same-module field reads are unaffected — `owner == cur_mod`), and
    the check only fires for non-generic `Named` structs (a generic `GenStruct` field projects
    `Unknown`, no check). **Non-breaking:** no existing example does cross-module non-generic
    struct field access, verified by the green `modules/`+`std/` demos. **Scope/limitations:**
    enforces field *reads* (not construction with a private field), and reuses the same
    module-origin machinery as the fn/const visibility check (§5.25). Demo (2-file)
    [`examples/visibility/main.jtr`](examples/visibility/main.jtr) + `geo.jtr` (`3, 7`);
    cross-module `p.y` on the private field is the rejected case. **Remaining §2.8:** untagged
    `union` / bit-fields.

50. **Untagged `union` (design §2.8).** `union Name { a: i32, b: f32 }` — all fields overlap in
    storage (C `union`); reading a field reinterprets the bytes (type punning, e.g. a float's bit
    pattern). Implemented by **reusing `Item::Struct`** with a new `is_union: bool` (parallel to
    `is_record`) — *zero* new `Item`/`TypeKind` variants, so the whole frontend (registration as a
    `TypeKindG::Struct`, field access, construction, `size_of`, `@copy`, per-field visibility) is
    inherited unchanged. The **only** backend difference is the C keyword: `struct_defs` and
    `forward_types` emit `union`/`typedef union` when `is_union` (one `let kw = if is_union {…}`
    each). The `Union` token was already reserved in the lexer; `parse_named_struct` gained an
    `is_union` arm next to `is_record`. **Construction** uses a designated initializer for one
    field (`(Jestyr_Bits){ .j_f = 2.5 }`); `size_of` is the largest field. **Not added:** an
    `unsafe` requirement on punning reads (the systems-language default trusts the programmer);
    field defaults on a union are meaningless (overlapping) and simply unused. Demo
    [`examples/union.jtr`](examples/union.jtr) (`1075838976, 2.5, 4`). **Remaining §2.8:**
    bit-fields (the last substrate item).

51. **Bit-fields — `flags: u8 : 3` (design §2.8, the final substrate item).**
    `StructMember::Field.bits: Option<u32>`, parsed as an optional `: <int>` after the field
    type (a second colon — the first separates name from type, the second introduces the width;
    it composes with the `= default` that may follow). cgen `struct_defs` lowers it to a C
    bit-field `uint8_t j_flags : 3;` (one `let bf = match bits { Some(n) => format!(" : {n}") …}`).
    Several small fields then pack into one storage unit, so `size_of` shrinks (`1 + 1 + 3 + 3`
    bits → 1 byte vs. 4). **Composes with `@packed`** (both are just C declaration syntax) and
    with field defaults / `pub` / `@volatile` (all live on the same `Field`). Construction and
    field access are unchanged — a bit-field reads/writes like any field (`p.j_a`). **Width is
    not range-checked** against the type size (the C compiler diagnoses an over-wide field).
    Demo [`examples/bitfields.jtr`](examples/bitfields.jtr) (`4, 1, 1, 5`). **This completes §2.8
    and the entire struct/enum/ADT plan** (`docs/structs-enums-design.md`) — all of §2.1–§2.8 are
    ✅; the only deferred items are cross-feature follow-ups (tier-aware `indirect`, a `: u8`
    tag-width repr, an optimal shared-test match decision tree).

52. **Length-carrying `str` view + `cstr` C-interop type (strings workstream, step 1 — design
    "E").** The keystone of the real-strings work. **`str` is now a `{ const char* ptr; size_t
    len }` view** (`JestyrStr` in the prelude) — a borrowed, length-carrying UTF-8 view, like Zig
    `[]const u8` / Rust `&str` — replacing the old bare-`const char*`-with-`strlen` model (§5.29).
    Consequences: `c_type("str")` → `JestyrStr`; a **string literal** lowers to `JSTR("…")`
    (`#define JSTR(s) ((JestyrStr){ (s), sizeof(s) - 1 })` — the byte length is computed by the C
    compiler, so escapes/UTF-8 are honest); **`.len` is an O(1) field** (no `strlen`); `s[i]` →
    `((uint8_t)s.ptr[i])`; `for c in s` walks `s.ptr[0..s.len]` (`emit_str_for`); `print_str`
    takes a `JestyrStr` and prints `%.*s`. A **distinct `cstr`** primitive (`= const char*`, the
    Zig `[*:0]u8` sentinel role) is the C-interop type — `extern "c"` functions take `cstr`, and
    `s.cstr`/`s.ptr` (typeck → `Ty::Prim("cstr")`) bridges a view to a bare pointer at the FFI
    boundary (null-terminated for a literal). `str` stays **non-`Copy`** (a view that borrows its
    data); `cstr` is `Copy` (a raw pointer). **Migration:** `extern_c.jtr` now declares
    `puts(s: cstr)` and calls `puts("…".cstr)`. Demo [`examples/strings.jtr`](examples/strings.jtr)
    (`13, 72, 3, 5,` then the greeting — `"café".len == 5` makes the UTF-8 *byte* cost visible,
    not 4 codepoints). **Next in the workstream:** cost-visible codepoint/grapheme views
    (`codepoints()`/`count_codepoints` O(n)), `bytes`→`str` validate-at-boundary (UTF-8 validity
    as a type-state), an owned `String` (allocator-as-value), `StringBuilder`/iolists (region-
    friendly, zero-copy), f-strings, and a `Bytes` unvalidated-platform-bytes type.

53. **Cost-visible codepoint views (strings step 2).** The "cost in the name" principle: `.len`
    is O(1) bytes; **`count_codepoints(s)` is O(n)** (`jestyr_rt_count_cp` counts UTF-8 leading
    bytes — those whose top two bits aren't `10`). Codepoint *iteration* is explicit:
    **`for cp in codepoints(s)`** decodes one codepoint at a time (`emit_codepoints_for` + the
    `jestyr_rt_decode_cp(ptr, len, &k)` runtime helper, each `cp` a `u32`), never an implicit
    decode (the D-language cautionary tale). `codepoints(s)` is a for-position-only marker
    (`codepoints_iter_arg` intercepts it in `emit_for_inner` *before* the str/slice dispatch,
    since its call type is `Unknown`). Three reusable UTF-8 helpers now live in the prelude:
    `count_cp`, `decode_cp`, and `valid_utf8` (the last seeds step 3). Demo
    [`examples/codepoints.jtr`](examples/codepoints.jtr) (`5, 4, 233, 1` — `"café"` is 5 bytes
    but 4 codepoints; the cost gap is the whole point).

54. **UTF-8 validate-at-boundary (strings step 3).** **`from_utf8([]u8) -> str` is the *only*
    way to turn raw bytes into a `str`** (besides a compile-time-valid literal), so every `str`
    is *proven* valid UTF-8 — validity is a **type-state**, not a per-use runtime tag. It lowers
    to a statement-expr that spills the byte slice once, `assert(jestyr_rt_valid_utf8(ptr,len))`,
    then builds the view — validated once at the edge, trusted thereafter (the provable-language
    upgrade to CJC's model). **`is_utf8([]u8) -> bool`** is the explicit, recoverable check (no
    trap) for when you'd rather branch. Both are `emit_call` intrinsics over a `[]u8` slice's
    `.ptr`/`.len`. Demo [`examples/utf8_validate.jtr`](examples/utf8_validate.jtr) (`1, 2, 2, 0`).
    *(A recoverable `-> str !Utf8Error` variant pairs with the error-set machinery later; today
    the boundary either traps or you pre-check with `is_utf8`.)*

55. **Owned `String` (strings step 4).** The **owned half of the owned/view split**: `String`
    is a heap-owned, growable `{ char* ptr; size_t len; size_t cap; }` (`JestyrString`,
    `c_type("String")`), while `str` borrows. Both **non-`Copy`** now (`is_copy` excludes `str`
    *and* `String`). Runtime + intrinsics: `string_new()`, `string_from(str)` (copies into an
    owned buffer), `string_push(s, str)` (grows in place — emitted `jestyr_rt_str_push(&s, …)`,
    so `s` is taken by address), **`string_view(s) -> str`** (borrows the buffer as a view, *no
    copy* — the owned→view bridge), `string_free(s)`; `String.len` is an O(1) field
    (cgen + typeck). Demo [`examples/owned_string.jtr`](examples/owned_string.jtr) (`5, 12,` then
    `Hello, world` / `Hello, Jestyr` — `greet` returns an owned `String`). *(Uses `malloc`/
    `realloc` directly; threading the stdlib's allocator-value through `String` like `List(T)` is
    the faithful refinement.)*

56. **`Builder` — iolist / StringBuilder (strings step 5).** Erlang-style **iodata**: a `Builder`
    (`JestyrBuilder { JestyrStr* frags; size_t n; size_t cap; }`) collects `str` fragments as
    `{ptr,len}` *views* with **no copying** during building; `builder_build` sums the lengths,
    `malloc`s **once**, and flattens in a **single pass** into an owned `String` — not the
    repeated reallocation of naive `a + b + c`. Intrinsics `builder_new`/`builder_push(b,str)`/
    `builder_build(b) -> String`/`builder_free(b)`; `Builder` is a non-`Copy` prim
    (`c_type → JestyrBuilder`). **Caveat:** fragments are views, so they must **outlive the
    build** — which is exactly why this composes with region arenas (step 7): the escape checker
    can prove no fragment outlives its region. Demo [`examples/builder.jtr`](examples/builder.jtr)
    (`9,` then `[1, 2, 3]`).

57. **f-strings (strings step 6).** `f"… {x} …"` interpolation. **Lexer:** `ident` detects the
    `f"` prefix (`&src[start..pos] == "f" && peek() == '"'`) and scans an `FStr` token. **Parser:**
    `parse_fstring` splits the captured span into literal `parts` and `{ident}` interpolations →
    `ExprKind::FString { parts, exprs }` (`parts.len() == exprs.len() + 1`); interpolations are
    **bare identifiers** (`{x}`), each lowered to an `ExprKind::Name` (no sub-expression re-lexing
    — a documented limit). **typeck** infers each interpolation (so names resolve / types are
    recorded) and types the whole thing as `String`. **cgen `emit_fstring`** builds a fresh owned
    `String` via a statement-expr, formatting per type: `str` inlined, `String` via its view,
    `bool` → `"true"`/`"false"`, integers (and the fallback) via `jestyr_rt_str_push_i64` (decimal,
    copying). **Watch-out (the §5.28 lesson):** `FString` is a new `ExprKind`, so escape's
    `walk_expr` and the printer gained arms (the two the compiler flagged) — its interpolations
    are bare names, so the monomorphization walkers (which have `_` arms) needn't descend. Demo
    [`examples/fstring.jtr`](examples/fstring.jtr) (`Jestyr says x = 42 (true)`, len `25`).
    *(Limits: identifiers only, no `{{`/`}}` brace escaping, floats truncate to int.)*

58. **Region-allocated strings + `bytes` (strings step 7 — the finale, and the differentiator).**
    **Region strings:** `region_str(r, str)` copies a `str` into region arena `r`'s bump buffer
    and returns a view; `region_concat(r, a, b)` allocates `a.len+b.len` in the arena and copies
    both. Used inside `region scratch { … }`, **every fragment is arena-allocated and the whole
    arena frees at the block's end — zero individual frees**, and the region scope makes it
    *lexically impossible* for a fragment to outlive its arena (§5.23). Provably-scoped,
    near-zero-overhead text processing — what heap-only string libraries can't do. The arena name
    is `j_<region>` (same as `region_alloc`). **`bytes(str) -> []u8`** exposes a string's bytes as
    an *unvalidated* `[]u8` (the platform-bytes / WTF-8 home) — the reverse of `from_utf8`,
    completing the bytes↔str round-trip. Demo
    [`examples/region_string.jtr`](examples/region_string.jtr) (`Hello, region!, 14, 5, 4`).
    **This completes the strings workstream (roadmap E), steps 1–7** (§5.52–§5.58): length-carrying
    `str` view + `cstr`, cost-visible codepoint views, validate-at-boundary, owned `String`,
    iolist `Builder`, f-strings, and region strings + `bytes`. *(Deferred refinements: a true
    grapheme iterator, a recoverable `from_utf8 -> str !Utf8Error`, threading the allocator-value
    through `String`/`Builder`, and an escape-checker rule that a region `str` can't leave its
    `region` — today that safety is lexical-by-construction.)*

59. **Substring / slicing (strings step 8 — unblocks `split`/`trim`/`find`).** `s[i..j]` and the
    named `substr(s, i, j)` produce a **boundary-checked, zero-copy sub-view** (Erlang sub-binary
    sharing + Rust's "no slicing on a non-char-boundary"): both lower to one runtime helper
    `jestyr_rt_substr` that asserts `start ≤ end ≤ len` *and* that both ends sit on UTF-8 char
    boundaries (`jestyr_rt_is_boundary`: a byte whose top two bits aren't `10`, or `== len`), then
    returns `{ ptr+start, end-start }` into the same buffer (no allocation). **cgen** intercepts a
    *range* index (`ExprKind::Range`) in the `Index` arm **before** the byte-index path (so `s[i]`
    still reads a `u8`); open ends default (`s[i..]` → `…len`, `s[..j]` → `0`, inclusive `..=` adds
    1). **typeck:** the `Index` arm types `s[range]` as `str` (vs `u8` for `s[i]`); a new
    `string_intrinsic_ret` types the bare intrinsics (`substr`/`from_utf8` → `str`,
    `count_codepoints` → `usize`, `is_utf8` → `bool`) so a `let` **without an annotation** gets the
    right C type. Demo [`examples/substr.jtr`](examples/substr.jtr) (`ell, Hello, lo, 3`).
    **Next (operations):** `eq`/`find`/`contains`/`split`/`trim` build on this view; then the
    region-escape static proof.

60. **Basic string operations (strings step 9).** Byte-level, view-based: `str_eq`,
    `starts_with`, `ends_with`, `contains`, `find` (`-> isize`, byte offset or `-1`), and `trim`
    (a **zero-copy** trimmed sub-view). Each is one `memcmp`/scan runtime helper
    (`jestyr_rt_str_eq` … `jestyr_rt_trim`); the binary ones share `cgen::emit_str_binop`. Typed
    via `string_intrinsic_ret` (eq/prefix/suffix/contains → `bool`, `find` → `isize`, `trim` →
    `str`) so no annotations are needed. With `find` + `substr` you can split by hand; a `split`
    that *returns a collection* needs a string-list/iterator (the next gap). Demo
    [`examples/str_ops.jtr`](examples/str_ops.jtr) (`true, true, false, 7, foo, bar`).

---

## 6. How to add tests

- **Unit/golden tests:** add `#[test]` fns inside the relevant module's
  `#[cfg(test)] mod tests`. Codegen tests assert substrings of the emitted C
  (e.g. `assert!(c.contains("jestyr_max__i32"))`).
- **Property tests:** `proptests.rs`, `mod prop`, inside `proptest! { … }`.
  Generators are regex strategies (`".{0,400}"`) or the recursive `arb_expr()`.
  Assert invariants (totality, span bounds, "generated-valid parses clean").
- **Fuzz tests:** `proptests.rs`, `mod fuzz`, `bolero::check!().with_type::<T>()`.
  These run a bounded corpus under `cargo test` and a real engine under
  `cargo bolero test <name>`.
- `run_pipeline(&str)` in `proptests.rs` drives all five stages and is the
  workhorse for "never panics on any input."

---

## 7. Remaining work (prioritized backlog)

Each item: **what / why / where (files) / approach / size**. **Done so far:**
lettered — A, B, C (method sugar / closures / generic-struct methods), **D**
(generational refs), **E** (bounds-check elision), F (contracts), H (`extern "c"`),
J (structured concurrency), L (diagnostics); plus the `restrict` speed work and these
**Section-C / design-doc features**: layout attributes (`@packed`/`@align`/`size_of`),
the **general attribute system** (registry-validated `@inline`/`@no_inline`/`@hot`/
`@cold`/`@must_use`/`@deprecated`/`@no_mangle`; workstream D — see `attrs.rs` + §5.30),
slices `[]T` + bounds-checked indexing, match exhaustiveness + payload projection,
bare-metal `@volatile`/`@address`, the **immutable `record`** (struct/record split —
§5.32; full struct/enum/ADT plan in `docs/structs-enums-design.md`), and **MVS**
(default = `read`).
**Remaining (lettered):** G (CTFE + reflection), N (self-hosting). **D is fully
done** (both reference tiers); **K (modules) and I (stdlib + allocator) are now
done** (this session — see their entries below). **Remaining (Section C):**
traits/`dyn` + definition-site constraints, `@verified` (SMT), build system +
package manager, real `String`/text. (M dropped → **Motley**, see `MOTLEY.md`.)

> **Loops ✅ DONE — MVP + all spec'd fast-follows.** Unified `for` (design:
> [`docs/loops-spec.md`](docs/loops-spec.md); user reference + examples:
> [`docs/loops.md`](docs/loops.md)). Shipped: range (`for i in 0..n`/`0..=n`),
> slice (`for x in xs`, `for mut x in xs` in place), wildcard `for _`, conditional
> `for cond`, infinite `for {}`, `break`/`continue`, `invariant`→assert, **free
> bounds-check elision** on range indices, reserved `while`/`loop` → "use `for`".
> **Fast-follows now also done:** the full borrow contract — *iterator invalidation
> is a compile error* (mutating an iterated collection is rejected); **element+index**
> `for x, i in xs`; **lockstep zip** `for x, y in xs, ys` (equal lengths checked);
> **region-scoped scratch arenas** `for … region scratch { }` (per-iteration O(1)
> reset, freed once); **`@no_panic`** functions (un-elided index = compile error).
> Demos `examples/loops.jtr` + `examples/loops_advanced.jtr`; gotcha §5.28.
> **Also done:** **labeled `break`/`continue`** (`for outer: …` → C `goto`),
> **`step`/descending ranges** (`for i in n..0 step -1`; signed index, no elision when
> stepped), **`variant`** termination measures (per-iteration `>= 0` + strict-decrease
> asserts), **casts** (`expr as T`, numeric + pointer), and **byte-level string
> iteration** (`for c in text`, `text.len` via `strlen`). User reference:
> [`docs/loops.md`](docs/loops.md). **Still deferred:** `take`-iteration,
> value-yielding loops, loop-`else`, custom iterators, **Unicode** string iteration
> (`for cp in text.codepoints()`), a length-carrying `String`, `par`.

> **Roadmap note (for the stated goals — Rust-like speed + rewriting compilers):**
> *speed is banked*: the C backend is at ~hand-C speed, `mut` borrows carry
> `restrict`, E elides bounds checks, and region refs (`&[r]T`) give zero-cost
> arena references. For *rewriting a compiler in Jestyr* (→ CJC-Lang/Carcosa on
> **Motley**), the gates are the systems tier — **K** (modules), **I** (stdlib),
> then **N** (self-hosting) — not the language theory.

### A. Method-call sugar — `v.push(x)`  ✅ DONE
- Demo `examples/methods.jtr`; gotcha §5.12. `typeck::resolve_free_method`
  matches `Field`-callee `name` to a free fn whose first non-comptime param
  head-matches the receiver, recovering comptime type args by unification;
  `cgen::emit_method_call` lowers it to a free call with the receiver threaded in
  (by `&` for `mut`/`out`). The give-away (route 4) escape check now fires through
  method calls too.

### B. Closure lowering — lambda-lift to fn-ptr + env  ✅ DONE
- Demo `examples/closure_run.jtr`; gotcha §5.13. `cgen` collects closures from
  non-generic bodies, emits `JestyrEnv_*` / `JestyrClosure_*` / `jestyr_lam_*`, and
  lowers `f(args)` → `f.call(&f.env, args)` (inline/IIFE closures spill to a temp).
  **Remaining:** closures inside generic bodies, closures crossing function
  boundaries (closure-typed *parameters* — needs a type-erased `void*` env), and
  capturing `mut`-pointer params by reference.

### C. Generic struct *methods*  ✅ DONE
- Demo `examples/genmethods.jtr`; gotchas §5.12 & §5.14. Methods declared *inside*
  the struct `List(T)` returns are monomorphized per instance as
  `jestyr_List__i32_push(...)`, sharing the fn-instance worklist
  (`collect_all_instances`). `xs.push(x)` resolves via `resolve_struct_method`
  (sets `MethodRes.recv_ctor`). **Remaining:** fallible methods (`!{…}` on a
  method — currently diagnosed), and `Self{ … }` literals in the C backend (the
  flagship `vec.jtr` still uses `Self{…}` + an `Allocator` value — see §8).

### D. Stored-reference tiers — BOTH tiers  ✅ DONE (the tiered-safety model is complete)
- **Generational refs `&T`** (runtime-checked) — demo `genref.jtr`; gotcha §5.21.
  `TypeKind::GenRef` / `Ty::GenRef` (`Copy`) → `JestyrRef_<T> { T* ptr; uint64_t
  gen; }`. `gen_new(T,v)` allocates `[gen | T]` and snapshots gen=1; `r.*` is a
  **checked deref** (`assert(((uint64_t*)ptr)[-1] == r.gen)`) so use-after-free
  **faults at runtime**; `gen_free` bumps the generation.
- **Region refs `&[r]T`** (zero-cost, compile-time/lexical) — demo `region.jtr`;
  gotcha §5.23. `TypeKind::RegionRef` / `Ty::RegionRef` (`Copy`) → a **plain `T*`**;
  `r.*` is a **raw deref** (no check). `region r { … }` opens a bump arena
  (`jestyr_arena_*` prelude), `region_alloc(r,T,v)` hands out zero-cost pointers
  into it, and the whole arena is freed O(1) at the block's brace. This is the
  *zero-cost half* of tiered safety — the menu: prove-it-pay-nothing (region/E) vs
  check-it-cheaply (gen-ref).
- **Remaining:** writing *through* a gen-ref (`r.* = v`); a real escape rule that a
  `&[r]T` cannot outlive its `region` block (today it's lexical-by-construction, not
  *checked*); `gen_free` leaks (bootstrap — a real impl uses a reuse-safe pool);
  gen-ref header trick assumes element align ≤ 8.

### E. Refinement types — bounds-check elision  ✅ DONE (elision; narrowing-check is the companion)
- Demo `refine.jtr`; gotcha §5.22. `cgen` carries the current function's refined
  params (`cur_refines`, from `Param.refine`); `index_in_range` proves `i < s.len`
  when the index is a param refined `in <lo>..s.len` naming the indexed slice, and
  the `Index` arm then emits the **raw access** (no `assert`). So safe code is as
  fast as unchecked C. **Remaining:** general refinements (`Percent = i32 in
  0..=100`, constant bounds with compile-time checks); **narrowing-site enforcement**
  (insert a check at the call when an argument isn't provably in range — makes the
  elision fully *sound* rather than a caller promise); arithmetic on refined values.

### F. Contracts — `requires` / `ensures`  ✅ DONE
- Demo `examples/contracts.jtr`; gotcha §5.15. `parser::parse_fn` parses any number
  of `requires <expr>` / `ensures <expr>` clauses between the signature and body
  (stored on `FnDecl`). `cgen::emit_fn_body` asserts preconditions on entry;
  `emit_value_return` spills the value to `j_result`, asserts each `ensures` (so
  `result` is in scope), at **every** return point. Lowers to C `assert` — active
  in debug, elided under `-DNDEBUG`. **Remaining:** contracts on *methods* (only
  free fns today), `@verified` static proof (design's third rung).

### G. Comptime evaluation beyond type params — CTFE + reflection  *(large — design §8)*
- **Why:** we have `comptime T: type` (generics) but not general compile-time
  *function execution* or field reflection. This is the D/Zig metaprogramming core.
- **Approach:** a small comptime interpreter over the AST; `comptime` blocks/consts;
  reflection as comptime library calls over type values. **This is the single
  largest unbuilt piece of the metaprogramming story** — the type-returning slice
  (generics) works because it needs only *substitution*, not an *evaluator*.

### H. Real C interop — `extern "c"`  ✅ DONE (single-decl form)
- Demo `examples/extern_c.jtr`; gotcha §5.16. `Item::Extern` carries a bodyless
  signature; typeck registers it in `table.fns`; `cgen::extern_protos` emits a C
  prototype and `emit_call` calls it by **bare name** (no `jestyr_` mangling), so
  it links straight to libc. **Remaining:** `import c "header.h"` (comptime header
  translation), `extern` *blocks*, and actually *retiring* the print/alloc
  intrinsics in favour of `extern` decls.

### I. Standard library + allocator-as-value  ✅ DONE (bootstrap scope)
- Demos `examples/std/{alloc_demo,demo}.jtr`; gotcha §5.24–26. Shipped as Jestyr
  modules (dogfooding K): **`mem`** — an `Allocator` *value* (`enum Allocator {
  system, arena(h) }`) that every allocating API takes; `allocate`/`release`/
  `destroy` dispatch on it via `match` (the bootstrap encoding of first-class
  allocators — a real fn-ptr vtable awaits fn-pointer types / traits). **`list`** —
  a generic growable `List(T)` whose `push` grows (doubling, recursive copy — no
  loops yet) using the passed allocator. **`core`** — allocation-free helpers
  (`imax`/`imin`/`abs_i32`). **`io`** — named wrappers over the print intrinsics.
  Backend: three new intrinsics back the value-level arena — `arena_open(cap)` →
  opaque `*mut u8` handle, `arena_alloc(h,T,n)` typed bump, `arena_close(h)` bulk
  free — reusing the region arena runtime (`uses_arena()` now also triggers on them).
  The *same* `list`/`fill_and_sum` code runs against `system` **and** `arena`
  allocators (proof: `alloc_demo` → 60, 60). **Remaining:** pool/fixed-buffer/page/
  debug allocators; the ambient allocator *context* (Odin); a fn-ptr/trait-based
  dynamic `Allocator`; bounds-checked/refined `list.get`; retiring the print/alloc
  intrinsics behind `extern "c"` (now possible via H).

### J. Concurrency  ✅ DONE (structured-concurrency core)
- Demo `examples/concurrent.jtr`; gotcha §5.17. `concurrent { spawn f(args) … }` is a
  nursery (design §10.2): each `spawn` site becomes an arg struct + a `void*`
  pthread trampoline (`jestyr_task_<id>`); the scope `pthread_create`s each task and
  `pthread_join`s them all at the closing brace. `main.rs` adds `-pthread` when the C
  uses threads. **Remaining:** `spawn` of closures (design's `s.spawn(|| …)` — needs
  first-class closures); task **results** + `await`; sync types (`Mutex`/`Atomic`/
  channels); `concurrent` join-safety in the escape checker (borrows into tasks are
  sound by structure but not yet *checked*); effect-polymorphic async (design says
  open). Threads map to OS threads (design §10.4).
### K. Module/package system  ✅ DONE (bootstrap scope) — *the gate for compiler-scale Jestyr*
- Demo `examples/modules/main.jtr`; gotchas §5.24–27; loader in `module.rs`.
  `import "path"` (relative, `.jtr` appended) / `import "path" as alias`; `pub` vs
  module-private with **visibility enforced** on calls/qualified access; **qualified
  access** `mod.func(args)` / `mod.CONST`; **cycle detection** (DAG enforced);
  **diamonds load once**. Multi-file programs merge into one shared arena + one
  source buffer (global spans → correct per-file diagnostics). 5 loader tests + 2
  full-pipeline integration tests over the example files. **Remaining (deferred):**
  true per-module namespaces (today the merged namespace is *flat* — names must be
  globally unique, §5.24); directory-as-module (today file-as-module); qualified
  *type* paths + per-module type privacy (§5.26); a `build.jestyr` + manifest +
  lockfile + vendored deps; parallel/incremental compilation off the DAG.
### L. Teaching-quality diagnostics  ✅ DONE
- `diag.rs`: `Diagnostic` gained optional `code`/`help` and a `render` that prints a
  labelled header, a `--> path:line:col` locator, and the offending source line with
  a caret underline (rustc-style). `Severity` = Error/Warning/Note. `main::report`
  and the emit-c notes use it. **Remaining:** assign stable codes per rule, attach
  `help:` suggestions, multi-line spans, color.

### Speed: `restrict` on exclusive borrows  ✅ DONE
- `cgen::borrow_ptr_cty` emits `T* restrict` for `mut`/`out` params and `self`,
  surfacing the ownership model's non-aliasing guarantee to the C optimizer (Rust
  `noalias` equivalent). `bench_fib.jtr` measures **0.985× hand-written C**.
  **Remaining (still ★★★ for speed):** **D** (region refs, so safe code isn't
  forced onto runtime-checked generational refs) and **E** (refinement-driven
  bounds-check elision — needs a safe-index primitive to elide first). Soundness of
  `restrict` rests on the exclusivity the escape checker enforces; the full
  aliasing guarantee lands with **D**.

### M. ~~LLVM/Cranelift release backend~~ → **"Jestyr's Motley"** (own backend; down the line)
  Decided: *not* using LLVM or Cranelift. A bespoke backend ("Jestyr's Motley") will
  come later. C-via-`gcc -O2` (single-TU) is already at hand-C speed, so this is a
  future *compile-speed* / control play, not a runtime win — deferred.
### N. Self-hosting  *(large — design §19 Phase 6)*.

---

## 8. The flagship `examples/vec.jtr`

The original design example (`Vec(T)` with allocators, error sets, refinements).
It type-checks and escape-checks but does **not** fully compile to a binary. With
**A** (method sugar) and **C** (generic struct methods) now done, `genmethods.jtr`
is the runnable proof that the generic-container-with-methods half works. To get
the *actual* `vec.jtr` running end to end the remaining gaps are: **I**
(allocator-as-value, to replace the `Allocator` param), **E** (refinement runtime
for `i: usize in 0..self.len`), `Self{ … }` literals in the C backend, **fallible
methods** (`push` is `!{ OutOfMemory }` — currently diagnosed by the backend), the
`@address` attribute (or drop that const), and a `main`. It's the integration test
for "the language is real."

---

## 9. Pointers

- **Vision & rationale:** [`jestyr-design.md`](jestyr-design.md) — thesis (§1),
  ownership model (§4), error handling (§6), type system (§7), metaprogramming
  (§8), and the prototyping roadmap (§19). The implementation has completed roughly
  design Phases 0–1 and parts of 2–3.
- **Most-edited files going forward:** `cgen.rs` (backend features), `typeck.rs`
  (resolution/types), `escape.rs` (the safety model), `parser.rs`/`ast.rs` (new
  syntax).
- **Invariant to preserve:** every feature lands with (a) a runnable demo in
  `examples/`, (b) unit tests asserting emitted C / diagnostics, and (c) green
  `cargo test` (incl. the fuzz tests, which catch panics introduced by new AST
  shapes). Keep the build warning-clean.
```
