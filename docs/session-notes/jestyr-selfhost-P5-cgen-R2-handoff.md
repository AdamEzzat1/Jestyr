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

### NEXT increment — **structs** (target `compute.jtr`), the plan

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
