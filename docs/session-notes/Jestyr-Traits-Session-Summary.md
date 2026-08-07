> **Internal development log** — kept for provenance. Status lines and counts in this file reflect the moment it was written, not the current state. Start at the repo [README](../../README.md) for current status and verified claims.

# Jestyr — Traits & Generics: Session Summary

**Date:** 2026-06-26
**Branch:** `claude/nice-goodall-48c49f`
**Scope:** the traits/interfaces epic + the generics work it depends on
**Headline:** test suite **345 → 413 green**, warning-clean across the default,
`dharht-experiment`, and `bench-alloc` builds; **8 commits**, ~2,700 lines added.
The full trait epic (**Stages A–F**) is now complete. Every increment ships three
test layers (unit + property + bolero fuzz), is teeth-verified by mutation, and
includes an end-to-end gcc round-trip example.

---

## The arc

The session completed the **trait static-dispatch story** and the **generics
substrate** underneath it, in a deliberate dependency order. Each layer reused the
previous one with almost no new machinery:

```
fn-ptr field call on a generic struct   (typeck completeness + latent ABI fix)
        │
        ▼
Stage C — static monomorphized dispatch  (emit + mangle impl methods)
        │
        ├──► Stage D — definition-site bounds      (check [T: Tr] obligations)
        │
        ├──► Stage E — operator traits             (+ - * / == != < > <= >=)
        │
        ▼
Bracket-generic codegen                  (monomorphize f[T: Bound], infer T)
        │
        ▼
Body-side bound enforcement              ("Zig fix": x.m() through the bound)
        │
        ▼
Stage F — dyn Trait vtable               (erase the receiver, dispatch at run time)
```

Static (monomorphized) and dynamic (`dyn`) dispatch are the two ends of the same
spectrum, and the session built both: the "Zig fix" picks the impl at *compile*
time per instantiation; `dyn` picks it at *run* time through a vtable — from one
un-monomorphized function.

A single idea recurs throughout: **resolve abstractly in the type checker, then
dispatch concretely per monomorphized instance in the backend.** A trait-method
call on an abstract `T` can't know its target until the generic is instantiated, so
the type checker records *what* (trait + method + type parameter) and the code
generator resolves *which* (the concrete `impl`) using the active type
substitution.

---

## 1 — Fn-pointer field call on a generic-struct receiver
**Commit:** `3c3ebd3` · **Files:** `typeck.rs`, `cgen.rs`

Calling a fn-pointer field method-style on a **generic-struct** receiver —
`gen.op(n)` where `gen: Box(i32)` and `op: fn(T) -> T` — now infers its result
under the struct's type-argument substitution instead of `Unknown`.

- `typeck::fn_ptr_field` was `Ty::Named`-only; it now also matches
  `Ty::GenStruct`, resolving the field type under substitution via the existing
  `gen_struct_field_decl_ty` template (`find_fn_decl → ctor_struct_body →
  comptime_tp_names → subst_ty`).
- **Bonus correctness:** the generic case previously fell through to a tail in
  `cgen::emit_call` that rendered the field access blindly — *conv-unaware*, so a
  `fn(mut T)` field passed its argument by value (an ABI mismatch). Setting the
  callee's type to `Ty::Fn` reroutes through `emit_fn_ptr_invoke`, which honors
  per-parameter `Conv`. This healed a latent miscompile, not just a type gap.
- **gcc round-trip:** `examples/gen_vtable.jtr` → `42 / 141 / 8` (both `&fn` and
  closure-coercion field init, plus a bare field *read* bound to a local then
  called through).

---

## 2 — Stage C: static, monomorphized dispatch
**Commit:** `b022b94` · **Files:** `cgen.rs` (+ a bare generic-field read fix in `typeck.rs`)

Consumes the `impl_calls` recorded by Stage B and lowers a resolved `recv.m(args)`
to a **direct call** of the impl method — no vtable.

- Each `impl Trait for Type` method is emitted as a mangled C function
  `jestyr_impl_<Trait>__<TypeKey>__<method>`, receiver-first, reusing the
  struct-method `self` machinery (`emit_impl_method_decl`, `emit_impl_call`).
- Distinct impls of one trait for different types produce distinct symbols, each
  selected at its call site by the receiver's type key — the essence of static
  dispatch.
- The "bare generic-field read" companion fix lets `let f = gen.op` (read a
  generic struct's field without calling) resolve under substitution.

---

## 3 — Stage D: definition-site bounds
**Commit:** `658f79b` · **File:** `typeck.rs`

A bracket-form bound `[T: Tr]` is checked in two halves (typeck-only):

- **Declaration** (`check_bound_traits_declared`): every bound names a registered
  trait — a typo / unknown trait is caught at the definition, over free functions,
  `impl` methods, and struct methods.
- **Call-site obligation** (`check_call_bounds`): at each call `f[T: Tr](…)`, the
  concrete `T` is recovered by unifying `f`'s declared parameter types against the
  actual argument types (`unify_tp`) and must `impl Tr` (reusing `impl_index`); an
  unsatisfied bound errors *at the call site*. Unknown-bound and
  unresolved/opaque `T` are skipped to avoid false positives.

This is the "blame the generic code" caller-obligation direction (design §8.2).

---

## 4 — Stage E: operator traits (+ f64 determinism seam)
**Commits:** `003a482`, completed in `60457e5` · **Files:** `typeck.rs`, `cgen.rs`, `main.rs`

Built-in operators desugar to synthetic-trait methods, so a user type opts into
operator syntax by `impl`-ing them — lowering through the Stage C path.

- **Six primitive operator traits** (`register_operator_traits`): `+`→`Add::add`,
  `-`→`Sub::sub`, `*`→`Mul::mul`, `/`→`Div::div`, `==`→`Eq::eq`, `<`→`Ord::lt`.
  Reserved names — a user `trait Add` collides with the built-in.
- **Four derived comparisons** need *no extra impls* — they reuse `Eq::eq` /
  `Ord::lt` with a swap and/or negate applied at lowering (`emit_operator_call`):

  | operator | lowering |
  |---|---|
  | `!=` | `!eq(a, b)` |
  | `>`  | `lt(b, a)` (swap) |
  | `<=` | `!lt(b, a)` (swap + negate) |
  | `>=` | `!lt(a, b)` (negate) |

- A binary op whose left operand is a *user type* resolves through
  `impl <OpTrait> for <lhs>` (`resolve_operator_trait`, recorded in `impl_calls`
  keyed by the binary expr). Result type = the impl method's return (`Add`/`Sub`/
  `Mul`/`Div` → the type; comparisons → `bool`). Missing impl → error; primitives
  keep native C semantics.
- **`f64` no-FMA determinism seam:** the gcc invocation now passes
  `-ffp-contract=off` (`main.rs`), forbidding `a*b + c` → fused multiply-add so
  `f64` `+`/`*` are bit-reproducible across platforms (the numerics workstream's
  key seam).
- **gcc round-trip:** `examples/operators.jtr` → `13 / 1 / 42 / 6 / 0 / 1 / 1 / 0 / 1 / 0`.

---

## 5 — Bracket-generic codegen
**Commit:** `ba8dc83` · **Files:** `typeck.rs`, `cgen.rs`

Bracket-form `[T: Bound]` generics now **compile and run**: a call emits a mangled
instance per concrete instantiation, with each `T` *inferred* from the value
arguments — the inference-based counterpart to a `comptime` generic's explicit
`pick(i32, …)`.

- `typeck::unify_tp` is now `pub(crate)` and **shared with cgen** — the single
  matcher that recovers `T = i32` from a declared `Opaque("T")` vs a concrete arg
  type, so the two layers can never disagree.
- `monomorphize_ret` infers bracket `T`s from the argument types, so `dup(5) -> T`
  types as `i32`.
- cgen: `is_generic` + the `generics` set include bracket generics;
  `infer_bracket_args` recovers the type args; `make_subst` maps `comptime ++
  bracket` names; instance collection (`find_calls_expr`) and emission
  (`emit_generic_call`) append the inferred bracket args.
- **Mixing** `comptime T: type` + bracket `[U]` in one signature works (type args
  assembled `comptime ++ bracket`; locked in by test + example).
- **gcc round-trip:** `examples/bracket_generic.jtr` → `42 / 0 / 99`.

---

## 6 — Body-side bound enforcement (the "Zig fix")
**Commit:** `6573437` · **Files:** `typeck.rs`, `cgen.rs`, `types.rs`

Inside a bracket-generic body `f[T: Tr]`, a method call on a `T`-typed value now
resolves **through the bound** `Tr` — and dispatches to the concrete `impl` at each
instantiation (design §8.2's "blame the generic code, not the caller").

- `check_fn` threads the current function's bracket params → bounds into
  `cur_type_param_bounds`.
- **typeck** (`resolve_bound_method`): for `x.m()` where `x: Ty::Opaque(T)` and `T`
  is a bracket param, `m` must be a method of `T`'s bound `Tr` — else a
  **definition-site error**; an unbounded `[U]` rejects every method. Types the
  call by the trait method's declared return and records
  `bound_method_calls[id] = BoundMethodCall { trait, method, type_param }`.
- **cgen** (in `emit_call`): the concrete receiver type is `T`'s binding in the
  *active monomorphization* (`self.subst[type_param]`), so a synthesized `ImplCall`
  reuses `emit_impl_call`. The same `x.m()` ExprId lowers to a *different* impl per
  instance — the reason the resolution is recorded abstractly rather than as a
  concrete `ImplCall`.
- **gcc round-trip:** `examples/bound_method.jtr` → `42 / 70` (one generic body,
  two impls: `Show for i32` and `Show for P`).

---

## 7 — Stage F: `dyn Trait` vtable (the capstone)
**Commit:** `f023324` · **Files:** `typeck.rs`, `cgen.rs`, `types.rs`

`dyn Trait` erases the receiver type and dispatches through a compiler-synthesized
vtable that is **byte-compatible with the hand-written fn-pointer-field vtable from
§1** — the compiler now builds what a user could write by hand, closing the arc the
project predicted.

- **Representation:** `dyn Trait` → a fat pointer
  `JestyrDyn_<T> = { void* data; const JestyrVtable_<T>* vtable; }`; the vtable is a
  struct with one function pointer per method (receiver erased to `void*`). Both
  synthesized per `dyn`-used trait (`cgen::dyn_typedefs`).
- **Per-impl vtables** (`cgen::dyn_vtables`): each `impl Trait for T` emits a shim
  per method (casting `void* self` back to `T`) and a `static const` instance
  `jestyr_vt_<T>__<key>` wired to the shims in trait-method order.
- **Coercion** (`typeck::record_dyn_coercion`): passing a concrete value where
  `dyn Trait` is expected verifies the impl and the backend builds the fat pointer.
  A subtlety worth noting — the erased data needs a **non-dangling** address, so a
  scalar is placed in a block-scoped C compound literal `&((T){v})` (a
  statement-expression temp would die at the `})` and dangle).
- **Dispatch:** `d.m(args)` → `d.vtable->m(d.data, args)` — one function, the impl
  chosen at **run time** by the value's actual type.
- **gcc round-trip:** `examples/dyn_dispatch.jtr` → `42 / 70 / 70` (one `describe`
  function, three values of two types).

`★ Static vs dynamic ─────────────────────────────`
The "Zig fix" (§6) and `dyn` (§7) are duals: both call a trait method on an
abstract receiver, but the Zig fix resolves it **per monomorphized instance** (a
`BoundMethodCall` recovered through `subst` → a direct call), while `dyn` resolves
it **once, at run time** (a `DynCall` → an indirect call through the vtable slot).
Same recording-abstractly-then-resolving-concretely pattern, two different "when."

---

## What works now

| Capability | Example |
|---|---|
| Trait + impl + coherence | `trait Show { … }  impl Show for i32 { … }` |
| Static monomorphized method dispatch | `x.show()` → `jestyr_impl_Show__i32__show(x)` |
| Definition-site bounds (declaration + call-site) | `fn f[T: Show](x: T)` checked, callers verified |
| Operator overloading (full set) | `a + b`, `a - b`, `a < b`, `a != b`, `a >= b`, … |
| f64 bit-reproducibility | `-ffp-contract=off` (no fused multiply-add) |
| Bracket-generic monomorphization | `fn dup[T](x: T) -> T`, `T` inferred from args |
| Mixed comptime + bracket generics | `fn mix[U](comptime T: type, a: T, b: U)` |
| Trait method on a bound type parameter | `fn f[T: Show](x: T) { x.show() }` (per-instance) |
| **Dynamic dispatch** via `dyn Trait` | `fn f(s: dyn Show) { s.show() }` (run-time impl) |
| Fn-pointer field vtable (plain + generic) | `gen.op(n)` on `Box(i32)` fully typed |

## Test posture

- **413 green**, warning-clean, across default + `dharht-experiment` + `bench-alloc`.
- Each feature ships **unit** (typeck + cgen), **property** (`proptests::prop`,
  e.g. `arb_operator_program`, `arb_bracket_generic_program`,
  `arb_bound_method_program`), and **bolero fuzz** (`fuzz_operator_traits`,
  `fuzz_bound_method_calls`, `fuzz_definition_site_bounds`) layers.
- Every new behavior was **teeth-verified by mutation** — the implementation was
  temporarily broken to confirm the relevant test fails (including catching one
  weak assertion that matched an always-emitted impl *definition* and strengthening
  it to assert the dispatch *call*).
- Seven end-to-end **gcc round-trip examples** under `examples/`: `gen_vtable.jtr`,
  `operators.jtr`, `bracket_generic.jtr`, `bound_method.jtr`, `dyn_dispatch.jtr`
  (plus the pre-existing `fn_ptr.jtr`).

## What remains

**The trait epic (Stages A–F) is complete** — static dispatch, bounds, operators,
generics, the "Zig fix", and `dyn`. The documented follow-ups are small and
non-blocking: defaulted trait methods aren't yet wired into `dyn` vtable slots, a
struct-returning *rvalue* passed directly as `dyn` needs a `let` binding first, and
non-object-safe traits (`Self` in a method's args) aren't rejected.

The frontier now moves to the broader roadmap (`jestyr-design.md §19`). Toward
**self-hosting** (the final roadmap milestone), the highest-value gaps are a
`HashMap`/`HashSet` in the standard library and file I/O — see the companion
analysis. The frontier handoff (`HANDOFF-NEXT.md`) and the trait-stage ledger
(`docs/TESTING.md §5.12`) carry the full status.

---

## Commit log (this session)

| Commit | Title |
|---|---|
| `3c3ebd3` | fn-ptr field call on a generic-struct receiver is fully typed |
| `b022b94` | Traits Stage C: static monomorphized dispatch + bare generic-field read |
| `658f79b` | Traits Stage D: definition-site bounds |
| `003a482` | Traits Stage E: operator traits + f64 no-FMA determinism seam |
| `ba8dc83` | Bracket-generic codegen: monomorphize f[T: Bound] |
| `6573437` | Body-side bound enforcement: the "Zig fix" for f[T: Tr] |
| `60457e5` | Operator-trait completeness: derived ops + mixed-generic coverage |
| `f023324` | Traits Stage F: dyn Trait vtable — the trait epic is complete |
