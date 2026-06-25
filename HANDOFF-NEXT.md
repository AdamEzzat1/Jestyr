# Handoff — function-pointer / coercion frontier → traits

> Written 2026-06-25. Continues the fn-pointer-types workstream and the in-flight
> traits/interfaces epic. Read with `HANDOFF.md`, `docs/TESTING.md §5.12`, and
> `jestyr-design.md §7.3`. Discipline is unchanged: every increment stays
> `cargo test`-green and warning-clean, ships its three test layers (unit +
> property + bolero fuzz), teeth-verifies each new property by mutation, and is
> auto-committed (`git commit -F <file>`).

---

## ✅ Done — `fn_ptr_field` extended to `Ty::GenStruct`

Calling a fn-pointer field *method-style* on a **generic-struct receiver** —
`gen.op(n)` where `gen: Box(i32)` and `op: fn(T) -> T` — is now **fully typed**:
the call's result infers under the struct's type-argument substitution instead of
falling through to `Unknown`.

- **Change:** `typeck::fn_ptr_field` now matches `Ty::GenStruct { ctor, args }` in
  addition to `Ty::Named`, resolving the field's declared type via
  `gen_struct_field_decl_ty` (`find_fn_decl → ctor_struct_body →
  comptime_tp_names → subst_ty`) and returning it when it is a `Ty::Fn`. Its sole
  call site (the `ExprKind::Call` field-callee disambiguation) needed no edit —
  it now sets the callee to the resolved `Ty::Fn`, which routes codegen through
  the real `emit_fn_ptr_invoke` path.
- **Bonus correctness:** the old generic *tail fallthrough* in `cgen::emit_call`
  emitted `callee(args)` with no awareness of the field-pointer's per-param
  conventions, so a `fn(mut T)` field passed its argument **by value** — an ABI
  mismatch. Routing through `emit_fn_ptr_invoke` reads the `Ty::Fn`'s conv and
  takes `&arg` for `mut`/`out`. (Test: `generic_vtable_field_call_takes_mut_arg_by_pointer`.)
- **Tests (teeth-verified by mutation):** typeck unit
  (`fn_pointer_field_call_on_a_generic_struct_*`), cgen
  (`lowers_a_field_call_through_a_generic_vtable_pointer`, the `mut` ABI test),
  a property + determinism prop over `arb_gen_vtable_program`, a
  `fuzz_generic_vtable_pipeline` target, and an end-to-end gcc round-trip example
  `examples/gen_vtable.jtr` (runs to `42/141`, covered by
  `gen_vtable_example_compiles_clean`). Suite now **353 green**, warning-clean.
- **Known adjacent gap (out of scope, not yet done):** a *bare* fn-pointer-field
  **read** on a generic struct — `let f = gen.op` without calling — still types as
  `Unknown` (`typeck::field_type` only resolves fields for `Ty::Named`, returning
  `Unknown` for `Ty::GenStruct`). The Main Objective was specifically the
  method-style *call*; generic-struct field *reads* are a broader generic
  field-access limitation to tackle separately if `dyn` needs it.

**Why it mattered — the arc this completes.** A fn-pointer field on a struct *is*
a hand-written vtable. Generic-struct vtable calls being fully typed was the last
step before the compiler-synthesized version: **`dyn Trait`** erases the receiver
type and dispatches through an emitted vtable struct byte-compatible with this
hand-written one (traits **Stage F**). The `dyn` vtable can now reuse this same
field-call machinery instead of inventing a parallel one.

---

## Main objective — traits **Stage C** (static, monomorphized dispatch)

With generic-vtable calls fully typed, the trait epic resumes at **Stage C**:
consume the `impl_calls` recorded by Stage B's `resolve_impl_method`, emit and
mangle the impl-method functions, and lower `recv.m(args)` to a **direct** call
(monomorphized — no vtable yet). See the trait-stage ledger in
`docs/TESTING.md §5.12` and the remaining-stages list below (D bounds, E operator
traits, F `dyn` vtable — which builds on the machinery just finished).

---

## Status — what is already done (committed to `master`)

### Function-pointer types (`fn(T1, T2) -> R`) — complete, end-to-end
Thin, first-class, capture-free pointer with **calling conventions in the type**
(`fn(read Node, take Buf) -> read T` — Jestyr-novel). Parses → type-checks →
escape-checks (escapes *freely*: it is `Copy`) → lowers to a C `typedef`
(`JestyrFn_<sig>`) that sidesteps C's inside-out declarator syntax. Storable in a
struct (the allocator-vtable shape), passable, returnable, reassignable, and
**callable through** — `f(x)` and `a.alloc_fn(n)`, disambiguated from a named call
purely by **type** (no `(a.f)()` ceremony). Obtained via `&fn` or by coercing a
non-capturing closure. See `examples/fn_ptr.jtr` (runs to `42/42/7/8/42`).

### Closure → fn-pointer coercion — complete
A *non-capturing* closure coerces to the expected `fn(...)` type in **let,
argument, return, and struct-literal-field** position — for **plain and generic**
structs (the generic field's type parameter resolved under substitution on both
the typeck and cgen sides; `subst_ty` recurses through `Ty::Fn`,
`collect_fn_types` walks monomorphized struct instances for the concrete typedef).
A *capturing* closure coerced to a fn-pointer is a clear error.

### Adjacent fix — clean "no `main`" diagnostic
`run`/`build` on a no-`main` library now reports a clear message instead of a raw
C-linker `WinMain` error (`test` mode is exempt; it synthesizes its own `main`).

### Traits / interfaces — Stages A & B done (epic in flight)
- **A — parse + represent:** `trait`/`impl`/`dyn Trait`/`[T: Bound]` into the AST
  (`Item::Trait`, `Item::Impl`, `TypeKind::Dyn`, `FnDecl::generics`); pipeline
  stays total with no semantics.
- **B — resolve + coherence:** typeck registers traits/impls
  (`GlobalTable::{traits, impls, impl_index}`), enforces coherence (unknown trait,
  conflicting `(trait, type)`, missing required method, non-member method), and
  resolves `recv.m(args)` through `impl Trait for <recv>` (`resolve_impl_method`,
  recorded in `impl_calls` for the backend).
- **Remaining (in order):** **C** static dispatch (monomorphized — consume
  `impl_calls`, emit + mangle impl-method functions, lower to a *direct* call, no
  vtable) · **D** definition-site bounds (`fn f[T: Tr]` checked once) · **E**
  operator traits (`+`/`*`/`==`/`<` → `Add`/`Mul`/`Eq`/`Ord`, the `f64` impl is the
  no-FMA determinism seam) · **F** `dyn` vtable (see Main Objective above).

---

## Test posture

Suite is **353 green**, warning-clean, clean across the `dharht-experiment` and
`bench-alloc` feature builds. New coverage adapts to the *real* harness
(`src/proptests.rs`: `mod prop` + `mod fuzz` + `arb_*_program` generators,
`typeck_diags` for diagnostic differentials) — the handoff's referenced
`c_oracle`/`escape_props`/`generic_props` modules and `--features c-oracle` do
**not** exist in-tree; do not assume them. `docs/TESTING.md §5.12` tracks the
trait stage ledger.

## Where to start reading

| Area | Start here |
|---|---|
| Generic field resolution (the Main Objective's template) | `typeck.rs::gen_struct_field_decl_ty`, `struct_field_decl_ty`, `fn_ptr_field` |
| fn-pointer lowering | `cgen.rs::{collect_fn_types, fn_type_typedefs, emit_thin_closure_fn}`; `c_type`/`ty_mangle` `Ty::Fn` arms |
| Trait resolution + coherence | `typeck.rs::{register_traits, register_impls, resolve_impl_method}`; `types.rs::{TraitDef, ImplDef, ImplCall, GlobalTable::ty_key}` |
| Monomorphization engine (extend for Stage C) | `cgen.rs::{collect_all_instances, collect_struct_instances, mangle, ty_mangle}` |
| Test architecture to mirror | `proptests.rs`: `mod prop` (`arb_trait_program`, `arb_fn_type_program`, coherence props), `mod fuzz` (`fuzz_traits_pipeline`) |
