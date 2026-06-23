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

**Documented limitations (follow-ups):** instantiation inference covers `let`/`return`
but **not call arguments** (`get(none)` can't infer — pass a typed binding, or write
`some(…)`); generic enums used only *inside a generic function body* (under an unapplied
substitution) aren't collected for monomorphization; generic-enum *methods* aren't
supported; a true auto-imported prelude awaits the module system (today `Option`/`Result`
are defined per-module or imported).

### 2.3 Struct-variant enums + explicit discriminants  ⏳

```jestyr
enum Shape {
    circle { r: f64 },
    rect   { w: f64, h: f64 },
}
enum Color : u8 { red = 1, green = 2, blue = 4 }   // raw-value discriminants
```
- **Inspiration:** Rust struct variants; Swift/Rust raw values; Ada discriminated records.
- **Struct variants:** `EnumVariant.fields` is *already* `Vec<(Ident, TypeId)>` in the
  AST — the names exist; the parser just needs a `{ field: T, … }` variant grammar
  alongside the positional `(T, …)` form, and `match` needs `circle { r }` patterns
  (field shorthand). Mostly parser + a match-pattern arm.
- **Explicit discriminants:** an optional `: <int-type>` on the enum sets the
  discriminant representation, and `= n` pins a variant's tag (Ada/Rust). Lowers to a C
  enum/`switch` on the chosen integer type; pairs with a `@repr(u8)`-style attribute
  (reuse the attribute registry — `@repr` slots next to `@layout`).
- **Conflict note:** this is the AST-shape change of the batch (variant payload grammar)
  → **land it early** so other enum work rebases on it (HANDOFF §1 strategy).

### 2.4 Match power + Maranget exhaustiveness  ⏳ (largest)

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

### 2.5 Recursive ADTs via explicit `indirect`  ⏳ (the novel angle)

```jestyr
enum Tree {
    leaf(i32),
    node(indirect &[r]Tree, indirect &[r]Tree),   // arena-tiered recursion
}
```
- **Inspiration:** Swift `indirect` (but Swift/Rust hide the heap; Jestyr won't).
- **The Jestyr difference:** the indirection *chooses its tier in the type* — `indirect
  &[r]Node` (arena, zero-cost), `indirect &Node` (generational, checked), `indirect
  *Node` (raw). A recursive type therefore **carries its allocation strategy**, which no
  inspiration offers and which is exactly "transparent cost." `indirect` is the marker
  that (a) breaks the infinite-size cycle and (b) names the tier.
- **Impl:** an `indirect` payload marker on a variant field; size computation treats it as
  a pointer; cgen allocates through the named tier. Depends on the reference tiers (have).
- **Replaces** CJC's GC `class` for the recursion use-case — no GC type needed.

### 2.6 `distinct` types  ⏳

```jestyr
distinct UserId = u64        // same bits as u64, but not interchangeable with it
distinct Meters = f64
```
- **Inspiration:** Haskell `newtype`, Odin `distinct`, Ada subtypes. The `distinct`
  keyword is **already reserved** in the lexer.
- **Rule:** identical representation, *nominally distinct* — no implicit coercion to/from
  the base type (an explicit `as` cast or constructor is required). Zero runtime cost.
- **Impl:** a thin nominal wrapper in the type table; cgen emits the base C type; the
  checker refuses cross-assignment. Great with refinements (`distinct Percent = u8` +
  `in 0..=100`).

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
| 2 | **Struct-variant enums + explicit discriminants** | the AST-shape change → land early | **yes** | M | ⏳ |
| 3 | **Match power + Maranget exhaustiveness** | rests on #2's pattern shapes; largest | **yes** | L | ⏳ |
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
