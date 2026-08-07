> **Internal development log** — kept for provenance. Status lines and counts in this file reflect the moment it was written, not the current state. Start at the repo [README](../../README.md) for current status and verified claims.

# Drawing on CJC-Lang for Jestyr: Structs/Enums/ADTs, Numerics, and Strings

> Research-and-inspiration notes for three upcoming Jestyr sessions, sourced from the
> CJC-Lang repo at `C:/Users/adame/CJC`. Each section: **what CJC does**, **what to
> borrow**, **what to avoid**, and **how to keep it transparent + provable** (Jestyr's
> identity). File references are `crate/path:line` into the CJC tree so you can dig in.
>
> _Draft — 2026-06-22. Sequencing per your plan: structs/enums first (after the attributes
> session), then numerics, then strings — as separate sessions._

---

## 0. The one framing fact that governs everything

**CJC-Lang is a dynamically-typed MIR tree-walking interpreter, not a static low-level
compiler.** There is no LLVM/Cranelift/native backend; `cjc-mir-exec` walks MIR trees
directly, and the type checker (`cjc-types`) is an *optional gate*, not a prerequisite for
running code. Consequences that color all three sections:

- CJC's **representations are not what Jestyr wants** — structs are `BTreeMap<String,
  Value>` keyed by field name (`cjc-runtime/src/value.rs:171`), enums are string-tagged
  (`value.rs:196`), method dispatch is string-keyed lookup. That's fine for a dynamic
  interpreter; it's the opposite of "transparent cost + provable layout."
- CJC's **concepts and — above all — its determinism engineering are gold.** The
  `struct`/`record`/`class` split, the tiered deterministic-reduction dispatch, the
  owned/view string model, the `@no_gc` escape-analysis contract: these are the
  transferable wins.

So the rule for every borrow below: **take the idea and the determinism discipline;
re-express the representation in Jestyr's static, layout-pinned, provable terms.**

---

# Session 1 — Structs, Enums & ADTs

## 1.1 What CJC does

**Three sibling product types, split at the keyword:**
- `struct` — mutable value type. `cjc-ast/src/lib.rs:146`.
- `record` — **immutable** value type; mutating a field is type error **E0160**.
  `cjc-ast/src/lib.rs:177`, `cjc-types/src/lib.rs:1145`.
- `class` — GC/reference-identity type; the intended path for recursion
  (`class Node { next: Node }`). `cjc-ast/src/lib.rs:162`.

All three share one field grammar with **per-field visibility** (`pub` prefix) and
**field defaults** (`field: Type = expr`). `cjc-parser/src/lib.rs:585,593`.

**Enums are uniform tuple-variant ADTs.** `enum E<T> { Unit, Tup(T1,T2) }`
(`cjc-parser:516`). Variants are unit or positional-tuple only — **no struct-variants**
(`V{x:T}`), **no explicit discriminants** (`Red = 1`), so there's no C-enum/ADT split:
every enum is the same sum-of-products. Generic ADTs exist but payload generics are
modeled as `Type::Unresolved("T")` placeholder strings, not real substitution
(`cjc-types:4247`). `Option`/`Result` are **hardcoded** in the resolver (`cjc-types:4242`),
not defined in a prelude. Each variant is registered as a **constructor function** so
`Some(42)` type-checks through the normal call machinery (`cjc-types:2709`).

**Typing is nominal** (same name = compatible) with HM-style `unify` + occurs check.
Traits exist (`trait`/`impl`, built-in `Numeric → Int/Float → Differentiable`) but
`impl Trait for Type` doesn't fully parse; dispatch is **dynamic, by mangled string name**
`"Type.method"` (`cjc-hir:816`) — no vtables, no monomorphization that matters
(`cjc-mir/monomorph.rs:9` admits generics already "work" via dynamic dispatch).

**Pattern matching:** `match` with wildcard/binding/literal/tuple/struct/variant patterns
and field shorthand (`Point{x,y}`). Exhaustiveness is **name-set coverage** over enum/bool
scrutinees (`cjc-types:3146`, E0130) — **not** Maranget usefulness analysis. **No guards,
no or-patterns, no range patterns, no `..` rest.** Match lowers to a linear arm list
(`cjc-mir:456`), not a decision tree.

**Memory/determinism:** layout is *aspirational* ("stack value type") but actually
heap-backed, name-keyed, dynamically typed. Determinism is bought by the field `BTreeMap`
being alphabetical and by a determinism-first allocator stack (slab + arena + binned,
LIFO) tied to an `@no_gc` contract verified by escape analysis (`cjc-mir/escape.rs`,
`nogc_verify.rs`). `docs/memory_model_2_0.md`.

## 1.2 Borrow for Jestyr

1. **The `struct` / `record` / `class` keyword-level split** of *value / immutable-value /
   reference* semantics. It's clearer than Rust's single `struct` + `&`/`Rc` convention,
   and it maps cleanly onto Jestyr's existing ownership vocabulary. Jestyr already has
   `struct`; add an **immutable `record`** (a struct whose fields are `let`, mutation is a
   compile error — Jestyr can make this a *static* guarantee, not a runtime E0160) and let
   the existing `&`/region/genref machinery cover the `class`-like reference case rather
   than adding a GC type.
2. **Per-field visibility and field defaults** in the declaration grammar — cheap, useful,
   and they compose with the attributes session (`@`-attributes on fields).
3. **Variant-as-constructor uniformity** and the clean nominal `EnumType`/`EnumVariant`
   model — a good shape for Jestyr's checker.
4. **The determinism-first allocator + `@no_gc`-by-escape-analysis idea** — this is the
   single most Jestyr-aligned thing in CJC. It's the same family as Jestyr's region
   arenas. Borrow the *guarantee* ("same alloc sequence ⇒ same slot indices / same
   (page,offset) layout") and the `AllocHint::{Stack,Arena,Rc}` classification.

## 1.3 Avoid / improve on

- **Avoid string-keyed `BTreeMap` structs and string-tagged enums.** Jestyr must have
  **declaration-ordered fields with computable offsets** and **real integer-discriminant
  tagged unions** (with niche optimization where provable). CJC sacrificed layout to get
  determinism via alphabetical maps — Jestyr should get *both*: a deterministic *and*
  explicit, layout-pinned representation. This is exactly where Jestyr's "transparent
  cost" beats CJC.
- **Avoid dynamic string dispatch and dynamically-typed generics.** Monomorphize for real;
  resolve methods statically. (Jestyr already monomorphizes generics in cgen.)
- **Improve match power and rigor.** Add **guards** (they interact directly with
  `invariant`/`variant`), **or-patterns**, **range patterns**, and **`..` rest**, and
  replace name-set exhaustiveness with **Maranget-style usefulness/exhaustiveness over
  nested patterns** (also detects redundant arms). For a provable language this is
  table-stakes, and CJC's gap is a clear opportunity.
- **Add layout attributes** (`@packed`/`@align`/`@layout(c)`) — CJC has *none*. This dovetails
  with the attributes session that's landing first, so structs arrive layout-controllable.
- **Define `Option`/`Result` in-language** over the ADT mechanism, not hardcoded in the
  resolver — so the sum-type machinery is real and self-hosting.
- **Support struct-variant enums** (`V { x: T, y: U }`) and **recursive ADTs** (via an
  explicit indirection — a boxed/region/genref payload), which CJC punts to `class`.

## 1.4 Keeping it transparent + provable

- Field layout is a *declared, inspectable* property (order + offsets + padding visible,
  `@packed`/`@align` to control). The opposite of an alphabetical BTreeMap.
- Enum discriminant is a real integer with a documented representation; pattern match
  lowers to a `switch` on the tag (decision tree), not string compares — and exhaustiveness
  is a *soundness proof*, not name coverage.
- `record` immutability and `@no_gc`/region escape are **static** guarantees with
  diagnostics, extending the escape checker you already have.

---

# Session 2 — Numerics  *(CJC as primary inspiration; improve perf/scale/memory while keeping determinism)*

> CJC's numeric stack is its strongest, most carefully engineered subsystem, built around
> one prime directive — **"same seed ⇒ bit-identical output across runs and platforms"** —
> with the explicit priority order **Determinism > Memory > Latency > Speed**
> (`docs/memory_model_2_0.md:3`). For Jestyr, **adopt the determinism architecture almost
> wholesale**, then take the perf/scale/memory headroom CJC left on the table.

## 2.1 What CJC does — the determinism playbook (borrow this)

The reproducibility is real and multi-layered. Five pillars:

1. **Tiered, strategy-dispatched reductions** — *the centerpiece*
   (`cjc-runtime/src/dispatch.rs`). A `ReproMode {Off, On, Strict}` × `ReductionContext`
   picks an accumulator:
   - **Kahan compensated summation** (serial; order-dependent but deterministic for fixed
     order; O(ε) error). `cjc-repro/src/lib.rs:195`.
   - **Pairwise** (fixed split at `len/2`). `cjc-repro/src/lib.rs:263`.
   - **`BinnedAccumulatorF64` superaccumulator** — the parallel-determinism core: each f64
     is binned by its 11-bit exponent into **2048 fixed bins**; within a bin values share
     magnitude so `a+b == b+a` exactly; **merge uses Knuth 2Sum** so it's *commutative AND
     associative* → any thread/chunk split gives the same result; finalize folds bins in
     fixed ascending-exponent order. Stack-allocated, zero heap. `cjc-runtime/src/
     accumulator.rs:73`. **This is what makes parallel sums bit-identical regardless of
     thread count.**
2. **Explicit no-FMA / no-FTZ / fixed-rounding policy** (`accumulator.rs:39`,
   `tensor_simd.rs:10`): separate mul+add (no `_mm256_fmadd_pd`, which changes rounding),
   subnormals preserved, round-to-nearest-ties-even. Leans on IEEE-754 mandating
   bit-identical `+ - * /` across x86_64/aarch64. SIMD paths are bit-identical to scalar by
   construction. Software two-rounding replaces hardware FMA where a fused op is wanted.
3. **Seeded SplitMix64 RNG with `fork()`** (`cjc-repro/src/lib.rs:50,161`): each
   closure/parallel lane derives its own reproducible stream via `fork()`, so RNG is
   independent of thread scheduling.
4. **No-HashMap policy**: `std::HashMap` (randomized seed) is banned on any
   order-sensitive path; replaced by insertion-order `DetMap` (fixed MurmurHash3 seed) and
   `BTreeMap` fallback. Plus **canonical NaN** (`0x7FF8…` collapse, `cjc-snap/src/
   encode.rs:71`) and little-endian canonical byte encoding for hashing/serialization.
5. **Content-addressed snapshots + CI canaries**: hand-rolled SHA-256 (`cjc-snap`),
   `.snap` manifests with a 32-byte hash, and — critically — a
   `cross-platform-determinism.yml` workflow that runs on **ubuntu + windows + macos** and
   asserts **15 locked SHA-256 canaries** every commit. Reproducibility is *gated*, not
   hoped for.

Plus: a **workload-counted (not wall-clock) energy/cost estimate** so even the cost model
is deterministic (`runtime_policy.rs:36`), and a determinism-preserving perf layer (AVX2
4-wide kernels, rayon behind size thresholds, 64×64 tiling) where **thread count is a
heat/speed axis that never changes the answer** (each output row computed by exactly one
thread; parallel reductions go through the order-invariant binned accumulator).

CJC's tensors: dense `Tensor` is **f64-only, row-major + explicit strides**, COW `Buffer`
(`Rc`, single-thread), zero-copy views (slice/transpose/broadcast set stride=0); multi-dtype
lives in a *separate* byte-store `TypedStorage` (`tensor_dtype.rs:151`). `cjc-ad` is a full
forward+reverse autodiff engine (dual numbers + flat-arena reverse tape) with strictly
ordered, Kahan-reduced, single-threaded gradient accumulation.

## 2.2 Where Jestyr can improve perf/scale/memory **without breaking determinism**

CJC chose Determinism > Memory > Latency > **Speed** (speed last). Jestyr can keep the
determinism guarantee and *raise* the lower three, because most of CJC's perf ceiling comes
from interpreter/representation choices, not from determinism itself:

1. **Statically-typed, multi-dtype tensors instead of f64-only.** CJC promotes everything to
   f64 in the dense path and shoves other dtypes into a byte-store. A Jestyr tensor with
   *native* f32/bf16/f16/i8 storage cuts memory **2–8×** and speeds compute — and dtype does
   **not** change reduction order, so determinism is untouched. (CJC's own quantized→binned
   path already proves the pattern: dequantize integer products directly into the binned
   accumulator.)
2. **A correctly-rounded (or fixed-polynomial) transcendental libm.** CJC's `sin/cos/pow`
   delegate to the platform libm, so cross-platform bit-identity is **not** guaranteed for
   transcendentals — the documented weak link. Jestyr, being provable, can ship its own
   correctly-rounded implementations and *close the gap*: better portability **and** stronger
   determinism. High-value, distinctly Jestyr.
3. **A deterministic *parallel runtime*, not just parallel kernels.** CJC is `Rc`-bound
   (single-threaded) and bolts parallelism onto specific kernels. Jestyr's ownership model
   can prove disjoint writes and drive a general deterministic task graph (fixed reduction
   order via the binned accumulator) — scaling across cores everywhere, not just in
   matmul/element-wise. This is the `for par` story from the loops-future doc.
4. **Real arena-inline storage.** CJC admits arena values "still use `Rc` internally… true
   arena-backed inline storage is a future optimization" and its escape analysis is only
   intraprocedural/conservative (`memory_model_2_0.md:139`). Jestyr's stronger static model
   can do **interprocedural escape analysis** and genuine stack/arena inlining — a pure
   memory/latency win, deterministic by construction, and a natural extension of Jestyr's
   region arenas.
5. **Tiled-matmul numerical seam.** CJC's tiled path uses *naive* accumulation (differs from
   its Kahan sequential path) for cache locality — a documented determinism asterisk. Jestyr
   should use **per-tile Kahan/binned partials** so tiled == sequential bit-for-bit, removing
   the asterisk at negligible cost.
6. **GPU/accelerator offload under a deterministic contract.** CJC has none. A fixed-tile-order
   GPU kernel feeding a host-side binned reduction can add an order of magnitude of scale while
   preserving bit-identity — the hard part (the reduction) is already solved by the binned
   accumulator.

## 2.3 Keeping it transparent + provable

- The reduction strategy is **in the type/contract** (`@reduce(strict)` etc.), so "is this sum
  order-independent?" is visible, not buried in a dispatch table.
- The no-FMA/no-FTZ/round-to-nearest policy becomes a **checkable invariant** of numeric
  codegen — a provable-language can *enforce* it, not just document it.
- Reproducibility is a **proof obligation backed by cross-OS canaries** in CI from day one
  (copy CJC's three-OS locked-hash gate — it's far stronger than unit tests).
- Determinism witnesses (seed lineage via `fork()`, fixed reduction order) are first-class,
  so a numeric kernel can *carry* its determinism proof the way a loop carries a `variant`.

---

# Session 3 — Strings  *(CJC as the main inspiration)*

> Jestyr today has only a bare `str` = `const char*` with `strlen`-based length and byte
> iteration — **no real, length-carrying string type**. CJC's string model is exactly the
> shape to adopt, with a few CJC bugs/gaps to *not* inherit.

## 3.1 What CJC does

**A two-tier owned/view model, mirrored for text and raw bytes** — the most transferable
idea:
- `Str` — owned, heap, **UTF-8, length-carrying (ptr+len), never null-terminated**
  (`Value::String(Rc<String>)`, COW via `Rc`). `cjc-types/src/lib.rs:82`,
  `cjc-runtime/src/value.rs:138`.
- `StrView` — **borrowed, zero-copy, guaranteed-valid-UTF-8 view** into a `Str`.
- `Bytes` / `ByteSlice` — the same owned/view split for raw, unvalidated bytes.
- The owned-vs-view distinction is **first-class in the type system and the memory model**,
  and `StrView`/`ByteSlice` sit in the **`@nogc` layer** — operations on them are
  *statically guaranteed allocation-free* (enforced by escape analysis, `nogc_verify.rs`).

**Encoding:** UTF-8 throughout; validation happens only at the bytes→`StrView` boundary and
returns a rich `Result<StrView, Utf8Error{valid_up_to, error_len}>` (`cjc-eval:3762`). No
normalization (NFC/NFD), no grapheme support.

**Mutability:** strings are value-semantic / functionally immutable — every op returns a
*new* string; clone is an `Rc` bump (O(1)); shared mutation triggers COW.

**Operations** are free `str_*` builtins; producing ops **always allocate** (no rope, no
SSO, no interning, no hidden buffer reuse) — *transparent but `+`-in-a-loop is quadratic*.
Comparison is **bytewise lexicographic** (= codepoint order for valid UTF-8). Rich literals:
plain `"…"`, raw `r"…"`/`r#"…"#`, byte `b"…"` (with `\xNN`), byte-char `b'A'`, **f-strings**
`f"…{expr}…"` (the parser re-lexes/re-parses each hole into a real sub-expression,
`cjc-parser:949`), and regex `/pat/flags` (byte-level NFA).

**Determinism:** formatting leans on Rust's stable `Display` (shortest-round-trip floats,
deterministic across platforms); sort/compare use fixed bytewise `cmp` / `total_cmp`; no
locale collation or case-folding.

## 3.2 Borrow for Jestyr (the future real-string type)

1. **The two-tier owned/view split as the core model** — `str`(owned) vs `strview`(borrowed,
   zero-copy), plus `bytes`/`byteslice`. This makes "does this allocate?" visible **in the
   type** — a perfect fit for Jestyr's transparency goal, and a clean upgrade from
   `const char*`.
2. **Length-carrying, never null-terminated, always UTF-8.** Drop `strlen` entirely; carry
   `len` in the value → O(1) length, embedded-`\0`-safe. (This directly retires the current
   `str.len`-via-`strlen` hack.)
3. **Validate only at the bytes→string boundary**, surfacing the error richly
   (`Utf8Error{valid_up_to, error_len}`) rather than validating everywhere.
4. **Tie the view type to a no-alloc contract** — a borrowed-string whose
   allocation-freedom is *statically proven* by the escape checker. For a provable language
   this is gold, and Jestyr already has the escape machinery (region/genref) to enforce it.
5. **Explicit, costed iteration layers.** Expose bytes / codepoints / graphemes as
   *distinct, clearly-named* iterators (`bytes()`, `codepoints()`, `graphemes()`), each with
   visible cost — and learn from CJC's **inconsistency footgun** (its `len` = bytes while
   `str_chars`/`str_substr` = codepoints). Name them unambiguously (`len_bytes` vs
   `count_chars`). This also fulfills the deferred `for cp in text.codepoints()` loop form.
6. **No implicit `s[i]` integer indexing** (CJC refuses it) — avoids the entire byte-vs-char
   ambiguity bug class. Keep slicing explicit and codepoint/grapheme-aware with documented
   clamping.
7. **Transparent allocation + an explicit builder.** Keep "producing ops allocate" honest
   (no hidden rope/SSO), but add an explicit, visibly-costed `StringBuilder`/buffer for
   amortized append, so loop-concatenation isn't silently quadratic.
8. **Deterministic, locale-free formatting from day one**: shortest-round-trip floats,
   bytewise (codepoint-order) comparison, no locale collation — matches both CJC's
   determinism priority and Jestyr's provability aims.
9. **f-strings via re-parsed holes** (`f"…{expr}…"`) is a clean, transparent interpolation
   design worth copying — each hole is a real checked sub-expression, not string soup.

## 3.3 Avoid / fix what CJC got wrong

- **Do NOT inherit CJC's `lex_string` Latin-1 bug.** CJC builds string-literal values with
  `value.push(ch as char)` *per byte* (`cjc-lexer:978`, and the f-string/raw paths too), so
  multi-byte UTF-8 inside a `"…"` literal is corrupted (each byte reinterpreted as Latin-1).
  Jestyr's string lexer must **decode UTF-8 properly** (or copy the raw byte span verbatim).
- **Add richer escapes** — CJC's plain strings support only `\n \t \r \\ \" \0` (no `\xNN`,
  no `\u{…}`). Jestyr should support hex and Unicode escapes in normal strings.
- **Add first-class fallible `str → int/float` parsing** (`parse(...) -> Result`) — CJC has
  essentially no user-facing number-parsing builtin; a clear gap to fill.
- **Decide the interpreter/compiler view honesty up front.** CJC's `StrView`/`ByteSlice` are
  documented as *owning snapshots* in the interpreter and only truly zero-copy in the
  MIR/compiler path (`value.rs:141`). Jestyr is a compiler — make views *actually* zero-copy
  from the start, backed by the borrow checker.

## 3.4 Keeping it transparent + provable

- Owned vs borrowed (and thus allocation behavior) is encoded in the type, checked by the
  escape system you already have; `strview` operations are *proven* non-allocating.
- Iteration granularity (bytes/codepoints/graphemes) is explicit and individually costed —
  no hidden re-encoding, no surprise O(n) on a "length" call.
- Formatting/comparison are locale-free and deterministic, so text behavior is reproducible
  and provable — consistent with the numerics determinism story above.

---

## Appendix — CJC files worth reading first

| Area | Start here (in `C:/Users/adame/CJC`) |
|---|---|
| Structs/enums/ADTs | `crates/cjc-ast/src/lib.rs` (`:146` struct, `:177` record, `:162` class, `:191` enum, `:717` patterns); `crates/cjc-types/src/lib.rs` (`:1122`,`:1166` reprs, `:3146` exhaustiveness); `crates/cjc-runtime/src/value.rs` (`:171`,`:196` runtime layout — the *anti-pattern*) |
| Determinism core | `crates/cjc-runtime/src/{accumulator,dispatch,runtime_policy}.rs`; `crates/cjc-repro/src/{lib,kahan}.rs`; `crates/cjc-snap/src/{hash,persist}.rs`; `.github/workflows/cross-platform-determinism.yml` |
| Tensors / autodiff | `crates/cjc-runtime/src/{tensor,tensor_simd,tensor_dtype,quantized}.rs`; `crates/cjc-ad/src/lib.rs` |
| Strings | `crates/cjc-types/src/lib.rs:82`; `crates/cjc-runtime/src/{value.rs:138, builtins.rs (str_*)}`; `crates/cjc-eval/src/lib.rs:3762` (UTF-8 validate); `crates/cjc-lexer/src/lib.rs` (`:931` strings, `:987` f-strings — and the `:978` Latin-1 bug); `crates/cjc-parser/src/lib.rs:949` (f-string parse) |
| Cross-cutting | `docs/memory_model_2_0.md`, `docs/CJC_Syntax_and_Types.md`, `docs/mathematics_hardening_phase/` |
