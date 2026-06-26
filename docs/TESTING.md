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
cargo test                       # unit + property + replayed fuzz corpus (285+ tests)
cargo test prop::                # just the property tests
cargo bolero test fuzz_pipeline  # real coverage-guided fuzzing of the pipeline
cargo run -- selfbench           # per-stage speed + footprint on a generated program
cargo run --features bench-alloc -- selfbench   # + peak/total heap bytes
cargo run -- test examples/tests_demo.jtr       # the in-language @test/@bench runner
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
- ⏳ **Remaining:** trait **F** `dyn` vtable (reuses the fn-pointer-field call machinery). Lands with
  the same three layers.

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
