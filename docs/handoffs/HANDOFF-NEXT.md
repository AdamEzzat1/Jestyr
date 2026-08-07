> **Internal development log** — kept for provenance. Status lines and counts in this file reflect the moment it was written, not the current state. Start at the repo [README](../../README.md) for current status and verified claims.

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
- **Full operator set:** all of `+`/`-`/`*`/`/`/`==`/`!=`/`<`/`>`/`<=`/`>=` are
  wired. Six "primitive" methods are implemented directly (`Add`/`Sub`/`Mul`/`Div`
  + `Eq::eq` + `Ord::lt`); the four remaining comparisons are **derived** by a
  swap/negate at lowering (`!=`→`!eq`, `>`→swapped `lt`, `<=`→`!`swapped `lt`,
  `>=`→`!lt`) — so a user type opts into the full set by `impl`-ing just those six.
  (`%` and the bit/logical operators keep native semantics by design.)

---

## ✅ Done — **bracket-generic codegen** (monomorphize `f[T: Tr]`)

Bracket-form `[T: Bound]` generics now **compile and run**: a bracket-generic call
emits a mangled instance per concrete instantiation, with each `T` *inferred* from
the value arguments (the inference-based counterpart to a `comptime` generic's
explicit `pick(i32, …)`).

- **Inference seam:** `typeck::unify_tp` is now `pub(crate)` and shared with cgen —
  the one matcher that recovers `T = i32` from a declared `Opaque("T")` vs a
  concrete arg type, used by both layers so they never disagree.
- **typeck:** `monomorphize_ret` also infers bracket `T`s from the argument types,
  so `dup(5) -> T` types as `i32` (not the bare parameter).
- **cgen:** `is_generic` and the `generics` set include bracket generics;
  `infer_bracket_args` unifies declared params against `info.type_of(arg)` (under
  the enclosing subst) to get the type args; `make_subst` maps `comptime ++
  bracket` names; instance collection (`find_calls_expr`) and emission
  (`emit_generic_call`) append the inferred bracket args to the comptime ones. A
  pure-bracket call passes all value args (no comptime positions to erase).
- **Tests (teeth-verified by mutation):** typeck unit (return inferred from the
  arg), cgen unit (i32 instance emitted + called, two types → two instances,
  multi-param mangles each arg in order), property + determinism over
  `arb_bracket_generic_program`, and the gcc round-trip `examples/bracket_generic.jtr`
  (`42/0`). Suite **390 green**, warning-clean.
- **Mixing comptime + bracket** generics in one signature works and is covered:
  type args are assembled `comptime ++ bracket` everywhere (the comptime arg is an
  explicit type expression erased from the value params; the bracket arg is
  inferred from a value arg), and `make_subst` maps names in that same order
  (`mixes_a_comptime_and_a_bracket_type_parameter`, `examples/bracket_generic.jtr`).

---

## ✅ Done — **body-side bound enforcement** (the "Zig fix")

Inside a bracket-generic body `f[T: Tr]`, a method call on a `T`-typed value now
resolves **through the bound** `Tr` — and dispatches to the concrete `impl` at each
instantiation:

- **Context:** `check_fn` threads the current function's bracket params → bounds
  into `TypeChecker::cur_type_param_bounds` (restored on exit).
- **typeck** (`resolve_bound_method`, last in the method-call resolution chain):
  for `x.m()` where `x: Ty::Opaque(T)` and `T` is a bracket param, `m` must be a
  method of `T`'s bound `Tr` (`TraitDef::has_method`) — else a **definition-site
  error** ("blame the generic code"); an *unbounded* `[U]` rejects every method.
  Types the call by the trait method's declared return (`trait_method_ret`, with
  `Self` → the parameter). Records `TypeInfo::bound_method_calls[id] =
  BoundMethodCall { trait, method, type_param }`.
- **cgen** (in `emit_call`, after the `impl_calls` check): the concrete receiver
  type is `T`'s binding in the *active monomorphization* (`self.subst[type_param]`),
  so a synthesized `ImplCall` reuses `emit_impl_call` to dispatch to
  `jestyr_impl_<Tr>__<concrete>__<m>`. The same `x.m()` ExprId lowers to a
  *different* impl per instance — the whole reason the resolution is recorded
  abstractly (trait+method+param) rather than as a concrete `ImplCall`.
- **Tests (teeth-verified by mutation):** typeck unit (bound method → typed +
  recorded, non-bound method → definition error, unbounded param → error), cgen
  unit (per-instance dispatch, asserting the *call* `(j_x)` not the always-emitted
  impl def), property + determinism over `arb_bound_method_program`,
  `fuzz_bound_method_calls`, and the gcc round-trip `examples/bound_method.jtr`
  (`42/70` — one body, two impls). Suite **398 green**, warning-clean.

This composes the three prior layers with almost no new machinery: Stage C emits
the impl methods, bracket-generic codegen monomorphizes the body + maintains
`subst`, and the "Zig fix" synthesizes the per-instance `ImplCall`.

---

## ✅ Done — trait **Stage F** (`dyn` vtable) · the trait epic is complete

`dyn Trait` now works end-to-end: the receiver type is erased and dispatch goes
through a compiler-**synthesized vtable** that is byte-compatible with the
hand-written fn-pointer-field vtable from the first done section.

- **Representation:** `dyn Trait` lowers to a fat pointer `JestyrDyn_<Trait> =
  { void* data; const JestyrVtable_<Trait>* vtable; }`; the vtable is a struct with
  one function pointer per trait method (receiver erased to `void*`). Both are
  synthesized by `cgen::dyn_typedefs` for every trait used as `dyn`.
- **Per-impl vtables** (`cgen::dyn_vtables`): each `impl Trait for T` emits a shim
  per method (casting the erased `void* self` back to `T` — deref'd for a by-value
  `read self`) and a `static const` vtable instance `jestyr_vt_<Trait>__<T>` wired
  to the shims in trait-method order.
- **Coercion** (`typeck::record_dyn_coercion`, recorded in `dyn_coercions`): passing
  a concrete value where `dyn Trait` is expected — at a call argument, a `let`, or a
  `return` — verifies `impl Trait for <type>` and the backend builds the fat
  pointer. A type without the impl is an error. Scalars are placed in a fresh,
  block-scoped compound literal `&((T){v})` so the erased data has a **valid,
  non-dangling** address that outlives the call (a statement-expression temp would
  dangle); an aggregate's lvalue address is taken directly.
- **Dispatch** (`typeck::resolve_dyn_method` → `dyn_calls`; `cgen` in `emit_call`):
  `d.m(args)` lowers to `d.vtable->m(d.data, args)` — one function, the impl chosen
  at **run time** by the value's actual type (the dynamic counterpart to the
  monomorphizing "Zig fix").
- **Tests (teeth-verified by mutation):** typeck unit (dispatch typed + recorded,
  coercion recorded, missing-impl + non-trait-method errors), cgen unit (vtable
  struct/typedef/shim/instance + dispatch + coercion; one function → two vtables),
  property + determinism over `arb_dyn_program`, `fuzz_dyn_dispatch`, and the gcc
  round-trip `examples/dyn_dispatch.jtr` (`42/70/70` — one `describe`, three values).
  Suite **413 green**, warning-clean.
- **Scope notes:** dispatch is single-method-name (defaulted trait methods an impl
  omits aren't wired into the vtable yet); coercion needs an addressable source or a
  struct/scalar literal (a struct-returning-call passed *directly* as `dyn` would
  need a hoisted temp — bind it to a `let` first); non-object-safe traits (`Self` in
  a method's args) aren't rejected, just unsupported through `dyn`.

**Trait stages A–F are all done.** The next frontier is the broader roadmap
(`jestyr-design.md §19`) — toward self-hosting. The three OS-/stdlib gaps a compiler
can't run without are now **built** (each demo + gcc-oracle tested): **file I/O**
(`std/fs.jtr` + read_file/write_file/file_exists/remove_file intrinsics),
**command-line args** (`std/env.jtr` + `main(argc,argv)` capture), and a **symbol-table
map** (`std/strmap.jtr` — an open-addressing `str -> i64` table, the deterministic
alternative to a chaining `HashMap`). See ROADMAP workstream P. The remaining
self-hosting work is the lexer vertical slice → the ~27K-line port, plus per-module
namespaces (K) for comfort.

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

### Traits / interfaces — Stages A–F done (epic complete)
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
- **Bracket-generic codegen** (not a trait stage): `[T: Bound]` generics
  monomorphize — each `T` inferred from the value args (`unify_tp`, shared
  typeck↔cgen), a mangled instance emitted per instantiation.
- **Body-side bound enforcement (the "Zig fix"):** inside `f[T: Tr]`, `x.m()`
  resolves through the bound (`resolve_bound_method`), erroring on a non-bound
  method, and dispatches to the concrete `impl` per instance via
  `bound_method_calls` + the active `subst`. See the done-section / §5.12.
- **F — `dyn` vtable:** `dyn Trait` erases the receiver to a `{ data, vtable }` fat
  pointer; a concrete value coerces in (verifying the impl), and `d.m(args)`
  dispatches through the vtable slot at run time. Synthesized vtable structs +
  per-impl static instances, byte-compatible with the hand-written fn-pointer
  vtable. See the done-section / §5.12.
- **Remaining:** none — **the trait epic (A–F) is complete.** The frontier moves to
  the broader roadmap (`jestyr-design.md §19`); the self-hosting plumbing — file I/O,
  command-line args, and a symbol-table map (`std/{fs,env,strmap}.jtr`) — is now built
  (ROADMAP workstream P), leaving the lexer slice + the full port.

---

## Test posture

Suite is **413 green**, warning-clean, clean across the `dharht-experiment` and
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
