# `core` / `std` — Phase 3 (no-alloc `core`, staging `std`)

A handoff for the **standard library** workstream: the no-alloc `core` tier first
(types, traits, Option/Result, slice/iterator algorithms, math, number
parse/format — design §C), with the allocating `std` (Vec/map/String over the
explicit allocator) gated on the Drop/allocator session (`DROP-ALLOC-PHASE3.md`).
Read with [`HANDOFF.md`](HANDOFF.md), [`HANDOFF-NEXT.md`](HANDOFF-NEXT.md) (the
trait epic), and [`docs/TESTING.md`](docs/TESTING.md) §5.14 (the test layer).

Discipline is unchanged: every increment stays `cargo test`-green and
warning-clean (the default suite is toolchain-free), ships its test layers,
teeth-verifies each new property by mutation, and is auto-committed.

---

## Achieved

Two green, warning-clean, auto-committed increments. **456 tests pass** under the
default `cargo test` (up from 448 → 456: +3 cgen goldens, +1 wiring, +7
`core_props`; the +3 over the 445 baseline are the codegen enabler's goldens).

### Reframing what `core`'s "trait surface" actually is

The task plan modeled `core` as Rust-style library text (`Iterator`/`FromStr` as
`.jtr` traits). The tree says otherwise, and verifying first saved building the
wrong thing:

- The **operator/comparison trait surface is already complete** — `Add`/`Sub`/
  `Mul`/`Div`/`Eq`/`Ord` are *synthetic* operator traits (traits Stage E,
  `typeck::register_operator_traits`); a user `trait Add` **collides**. Nothing to
  write.
- Traits are **method-only and receiver-dispatched**: `parse_trait` accepts only
  `fn` signatures (no `type Item` associated types, no generic trait params), and
  there is no `Type::method`/UFCS path. So a Rust-shaped `Iterator` (associated
  `Item`) is **not expressible**, and a static `FromStr::from_str` constructor is
  **not dispatchable**, *as traits* — they need new compiler features, not library
  text.
- What Jestyr *is* strong at — generic structs/enums + monomorphization — is the
  right substrate. So `core` content is **generic free functions**, and the first
  one is the Option/Result combinators.

(Separately: the deterministic-map question — D-HARHT vs a randomized hash map —
is already settled by the in-tree experiment, `docs/TESTING.md` §6: D-HARHT is
*not* the `std` map (4–23× heavier, `u64`-keyed vs `String` tables), determinism
is already guaranteed by `compilation_is_deterministic`, and D-HARHT is parked for
a future self-hosted runtime string-interner/path-index where its byte-first
prefix model pays off. The `std` map should be insertion-ordered / BTree-like.)

### The codegen enabler — generic enums inside generic functions

A generic `Option(T)`/`Result(T,E)` constructed or matched *inside a generic
function* now monomorphizes (commit 1). Five small, coupled, teeth-verified fixes:

- **`cgen::apply_subst` gained its missing `Ty::GenEnum` arm** — it substituted
  `GenStruct` args but fell through `GenEnum` to a clone, so `Option(U)` with `U`
  opaque never became `Option(i32)` under the active monomorphization subst.
- **Both enum-construction paths and `emit_match`** resolve the inferred type
  through `self.subst` before the concreteness check / tag prefix / payload
  binding. Previously a generic body emitted `Jestyr_Option__T_*` or the
  "cannot infer the type arguments of generic enum" diagnostic.
- **typeck `check_fn` seeds `cur_expected = cur_ret`** for the body block, so a
  tail `match`/`if` propagates the return type into its arms — a nullary `none` in
  tail position (`-> Option(U) { match … { none => none } }`) now resolves. (Non-
  tail `let`/`return` save/restore `cur_expected`, so only the tail inherits it.)
- **`collect_fn_types` walks monomorphized generic function signatures**, emitting
  the concrete fn-pointer typedef for a higher-order combinator's `f: fn(T) -> U`
  parameter (previously collected from struct fields only).
- **`ty_mangle` gained `GenEnum`/`Result` arms** (both previously collided on
  `x`), so an `Option(i32)`-returning fn-pointer gets a distinct typedef.

Goldens (`cgen::tests`): `monomorphizes_a_generic_enum_inside_a_generic_function`,
`collects_fn_pointer_typedef_through_a_generic_signature`,
`generic_combinator_lowers_deterministically`. Teeth: disabling the tail-expected
seed or the fn-type loop fails the goldens, then passes on revert.

### Option/Result combinators (no-alloc `core`)

`examples/std/core.jtr` now defines `Option(T)`/`Result(T,E)` and their
allocation-free combinators as monomorphized generic functions (commit 2). No
method syntax on a generic enum, so the receiver is the first value argument and
element/result types are `comptime`: `core.opt_map(i32, i32, o, &f)`. A thin
`fn(T) -> U` pointer carries behavior (a non-capturing closure coerces to one).

- **Option:** `opt_is_some`/`opt_is_none`, `opt_unwrap_or`, `opt_unwrap_or_else`
  (lazy default via `fn() -> T`), `opt_map` (functor), `opt_map_or`, `opt_ok_or`.
- **Result:** `res_is_ok`/`res_is_err`, `res_unwrap_or`, `res_map` (functor over
  the ok value), `res_map_err`, `res_ok`.

### Determinism guarantees realized

- **Combinator lowering is deterministic** — byte-identical C twice, for any
  element type (`core_props::core_option_combinator_is_deterministic`,
  `cgen::generic_combinator_lowers_deterministically`). The new collectors iterate
  ordered `Vec`s and a pure substitution; no `HashMap` iteration order leaks. This
  extends the `compilation_is_deterministic` discipline (§3.1) to the new paths.

### Tests (this workstream)

- **Goldens** (`cgen::tests`): the three enabler goldens above.
- **Laws as oracles** (`proptests::core_props`): functor identity (`map(id)==id`),
  functor composition (`map(g∘f)==map(f) then map(g)`) for `Option` and `Result`,
  `unwrap_or` selection, and the `res_ok(ok_or(o,e))==o` bridge round-trip —
  against a faithful Rust mirror of the combinators' `match` structure.
- **Codegen properties** (`proptests::core_props`): a generic Option combinator
  compiles clean and byte-identically for every integer element type.
- **Wiring** (`module::tests`): `core_combinators_example_compiles_clean` —
  `examples/std/combinators.jtr` resolves across `import "core"` and lowers clean.
- **gcc round-trip**: `jestyrc run examples/std/combinators.jtr` →
  `1, 20, 7, 42, 21, 22, 99, 0, 40, 20` (the `22` is the functor-composition law).

Run them: `cargo test core_props`, `cargo test cgen::tests`. gcc round-trip via
`cargo run -- run examples/std/combinators.jtr`.

---

## Limitations

Honest accounting of what is stubbed, deferred, or only partially covered.

- **No monadic `and_then` / `flat_map` yet.** A combinator whose `f` *returns* a
  generic enum (`fn(T) -> Option(U)`) needs a fn-pointer typedef that returns an
  aggregate by value, which must be emitted *after* that aggregate's typedef. Today
  `fn_type_typedefs()` runs before the generic enum/struct defs (so a struct field
  can be a fn-pointer), creating a forward-reference. C *does* accept a forward-
  declared aggregate as a by-value fn-pointer return type, so the fix is to hoist
  the aggregate **forward-declarations** ahead of `fn_type_typedefs()` (a named-
  struct split of the current `typedef struct {…} X;` emission). Scoped out of this
  pass to avoid a golden-churning reorder mid-stream; it is the next increment, and
  unblocks the monad laws (`m.and_then(some) == m`, `some(x).and_then(f) == f(x)`).
- **No `filter` with value reuse.** `opt_filter` wants `pred: fn(read T) -> bool`
  so the matched value survives the predicate call (a `take`-convention predicate
  would consume it). Deferred with `and_then`.
- **Combinators are free functions, not methods.** `core.opt_map(i32, i32, o, &f)`,
  not `o.map(f)`. Method-on-generic-enum sugar / UFCS is a compiler feature, not
  library text.
- **`Iterator` / `FromStr` traits are not built.** They need compiler features
  (associated types; static/UFCS dispatch). Slice/iterator algorithms will land as
  **generic free functions over `[]T`** instead of a trait, until then.
- **Number parse/format not started.** The marquee determinism deliverable
  (correctly-rounded, locale-free `parse_int`/`parse_float`, shortest-round-trip
  `format_*`, the cross-OS canary) is future work — see below.
- **No allocating `std` (Phase 2).** Vec/deterministic-map/String over the
  allocator-as-value are gated on the Drop/allocator session and not begun here.
- **No gcc-in-`cargo test` harness.** Runtime behavior is verified by running the
  example and pinning its output (plus `cgen` goldens on emitted C); the property
  layer asserts on emitted C / a Rust mirror, never compiling the C. (Same posture
  as the Drop/alloc workstream; the `--features c-oracle` harness is shared future
  work — `DROP-ALLOC-PHASE3.md` future item 3.)

---

## Future plans

In rough priority order toward a usable `core` and self-hosting:

1. **Typedef forward-declaration reorder → `and_then`/`flat_map` + the monad
   laws.** Hoist aggregate forward-declarations before `fn_type_typedefs()`; then
   ship the monadic combinators and add the monad-law oracles to `core_props`.
2. **Slice / iterator algorithms** as generic free functions over `[]T`:
   `map`/`filter`/`fold`/`zip`/`enumerate`/`find`/`all`/`any`/`count`/`sum`;
   `binary_search`; a **deterministic stable `sort`** (laws: output is a sorted
   permutation of the input, and identical across runs). Iterator laws as oracles
   (collect-and-compare against a Rust reference).
3. **Math with defined semantics** (ROADMAP J): `wrapping_*`/`saturating_*`/
   `checked_*`, bit-width-aware, with the overflow behavior pinned by property.
4. **Number parse/format — the determinism deliverable.** Correctly-rounded,
   locale-free, bytewise `parse_int`/`parse_float` (Eisel–Lemire) and
   shortest-round-trip `format_int`/`format_float` (Ryū) into a caller buffer so
   `core` stays no-alloc. Properties: `parse(format(x)) == x`; differential vs
   Rust's correctly-rounded parse / `format!`; a **cross-OS locked-SHA-256 canary**
   on a reference set (the std side of the numerics determinism contract, §3.3 of
   `Jestyr-Remaining-And-Numerics-Research.md`). Transcendentals stay out of scope
   (the numeric stack / SoftFloat).
5. **A real `core`/`std` trait surface, if the compiler grows it.** Associated
   types unlock `Iterator`; static/UFCS dispatch unlocks `FromStr` and `o.map(f)`
   method sugar — each a compiler-feature increment with its own tests.
6. **Allocating `std` (Phase 2), converging with the Drop/allocator session.**
   `Vec(T, A)` (already prototyped there), a **deterministic** insertion-ordered /
   BTree-like map/set (**never** a randomized hash map — it would break
   `compilation_is_deterministic`; and **not** D-HARHT, which is the wrong weight
   class for small `String`-keyed tables — see `docs/TESTING.md` §6), owned
   `String`, explicit allocator-aware file/IO. This is the substrate the
   self-hosted compiler runs on: the symbol tables and node arenas, and the
   compiler driver's I/O.
