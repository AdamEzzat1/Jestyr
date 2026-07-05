# Jestyr self-hosting — P2 parser progress + what remains (cold-start handoff)

> Continues ROADMAP workstream **P** (self-hosting the compiler in Jestyr). Read with
> `docs/session-notes/jestyr-selfhost-port-P2-P5-R2.md` (the P2–P5/R2 master plan),
> `src/parser.rs` / `src/ast.rs` (the reference this port mirrors), and
> `examples/std/parser.jtr` (the Jestyr parser being grown).
>
> **State: P2 is COMPLETE.** The front end, the **whole P2 expression parser** (every form —
> incl. loops, closures, concurrency, region; see §3), the **statement + block-led** layer, the
> **structural type parser**, the **pattern parser**, and **all nine item kinds, fully-featured**
> — fn (generics, attrs, error sets, `requires`/`ensures`), struct/record/union (fields with
> `@volatile`/bit-fields/defaults + methods), enum, trait, impl (blanket generics), const,
> distinct, import, extern (abi) — are done, cross-checked by **four** byte-exact goldens (expr,
> item, **whole-corpus module**, depth). The whole-corpus module golden runs `parse_module` over
> **all 125 example files and every one is item-for-item identical** to the Rust reference — the
> `MODULE_GOLDEN_DENYLIST` is now empty. This note captures what's built, the recipe, and the road
> **past P2 → P3 typeck** (§4).

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

## 3. Immediate next: the remaining EXPRESSION forms (to clear the 49 denylisted files)

All nine item kinds are **fully featured** now (arenas: `it` items; `iar` fn/method param
7-tuples; `mar` struct-member 10-tuples / trait-method 7-tuples / impl-method fn-ids; `ear` enum
sub-lists; `aar` attribute args+4-tuples; `gar` generic 4-tuples; `far` fn error/require/ensure
"extras"; shared helpers `parse_params_into_iar`/`parse_generics`/`parse_attrs` +
`dump_params`/`dump_generics`/`dump_attrs`/`dump_fn_extras`/`ref_dump_*`). ItemData kinds: 0
Import 1 Distinct 2 Const 3 Fn 4 Struct(op=0/1/2) 5 Enum 6 Trait 7 Impl 8 Extern 99 Error.
Nothing item-level is deferred anymore.

**All `parse_primary` EXPRESSION forms are now built** — the `MODULE_GOLDEN_DENYLIST` is empty and
the **whole-corpus module golden** (`jestyr_parser_module_dump_matches_reference`) passes **125/125
files item-for-item**. The test reports **all** diverging files at once (and, with `DUMP_DIVERGE=1`
in the env, prints the first-differing atom window per file — the fast way to see *which* form a
file is tripping on; keep it for the P3+ goldens). The expression forms landed this pass:

- ✅ **Loops** (kinds 31=For 32=Break 33=Continue 34=Invariant 35=Variant). `for` in all three head
  shapes (infinite / conditional / iterating w/ binds+sources+step), label, `region`, loop-`else`;
  header spilled into the `lar` arena (see `mk_for`); `peek_kind` lookahead added.
- ✅ **`struct { … }` value** (kind 36). Shared `parse_struct_members`/`dump_members`
  (+ `ref_dump_members`) extracted so the struct *item* parser and the value form share the
  10-tuple member loop; `parse_struct_type` dispatches from `parse_primary` on `struct`.
- ✅ **Closures** `|x| body` / `|| body` (kind 37). Param 3-tuples (name span, type or -1) in the
  `clar` arena, appended before the body. `Pipe`/`PipePipe` in `parse_primary`.
- ✅ **Concurrency** (kinds 38=Concurrent 39=Spawn 40=Await 41=ParFor 42=Select). `spawn` parses at
  the unary level, `await` at postfix (binds tighter than `as`/binary). `select` arm 4-tuples in
  `par`. **Contextual-keyword note:** `recv`/`reduce`/`step` are positionally unambiguous, but
  `par` is NOT (a plain ident before `for` also occurs, e.g. `var i = lo` then `for …`), so the
  text must really be "par". The parser can't store the borrowed `src` (escape checker forbids it),
  so the driver precomputes a **`parw[i]` mask** (token i is an ident with text "par") and stores
  that `List(bool)` in the `Parser` — the pattern to reuse if P3+ ever needs another text test.
- ✅ **`region r { }`** as an expression (kind 43) — `parse_region`, block-led. (`for … region r`
  as a loop *clause* was already handled by the loop parser.)

Every form is an `ExprData` kind with a `dump`/`ref_dump_expr` arm; the buffer-then-append (or
append-before-body) discipline keeps each variable-arity slice contiguous under nesting.

---

## 4. The road past the expression parser (P2 → R2)

Ordered, each gated by a cross-implementation golden on the shared corpus (see the master plan):

1. ✅ **P2 expressions** — done (all forms).
2. ✅ **Type parser + pattern parser** — done (cast/`let` dumps upgraded to structure).
3. ✅ **Statement parser + block-led forms** — done (`let`/`var`/`return`/expr, blocks,
   `if`/`else`, `unsafe`, `match`).
4. ✅ **Item parser + all expression forms** — DONE. All nine item kinds fully featured; every
   expression form (§3) built; the whole-corpus module golden passes **125/125** files. **P2 is
   complete.**
5. **P3 typeck** (NEXT) → resolved-type-dump golden. **P4 escape** → diagnostic-set golden.
   **P5 cgen** → byte-identical-C golden (construct by construct). **R2 fixpoint** (`--features
   selfhost-fixpoint`): jc1→jc2→jc3, assert `jc2 ≡ jc3`, stood up early on a subset. The parser's
   arena + flattened-S-expression-dump + cross-impl-golden toolkit (`build_exe`, the four
   `jestyr_parser_*` goldens, `DUMP_DIVERGE`) is the template each later pass reuses.

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
| Reference dump + goldens | `src/proptests.rs` `mod c_oracle`: `ref_dump_{expr,type,pat,item,stmt,block,fn,params,generics,attrs}`, `rust_{expr,item,module}_dump`, `jestyr_{expr,item,module}_dump`, `jestyr_parser_{expr,item,module}_dump_matches_reference` (+ `MODULE_GOLDEN_DENYLIST`), `jestyr_parser_bounds_deep_nesting`, `Parser::parse_single_{expr,item}` / `parse_module` |
| Jestyr driver modes | `parser_exe <file>` = expr · `<file> item` = one item · `<file> x module` = whole module |
| Token kinds | `src/token.rs` (`TokenKind` discriminant order = the integer tags; e.g. Let=8 Const=10 Struct=11 Enum=13 Distinct=18 Match=21 If=22 Else=23 Return=28 Trait=15 Impl=16 Fn=7 Import=41 Pub=42 As=48 unsafe=38) |
| Master plan | `docs/session-notes/jestyr-selfhost-port-P2-P5-R2.md` |

## One-line summary
**P2 is COMPLETE.** The Jestyr-written parser (`examples/std/parser.jtr`) builds the same item
stream as the Rust reference for **all 125 corpus files** (module golden 125/125, empty denylist):
every expression form (loops, `struct{}` value, closures, concurrency — concurrent/spawn/await/
par-for/select, region), statements/blocks, `if`/`unsafe`/`match`, the structural type & pattern
parsers, and all nine item kinds — across **four** byte-exact goldens (expr, item, whole-corpus
module, depth). **Next: P3 typeck** (resolved-type-dump golden) → P4 escape → P5 cgen → R2 fixpoint,
reusing the parser's arena + S-expression-dump + cross-impl-golden toolkit.
