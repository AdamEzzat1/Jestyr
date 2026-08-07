> **Internal development log** — kept for provenance. Status lines and counts in this file reflect the moment it was written, not the current state. Start at the repo [README](../../README.md) for current status and verified claims.

# Jestyr — Workstream Q: the `@span` work-span cost model

*Session summary. Landed on `master` as `276eef7`. The first genuinely Q-distinct
compiler feature — the tier-5 Motley cost-model tie-in. Companion: `PARALLELISM-HANDOFF.md`.*

## The idea

Lift Cilk/NESL's **work–span** cost model into Jestyr's contracts culture. A function
may declare its parallel **span** (depth — the length of its critical path, i.e. the time
it would take on unbounded processors), and the compiler **checks** it:

```jtr
@span(log)    fn par_sum(read s: []i64) -> i64 { par for x in s reduce(core.sum_reduction()) { x } }
@span(linear) fn serial_sum(read s: []i64) -> i64 { var a: i64 = 0  for x in s { a = a + x }  return a }
```

- A sequential `for` over the input has span **O(n)** → `@span(linear)`.
- A deterministic `par for … reduce(r)` is a parallel tree reduction with span **O(log n)**
  → `@span(log)`.

So if someone refactors `par_sum` and accidentally **serializes** it (replaces the `par for`
with a `for`), its span jumps from O(log n) to O(n), exceeds the declared `@span(log)`, and
becomes a **compile error** — not a silent performance regression. Cost is part of the proven
interface.

## How the span is computed

`attrs::validate_fn` walks the function body and computes its span as an asymptotic class
**n^k · (log n)^j**, tracked as a pair `(k, j)`:

| Construct | Contribution |
|---|---|
| straight-line code, a call | `(0,0)` — constant (a call's cost is *its own* `@span`, intraprocedural) |
| `if` / `match` | max of the branches (worst arm dominates the critical path) |
| sequence of statements | max (asymptotically, sum ≈ max for these classes) |
| a sequential `for` (any head) | multiply the body by **n** → `k += 1` |
| `par for … reduce(r)` | **log n** → `(0,1)`, regardless of the per-element map body |
| nesting | multiply → exponents add |

Ordering is lexicographic on `(k, j)` — the polynomial degree `k` always dominates a log
factor, then `j` breaks ties. The declared class must be **≥** the computed one (declaring a
*looser* bound is allowed, like weakening a `requires`). Classes:
`constant` · `log` · `linear` · `linearithmic` · `quadratic`.

(`constant`, not `const` — `const` is a reserved keyword, so it can't be the attribute's
identifier argument.)

## Where it lives — and why

Entirely in **`src/attrs.rs`** (the attribute registry + `validate_fn`), which already has the
function body and runs in the parser's diagnostic stream. Deliberately **no new compiler pass
and no `main.rs` edit**: there is no `lib.rs`, so all `mod` declarations live in `main.rs`,
which another workstream (Tooling/O) owns. `@span` is a *check-only* contract (like `@no_alloc`)
— it lowers to nothing, so no cgen change either. The span computation needs no type
information; it is purely structural.

**Intraprocedural v1**: a function call counts as O(1) (its own `@span` is its contract). So
the model reasons about *this* body's loop structure — which is exactly what catches a `par for`
quietly rewritten as a `for`. Interprocedural propagation (a callee's `@span` flowing into the
caller's cost) is a clean follow-up.

## Tests

| Layer | Test | Toolchain |
|---|---|---|
| **Rejection soundness (the star)** | `cost_model::span_log_accepts_par_for_rejects_serial` — a `par for` satisfies `@span(log)`; the same fold written sequentially violates it | none (default `cargo test`) |
| Class algebra | `cost_model::span_classes_compose_and_check` — one loop is linear, nested loops are quadratic, loop-free is constant; under/over-declaration checked | none |
| Looseness + bad input | `cost_model::span_looser_ok_unknown_class_errors` — a looser declared class is fine; an unknown class name is a clean error | none |
| Compile-clean demo | `cost_model::par_cost_example_compiles_clean` | none |
| End-to-end | `c_oracle::par_cost_demo` — `par_cost.jtr` runs (`5050, 1`) | `--features c-oracle` (gcc) |

Full default suite: **645 passed / 0 failed**; c-oracle demo green. Demo: `examples/std/par_cost.jtr`.

**Teeth:** the model distinguishes `par for` (clean) from the identical serial fold (violation)
in both directions — if the computation treated a `for` as O(log n), the rejection test fails.

## The session-level context

Workstream Q (data parallelism) and Workstream N (concurrency) have **heavily overlapping
scope**, and the N session moved faster, independently landing `core.par_reduce`, the
`par for … reduce(r)` surface, dynamic-N spawn, and `@deterministic` — twice forcing this Q
session to discard a duplicate build. The `@span` cost model is the **first feature squarely in
Q's own lane** (a cost-analysis the concurrency stream has no reason to build). It is also the
clearest path toward the **Motley** thesis: a systems language where a parallel computation's
*cost* — and, by layering CJC's thermal/energy estimate onto the same `@span` machinery, its
*energy* — is part of the machine-checked contract.

## Explicitly deferred

- **Interprocedural cost** — propagate a callee's `@span` into the caller (v1 is intraprocedural).
- **Work (`W`) alongside span (`D`)** — `@work(...)`; span is the differentiator for the
  serialization thesis, so it shipped first.
- **CJC thermal / energy classes** on the same attribute — the deeper Motley tie-in.
- **`with schedule(threads, chunk)`**, SIMD, GPU SOACs — the rest of the parallelism roadmap.
