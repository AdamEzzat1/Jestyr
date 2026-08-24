# The port's monomorphization gaps — all four `jc_build_matrix` failures closed

A record of closing the four programs the self-hosted compiler could not build, and of the
thing that finding them turned up: **`BUILD_OK` was never evidence of correctness, and one of
the four proved it by compiling into a program that printed zeros.**

Everything here is PORT-ONLY (`examples/std/cgen.jtr`). The reference already did all of it;
each fix makes the port agree. Seed refreshed (42,017 → 42,212 lines).

---

## §0. WHAT CHANGED, IN ONE TABLE

| # | gap | observable | fix |
|---|---|---|---|
| 1 | `push_su_mangle` had no kind-7 (App) arm | `JestyrFn_fn_di32_ret_?` — a typedef name that cannot exist | mirror `emit_su_ty`'s App arm, minus its `Jestyr_` prefix |
| 2 | `collect_fn_types` pass 3 missing | fn-pointer typedefs NAMED in a prototype, never defined | walk `g.gfi`, emit each instance's fn-pointer params + return |
| 3 | `emit_su_tyid` had no Slice arm | `JestyrSlice_T` inside a monomorphized body | add it beside the Array arm |
| 4 | 5 body emitters used the unsubstituted renderer | same | `emit_ty_c` → `emit_su_tyid` at the slice-index, slice-for and array-for sites |
| 5 | `prim_jname` / `prim_code_c` stopped at code 15 | `JestyrResult_?` for a `String` result | extend both to 21, matching `typeck.prim_code` |
| 6 | `genum_item_of` required CONCRETE args | **an EMPTY function body** — a silent miscompile | `genum_item_of_su`: an opaque arg the substitution binds is concrete |

Numbers 1–5 were compile errors. **Number 6 was not**, and that is the important one.

---

## §1. THE FINDING: `BUILD_OK` IS NOT `CORRECT`

`jc_build_matrix_matches_expectations` records whether `jc build` produced a binary. It does
not run it.

After fixes 1–5, `combinators` moved FAIL → BUILD_OK and printed:

```
0 0 0 0 0 22 0 0 …          (the reference prints 1 20 7 42 21 22 99 0 …)
```

The port had emitted

```c
bool jestyr_opt_is_some__i32(Jestyr_Option__i32 j_o)
{
}
```

— an empty body. **gcc accepts a non-void function that falls off its end with a warning**,
so it compiled, linked, ran, and returned whatever was in the return register. Every
`Option`/`Result` combinator in `core` answered 0.

Had the matrix been regenerated at that moment it would have recorded `BUILD_OK combinators`
and the session would have reported "all four fixed". **The gate would have certified a
silent miscompile.**

This is the same shape as the recorded `JestyrArr_T_8` bug — *"a missing typedef is a link
error you cannot miss; a WRONG typedef that names something valid is a miscompile"* — with the
same moral one level up: **a gate that checks whether something builds certifies building,
and nothing else.**

### What was added

`jc_built_generics_run_the_same_as_the_reference` runs the `jc`-built binary and the
reference's for the four fixed programs and compares stdout and the exit code.

**`log_demo` and `str_demo` ride along as CONTROLS**, and they earned their place immediately:
the first version of this comparison reported all six as differing, including two programs
already known good. The cause was line endings, not the compiler. **A differential harness
needs a case whose answer you already know, or a broken harness reads as a broken compiler.**

Extending the comparison corpus-wide is the obvious follow-up and is deliberately not done —
it doubles the matrix's runtime, and these six are where the evidence is.

---

## §2. THE GAPS, AND WHY EACH SURVIVED

### §2.1 — Pass 3: monomorphized generic FUNCTIONS (fixes 1–2)

The reference's `collect_fn_types` has three passes: AST-written fn types, generic STRUCT
instances' fn-pointer fields, and **generic FUNCTION instances' fn-pointer parameters and
return**. The port had the first two.

So `core.opt_map(i32, i32, o, &f)` — a generic function whose parameter is `fn(T) -> U` — named
`JestyrFn_fn_di32_ret_i32` in its prototype and nothing defined it. Pass 1 skips `fn(T) -> U`
because it is not concrete; pass 2 only walks struct fields. `sync.mutex_with`'s
`op: fn(*mut T)` is the same shape, and `slice_algos` reaches it through `core`'s predicates.

Fix 1 is the mangler half: `push_su_mangle` fell through to the substitution-blind
`push_ast_mangle` for a type APPLICATION, so `f: fn(T) -> Option(U)` mangled
`JestyrFn_fn_di32_ret_?`. The reference spells it `JestyrFn_fn_di32_ret_Option__i32`.

Concreteness is screened in the port by testing the mangle for `?`, which is
`is_concrete` in the form this side can observe: a `?` means a tparam reached the mangle
unsubstituted, and a typedef under that name defines something no use can spell.

### §2.2 — The substituted renderer, again (fixes 3–4)

`emit_su_tyid` gained an Array arm when `JestyrArr_T_8` was fixed. It did not gain a Slice
arm, and the note beside that fix had said in as many words that **these renderers must be
checked AS A SET, because a type form handled by one and not the others is exactly this bug.**

`s[mid]` inside `core.sl_binary_search`'s monomorphized body emitted `JestyrSlice_T _s124`.
It fails LOUDLY where the array version failed silently, and the difference is worth keeping:
there is no `int` fallback for an undefined struct typedef, so a wrong slice element is a
compile error while a wrong array element was a 32-bit truncation of every `i64`.

Five call sites in body emitters were still reaching for `emit_ty_c` (the unsubstituted one):
three in the slice-index lowering, two in `emit_slice_for`, one in `emit_array_for`.
**`emit_su_tyid` falls through to `emit_ty_c` when no substitution is armed**, so every one of
these switches is a strict superset and cannot change emission outside an instance body —
which is what made them safe to change in a closure module.

### §2.3 — The primitive tables stopped at 15 (fix 5)

`typeck.prim_code` runs to 21 (`os_str` 16, `String` 17, `Builder` 18, `Cow` 19, `cptr` 21).
`cgen.prim_jname` (mangle) and `cgen.prim_code_c` (C type) both stopped at 15.

The observable was `let a = f()` where `f` is `-> String !{ E }`: an un-annotated `let` takes
its C type from the CHECKER, so that is the only path which renders it, and the port emitted
`JestyrResult_? j_a = …`. gcc parses `JestyrResult_` as an identifier and `?` as a conditional
— hence its otherwise baffling *"expected ':' before ';'"*.

**An earlier note recorded this as the port's INFERENCE gap. Measured, it is a mangle gap.**
Typeck agrees with the reference on the type — the whole-corpus typeck golden passes on
`try_read.jtr` — and only the emission was wrong. That is the second time in two sessions a
recorded mechanism turned out to be the wrong half (the `jc build` collision bug was recorded
backwards too). **Re-measure a recorded diagnosis before building on it.**

### §2.4 — The empty body (fix 6)

`genum_item_of` requires every type argument of a generic-enum instance to be CONCRETE. Inside
a template body the argument is the still-opaque `T`, so it answered -1, and `emit_match` took
its import-degenerate escape hatch — *"the reference diags and emits NOTHING"* — which is
correct for an unresolvable scrutinee and catastrophic for a resolvable one.

`genum_item_of_su` asks the same question from inside an instance: an opaque argument that the
active substitution BINDS is concrete, because an instance's bindings are concrete by
construction. Three variant-construction guards and the match scrutinee now use it.

The minimal reproduction is eleven lines and worth keeping:

```jtr
enum Maybe(T) { nothing, just(v: T) }
fn is_just(comptime T: type, read o: Maybe(T)) -> bool {
    match o { just(v) => true, nothing => false }
}
fn main() -> i32 { let a: Maybe(i32) = just(5)  print_bool(is_just(i32, a))  return 0 }
```

Reference: `true`. Port before the fix: `false`, with no diagnostic and no gcc error.

**Note the shape of the debugging**: each fix exposed the next, in the order compile-error →
compile-error → wrong-output → compile-error → correct. A chain like that is only walkable if
the check at each step is "does it behave", not "does it build" — which is §1 again.

---

## §3. WHAT THIS DID NOT FIX

* **The corpus-wide output comparison** (§1). Six programs are checked; 57 build.
* **`emit_gs_ty`**, the third renderer in the set, was not audited for the same missing arms.
  `emit_su_tyid` and `emit_su_ty` now both handle Slice; whether `emit_gs_ty` does was not
  checked, and the memory of the `JestyrArr_T_8` fix says all three must be.
* **Nested substitution in `genum_item_of_su`** — an argument like `Maybe(Vec(T))` is checked
  one level deep. No corpus file has the shape; a deeper one would answer -1 and emit nothing,
  which is the failure mode §2.4 just closed, so this is where it would come back.
* **Typedef ORDER** differs from the reference in the flattened build (the generic-enum
  definitions land in a different place). Valid C either way, and invisible to every golden,
  because the byte-identity goldens run with imports unresolved where no instances exist.
  That blind spot is worth knowing: **the goldens cannot see the flattened program at all.**
