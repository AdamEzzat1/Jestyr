# Jestyr self-hosting — P5 cgen + R2 fixpoint (cold-start handoff)

> The last two stretches of the port (ROADMAP workstream P). **P1/P2a lexer, P2 parser, P3
> typeck, and P4 escape are all DONE** — each written in Jestyr and cross-checked byte-for-byte
> against the Rust reference over the whole corpus (see `jestyr-selfhost-REMAINING.md` for the
> status table and the P2/P3/P4 handoffs for detail). What remains: **P5 cgen** (the giant,
> ~10.8K reference lines) then **R2** (the fixpoint that *proves* self-hosting).
>
> **Read alongside:** `src/cgen.rs` (the reference), `examples/std/typeck.jtr` + `escape.jtr`
> (the two consumer passes already built — copy their shape), `src/proptests.rs` `mod c_oracle`
> (the golden toolkit), `src/attest.rs` (the C-hash lever).

---

## Progress log

### Increment 1 — `hello.jtr` byte-identical (commit `eb50eac`, branch `claude/jestyr-p5-cgen-r2-ac4439`)

`examples/std/cgen.jtr` (a leaf consumer with `main`, built directly by the golden — no `_cli`
driver needed) + golden `jestyr_cgen_matches_reference` (`--features c-oracle`). Emits
`examples/hello.jtr`'s C byte-identical. **Findings the next session should NOT re-derive:**

- **cgen emits string literals VERBATIM.** `ExprKind::Str(l) => JSTR({l})` where `l = src[span]`
  (raw source text *with* quotes, no decode/re-encode). So the escape model composes trivially:
  a Jestyr source `"\\n"` → C `JSTR("\\n")` → gcc → literal backslash-n (2 bytes) in the *output*
  C; a Jestyr `"\n"` → a real C-source line break. **Jestyr and Rust share escape conventions**,
  so the prelude's `self.raw("…")` strings transcribe to `string_push(sb, "…")` ~1:1. (Earlier
  confusion was a bash heredoc mangling backslashes — always author `.jtr` with the Write tool.)
- **`out` is a reserved word in `.jtr`** → the string builder is named `sb`. (Same trap the P4
  session hit with `out`/`comptime`.)
- **The golden target has NO `#line` directives.** `rust_cgen_dump` uses the single-file
  `parse` + `typeck::check` path (not `module::load`), so `TypeInfo::debug` is empty and
  `mark_line` is a no-op. Don't port `#line` for the golden; it only appears via `module::load`
  (the `emit-c` CLI). `str::lines()` also strips trailing `\r`, so the Windows exe's CRLF stdout
  compares equal — no newline-mode fighting.
- **The empty-program section skeleton (blank-line bookkeeping), fully reconciled against the
  golden.** After `prelude` (ends `…arg}\n\n`), only these emit bytes for a program with no
  aggregates, in this order: `forward_types`→`\n`; `result_defs`→`JestyrResult_str` typedef+`\n`
  (the typedef is emitted UNCONDITIONALLY — it's `try_from_utf8`'s result type); `closure_types`
  →`\n`; `fn_protos`→proto+`\n`; `method_protos`→`\n`; `consts`→`\n`; `fn_defs`→sig+body+`\n`;
  `main_wrapper`. Every other section (`gen_forward_types`, `fn_type_typedefs`, struct/enum/slice/
  array/genref defs, `flush_def_capture`, `dyn_typedefs`, `extern_protos`, `impl_protos`,
  `dyn_vtables`, `spawn_runtime`, `closure_fns`, `method_defs`, `impl_defs`) is a guarded no-op
  there. `forward_types`/`closure_types`/`method_protos`/`consts` emit their trailing `\n`
  UNCONDITIONALLY — that's the 2/2/3/1 blank-gap pattern. cgen.jtr elides the guarded no-ops and
  hardcodes those bare `\n`s with a comment naming each reference section (generalize as needed).
- **Output mechanism:** accumulate into one `String sb`; at the end print `sb[0 .. len-1]` via
  `print_str` (which re-adds the one trailing `\n`) so stdout equals `cgen::emit`'s String exactly.
- **AST decode cheat-sheet (Jestyr flat arenas, from `parser.jtr`):** item kind 3 = Fn
  (`x,y`=name span, `a,b`=param slice in `iar`, `z`=ret `TypeId` (-1=unit), `w`=body Block ExprId);
  expr kinds 0=Int 2=Name 10=Call (`a`=callee, `x,y`=arg slice in `ar`) 23=Block (`x,y`=stmt slice
  in `sar`) 29=Str; stmt kinds 0=Let 1=Return (`a`=value, -1=bare) 2=Expr (`a`=expr). Iterate
  top-level items via `p.roots` → `p.it[iid]`.

### Increments 2–4 — a real function, string intrinsics, distinct types

Allowlist is now `{hello, bench_fib, eq_fold, distinct}`, all byte-identical (commits `2913bbc`
params/if/binops/calls, `1e9aefa` string intrinsics, `238c79e` let/var + casts + distinct).
What's ported in `cgen.jtr` now:
- **params** (`emit_params`, mirrors `params_str`): 7-tuples in `iar` at `it.a`, count `it.b`,
  layout `[comptime, conv, name_start, name_end, is_self, type_id, refine]`; conv 0=default 1=read
  2=mut 3=take 4=out (mut/out → `*`); self/comptime erased; empty → `void`.
- **binops** (`emit_expr` kind 4 → `(lhs OP rhs)`, `binop_c` codes 1..18) + **value-position
  names** (kind 2 → `j_<name>`) + **int literals** (`_`-stripped; `0b`→decimal still TODO).
- **calls** (`emit_call`): `intrinsic_helper(name)` maps print_*/str_eq/eq_fold → `jestyr_rt_*`,
  emitted as `<helper>(<args>)` via `emit_arg_list` (byte-identical to the reference's print arms
  + `emit_str_binop` for these arg counts); a plain user call → `jestyr_<name>(<args>)`.
- **depth-aware body/stmt/if** (`emit_body`/`emit_stmt`/`emit_return`/`emit_if`): brace at depth,
  stmts at depth+1, tail-as-return; `if` in stmt/return position → block form `if ((cond)) { … }
  [else …]`. **Drop-scope glue is still a no-op** (add when Drop types land — keeps output
  byte-identical until then).
- **`let`/`var`** (kind 0, **annotated only** — `<cty> j_<name> = <init>;`), **`as` casts**
  (kind 11 → `(Target)(inner)`), **distinct forward typedefs** (kind 1 → `typedef <base>
  Jestyr_<name>;` in `emit_forward_types`), and the **push-based type renderer `emit_c_ty`**
  (primitive + user Name types → `Jestyr_<name>`; `c_prim` returns `""` for non-prims so a user
  type is detected). Structural types (str/slice/ptr/App) are still TODO in `emit_c_ty`.

Whole-corpus probe: only the 4 allowlisted files match; **no free wins** — each next file needs
new constructs.

### Increment 5 — structs — DONE (`f442299`, allowlist now 6 files)

Implemented exactly the plan below; `compute.jtr` byte-identical and `copy_optin.jtr` fell out as
a **free win** (structs + Copy opt-in need nothing more). Landed in `cgen.jtr`: struct/union
**forward typedefs** (kind 4, `it.op` 0/1=struct 2=union), **`emit_struct_defs`** (mar 10-tuples
`[tag,pub,ns,ne,tyid,default,attr_s,attr_c,bits_s,bits_e]`, `};\n\n`; called before
`emit_result_defs` — matches capture/flush order with no by-value dep), **StructLit** (kind 16 →
`(Jestyr_X){ .j_f = v, … }`, source order; FieldInit kind 18 `(x,y)`=name `a`=value; defaults/
spread still TODO), **Field access** (kind 5 → `<base>.j_<name>`), and **inferred-`let` types**:
`read c: typeck.Checker` threaded through `emit_body`/`emit_stmt`/`emit_return`/`emit_if`, plus
`emit_ty_c(sb, src, c, tyid)` reading `c.et[init]` → `c.tys` TyData (Unit/Prim via `prim_code_c`/
Named → `Jestyr_<name>` from the `c.tdecl` row cols 0/1). `emit_expr` stays Checker-free.

### Increment 6 — literals + unary + the str family + size_of (`fa48b4f`, allowlist 10)

New in `cgen.jtr`: Float/Char/Bool/Null literals, Unary (op 1=neg `-`, 2=not `!`, 3=bitnot `~`,
4=ref `&`; Bool stores `op`=1/0 for true/false), the full prim C-type table (char→`uint32_t`,
str/os_str→`JestyrStr`, cstr→`const char*`, String/Builder/Cow→`Jestyr*`), and — the structural
change — **`emit_expr` now threads the Checker** for type-directed lowering: `expr_is_str(c,eid)`
(TyData kind 2, code 14) drives Field (`.len`/`.ptr`/`.cstr` are real C fields on a `str`) and
Index (`s[lo..hi]` → `jestyr_rt_substr(b, lo, hi)`; absent lo→`0`, open hi→`({b}).len`, inclusive
→`((hi) + 1)`). Intrinsics added: starts_with/ends_with/contains/find/trim/substr/count_cp/
count_graphemes (all `<helper>(<args>)`), and `size_of(T)` → `sizeof(<cty>)` (bare-Name type arg).
Allowlist 6→10: +`io.jtr` (str params), `str_ops.jtr`, `substr.jtr`, `union.jtr` (union defs
already worked via the kind-4 `op`==2 path; only size_of was missing).

### R2 fixpoint harness — STANDS (`5ba8f43`)

`Cargo.toml` gained `selfhost-fixpoint = ["c-oracle"]`. `selfhost_fixpoint_subset` (in
`mod c_oracle`, additionally `#[cfg(feature = "selfhost-fixpoint")]`): **jc1** = `build_exe`
of `cgen.jtr`; for every `CGEN_GOLDEN_ALLOWLIST` program, jc1 compiles it → C (CRLF-normalized),
gcc builds it (skip library modules — no `int main(` → no link), and the exe must produce
**exactly** the Rust-compiled program's stdout + exit code. 9 runnable programs green. Run:
`cargo test --features selfhost-fixpoint selfhost_fixpoint_subset`. The full jc2≡jc3 fixed point
reuses this scaffold once cgen.jtr can compile the compiler sources themselves.

### Increment 7 — assign + break/continue + the for family (`7564a6d`, allowlist 11)

Assign (kind 12, `assign_c` codes 1..9), plain break/continue, and `for` via a new threaded
`struct Cg { tmp }` (the reference's never-reset global temp counter, created once in
`emit_program` and threaded `mut` through the stmt group): infinite→`for (;;)`, conditional→
`while (cond)`, range→`<cty> _hi{n} = hi;` + `for (<cty> j_i = lo; j_i </<= _hi{n}; j_i++)`
(index cty = hi's numeric prim via `c.et`, else `size_t`; `_` binds `_i{n}`). lar header decode:
[4]=cond/bindcount, [5]=srccount, [6]=step, binds (conv,ns,ne) at +7, sources after.
+`tests_demo.jtr`. TODO in emit_for: zip/slice/str/array iteration, `step`, labels, loop-`else`.

### Increment 8 — the slice/pointer machinery (`82e651d`, allowlist 13)

+`loops.jtr` +`slices.jtr`. Slice typedefs (`collect_slices` analogue: `[]T` annotations +
`slice(T,…)` calls, deduped by a `|mangle|` marker string), slice indexing (bounds-check spill
`({ S _s{n}=…; assert(_ix{n} < .len); … })` + **refinement elision** — an exposed exclusive
`for i in 0..s.len` pushes `(ns,ne,rangeExprId)` onto `Cg.rf`; `index_in_range` mirrors the
reference), slice `for` iteration (`_s{n}` snapshot; `read` binds by value, `mut` binds a pointer
and joins `Cg.pp`), `.len`/`.ptr` bare fields, Ptr types `T*`, Deref `(*x)`, `unsafe{…}`/bare
`{…}` block statements, `invariant`→assert, the alloc/slice/free intrinsics, and `mut`/`out` args
by address (`&(arg)` — `emit_user_args` looks up the callee Fn item's param convs). `Cg` grew
`pp` (by-pointer name spans) + `rf` (refinement stack); per-fn `mut`/`out` params registered in
`emit_fn_defs`. Note: a `_hi{n}`/`_s{n}` temp is consumed even when elided-vs-not, so the global
counter must match the reference exactly — an off-by-one there desyncs every later temp.

### Increment 9 — arrays + consts (`5b143d7`, allowlist 14)

+`array_lit.jtr`. Array types `[N]T` → `JestyrArr_<mangle>_<N>` (both renderers + mangles).
`emit_array_defs` mirrors `collect_arrays`' scan ORDER — the Checker's `c.et` per-expr types
first (expr-arena order), then fn-sig annotations, deduped by a `|key|` marker. **The expr scan is
load-bearing:** `walk_items` infers a `const`'s value with NO expected type, so `const P: [5]i64 =
[2,3,5,7,11]` gives the literal its own `[5]i32` → both `JestyrArr_i32_5` and `JestyrArr_i64_5`
emit (the ghost typedef). ArrayLit (14) → `({ A _al{n}; _al{n}.a[i]=(v); … _al{n}; })`, array
index → `({ const A* _a{n}=&(base); assert(_ix<N); _a{n}->a[_ix]; })`, array `.len` →
`((size_t)N)` (base NOT emitted — it's a place), array `for` iterates in place by address. The
**consts section** is now real (`static const <cty> j_<name> = <value>;`), with an array-literal
value taking a brace init `{ { … } }` (a `static const` can't use a GNU statement-expression).
TODO consts: `@no_mangle` consts, `@section`, ArrayRepeat-valued consts.

### Increment 10 — error sets (`63a02e2`, allowlist 15)

+`errors.jtr`. Result type `T!` (Checker TyData **kind 5**, a=ok) → `JestyrResult_<mangle(ok)>` in
both renderers + mangles. `result_defs` emits one typedef per distinct fallible-fn ok type
(`typedef struct { bool is_err; <okcty> ok; int err; } JestyrResult_<m>;`, deduped, `str`
pre-seeded; a unit ok omits the `ok` field). **Fallible detection:** `it.e >= 0 && far[it.e] > 0`
(the `far` extras block at `it.e` is `[err_count, (ns,ne)…, req_count, …, ens_count, …]`).
`emit_fn_sig` returns the result struct when fallible (and such a fn always "returns a value").
**Whole-program error-tag map** `build_error_tags` → `Cg.etags` (src-span pairs, tag = idx+1,
first-seen order across all fns); `error_tag_of` linear-scans. `ok(v)`/`err(E)`/`unwrap(e)`/
`is_err(e)` intrinsics + the `?` Try (kind 8) stmt-expr `({ R _q{n}=base; if(_q{n}.is_err) return
(cur){.is_err=true,.err=_q{n}.err}; _q{n}.ok; })` — temp type = base's own Result type (`c.et`),
re-wrap = the current fn's result via `Cg.res_ok` (the live fallible-fn ok TypeId; **-2** sentinel
= not fallible, since -1 = unit is a valid ok type).

### Increment 11 — enums + flat match (`3a11126`, allowlist 19)

+`discriminants` +`shapes` +`recursion` +`rest_pat`. Enum forward typedefs + `emit_enum_defs`
(tag enum with `= discriminant` values, then `struct { tag; union { struct{…} v; } u; }` — the
union only when some variant has fields), variant construction (bare nullary Name — checked
BEFORE the local-name path — and payload Call `circle(2.0)` — checked before intrinsics),
enum→int casts `(T)((e).tag)`, and the flat tagged `switch (jm_{n}.tag)` match: case labels one
level in from the switch brace, payload Ident subpats project `<fcty> j_<b> = jm_{n}.u.<v>.j_<f>;`
(wildcard/`..` skip), binding-catch-all + `_` → `default:`, non-ret arms `break;`, exhaustive
ret-position closes `__builtin_unreachable();`. Infra: `find_variant_enum`/`variant_tuple` (the
reference's `variants` map as an `ear` 5-tuple scan), `expr_enum_row` (Named TyData → tdecl kind 1),
`emit_arm_body`. **Deferred match forms:** guarded (`emit_guarded_match` ordered if-chain — a C
switch can't stack same-tag cases), nested (`emit_nested_match` decision tree), niche (`Option(*T)`
null test), scalar scrutinee (int/char/bool ordered if-chain), or-patterns (stacked case labels),
struct-variant patterns, generic-enum instances. Targets: guards.jtr, nested_match.jtr,
match_check.jtr, exhaustive_check.jtr.

### Increments 12–14 — near-miss sweeps (`0d37d28`/`32b5246`/`64f7ce4`, allowlist 30)

The diff-ranked probe (rank every non-matching file by `Compare-Object` line count) is the
increment picker now — files unlock in small clusters:
- **12** (+refine, spread, layout, defaults): refined PARAMS seed the refinement stack
  (iar slot 6) so `s[i]` elides its check; struct-lit spread → `({ Jestyr_X jss_{n} = base;
  jss_{n}.j_f = v; … jss_{n}; })`; `@packed`/`@align(n)` → ` __attribute__((…))` between
  keyword and name; struct-lit fills omitted defaulted fields (member slot 5) in decl order.
- **13** (+mmio, try_utf8, container, extern_c): `extern "c"` protos section (bare names, no
  `restrict`) + bare-name calls w/ `&()` for mut/out (item kind 8); `@volatile` field prefix +
  `@address(0x…)` → `((void*)(addr))` (Attr callee, expr kind 21); `bytes(s)` `_bv{n}` stmt-expr
  + `try_from_utf8` `_u` conditional (fixed `_u`, no counter); `realloc_i32`; return-position
  `unsafe{…}`/`{…}` blocks.
- **14** (+bitfields, reflect, contracts): `: N` bit widths (member slots 8/9 span);
  `align_of`→`_Alignof`, `offset_of`→`offsetof(cty, j_f)`; `requires` asserts at fn-body top
  (armed per fn, consumed by the FIRST emit_body — nested blocks see an empty list), `ensures`
  → `j_result` spill + asserts + `return j_result;`. Cg now: tmp/pp/rf/res_ok/etags/req/ens/retty.

**Deferred with reasons:** FString needs the parser to give interps real sub-exprs (parser.jtr
kind 22 is span-only; typeck golden shims skip interp Names — a parser+typeck+cgen change);
records.jtr = struct METHODS (protos/defs + `p.m()` sugar via typeck `mcalls`) — the next
medium chunk; guards.jtr = guarded match + GENERIC-ENUM instances (Option(i32) mono).

### Increments 15–16 — struct methods; guarded match + generic-enum instances (`a53fc00`, `9ec6abe`, allowlist 33)

**15 — struct methods** (+`records`, `docs`). Resolution is structural: a Call whose callee is a
Field types the receiver via the Checker (`expr_struct_row`: Named TyData → tdecl kind 0), finds
the struct item, and scans its `mar` **tag-1** members for the method's fn item. Instances collect
into `Cg.mi` as (struct item, method fn item) pairs by expr-arena scan (creation order ≈ body
order in one file). Sections: protos in the `method_protos` slot, defs after the free fns;
`emit_method_sig` renders `self` as a real first param (`Jestyr_<S>[* restrict] j_self`). Bodies
set `Cg.self_ptr` so SelfValue (kind 19) → `j_self` / `(*j_self)`. Call sites emit
`jestyr_<S>_<m>(recv, args)` with `&(recv)` for a `mut`/`out self` and **param 0 = self** offsetting
the arg-convention lookup.

**16 — guarded match + generic-enum instances** (+`guards`). The first monomorphization step:
`collect_genum_instances` scans the Checker's per-expr types for kind-6 TyData whose ctor names an
enum item with all-concrete args (deduped by rendered C name, expression order — mirroring
`collect_enum_instances`' `expr_types` pass; the reference's follow-up fn-signature pass is
deferred). Kind 6 renders `Jestyr_<ctor>__<arg mangles>` in **both** renderers, and AST App (kind 7)
matches so annotations and values agree. `emit_gen_forward_types` emits a **conditional** trailing
blank (unlike `forward_types`'). `emit_gen_enum_defs` substitutes payload field types through the
enum's tparam→arg map (`emit_field_ty_subst`). Variant construction picks the instance name from
the construct expression's own type (`emit_gvariant_head`). For the match: `emit_match` resolves a
shared `pfx` (plain `Jestyr_<E>` or the instance name) used as both the scrutinee C type and the
tag prefix, and dispatches to `emit_guarded_match` when **any** arm has a guard — an ordered
if-chain that re-tests the tag per arm so a false guard falls through to the next arm; statement
position `goto`s a shared `jm_end_{n+1}`, return position closes `__builtin_unreachable()` unless
an unguarded catch-all exists. **It consumes TWO temps** (scrutinee + end label) exactly like the
reference — the global counter must stay in lockstep or every later temp desyncs.

### Increments 17–19 — owned strings; alloc/utf8/str-bytes; f-strings (allowlist 41)

**17 — String/Cow/Builder intrinsics** (+`builder`, `cow`, `os_str`, `owned_string`). A second
intrinsic table `intrinsic_helper_byref` for the mutating owned-buffer ops that take arg 0 by
address (`string_push/view/free`, `cow_free`, `builder_push/build/free` → `<helper>(&<a0>, …)`);
by-value additions `to_str_lossy`/`string_from`/`cow_borrow`/`cow_to_mut`/`cow_view`/
`cow_is_owned`; zero-arg `string_new`/`builder_new`; `os_from_bytes` (the `__typeof__` stmt-expr,
consumes NO temp). String `.len` is a bare C field via `expr_is_prim` — **String is prim code 17**
(14 str, 15 cstr, 16 os_str, 18 Builder, 19 Cow).

**18 — alloc/utf8 + str bytes** (+`strings`, `utf8_validate`, `slice_utf8`). `alloc(T,n)`/
`realloc(T,p,n)` via the type-arg renderer; `is_utf8`/`from_utf8` spill into a FIXED `_u` (no temp
consumed) — check vs assert-then-view; `bytes(s)` → `JestyrSlice_u8` view (consumes `_bv{n}`);
`count_codepoints`; scalar str index `s[i]` → `((uint8_t)(<s>).ptr[(<i>)])`; `for b in s` byte
iteration (`emit_str_for`: `JestyrStr _str{n}` snapshot, `uint8_t j_b = (uint8_t)_str{n}.ptr[_k{n}];`).

**19 — f-strings WITHOUT a parser change** (+`fstring`). The kind-22 node is span-only, so
`emit_fstring` re-scans the token text from src (the parser-dump trick) into the reference's
`({ JestyrString _fs{n} = jestyr_rt_str_new(); …; _fs{n}; })` push chain. Interp dispatch by the
name's type — str push / String via `str_view(&…)` / bool ternary / decimal `str_push_i64` — with
names resolved against the current fn's params + lets (`Cg.curfn`, set around each fn/method
body). Bare-name interps only; a general sub-expression interp still needs parser support.

### Increments 20–21 — vec fixes; scalar match + or-patterns (allowlist 44)

**20** (+`vec`): `0b`/`0B` binary literals convert to decimal in `emit_int_literal` (C has no
binary literals); `fn_is_template` (bracket generics via `it.h`, or ANY comptime param) now gates
`emit_fn_protos`/`emit_fn_defs` so a comptime type-fn (`fn Vec(comptime T: type) -> type`) no
longer leaks a bogus `void jestyr_Vec(void)`.

**21** (+`orpat`, `ranges`): `emit_scalar_match` — an int/char/bool scrutinee (Checker prim code
≤9, 12, 13) dispatches on the VALUE via an ordered if-chain sharing `emit_guarded_arm`, TWO temps.
`scalar_pat_cond` writes the test (lit equality, range bounds, or-pattern ORs parenthesized
alternatives; an everything-matching alternative makes the arm unconditional; wildcard/binding =
none). Or-patterns of nullary variants in the unguarded tag switch → stacked `case` labels.

### Increment 22 — RAII drop glue + concrete trait-impl sections (`ebe1745`, allowlist 46)

**+`drop`, `drop_nested`** (cluster 1 of the consolidated worklist). What landed, and the
findings the next session should NOT re-derive:

- **The drop model is name-driven in the port.** The reference walks `Ty` values; cgen.jtr
  canonicalizes every droppable to its *type-name span* (annotation Name span, or the Checker's
  kind-15 `tdecl` row name) and recurses by AST item lookup (`find_named_item`). Sound because
  only concrete Named types can need drop in the current corpus (blanket generic `Drop` impls —
  vec_alloc/strmap/intern — wait for the generics cluster). The `is_copy` short-circuit is
  omitted as inert: a `@copy` aggregate can't own droppables.
- **`emit_drop_place` emits FLAT at statement depth** — the reference's `self.line` never changes
  depth inside the walker, so the enum-payload `switch`/`case`/`break;`/`}` lines all share the
  enclosing statement's indentation. Struct fields recurse in reverse decl order; unions never
  auto-drop; indirection (Ptr/GenRef/RegionRef) is never followed.
- **`cur_moved` lifecycle:** ONLY `emit_fn` collects the move set (2003 clears it) — struct-method
  and concrete-impl bodies run with it EMPTY in the reference, so the port collects in
  `emit_fn_defs` alone. `collect_moved` marks bare-name let-inits/returns/tails, `take`-conv args
  of resolvable *free* calls (comptime-type param slots skipped), struct-lit field values, and
  assign values. Take-`self` receiver moves via icalls/mcalls are deferred (no droppable+take-self
  file yet).
- **Spilled returns:** `return <v>` with ensures OR live drops spills `<retcty> j_result = v;`,
  asserts, drops, `return j_result;`. In a method/impl body the reference's `cur_ret_cty` is unset
  → `__auto_type` — ported as `Cg.autoret`. A bare `return;` runs `emit_all_drops` first.
  `block_diverges` = ret-position tail or trailing explicit Return → scope discarded, else the
  fall-through scope-exit emits drops (reverse decl order) before the closing brace.
- **Impl sections:** `emit_impl_protos` (after method_protos; **CONDITIONAL** trailing blank —
  unlike method_protos') and `emit_impl_defs` (after method_defs) lower each concrete
  (`it.h <= 0`) impl method as `jestyr_impl_<Trait>__<TypeKey>__<method>` with `self` as a real
  first param (`* restrict` for `mut`/`out self`); a Name target's text IS the type key.
  Fallible impl methods emit nothing (reference parity). Impl item decode: kind 7, `(x,y)`=trait
  span, `a`=target TypeId, `(b,z)`=method fn-ItemId slice in `mar`, `h`=gen_count.
- Drop glue consumes NO temps — the counter stays in lockstep for free.

### Increments 23–26 — genrefs; loop-else/labels; regions + zip/step/variant; codepoints (allowlist 52)

**23 — genrefs** (`6e93671`, +`genref`): GenRef arms in BOTH renderers (AST kind 5 / Checker
kind 10 → `JestyrRef_<elem>`, nested mangle `ref_<elem>`); `emit_genref_defs` mirrors
`collect_genrefs` (type-arena `&T` pass in arena order, then `gen_new` calls in expr order,
deduped, CONDITIONAL blank) between array typedefs and result defs; `gen_new`/`gen_free`
(ONE temp each); checked deref (`assert(((uint64_t*)_r{n}.ptr)[-1] == _r{n}.gen)`, ONE temp).

**24 — loop-else + labels** (`c1a7af6`, +`loops_else`): `emit_for` is now the wrapper/inner
split. Label span = `lar[0,1]`, else Block = `e.x`. A labeled loop arms `Cg.cl_*` (the
continue target), consumed by the FIRST loop body that closes — the reference `.take()` quirk,
mirrored. An unlabeled loop with an else takes `_fe{n}` (ONE temp, BEFORE the head). Plain
`break`s reroute via `Cg.bl_*` ONLY when an else exists (saved/restored per loop). Else block
= stmts at depth+1 with NO new drop scope; `<label>__break: ;` lands after it (a label-only
loop still gets its target). `emit_loop_body` (drop scope + `__continue`) backs
infinite/while/range; slice/str/array consume the cont target inline.

**25 — regions/arenas + zip/step/variant** (`f2070e6`, +`region`, `region_string`,
`loops_advanced`): arena prelude gated on `uses_arena` (region blocks / region-loops /
`arena_*` calls); `emit_region` (arena open, body, **DISCARDED** drop scope — region bulk
drop — arena free); a region-scoped `for` wraps loop+else+break-target in an arena scope at
depth+1 and arms `Cg.sr_*` (the scratch reset, consumed at the next body top — wired into all
FIVE body shapes). `&[r]T` = plain pointer in both renderers. Intrinsics: `region_alloc`/
`region_str`/`region_concat`/`arena_open`/`arena_close` (ONE temp each), `arena_alloc` (none).
Zip: `emit_zip_for`, ONE shared temp, `_z{n}_{i}` snapshots + length assert; binds take their
own source's elem. Stepped ranges: negative-literal step → `int64_t` + flipped compare, `+=`
increment, NO refinement. `variant`: per-loop hoisted `int64_t _vt{n} = INT64_MAX;` (ONE temp
each, hoisted AFTER region open and BEFORE the `_fe` temp — order is load-bearing) on a
floor-masked flat list (`Cg.vt`/`vtf` models the reference's per-loop map REPLACE), and the
checked-decrease stmt-expr.

**26 — codepoints** (`505f8c7`, +`codepoints`): the `codepoints(s)` Call marker is recognized
right after the Range check (dispatch order matters); `_str{n}` + manual `_k{n}` cursor +
`while` + `jestyr_rt_decode_cp` (ONE temp), `(void)`-discarded for `_`, optional byte-offset
second binding (= `_k` BEFORE the decode).

**Roadmap clusters 1–5 are DONE.** Next up (leverage order): **generic FUNCTIONS** (cluster 6,
the hardest infra — `collect_all_instances` worklist, `mangle`, `make_subst`,
`emit_generic_call`, monomorphized sigs feeding slice/array instances), then nested/niche
match, closures, traits/dyn, concurrency, fn-ptr types, test mode.

### Increment 27 — generic-fn monomorphization SLICE 1 (`108075c`, allowlist 54)

**+`bracket_generic`, +`generic` (free win).** The first cut of cluster 6. What's in:
`fn_is_gen` (brackets OR comptime-TYPE params — mirrors the reference's `generics` set;
comptime-VALUE templates never instantiate), instance records as (kind,a,b) type-arg triples
(0 = explicit bare-Name comptime arg span / 1 = inferred Checker TyId / 2 = unresolved → `x`),
`collect_gfn_instances` (scan non-template fn + concrete impl-method bodies, **LIFO drain — the
reference pops a stack, instances land in REVERSE discovery order**, mangle-dedup),
`gcall_targs` (comptime targs first, then brackets unified from bare-Name param annotations vs
arg Checker types, first bind wins), `emit_su_ty` + `Cg.su` (subst-aware Name rendering — SIG
use only), instance protos inside `fn_protos` / defs at the end of `fn_defs` (full emit_fn
wiring incl. collect_moved), and `emit_generic_call` (comptime args erased, mut/out by `&`).
**Deferred (needed for `generics.jtr` at 99 diff lines, vec_alloc, list/tokens):** walking
INSTANCE bodies for nested generic calls (a generic calling a generic), structural unification
(`[]T`/`*T` patterns), template-body `let`s under subst (`emit_c_ty`/`emit_ty_c` are not
subst-aware inside bodies — the reference threads `self.subst` through `c_ty_ast`/`c_type`),
monomorphized sigs contributing slice/array instances, generic-struct METHODS (`Work::Method`),
`qualified`/`call_sym` resolution. Trap fixed en route: a `for k < n` loop missing its
`k = k + 1` hung on every impl file — Jestyr `for` is while-form; the corpus probe catches
it as a timeout.

### Increments 28–30 — near-miss sweeps: dispatch records, I/O, arrays (allowlist 62)

**A correction first: `generics.jtr` does NOT exist** — the roadmap's cluster-6 target list was
speculative; the **diff-ranked probe is the real worklist** (rank every import-free non-matching
file by `diff | wc -l`; import-driver files like `try_read.jtr`/`demo.jtr` are golden
non-targets — the single-file path degenerates their module-qualified calls on BOTH sides).

**28** (`69dcbdb`, +`unsafe_init`, `env`, `bound_method`, `traits_static`): VALUE-position
`unsafe { E }`/`{ E }` = the single tail expr (B4); `arg_count`/`arg` intrinsics (the `arg`
form casts `(int64_t)(…)`); **impl-method CALL dispatch** — an `icalls`-recorded `recv.m(args)`
lowers to `jestyr_impl_<Trait>__<Key>__<method>(recv, …)`, key from the receiver's Checker type
(Named name / prim jname); **bound dispatch** — typeck.jtr's `resolve_bound_method` now records
into a new `pub bcalls` arena (5-tuples: call id, bound span, tparam span), and cgen resolves
the key through the ACTIVE substitution (`g.su`), so a template's `x.show()` dispatches per
instance.

**29** (`4f9a4f1`, +`operators`, `fs`, `str_iter`): **operator-trait dispatch** — typeck.jtr's
`resolve_operator` records into `icalls` keyed on the BINARY ExprId (the impl decl's own
trait/method spans spell "Add"/"add"); cgen's Binary arm calls `emit_operator_call` (swap for
`>`/`<=`, negate for `!=`/`<=`/`>=`; `find_impl_method_item` factored + shared). File I/O:
`read_file`/`write_file`/`file_exists`/`remove_file` helper-table entries; `try_read_file` =
the B3 stmt-expr (FIXED `_s`/`_ok`, no counter) + prelude helper + `JestyrResult_String`
typedef gated on `uses_try_read` (seeds result_defs' dedup). Split iteration:
`emit_split_for` (ONE temp; `_ss`/`_sep`/`_start`/`_go` scan loop).

**30** (`92826a5`, +`arrays`): ArrayRepeat `[v; N]` fill stmt-expr; array-index ASSIGN folds
into its own stmt-expr (non-const ptr). **Temp-order trap (recurring!): the reference emits
subexpressions (base/index/VALUE) BEFORE taking the enclosing form's temp** — `xs[i] = xs[i]+1`
numbers the READ first. Port such forms by buffering subexpr emission into Strings, then taking
the temp (split/repeat/index-write do this; the older emitters take the temp first and only
match because their sources are temp-free so far — fix each when a file exposes it).

**Next up (ranked):** fn-pointer types (cluster 11 — unlocks `vec_alloc` 22, `alloc_vtable`
26, `gen_vtable` 29, `fn_ptr` 55), closures (`closure_run` 28), concurrency (`dynamic_spawn`
22, `concurrent` 33, `await` 36, `atomics` 41), `mem.jtr` 33, `attributes.jtr` 38 (fn attr
prefixes + @no_mangle bare names), `option` 39/`niche` 59, `nested_match` 47,
`struct_variant` 49; then the generics chain `genlist` 70 → `vec_generic` 82 → `list.jtr` 95
(the first compiler-source dependency, needs nested-generic instance walking + body substs).

### Increment 31 — fn-pointer types (`4df4411`, allowlist 65)

**+`vec_alloc`, `alloc_vtable`, and `mem.jtr` as a FREE WIN** (the arena-allocator std module —
a compiler-source dependency). Cluster 11's core: Fn types render as `JestyrFn_<sig>` in BOTH
renderers (AST kind 9: `(op,a)`=(conv,TypeId) pairs in `tar`, `b`=ret_conv, `x`=ret; Checker
kind 12: `(a,b)`=(conv,TyId) pairs in `tya`, `x`=ret) with the conv-tagged mangle
`fn_<tag><ty>_…_ret_<ty>` (tags d/r/m/t/o via the shared `push_conv_tag`). `emit_fn_type_defs`
scans the type arena in order (nested fn types have smaller ids → inner typedefs first),
deduped, emitted between gen-forward-types and struct defs; a `mut`/`out` param renders `T*`
(NO restrict — a real fn's ABI, so a pointer can hold `&some_fn`); empty params → `void`.
`&some_fn` (Unary ref of a Name that resolves to a fn item) → `(&jestyr_<name>)`. A call whose
callee's CHECKER type is kind 12 is an INDIRECT call — the disambiguate-by-field-type rule:
callee emitted plainly, args by `&` for the fn TYPE's `mut`/`out` params — placed by TYPE
before the Name/intrinsic dispatch, exactly like `emit_fn_ptr_invoke`. Deferred: generic
placeholders (`fn(T)->T`) + monomorphized-signature typedef contributions.
`fn_ptr.jtr` (10 lines) and `gen_vtable.jtr` (24) now wait ONLY on closures (capture-free
`jestyr_lam_<id>` + fn-ptr coercion) and generic-struct instance defs.

### Increment 32 — closures, thin + fat (allowlist 67)

**+`fn_ptr` (thin coerced closure) and `closure_run` (fat lambda-lifted closures).** The full
`collect_closures`/`closure_types`/`closure_fns`/`emit_closure_literal`/`emit_closure_invoke`
family, PLUS the discovery this increment forced — record it, it's load-bearing forever:

- **The two parsers' ExprIds do NOT match, and the reference's C embeds ITS ids** in every
  lifted name (`jestyr_lam_<id>`, `JestyrEnv_<id>`, `JestyrClosure_<id>`, and the inline-invoke
  spill's struct name). The Rust AST stores fn/method/impl **bodies**, if-**then** blocks, for
  **body/else** blocks, unsafe/concurrent/region **inner** blocks as `Block` STRUCTS (no
  ExprId), `FieldInit`s as plain structs, and struct-lit **path** / gen-lit **ctor** as
  `Ident`s — the Jestyr flat-arena parser allocates a real expr node for each, so every such
  node shifts all later Jestyr ids +1. `ref_expr_id(p, eid)` maps back by counting Jestyr-only
  nodes below `eid` (O(arena) per call; closures are rare). The P2 parser golden never saw this
  — it compares structure, not indices. **Known future liabilities** (documented in the
  mapper's comment, none co-occur with closures in today's corpus): f-string interps are
  RUST-only nodes (offset would go the other way), select-arm blocks, and whatever orphan the
  `Box(i32){…}` ctor parse leaves.
- **Thin vs fat is the Checker's call:** typeck.jtr already stamps a closure used where a
  `fn(…)` is expected with that Fn type (TyData kind 12); otherwise Opaque("closure") (kind
  18, x=0). `closure_is_thin` just reads the stamp. Thin → bare `static` fn whose sig comes
  from the **fn TYPE** (`emit_thin_closure_c`: params from `c.tya` conv/ty pairs, names from
  the closure's `clar` triples else `_p{i}`, mut/out → `T*` + registered in `g.pp`) and the
  literal is `(&jestyr_lam_<id>)`. Fat → env struct (`char _unused;` when capture-free) +
  `{call, env}` typedefs in the **closure_types** slot (thin closures SKIPPED, trailing blank
  UNCONDITIONAL), lifted fn taking `JestyrEnv_<id>* j__env` in the **closure_fns** slot
  (between consts and fn_defs).
- **Captures = free-name walk with deliberate holes.** `crefs_expr` mirrors `collect_refs`
  EXACTLY — no Cast/Range/Region arms (names appearing only there are never captured; porting
  the holes is part of byte-identity). Filter: closure's own params (by text), then
  `is_global_cname` (fn items, named types, variants, consts, the full `is_intrinsic` name
  set as a marker string), dedup by text, first-ref order; the capture's env-field C type is
  the FIRST referencing expr's Checker type (`c.et[rid]`). Discovery walk (`cfind_expr`) is
  preorder — a closure pushes itself, then its body walks — over non-generic fn bodies, const
  values, and non-generic struct-method bodies (impl-method bodies deliberately NOT walked,
  like the reference).
- **Body emission:** Block/Unsafe body → `emit_body` at depth 0; any other body wraps in
  `{`/`}` with `emit_return` (or If/Match/bare-stmt when the C ret is `void` — compare the
  RENDERED ret string, not the TyData kind). Captured names render `(j__env->j_<name>)` via
  the ACTIVE `g.caps` span set, checked FIRST in the Name arm. Per-lam state reset mirrors the
  reference: su/pp/req/ens cleared, res_ok=-2, caps armed (thin also clears caps, sets pp).
- **Invoke:** callee's Checker type kind 18/x=0 → closure invoke, checked BEFORE the kind-12
  fn-ptr invoke. Args buffer FIRST (temp order!), then a Name callee →
  `j_f.call(&j_f.env, args)`; any other callee spills `({ JestyrClosure_<id> _f{n} = <lit>;
  _f{n}.call(&_f{n}.env, args); })` — ONE temp. A closure-inited inferred `let` types as
  `JestyrClosure_<id>` (reference emit_stmt special case).
- `gen_vtable.jtr` (22 diff lines) now waits ONLY on generic-struct instance defs (the
  comptime-ctor `Box(i32)` chain). Pre-existing, NOT from this increment: the port segfaults
  on `numbers.jtr`/`numerics_canary.jtr` (never targets — verified against the increment-31
  build).

### Increment 33 — generic-STRUCT instances (allowlist 68)

**+`gen_vtable`** (the Main-Objective generic-vtable shape: `fn Box(comptime T: type) -> type
{ return struct { op: fn(T) -> T } }`). What landed:

- **`ref_expr_id` correction:** a GenStructLit's ctor is NOT a Jestyr-only node — the Rust
  parser reaches `Name(args){` through the call-postfix path, so it allocates-and-ABANDONS the
  same orphan `Name` this parser does (unlike a plain StructLit's path, which stays an Ident).
  Only kind 16 subtracts.
- **Instance collection** (`collect_gstruct_instances` → `g.gsi`): kind-6 per-expr Checker
  types whose ctor is a comptime struct ctor (`gstruct_ctor_fn`: a fn item whose body has
  `return struct {…}` — `ctor_struct_node` finds the kind-36 StructType expr), expr order,
  dedup by rendered C name. The reference's annotation/signature pass is deferred (as the
  enum collection's was).
- **Substitution machinery**: `gs_tparam_arg` zips the ctor fn's `comptime X: type` params
  (declaration order) with the instance's `tya` args; `emit_gs_ty`/`push_gs_mangle` render an
  AST type under that substitution (a substituted Name renders/mangles its ARG's Checker
  form; Fn types name their SUBSTITUTED typedef).
- **Sections**: `gen_forward_types` emits struct instances BEFORE enum instances;
  `emit_gen_struct_defs` (fields only, tag 0) lands between plain-enum defs and gen-enum
  defs; `emit_fn_type_defs` (now takes c+g) gained BOTH halves of `collect_fn_types` parity —
  the textual scan skips NON-CONCRETE fn types (`ast_ty_concrete`: every Name a prim or
  declared type — a template's bare `T` is neither), and each struct instance contributes the
  substituted fn-pointer typedef of its fn-typed fields (instance order, member order, same
  dedup, shared trailing blank).
- **GenStructLit (kind 17)** emits `(Jestyr_<ctor>__<args>){ .j_f = v, … }` — the type name
  from the expr's OWN Checker type, fields in source order (no spread/defaults, like the
  reference arm).

### Increment 34 — spawn runtime + atomics (allowlist 72)

**+`dynamic_spawn`, and `concurrent.jtr` + `parallel.jtr` as FREE WINS** (parallel.jtr is a
compiler-adjacent std module), **+`atomics`** (a 5-intrinsic rider). The concurrency core:

- **`collect_spawns`** (`g.sps` = inner-call ExprIds): non-generic fn bodies + ALL struct
  methods (the reference applies no generic filter to methods), narrow arm walk
  (Concurrent/Block/Unsafe/Spawn/If/Match/For; Select deferred). Gates the prelude's
  `#include <pthread.h>` (after assert.h, inside the include run).
- **`spawn_runtime` section** (between impl_protos and consts): per site
  `struct _jsp_<id> { <cty> a<i>; … [<ret> ret;] };` + the `void*` trampoline
  `jestyr_task_<id>` unpacking and calling `jestyr_<target>` (`_a->ret =` for a non-void
  target; `char _unused;` when empty). **`<id>` is the inner call's REFERENCE ExprId** —
  `ref_expr_id` again.
- **`emit_concurrent`** (stmt kind 38): plain braces (NO drop scope), fixed top-level bare
  spawns get numbered handles (`pthread_t _jt<h>;` + arg box + create, args buffered FIRST)
  joined at the brace; `stmt_has_nested_spawn` (a spawn NOT at top level) arms the dynamic-N
  path: the growable `_dt/_da/_dn/_dc` array decl, `g.dspawn` making a Spawn STATEMENT lower
  via `emit_dyn_spawn` (grow-on-full, heap arg box — a stable address across array reallocs —
  create, count), and the brace's join-all + frees. **Deferred:** `let h = spawn …` handles +
  `await` (await.jtr is at 27 diff lines), `select`.
- **Atomics rider:** `atomic_store/load/add/sub/xchg` → `__atomic_*_n`/`__atomic_fetch_*`
  with the `(int64_t*)(p), (int64_t)(v), __ATOMIC_SEQ_CST` cast pattern (no stdatomic.h).

### NEXT increments — everything still remaining (the session-22+ worklist)

> **Superseded summary:** the CURRENT consolidated worklist (post-increment-21, allowlist
> 44/130) lives in `jestyr-selfhost-and-backend-roadmap.md` Horizon 1 — use that first; the
> list below is kept for its per-construct reference pointers. Done since it was written:
> ArrayRepeat still pending, but FString, Try, error sets, enums + flat/guarded/scalar match,
> or-patterns, struct methods, generic-enum instances, String/Cow/Builder, alloc/utf8,
> template suppression all landed (increments 9–21).

Grow the allowlist file-by-file (`DUMP_DIVERGE=1` to converge;
probe the whole corpus after each construct — files unlock in clusters). Expr kinds handled so
far: 0 Int, 1 Float, 2 Name, 3 Unary, 4 Binary, 5 Field, 6 Index (str-range + slice), 7 Deref,
10 Call, 11 Cast, 12 Assign, 13 Range (str-index), 16 StructLit, 23 Block, 24 If, 25 Unsafe,
26 Char, 27 Bool, 29 Str, 30 Null, 31 For (range/while/infinite/slice), 32 Break, 33 Continue,
34 Invariant. Still remaining, roughly in order of leverage:

- ~~Deref/slice-Index/unsafe/alloc-slice-free~~ (incr 8), ~~ArrayLit (14) + arrays + consts~~
  (incr 9) DONE. Still to do: **ArrayRepeat (15)** `[v; N]` (`({ A _ar{n}; E _v{n}=(v); for(…)
  _ar{n}.a[_k]=_v{n}; _ar{n}; })`).
- **Range (13) outside str-index**, **FString (22)** (statement-expr `_fs{n}` push chain — see
  the fstring.jtr near-miss diff). ~~Try (8) `?`~~ + ~~error sets~~ DONE (incr 10).
- ~~Error sets~~ DONE (incr 10). (Original note kept for reference:) fallible fns → per-ok-type result structs in `result_defs`, `ok`/`err`/
  `unwrap`/`is_err`/`?` lowering (errors.jtr).
- **Enums + `match`** (LARGE): enum defs (tag enum + struct+union payload — see the shapes.jtr
  near-miss diff for the exact shape), variant construction, discriminants, `emit_scalar_match`/
  tagged-union match first, niche/nested/guarded later.
- **Traits/impls/`dyn`** (LARGE): methods (`self`), impl dispatch, operator traits (needs
  typeck.jtr to expose `impl_calls` per-expr — port the reference's `TypeInfo.impl_calls` into
  the Checker as a flat arena FIRST), dyn vtables/fat pointers.
- **Generics/monomorphization** (LARGE, the hardest infra): `collect_all_instances` whole-program
  walk → per-instance fn/struct/enum emission + `mangle` (`Jestyr_<name>__<types>`).
- **RAII drop glue** (medium-large): `needs_drop`/`emit_all_drops`/`emit_drop_place` +
  `collect_moved` move analysis — currently a deliberate no-op in cgen.jtr's `emit_body`.
- **Closures** (lambda-lifting: `collect_closures`, `JestyrEnv_<id>`/`JestyrClosure_<id>`/
  `jestyr_lam_<id>`), **Concurrency** (Concurrent 38/Spawn 39/Await 40/ParFor 41/Select 42 →
  pthread; `spawn_runtime`, `collect_spawns`), **Regions (43)** (arena prelude block is gated on
  use — remember to emit it when `uses_arena`), **consts section**, **extern "c"**, **contracts**
  (`requires`/`ensures` asserts), **@-attributes** (fn attr prefix, struct attr), **def-capture
  topological ordering** (only needed when a by-value field dep forces a reorder), **test mode**
  (`emit_tests`, the `jestyrc test` harness main).
- **typeck side-tables to expose as needed** (add `pub` flat arenas to typeck.jtr right before
  the construct that reads them, like P4 did): `impl_calls`(exists: `icalls`)/`method_calls`
  (exists: `mcalls`)/`dyn_coercions`/`call_sym`/`qualified` (module-qualified consts)/closure
  index/niche info.
- **R2 full fixpoint**: grow `selfhost_fixpoint_subset` alongside the allowlist (it consumes it
  automatically). The jc2≡jc3 step needs cgen.jtr to compile parser.jtr+typeck.jtr+escape.jtr+
  cgen.jtr — i.e. most of the list above (imports/modules are the extra wrinkle: the golden path
  is single-file; the compiler sources are multi-module, so R2-full also needs the module-merge
  behavior or a concatenated-source build).

**Known non-targets for the golden:** `typeerr.jtr`-style files (the reference CLI emits nothing
on type errors, but `rust_cgen_dump` calls `emit` unconditionally — if a divergence appears there,
check error handling first); `demo.jtr`/driver files with modules (single-file golden path).

Reference for the plan that produced increment 5 (kept for the pattern):

#### structs plan (as executed)

The reference target (`emit-c compute.jtr | grep -v '^#line'`) shows exactly what's needed:
1. **struct forward typedef** in `emit_forward_types` (item kind **4**): `typedef struct
   Jestyr_<name> Jestyr_<name>;` (union → `typedef union …`). Add alongside the distinct arm.
2. **struct def** (a new `emit_struct_defs`, emitted *before* `emit_result_defs` — for a program
   with no by-value field deps that matches the capture/flush order): per struct,
   `<kw><attr> Jestyr_<name> {\n` then per field `    <cty> j_<field>;\n` then `};\n\n` (note the
   **double** newline — the trailing blank). Members are **10-tuples in `mar`**; get the exact
   layout from `parse_struct_members`/`mk_structtype` (kind 36 stores `(x,y)`=member slice) and
   how typeck lowers struct fields (typeck.jtr phase 2). `struct_attr` (`@packed`/`@align`) can be
   deferred.
3. **StructLit** (`emit_expr` kind 16, and generic kind 17): `(Jestyr_<name>){ .j_<f> = <v>, … }`
   — designated initializers, field name prefixed `j_`. FieldInit children are kind 18. Get the
   field-init encoding from the parser (`FieldsResult`, the arg-arena slice).
4. **Field access** (`emit_expr` kind 5): `<base>.j_<name>` (the field name span is Field's `(x,y)`).
5. **Inferred-`let` types** (`let p = Vec2{…}`, annotation `ty == -1`): read the **Checker's
   per-expr type** — `c.et[init_exprid]` → `TyId` into `c.tys` (a `TyData{kind,a,b,x,y}` arena),
   then a NEW `emit_ty_c(sb, src, c, tyid)` mirroring `cgen::c_type`: kind 2 Prim (`prim_name(d.x)`
   → `c_prim`), kind 15 Named (`Jestyr_<name>`, name at `td(c,d.x,0)..td(c,d.x,1)`), kind 1 Unit →
   `void`; defer the rest. **This requires threading `read c: typeck.Checker` through the statement
   group only** (`emit_body`/`emit_stmt`/`emit_return`/`emit_if` — `emit_expr` does NOT need `c`
   for structs). See `ty_str` in typeck.jtr for the TyData kind table (0=?,1=(),2=Prim,3=Ptr,
   4=Opaque,5=Result,6=GenStruct,8=Slice,9=Array,10=GenRef,11=RegionRef,12=Fn,13=Task,15=Named,
   17=DynOpaque,18=OpaqueLit,16=Error).

After structs: field-typed lets, `str`/slice types in `emit_c_ty`, then the `union`+`size_of`
family, enums/match, closures, concurrency — grow the allowlist file-by-file with `DUMP_DIVERGE=1`.

---

## 0. Discipline (unchanged — every increment)

- `cargo test`-green (685 default) + warning-clean; cross-impl goldens behind `--features
  c-oracle`. Keep non-users byte-identical. **Auto-commit each green increment to `master` +
  `git push origin master`** (`git commit -F <file>`, end with the Co-Authored-By trailer).
- One construct per increment with its golden slice. Never a big drop.
- **The deep-dive lever:** every pass got a `DUMP_DIVERGE=1` first-diff printer in its golden;
  P3 also got `TYPECK_FILE=<basename>` for an aligned per-expr want/got stream. Build the cgen
  equivalent FIRST (a `CGEN_FILE=<basename>` that prints the reference C with line numbers) — it
  turns every divergence into a mechanical fix.

## 1. The reusable toolkit (all in place)

- **Importable libraries:** `parser.jtr` (`pub fn parse_source -> Parser`, all AST arenas pub),
  `typeck.jtr` (`pub fn check_parsed(p, src, a) -> Checker`; the Checker carries `et` per-expr
  TyId, the `tys`/`tya` TyData arena, `tdecl`/`tch`/`fns`, `mcalls`/`icalls` resolution records,
  and `pub fn ty_is_copy`). **cgen.jtr just does:** `import "parser"` + `import "typeck"` →
  `parse_source` → `check_parsed` → walk items emitting C. Each pass has a thin `*_cli.jtr`
  driver (a module with `main` can't be imported — the duplicate-`main` link error).
- **Golden template:** `jestyr_escape_dump_matches_reference` is the closest model (whole corpus,
  per-file compare, `DUMP_DIVERGE`, empty denylist). Copy it verbatim for
  `jestyr_cgen_matches_reference`.
- **std:** `list` (make/push/get/set/len/truncate/free), `String`/`string_new`/`string_push`/
  `string_view` (cgen is one big string builder — this is exactly the API you need), `mem`,
  `intern`, `fs`, `env`.

## 2. P5 cgen — the plan

**Goal:** `examples/std/cgen.jtr` lowers AST + P3 types to C **byte-identical** to `src/cgen.rs`.

**The golden = a byte compare of the whole emitted C**, per corpus file. The reference is
`crate::cgen::emit(&ast, &info) -> (String, diags)` (via `parse()` + `typeck::check`). The
acceptance bar is exact string equality — but you can lean on the **C-hash** (`src/attest.rs`
already SHA-256s the emitted C; `proptests::compilation_is_deterministic` pins determinism), so
cross-impl equality can be a hash compare once close, and a full-text diff (with `DUMP_DIVERGE`)
while converging.

**Stage by construct AND by corpus subset** (both — cgen output is all-or-nothing per file, so
a file only matches once *every* construct it uses is ported). Start with `hello.jtr`
(`fn main -> i32 { print_str("…") return 0 }`) — it exercises the prelude + one fn + a call +
a string literal + return. Grow outward. Expect a large denylist early that shrinks per
construct, exactly like P2's module golden did.

**Orchestration to mirror** (`emit_program`, cgen.rs ~260–320) — the top-level emit ORDER is
load-bearing for byte-identity:
1. collect instances: slices, arrays, **struct instances** (monomorphized generics via a
   whole-program walk `collect_struct_instances`/`collect_structs_in_*`), enum instances, fn-type
   instances, closures. (This instance-collection walk is the hardest infra — it decides which
   monomorphizations exist and in what order.)
2. `prelude()` — a FIXED C header block (includes + `jestyr_rt_*` runtime fns + `JestyrStr` etc.;
   see cgen.rs ~685). Copy it byte-for-byte; conditionalized on pthread/time feature use.
3. `forward_types()` → `gen_forward_types()` (forward-decl monomorphized aggregates) →
   `fn_type_typedefs()` → `struct_defs()` (topological, by-value-field ordering — the aggregate
   ordering fix the parser session already made on the Rust side) → `enum_defs()` → then
   function bodies (`emit_fn`/`emit_fn_body`).

**The big machinery inside** (each its own increment, roughly in dependency order):
- **Name mangling** (`mangle`, `gen_struct_c_name`, `gen_enum_subst`) — `Jestyr_<name>__<types>`;
  must match exactly or every symbol diverges.
- **Type → C type** rendering (prims → `int32_t`/…; `str` → `JestyrStr`; slices → fat structs;
  pointers; generic instances → their mangled C struct name).
- **`emit_expr`** (cgen.rs ~4029, the biggest match) — literals, names, binops (+ operator-trait
  dispatch → the impl's C fn), calls (`emit_call`/`emit_named_call` — method sugar via `mcalls`,
  intrinsics → `jestyr_rt_*`), field/index/deref, struct/array literals, casts, f-strings,
  closures (lifted to top-level C fns via `collect_closures`), `spawn`/`await`/`concurrent`/
  `par for`/`select` (pthread lowering), `region` (arena), `?`/try.
- **`emit_stmt`/`emit_block`/`emit_return`/`emit_if`/`emit_match`** — match is a whole family
  (`emit_niche_match`, `emit_scalar_match`, `emit_nested_match`, guarded variants) — niche
  optimization (Option(ptr) etc.) is subtle; defer niche to a late increment, do the tagged-union
  match first.
- **RAII drop glue** (`emit_all_drops`/`emit_drop_place`/`collect_moved`) — auto-inserted
  scope-exit drops in reverse declaration order, skipping moved-out locals. Byte-identity here
  needs the exact move analysis (`collect_moved_expr`).
- **Monomorphization**: emit one C fn per (generic fn, type-args) instance actually used — the
  instance-collection walk drives this.

**What cgen needs from typeck that isn't exposed yet** (add as `pub` when you hit them, like P4
did): the closure index, niche info, and possibly the per-call resolved-symbol map (`call_sym`)
and dyn-coercion records. typeck currently records `mcalls`/`icalls`; cgen will want more of the
reference's `TypeInfo` side tables (`method_calls`/`impl_calls`/`dyn_calls`/`dyn_coercions`/
`qualified`/`call_sym`). Port them into the typeck Checker as flat arenas the same way, each with
its own tiny increment, *before* the cgen construct that reads it.

**Risk to expect:** porting 10.8K more lines of Jestyr will surface bootstrap-compiler
(Rust-side) bugs — the parser session already forced ~3 cgen fixes. When the Jestyr `cgen.jtr`
*itself* miscompiles (it's a big Jestyr program), that's a real bug in the Rust cgen to fix, and
it hardens the compiler. Budget for it.

## 3. R2 — the fixpoint (the acceptance criterion)

Gate behind `--features selfhost-fixpoint` (outside the toolchain-free default suite; the feature
doesn't exist yet — add it to `Cargo.toml`).

**Stand it up EARLY, on a subset** — you do NOT need full corpus parity first. As soon as
`cgen.jtr` emits correct C for one self-contained program P (start with `hello.jtr`):
- **jc1** = the Rust compiler builds the Jestyr compiler (parser+typeck+escape+cgen `.jtr` as one
  program) → a native `jc1` exe.
- **jc2** = `jc1` compiles P (or compiles the Jestyr-compiler sources) → C → exe.
- **jc3** = `jc2` recompiles the same → C.
- **Assert `jc2 ≡ jc3`** (byte-identical C, a hash compare). That fixed point *is* the proof.

Practically the first milestone is smaller: `jc1` (the Jestyr-written cgen) emitting C for
`hello.jtr` that gcc builds and runs to the same output as the Rust compiler's. Grow P from there.

## 4. Known constraints / lessons (carry forward)

- **Reserved words bite in `.jtr`:** `out`, `comptime`, `par`, `select`, `region` etc. are
  keywords — don't use them as identifiers (P4 hit `out`/`comptime` as param/local names).
- **A module with `main` can't be imported** → library + thin `*_cli.jtr` driver (done for
  parser + typeck; do the same for cgen).
- **`parse()` vs `parse_module()`** on the reference side: goldens use `parse()` (fills
  `ast.items`); `parse_module` leaves it empty.
- **Emission ORDER = byte-identity.** The reference's careful ordering (instances → prelude →
  forward types → fn-type typedefs → struct/enum defs → bodies, and topological by-value struct
  ordering) must be mirrored exactly. Diff early on `hello.jtr` to lock the skeleton before
  scaling.
- **Copy programs stay byte-identical** — a construct you haven't ported must not perturb the C
  of a program that doesn't use it.

## One-line
P2+P3+P4 self-hosted & byte-exact. P5 = `cgen.jtr` (`import parser`+`typeck` → `check_parsed` →
emit C), golden = byte-identical C (lean on the `attest` C-hash), staged construct-by-construct
from `hello.jtr` out, mirroring `emit_program`'s exact order. R2 = stand up jc1→jc2→jc3 early on a
subset, assert jc2≡jc3. Everything is on `origin/master` (`9d9e5bd`).
