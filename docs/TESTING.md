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
