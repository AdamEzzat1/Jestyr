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

Several green, warning-clean, auto-committed increments. **474 tests pass** under
the default `cargo test` (from a 445 baseline: the codegen enablers' goldens, the
combinator/slice/number `core_props` laws, and the wiring tests).

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
  (lazy default via `fn() -> T`), `opt_map` (functor), `opt_map_or`, `opt_ok_or`,
  `opt_and_then` (monadic bind), `opt_filter` (`pred: fn(read T) -> bool`, value
  survives), `opt_or_else` (lazy alternative).
- **Result:** `res_is_ok`/`res_is_err`, `res_unwrap_or`, `res_map` (functor over
  the ok value), `res_map_err`, `res_ok`, `res_and_then` (monadic bind).

### Monadic combinators — the fn-pointer-returning-aggregate typedef reorder

A monadic `f: fn(T) -> Option(U)` is a fn-pointer that returns a generic enum *by
value*, so its typedef must follow a forward declaration of the `Option(i32)`
instance. `gen_forward_types` now forward-declares every generic struct/enum
instance **before** `fn_type_typedefs` (C accepts a forward-declared aggregate as
a fn-pointer return/param type; the bodies follow in `gen_struct_defs`/
`gen_enum_defs`). This unblocked `opt_and_then`/`res_and_then`/`opt_filter` and the
**monad laws** (left/right identity, associativity). Golden
`fn_pointer_returning_a_generic_enum_is_forward_declared_first` pins the ordering;
teeth: neutering `gen_forward_types` fails it.

### Slice / iterator algorithms (no-alloc)

The consuming/searching/in-place algorithms over `[]T` (`examples/std/core.jtr`) —
producers like `map`/`filter` that build a new sequence are the allocating `std`
(Phase 2); a no-alloc `map_into(src, dst, f)` over a caller buffer can follow.

- `sl_fold` (the reducer `sum`/`count`/... specialize to), `sl_count_where`,
  `sl_any`, `sl_all`, `sl_find` (→ `Option(usize)`), `sl_binary_search`
  (→ `Option(usize)`), `sl_swap`, and an in-place **stable, deterministic**
  insertion `sl_sort`. Indices come back as `Option(usize)` so nothing copies a
  (possibly non-`Copy`) element out; the in-place swap is raw-pointer-level (in
  `unsafe`) so it works for a generic `T` without the borrow checker rejecting the
  temporary.
- This needed the same substitution discipline one layer down: a `for x in s` /
  `s[i]` over a generic `[]T` now resolves the slice type through `self.subst`
  (names `JestyrSlice_i32`, not the opaque `JestyrSlice_T`), and `collect_slices`
  walks monomorphized function signatures so a `[]T` parameter contributes its
  concrete typedef even with no local `slice(T, …)` construction (and a generic
  `[]T` *annotation*, a template, no longer emits a bogus `JestyrSlice_T`). Golden
  `generic_slice_algorithm_monomorphizes_to_the_concrete_slice_type`; teeth-verified.
- Iterator laws as oracles (`core_props`): `find` matches `iter().position()`,
  `any`/`all` match the reference; **sort invariants** — sorted permutation,
  deterministic, and **stable** (equal keys keep input order).

### Number parse / format — the determinism deliverable (integer side)

Locale-free, bytewise, deterministic *by construction* (decimal, ASCII `0`–`9`, no
locale, no rounding modes) — so the digit bytes are identical on every platform.
`core` stays no-alloc: `format_*` writes into a **caller `[]u8`** and returns the
byte count.

- `parse_i64`/`parse_u64` (optional sign; overflow is a **defined**
  `ParseIntError::overflow`, never a silent wrap — the signed parser accumulates
  *negatively*, `acc*10 - d`, so `i64::MIN` is representable and the bound is
  checked before each step) and `format_i64`/`format_u64` (digits written
  back-to-front in one pass; `i64::MIN` formatted via an unsigned magnitude).
- This needed a codegen fix: **slice-index assignment** (`buf[i] = v`) now lowers
  to a bounds-checked *lvalue* (`_s.ptr[_ix] = v`), not the rvalue
  statement-expression an `Index` read produces. Golden
  `slice_index_assignment_lowers_to_an_lvalue`; teeth-verified.
- Laws as oracles (`core_props`, against a Rust mirror of the same algorithm):
  `parse(format(x)) == x` for every `i64`; `format_i64(x)` byte-identical to Rust's
  `Display`; differential `parse_i64` vs `str::parse::<i64>()` on arbitrary
  sign/digit/letter input; and the defined-overflow boundaries. End-to-end
  format→parse round-trip in `examples/std/numbers.jtr` (via `from_utf8` over the
  written bytes).

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

- **A nullary/ambiguous generic variant in generic-call argument position needs a
  typed binding.** `res_and_then(i32, i32, i32, ok(20), &f)` fails — a bare `ok(20)`
  pins only `T`, not `E`, and the generic call doesn't propagate the substituted
  parameter type `Result(i32, i32)` onto the argument as its expected type. Bind it
  first (`let r: Result(i32, i32) = ok(20)`). A targeted fix would substitute the
  callee's comptime args into the parameter types before inferring each argument.
- **Combinators are free functions, not methods.** `core.opt_map(i32, i32, o, &f)`,
  not `o.map(f)`. Method-on-generic-enum sugar / UFCS is a compiler feature, not
  library text.
- **`Iterator` / `FromStr` traits are not built.** They need compiler features
  (associated types; static/UFCS dispatch). Slice/iterator algorithms will land as
  **generic free functions over `[]T`** instead of a trait, until then.
- **Float parse/format not started.** Integer parse/format is done; the
  **correctly-rounded float** half (Eisel–Lemire parse + Ryū shortest-round-trip
  format) and the **cross-OS locked-SHA-256 canary** are the remaining marquee
  determinism work — a multi-increment algorithm port (128-bit intermediate math)
  likely to surface compiler gaps to fix along the way.
- **Format needs a caller buffer (no `Display`/`Builder` sugar).**
  `format_i64(n, buf)` returns a byte count into a `[]u8`; viewing it as a `str`
  needs an exact-length `slice(u8, p, n)` + `from_utf8` (range sub-slicing `buf[0..n]`
  on a `[]T` isn't supported in cgen yet — only on `str`).
- **No allocating `std` (Phase 2).** Vec/deterministic-map/String over the
  allocator-as-value are gated on the Drop/allocator session and not begun here.
- **No gcc-in-`cargo test` harness.** Runtime behavior is verified by running the
  example and pinning its output (plus `cgen` goldens on emitted C); the property
  layer asserts on emitted C / a Rust mirror, never compiling the C. (Same posture
  as the Drop/alloc workstream; the `--features c-oracle` harness — needed for the
  cross-OS canary — is shared future work, `DROP-ALLOC-PHASE3.md` future item 3.)

---

## Future plans

In rough priority order toward a usable `core` and self-hosting:

1. ✅ **Done — typedef reorder → `and_then`/`filter` + the monad laws.**
2. ✅ **Done — slice / iterator algorithms** (consuming/searching/in-place):
   `fold`/`count_where`/`any`/`all`/`find`/`binary_search`/`sort`, with iterator
   laws + sort invariants as oracles. (Producers `map`/`filter`/`zip`/`enumerate`
   that build a new sequence — and `map_into` over a caller buffer — remain, the
   former gated on the allocating `std`.)
2b. ✅ **Done — integer parse/format** (`parse_i64`/`parse_u64`/`format_i64`/
   `format_u64`) with round-trip + differential-vs-Rust + defined-overflow laws.
   The slice-index-assignment lvalue lowering landed with it.
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
