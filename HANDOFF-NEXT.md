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
- **Adjacent gap — now closed:** a *bare* fn-pointer-field **read** on a generic
  struct (`let f = gen.op` without calling) used to type as `Unknown`.
  `typeck::field_type` now resolves a field on `Ty::GenStruct` under substitution
  (via `gen_struct_field_decl_ty`), so the read is fully typed and a later `f(n)`
  is a typed indirect call (and a genuinely-missing field still diagnoses). Tests:
  `bare_fn_ptr_field_read_on_a_generic_struct_*`, `unknown_field_on_a_generic_struct_is_reported`,
  property `generic_vtable_bare_field_read_types_under_substitution`, and the
  `examples/gen_vtable.jtr` round-trip (now `42/141/8`).

**Why it mattered — the arc this completes.** A fn-pointer field on a struct *is*
a hand-written vtable. Generic-struct vtable calls being fully typed was the last
step before the compiler-synthesized version: **`dyn Trait`** erases the receiver
type and dispatches through an emitted vtable struct byte-compatible with this
hand-written one (traits **Stage F**). The `dyn` vtable can now reuse this same
field-call machinery instead of inventing a parallel one.

---

## ✅ Done — traits **Stage C** (static, monomorphized dispatch)

The backend now consumes the `impl_calls` Stage B records. A `recv.m(args)` call
that resolved through `impl Trait for <recv>` lowers to a **direct** call of the
emitted impl-method function — no vtable; the target is fixed at compile time by
the receiver's type key.

- **Emit:** `cgen::{impl_protos, impl_defs}` walk every `Item::Impl` and emit each
  method as a top-level C function named
  `jestyr_impl_<Trait>__<TypeKey>__<method>` (free helper `impl_method_c_name`,
  the type key sanitised to a C identifier). The receiver is the first parameter
  (`j_self`, by `T* restrict` for `mut`/`out self`), reusing the **struct-method**
  `self` machinery (`self_cty`/`self_is_ptr`/`method_params_str`) rather than a
  parallel `self` lowering — so `self.field` projection works for free.
- **Lower:** `cgen::emit_impl_call` (dispatched from `emit_call` via
  `info.impl_calls`) threads the receiver in as the first argument (by `&` for
  `mut`/`out self`), `mut`/`out` args by address, and derives the same mangled
  name from the `(trait, type-key, method)` triple — so definition and call site
  always agree without a symbol table. `find_impl_method` recovers the conv.
- **Monomorphization:** impl bodies are now walked by `collect_all_instances` and
  `collect_struct_instances`, so a generic call/struct *inside* an impl method
  instantiates correctly (they emit like free functions).
- **Tests (teeth-verified by mutation):** cgen unit (primitive + struct receiver,
  trailing arg, `mut self` by pointer, two impls → two symbols), properties
  `trait_call_lowers_to_a_direct_impl_method_call` + determinism over
  `arb_trait_call_program`, `fuzz_trait_static_dispatch`, and the gcc round-trip
  `examples/traits_static.jtr` (`42/141/15/18`, `traits_static_example_compiles_clean`).
  Suite now **364 green**, warning-clean across default + both feature builds.
- **Stage-C limitations (documented, not blocking):** a trait **default method**
  the impl omits is not yet dispatched (only *provided* methods are in
  `method_rets`, so such a call doesn't resolve — Stage B behavior unchanged); and
  **closures/`spawn` inside an impl-method body** aren't collected (a closure there
  degrades to a clean "unsupported" diagnostic, not a crash). Lift these if a later
  stage needs them.

---

## ✅ Done — traits **Stage D** (definition-site bounds)

A bracket-form bound `[T: Tr]` is now **checked** (typeck-only — bracket generics
aren't monomorphized yet, so there's no runtime differential and no new codegen):

- **Declaration half** (`typeck::check_bound_traits_declared`, phase 4): every
  bound names a registered trait, else an error at the definition — a typo
  (`[T: Bogus]`) is caught, not silently ignored. Covers free fns, `impl` methods,
  and struct methods.
- **Call-site obligation** (`typeck::check_call_bounds`, in `infer`'s
  `ExprKind::Call` name-callee path): at a call `f[T: Tr](…)`, the concrete `T` is
  recovered by unifying `f`'s declared param types against the actual arg types
  (`unify_tp`), then must `impl Tr` (reusing `impl_index`); an unsatisfied bound
  errors *at the call*. Unknown-bound `T` (declaration half's job) and
  unresolved/opaque `T` (a call nested in another generic) are skipped to avoid a
  false positive. Comptime-generic and method-sugar calls are unaffected
  (`generics` is empty / the hook is the free-call path only).
- **Tests (teeth-verified by mutation):** typeck unit (unsatisfied → error,
  satisfied → clean, unknown-trait bound → definition error, struct receiver
  satisfies via its impl, unbounded `[U]` imposes nothing), properties
  `unsatisfied_bound_always_errors_at_the_call` (soundness) +
  `satisfied_bound_never_errors_at_the_call` (completeness), and
  `fuzz_definition_site_bounds`. Suite **372 green**, warning-clean.

**Note on scope.** This is the *enforcement* layer. Bracket generics still lower
to `Ty::Opaque("T")` and the backend does not monomorphize them (a bracket-generic
*call* still hits cgen's "cannot lower external type `T`"), so there is no gcc
round-trip yet — that's a separate **bracket-generic codegen** workstream, not part
of the trait stages. The complementary *body-side* "blame the generic code" check
(inside `f[T: Tr]`, only `Tr`'s methods are callable on a `T` value — design §8.2's
"Zig fix") is **not yet done**: it needs bracket params treated as real type
parameters in `fn_type_params`/`lower_type` and method resolution through the bound.

---

## ✅ Done — traits **Stage E** (operator traits)

The built-in operators now desugar to synthetic-trait methods, so a user type opts
into operator syntax by `impl`-ing the matching trait:

- **Built-in traits** (`typeck::register_operator_traits`, phase 3): `+`→`Add::add`,
  `*`→`Mul::mul`, `==`→`Eq::eq`, `<`→`Ord::lt`, registered synthetically (no AST
  `trait` item; reserved — a user `trait Add` collides).
- **Resolve** (`typeck::resolve_operator_trait`, in the `ExprKind::Binary` arm): a
  binary op whose **left operand is a user type** resolves through
  `impl <OpTrait> for <lhs>` and is recorded in `impl_calls` keyed by the binary
  expr; result type = the impl method's return (`Add`/`Mul` → the type, `Eq`/`Ord`
  → `bool`). A user type used with the operator but lacking the `impl` is an error;
  primitives keep native C semantics.
- **Lower** (`cgen::emit_operator_call`, in the `ExprKind::Binary` arm): `a + b` →
  `jestyr_impl_Add__<T>__add(a, b)` (lhs receiver, rhs arg), reusing the Stage C
  mangling + emission. No new backend machinery.
- **`f64` no-FMA determinism seam:** the gcc invocation now passes
  `-ffp-contract=off` (`src/main.rs`), forbidding `a*b + c` → fused multiply-add so
  `f64` `+`/`*` are bit-reproducible across platforms (the numerics workstream's
  key seam — see `NUMERICS-RESEARCH.md`).
- **Tests (teeth-verified by mutation):** typeck unit (arith → type + recorded,
  comparison → `bool`, missing-impl error, primitives untouched, reserved-name
  collision), cgen unit (operator → impl-call lowering, primitive stays native),
  properties over all four operators + determinism, `fuzz_operator_traits`, and the
  gcc round-trip `examples/operators.jtr` (`13/42/0/1`). Suite **383 green**,
  warning-clean.
- **Scope note:** only the four operators `+`/`*`/`==`/`<` are wired. Derived
  operators (`-`/`/`/`!=`/`>`/`<=`/`>=`) on user types are **not** yet desugared
  (they'd negate/swap an `Eq`/`Ord`/`Add` call); they currently keep native
  semantics (invalid C for a struct, but a pre-existing gap, not a regression).

---

## Main objective — **bracket-generic codegen** (monomorphize `f[T: Tr]`)

The next step (a prerequisite for the body-side bound check below): make
bracket-form `[T: Bound]` params **real type parameters that compile and run**.
Today they lower to `Ty::Opaque("T")` and the backend doesn't monomorphize them — a
bracket-generic *call* hits cgen's "cannot lower external type `T`". The work:
infer each `T` from the value arguments at a call (the inference-based counterpart
to comptime generics' explicit `pick(i32, …)`), thread it through the
monomorphization engine (`cgen::{is_generic, make_subst, mangle, collect_all_instances}`),
and emit a mangled instance per concrete instantiation. This gives Stage D a gcc
round-trip and unblocks the **body-side "blame the generic code" check** (design
§8.2's "Zig fix": inside `f[T: Tr]`, only `Tr`'s methods are callable on a `T`
value — needs bracket params as real type params + method resolution through the
bound). Trait **Stage F** (`dyn` vtable, reusing the fn-pointer-field call
machinery) is the remaining trait stage after these.

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

### Traits / interfaces — Stages A–E done (epic in flight)
- **A — parse + represent:** `trait`/`impl`/`dyn Trait`/`[T: Bound]` into the AST
  (`Item::Trait`, `Item::Impl`, `TypeKind::Dyn`, `FnDecl::generics`); pipeline
  stays total with no semantics.
- **B — resolve + coherence:** typeck registers traits/impls
  (`GlobalTable::{traits, impls, impl_index}`), enforces coherence (unknown trait,
  conflicting `(trait, type)`, missing required method, non-member method), and
  resolves `recv.m(args)` through `impl Trait for <recv>` (`resolve_impl_method`,
  recorded in `impl_calls` for the backend).
- **C — static, monomorphized dispatch:** the backend emits each impl method as a
  mangled C function (`jestyr_impl_<Trait>__<TypeKey>__<method>`, receiver-first,
  reusing the struct-method `self` machinery) and lowers a resolved `recv.m(args)`
  to a *direct* call of it (no vtable). See the §5.12 ledger.
- **D — definition-site bounds:** typeck checks each bracket-form bound `[T: Tr]` —
  the bound names a known trait (declaration), and every call instantiates `T` at a
  type that `impl`s `Tr` (call-site obligation, reusing `impl_index`). Typeck-only;
  bracket generics aren't monomorphized yet. See the done-section / §5.12 ledger.
- **E — operator traits:** `+`/`*`/`==`/`<` desugar to synthetic `Add`/`Mul`/`Eq`/`Ord`
  traits; a binary op on a user type dispatches through its `impl` (Stage C path).
  Plus the `-ffp-contract=off` f64 determinism seam. See the done-section / §5.12.
- **Remaining (in order):** **bracket-generic codegen** + the **body-side bound
  check** (see Main Objective above) · trait **F** `dyn` vtable (reuses the
  fn-pointer-field call machinery — see the first done section's arc).

---

## Test posture

Suite is **383 green**, warning-clean, clean across the `dharht-experiment` and
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
