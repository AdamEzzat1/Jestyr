# Frontend architecture: assessment and roadmap

Companion to [frontend-grammar.md](frontend-grammar.md). That file says what the
parser accepts; this one says what the frontend *is*, what is weak about it, and
in what order to strengthen it — under the constraint that every change must
survive two independent implementations held byte-identical.

The backend is out of scope. C generation works and is not the bottleneck.

## 1. Assessment

The frontend is `lexer.rs` → `parser.rs` → `ast.rs`, consumed by `typeck.rs`,
`escape.rs`, `cgen.rs` and `printer.rs`.

**What is genuinely good, and worth not breaking:**

- **Total, recovering, bounded.** The lexer never stops on a bad character; the
  parser records a diagnostic and continues; every loop has a progress guard;
  `MAX_EXPR_DEPTH` bounds AST *height* so a later recursive pass cannot overflow.
  These are the properties most hand-written parsers lack, and they are already
  covered by `pipeline_is_total` and friends.
- **Spans are byte ranges, everywhere, exactly.** Tokens carry no text; lexemes
  are recovered by slicing. This is what makes lossless tooling *possible later*
  without touching the lexer (see §3).
- **Zero dependencies.** The compiler has no runtime deps at all.
- **The Pratt core is already a table.** `bin_op` is a clean binding-power table;
  the precedence ladder is one function, not scattered conditionals.
- **Two implementations, byte-identical.** The strongest correctness asset in the
  project — and the strongest constraint on changing anything here.

**What is weak:**

1. **The six-file feature tax.** Adding one expression form means touching
   `ast.rs`, `parser.rs`, `typeck.rs`, `escape.rs`, `cgen.rs`, `printer.rs` — and
   then the whole thing again in `examples/std/*.jtr`. Nothing enforces that you
   did all twelve; a missed pass is a silent wrong answer, not a compile error.
2. **Traversal is duplicated per pass.** Each of typeck/escape/cgen/printer walks
   `ExprKind` with its own `match`. Several use `_ =>` arms, so a new variant is
   silently ignored rather than rejected at compile time.
3. **Trivia is discarded.** Comments and whitespace never reach the parser. Good
   for the grammar, fatal for a formatter or an LSP that must round-trip text.
4. **Statement boundaries are structural only.** `f` on one line and `(x)` on the
   next parses as `f(x)`. Documented, but a real trap (§8).
5. **Diagnostics are positionally excellent and lexically generic.** The renderer
   is teaching-quality (caret, source line, `help:`), but the messages are mostly
   `expected X, found Y` with no recovery hint and no "note: to close this" span.
6. **Error paths were untested.** The corpus goldens only ever exercise *valid*
   programs. Invalid-input behaviour was covered only by "does not panic" fuzzing
   — nothing checked that a given malformed input is actually *rejected*, or that
   rejection stays bounded. (Addressed in §2; this is now tested.)

## 2. What this pass implemented

Deliberately small, additive, and behaviour-preserving.

- **`docs/frontend-grammar.md`** — EBNF for the syntax the parser accepts today,
  with an explicit list of where it is approximate.
- **`grammar_conformance` in `src/proptests.rs`** — a table with one snippet per
  documented production, asserted to parse clean; a table of malformed inputs
  asserted to be rejected *and* to stay under a diagnostic-cascade budget; a
  recovery-boundedness check; and a "every prefix of a valid program survives"
  check, which is the cheapest realistic source of malformed input there is
  (it is what a file looks like mid-keystroke, i.e. what an LSP would send).
- **`the_precedence_ladder_is_pinned` in `src/parser.rs`** — the executable copy
  of the grammar's binding-power table: 24 cases covering precedence,
  associativity, assignment, `catch`, ranges, casts, postfix, unary.
- **`a_block_led_statement_is_not_extended_by_a_trailing_operator`** — pins both
  halves of the statement/expression contrast, which nothing covered before.
- **`RANGE_BP` + `infix_bp`** — the range binding power was the literal `5`/`6`
  written in two places (`parse_binary` and `drain_binary_chain`). Now named
  once. Behaviour-preserving; the P2 parser goldens confirm the self-hosted
  parser still agrees byte-for-byte.

## 3. Concrete syntax tree (for formatter / LSP)

> **Stage 1 is implemented** in `src/cst.rs`. `attach` pairs each token with the
> trivia before it, `render` reproduces the source, and `pieces` classifies a
> trivia run into whitespace / line comment / block comment. The round trip is a
> property over arbitrary text, over trivia soup, and over every `.jtr` file in
> the repository (`proptests::cst_props`). No lexer, parser or AST change was
> needed, exactly as predicted below.

**The key fact: no lexer change is needed.** Spans are exact byte ranges and the
token stream is ordered, so the trivia between two tokens is exactly
`src[tok[i].span.end .. tok[i+1].span.start]`. Nothing is lost today — it is
merely *not materialized*. The self-hosted doc generator already exploits this:
`tokens.collect_docs` finds comments by scanning the gaps between tokens, which
is why doc comments could be added without perturbing the pinned token stream.

**Proposed shape** (`src/cst.rs`, new, coexisting with the AST):

```rust
struct Trivia { span: Span }          // whitespace and/or comments, verbatim
struct CstToken { trivia: Trivia,     // everything before this token
                  token: Token }
fn tokens_with_trivia(src, &[Token]) -> Vec<CstToken>
fn render(src, &[CstToken]) -> String
```

**The acceptance test is a round-trip**: `render(tokens_with_trivia(src)) == src`,
as a proptest over arbitrary text. That single property is what "lossless" means,
and it is cheap to state and impossible to fake.

Staging:

- **Stage 1** — the token-level CST above, plus the round-trip property. Buys a
  formatter that can reprint unchanged regions verbatim. No parser change, no
  AST change, no risk to the goldens.
- **Stage 2** — attach node spans: every AST node already has a `Span`, so a
  syntax-node → token-range map is derivable without a second parse. Enough for
  LSP hover, go-to-definition, and semantic tokens.
- **Stage 3** — only if a formatter demands it: a real green/red tree
  (rowan-style) built alongside the AST. This is the expensive one because it
  must be mirrored in the port; defer until Stages 1–2 prove insufficient.

Do **not** replace the AST with a CST. The AST is what two implementations agree
on byte-for-byte; a CST is a tooling artifact and should stay one.

## 4. Diagnostics

Current state: rendering is strong, *content* is generic. `expect()` produces
`expected {what}, found {kind}` with no hint about the construct being parsed and
no secondary span.

> **Item 2 is implemented.** `Parser::expect_close` keeps the message identical
> (so anything matching on it still works) and attaches a `help:` line naming the
> opener's line and column, resolved through a lazily-built `LineIndex` so a file
> full of unclosed delimiters stays linear. Wired into block, `trait` and `match`
> bodies; the remaining ~28 `expect(RBrace/RParen/RBracket)` sites are the same
> mechanical change and can be converted as they are touched.

Low-risk improvements, in order:

1. **Context in the message.** `expect` takes a `what: &str` already; thread an
   enclosing-construct label so it reads "expected `}` to close this struct body"
   rather than "expected `}`, found `fn`".
2. **Opening-delimiter secondary span.** `Diagnostic` supports `help`; an
   unclosed `{`/`(`/`[` should point at the opener. Highest value per line of
   code — unclosed delimiters are the most common real syntax error.
3. **Synchronization points.** `parse_module` recovers by bumping one token.
   Recovering to the next *item keyword* (`fn`/`struct`/`enum`/…) would cut
   cascades further. The cascade budget test in §2 is the guardrail for this.
4. **Error codes.** `Diagnostic::with_code` exists and is unused. Assigning
   stable codes to the top ~20 parse errors makes them linkable and testable by
   identity rather than by message substring.

Caution: diagnostic *text* is not part of the two-sided byte-identity contract
(the port renders its own messages), so this work does not owe a port mirror —
but `jestyrc check --json` output shape is consumed by tooling, so codes should
be added additively.

## 5. HIR

**Jestyr already has a HIR — it is just spelled as side tables.** `TypeInfo`
carries `expr_types`, `method_calls`, `qualified`, `call_sym`, `impl_calls`,
`bound_method_calls`, `dyn_coercions`, `dyn_calls`, `err_payloads`. `cgen` reads
all of them, keyed by `ExprId`. That *is* a resolved layer; it is simply
scattered across nine `HashMap`s instead of living in one tree.

That reframes the work: not "introduce a HIR" but "collect the existing one".

Staged, each stage gated on byte-identical C over the 148-file corpus:

- **Stage 0 (documentation only).** Write down that `TypeInfo`'s side tables are
  the de-facto HIR and that `ExprId` is its node identity. Zero risk; makes the
  next stages legible.
- **Stage 1.** Introduce `hir::Resolved`, a *single* struct per `ExprId` folding
  the nine maps into one lookup. Purely a data-structure change behind the same
  accessors; no pass changes shape. Byte-identity is trivially preserved.
- **Stage 2.** Move *desugaring* into HIR construction — the rewrites all three
  back passes currently repeat or assume: `while`/`loop` → `for` (already done in
  the parser), block-led vs expression forms, compound assignment, `?`
  propagation. One construct per commit, each proven by the corpus.
- **Stage 3.** Point `escape.rs` at HIR. It is the smallest consumer and the one
  whose invariants are clearest.
- **Stage 4.** Point `cgen.rs` at HIR, one node kind at a time.

**What stays in the AST, permanently:** anything the printer, the doc generator,
`attest`'s signature reconstruction, or the grammar goldens need — those are
*syntactic* consumers and must keep seeing syntax.

The port tax is the real cost: each stage that changes emitted C must be mirrored
in `examples/std/cgen.jtr` and re-seeded. Stages 0–1 change no output and so cost
nothing on that axis; that is exactly why they come first.

## 6. Traversal helpers

> **Implemented, but narrower than first proposed** — and the investigation is the
> interesting part. `errsets` and `simd` do *not* have this problem: they scan the
> expression arena flat (`for e in ast.exprs.iter()`), which is complete by
> construction and so immune to a new variant without any helper. The pass that
> genuinely needed it is `provenance` (`jestyrc unsafe`), which must recurse
> because it tracks *lexical* `unsafe` scope, and whose `_ => {}` arm would have
> silently hidden raw-pointer operations nested in any newly-added expression
> form — an under-reporting safety report.
>
> `src/visit.rs` therefore ships `child_exprs`, an exhaustive structural match with
> no `_` arm, rather than a `Visitor` trait: it answers only "what are this node's
> sub-expressions", leaving each pass its own context. A reachability test asserts
> that walking from every item body reaches every expression the parser built, so a
> forgotten variant fails loudly. Adopting it in `provenance` changed the `unsafe`
> report on **zero** of 155 corpus files — no latent bug was hiding, but the hole
> can no longer open.
>
> The original proposal below is kept for the trait-shaped alternative and the
> reasoning about which passes should *not* be retrofitted.

Proposal (`src/visit.rs`, additive):

```rust
trait Visit<'a> { fn expr(&mut self, ast: &'a Ast, id: ExprId) { walk_expr(self, ast, id) } … }
fn walk_expr<V: Visit>(v: &mut V, ast: &Ast, id: ExprId)   // exhaustive match, NO `_` arm
```

The value is the missing `_` arm: adding an `ExprKind` variant becomes a compile
error in exactly one place, instead of a silent no-op in four.

Adopt where passes are **pure collectors** and the win is unambiguous:
`simd::sites_in_span`, `obligations::collect`, `provenance::collect`,
`errsets::collect`. Do **not** retrofit `cgen` or `typeck` onto a generic walker —
they thread per-node context (`subst`, scopes, `cur_*`) that a generic visitor
would have to model, and the abstraction would cost more than the duplication.

## 7. Prioritized roadmap

| # | Item | Risk | Port mirror? | Why this order |
|---|---|---|---|---|
| P0 | Grammar doc + conformance tables | none | no | done — makes everything below reviewable |
| P0 | Invalid-syntax + cascade-budget tests | none | no | done — error paths were untested |
| P1 | Unclosed-delimiter secondary spans | low | no | **done** — `expect_close`, opener named in `help:` |
| P1 | CST Stage 1 + round-trip property | low | no | **done** — `src/cst.rs`, lossless over the whole corpus |
| P2 | `visit.rs` + adopt in the 4 collector passes | low | no | kills the silent-`_`-arm class of bug |
| P2 | HIR Stage 0 (name the de-facto HIR) | none | no | **done** — see `TypeInfo`'s doc comment |
| P2 | HIR Stage 1 (fold side tables) | low | no | no output change, so no seed churn |
| P2 | Item-keyword synchronization in recovery | medium | no | needs the cascade budget as a guard |
| P3 | Newline rule (§8) | medium | **yes** | measured-safe, but changes the language |
| P3 | HIR Stage 2–4 | high | **yes** | the real payoff, and the real cost |

## 8. Newline and statement boundaries

The problem, restated: the lexer discards newlines, so

```
f
(x)
```

is the call `f(x)`. The same applies to a line beginning `[`, `-`, `&`, `*`, `.`
or `?` after an expression statement.

**Options.**

- **(a) Keep and document.** Zero risk, zero cost. The trap stays.
- **(b) Newlines significant in limited contexts.** A newline ends a statement
  unless the line is obviously incomplete (trailing operator, open delimiter).
  Powerful, but "obviously incomplete" is a second grammar to specify and mirror.
- **(c) Require terminators.** Semicolons or an explicit statement separator.
  Large, breaking, and against the language's stated feel.
- **(d) Lexer records newline positions; the parser consults them at one decision
  point.** No new token kind — the lexer already knows where newlines are, and
  §3 shows the gaps are recoverable from spans alone. The rule: **in statement
  position, a postfix continuation (`(`, `[`, `.`, `?`) does not cross a
  newline.** This is the restricted rule Go and Swift use variants of.

**Measurement.** I scanned all 175 `.jtr` files in `examples/`:

| line begins with | occurrences |
|---|---|
| `(` or `[` | **0** |
| `-` | **0** |
| `&` | **0** |
| `*` | **0** |
| `.` | **0** |
| `?` | **0** |

Adopting (d) would change the parse of **zero** files in the corpus, including
the compiler's own ~30,000 lines of Jestyr. The compatibility risk is measured,
not assumed.

**Recommendation: (d), scheduled at P3, not taken opportunistically.**

Rationale. It removes a real trap; the evidence says it breaks nothing that
exists; and it is the only option that makes the language *more* tooling-friendly
rather than less (a formatter and an LSP both benefit from statement boundaries
being decidable without semantic context). It is P3 rather than P1 because it is
the one item on this list that changes what programs mean, and therefore owes a
port mirror in `examples/std/parser.jtr`, a corpus re-verification, and a seed
refresh. Do it as a deliberate, isolated change with the conformance tables from
§2 already in place — not bundled with anything else.

The counter-argument, honestly: option (a) is defensible. The trap has not
actually bitten anyone in 30,000 lines of self-hosted Jestyr, which is some
evidence that idiomatic code never writes those lines. If the port tax is the
binding constraint, documenting it (already done, in the grammar) is a legitimate
place to stop.

## 9. Fuzzing and invalid input

Already present and good: `lexer_is_total`, `pipeline_is_total`,
`pipeline_is_total_on_ascii_soup`, `deep_expressions_error_not_crash`, plus
`bolero` targets (`fuzz_pipeline`, `fuzz_comptime_eval`) that run as unit tests
by default and as real campaigns under `cargo bolero`.

Added here: rejection is now *asserted* (not merely survived), cascades are
budgeted, recovery is bounded, and every prefix of every conformance snippet is
exercised.

Still worth adding, in rough value order:

1. **A grammar-directed generator.** Generate token sequences from the grammar in
   §1 of `frontend-grammar.md` and assert they parse; mutate them one token at a
   time and assert the result is rejected-or-parsed but never hangs. Higher
   signal than random ASCII, which mostly exercises the lexer.
2. **Differential fuzzing against the port.** The strongest available oracle:
   feed the same random input to `jestyrc` and to the self-hosted parser and
   require identical diagnostics-or-tree. The P2 goldens already do this for the
   *corpus*; doing it for generated input would be the highest-value fuzz target
   in the project.
3. **A diagnostic-count bound as a property**, not just a table: for any input,
   error count ≤ some linear function of token count.
