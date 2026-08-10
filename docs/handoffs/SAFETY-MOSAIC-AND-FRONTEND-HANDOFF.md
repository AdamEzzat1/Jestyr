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

### 1.1 HIR Stage 1 — fold the resolution tables (next; cheap on a clean start)

`TypeInfo` carries eight per-expression channels: `expr_types` plus seven
`HashMap<ExprId, …>` tables (`call_sym`, `method_calls`, `qualified`,
`impl_calls`, `bound_method_calls`, `dyn_coercions`, `dyn_calls`). Stage 0 (done)
documented that these *are* Jestyr's HIR, stored column-wise. Stage 1 folds them
into a single `HashMap<ExprId, Resolved>` behind the same accessors.

- **Changes no emitted C**, so it costs nothing in corpus goldens, attest hashes
  or bootstrap-seed churn, and owes **no port mirror**.
- Mechanical, but touches every read site across `cgen.rs`'s ~12,800 lines.
- **Why it stopped**: it was proposed at the tail of a session that had already
  landed a two-toolchain lowering change. Starting a wide mechanical refactor
  there is how you get a half-finished one. It wants a clean start, not more
  budget.

### 1.2 The newline / statement-boundary rule (P3, measured safe)

`f` on one line and `(x)` on the next parses as `f(x)`. Documented in
`docs/frontend-grammar.md`; options and the recommendation are in
`docs/frontend-roadmap.md` §8.

**The measurement is the useful part**: across all 175 `.jtr` files — including
the compiler's own ~30,000 lines — there are **zero** lines beginning with `(`,
`[`, `-`, `&`, `*`, `.` or `?`. Adopting the restricted rule (a postfix
continuation may not cross a newline in statement position) would change the parse
of *no existing file*.

- Recommendation stands: **do it**, as an isolated change, at P3.
- It is P3 only because it changes what programs *mean*, so it owes a mirror in
  `examples/std/parser.jtr`, a corpus re-verification, and a seed refresh.
- The conformance tables (§1.6) should be in place first. They are.

### 1.3 HIR Stages 2–4 (the real payoff, and the real cost)

Stage 2 moves desugaring into HIR construction; Stage 3 points `escape` at HIR;
Stage 4 points `cgen` at it, one node kind at a time. Each stage that changes
emitted C owes the full two-sided tax (Part 3). Do not start these before 1.1.

### 1.4 Diagnostics remainder (low risk, no mirror owed)

`Parser::expect_close` (done) keeps the message identical and adds a `help:` line
naming the opener. Wired into **block, `trait` and `match`** bodies only.

- ~28 other `expect(RBrace/RParen/RBracket)` sites are the same mechanical change.
  Deliberately **not** swept: a 31-site sweep is the kind of change that looks
  safe and isn't. Convert them as they are touched.
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

Missing, in value order:

1. **Differential fuzzing against the port.** The strongest oracle available: feed
   the same generated input to `jestyrc` and the self-hosted parser and require
   identical tree-or-diagnostics. The P2 goldens already do this for the *corpus*;
   doing it for generated input would be the highest-value fuzz target in the
   project. Nobody has built it.
2. A grammar-directed generator (mutate valid token sequences one token at a time).
3. A diagnostic-count *property* — error count ≤ a linear function of token count —
   rather than the per-case budget in the table.

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

- **The port emits no `#line` directives** where the reference's module path does,
  so `jestyrc attest` and `jc attest` disagree on `c-sha256`. Invisible to every
  golden, self-consistent within the port. Needs a module-path C golden first.
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
| 1 | `Unknown` safety finalization | high | **low** | small | no (diagnostics only) | **implement first** |
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
in one sitting.

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
