# Jestyr — Testing & QA Handoff

A consolidated view of **what exists** and **how it is verified**, plus the *stricter
layer* of property / fuzz / benchmark testing that wraps the whole compiler. Read with
[`../HANDOFF.md`](../HANDOFF.md) (the feature-level handoff, §5 gotchas) and
[`structs-enums-design.md`](structs-enums-design.md).

> Snapshot: ~17.4k lines of Rust, **285 in-crate unit tests** + `proptest` properties +
> `bolero` fuzz targets, **77 example programs** (67 single-file + 10 multi-file) that each
> run natively or are correctly rejected. Build is warning-clean.

---

## 1. The test surface — everything that must keep working

### 1.1 Compiler stages (`src/`)

| Stage | Module | What it must guarantee | Unit tests |
|---|---|---|---|
| Lex | `lexer.rs` | total on any input; spans in-bounds & on char boundaries; ends in one `Eof`; doc-comment trivia | 17 |
| Parse | `parser.rs` | total; error-recovery never loops; every AST node spans a real source range | 41 |
| Resolve + typeck | `typeck.rs` | names/types/methods; exhaustiveness (Maranget); visibility; lenient elsewhere | 33 |
| Ownership/escape | `escape.rs` | the 4 escape routes + region-escape proof; no false positives on valid code | 32 |
| C codegen | `cgen.rs` | every supported construct lowers to compilable C; unsupported → a diagnostic, never bad C | 106 |
| Load (multi-file) | `module.rs` | import resolution, cycle detection, shared-arena merge, global spans | (via examples) |
| Pipeline (whole) | `proptests.rs` | **never panics on any input**; (new) **deterministic** | properties + fuzz |

Support modules: `span`, `token`, `diag`, `doc`, `ast`, `attrs`, `printer`, `types`, `main`.

### 1.2 Language features (each has a runnable `examples/*.jtr` proof)

- **Types:** structs, immutable `record`, enums (tagged), generic structs/enums, `distinct`,
  untagged `union`, niche-optimized `Option`, recursive ADTs (`indirect`).
- **Struct/enum/ADT substrate (§2.x):** field defaults, struct spread, per-field visibility
  (`pub x`), opt-in `Copy`, bit-fields, struct-variant syntax, explicit discriminants.
- **Match power (§2.4):** guards, or-patterns, ranges, `..` rest, nested-pattern dispatch,
  Maranget exhaustiveness + redundant-arm warnings.
- **Ownership/refs (D):** MVS default-`read`, `take`/`mut`/`out`, generational `&T`, region
  `&[r]T`, the escape checker's 4 routes.
- **Strings (E):** `str`/`cstr`/`String`/`Builder`/`Bytes`/`os_str`/`Cow`; views, slicing,
  iterators (bytes/codepoints/graphemes/split), operations, UTF-8 validation (trap +
  recoverable), f-strings, region strings + the **region-escape static proof**.
- **Generics:** monomorphization (functions, structs, enums, methods).
- **Layout/bare-metal:** `@packed`/`@align`/`size_of`/`align_of`/`offset_of`, `@volatile`,
  `@address`, slices `[]T` + bounds checks + refinement elision.
- **Concurrency / interop / contracts:** `concurrent`/`spawn`, `extern "c"`, `requires`/
  `ensures`, the attribute registry, doc comments, `@test`/`@bench` runner.

---

## 2. Existing test layers

1. **Unit / golden** (`#[cfg(test)] mod tests` in each module). cgen tests assert *substrings*
   of emitted C; typeck/escape tests assert diagnostics. 285 total.
2. **Example corpus** (`examples/`). The `run`/`check`/`test` demos are executable proofs; a
   regression is a demo that stops producing its documented output.
3. **Property tests** (`proptests.rs::prop`, `proptest`): totality of lexer + pipeline, span
   invariants, "a generated valid program parses clean", lexer token-shape laws.
4. **Fuzz** (`proptests.rs::fuzz`, `bolero`): the pipeline on `String`, the lexer on `Vec<u8>`;
   replayed under `cargo test`, run for real under `cargo bolero test <name>`.

---

## 3. The stricter layer (this workstream)

The additions, by intent:

### 3.1 Determinism (the headline)
A *deterministic language* needs a deterministic compiler: the **same source must emit
byte-identical C** every run, and re-running any stage must be stable. The risk is
`HashMap`/`HashSet` iteration order leaking into output. The property test
`compilation_is_deterministic` compiles random + generated-valid programs twice and asserts the
emitted C (and diagnostics) are identical — across many shrunk inputs, not one hand-picked file.

### 3.2 Per-stage totality & well-formedness
- Each stage is independently total on adversarial input (not just the whole pipeline).
- Emitted C is *structurally* well-formed: balanced `{}`/`()`/`[]`, starts with the prelude,
  never contains an un-substituted sentinel.
- `print_ast` is total and stable (re-printing is idempotent).

### 3.3 Metamorphic properties
- **Whitespace insensitivity:** inserting spaces/newlines between tokens doesn't change the
  token-kind sequence (the lexer discards layout).
- **Comment insensitivity:** adding `//` comments doesn't change the emitted C.

### 3.4 Richer generators
`arb_program` builds *valid* programs over structs/enums/functions/match (not just arithmetic),
so typeck/escape/cgen are exercised in depth — asserting total + deterministic + (valid →) clean.

### 3.5 Benchmarks (speed + memory)
A `jestyrc selfbench` subcommand compiles a large generated program and reports **per-stage
throughput** (lex/parse/typeck/escape/cgen, lines & tokens per second) and an **AST/output
footprint** (token count, arena sizes, emitted-C bytes). A feature-gated counting allocator
(`--features bench-alloc`) reports **peak / total heap bytes** for a full compile — no new deps.

Sample (1501-line generated program, ~25.5k tokens, release build):

```
    total     14.155 ms    (106040 lines/s, 1802264 tokens/s)
  memory (one full compile): peak 4054 KiB resident, 9148 KiB total allocated
```

Per-stage breakdown shows where time goes (typeck + cgen dominate); the numbers are a
*regression baseline* — a future change that halves throughput or doubles peak memory is
visible at a glance. (Run `--release` for representative speed; `dev` is `opt-level = 0`.)

---

## 4. How to run

```sh
cargo test                       # unit + property + replayed fuzz corpus (430+ tests)
cargo test prop::                # just the property tests
cargo test drop_props            # Drop/RAII scope-exit drop-glue properties (Phase 3)
cargo test alloc_props           # @no_alloc soundness/completeness properties (Phase 3)
cargo test core_props            # core Option/Result combinator laws + codegen (§5.14)
cargo bolero test fuzz_pipeline  # real coverage-guided fuzzing of the pipeline
cargo bolero test fuzz_drop_alloc_pipeline      # fuzz Drop/RAII + allocator lowering
cargo run -- selfbench           # per-stage speed + footprint on a generated program
cargo run --features bench-alloc -- selfbench   # + peak/total heap bytes
cargo run -- test examples/tests_demo.jtr       # the in-language @test/@bench runner
cargo run -- emit-c examples/drop.jtr --show-drops   # inspect where drop glue is inserted
```

Discipline (unchanged): every increment stays `cargo test`-green and warning-clean; new
invariants live in `proptests.rs`, new goldens in the relevant module's `mod tests`.

---

## 5. Expanding the layer — feature by feature

A roadmap for pushing property + `bolero` coverage into *every* shipped feature. Each row is a
concrete invariant or generator someone can implement; "P" = `proptest` property, "F" = `bolero`
fuzz target, "G" = a generator to build first. The unifying tactic: **build a generator of *valid*
programs for a feature, then assert an invariant that must hold for all of them** — the strongest
test a feature can have, far beyond the per-feature golden examples.

### 5.1 Lexer / parser (deepen what exists)
- **P — round-trip:** `tokens(src)` reconstruct the source up to trivia; every token's
  `src[span]` re-lexes to the same single token.
- **P — parser totality on token soup:** generate random *token* sequences (not char soup) and
  assert `parse` never panics and recovers (already partially covered; tighten to assert the AST
  is non-empty when the stream is).
- **P — `print → parse → print` idempotence:** for a generated valid program, re-parsing the
  printer's output yields a structurally-identical AST (printer/parser are inverse on the valid
  subset).
- **F:** fuzz the parser on `Vec<Token>` directly (skip the lexer) to hammer recovery paths.

### 5.2 Loops (`for`, ranges, slices, zip, labels, `break`/`continue`)
- **G — `arb_loop`:** range / inclusive / slice / zip / `for _` / infinite+break, with optional
  `invariant`/`variant`, labels, and a step.
- **P — bounds-elision soundness:** a generated `for i in 0..xs.len { xs[i] }` emits **no**
  `assert(` (elided), while `for i in 0..n { xs[i] }` (unproven) keeps the bounds check — a
  metamorphic pair.
- **P — iterator-invalidation is rejected:** any generated loop that mutates its iterated
  collection in the body produces a diagnostic (the borrow contract holds for *all* shapes).
- **P — determinism:** loop lowering is byte-stable (already covered by the global determinism
  property, but worth a focused generator).

### 5.3 Strings (the largest new surface)
- **G — `arb_str_program`:** literals (incl. `\x` escapes), slicing `s[i..j]`, the iterators
  (`bytes`/`codepoints`/`graphemes`/`split`), operations (`find`/`trim`/`eq`/…), validation,
  `Builder`, f-strings, `Cow`, `os_str`.
- **P — UTF-8 invariant:** for any `bytes` input, `from_utf8`/`try_from_utf8` only ever yield a
  `str` whose bytes are valid UTF-8 (cross-check the emitted runtime against a Rust
  `std::str::from_utf8`).
- **P — slice boundary law:** `s[i..j]` either returns a view whose ends are on char boundaries
  or traps — never a half-codepoint (differential vs Rust slicing).
- **P — `len ≥ count_codepoints ≥ count_graphemes`** for every generated string (the cost-tiers
  are monotone).
- **P — split/concat round-trip:** `join(split(s, sep), sep) == s` for non-empty `sep`.
- **P — `eq`/`eq_fold` laws:** reflexive, symmetric; `eq ⇒ eq_fold`.
- **F:** fuzz `from_utf8`/`substr`/`split` lowering on `Vec<u8>` → assert the emitted C compiles
  (gcc) and the runtime matches a Rust oracle for a sample of inputs.

### 5.4 Attributes (registry-validated)
- **G — `arb_attr`:** every attribute × every legal/illegal target × arg shapes.
- **P — registry totality:** an attribute on a *legal* target with a *legal* arg shape never
  errors; on an illegal target/arg it *always* errors (the registry is a total function).
- **P — "did you mean":** a one-edit typo of a known attribute always suggests the original.
- **P:** attributes are ABI/hints only — adding any valid attribute never changes program
  *behavior* (the emitted C differs only in `__attribute__` clauses, not logic).

### 5.5 Structs / enums / ADTs / records / unions / distinct
- **G — `arb_type_decl`:** struct/record/union/enum (incl. generic, niche, recursive `indirect`)
  with field defaults, bit-fields, visibility, `@copy`.
- **P — `size_of`/`align_of`/`offset_of` agree with C:** emit a program that prints them and
  compare to a tiny C oracle (or `std::mem` for the mapped Rust types).
- **P — niche law:** `size_of(Option(*T)) == size_of(*T)` for any thin-pointer `T` (the niche
  proof generalized).
- **P — record immutability:** any generated field assignment on a `record` is rejected; the same
  on a `struct` is accepted.
- **P — construction round-trip:** `let x = T{…}; x.f` returns the constructed field value
  (positional *and* named `T{ f: … }`), for generated field sets.
- **P — exhaustiveness ⇔ Maranget:** a generated `match` is accepted iff a reference usefulness
  oracle says it's exhaustive; redundant arms always warn.

### 5.6 Match power
- **G — `arb_pattern`:** wildcard / binding / variant / nested / literal / range / or / `..` rest /
  struct-variant, over a generated enum.
- **P — exhaustiveness soundness:** if typeck accepts a guard-free `match`, then for *every* value
  of the scrutinee type some arm matches (check against an enumerated value space for small enums).
- **P — redundancy:** an arm that duplicates an earlier one always warns; reordering arms changes
  *which* arm warns but not *whether* (metamorphic).
- **P — nested-dispatch correctness:** a generated nested match compiles and, at runtime, routes
  each constructed value to the arm a reference matcher picks.

### 5.7 Ownership / escape (the language thesis)
- **G — `arb_borrow_program`:** functions with `read`/`mut`/`out`/`take` params that variously
  return / store / capture / give-away their borrows.
- **P — soundness direction:** every program the escape checker *accepts* has no borrow outliving
  its frame (check against a reference dataflow oracle on the small generated subset).
- **P — completeness direction:** each of the 4 routes + the 2 region routes, when generated
  deliberately, is always rejected.
- **P — `@copy`/Copy refinement:** marking a generated aggregate `@copy` flips exactly the
  "return a value param" diagnostics off and nothing else.
- **P — region proof:** a generated `region` value that escapes (return *or* assign-to-outer) is
  always rejected; one that stays in scope is always accepted.

### 5.8 Generics / monomorphization
- **G — `arb_generic_use`:** generic fns/structs/enums/methods instantiated at varied type sets.
- **P — instance completeness:** every concrete instantiation reachable from `main` appears
  exactly once in the emitted C (no missing instance — the §5.28 walker bug class — and no dup).
- **P — erasure:** comptime type params never appear in a runtime signature.
- **P — determinism of mangling:** instance C names are a pure function of `(ctor, type-args)`,
  order-independent.

### 5.9 Modules / visibility
- **G — `arb_module_graph`:** N files with imports (incl. diamonds), `pub`/private items.
- **P — cycle detection:** any generated import cycle is a hard error; any DAG loads once.
- **P — visibility:** a cross-module reference to a private item/field is always rejected; to a
  `pub` one always accepted; same-module always accepted.
- **P — flat-merge determinism:** the merged program (and its emitted C) is independent of import
  *discovery order* for a fixed graph.

### 5.10 Determinism & the compiler itself (deepen §3.1)
- **P (have):** `compile(s) == compile(s)`. **Strengthen to:** `compile(s)` is identical across a
  fresh process / different `RUST_MIN_STACK` / shuffled item order where semantics allow.
- **P — order-independence:** permuting top-level *independent* items doesn't change the emitted C
  (modulo the items' own order in the file) — flushes out any hidden iteration-order dependence.
- **P — stage purity:** `typeck::check` and `escape::check` never mutate the AST (annotate-don't-
  mutate); assert the AST is bit-identical before/after.
- **F (have):** `fuzz_determinism`. Add a long `cargo bolero` soak in CI.

### 5.11 Benchmarking — speed & memory (deepen §3.5)
- **Per-feature micro-benches** in `selfbench`: a generator knob to emit string-heavy /
  generic-heavy / match-heavy / deeply-nested programs, so each subsystem's throughput is tracked
  separately (today it's one mixed program).
- **Memory efficiency:** beyond peak/total heap, report **bytes-per-AST-node** and
  **emitted-C-bytes-per-source-line** — efficiency ratios that catch bloat a raw peak misses.
- **CI budget:** wire `selfbench --release` into a canary that fails if throughput drops below a
  floor or peak memory rises above a ceiling — turning the §3.5 baseline into an enforced budget.
- **Allocation count:** extend the `bench-alloc` allocator to also count *number* of allocations
  (not just bytes) — a proxy for allocator pressure that the arena-AST design aims to keep low.

### 5.12 Traits / interfaces (in progress — the biggest remaining gap)

Built stage-by-stage (parse → resolve+coherence → static dispatch → bounds → operators →
`dyn`); each increment ships unit + property + bolero coverage and stays green/warning-clean.
Run: `cargo test trait_programs`, `cargo test parses_a_trait`, `cargo bolero test fuzz_traits_pipeline`.

- ✅ **Stage A — parse + represent.** `trait`/`impl`/`dyn Trait`/`[T: Bound]` parse into the AST
  (`Item::Trait`, `Item::Impl`, `TypeKind::Dyn`, `FnDecl::generics`); printer/doc stay idempotent;
  the whole pipeline stays **total** (later stages ignore trait/impl items, `dyn` lowers to an
  opaque placeholder — *no semantics yet*).
  - Unit: trait (required vs default methods), impl block, bounded generic, `dyn` type, print
    round-trip, malformed-body recovery (parser/printer `mod tests`).
  - Property (`prop`): `trait_programs_parse_clean`, `..._are_total_and_deterministic`,
    `..._print_stably` over the `arb_trait_program` generator (trait + impl + bounded use + `dyn`).
  - Fuzz (`fuzz`): `fuzz_traits_pipeline` — fuzz bytes inside an `impl`/bounded-generic body;
    total + deterministic.
- ✅ **Stage B — resolve + coherence.** typeck registers each `trait` (method set) and `impl`
  (`GlobalTable::{traits, impls, impl_index}`), enforcing **coherence**: unknown trait, duplicate
  `impl` per `(trait, type)`, missing required method, non-member method. A `recv.m(args)` call
  resolves through `impl Trait for <recv-type>` (`resolve_impl_method`, recorded in `impl_calls`
  for the backend) and types as the impl method's return.
  - Unit (typeck): each coherence diagnostic, a defaulted method may be omitted, and a resolved
    call types by the impl return + is recorded.
  - Property (`prop`): `duplicate_impl_is_always_a_coherence_error` (soundness),
    `distinct_type_impls_are_accepted` (completeness), `coherence_verdict_is_order_independent`.
- ✅ **Stage C — static, monomorphized dispatch.** The backend consumes `impl_calls`: each
  `impl Trait for Type` method is emitted as a mangled top-level C function
  (`jestyr_impl_<Trait>__<TypeKey>__<method>`, the receiver as the first parameter, reusing the
  struct-method `self` machinery), and a resolved `recv.m(args)` lowers to a **direct** call of it
  (no vtable — the target is fixed at compile time by the receiver's type key). Impl bodies are
  walked by the instance collectors, so generic calls/structs inside them instantiate correctly.
  - Unit (cgen): primitive + struct receivers, an explicit argument threaded after the receiver,
    `mut self` by pointer, and two distinct impls → two distinct mangled symbols.
  - Property (`prop`): `trait_call_lowers_to_a_direct_impl_method_call` (the call targets the
    mangled symbol, every receiver type), `trait_call_programs_compile_deterministically`.
  - Fuzz (`fuzz`): `fuzz_trait_static_dispatch` — fuzz bytes fill the now-emitted impl body while a
    real `x.m()` drives dispatch; total + deterministic.
  - gcc differential: `examples/traits_static.jtr` runs to `42/141/15/18`
    (`traits_static_example_compiles_clean`).
- ✅ **Stage D — definition-site bounds.** A bracket-form bound `[T: Tr]` is checked in two halves,
  both typeck-only (no codegen — bracket generics aren't monomorphized yet, so there's no runtime
  differential): **declaration** — every bound names a registered trait (`check_bound_traits_declared`,
  over free fns + impl/struct methods); **call-site obligation** — at each call to `f[T: Tr](…)`,
  the concrete `T` is recovered by unifying declared params against the actual arg types
  (`unify_tp`) and must `impl Tr` (`check_call_bounds`, reusing `impl_index`); an unsatisfied bound
  errors at the call. Unknown-bound and unresolved/opaque `T` are skipped in the call-site check
  (the former is the declaration check's job; the latter avoids a false positive).
  - Unit (typeck): unsatisfied bound errors, satisfied bound is clean, unknown-trait bound errors at
    the definition, a struct receiver satisfies via its impl, an unbounded `[U]` imposes nothing.
  - Property (`prop`): `unsatisfied_bound_always_errors_at_the_call` (soundness, distinct
    impl/call types), `satisfied_bound_never_errors_at_the_call` (completeness).
  - Fuzz (`fuzz`): `fuzz_definition_site_bounds` — fuzz bytes in a bracket-generic body, a real call
    drives `check_call_bounds`; total + deterministic.
- ✅ **Stage E — operator traits.** The built-in operators desugar to synthetic-trait methods. Six
  *primitive* methods are implemented directly — `+`→`Add::add`, `-`→`Sub::sub`, `*`→`Mul::mul`,
  `/`→`Div::div`, `==`→`Eq::eq`, `<`→`Ord::lt` (`register_operator_traits`) — and the four remaining
  comparisons are **derived** at lowering by a swap/negate (`!=`→`!eq`, `>`→swapped `lt`, `<=`→`!`
  swapped `lt`, `>=`→`!lt`), so a user type gets the full set from those six impls. A binary
  op whose left operand is a *user type* resolves through `impl <OpTrait> for <lhs>`
  (`resolve_operator_trait`, recorded in `impl_calls` keyed by the binary expr) and lowers to a direct
  impl-method call (`emit_operator_call`, reusing the Stage C path). Result type = the impl method's
  return (`Add`/`Mul` → the type, `Eq`/`Ord` → `bool`); a user type used with the operator but lacking
  the `impl` is an error; primitives keep native semantics. The **`f64` no-FMA determinism seam** is
  the gcc flag `-ffp-contract=off` (forbids `a*b+c` → fused multiply-add, for bit-reproducibility).
  - Unit (typeck): arithmetic op → the type + recorded, comparison op → `bool`, `-`→`Sub`, the derived
    `>`/`!=` resolve through `Ord`/`Eq`, missing-impl error, primitives untouched, a user `trait Add`
    collides with the reserved built-in.
  - Unit (cgen): base ops lower to `jestyr_impl_<Trait>__V__<m>(j_a, j_b)`; `-`/`/` → `Sub`/`Div`; the
    derived comparisons lower via swap/negate (`>`→`lt(j_b,j_a)`, `!=`/`<=`/`>=` negated); primitives native.
  - Property (`prop`): `an_operator_on_a_user_type_lowers_to_its_impl_call` over all **ten** operators
    (swap-aware expected symbols), `operator_programs_compile_deterministically`. Fuzz: `fuzz_operator_traits`.
  - gcc differential: `examples/operators.jtr` runs to `13/1/42/6/0/1/1/0/1/0`
    (`operators_example_compiles_clean`).
- ✅ **Bracket-generic codegen** (not a trait stage, but unblocks the next): bracket-form `[T:
  Bound]` generics now monomorphize — each `T` is *inferred* from the call's value arguments
  (`unify_tp`, shared between typeck and cgen) and a mangled instance `jestyr_<name>__<targs>` is
  emitted. `monomorphize_ret` infers the return too. Mixing `comptime T: type` + bracket `[U]` in one
  signature works (type args assembled `comptime ++ bracket`). Unit (typeck return-inference; cgen
  instance emit, per-type instances, multi-param mangle, comptime+bracket mix), property + determinism
  over `arb_bracket_generic_program`, gcc round-trip `examples/bracket_generic.jtr` (`42/0/99`).
- ✅ **Body-side bound enforcement (the "Zig fix", design §8.2):** inside `f[T: Tr]`, a method call
  on a `T` value resolves through the bound (`resolve_bound_method`) — a non-bound method (or any
  method on an unbounded `[U]`) is a *definition-site* error; the call types by the trait method's
  return and is recorded in `bound_method_calls`. cgen selects the concrete `impl` per monomorphized
  instance via the active `subst`, so one `x.m()` lowers to different impls per instantiation. Unit
  (typeck: bound method typed, non-bound + unbounded errors; cgen: per-instance dispatch asserting
  the *call*, not the impl def), property + determinism over `arb_bound_method_program`,
  `fuzz_bound_method_calls`, gcc round-trip `examples/bound_method.jtr` (`42/70`).
- ✅ **Stage F — `dyn Trait` vtable.** `dyn Trait` lowers to a `{ void* data, const JestyrVtable_<T>*
  vtable }` fat pointer; `cgen::dyn_typedefs` synthesizes the vtable struct (one fn-ptr per method,
  receiver erased to `void*`) + the fat-pointer typedef, and `cgen::dyn_vtables` emits a per-method
  shim (casting `void* self` back to the concrete type) + a `static const` vtable instance per `impl`.
  A concrete value coerces in (`typeck::record_dyn_coercion`, at call-arg/`let`/`return`; verifies the
  impl; backend builds the fat pointer with a block-scoped compound literal so the erased data has a
  valid address), and `d.m(args)` dispatches through the vtable slot (`resolve_dyn_method` →
  `dyn_calls`; `d.vtable->m(d.data, args)`) — run-time impl selection from *one* function. Unit
  (typeck: dispatch typed + recorded, coercion recorded, missing-impl/non-trait-method errors; cgen:
  vtable struct/typedef/shim/instance + dispatch + coercion, one function → two vtables), property +
  determinism over `arb_dyn_program`, `fuzz_dyn_dispatch`, gcc round-trip `examples/dyn_dispatch.jtr`
  (`42/70/70`).
- ✅ **The trait epic (A–F) is complete.**

### 5.13 Drop / RAII + allocators (design Phase 3)

Deterministic, drop-flag-free destructors and the enforced allocation-free contract. The oracle for
every drop property is **known by construction**: a generator builds a program with a known number
of owned, non-moved droppables, and the property asserts the emitted C holds exactly that many drop
*call sites* — `0` is a missed drop (leak), `≥2` is a double-free. All pure-Rust (scans emitted C),
so they run under the toolchain-free default `cargo test`.

- ✅ **Scope-exit drop glue (Increment 1).** A local of a type with an `impl Drop for T` is dropped
  at scope exit in **reverse declaration order**, with no runtime drop flags — liveness is static
  because the ownership model makes moves syntactic. cgen keeps a drop-scope stack (one per `{ }`
  block); a local registers *as its `let` is emitted*, so an early `return` never drops a not-yet-
  declared local. A `return` spills its value to a temp, runs drops, then returns (no use-after-drop).
  Unit goldens (`cgen::tests`: call emitted, reverse order, move elision, `--show-drops` comment, no
  glue without an impl); properties (`drop_props`: drops-each-owned-exactly-once,
  move-elision, no-double-free, determinism); `fuzz_drop_alloc_pipeline` / `_determinism`. gcc round-
  trip `examples/drop.jtr` (`100, 2, 1, 200, 300, 7`). **Teeth:** suppressing the emitter fails the
  count properties + goldens, then passes on revert.
- ✅ **Move analysis (drop-after-move elision).** `cgen::collect_moved` over-approximates (leak-safe):
  a local that is returned, passed by value to a call, captured into a struct, rebound, or used as a
  `take self` receiver is **not** dropped at its origin — the new owner drops it, so no double-free
  by construction. Property `moved_out_value_is_not_dropped` (the returned local `x0` is never dropped).
- ✅ **`--show-drops` inspection.** `jestyrc emit-c <f> --show-drops` annotates each inserted drop with
  a `/* drop j_x : T */` comment — implicit is not hidden. Golden `show_drops_annotates_the_glue`.
- ✅ **Manual-`drop()` rejection.** A `value.drop()` call (resolving through `impl Drop`'s `drop`) is a
  compile error — the auto-drop would double-free. Unit `cannot_call_drop_manually`.
- ✅ **Region-integrated bulk drop (Increment 4, partial).** A value owned by a `region { }` block emits
  **zero** per-value drop glue — the arena reclaims it in bulk; the region *determines* the drop
  strategy. Metamorphic golden + property `region_owned_value_emits_no_drop_glue`: the same droppable
  emits one drop in a plain block and zero inside a `region`.
- ✅ **`@no_alloc` — the enforced contract (Increment 3).** A `@no_alloc` function must be *proven*
  allocation-free by the escape checker, or it is a compile error (the `@no_panic` analog). Rejects a
  call to any allocation intrinsic (`alloc`/`realloc`/`arena_*`/`region_*`/`gen_new`, bare or
  module-qualified), a `region { }` block, and a region-scoped `for`. Unit (`escape::tests`: rejects
  heap alloc / region; accepts a clean body; per-function, not inherited); property `alloc_props` —
  soundness **and** completeness against an independent oracle (rejected *iff* it allocates, no false
  positives). **Teeth:** neutering `is_alloc_intrinsic` fails the rejection tests, then passes on revert.
- ✅ **fn-pointer-vtable `Allocator` + `Layout` (Increment 5).** Zig's `std.mem.Allocator` shape as a
  real value (opaque `ctx` + thin `alloc_fn`/`free_fn` pointers), retiring the enum stand-in. One path
  (`alloc_n`/`free_n`) runs over any allocator: `a.alloc_fn(a.ctx, layout)` lowers to an **indirect**
  call, not a bare malloc. Golden `allocator_value_routes_through_the_vtable_not_bare_malloc`; gcc
  round-trip `examples/alloc_vtable.jtr` (`10/20/30/40`, system + arena over one path).
- ✅ **Take-vs-borrow move precision (Increment 5).** `collect_moved` moves a call argument **only** at a
  `take` parameter; a `read`/`mut`/`out` borrow does not — so a droppable mutated via `mut` methods
  still drops once. Goldens `mut_borrowed_droppable_still_drops_at_scope_exit` /
  `taken_droppable_is_not_dropped_by_caller`; property `borrow_passed_droppable_still_drops_once`.
  **Teeth:** reverting to "all args move" fails the seam tests, then passes on revert.
- ✅ **Allocator-parameterized `Vec`, freed by RAII (Increment 6).** `IntVec` stores its `Allocator`,
  grows via the vtable, and `Drop` frees the buffer at scope exit *through the stored allocator* — the
  integration forcing function. gcc round-trip `examples/vec_alloc.jtr` (`5/10/50/99`, then auto-free).
- ✅ **Generic `Vec(T, A)` + generic-call move precision (Increment 7).** `Vec(T)` monomorphized per
  element type, RAII-freed via `impl Drop for Vec(i32)` even through generic `vec_push(i32, v, …)`
  operations. `collect_moved` aligns args to params by call shape (free call skips the leading
  `comptime` type-arg slot; method/impl call offsets past the receiver), so a generic `mut`-borrow arg
  is a borrow, not a move. Goldens `generic_struct_instantiation_drops_at_scope_exit` /
  `generic_call_borrow_arg_does_not_move_droppable`; property `generic_borrow_call_still_drops_once`;
  gcc round-trip `examples/vec_generic.jtr`. **Teeth:** reverting to "all generic args move" fails the
  seam tests, then passes on revert.
- ✅ **`std/` ported onto the vtable allocator + RAII `List` (Increment 8).** `std/mem.jtr` is the
  real vtable `Allocator` (retiring the enum stand-in); `std/list.jtr`'s `List(T)` stores its
  allocator and frees by `Drop`. Wiring test `module::stdlib_demo_frees_its_list_by_raii` (the list
  drops by RAII, allocation routes through the vtable); `stdlib_demo_compiles_clean` still green;
  demos `examples/std/demo.jtr` (`5/10/50/40`) + `alloc_demo.jtr` (`60/60`).
- ✅ **Blanket generic `impl[T] Drop for Ctor(T)` (Increment 9).** One impl covers every
  instantiation; cgen monomorphizes the `drop` method per concrete `struct_instance`. `impl` gained
  bracket generics (parsed + printed). Golden `blanket_generic_drop_impl_monomorphizes_per_instance`
  (two instantiations, each a def + scope-exit call); fuzz `fuzz_blanket_drop_impl`. **Teeth:**
  disabling the generic `drop_key` path fails the golden, then passes on revert. Both `std/list.jtr`
  and `examples/vec_generic.jtr` now use a single blanket impl.
- **Remaining (future work):** qualified calls + generic-helper calls inside impl-method bodies
  (the std destructor delegates to a bare non-generic helper for now); `defer`/`errdefer`; owned
  (`take`) parameter drop; a leak-catching debug allocator (the `--features c-oracle` gcc-round-trip
  exit criterion); `@deterministic` allocators; linear / must-use types; conditional (per-branch) move
  precision; transitive `@no_alloc`. See `DROP-ALLOC-PHASE3.md`.

### 5.14 `core` / `std` — Option/Result combinators (no-alloc `core`)

The no-alloc `core` standard library (design §C). `Option(T)`/`Result(T,E)` and their
allocation-free combinators live in `examples/std/core.jtr`; the oracle for the
combinators is **the functor/monad laws** — checked against a faithful Rust mirror, with
the runtime side pinned by an end-to-end example and `cgen` goldens.

- ✅ **Generic-enum-in-generic-fn codegen (the enabler).** A generic `Option(T)`/`Result(T,E)`
  *constructed* and *matched* inside a generic function now monomorphizes: cgen resolves the
  inferred type through the active substitution before the concreteness check, the match tag
  prefix, and the payload binding (`apply_subst` gained its missing `GenEnum` arm); typeck seeds
  `cur_expected = cur_ret` so a tail `match`'s nullary `none` inherits the return type;
  `collect_fn_types` walks generic *function signatures* (not just struct fields) so a
  higher-order `f: fn(T) -> U` parameter gets its concrete typedef; `ty_mangle` gained
  `GenEnum`/`Result` arms (both previously collided on `x`). Goldens (`cgen::tests`):
  `monomorphizes_a_generic_enum_inside_a_generic_function`,
  `collects_fn_pointer_typedef_through_a_generic_signature`,
  `generic_combinator_lowers_deterministically`. **Teeth:** disabling the tail-expected seed or
  the fn-type loop fails the goldens, then passes on revert.
- ✅ **fn-pointer-returning-aggregate typedef ordering (the monad enabler).** A `fn(T) -> Option(U)`
  parameter is a fn-pointer that returns a generic enum *by value*, so its typedef must follow a
  forward declaration of the `Option(i32)` instance. `gen_forward_types` now forward-declares every
  generic struct/enum instance **before** `fn_type_typedefs` (C accepts a forward-declared aggregate
  as a fn-pointer return/param type; the bodies follow in `gen_struct_defs`/`gen_enum_defs`). Golden
  `fn_pointer_returning_a_generic_enum_is_forward_declared_first` asserts the ordering. **Teeth:**
  neutering `gen_forward_types` (forward typedefs back after the fn-pointer typedef) fails it.
- ✅ **Option combinators.** `opt_is_some`/`opt_is_none`, `opt_unwrap_or`, `opt_unwrap_or_else`
  (lazy default via `fn() -> T`), `opt_map` (functor), `opt_map_or`, `opt_ok_or` (→ `Result`),
  `opt_and_then` (monadic bind), `opt_filter` (`pred: fn(read T) -> bool`, value survives),
  `opt_or_else` (lazy alternative).
- ✅ **Result combinators.** `res_is_ok`/`res_is_err`, `res_unwrap_or`, `res_map` (functor over
  the ok value), `res_map_err`, `res_ok` (→ `Option`), `res_and_then` (monadic bind).
- ✅ **Laws as oracles (`proptests::core_props`).** Functor identity (`map(id) == id`), functor
  composition (`map(g∘f) == map(f) then map(g)`) for `Option` and `Result`; the **monad laws** —
  left identity (`and_then(some(x), f) == f(x)`), right identity (`m.and_then(some) == m`),
  associativity; `unwrap_or` selection; `filter` keep-iff-predicate; and the
  `res_ok(ok_or(o, e)) == o` bridge round-trip — all against a Rust mirror of the combinators'
  `match` structure.
- ✅ **Generic-`[]T` codegen.** A `for x in s` / `s[i]` over a generic slice inside a generic
  function now resolves the slice type through the active subst (names `JestyrSlice_i32`, not the
  opaque `JestyrSlice_T`), and `collect_slices` walks monomorphized function signatures so a `[]T`
  parameter contributes its concrete typedef even with no local `slice(T, …)`. Golden
  `generic_slice_algorithm_monomorphizes_to_the_concrete_slice_type`. **Teeth:** dropping the
  slice-`for` subst reintroduces an undefined `JestyrSlice_T`.
- ✅ **Slice / iterator algorithms (no-alloc).** The consuming/searching/in-place algorithms over
  `[]T`: `sl_fold`, `sl_count_where`, `sl_any`, `sl_all`, `sl_find` (→ `Option(usize)`),
  `sl_binary_search` (→ `Option(usize)`), `sl_swap`, and an in-place **stable, deterministic**
  insertion `sl_sort`. (Producers like `map`/`filter` that build a new sequence are the allocating
  `std`, Phase 2.) Iterator laws as oracles (`core_props`): `find` matches `iter().position()`,
  `any`/`all` match the reference; **sort invariants** — output is a sorted permutation of the
  input, deterministic (same input → same output), and **stable** (equal keys keep input order);
  plus a codegen property that a generic slice fold compiles clean for any integer element type.
- ✅ **Codegen properties (`proptests::core_props`).** A generic Option combinator compiles with
  **zero diagnostics** and **byte-identically** for every integer element type — the real
  compiler path the combinators ride (`core_option_combinator_compiles_clean_for_any_int_type`,
  `core_option_combinator_is_deterministic`).
- ✅ **Slice-index assignment lowering.** `buf[i] = v` now lowers to a bounds-checked **lvalue**
  assignment through the element pointer (`_s.ptr[_ix] = v`), not the rvalue statement-expression an
  `Index` *read* produces — which is what lets the integer formatter write digits into a caller
  `[]u8`. Golden `slice_index_assignment_lowers_to_an_lvalue`; teeth-verified.
- ✅ **Number parse / format — the determinism deliverable (integer side).** Locale-free,
  bytewise, deterministic by construction: `parse_i64`/`parse_u64` from a `str` (overflow is a
  *defined* `ParseIntError`, never a silent wrap — the signed parser accumulates negatively so
  `i64::MIN` is representable) and `format_i64`/`format_u64` into a caller `[]u8` (no allocation).
  Laws as oracles (`core_props`, against a Rust mirror of the same algorithm): `parse(format(x)) == x`
  for every `i64` (round-trip); `format_i64(x)` byte-identical to Rust's `Display`; differential
  `parse_i64` vs `str::parse::<i64>()` on sign/digit/letter soup (same successes incl. overflow, same
  values); and the defined-overflow boundaries (`i64::MAX`±1, `i64::MIN`±1).
- ✅ **Float-support primitives (toward correctly-rounded float parse/format).** Probed and built
  the deterministic building blocks the float algorithms (Eisel–Lemire, Ryū) sit on: `mul64`
  (64×64→128 product synthesized from 32-bit halves, since `core` has no `u128`), `clz64`
  (count-leading-zeros), and IEEE-754 bit access via an untagged `union` (`f64_bits`/`f64_from_bits`
  + `f64_sign`/`f64_raw_exp`/`f64_mantissa`). Properties (`core_props`): `mul64_matches_u128` — the
  synthesized product equals Rust's native `u128` for any operands (the crux); `clz64_matches_builtin`
  vs `u64::leading_zeros`. Wiring `module::float_bits_example_compiles_clean`; gcc round-trip
  `examples/std/float_bits.jtr` → `3.14159, 1023, 0, 1, 51, 63`.
- ✅ **Wiring.** `module::core_combinators_example_compiles_clean` +
  `slice_algos_example_compiles_clean` + `numbers_example_compiles_clean`. gcc round-trips:
  `jestyrc run examples/std/combinators.jtr` →
  `1, 20, 7, 42, 21, 22, 99, 0, 40, 20, 21, 7, 21, 20, 20, 7, 40` (functor composition `22`; monad
  identity `21`/`20`); `jestyrc run examples/std/slice_algos.jtr` →
  `100, 2, 1, 0, 2, 99, [10 20 30 40 50], 3, 99`; and `jestyrc run examples/std/numbers.jtr` →
  `12345, -42, 7, 9, -4271, -4271, 3` (parse, overflow/invalid → default, format, then a
  format→parse round-trip).
- **Remaining (future work):** the **correctly-rounded float** algorithms themselves (Eisel–Lemire
  `parse_float` + Ryū `format_float`, now that the primitives exist) + the cross-OS locked-SHA-256
  canary; defined-overflow integer math (`wrapping_*`/`saturating_*`/`checked_*`); the allocating
  `std` (Phase 2). See `CORE-STD-PHASE3.md`.

---

## 6. Experiment — D-HARHT (Memory profile) vs `HashMap`

Could the compiler's randomized `HashMap` symbol tables be replaced by **D-HARHT**, a
deterministic hash/radix table whose "seal then look up" model matches a compiler's *build-in-
typeck / read-in-cgen* access pattern? The Memory-profile code is vendored at
[`src/dharht.rs`](../src/dharht.rs) behind `--features dharht-experiment`, with a comparison
benchmark (`jestyrc dharht-bench`), a differential property test
(`proptests::dharht_experiment::dharht_memory_matches_hashmap` — a sealed D-HARHT must agree with
`HashMap` on every key), and the vendored unit tests.

```sh
cargo test --features dharht-experiment dharht                 # correctness (differential + units)
cargo run --release --features dharht-experiment -- dharht-bench
```

**Result (release, `u64 → u64`, 4n hits in pseudo-random order):**

| n | HashMap lookup | D-HARHT(mem) lookup | HashMap mem | D-HARHT(mem) mem |
|---|---|---|---|---|
| 2,000 (compiler-realistic) | 18.6 ns/op | **9.9 ns/op (0.53×)** | ~61 KB | **1.38 MB (22.7×)** |
| 100,000 | 51.3 ns/op | 71.1 ns/op (1.39×) | ~1.95 MB | 7.95 MB (4.08×) |

**Reading it honestly:**
- **Lookup speed** is genuinely good at compiler-realistic sizes — ~**2× faster** than `HashMap`
  at n=2,000 (the warm `second_leaf` cache + cache-resident data win), crossing over to ~1.4×
  *slower* at n=100,000 for random `u64` keys.
- **Memory** is the catch: D-HARHT(mem) is **4–23× heavier** than `HashMap` here. The "Memory
  profile" is memory-efficient *relative to D-HARHT's own Speed/Balanced profiles*, **not** versus
  `HashMap`. The cause is a **fixed 256-shard overhead** — each `Shard` carries 256-entry
  `second_jump`/`second_leaf` arrays (~2 KB/shard ⇒ ~0.5 MB of constant before any data), which
  dominates small tables.

**Verdict for the bootstrap compiler:** **not a drop-in fit, for two structural reasons.**
1. **Key type.** Jestyr's tables are `HashMap<String, _>` (and a few `HashMap<ExprId, _>`).
   D-HARHT is **`u64`-keyed** and does its full-equality check on that `u64`. Replacing a
   `String`-keyed table means hashing `String → u64`, at which point collisions silently alias
   (two strings, same `u64`, the `==` check passes) — reintroducing exactly the problem D-HARHT's
   key-equality model avoids. Only the `HashMap<ExprId, _>` tables (`method_calls`,
   `closure_index`, `qualified`) are natively `u32`-keyed and could map to `u64` cleanly.
2. **Table size.** Those tables hold hundreds–thousands of entries, where the ~0.5 MB shard
   constant makes D-HARHT 20×+ heavier for a sub-millisecond lookup saving the compiler never
   notices. (Tuning the shard count *down* would shrink the constant — a 16-shard build would be
   far lighter — but the key-type problem remains for the `String` tables.)
3. **Determinism** — D-HARHT's headline draw — is **already achieved**: the
   `compilation_is_deterministic` property (§3.1) shows the compiler emits byte-identical C today,
   so there's no determinism gap to close here. (If one ever appeared, the cheaper fix is
   `BTreeMap`/sorted iteration, not a radix table.)

**Where D-HARHT *would* shine:** large, **byte-addressable / prefix-heavy**, build-once/lookup-many
tables — e.g. a future self-hosted Jestyr's *runtime* string-interner or path index, exactly the
"byte-first, view-second" workload it was designed for. The experiment, benchmark, and differential
property test stay in-tree (feature-gated, zero cost to the default build) so that case can be
re-measured when it arises.
