# Structs, Enums & ADTs — Design & Sequenced Plan

> The design for completing Jestyr's product/sum types, synthesizing **CJC-Lang's
> concepts + determinism discipline** with representation/ergonomics/proof ideas
> borrowed from **Zig, Rust, Ada/SPARK, OCaml, and Swift**. Read with
> [`HANDOFF.md`](../HANDOFF.md) §7-B (current ~75%-done struct/enum code) and the
> CJC research notes. Status legend: ✅ done · 🔜 next · ⏳ planned.

---

## 0. The blend (one paragraph)

Take from each language only what fits Jestyr's identity — **nominal,
statically-typed, layout-pinned, compiles-to-C, provable**:

| Concern | Source | What |
|---|---|---|
| Representation | **Zig** | explicit layout, `union(enum)` tagged unions, `@offsetOf`/`@sizeOf` reflection |
| Ergonomics | **Rust** | enum-with-payload, niche optimization, match power (guards/or/range/rest) |
| Proof | **Ada/SPARK** | representation clauses, discriminated records (checked variants) |
| Rigor | **OCaml** | Maranget usefulness → real exhaustiveness + redundancy |
| Recursion / value-vs-ref | **Swift** | `indirect`, value/reference split (but realized via Jestyr's own tiers) |
| Substrate | **Odin / C** | `bit_field`, `union`/`#raw_union`, `distinct` |
| Split + determinism | **CJC** | `struct`/`record` split, allocator/`@no_gc` discipline |

The governing rule (same as the attributes work): **representation is explicit and
inspectable; every guarantee is static and costs nothing at runtime.** CJC's
string-keyed `BTreeMap` structs and string-tagged enums are the anti-pattern — Jestyr
gets determinism *and* layout, not one at the cost of the other.

---

## 1. Current state (the starting line)

Already shipped (HANDOFF §7-B, §5.19):
- `struct` with fields, methods (incl. inside generic structs, monomorphized), generics,
  layout attributes (`@packed`/`@align`/`@layout`), `@volatile` fields, `size_of(T)`.
- `enum` as a **tagged union** (`{ tag; union payload; }`) with positional-tuple variants,
  `match` → `switch`, **exhaustiveness** (name-set coverage) + payload projection.
- The `&`/`&[r]`/`*` reference tiers, regions, generational refs.
- The attribute system (`@`-attributes, registry-validated) — landed last session.

Known gaps this design closes: immutable `record`, struct-variant enums, explicit
discriminants, niche optimization, in-language `Option`/`Result`, recursive ADTs,
`distinct` types, richer match (guards/or/range/rest), Maranget-grade exhaustiveness,
and layout reflection (`offset_of`/`align_of`).

---

## 2. Feature designs

### 2.1 `record` — immutable product type  ✅ DONE (this session)

```jestyr
record Point { x: i32, y: i32
    fn norm_sq(read self) -> i32 { return self.x * self.x + self.y * self.y }
}
```
- **Inspiration:** CJC `record` (E0160) + Swift `let`-fields + Ada `constant`.
- **Rule:** a record's fields are immutable — `p.x = …` is a **compile error**; a
  `mut self`/`out self` method is rejected. The binding itself may still be rebound
  (`var p = …; p = Point{…}` is fine), exactly like a `let`-bound value reused.
- **Cost:** none — a record lowers to the *identical* C struct as a `struct`. The
  guarantee lives entirely in the checker (provable at the source level).
- **Where it landed:** `Record` token (`token.rs`); `is_record` on `Item::Struct`
  (`ast.rs`) and `TypeDecl` (`types.rs`); the mutation check in `typeck`'s `Assign`
  arm via `record_name`; the `mut self` rejection in `parser::parse_named_struct`.
  Demo [`examples/records.jtr`](../examples/records.jtr).

### 2.2 Niche optimization  ✅ DONE (this session) · 2.2b generic `Option`/`Result`  🔜 NEXT

```jestyr
enum Maybe { none, some(p: *mut i32) }   // ← lowers to a bare `int32_t*` (done)
enum Option(T) { none, some(T) }         // ← generic form (needs generic enums, next)
```
- **Inspiration:** Rust niche optimization; ML/Rust prelude ADTs (not hardcoded like CJC).
- **The flagship transparency demo:** an enum that is *one nullary variant* + *one
  single-field variant whose payload is a **thin pointer*** (`*T` or `&[r]T` — both have a
  `null` niche) is represented as **just the payload**: `some(p)` is `p`, `none` is the
  null pointer. No tag, no padding — `size_of(Maybe) == size_of(*mut i32) == 8` (a tagged
  union is 16). The provable fact (a pointer has an unused null bit-pattern) and the cheap
  fact (no tag needed) are the *same fact* — Jestyr's thesis in one optimization.
- **A sharper framing than the original note:** Jestyr's `&T` is a *fat* generational ref
  (`{ptr,gen}`), so it has no single null niche. The niche applies to the **thin tiers**
  `*T` and `&[r]T`. (A fat `&T`/`[]T` correctly falls back to the tagged union.)
- **What landed (`cgen`):** `NicheInfo` + `niche_enum_at`/`niche_enum_named` detect a
  qualifying enum; `c_type`/`c_ty_ast` return the bare payload pointer; `enum_defs` and
  `forward_types` skip it (no struct/tag); `emit_variant_construct` emits `some(p)`→`p` and
  `none`→`((T*)0)`; `emit_niche_match` lowers `match` to an `!= NULL` test instead of a tag
  `switch`. Demo [`examples/niche.jtr`](../examples/niche.jtr) (`8, 42, 0`); the
  `size_of`/`!switch`/`!Jestyr_Maybe` proof is a cgen test.
#### 2.2b Generic enums + in-language `Option`/`Result` — ✅ DONE

Decision: **direct `enum Name(T) { … }` syntax** (not the comptime-`fn … -> type`
pattern generic structs use) — it keeps the enum a registered `TypeDecl`, so variant
registration, `match`, exhaustiveness, and the niche detector all reuse the existing
non-generic-enum machinery. (Generic *structs* can migrate to this form later.)

**Landed now:** `EnumDecl.type_params` (`ast`), the parser (`enum Option(T, E) { … }`),
typeck lowers variant field types with the enum's type params in scope (`some(x: T)` →
`T` is a real type parameter), and cgen treats a generic enum as a **template** — skipped
in `enum_defs`/`forward_types`; *using* one (construction/match) emits a clear
"cannot lower generic enum … yet" diagnostic instead of broken C. Declared-but-unused
generic enums compile clean. Tests: parser (type params), cgen (template not emitted +
use diagnoses).

**The codegen completion — all landed:**
1. **`Ty::GenEnum { ctor, args }`** instance type (a distinct variant, not reusing
   `GenStruct`); `lower_type`/`ast_type_to_ty` of `App{ctor,args}` produce it when
   `ctor` is a generic enum.
2. **Instantiation inference** via `variant_ctor_type` — a payload variant `some(5)`
   recovers `Option(i32)` by `unify_tp` against the variant's template field types; a
   nullary `none` takes its instantiation from a new, *targeted* expected-type
   (`cur_expected`/`cur_ret`, set around `let`-annotations and `return`). So
   `var b: Option(i32) = none` and `return none` work.
3. **Monomorphization** — `collect_enum_instances` scans every expression's type +
   function signatures for concrete `GenEnum`s; `gen_enum_defs`/`emit_enum_instance`
   emit one `Jestyr_Option__i32` tagged union per instantiation (type params
   substituted), mangled via `gen_struct_c_name`.
4. **Construction + match on instances** — `emit_variant_construct` reads the
   construction expr's inferred `GenEnum` type; `emit_match` carries a `(tag_prefix,
   subst)` pair so payload bindings get their concrete C type.
5. **Niche-opt inheritance (free win)** — `niche_enum_instance` runs the niche rule on
   the substituted variant templates, so `Option(*T)`/`Option(&[r]T)` collapse to a
   bare pointer automatically. The §2.2 optimization, now generic.
6. **In-language `Option`/`Result`** — `enum Option(T) { none, some(v: T) }` and
   `enum Result(T, E) { ok(v: T), err(e: E) }` are ordinary source now (no hardcoding).
   Demo [`examples/option.jtr`](../examples/option.jtr) (`42, 7, 5, -3, 8`).

**Instantiation inference** covers `let` annotations, `return`, **and call arguments**
(`or_else(none, 5)` types `none` from the parameter's type). **Remaining limitations
(follow-ups):** generic enums used only *inside a generic function body* (under an
unapplied substitution) aren't collected for monomorphization; generic-enum *methods*
aren't supported; a true auto-imported prelude awaits the module system (today
`Option`/`Result` are defined per-module or imported).

### 2.3 Explicit discriminants ✅ DONE · struct-variant *syntax* ⏳ (mostly already present)

```jestyr
enum Color { red = 1, green = 2, blue = 4 }   // explicit discriminants (done)
enum Shape { circle(r: f64), rect(w: f64, h: f64) }  // named fields — already supported
```
- **Inspiration:** Rust struct variants; Swift/Rust raw values; Ada discriminated records.
- **Explicit discriminants — landed.** `EnumVariant.discriminant: Option<ExprId>` (the
  AST-shape change), parsed as `= <expr>` after a variant. cgen emits `Jestyr_E_<v> = n`
  in the tag enum (both plain and generic-instance tag enums), and `e as i32` reads the
  discriminant by extracting `.tag` (`cgen::is_tagged_enum` gates the rewrite; a niche
  enum has no tag and keeps the pointer cast). Demo
  [`examples/discriminants.jtr`](../examples/discriminants.jtr) (`1, 2, 4, 7, 2`).
  *Note:* variant names can't be language keywords (`red`, not `read`).
- **Struct variants are *already* here in substance.** Jestyr variants carry **named
  fields** today via the paren form `circle(r: f64)` (`EnumVariant.fields` is
  `Vec<(Ident, TypeId)>`). What's *not* yet supported is **named construction/projection**
  (`circle { r: 2.0 }` / `match { circle { r } => }`) — construction and binding are
  positional. Adding the brace grammar + named binding is the remaining, optional polish
  (it adds ergonomics, not capability). Deferred as lower-value than §2.4.
- **A `: <int-type>` repr** (choosing the tag's integer width, e.g. `enum Color : u8`)
  is a separate small follow-up — pairs with a `@repr(u8)` attribute slotting next to
  `@layout` in the registry.

### 2.4 Match power + Maranget exhaustiveness  ✅ DONE (analysis; decision-tree cgen pending)

```jestyr
match shape {
    circle { r } if r > 0.0 => area_circle(r),   // guard
    rect { w: 0.0, .. }     => 0.0,              // literal + `..` rest
    1 | 2 | 3               => small(),          // or-pattern
    0..=9                   => digit(),          // range pattern
    _                       => other(),
}
```
- **Inspiration:** Rust match ergonomics; **OCaml/Maranget** *"Warnings for pattern
  matching"* (2007) for exhaustiveness **and** redundant-arm detection over *nested*
  patterns (CJC only does flat name-set coverage — the clear gap).
- **Pieces:** extend `PatKind` (or-patterns, ranges, `..` rest, `@`-binding) and `MatchArm`
  (an optional `guard: Option<ExprId>`); replace the name-set check with the usefulness
  algorithm; lower to a **decision tree** (`switch` on tag, then nested tests) rather than
  a linear arm scan. Guards interact with `invariant`/`variant` — a guard is just a
  boolean the arm is gated on.
- **Provability:** exhaustiveness becomes a *soundness proof*, and redundant arms a
  *warning* — table-stakes for a provable language.
- **Step 1 ✅ — guards (`pat if <bool> => …`).** `MatchArm.guard: Option<ExprId>`; the
  guard is a boolean the arm is gated on. The soundness rule: a guarded arm contributes
  **nothing** to exhaustiveness (the guard may be false), so `check_exhaustive` skips it
  and an unguarded fallback is still required. cgen flips a match with any guarded arm to
  an ordered if-else-if chain (`switch` can't re-test a tag or fall through on a failed
  guard); no-guard matches keep the existing `switch`/null-test lowering untouched. Demo
  [`examples/guards.jtr`](../examples/guards.jtr); HANDOFF §5.38.
- **Step 2 ✅ — literal + range patterns (`match` on integers).** `PatKind::Lit(ExprId)`
  (`0`, `-3`, `'a'`, `true`) and `PatKind::Range { lo, hi, inclusive }` (`0..=9` / `0..9`).
  The first non-enum scrutinee: a `Ty::Prim` integer/char/bool routes to a value if-chain
  (`emit_scalar_match`), guards composing. Exhaustiveness gains a scalar branch — the domain
  can't be enumerated, so a scalar `match` requires a `_`/binding catch-all. Demo
  [`examples/ranges.jtr`](../examples/ranges.jtr); HANDOFF §5.39.
- **Step 3 ✅ — or-patterns (`a | b`).** `PatKind::Or(Vec<PatId>)`; an arm matches if any
  alternative does, and each alternative counts independently for exhaustiveness (so
  `red | green | blue` needs no catch-all). cgen: scalar ORs the value tests, the enum
  switch stacks `case` labels, the enum if-chain ORs the tag tests. Nullary-variant
  alternatives only (shared payload bindings are future work). Demo
  [`examples/orpat.jtr`](../examples/orpat.jtr); HANDOFF §5.40.
- **Step 3b ✅ — `..` rest in variant patterns.** `PatKind::Rest`, parsed from a bare `..`;
  valid as a variant's *last* field only (`rect(w, ..)` binds `w`, drops the rest; a non-
  trailing `..` is a parse error). Nearly free — the cgen binding loop already binds only
  named subpatterns, so a trailing rest is simply skipped. Demo
  [`examples/rest_pat.jtr`](../examples/rest_pat.jtr); HANDOFF §5.41.
- **Step 4 ✅ — Maranget usefulness (the capstone, *analysis*).** `check_exhaustive` is now
  the usefulness algorithm over a `Pat` IR (`Wild | Var | Int | Range | Or`):
  exhaustiveness = the all-wildcard vector is not useful against the arm matrix (finds
  **nested** gaps the old name-set check missed), and a redundant/unreachable arm is a
  **warning** (`Diagnostic.severity` now distinguishes warnings — they report but don't fail
  the build). Scalars use an interval engine, so `true|false`/`0..=255` are exhaustive without
  a catch-all, and a subsumed literal/range warns. Guarded arms are excluded. Check-demo
  [`examples/exhaustive_check.jtr`](../examples/exhaustive_check.jtr); HANDOFF §5.42.
  **The one remaining piece** is the *backend*: cgen still lowers via the flat switch/if-chain
  and **diagnoses** (rather than dispatches) a nested non-wildcard subpattern — the
  decision-tree lowering that closes this frontend/backend gap is the next match-power task.

### 2.5 Recursive ADTs via explicit `indirect`  ✅ DONE

```jestyr
enum Tree { leaf(v: i32), node(left: indirect Tree, right: indirect Tree) }
```
- **Inspiration:** Swift `indirect` (but Swift/Rust hide the heap; Jestyr won't).
- **What landed.** Two things, and a key discovery:
  - **Recursion already worked** through any pointer tier (`*T`, `&T`, `&[r]T`) — they
    lower to a pointer, so a recursive enum/struct is already finite-sized and compiles.
  - **`indirect T`** is a new keyword that is currently **sugar for a raw pointer `*T`**
    (parser-level → `TypeKind::Ptr`), giving a readable, intent-revealing spelling for a
    self-referential field. Demo [`examples/recursion.jtr`](../examples/recursion.jtr)
    (`30, 70`) — a binary tree summed recursively.
  - **A by-value-recursion guard** (`typeck::check_no_value_recursion`): a field whose
    type is the *enclosing type by value* (`enum List { cons(tail: List) }`,
    `struct Node { next: Node }`) is a clear error — "infinitely sized … store it behind
    an indirection (`indirect List` or `*List`)". This is what makes `indirect` *mean*
    something rather than being pure cosmetics.
- **The Jestyr difference (future):** the design goal is that the indirection *chooses its
  tier in the type* — `indirect &[r]Node` (arena, zero-cost), `indirect &Node`
  (generational), `indirect *Node` (raw) — so a recursive type **carries its allocation
  strategy**. Today `indirect T` is just the raw-pointer form; tier-aware `indirect` +
  auto-allocation on construction is the follow-up. Already replaces CJC's GC `class` for
  recursion — no GC type needed.
- **Limitations:** the guard catches *direct* by-value self-reference; mutual cycles
  (A↔B) and generic-by-value self-reference (`Option(Option(T))` by value) are left to the
  C compiler for now.

### 2.6 `distinct` types  ✅ DONE

```jestyr
distinct UserId = i32        // same bits as i32, but not interchangeable with it
distinct AccountId = i32
```
- **Inspiration:** Haskell `newtype`, Odin `distinct`, Ada subtypes.
- **What landed.** `distinct Name = Base` (`Item::Distinct`, `TypeKindG::Distinct{base}`).
  It lowers to a **zero-cost C typedef** of the base (`typedef int32_t Jestyr_UserId;`,
  emitted in `forward_types`); `is_copy` follows the base. Construction/extraction is the
  ordinary `as` cast (`5 as UserId`, `uid as i32`). Enforcement: a `let` whose annotation
  is a distinct type rejects a non-matching initializer (`typeck::distinct_mismatch`) with
  "expected `UserId`, found `i32` — `distinct` types need an explicit `as`". The check is
  scoped to *only fire when a distinct type is involved*, so the lenient checker is
  unaffected everywhere else. Demo [`examples/distinct.jtr`](../examples/distinct.jtr)
  (`1001, 42, 7`).
- **Limitation:** enforcement currently covers `let` annotations (the common case); the
  lenient checker doesn't yet type-check *call arguments* or *returns*, so passing a
  distinct where its base is expected at a call isn't rejected yet — it lands when general
  argument-vs-parameter type-checking does. Pairs well with refinements later
  (`distinct Percent = u8` + `in 0..=100`).

### 2.7 Layout reflection  ⏳

```jestyr
size_of(Point)            // already exists
align_of(Point)           // new — _Alignof
offset_of(Point, y)       // new — offsetof
```
- **Inspiration:** Zig `@sizeOf`/`@alignOf`/`@offsetOf`/`@typeInfo`.
- **Impl:** new cgen intrinsics emitting `_Alignof(T)` / `offsetof(T, f)`. Makes layout
  *inspectable in-language* — the opposite of CJC's hidden alphabetical map, and a seed
  for the CTFE/reflection workstream (G).

### 2.8 Lower-priority / substrate  ⏳

- **Field defaults** (`x: i32 = 0`) + **per-field visibility** (`pub x`) — cheap grammar
  additions (CJC has both); compose with field attributes.
- **Struct update / spread** (`Point { x: 9, ..p }`) — ergonomic, pairs with `record`.
- **`union` / `#raw_union`** (untagged) and **bit-fields** (named bit ranges) — the Odin/C
  substrate for MMIO; dovetails with `@volatile`/`@address`/`@packed`.
- **opt-in `Copy`** for small aggregates (HANDOFF §7-B).

---

## 3. Sequenced plan

Ordering by **leverage × dependency × conflict**. The repo is a single shared arena, so
AST-*shape* changes (new node fields) are the high-conflict moves — land them first and
rebase the rest (HANDOFF §1).

| # | Step | Why here | AST-shape? | Size | Status |
|---|---|---|---|---|---|
| 0 | **`record`** | smallest entry; pure subtraction | no | S | ✅ done |
| 1a | **Niche optimization** (optional thin pointers) | flagship "transparent cost"; no shape change | no | M | ✅ done |
| 1b-shape | **Generic enum AST + parser + frontend** | the high-conflict shape, landed early; templates skipped, use diagnoses | **yes** | M | ✅ done |
| 1b-codegen | **Generic-enum monomorphization + inference + in-language `Option`/`Result`** | the codegen completion; inherits 1a's niche opt | no | M–L | ✅ done |
| 2 | **Explicit discriminants** (`= n` + `e as int`) | the AST-shape change, landed early | **yes** | S | ✅ done |
| 2b | **Struct-variant *syntax*** (`V { … }` named construct/match) | ergonomics; named fields already exist | **yes** | M | ⏳ |
| 2.5 | **Recursive ADTs via `indirect`** + by-value-recursion guard | recursion already worked via tiers; `indirect` is the spelling | **yes** | S | ✅ done |
| 2.6 | **`distinct` nominal types** (zero-cost typedef + `as` + let-enforcement) | `distinct` keyword; reuses casts | **yes** | S | ✅ done |
| 3 | **Match power + Maranget exhaustiveness** | rests on #2's pattern shapes; largest | **yes** | L | ✅ guards/or/range/rest/Maranget-analysis; decision-tree cgen ⏳ |
| 4 | **Recursive ADTs (`indirect`)** | needs ref tiers (have); novel | small | M | ⏳ |
| 5 | **`distinct` types** | isolated; keyword reserved | small | S | ⏳ |
| 6 | **Layout reflection (`offset_of`/`align_of`)** | isolated cgen intrinsics | no | S | ⏳ |
| 7 | **Substrate** (defaults, visibility, spread, `union`, bit-fields, `Copy`) | polish; parallel-safe | mixed | M | ⏳ |

**Discipline (unchanged):** every step ships a runnable `examples/*.jtr` demo, unit tests
asserting emitted C / diagnostics, and stays `cargo test`-green + warning-clean. Niche
optimization specifically ships a `size_of(Option(&T)) == size_of(&T)` test — the proof
*is* the feature.

---

*Part of the Jestyr language. See [`HANDOFF.md`](../HANDOFF.md) for compiler internals,
[`docs/attributes.md`](attributes.md) for the attribute system, and the CJC-Lang research
notes for the determinism/allocator discipline this builds toward.*
