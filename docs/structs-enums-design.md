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

### 2.2 In-language `Option`/`Result` + **niche optimization**  🔜 NEXT (flagship)

```jestyr
enum Option(T) { none, some(T) }
enum Result(T, E) { ok(T), err(E) }
```
- **Inspiration:** Rust niche optimization; ML/Rust prelude ADTs (not hardcoded like CJC).
- **The flagship transparency demo:** when an enum is *one nullary variant* + *one
  single-field variant whose payload type has a **niche*** (an invalid bit-pattern — a
  pointer's `null`, a `bool`'s `2..=255`, a `&T`/`&[r]T`), represent the whole enum as
  **just the payload**, using the niche to encode the nullary case. So
  `Option(&T)` is bit-for-bit a pointer: `size_of(Option(&T)) == size_of(&T)`.
- **Why it's *the* Jestyr demo:** the provable thing (the type has an unused
  bit-pattern) and the cheap thing (don't store a separate tag) are the *same thing* —
  Jestyr's thesis in one feature. Ship it with a `size_of` test that proves the equality.
- **Impl sketch:** define the two enums in a prelude module over the existing ADT
  machinery (retire any hardcoding); add a **niche-detection pass** in `cgen` that, for
  a qualifying enum, emits the bare payload C type and lowers `some(p)`/`none`/match to
  null-checks instead of a tag. Start with the pointer-niche (`&T`/`*T`/`&[r]T`); extend
  to `bool`/enum-with-spare-tags later. Falls back to the ordinary tagged union otherwise.
- **Depends on:** enums (have), generics (have). **No AST-shape change** → low conflict.

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
| 1 | **In-language `Option`/`Result` + niche opt** | flagship "transparent cost"; no shape change | no | M | 🔜 next |
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
