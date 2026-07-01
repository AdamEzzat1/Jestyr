# Jestyr self-hosting — the port: P2–P5 + R2 fixpoint (handoff)

> Cold-start handoff for the remainder of the self-hosting port (ROADMAP workstream P).
> **P1 (full-token-set lexer) is DONE** — `examples/std/lexer.jtr` matches the Rust
> reference lexer token-for-token across the whole 122-file corpus (`489aa2d`). What
> remains is the bulk of the work: porting **parser → typeck → escape → cgen** to Jestyr
> (~27K lines), each gated by *cross-implementation equivalence* on a shared corpus, then
> standing up the **R2 fixpoint** as the acceptance test. Read with the repo's
> `jestyr-selfhost-blockers-handoff.md` (§4 Tier-3, §5 Tier-4), `jestyr-design.md` (§19),
> `ROADMAP.md` §P, and the P1 commit.

---

## 0. Discipline (unchanged — applies to every increment)

Every increment stays `cargo test`-green and warning-clean (default suite stays
toolchain-free; the gcc-oracle cross-checks live behind `--features c-oracle`). Ship the
four test layers (unit/wiring, property, bolero fuzz, teeth-by-mutation), keep programs
that don't use a new feature byte-identical, and **auto-commit each green increment to
`master`** (`git commit -F <file>`, `Co-Authored-By: Claude Opus 4.8 …`), then push.
Write a session summary to `~/Downloads/` when a stage's first cut lands.

**The load-bearing pattern for the whole port** — established by P1, reuse it verbatim:
the Jestyr-written pass and the Rust pass must produce **identical output on the same
corpus**. P1's test (`proptests.rs::c_oracle::jestyr_lexer_matches_reference_on_corpus`)
is the template: build the Jestyr pass to an exe (`build_exe`), run it on every
`examples/**/*.jtr`, and diff its output against the Rust pass used as a library. Each of
P2–P5 gets its own such golden. This is *why* the project built module content-hashing +
`attest`: "same input ⇒ same output" is mechanically checkable across implementations.

---

## 1. What exists to build on

- **P1 lexer (`examples/std/lexer.jtr`)** — full token set, cross-checked. Its output is a
  lexeme-per-line dump + a 6-number summary; the cross-check strips the summary and diffs
  lexemes. For P2 the parser will consume *tokens*, so the lexer likely needs a companion
  that emits `(kind, span)` per token rather than bare lexemes — see §2.
- **std toolbox in Jestyr:** `fs` (recoverable `try_read_text` too, B3), `env` (argv),
  `intern` (str→dense id, the `Symbol` pattern — **the chosen symbol-table strategy, B2**),
  `strmap` (str→i64 open-addressing), `list` (growable `List(T)`), `mem`, `core`.
- **Language features cleared:** real strings, traits + `dyn`, fn-pointer types, generics
  + monomorphization, recursive ADTs (`indirect`), enums + `match` + exhaustiveness, error
  sets + `?`, RAII incl. nested fields (B1), value-position `unsafe`/block (B4), inline
  `slice` typing (B5). No known language blocker remains for writing the compiler.
- **The Rust compiler is the reference/oracle** for every stage: `src/lexer.rs`,
  `src/parser.rs` (~2.6K), `src/typeck.rs` (~4.4K), `src/escape.rs` (~1.4K),
  `src/cgen.rs` (~9.6K), `src/{ast,types,token,span,module,diag}.rs`.

## 2. P2 — the parser (recursive-descent → AST)

**Goal:** a Jestyr program that reads tokens and builds an AST, matching `src/parser.rs`.

- **First: a token stream the parser can consume.** The P1 lexer prints lexemes; the
  parser needs `(kind, start, end)` per token. Options: (a) extend `lexer.jtr` with a
  `dump-kinds` mode that prints `<kind-tag> <start> <end>` per line, and add a matching
  Rust `jestyrc lex-dump --kinds` so the token *stream* (not just lexemes) is cross-checked
  first; or (b) have the Jestyr parser call the lexer in-process (both are Jestyr modules).
  Prefer (b) for the real compiler, but land (a) first as a cheap cross-impl gate on token
  kinds (P1 only proved lexeme boundaries; kinds like Int-vs-Float aren't yet differentially
  tested — a good small increment to add).
- **AST representation.** The Rust AST uses arena vectors (`exprs`, `types`, `pats`) with
  integer ids (`ExprId(u32)`), which ports cleanly to Jestyr `List(T)` + integer indices —
  and dodges recursive-enum ownership questions. Mirror `ast.rs`'s `ExprKind`/`Stmt`/`Item`
  as Jestyr enums; keep the same node set.
- **Cross-impl golden.** Add a Rust `jestyrc parse --dump` canonical S-expression/JSON AST
  printer (there's already `print_ast` for `Mode::Parse`), and have the Jestyr parser emit
  the *same* canonical form. Diff over the corpus. **Watch:** the printer must be a pure
  function of the AST (deterministic field order) so the two agree byte-for-byte.
- **Watch (R1, already observed):** the Rust parser **stack-overflows on ~1000-deep
  expressions** (see the flagged bug / `task_109fb4cb`). A Jestyr recursive-descent parser
  will hit the same wall *sooner or later* on its own stack — decide the depth-guard story
  before the port hardens, and add the guard to *both* implementations so they still agree
  (both reject over-deep input with a diagnostic rather than one crashing).

## 3. P3 — typeck/resolve, then P4 — escape

- **P3 typeck** (`src/typeck.rs`): name resolution + type inference, producing the
  per-expr type table. This is the largest pass after cgen; stage it — build the global
  table first (two-phase, order-independent — `build_table`), then check bodies. The
  symbol tables use the intern+id-indexed-array discipline (B2). **Cross-impl golden:** a
  resolved-type dump (each `ExprId` → its `Ty`) diffed over the corpus. Leniency rules
  (unknown named types → `Opaque`) must match exactly.
- **P4 escape** (`src/escape.rs`): the ownership/borrow-escape checker. Smaller (~1.4K).
  **Cross-impl golden:** the *set of diagnostics* (message + span) must match the Rust
  checker on the corpus — the escape examples (`examples/escapes.jtr`,
  `examples/region_escape.jtr`) already pin expected error counts.

## 4. P5 — cgen (the big one), and the byte-identity lever

- **P5 cgen** (`src/cgen.rs`, ~9.6K): lower the AST+types to C. The acceptance bar is the
  strongest possible and already mechanized: **the Jestyr cgen must emit byte-identical C
  to the Rust cgen** on every corpus program. That is exactly what
  `proptests.rs::compilation_is_deterministic` + the `attest` C-hash already assert *within*
  the Rust impl; the cross-impl version diffs Jestyr-emitted C against Rust-emitted C. If
  the C matches, the downstream gcc build + run behavior is identical for free.
- Port incrementally by construct (the Rust cgen is a big `emit_expr`/`emit_stmt` match);
  after each construct, the subset of the corpus using only ported constructs must match.
- **This is where module content-hashing + `attest` earn their keep** (design already
  anticipated this): use the module content-hash as the cache key and `attest` to pin the
  emitted-C hash, so cross-impl equality is a hash compare, not a full text diff.

## 5. R2 — the fixpoint (the acceptance criterion)

Self-hosting is *proven* by a 3-stage fixpoint (gate behind `--features selfhost-fixpoint`,
outside the toolchain-free default suite):
- **stage-1:** the Rust compiler builds the Jestyr compiler → `jc1`.
- **stage-2:** `jc1` builds the Jestyr compiler from the same source → `jc2`.
- **stage-3:** `jc2` builds itself → `jc3`.
- **Assert `jc2 ≡ jc3` byte-for-byte** (the emitted C, and ideally the final binary).

Stand it up *as soon as a partial stage-1 self-build exists* (even before P5 is complete —
e.g. a compiler that only handles a language subset can fixpoint on a subset corpus), so
regressions are caught from day one. Use the compiler source's module content-hash as the
cache key; `attest` pins the stage-2/stage-3 artifacts. **Teeth:** perturb one byte of a
stage input → the fixpoint diff fails.

## 6. R1 — scaling (partly measured; a known bug to fix first)

The self-host experiment (this session) established:
- **Program *size* scales fine:** 3000 functions / 15K lines compiles without issue.
- **Expression *depth* does NOT:** the Rust parser/typeck/cgen recursive walks
  **stack-overflow between depth 500 (ok) and 1000 (overflow)**. Fix before the port
  hardens (flagged as a background task): a recursion-depth-guard diagnostic in the parser
  (and check typeck/cgen). A 27K-line real compiler source won't nest expressions that
  deep, but a fuzzer or generated corpus can, and the Jestyr port will have the same limit
  on its own stack.
- **Mitigation to build early:** a "large input" corpus (concatenate std+examples into
  ever-bigger single modules) + a `huge_program_compiles` test; profile cgen if compile
  time bites at 27K lines.

## 7. Recommended order

1. **P2a — token-kind cross-check** (cheap): differential-test token *kinds* (Int vs Float,
   keyword vs ident, each operator kind), not just lexeme boundaries. Closes the one gap P1
   left (P1 proved boundaries, not classification).
2. **Fix the deep-expression stack overflow** (task `task_109fb4cb`) in the Rust compiler —
   before the Jestyr port inherits the pattern.
3. **P2 parser** → cross-impl AST-dump golden.
4. **P3 typeck** → resolved-type-dump golden. **P4 escape** → diagnostic-set golden.
5. **P5 cgen** → byte-identical-C golden (per construct, then whole corpus).
6. **R2 fixpoint** → stand up early on a subset; expand as P5 completes.

## 8. Anchors

| Thing | Where |
|---|---|
| Reference lexer / token set | `src/lexer.rs`, `src/token.rs` (`TokenKind`) |
| P1 Jestyr lexer + cross-check | `examples/std/lexer.jtr`; `proptests.rs::c_oracle::{build_exe, rust_lexemes, jestyr_lexemes, jestyr_lexer_matches_reference_on_corpus}` |
| Reference parser / AST | `src/parser.rs`, `src/ast.rs` (`print_ast` for the dump) |
| Reference typeck / types | `src/typeck.rs`, `src/types.rs` (`TypeInfo`) |
| Reference escape | `src/escape.rs` (+ `examples/escapes.jtr`) |
| Reference cgen | `src/cgen.rs` (+ `attest` C-hash, `Modules::hashes`) |
| Symbol strategy (B2) | `examples/std/intern.jtr` (intern + id-indexed arrays) |
| Deep-expr overflow (R1) | flagged task `task_109fb4cb`; repro: `return 1+1+…` depth ≥ 1000 |

## One-line summary

P1 (lexer) is done and cross-checked token-for-token over 122 files. The rest of the port
is **P2 parser → P3 typeck → P4 escape → P5 cgen**, each gated by a cross-implementation
golden on the shared corpus (AST dump → type dump → diagnostic set → byte-identical C),
then the **R2 fixpoint** (`jc2 ≡ jc3`) as the acceptance test — stood up early on a subset.
Fix the known deep-expression stack overflow before the Jestyr port inherits it.
