# Handoff — function-pointer / coercion frontier → traits

> Written 2026-06-25. Continues the fn-pointer-types workstream and the in-flight
> traits/interfaces epic. Read with `HANDOFF.md`, `docs/TESTING.md §5.12`, and
> `jestyr-design.md §7.3`. Discipline is unchanged: every increment stays
> `cargo test`-green and warning-clean, ships its three test layers (unit +
> property + bolero fuzz), teeth-verifies each new property by mutation, and is
> auto-committed (`git commit -F <file>`).

---

## Main objective — extend `fn_ptr_field` to `Ty::GenStruct`

**The one genuinely-remaining gap in fn-pointer field ergonomics, and the next
thing to build.** Calling a fn-pointer field *method-style* on a **generic-struct
receiver** — `gen.op(n)` where `gen: Box(i32)` and `op: fn(T) -> T` — already
lowers to correct C (`j_gen.j_op(j_n)`) and runs, but the type checker routes it
through the generic fallthrough, so the call expression's **result type infers as
`Unknown`**. On a *plain* struct the same call is fully typed, because
`typeck::fn_ptr_field` recognizes the fn-pointer field and `resolve`s the call by
its return type.

**Objective:** teach `fn_ptr_field` (and the field-call disambiguation that uses
it) about `Ty::GenStruct`, so `gen.op(n)` infers its return type under the
struct's type-argument substitution — exactly as `gen_struct_field_decl_ty`
already resolves a generic field's *declared* type for closure coercion.

- **Where:** `src/typeck.rs` — `fn_ptr_field` is `Ty::Named`-only today; mirror the
  generic resolution in `gen_struct_field_decl_ty` (`find_fn_decl` →
  `ctor_struct_body` → `comptime_tp_names` → `subst_ty` with the receiver's type
  args). The field-call site is the `ExprKind::Call` / `ExprKind::Field` callee
  branch where `fn_ptr_field` is consulted ahead of method-call sugar.
- **Codegen already works** (the call lowers correctly); this is a *typeck
  completeness* fix, so the test that has teeth is the inferred-type assertion:
  find the `gen.op(n)` `Call` expr and assert `info.type_of(call)` is the
  substituted return, not `Unknown`. Add the property over `arb_*` generic-vtable
  programs and a gcc round-trip if you wire one.

**Why it matters — the arc this completes.** A fn-pointer field on a struct *is*
a hand-written vtable. Making generic-struct vtable calls fully typed is the last
step before the compiler-synthesized version: **`dyn Trait`** erases the receiver
type and dispatches through an emitted vtable struct that is byte-compatible with
this hand-written one (traits **Stage F**). So this small typeck fix is the
foundation `dyn` / auto-synthesized vtables build on — finish it first, then the
`dyn` vtable reuses the same field-call machinery instead of inventing a parallel
one.

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

Suite is **345 green**, warning-clean, clean across the `dharht-experiment` and
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
