# Handoff: unfinished frontend work, and the safety-mosaic workstream

Two things in one document, because they share a budget and a constraint.

**Part 1** is what the frontend/lowering sessions left unfinished, with honest
reasons and sizes. **Part 2** is the next major workstream — upgrading the safety
mosaic so complex aliasing has somewhere to go besides "rejected" or "drop to
raw". **Part 3** is the standing constraint both live under, which this session
finally has a *measured* example of.

Status lines here are true as of master `04d8153`. Trust the code over this file
if they disagree.

---

## Part 1 — Unfinished, in priority order

### 1.1 HIR Stage 1 — fold the resolution tables — **DONE** (master `b791d01`)

`TypeInfo` carried eight per-expression channels: `expr_types` plus seven
`HashMap<ExprId, …>` tables. Stage 0 documented that these *are* Jestyr's HIR,
stored column-wise; Stage 1 transposed them to one `Resolved` row per `ExprId`,
behind seven point-lookup accessors and seven `record_*` writers.

Two things this handoff got wrong, worth carrying forward:

- **The size estimate.** "Touches every read site across `cgen.rs`'s ~12,800
  lines" — it was ~30 non-test read sites in total, all `get`/`contains_key`.
  A `grep` before budgeting would have shown that.
- **The storage.** A `HashMap<ExprId, Resolved>` — the fold this document
  proposed — is measurably *slower* than the seven maps it replaces, because
  sparse columns are cheap to **miss**: `HashMap::get` short-circuits on an empty
  table, so in a single-module program `qualified`, `impl_calls` and `dyn_calls`
  cost `escape` and `cgen` almost nothing per call. Folding them into one
  populated map turns every free miss into a real hash-and-probe (interleaved
  `selfbench`, `lex`/`parse` as controls: **escape +20%, 3/3 rounds; total
  +2.9%**). The shipped storage is a dense `Vec<Option<Box<Resolved>>>`, at
  parity with master within a ±5% noise floor. **The key is already a dense
  index — do not put it in a map.** This applies to Stages 2–4 as well.

Byte-identity held for the reason predicted: every pass read these as point
lookups and none iterated. That is now *enforced* rather than reviewed — the two
whole-program iterators are `#[cfg(test)]`, so an emitting pass that grew a
dependency on iteration order fails to compile.

Method note: the first A/B showed a 12% total regression that was entirely
spurious — `lex` and `parse` moved ~10% too, and neither can be affected by this
change. Comparing two separately-built processes measures the machine. Interleave
runs against two saved binaries and keep `lex`/`parse` as controls; if the
controls disagree, that gap *is* your noise floor.

### 1.2 The newline / statement-boundary rule — **DONE**, both toolchains

`f` on one line and `(x)` on the next no longer parses as `f(x)`. Option (d)
from `docs/frontend-roadmap.md` §8; the rule and its two failure modes are now
specified in `docs/frontend-grammar.md` under *Statement boundaries*.

The measurement held on re-verification: **zero** lines begin with `(`, `[`,
`-`, `&`, `*`, `.` or `?` across all 176 `.jtr` files, and parse trees are
byte-identical over all 155 corpus files.

Three things the plan did not anticipate:

- **No "statement position" flag was needed.** The rule as specified ("in
  statement position") implies threading a flag through the expression parser
  and clearing it inside every delimiter. Unnecessary: the test can only fire
  where a line *begins* with a postfix token, and anywhere else the previous
  token is on the same line. One guard at the single point `parse_postfix`
  consumes a postfix token is the entire specification, on both sides — which is
  what kept the mirror cheap, and is exactly the cost option (b) was rejected
  for.
- **The `.` half fails better than "chaining breaks".** `(` and `[` can begin an
  expression, so those become two well-formed statements. `.`, `.*` and `?`
  cannot, so a leading-dot chain is a *syntax error at that token* — not a
  different program. Net: nothing in the grammar silently means something else
  because of a line break.
- **The port's parser cannot hold `src`** (the escape checker refuses the
  stored borrow), so it precomputes a per-token newline mask in the driver,
  mirroring the existing `parw` mask for the `par` contextual keyword. Worth
  knowing before designing any other rule that wants to consult source text.

**And the testing trap, for the second time this session:** the corpus is silent
on this rule *by construction* — that silence is what made adoption safe — so
the whole-corpus P2/P3/cgen goldens cannot distinguish a port that implements it
from one that does not. Probes live in the P2 golden's curated snippet list
instead. Same shape as §2.5b. Treat "the corpus does not exercise it" as a
signal that a differential test is *required*, not as evidence of safety.

### 1.3 HIR Stages 2–4 (the real payoff, and the real cost)

Stage 2 moves desugaring into HIR construction; Stage 3 points `escape` at HIR;
Stage 4 points `cgen` at it, one node kind at a time. Each stage that changes
emitted C owes the full two-sided tax (Part 3). Do not start these before 1.1.

### 1.4 Diagnostics remainder (low risk, no mirror owed)

`Parser::expect_close` keeps the message identical and adds a `help:` line naming
the opener. Wired into block, `trait`, `match`, **struct, enum and `impl` bodies,
error sets, and all three function-signature parameter lists**.

- **22** `expect(RBrace/RParen/RBracket)` sites remain, down from ~28. Still
  deliberately **not** swept: each conversion needs the opener's span in scope at
  the matching close, so it means reading the pairing at every site rather than
  matching a pattern. Convert them as they are touched.
- The converted set was chosen by where the opener is *furthest* from the
  detection point — an unclosed item body puts the mistake a whole declaration
  away, which is where `expected `}`, found `<eof>`` is least useful.
- Still unstarted from `docs/frontend-roadmap.md` §4: construct context in the
  message ("expected `}` to close this struct body"); item-keyword
  synchronization in `parse_module`'s recovery (the cascade-budget test is the
  guardrail); stable error codes — `Diagnostic::with_code` exists and is **unused**.
- Diagnostic *text* is not part of the byte-identity contract, so none of this
  owes a port mirror. `check --json` shape is consumed by tooling, so add codes
  additively.

### 1.5 CST Stages 2–3

Stage 1 shipped (`src/cst.rs`): `attach` / `render` / `pieces`, lossless over
arbitrary text, trivia soup, and every `.jtr` file in the repo.

- **Stage 2** — map syntax nodes to token ranges. Every AST node already carries a
  `Span`, so this is derivable without a second parse. Enough for LSP hover,
  go-to-definition, semantic tokens. `CstToken::full_span` exists for this and is
  `#[allow(dead_code)]` until then.
- **Stage 3** — a real green/red tree. Expensive *because it must be mirrored*.
  Defer until Stages 1–2 prove insufficient.

### 1.6 Fuzzing and conformance gaps

Present: `grammar_conformance` (production table, malformed-input table with a
cascade budget, recovery boundedness, every-prefix survival), plus the older
`pipeline_is_total` / `lexer_is_total` / bolero targets.

All three items — **DONE** (`jestyr_parser_matches_reference_on_generated_input`,
a grammar-directed generator with single-token mutation, and
`diagnostic_count_is_linear_in_input_size`).

The estimate "highest-value fuzz target in the project" was right, and it paid
within a few hundred cases: **seven real divergences**, none visible to any
existing gate, because every corpus file and every curated snippet is well-formed
and all seven are *recovery* paths. Worst was an **out-of-bounds read** — the
port's `cur_tok` indexed its token list unguarded, so input ending mid-construct
(`not 1.5.`) produced spans from whatever memory followed the arena. Fixed at the
root by making `cur_tok` total, which is the invariant the reference gets free
from always appending `Eof`.

The rest were one systematic mismatch and its relatives: the reference reads
`self.cur().span` *before* `expect(closer)` (all 17 of its sites do), while the
port fell back to `prev_end` — which agrees exactly when the closer is present,
i.e. everywhere the corpus looks. Plus `eat_ident`'s failure semantics, which
report the offending token's span and consume nothing; one of those was a **tree**
difference rather than a span.

**The item-level twin is DONE too**
(`jestyr_parser_item_matches_reference_on_generated_input`): generated
declarations — fns, structs, enums, traits, impls, externs, consts, attributes —
plus single-token mutations, 2000 cases clean. It found **six more port bugs**,
raising the session total to thirteen. Recurring shapes worth knowing when the
next one appears: (a) *commit-before-check* — the port branched to "a method"
on `@`-led input without verifying `fn` follows, in both struct and impl
bodies; (b) *the progress bump runs before the comma check* — the reference's
recovery bump consumes the offending token so the separator test needs a comma
of its own, and the port's direct comma check instead let the same `,` continue
the list (enum variants, fn generics); (c) two more missing-`eat_ident` span
sites shaped unlike the fifteen a blanket regex had aligned; (d) missing attr
*names*, where the reference fabricates the ident `<error>` and the dump
compares name **text** — the port marks these with an empty span (an ident is
never empty) and prints `<error>` for it. One deliberate structural difference
is asserted rather than skipped: no-item on the reference side must equal
exactly one `itemerr` (kind 99) on the port side, since `jc`'s parse-refusal
scan depends on that node existing.

Three things worth carrying into the next fuzz increment:

- **Token-granular mutation, not byte-granular.** Byte mutation mostly yields
  lexer errors, which both sides trivially agree on. Token mutation yields input
  that *lexes*, which is what puts the two parsers' recovery against each other.
- **Fixed seed, fixed budget, no `proptest`.** Each case costs a process spawn,
  and a gate that shrinks but does not reproduce is worse than one that
  reproduces: a CI divergence has to replay locally. The failure carries seed and
  case number; `FUZZ_CASES` raises the budget.
- **The generator is expression-level, so this is not full coverage.** Twelve more
  sites share the "default instead of the offending token's span" shape at *item*
  level (fn params, struct fields, attributes). The reference uses `eat_ident` at
  each, but nothing proves the port matches and the valid-only corpus cannot. **An
  item-level generator over `jestyr_parser_item_dump` is the next increment**, and
  is where the next batch will be.

### 1.7 `par for` fusion follow-ups

Fusion landed on both toolchains. Eligibility is deliberately narrow: top-level
non-generic function, non-`@simd` site, integer element, and a body mentioning no
name but the loop variable.

- **Widening it needs capture analysis** — hoisting a body that reads an enclosing
  local into a worker requires passing that environment. Separate piece of work.
- The `@simd` path keeps its own lowering; a fused *and* vectorized site is
  unexplored.

### 1.8 Recorded so it is not re-attempted

- **The string interner is measured dead.** Two structurally identical 4,000-item
  programs differing only in identifier length (375 KB vs 1.9 MB source, names
  ~10× longer) check in **269 ms vs 257 ms** — no difference. typeck's cost is
  structural (tree walk, scope push/pop, map operation *count*), not key size.
  Interning `Ident.name` and the `Vec<HashMap<String, Ty>>` scopes would not repay
  a refactor touching every file. Footprint is a non-issue too: peak 4.3 MB.
- **`errsets` and `simd` do not need a visitor.** They scan the expression arena
  flat, which is complete by construction and immune to a new variant. Only
  `provenance` needed `visit::child_exprs`, because it tracks *lexical* `unsafe`
  scope. Adopting it changed the `unsafe` report on **zero of 155** corpus files —
  a safety net, not a fix.

### 1.9 Pre-existing open items (verify before acting)

- **CLOSED: the port's `#line` gap.** The prerequisite module-path C golden
  (`jestyr_driver_module_c_matches_reference`) was built first — and promptly
  found a *second*, unrecorded divergence: the port renamed cross-module
  collisions by module *name* (`mag__util`) where the reference canons by module
  *id* (`mag__m1`); the reference numbers modules **pre-order at first visit**
  while the port merges deps-first, so the port now records the pre-order index
  and renames `__m<id>`. Then the port grew `#line` emission itself
  (`cg_mark_line` in `cgen.jtr`, mirroring `Cgen::mark_line`: five emission
  points, per-function dedup reset, `\`→`/` path normalization, a newline
  *binary-search* index — not a scan from byte 0), gated on the driver's debug
  table so every non-`jc build` path stays byte-identical by construction. The
  golden is now **full byte equality** over a fixture that exercises every
  emission point (requires/ensures, same-line dedup, tail returns, monomorphized
  generic instances): 19 directives over 177 lines, byte-identical. The enriched
  fixture caught one real gap on the way — the port's *generic-instance* emitter
  is a separate path from plain fns and needed its own entry mark. The
  `jestyrc attest` vs `jc attest` `c-sha256` disagreement is closed with it.
- **Generic-STRUCT cross-module collisions** remain open (`DESIGN-STATUS.md`,
  modules row). Generic *enum* collisions are done.

---

## Part 2 — The safety-mosaic workstream

### 2.1 What Jestyr enforces today

- **Owned values**: move by default, RAII `Drop`, recursive field/payload auto-drop.
- **Second-class borrows** (`read`/`mut`/`out`): may flow *down* the stack, never
  escape the frame. This is the thesis — provably frame-bounded, so no lifetime
  annotations. `escape.rs` is 1,944 lines and enforces exactly this.
- **Borrowed returns** exist (`f.ret_conv` is `Read`/`Mut`/`Out`, `escape.rs:335`)
  but the *source* relationship is coarse: the checker knows the return is a
  borrow, not which input it projects from.
- **Regions**: lexical arenas; escapes are compile errors.
- **Genrefs**: stored references with runtime generation checks; use-after-free is
  a deterministic fault.
- **Raw pointers**: fenced by an enforced `unsafe` ladder.
- **Checked attributes**: `@no_alloc`, `@no_panic`, `@deterministic`, `@span`,
  `@simd`.

### 2.2 Where it is elegant

Ordinary code pays nothing. A function taking `read xs: []i32` needs no
annotation, and the frame-bound rule is one sentence. The tiers are *named* and
opt-in rather than a single lattice everyone must learn. `@span` putting parallel
depth in the signature — so serializing a parallel reduction is a compile error,
not a dashboard regression six weeks later — is the same idea applied to cost.

### 2.3 Where complex safe code is still awkward

Complex aliasing collapses into one of three unsatisfying outcomes: **rejected**,
**re-expressed with region/genref/index handles**, or **dropped into `unsafe`**.
There is no middle. Doubly linked lists, parent pointers, observer lists, B-trees
and in-place graph mutation all land in that gap.

The goal is to fill it with small, *named* capabilities — not with general
lifetimes.

### 2.4 Prioritized roadmap

Ranked by (value ÷ risk × size), with the self-hosting cost made explicit, because
that is what actually gates each one.

| # | Mechanism | Value | Risk | Size | Port mirror? | Verdict |
|---|---|---|---|---|---|---|
| 1 | `Unknown` safety finalization | high | **low** | small | yes — see §2.5a | **DONE** (`a11b35e`, gate below) |
| 2 | Borrowed projections (`-> read T from xs`) | high | medium | large | **yes** (syntax) | design doc first |
| 3 | Checked genref scopes (`with alive p as read node`) | high | medium | medium | **yes** (syntax) | design, then judge |
| 4 | Disjoint borrowing (`split_mut`) | high | medium | medium | maybe (library-first) | try library-only |
| 5 | Branded region tokens | high | high | large | **yes** | design only |
| 6 | Safe mutable graph cells (`Cell[r, T]`) | very high | high | large | **yes** | design + examples only |
| 7 | Linear capabilities (`linear File`) | high | high | large | **yes** | staged design only |
| 8 | Reference capabilities for concurrency | medium | high | large | **yes** | design only; resist the lattice |
| 9 | Formal mini-model | medium | none | medium | no | write alongside 2–3 |

**Start with #1.** It strengthens soundness, changes no surface syntax, owes no
port mirror, and is the only item on the list that is unambiguously safe to land
in one sitting. *(Done — §2.5a/§2.5b.)*

> **The per-item designs now live in [`docs/safety-mosaic-next.md`](../safety-mosaic-next.md).**
> That file is the one to read before touching items 2–9: it grounds each in
> what the implementation actually does today, gives the minimal mechanism, and
> states the design questions that must be answered *before* syntax is chosen.
>
> Two findings from writing it, which change this table's reading:
>
> - **Item 4's design is forced, not chosen.** A splitting function cannot
>   *return* its two halves — a pair holding two borrows is escape route 2,
>   capture — so the safe interface must be continuation-passing
>   (`split_mut(xs, at, f)`). That is the language's own rule selecting the
>   shape, and it is why item 4 is the next implementable item.
> - **Item 2 may have no consumer yet.** `-> read T from xs` adds precision that
>   nothing in the compiler currently reads. If no caller benefits until items
>   4/6 exist, it is a signature change — with attest-hash and doc-rendering
>   cost — for no present payoff. Answer that before building it.

### 2.5 Item 1, grounded — the `Unknown` finalization pass

The premise checks out, but it is **narrower than "reject `Unknown` everywhere"**,
and getting that wrong would produce exactly the diagnostic cascades the current
leniency exists to prevent.

What is actually true in the code today (`src/types.rs`):

```rust
Ty::Opaque(_) => false,  // generic/external: assume non-Copy — conservative, correct
Ty::Unknown   => true,   // lenient: suppress escapes we couldn't type
Ty::Error     => true,   // suppress cascades
```

So `Unknown` is `Copy`, which means the escape checker will *not* flag a move of
it. That is the hole.

But it is not uniformly lenient. `escape.rs`'s `carries_arena_ref` **already
includes `Unknown`** deliberately ("the region intrinsics return it"), so the
region-escape path is conservative today. Any finalization pass must not
double-count that or it will change existing diagnostics.

Suggested shape:

- Keep `Unknown` lenient **for diagnostics** — that is what stops cascades, and it
  is load-bearing.
- Add a **finalization gate**: after checking, if `Unknown` appears in an
  ownership- or safety-relevant position, fail the *build* rather than silently
  emitting. Focus on return / capture / store / take paths and raw / region /
  genref operations.
- Implement it as a separate pass over `TypeInfo`, not by flipping `is_copy` —
  flipping `is_copy` would change escape diagnostics corpus-wide and is the
  cascade risk.
- Watch: `escape.rs` mentions `Ty::Unknown` **once**; `typeck.rs` 57 times; `cgen.rs`
  18 times. The typeck occurrences are mostly inference plumbing, not policy.

**Acceptance**: no corpus file changes its diagnostics; a new test file with a
deliberately-unresolvable type in a return/store position is rejected with one
clear message.

#### 2.5a What the census actually found (master `a11b35e`)

The bullets above are still the right destination, but the framing —
"ownership-relevant positions: return / capture / store / take, raw / region /
genref" — is **too broad to implement against**. `Ty::Unknown` is *produced* in
~50 places in `typeck.rs`, and most are deliberate quiet fallbacks: `null`, a
bare fn name used as a value (`&make`), `Attr`, `UnOp::Ref`, the region
intrinsics. A gate phrased over positions rejects those and breaks ordinary code.

**Ask instead where `Unknown` changes an answer.** Only **two** sites in all of
`escape.rs` consult copy-ness — `escapes_as` and `captured_borrow_name` — so the
hole is exactly: *an expression that is already a borrow place (or a captured
borrow), whose type we failed to infer.* That is a one-line predicate, not a
taxonomy of positions.

Instrumenting those two predicates and sweeping the corpus is cheap (~20 lines,
temporary) and is the step to repeat before touching anything here. The first
sweep found **2 sites in 1 file** — and both were benign only by luck, so a
"corpus is clean, ship the gate" reading would have been wrong:

> `examples/struct_variant.jtr`, `rect { w, .. } => w`. The binding *should* be
> `f64`; it was `Unknown`. `f64` is `Copy`, so the leniency returned the right
> verdict for the wrong reason. Substituting a non-`Copy` field reproduced a real
> missed escape immediately.

Root cause, now fixed on both toolchains: **struct-variant patterns bound every
field to `Unknown`** because `GlobalTable` stores a variant's field types
positionally and drops the names. `one { n, k } => n` let a borrowed non-`Copy`
field escape a `read` parameter, while the positional `one(n, k) => n` and the
plain projection `h.inner` both rejected it. Fixed via `variant_field_names`
(reference) and payload `(name_start, name_end, ty)` triples (port).

**Cost note that contradicts §2.4's table.** That row says item #1 owes no port
mirror because it is "diagnostics only". True of the *gate*; false of anything
that changes an inferred type. Emitted C and all 155 files' diagnostics were
unchanged — `cgen` resolves variant fields itself via `variant_field_by_name` and
never read these bindings — **yet the mirror was still owed**, because the P3
golden compares `Ty::display` for every expression against the self-hosted typeck
with an empty denylist. *Zero C change does not imply zero mirror owed.* Check
the P3 golden, not just `emit-c`, whenever a type becomes more precise.

#### 2.5b The gate itself — **DONE**, and item 1 is closed

The census reaching **0 over 155 files** is what unblocked it. Shipped as a
refusal at the two copy-ness consumers (`escapes_as`, `captured_borrow_name`),
deduped by `ExprId` and emitted sorted; mirrored in `examples/std/escape.jtr`
with `typeck.ty_is_unknown`. No corpus diagnostic moved — the acceptance
criterion — because the root cause was fixed first.

Three things worth carrying forward:

- **It was not only a backstop.** Two ill-formed shapes reach it today and used
  to compile *clean*, straight through to code generation:
  `fn f[T](read x: T) -> i32 { return x.v }` (field of an unbounded `T`) and
  `fn h(read p: N) -> i32 { return p.v.w }` (`.w` on an `i32`). Neither has a
  type, so neither had an escape verdict. **Open follow-up:** typeck should
  reject both at the field access with a better message, after which the gate
  stops firing for them and becomes the pure backstop it was meant to be.
- **Order the two sides by a *total* key.** Both sort by `(span start, ExprId)`,
  not by span with insertion order breaking ties. Expression ids correspond
  exactly across toolchains (that is what P2/P3 compare), so neither side has to
  reproduce the other's traversal. The unsafe rung's span-only sort works today
  only because its ties happen not to arise; do not copy that part.
- **A whole-corpus golden can be structurally blind to a new rule.** The P4
  escape golden compares all 155 files, and would have passed with the port
  missing this rung *entirely* — the census is zero over the corpus by design, so
  both sides agree everywhere the golden looks and disagree everywhere else.
  `jestyr_escape_finalization_matches_reference` covers it with probes that do
  trigger, and asserts they still trigger so a later inference fix cannot render
  it silently vacuous. **Whenever a rule is deliberately silent on the corpus,
  the corpus goldens cannot mirror-check it — write the differential test.**

### 2.6 Design constraints (non-negotiable)

- No Rust-style lifetime syntax. Ordinary borrows stay second-class.
- Do not weaken the escape checker.
- Do not make raw pointers *look* checked.
- Do not let `Unknown` pass through safety-sensitive code silently.
- No large mechanism without tests **and** documentation.
- Do not break the byte-identical reference-vs-self-hosted gates.
- If a feature needs mirroring in `examples/std/*.jtr`, either implement the
  mirror or keep the feature design-only/disabled. **Do not land it half-mirrored.**

### 2.7 Why this is not lifetimes under another name

Worth stating in the eventual `docs/safety-mosaic-next.md`, because it is the
first objection any reviewer will raise: Rust's lifetimes are *inferred, pervasive
and structural* — every reference carries one whether or not the programmer writes
it. The proposals here are **named, local, and opt-in**: a projection names its
source input; a genref scope is a lexical block; a region token is a value you
pass. Ordinary code never mentions any of them. The test is simple — if a
mechanism starts appearing in signatures that do not need it, it has become a
lifetime and should be redesigned.

---

## Part 3 — The standing constraint, with a measured example

Every change that alters emitted C owes the **two-sided tax**: a mirror in
`examples/std/cgen.jtr` (or `parser.jtr`/`typeck.jtr`), re-blessed corpus goldens,
`REFRESH_SEED=1`, and the full gate.

This session produced a worked example — the `par for` fusion — so the cost is no
longer a guess:

- Reference-side implementation: prepass + eligibility rule + worker emission.
- Port mirror: ~200 lines of `.jtr`, and it did **not** work first time. The
  failure was a silently-unmatched string edit that dropped the `#include
  <pthread.h>` gate, which surfaced as a corpus divergence rather than a compile
  error.
- Two things had to be designed *for agreement*, not just for correctness:
  1. **Worker indices assigned in ascending expression id on both sides.** The
     arena is in source order, so each implementation reaches that order by
     walking expressions ascending — rather than two traversal orders happening to
     line up.
  2. **The port stores each site's lowered body at collection time** rather than
     re-emitting it in the worker section, because re-emitting consumes temporary
     numbers a second time and shifts every later `_pf<n>` in the file. For the
     same reason the fused call site still emits iter → reduction → body in the
     reference's order, discarding the body.
- A divergence risk had to be *removed* rather than mirrored: the reference was
  separately rejecting non-integer elements while the port normalized them. Both
  now use the same normalization — one rule, not two.

**The lesson for Part 2**: any syntax-bearing mechanism (items 2, 3, 5–8) costs
roughly *twice* its reference implementation, plus a reseed, plus a gate run
measured in minutes. Design docs are cheap; syntax is not. Rank accordingly.

### Useful commands

```bash
cargo test --release
```

```bash
cargo test --release --features c-oracle
```

```bash
cargo test --release --features c-oracle,selfhost-fixpoint
```

```bash
REFRESH_SEED=1 cargo test --release --features c-oracle,selfhost-fixpoint bootstrap_seed_is_current
```

`DUMP_DIVERGE=1` on the c-oracle run prints the first differing line when the port
and the reference disagree — that is how the fusion mirror was debugged. The
c-oracle and fixpoint gates need `gcc` on `PATH`; without it they cannot run and
that should be stated rather than silently skipped.
