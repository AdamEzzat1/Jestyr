# Jestyr self-hosting — the remaining port: P2 → P5 + R2 (cold-start handoff)

> Authoritative handoff for the rest of the self-hosting port (ROADMAP workstream P).
> **Front end classification is DONE and cross-checked:** P1 (full token set, lexeme
> boundaries) and P2a (full token *kinds*) both match the Rust reference token-for-token
> over the whole 122-file corpus. What remains is the bulk: **P2 parser → P3 typeck →
> P4 escape → P5 cgen** (~27K lines), each gated by a cross-implementation golden on the
> shared corpus, then the **R2 fixpoint** as the acceptance test.
>
> Read with `ROADMAP.md` §P, `jestyr-selfhost-blockers-handoff.md` (§4 Tier-3, §5 Tier-4),
> `jestyr-design.md` §19, and the commits `489aa2d` (P1), `6e10361` (P2a).

---

## 0. Discipline (unchanged — applies to every increment)

- Stay `cargo test`-green and warning-clean. Default suite stays toolchain-free; the
  cross-impl goldens live behind `--features c-oracle` (they build a Jestyr program with
  gcc and run it).
- Ship the four test layers: **unit/wiring**, **property**, **bolero fuzz**, and
  **teeth-by-mutation** (break the new logic, watch a test fail, revert).
- Keep programs that don't use a new feature **byte-identical**.
- **Auto-commit each green increment to `master`** (`git commit -F <file>`, end with
  `Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>`), then `git push origin master`.
- Land increments small. A pass is ported *construct by construct*, each with its slice of
  the golden — never a 2.6K-line drop.

---

## 1. The load-bearing pattern — the cross-implementation golden

Every Jestyr-written pass must produce **identical output to the Rust pass on the same
corpus**. This is already mechanized; **reuse the toolkit verbatim**
(`src/proptests.rs`, `mod c_oracle`, behind `--features c-oracle`):

- `build_exe(rel) -> PathBuf` — compile a `.jtr` program to a native exe (load → typeck →
  escape → cgen → gcc), returns the exe path. Use for any Jestyr pass that takes CLI args.
- `run_jestyr_lexer(exe, file, extra) -> Vec<String>` — run the exe, drop the trailing
  6 summary lines, return output lines. Generalize/rename per pass.
- `rust_lexemes(src)` / `rust_kinds(src)` — the Rust reference output (lexer used as a
  **library**, not a subprocess). Model P3/P4/P5 references the same way: call
  `typeck::check_program`, `escape::check`, `cgen::emit` directly.
- The two goldens already in place — `jestyr_lexer_matches_reference_on_corpus` (P1,
  lexemes) and `jestyr_lexer_kinds_match_reference_on_corpus` (P2a, kinds) — are the
  **templates**. Each of P2–P5 adds one: AST dump → resolved-type dump → diagnostic set →
  byte-identical C.

This is *why* module content-hashing + `attest` exist: "same input ⇒ same output" is a
mechanical hash compare across implementations and stages.

---

## 2. What exists to build on (the Jestyr-side toolbox)

- **Front end (Jestyr):** `examples/std/lexer.jtr` — full token set (P1), kind-dump mode
  (`lexer.exe <file> kinds`, P2a). Its output is one token per line (lexeme, or kind
  label with a 2nd CLI arg) + a 6-number summary. For the parser it will likely become a
  *module* the parser imports and calls in-process (see §3).
- **std toolbox:** `fs` (+ recoverable `try_read_text`, B3), `env` (argv), `intern`
  (str→dense id — **the chosen symbol-table strategy, B2**, the rustc `Symbol` pattern),
  `strmap` (str→i64 open-addressing), `list` (growable `List(T)`), `mem`, `core`.
- **Language features cleared** (no known blocker to writing the compiler): real strings,
  traits + `dyn`, fn-pointer types, generics + monomorphization, recursive ADTs
  (`indirect`), enums + `match` + exhaustiveness, error sets + `?`, RAII incl. nested
  fields (B1), value-position `unsafe`/block (B4), inline `slice` typing (B5).
- **The Rust compiler is the reference/oracle** for every stage:
  `src/lexer.rs`, `src/parser.rs` (~2.6K), `src/typeck.rs` (~4.4K), `src/escape.rs`
  (~1.4K), `src/cgen.rs` (~9.6K), and `src/{ast,types,token,span,module,diag}.rs`.

---

## 3. P2 — the parser (the next major effort; ~2.6K lines)

**Goal:** a Jestyr program that reads the classified token stream and builds an AST,
matching `src/parser.rs` construct for construct, verified by an AST-dump golden.

### 3.1 Prerequisite: fix the deep-expression stack overflow FIRST
The Rust parser (and typeck/cgen) recursive walks **stack-overflow between expression
depth 500 (ok) and 1000 (overflow)** — measured in the self-host experiment; flagged as
background task `task_109fb4cb`. A Jestyr recursive-descent parser will hit the same wall
on its own stack. Add a **recursion-depth guard** that emits a clean "expression nesting
too deep" diagnostic to *both* implementations (so they still agree: both reject
over-deep input rather than one crashing). Repro: `fn main() -> i32 { return 1+1+…+1 }`
with ≥1000 operands.

### 3.2 AST representation — arenas, not recursive enums
Mirror `src/ast.rs`: the Rust AST is **arena vectors** (`exprs: Vec<ExprData>`,
`types`, `pats`) addressed by integer newtype ids (`ExprId(u32)`, `TypeId`, `PatId`).
Port this directly to Jestyr: `List(ExprData)` + integer indices. This **dodges
recursive-enum ownership questions** (a node references children by id, not by owned
pointer) and matches the reference structure 1:1, which makes the AST dump easy to align.
- Mirror `ExprKind` / `Stmt` / `Item` / `TypeKind` / `PatKind` as Jestyr enums. Keep the
  exact node set and field order.
- `Span` is `{start: u32, end: u32}` — trivial to carry.

### 3.3 Token feed
The parser needs random-ish access to a token vector with `(kind, span)`. Two options:
- **(a) In-process (preferred for the real compiler):** refactor `lexer.jtr` so the
  tokenizer fills a `List(Token)` (a `Token` = `{kind: i32, start: usize, end: usize}`)
  that the parser imports and consumes. The kind is an integer tag matching `TokenKind`'s
  discriminant order (define a shared numbering).
- **(b) Text hand-off (cheap first step):** have the parser read the kind-dump the lexer
  already emits. Brittle for spans; use only to bootstrap.
  Prefer (a). It also lets you keep the P2a kind cross-check as a unit test of the shared
  tokenizer.

### 3.4 The AST-dump golden
- The Rust side already has `print_ast` (drives `jestyrc parse`). Make it (or a new
  canonical printer) emit a **deterministic, structural dump** — e.g. an S-expression per
  node with kind + children ids + spans — that is a *pure function of the AST* (fixed
  field order, no HashMap iteration).
- The Jestyr parser emits the **same** canonical dump. Cross-check: for each corpus file,
  `jestyr_parse_dump(f) == rust_parse_dump(src)`. Stage it: start with only the
  constructs the Jestyr parser handles and a corpus subset that uses only those; grow both.
- **Watch:** the dump must be span-exact and order-exact, or the diff is noisy. Decide the
  canonical form up front and pin it with a small golden before scaling to the corpus.

### 3.5 Staging (suggested increment order)
1. **DONE** (commit `89721f0`). Depth-guard fix — `parser::MAX_EXPR_DEPTH = 256` bounds
   AST *height* (so a left-deep `1+1+…` fold is caught too), reports once, teeth in
   `parser.rs`. Jestyr inherits the same cap when P2's parser is written.
2. **DONE.** Shared tokenizer producing `List(Token)`. `examples/std/lexer.jtr` now has
   `tokenize(...) -> List(Token)` — each `Token` is `{kind: i32, start, end}` with `kind`
   the `TokenKind` discriminant (Ident=0 … Unknown=111); keyword kind = `interned_id + 7`,
   operators via `classify_op`. `lex` walks the list to print lexemes / labels / integer
   tags, so P1 + P2a stay byte-identical. **Golden strengthened:** a `nums` mode emits the
   integer tag per token, cross-checked vs the reference discriminants over the whole
   corpus (`jestyr_lexer_kind_ids_match_reference_on_corpus`) — closes the label golden's
   blind spot (operator/keyword labels are span-derived, so the *integers* were unverified).
   `List(struct)` confirmed working (the AST-arena primitive).
   **Extraction DONE:** the tokenizer now lives in `examples/std/tokens.jtr` (`pub struct
   Token { pub kind, pub start, pub end }`, `pub fn tokenize -> List(Token)`,
   `pub fn intern_keywords`); `lexer.jtr` is a thin driver that `import`s it. The parser
   will `import "tokens"` and hold `List(tokens.Token)` in-process. This required a **cgen
   fix** (`eval_type_arg`, cgen.rs): a module-qualified comptime type argument `mod.Type`
   (e.g. `list.get(tokens.Token, …)`) previously degraded to `Opaque("?")`, so a generic
   container instantiated over an imported type mangled to `Jestyr_List__?` in the consumer
   vs `Jestyr_List__Token` in the producer → invalid C. Now it resolves via the import map
   like the type-position `TypeKind::Path` resolver, so producer and consumer agree. This
   unblocks the whole modular architecture (any module returning `List(ExprData)` etc.).
   (Aside: naming a user struct literally `T` still collides with the blanket-impl generic
   param `T` and skips its Drop glue — a separate pre-existing corner; `Token` avoids it.)
3. **Expression parser — first slice DONE.** `examples/std/parser.jtr` imports `tokens`,
   builds an expression AST as a threaded `List(ExprData)` arena + integer child ids
   (mirroring `src/ast.rs`'s `ExprId` arenas), and dumps a canonical **flattened
   S-expression** (one atom/line: kind label + operator label + exact span + children in
   order — a pure function of the arena). The Rust reference emits the identical stream via
   a new `Parser::parse_single_expr` (parser.rs) + `ref_dump_expr` (proptests.rs); the
   golden `jestyr_parser_expr_dump_matches_reference` diffs them over a curated expression
   corpus. Handled: int/float/name leaves, prefix unary (`-`/`!`/`not`/`~`/`&`), the full
   binary precedence table (matching `bin_op`), `( … )` grouping. Spans exact (leaf = token
   span; unary = `op.start..rhs.end`; binary = `lhs.start..rhs.end` per `Span::to`). Teeth:
   perturbing `*`'s precedence flips `1+2*3`'s tree and fails the golden (shape + span).
   *Deferred to later slices (grow the corpus alongside):* assignment, ranges, `as` casts,
   postfix (call/field/index/`?`/`.*`), struct/array literals, `if`/`match`/blocks, and the
   recursion-depth guard (§3.1) — the Jestyr parser recurses, so it needs the same
   `MAX_EXPR_DEPTH` cap before it sees adversarially-deep input.
   **Design constraint discovered (arenas are threaded, not nested):** cgen does not yet
   order a generic `List(T)` embedded *by value as a struct field* before its container
   (emits an incomplete C type), so the parser threads the arena + token vector as
   standalone `mut`/`read` params plus a scalar-only `Cur` cursor — which also matches the
   reference (`Ast` owns the vecs, `Parser` borrows them). Two cgen gaps logged for a later
   dedicated pass: (a) generic-`List`-as-struct-field ordering; (b) a user struct literally
   named `T` collides with the blanket-impl generic param `T` and skips its Drop glue.
4. Type parser + pattern parser.
5. Statement parser (`let`/`var`/`return`/expr-stmt/blocks/`if`/`match`/loops).
6. Item parser (`fn`, `struct`/`record`/`union`, `enum`, `trait`/`impl`, `const`,
   `distinct`, `import`, attributes, contracts) — then the whole-corpus AST golden.

---

## 4. P3 typeck → P4 escape

- **P3 typeck** (`src/typeck.rs`, ~4.4K): name resolution + type inference producing the
  per-expr `Ty` table. Largest pass after cgen. Stage it as the Rust one is structured:
  build the global table first (two-phase, order-independent — `build_table`/`GlobalTable`),
  then check bodies. Use the intern+id-indexed-array discipline (B2) for symbol tables.
  **Golden:** a resolved-type dump — each `ExprId` → its `Ty` (canonical form) — diffed
  over the corpus. The leniency rules must match exactly (unknown named type → `Opaque`,
  generic param → `Opaque`, treated non-`Copy`).
- **P4 escape** (`src/escape.rs`, ~1.4K): the ownership/borrow-escape checker (the
  smallest pass). **Golden:** the *set of diagnostics* (message + span) must equal the
  Rust checker's on the corpus. The escape examples (`examples/escapes.jtr`,
  `examples/region_escape.jtr`) already pin expected error counts — reuse them.

---

## 5. P5 — cgen (the biggest pass) and the byte-identity lever

- **P5 cgen** (`src/cgen.rs`, ~9.6K): lower AST + types to C. The acceptance bar is the
  strongest and already mechanized: **the Jestyr cgen must emit byte-identical C to the
  Rust cgen** on every corpus program. That is exactly what
  `proptests.rs::compilation_is_deterministic` + the `attest` C-hash assert *within* the
  Rust impl; the cross-impl version diffs Jestyr-emitted C vs Rust-emitted C.
- Port by construct (the Rust cgen is a big `emit_expr`/`emit_stmt` match). After each
  construct, the subset of the corpus using only ported constructs must match byte-for-byte.
- **Content-hash + `attest` earn their keep here:** use the module content-hash as the
  cache key and `attest` to pin the emitted-C hash, so cross-impl equality is a hash
  compare, not a full text diff.
- If the emitted C matches, the downstream gcc build + run behavior is identical for free.

---

## 6. R2 — the fixpoint (the acceptance criterion)

Self-hosting is *proven* by a 3-stage fixpoint (gate behind `--features selfhost-fixpoint`,
outside the toolchain-free default suite):
- **stage-1:** the Rust compiler builds the Jestyr compiler → `jc1`.
- **stage-2:** `jc1` builds the Jestyr compiler from the same source → `jc2`.
- **stage-3:** `jc2` builds itself → `jc3`.
- **Assert `jc2 ≡ jc3` byte-for-byte** (the emitted C, and ideally the final binary).

Stand it up *as soon as a partial stage-1 self-build exists* — even a compiler that only
handles a language subset can fixpoint on a subset corpus — so regressions are caught from
day one. Use the compiler source's module content-hash as the cache key; `attest` pins the
stage-2/stage-3 artifacts. **Teeth:** perturb one byte of a stage input → the diff fails.

---

## 7. R1 — scaling (measured; one bug to fix first)

From the self-host experiment (evidence, not speculation):
- **Program *size* scales fine:** 3000 functions / 15K lines compiles without issue.
- **Expression *depth* does not:** recursive walks **stack-overflow between depth 500 (ok)
  and 1000 (overflow)**. Fix before the port hardens (§3.1, task `task_109fb4cb`).
- **Mitigation to build early:** a "large input" corpus (concatenate std+examples into
  ever-bigger single modules) + a `huge_program_compiles` test; profile cgen if compile
  time bites at 27K lines.

---

## 8. Recommended order

1. **Fix the deep-expression stack overflow** (both impls) — before the Jestyr port
   inherits the pattern. (task `task_109fb4cb`)
2. **P2 parser** — shared tokenizer → expr parser → types/patterns → statements → items,
   each with its slice of the AST-dump golden.
3. **P3 typeck** → resolved-type-dump golden. **P4 escape** → diagnostic-set golden.
4. **P5 cgen** → byte-identical-C golden (per construct, then whole corpus).
5. **R2 fixpoint** → stand up early on a subset; expand as P5 completes.

---

## 9. Anchors

| Thing | Where |
|---|---|
| Reference lexer / token set | `src/lexer.rs`, `src/token.rs` (`TokenKind`, `describe`) |
| P1/P2a Jestyr lexer | `examples/std/lexer.jtr` |
| Cross-impl golden toolkit | `src/proptests.rs` `mod c_oracle`: `build_exe`, `run_jestyr_lexer`, `rust_lexemes`, `rust_kinds`, `jestyr_lexemes`, `jestyr_kinds`, and the two `..._match_reference_on_corpus` tests |
| Reference parser / AST | `src/parser.rs`, `src/ast.rs` (`ExprId`/arenas; `print_ast`) |
| Reference typeck / types | `src/typeck.rs`, `src/types.rs` (`TypeInfo`, `GlobalTable`) |
| Reference escape | `src/escape.rs` (+ `examples/escapes.jtr`, `region_escape.jtr`) |
| Reference cgen | `src/cgen.rs` (+ `attest` C-hash, `Modules::hashes`, `compilation_is_deterministic`) |
| Symbol strategy (B2) | `examples/std/intern.jtr` |
| Deep-expr overflow (R1) | task `task_109fb4cb`; repro `return 1+1+…` depth ≥ 1000 |

## One-line summary

Front-end classification is done and cross-checked (P1 lexemes + P2a kinds, 122 files
each). The remaining port is **P2 parser → P3 typeck → P4 escape → P5 cgen**, each gated
by a cross-implementation golden on the shared corpus (AST dump → resolved-type dump →
diagnostic set → byte-identical C), then the **R2 fixpoint** (`jc2 ≡ jc3`) as acceptance —
stood up early on a subset. Represent the AST as `List(T)` arenas + integer ids, and fix
the deep-expression stack overflow before the Jestyr port inherits it.
