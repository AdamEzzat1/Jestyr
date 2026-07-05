# Jestyr self-hosting — P2 parser progress + what remains (cold-start handoff)

> Continues ROADMAP workstream **P** (self-hosting the compiler in Jestyr). Read with
> `docs/session-notes/jestyr-selfhost-port-P2-P5-R2.md` (the P2–P5/R2 master plan),
> `src/parser.rs` / `src/ast.rs` (the reference this port mirrors), and
> `examples/std/parser.jtr` (the Jestyr parser being grown).
>
> **State:** the front end (lex + classify), the **whole P2 expression parser**, the
> **statement + block-led** layer (blocks, `if`/`else`, `unsafe`, `match`), the **type
> parser**, the **pattern parser**, and the **item layer's infrastructure + first three item
> kinds** (`import`/`distinct`/`const`) are all done and cross-checked with three byte-exact
> AST-dump goldens against the Rust reference. What remains in P2: the rest of the item kinds
> (`fn`/`struct`/`record`/`union`/`enum`/`trait`/`impl`/`extern` + attributes/contracts), then
> the **whole-corpus** item golden over all ~122 files. This note captures what's built, the
> recipe for adding a construct, the fn design already worked out, and the road past P2.

---

## 1. What's done (this session)

### 1a. Front-end plumbing / cgen (cross-cutting)
- **Shared tokenizer** `examples/std/tokens.jtr` — `pub fn tokenize(src, it, kwcount, a) ->
  List(Token)`, `Token = { pub kind, pub start, pub end }` (kind = `TokenKind` discriminant),
  `pub fn intern_keywords`. `lexer.jtr` is a thin driver over it. Cross-checked by the P1
  lexeme, P2a label, and P2a **integer-tag** goldens over the 122-file corpus.
- **cgen fix — cross-module generic type args** (`eval_type_arg`): a module-qualified comptime
  type argument (`list.get(tokens.Token, …)`) now resolves via the import map instead of
  degrading to `Opaque("?")`. Unblocks any module returning `List(T)` etc. to a consumer.
- **cgen fix — aggregate-definition topological ordering**: a struct embedding another
  aggregate *by value* (`List(E)` field, `[N]T`, `X !E`, `Wrapper(Leaf)`) now has that
  aggregate defined first. Stable DFS in emission order → byte-identical for programs with no
  forward dep. (`aggregate_defs_topologically_ordered_by_by_value_fields`.)
- **cgen fix — blanket Drop glue vs a type arg named `T`**: `has_concrete_drop_impl` replaces a
  bare `impl_index` lookup so a user `struct T` no longer collides with a blanket `impl[T] Drop
  for List(T)` and skips its drop glue. (`examples/std/drop_named_type_param.jtr`.)

### 1b. The P2 parser (`examples/std/parser.jtr`)
A recursive-descent + Pratt parser on the **`Parser` struct**, threaded `mut`, bundling
**multiple parallel arenas** (each `List(...)`, id = index) and their i32 **child-slice
pools**: `ex`/`ar` (expressions), `st`/`sar` (statements), `pt`/`par` (patterns; `par` also
holds match-arm triples), `ty`/`tar` (types), `it` (items). Plus `alloc`, `pos`/`n`,
`depth`/`over` (the nesting guard), `no_struct`. (Nesting arenas in a struct works thanks to
the cgen ordering fix — it dogfoods it.) `src` is **not** stored (the escape checker forbids
storing a borrowed `str` in a struct — "second-class borrow may not outlive its call"), so
`dump*` take `read src: str` as a param; text-bearing dumps (f-strings, import paths) slice it.

**Constructs handled** (each landed with its golden slice, teeth-verified):
- Leaves: int / float / name / **char / bool**.
- Prefix unary `-` `!`/`not` `~` `&`; full binary precedence table (`bin_op`); `( … )` grouping.
- Postfix: `.field`, `[index]`, `.*` (deref), `?` (try), `(args)` (call).
- `as` casts (now **structural** — see the type parser below).
- Assignment (`=` `+=` … `^=`); ranges `..` / `..=`; array literals `[e0, …]` / `[value; count]`.
- Struct literals `Path{ …, ..spread }` and **generic struct literals** `Ctor(T…){ … }`.
- `self` / `Self` (and `Self{ … }`); `@attr` callables; **f-strings** `f"… {x} …"` (text dump).
- **Statements** (`let`/`var` with `: T` + `= init`, `return`, expr-stmt) and `{ … }` **blocks**.
- **Block-led** `if`/`else`/`else if`, `unsafe { … }`, and **`match`** with a full **pattern
  parser** (wildcard, ident binding, variant `n(subpats)`, struct-variant `n{ f, .. }`,
  or-`a|b`, ranges, guards, `..` rest, char/bool/int/neg literals).
- **Structural type parser** (`ty`/`tar`): Name, `type`, Ptr (`*`/`*mut`/`*const`, `indirect`),
  Slice `[]T`, Array `[N]T`, GenRef `&T`, RegionRef `&[r]T`, App `Ctor(args)`, Path
  `mod.Type(args)`, Fn `fn(conv T,…) -> conv R`, Dyn `dyn Trait`, Error. Cast + `let: T` carry a
  structural `TypeId` (not a span); their dumps upgraded to full structure.
- **Item layer** (`it`) — infra + `import "p" [as a] [= "hash"]`, `distinct N = T`,
  `const N [: T] = v`. Selected by a 2nd CLI arg (`parser_exe <file> item`).

**Variable-arity representation:** every list-shaped node (call args, array/struct fields,
type args, block statements, variant subpats, or-alts, match arms, …) stores a contiguous
`(start, count)` slice into the relevant child pool. The hazard: a *nested* parse pushes into
the same pool and scatters the slice — so each node **buffers its children in a per-call temp
`List(i32)` and appends them contiguously after parsing**. Always use this pattern.

**Depth guard** (matches `MAX_EXPR_DEPTH = 256`): `descend`/`depth`/`over` bound AST *height*
at the recursive entry points and the iterative folds/chains, so adversarial nesting bails
cleanly instead of overflowing the parser — or the recursive `dump` — stack
(`jestyr_parser_bounds_deep_nesting`). (Blocks nested via `parse_block_like` are *not* guarded,
matching the reference; not in the corpus.)

**The goldens** (`src/proptests.rs`, `--features c-oracle`) — three now:
- `jestyr_parser_expr_dump_matches_reference` (`parse_single_expr` + `ref_dump_expr`) — the
  expression/statement/type/pattern corpus (~130 snippets).
- `jestyr_parser_item_dump_matches_reference` (`parse_single_item` + `ref_dump_item`) — the
  item corpus; runs the exe in **item mode** (2nd arg).
- `jestyr_parser_bounds_deep_nesting` — the depth guard.
- The dump is a **flattened S-expression, one atom per line** — a *pure function of the arena*;
  both impls emit the identical stream. Node kinds are enumerated in the `ExprData` header
  comment in `parser.jtr` (0 Int … 22 FString 23 Block 24 If 25 Unsafe 26 Char 27 Bool
  28 Match); `PatData` / `TypeData` / `StmtData` / `ItemData` have their own kind tables in
  their struct-header comments.

---

## 2. The recipe for adding a construct (follow this exactly)

1. **Reference:** find its `parse_*` in `src/parser.rs` and its `ExprKind` in `src/ast.rs`;
   note the **exact span** rule (`Span::to` = `{min(starts), max(ends)}`).
2. **Jestyr parse:** add the parse logic to the right layer of `parser.jtr`. Reuse the temp-list
   → `ar` pattern for any variable-arity children. Guard new recursion with `descend`.
3. **Node:** pick a new `kind` int; add an `mk_*` constructor. Aux data goes in `op`/`x`/`y`
   (spans/counts, cast) or child `ExprId`s in `a`/`b`; variable-arity children go in `ar` with
   `(start, count)`.
4. **Dump (Jestyr):** add a case to `dump` in `parser.jtr` — emit `kind label`, any op/aux, the
   span (`print_uint`), then children in order. Use `dump_opt` for an optional child.
5. **Dump (reference):** add the matching arm to `ref_dump_expr` in `src/proptests.rs` (and a
   label helper if needed). **The two must emit the identical atom sequence.**
6. **Golden:** add snippets to the `jestyr_parser_expr_dump_matches_reference` array exercising
   the *distinguishing* behavior (precedence, associativity, spans, nesting).
7. **Verify:** `cargo test --features c-oracle jestyr_parser` (golden + depth), `cargo test`
   (default 685), warning-clean. Auto-commit the green increment; teeth-check a construct or two
   by mutation.

---

## 3. Immediate next: the remaining item kinds

The item infra is up (`it: List(ItemData)`, `parse_item` dispatching on an optional `pub`,
`dump_item`, `parse_single_item` on the reference, the `jestyr_parser_item_dump_matches_reference`
golden). Add the remaining kinds one per green increment. **`fn` is next** (the design is
already worked out below); then the aggregates, then attributes, then the whole-corpus golden.

- **`fn`** — *the big one.* Reference `parse_fn`/`parse_params`/`parse_param`/`parse_generics`/
  `parse_error_set` (parser.rs 326–582). `FnDecl` = is_pub, generics (`[T: Bound]`, each a
  name + optional bound), attrs, name, params, ret_conv/ret_ty, errors (`!{ Names }`), requires/
  ensures contracts, body. **Plan:** widen `ItemData` (it has only `a,b,x,y,z,w` + `is_pub` — add
  an `op` field; fn needs name span + param slice + ret_conv + ret_ty + body = 7 aux). Add an
  item-child arena `iar: List(i32)`; store each **param as a fixed 7-tuple** (comptime, conv,
  name_start, name_end, is_self, ty|-1, refine|-1). Suggest **fn-core first** (name, params
  incl. `self`/`comptime`/conv/`: T`/`in refine`, `-> conv ret`, body) — dump `(fn <is_pub>
  <name span> <paramcount> <params…> <ret_conv> <ret-opt> <body>)`, each param `(param
  <comptime> <conv> <is_self> <name span> <ty-opt> <refine-opt>)` — testing fns with **no**
  generics/errors/contracts/attrs (the reference gets them empty; simplest to just omit those
  fields from the dump until implemented, exactly as cast was span-only pre-type-parser). Then
  **fn-generics** (`[T: Add]`), **fn-errors** (`!{ … }`), **fn-contracts** (`requires`/`ensures`
  — set `no_struct` for each condition), each extending the dump.
- **`struct` / `record` / `union`** — `parse_named_struct`/`parse_struct_body`. Shared field
  grammar: `StructMember` is a Field (name, ty, `@volatile`, `= default`, `pub`, `: bits`) or a
  Method (a nested `FnDecl` — reuses the fn parser). `is_record`/`is_union` are just the keyword.
- **`enum`** — `parse_enum`. `EnumDecl` = name, type_params `(T)`, variants (each name + fields
  `(Ident, TypeId)` + optional `= discriminant`).
- **`trait` / `impl`** — `parse_trait`/`parse_trait_method`/`parse_impl`. Trait = methods
  (signatures, optional default body). Impl = generics, trait_name, target ty, methods (FnDecls).
- **`extern`** — `parse_extern`: `extern "c" fn name(...) -> T` (bodyless).
- **Attributes** (`@packed`, `@align(8)`, `@section("data")`) — `parse_attrs` runs *first* in
  `parse_item` (before `pub`). Each is name + args (ExprIds). **Gotcha:** attr args can be
  strings (`@section("data")`) — `parse_primary` does **not** yet handle `Str` (kind 3) or
  `Null`; add those leaves before/with attrs, or the arg becomes an `Error` node and diverges.
- **Whole-corpus golden** — once all kinds parse: `parse_module` over each of the ~122 example
  files, dump every item, diff. This is the P2 acceptance test; expect it to surface gaps (rare
  type forms, contract spellings, `error` sets) to fill in.

---

## 4. The road past the expression parser (P2 → R2)

Ordered, each gated by a cross-implementation golden on the shared corpus (see the master plan):

1. ✅ **P2 expressions** — done (all forms).
2. ✅ **Type parser + pattern parser** — done (cast/`let` dumps upgraded to structure).
3. ✅ **Statement parser + block-led forms** — done (`let`/`var`/`return`/expr, blocks,
   `if`/`else`, `unsafe`, `match`).
4. **Item parser** (in progress): infra + `import`/`distinct`/`const` done; remaining
   `fn`/`struct`/`record`/`union`/`enum`/`trait`/`impl`/`extern`/attributes/contracts → then the
   **whole-corpus AST-dump golden** (§3).
5. **P3 typeck** → resolved-type-dump golden. **P4 escape** → diagnostic-set golden. **P5 cgen**
   → byte-identical-C golden (construct by construct). **R2 fixpoint** (`--features
   selfhost-fixpoint`): jc1→jc2→jc3, assert `jc2 ≡ jc3`, stood up early on a subset.

**cgen notes for later:** the emitted C, module content-hashing, and `attest` make "same input ⇒
same output" a hash compare; lean on them for P5 and R2. Two cgen gaps this session are fixed
(cross-module generic type args; aggregate ordering; blanket-drop `T` collision) — nesting arenas
in structs now works, so the Jestyr typeck/cgen can hold their tables in structs.

---

## 5. Discipline (unchanged)
Stay `cargo test`-green (default 685) + warning-clean; the cross-impl goldens live behind
`--features c-oracle`. Keep programs that don't use a new feature **byte-identical**. Ship the
four test layers where they apply; teeth-verify by mutation. **Auto-commit each green increment
to `master`** and `git push origin master`. Land increments small — one construct at a time,
each with its golden slice.

## 6. Anchors
| Thing | Where |
|---|---|
| Jestyr parser | `examples/std/parser.jtr` (kinds/arenas/`dump`) |
| Shared tokenizer | `examples/std/tokens.jtr` |
| Reference parser / AST | `src/parser.rs`, `src/ast.rs` (`ExprKind`/`PatKind`/`TypeKind`/`Item`, `Span::to`) |
| Reference dump + goldens | `src/proptests.rs` `mod c_oracle`: `ref_dump_{expr,type,pat,item,stmt,block}`, `rust_{expr,item}_dump`, `jestyr_{expr,item}_dump`, `jestyr_parser_{expr,item}_dump_matches_reference`, `jestyr_parser_bounds_deep_nesting`, `Parser::parse_single_{expr,item}` (parser.rs, `#[allow(dead_code)]`) |
| Token kinds | `src/token.rs` (`TokenKind` discriminant order = the integer tags; e.g. Let=8 Const=10 Struct=11 Enum=13 Distinct=18 Match=21 If=22 Else=23 Return=28 Trait=15 Impl=16 Fn=7 Import=41 Pub=42 As=48 unsafe=38) |
| Master plan | `docs/session-notes/jestyr-selfhost-port-P2-P5-R2.md` |

## One-line summary
Front end + **all P2 expressions** + statements/blocks + `if`/`unsafe`/`match` (with the full
pattern parser) + the **structural type parser** + **item infra & `import`/`distinct`/`const`**
are done, across three byte-exact AST-dump goldens (expr, item, depth). **Next:** `fn` (design
in §3), then `struct`/`enum`/`trait`/`impl`/`extern` + attributes, then the whole-corpus item
golden → P3/P4/P5 → R2 fixpoint.
