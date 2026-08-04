# Proof obligations — `jestyrc obligations <file>`

`@verified` promises static proof. Before building a solver, this answers the question
that has to come first: **what would have to be proven?**

```bash
jestyrc obligations examples/contracts.jtr
```

```text
obligations v1
total 2
requires 1 (caller)
ensures 1 (callee)
verified-demanded 0
fn abs
  ensures result >= 0
fn safe_div
  requires b != 0
note: declared obligations only; implicit ones (bounds, overflow) are not counted
```

## The finding this produced

**7 declared obligations across the whole 144-file corpus.**

That number is the point of the exercise. It says an SMT backend would have almost
nothing to discharge today, so the prerequisite for `@verified` is **writing contracts**,
not building a solver. That conclusion was available for the price of a report, and not
otherwise — it is exactly the kind of thing an impression gets wrong in both directions.

It is pinned as an *upper* bound (`obligation_extraction_is_total_over_the_corpus`),
never an equality: contracts should grow, and a test that failed when someone wrote a
`requires` would be worse than useless. It fires when the corpus is contract-rich enough
to re-open the sizing question.

## What is collected

| Kind | Syntax | Who discharges it |
|---|---|---|
| Precondition | `requires <expr>` | the **caller**, at every call site |
| Postcondition | `ensures <expr>` | the **callee**, before every return |
| Invariant | `invariant <expr>` | the callee, on entry to each loop iteration |
| Variant | `variant <expr>` | the callee — `>= 0` **and** strictly decreasing |
| Refinement | `i: usize in 0..len` | the caller, per argument |

The **caller/callee split** is the column a solver actually needs: a precondition is
*assumed* inside the body and *proven* at each call site; a postcondition is the reverse.
Getting that backwards is the classic way to build a verifier that proves nothing.

`variant` is the only kind whose statement is a *pair* of facts, which is why
termination is usually the expensive obligation.

## What is deliberately not counted

Only **declared** obligations — the ones a human wrote down. The implicit ones (every
index in bounds, every arithmetic op non-overflowing) are real proof obligations too, and
there are vastly more of them, but they are *discovered* by a pass rather than stated by a
programmer.

The report says so on **every run**, not just here. A count that quietly omitted them
would badly under-size the very work it exists to size — and that failure would look
exactly like good news.

## Design notes

* **Analysis only.** Parsing is enough (an obligation is declared syntax), so this runs
  neither the type checker nor the backend and emits nothing. Type errors are not fatal:
  "what does this function promise?" is worth answering while it is still being written —
  the same call `jestyrc layout` makes.
* **Contract text is sliced from the source, not re-rendered.** A proof obligation is a
  claim about what the user wrote, so echoing their own words is both exact and
  reviewable — the rule `doc` and `attest` already follow. Internal whitespace is
  collapsed so a multi-line contract stays one diffable line.
* **Methods are qualified** (`Type.method`), or two types' `len` methods would merge into
  one heading and the report would be useless for review.
* **`for` is Jestyr's unified loop** — infinite, condition, and iteration are `ForHead`
  shapes of one expression — so one walk arm covers every loop a contract can attach to.

## Status

`@verified` remains **reserved**: promising proof while not proving would be unsafe, so
using it is an error. The refusal now points here, which is the honest answer to "what
would it do?"
