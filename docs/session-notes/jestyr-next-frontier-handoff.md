# Jestyr — next-frontier handoff (post-self-hosting): G/L/Q

> **Cold-start orientation.** Two whole workstreams are finished and on `master`
> (`ebf8397`, **757 tests green** with `--features selfhost-fixpoint`, 700 in the
> toolchain-free default suite):
>
> * **P (self-hosting) + its productization arc** — the fixed point (jc2 ≡ jc1 on the
>   compiler's own C, all modes), the driver, in-language modules (the `ml_*` flatten
>   loader — **jc compiles itself from its real multi-file sources**), exact per-file
>   diagnostics, parse/typeck/escape refusal gates, and the committed **bootstrap seed**
>   (`bootstrap/` — building Jestyr from scratch needs only a C compiler, never Rust).
> * **O (tooling) — now COMPLETE IN-LANGUAGE**: the ported compiler emits the full
>   attestation manifest, the breaking-change diff/verify gate, and the documentation
>   page, each byte-for-byte identical to the Rust reference.
>
> **What this note now scopes is only Group 3** — the three Rust-side workstreams
> (G CTFE, L memory-layout, Q SIMD), of which **G increment 1 is done**. Section 2 is
> kept as a record of how O was closed; §3 is the live worklist.
>
> **Read alongside:** `docs/session-notes/jestyr-selfhost-P5-cgen-R2-handoff.md`
> (increments 1–54, the authoritative history + every recorded trap), `ROADMAP.md`
> workstreams G/L/Q (percentages there are stale for F/H — both are DONE — but the
> open-item descriptions are accurate), `MOTLEY.md` (the long game).

---

## Progress ledger (newest first — what landed since this note was written)

| Increment | Commit | What |
|---|---|---|
| **Q-W1** `par for` over any integer width | *(this run)* | **Q-S2's prerequisite, and it did NOT need the generic `spawn` this note assumed was blocking.** `emit_par_for` is map-then-reduce: it fills an `int64_t` buffer by running the body per element and hands only *that* to `core.par_reduce`, so the engine never sees the source slice. The `[]i64` restriction was therefore a **typeck rule, not an engine limitation** — `par_reduce` stays exactly as it is, i64-only and untouched. `par for` now iterates any integer element type; the loop variable carries the element's **own** type, so a body over `i32` computes in `i32`, and only the per-element contribution is widened, once, on the way into the buffer. The determinism argument does not move at all: the reduction domain is still `i64`, where the declared operators are exactly associative. **Why it is the SIMD prerequisite:** a lowering fills lanes with the *element* type, so `i32` gets twice the lanes of `i64` and `u8` eight times — building Q-S2 first would have shipped the 4-lane version of the feature. **Zero emission change**: the `i64` path reproduces its previous C character for character (no cast is added when the body is already `i64`), so all 138 corpus files, the concat, the fixpoint and the seed are untouched and no port mirror is due yet. 6 tests: every width accepted, the loop variable's C type, the two *different* slice types in one lowering (source keeps its own, the engine still takes `[]i64`), byte-identity of the `i64` path, refusal of a float element or contribution, and — the one that matters — the checked non-deterministic-reduction guarantee surviving the widening. |
| **M9** the lexer dogfood — the compiler's own source uses tier 7 | `5e24ce1` | **The first real USE of the CTFE ladder inside the compiler, and the increment that answers whether it was worth building.** `examples/std/tokens.jtr`'s six classifier functions (`is_space`/`is_alpha`/`is_digit`/`is_alnum`/`is_hex`/`is_digit_us`, ~30 lines of branch chains with `is_hex` calling `is_digit` to avoid restating a rule) become one comptime-computed 256-entry table plus six one-line predicates. One bit per class, so a byte's full classification is a single load; `_` carries both the alpha bit and a separate separator bit, because "is an identifier character" and "is the `1_000` separator" are not the same question and the branch chains had been conflating them by call order. **The crown result: `selfhost_fixpoint_full` still holds — `jc` now lexes its own 25K lines using a table its own compile-time evaluator computed, and still reproduces itself byte-for-byte.** The token stream is unchanged corpus-wide (all six P1 lexer goldens green), so the conversion is behaviour-preserving by test, not by argument. **The honest verdict on the feature, measured rather than asserted:** emitted C shows each classification is now a bounds-checked load (`assert(_ix < 256)`) where it used to be register comparisons — a real per-call cost — but timing both self-hosted compilers over a 12.7K-line file gives medians of ~163 ms (branches) and ~170 ms (table) with a 148–191 ms spread in **both**, i.e. indistinguishable, because lexing is a small fraction of a full compile. So the win is expressiveness and maintainability, and the cost is real but invisible at this granularity; resolving it either way needs a lexer-only benchmark the repo does not have. |
| **M7** the comptime `for` + mutation in the port | `270f947` | **The CTFE ladder (0–7) is now closed on BOTH sides.** Loops and `var` assignment run at compile time in the self-hosted compiler: all three heads, ranges (inclusive / `step` / descending), list iteration, element+index, `break`/`continue` incl. **labelled**, `for … else`, and compound assignment through the same **checked** arithmetic (so `x += 1` overflows into a diagnostic). Fuel per **iteration** — the recorded trap holds, an empty body evaluates nothing so nothing else charges the budget. **The increment is where M6's deferred obligation comes due, and the shape of the debt was exactly as predicted:** a handle copies for free, so the copies the reference gets from ownership have to be written down, at three points now marked in place — reading out of a binding, reading an element, and **each element of `[v; n]`**. The third is the one that is invisible until it isn't: `[[0; 2]; 3]` without it is three cells naming ONE run, so `m[0][1] = 5` writes through all three rows. Verified directly, not just via the golden. **The flat arena paid for itself on the other side of the trade**, also as predicted: an assignment target is a `Place` — an env row or a cell index — so walking an index path is an address computation, where the reference needs `find_binding` to hand out a `&mut Value` and `walk_path` to re-borrow through each index, a chain the `.jtr` subset cannot express at all. Assignment resolution reads through `place_read_raw`, which deliberately does **not** copy: copying there would resolve `t[0]` against a duplicate and drop the write. 16 new golden fixtures (9 accepts + 7 refusals). Two side findings: the escape checker refuses `return v` for a `read` parameter (rebuild the value instead), and a real dump bug surfaced only once there were enough refusal fixtures to see it — `eval` cleared `failed` but not `emsg`, so a second failing `const` reported the concatenation of every message so far. |
| **M8** aggregate EMISSION in the port, **corpus 138** | `f7b0130` | **Tier 6 is now closed end-to-end on both sides**: the port types a comptime block that folds to a list as `[N]T` and emits it, byte-identical to the reference. `examples/comptime_table.jtr` is the corpus file, and it exists to make the tier's point concrete — `const SQUARES: [6]i64 = [sq(0), …]` **does not compile**, because a `static const` initializer must be a constant expression and `jestyr_sq(0)` is a call; wrapping the same literal in `comptime { … }` moves the calls to compile time and what reaches C is `{ { 0, 1, 4, 9, 16, 25 } }`. A recursive `fib` table lands the same way — the C compiler gets `{ { 1, 1, 2, 3, 5 } }`, never the call tree. **Two emission paths, deliberately separate:** a `const` needs the BRACE form (`push_ctfe_brace`), since a static may not be initialized by the GNU statement-expression an expression-position aggregate uses (`({ T _cl0; _cl0.a[0] = …; _cl0; })`) — both are mirrored, and the const path is checked *before* the array-literal path in `emit_consts` rather than inside `emit_expr`, exactly as the reference orders it. Element type follows the reference's rule: an annotation wins, else the first element decides, else it is refused rather than guessed. Worth knowing for the next reader: a `const T: [6]i64 = comptime { … }` emits BOTH `JestyrArr_i32_6` and `JestyrArr_i64_6`, because a const's value is inferred with no expected type (so the block's own type is `[6]i32`) while the annotation gives `[6]i64` — pre-existing documented behaviour that aggregates now exercise. All 138 corpus files byte-identical on the first run; seed refreshed. |
| **M6** aggregate values in the port | `70e06af` | **Tier 6 closed on both sides — and it was the REPRESENTATION change this note predicted, not more of the same.** `ctfe.jtr`'s `Value` stays a tag plus one `i64`; a list's payload is a pair index into `lidx`, whose (start, len) names a contiguous run of CELLS in the parallel `ltag`/`lval` arenas, and a cell tagged 4 names another run — the reference's owned `Vec<Value>` tree, with the ownership expressed as an index. Two things fell out that the design had to answer rather than assume. **(a) Elements must be STAGED before they are stored:** `[[1, 2], 3]` builds the inner list while the outer one is mid-construction, so appending to the arena as each element evaluates would interleave the two runs; elements land on a stack and move into the arena as one run when the literal closes, and nested literals complete first so the stack discipline is free. **(b) Runs are already SHARED, before any binding exists** — `[v; n]` evaluates `v` once and stages one handle `n` times, so `[[0; 2]; 3]` is three cells naming ONE inner run where the reference holds three cloned `Vec`s. Nothing in tier 6 can observe it (no way to write through a handle), which is precisely why the deep-copy obligation belongs to **M7** and is recorded in the module docs at the two places the reference clones. Arms added: ArrayLit (14), ArrayRepeat (15), Index (6), Field `.len` (5), and structural `==`/`!=` (equality only — an ordering on aggregates would have to be invented). Fuel is spent **per element**, so `[0; 10000000000]` and the nested `[[0; 100000]; 100000]` are diagnostics in microseconds on both sides. 19 new golden fixtures (9 accepts + 10 refusals) in `jestyr_ctfe_folding_matches_reference`, which grew a one-line list rendering shared by `render_ctfe_value` (reference) and `render_value` (port). Seed refreshed — the gcc-only bootstrap now carries an aggregate evaluator. Two small traps: the **recorded** reserved-word list bit again (a parameter named `out` — a whole-file parse cascade, ~40 diagnostics, all from one identifier), and decimal rendering has to accumulate the digits as a NEGATIVE number because `0 - i64::MIN` does not exist. |
| **Q-S1** the checked `@simd` legality pass | `c77f6d2` | The SIMD frontier opens where L1 opened the layout one: **analysis first, zero emission change.** `src/simd.rs` decides whether a `par for` body may be evaluated a lane at a time — a total, elementwise integer expression — and `@simd` is the *checked declaration* that it can be, in `@span`'s shape rather than a `#pragma`'s: it emits nothing, so a refactor that slips a call, a division or a float into a hot body is a diagnostic instead of a silent fall off the vector path. Nothing SIMD-shaped existed on master beforehand, so this is increment 1 of the workstream, not a duplicate. The determinism argument is two claims, and only the second was open: the four declared reductions are exactly associative on machine integers (so a lane tree of ANY width equals the serial fold — which is why float is *excluded* rather than discouraged; `-ffp-contract=off` constrains contraction, not order), and the body must be total. **gcc is the authority, as it is for layout**: `simd_lanes_match_scalar_bit_for_bit` evaluates every certified body scalar-wise and through GCC vector extensions at widths 2, 4 and 8, reduced four ways, and requires identical bits. It earned its keep immediately by catching a real miscompile in its own harness — the scalar tail of a vector loop needs the *scalar* lowering of a select, the bug Q-S2 is now forewarned about. |
| **M5** def-order dependency registration | *(this run)* | Closes the divergence M4 recorded: the port emitted `JestyrSlice_<T>` **after** the struct whose field embeds it, where the reference emits it before. The sorter was never the bug — the port's slice and genref typedefs never called `dc_begin`/`dc_end` at all, and the array typedef opened a segment it **never closed**, so all three were absorbed as anonymous *glue*. Glue is pinned in place and invisible to `dc_find_dep`, so the struct's (correctly recorded) dep resolved to nothing. Registering all three as named segments — no deps for slice/genref, which reach `E` through a pointer; one dep for the array, which embeds `E` by value — makes the port's segment graph structurally identical to the reference's. `examples/def_order.jtr` is the new corpus file (**136 → 137**) and pins all three at once; the slice half is also pinned in place by restoring the field `comptime_reflect.jtr` had to omit. **Zero emission change on the other 136 files.** The unclosed `dc_begin` is the failure mode worth remembering: it loses the name without losing any text, so it is a silent divergence rather than a crash or a corrupt buffer. |
| **M4** G3 reflection port mirror | `b292426` | **Tier 3 closed on both sides.** `@type_name`/`@field_count`/`@field_name`/`@field_type` in the self-hosted compiler; `examples/comptime_reflect.jtr` is corpus **136**, byte-identical. The structural decision: `@field_type` needs a Jestyr-syntax type renderer, and one already existed in cgen.jtr for attest + doc — so `at_ty` **MOVED DOWN** into `ctfe.jtr` (below cgen, above parser, depends only on the type arena) and cgen keeps a three-line wrapper, leaving its 16 call sites unchanged. ONE renderer, two consumers: a reflected type name cannot drift from the documented or attested one, the same invariant `at_guarantee_phrases` gives the guarantee text. typeck types a query from a STATIC table (`field_count`→i64, the rest→str) and does **not** infer the arguments — the first is a *type*, so inference would report a binding that does not exist; only emission evaluates. **The `@` sigil justified itself in reverse:** the first cgen arm keyed on the Attr node KIND and broke two passing corpus files, because `@address(0x…)` in mmio.jtr is also `@name(…)` and must still emit normally. `@name` is a family; the guard belongs on the intrinsic NAME. Nine reflection fixtures added to the ctfe golden (incl. a primitive, a `record`, methods-are-not-fields, a const index, composition inside `comptime {}`, and three refusals) — all passed first run. |
| **G8** plan-as-list | *(this run)* | `const targets` now selects the build script's form **by its type**: an integer is a count (the tier-4 index form), a list is the targets themselves — `[[source, output], …]`, buildable by a comptime `for`. A pair is a two-element list because the value domain has no struct; two parallel lists would read better and can disagree in length. Artifacts take the same two forms. Fully backwards compatible, and the two forms render byte-identical plans (pinned by planning one build both ways). `examples/build_list.jestyr` is the worked example. |
| **L1** layout analysis | *(this run)* | `src/layout.rs` + `jestyrc layout <file>` — size, alignment, field offsets and **padding waste** for every declared type. **Zero emission change**, so no port mirror and no seed refresh; L's later increments are opt-in *because* this one is not. Verified rather than asserted: `layout_matches_c_sizeof` generates a probe `main` printing real `sizeof`/`_Alignof`/`offsetof` and diffs 19 values across the corpus shapes against the model — **the C compiler is the authority, and now says so in CI**. Two honest gaps, both stated in the report as `(incomplete)` rather than guessed: generic/opaque components, and **bit-fields** (implementation-defined in C — the model would otherwise report 4 bytes for a struct that occupies 1). Unblocks `@size_of`/`@align_of`/`@offset_of` as comptime *values*, the gap tier 3 records. |
| **M3** typeck fold + cgen emission, **first corpus file** | *(this run)* | `examples/comptime_block.jtr` — **corpus 134 → 135**, and the first `.jtr` in the tree to use comptime syntax. typeck.jtr folds a kind-44 node to its value's type (i32/bool/str, Error on failure); cgen.jtr emits the folded literal (`push_i64`, `push_c_string` with the octal + trigraph rules mirrored from `c_string_literal`); `eval_array_len`/`push_array_len` now go through the interpreter instead of accepting only a literal — the port's own G1. Emitted C byte-identical across all 135. **Two shims the next mirror will hit as well** — see findings 8 and 9. |
| **M2** the CTFE interpreter in `.jtr` | *(this run)* | `examples/std/ctfe.jtr` + `ctfe_cli.jtr` — the self-hosted comptime evaluator, scalars (Int/Bool/Str). Golden `jestyr_ctfe_folding_matches_reference` drives both implementations over 23 fixtures and requires agreement on **what folds** and **what is refused**: arithmetic, every literal base, bitwise/shifts, const chains, comparisons, short-circuit, `if`/`else`, calls, recursion, `let`, strings (concat/escapes/ordering), chars, casts, `comptime` blocks incl. nesting, and 8 refusal cases. **Named `ctfe`, not `comptime`** — see the finding below. Message-TEXT parity is deliberately not claimed (the `.jtr` subset has no `format!`; same concession the P-series made for the refusal gates). |
| **M1** the port parses `comptime` | `63ba3fa` | `parser.jtr` expr kind 44 + the three dispatch sites + dump arm; verified by the P2 expression golden on a temp probe file (6 snippets incl. the tier-7 shape), so it lands without touching a corpus golden. |
| **G7** comptime `for` + mutation | *(this run)* | **Also cleared tier 3's field-iteration blocker** — see below. Loops and `var` assignment run at compile time, so a table's **shape** is computed, not spelled out: a 256-entry CRC-32 table is built by `for i in 0..256 { t[i] = crc_entry(i) }` and emitted as a plain static (values verified against an independent implementation, not just self-consistency). All three loop heads, ranges (inclusive/`step`/descending), list iteration, element+index, `break`/`continue` incl. **labelled**, and `for … else`. Assignment reaches locals and elements at any depth; compound assignment uses the same **checked** arithmetic, so `+=` overflows into a diagnostic. Fuel per **iteration** — the trap here is that an empty body evaluates nothing, so nothing else charges the budget at all. |
| **G6** aggregate values | *(this run)* | `Value::List` — CTFE goes from "compute a number" to "compute a **table**". Array literals/repeats evaluate; `[i]` and `.len` read. A `const` initialised by a comptime block yielding a list becomes an ordinary **static** (`static const JestyrArr_i64_8 jestyr_FIB = { { 0, 1, 1, 2, 3, 5, 8, 13 } };`) — the C compiler gets the answers, not the recursion. Emission detail with teeth: a static **must** be a brace initializer, since it cannot be initialised by the GNU statement-expression an expression-position aggregate uses; both paths exist and the test asserts no `({` reaches a static. Fuel is spent **per element**, so `[0; 10_000_000_000]` (and the nested `[[0; 100000]; 100000]`, where the *product* blows up) is a diagnostic in microseconds. |
| **G5** bounded generation | *(this run)* | `jestyrc plan … --emit`. A build script **computes the bytes of a file**; the plan records it by SHA-256, and the driver writes it only on explicit `--emit`. The evaluator gained no new power — it computed a string — so generation is a pure function whose result the *user* places, never an effect a script performs. Reproducible, attestable (hash in the plan), and bounded literally: size-capped, and an absolute or `..` path is **refused, not normalised**. A generated file can be named as a build target, so a program can be computed and compiled in one invocation. |
| **G4** `build.jestyr` | `29bd2bd` | `src/buildscript.rs` + `jestyrc plan <script> [--build]`. The build described in Jestyr and **evaluated, never run**. The plan is a *pure function of an index* (`const targets` + `fn source(i)`/`fn output(i)`) rather than the imperative `exe(…)`/`test(…)` shape, because that shape needs comptime **effects** — the one thing the ladder exists to forbid. Closes K's last leftover. `Interp::{eval_const, call_fn}` added. `examples/build.jestyr` is the canonical demo (a `.jestyr` extension, so no golden sweeps it). |
| **G3** reflection | `4a85bc6` | `@type_name` / `@field_count` / `@field_name` / `@field_type` over the **declared shape**, answered by this compiler and folded to literals. |
| **G2** comptime blocks | `b063ca4` | `comptime { … }` in user syntax, reusing the existing keyword. Typed as and emitted as the literal it folds to; **the body belongs to the interpreter alone**, so non-users are byte-identical with no gating flag. |
| *(chore)* seed refresh | `458911a` | `bootstrap_seed_is_current` was **already failing on master** — the committed flat was missing the ten import-placeholder blank lines. `jestyr_seed.c` was byte-identical, so it was cosmetic; the gcc-only bootstrap was never broken, only its drift guard. |
| **G1** CTFE interpreter | `ebf8397` | `src/comptime.rs` — a TOTAL comptime interpreter (step budget, call-depth cap, const-cycle detection, checked arithmetic). First consumer closed a silent miscompile: `[SIZE]i32` had been emitting a **zero-length** array with `assert(_ix < 0)` and no diagnostic. Zero emission change → no port mirror yet. |
| **54** `doc` renderer | `1a5ad67` | `jc <file> doc` byte-identical to `doc::generate` over 134 files; `at_guarantee_phrases` became the ONE guarantee extractor shared with attest. |
| **53** doc trivia | `1a5ad67` | `tokens.collect_docs` — finds comments by scanning the **gaps between tokens**, so the golden-pinned `tokenize` is untouched. `examples/std/doc_cli.jtr` added → corpus 134. |
| **52** attest diff/verify | `094bd6e` | `jc <old> attest-diff <new>` / `jc <file> attest-verify <manifest>` reproduce `attest::diff(…).render()` byte-for-byte, exit code included. Surfaced the `#line` divergence (§1). |
| **51** attest records | `e3c5fbe` | `jc <file> attest` emits the FULL manifest; ported `doc::{ty_str, fn_sig, extern_sig, const_sig, fn_guarantees}`. |

**Two findings from the O run worth carrying forward**, both recorded in place below:
the `#line` emission gap (§1 — invisible to every golden, so it hid for 50 increments),
and the fact that a map-free port of an ordered-container algorithm can still be exact
when the reference re-sorts its output by content (§2, O1).

### Findings from the G2–G4 run (read these before continuing CTFE)

1. ~~**`Value::List` — aggregate comptime values — is the single next unlock.**~~ —
   **DONE (G6).** It cost no totality, as predicted, *provided* fuel is spent per
   element — that one line is what separates a bounded evaluator from one that tries
   to allocate ten billion values. What it delivered: comptime **tables** as real
   statics. What it did *not* deliver on its own: building a table still meant spelling
   out `[f(0), f(1), …]` — the *values* were computed, the *shape* was not. ~~And
   tier-3 field iteration still needs comptime-only functions.~~ — **both closed by G7.**
2. ~~**Field iteration is emission-blocked, not L-blocked.**~~ — **DONE (G7), and the
   predicted fix was the wrong one.** The diagnosis was right: a top-level `fn` is also
   emitted as ordinary runtime code, where the index is a parameter and the query cannot
   fold. The prescription — **comptime-only functions** — was not needed. A comptime
   `for` binding is not a function parameter; it lives in the interpreter's own
   environment, and typeck never descends into a `comptime` body, so the loop form folds
   where the function form could not:

   ```jestyr
   const SHAPE: str = comptime {
       var acc = ""
       for i in 0..@field_count(Point) { acc += @field_name(Point, i) … }
       acc
   }
   ```

   That reaches C as one `JSTR(…)` constant (`a_comptime_loop_iterates_struct_fields_end_to_end`).
   **The transferable lesson:** when a feature is blocked because a value "is a
   parameter, not a constant", check whether some *other* binding form is already
   constant before building a new function kind. Comptime-only functions remain a
   reasonable convenience, but they block nothing now.
2b. **A design decision can be *unmade* by a later tier, and the docs must say so.**
   Tier 4 chose index functions (`source(i)`/`output(i)`) over a target list on the
   explicit ground that a list "costs the evaluator no new powers — no effects, no
   aggregate values, no comptime `for`". Two of those three arrived (G6, G7), so the
   ground was gone and the module docs were still asserting it. G8 added the list form
   *beside* the index form — `const targets` dispatches on its value's type, which made
   the change free of any migration — and rewrote the rationale rather than leaving a
   stale one. Worth a sweep after every tier: the ladder's earlier rungs contain
   "we can't do X yet, so Y" reasoning that expires silently.
3. **`size_of`/`align_of`/`offset_of` are C-deferred**, lowering to `sizeof`/
   `_Alignof`/`offsetof`. This compiler never learns the numbers, so exposing them as
   comptime *values* genuinely does require **L**. That is the real L dependency; the
   declared shape (names, field types, order) needed none of it.
4. **Bare-name intrinsics are a live hazard, and the compiler's own source proves it.**
   G3's first draft put reflection beside `size_of` as ordinary identifiers — but
   `examples/std/typeck.jtr:919` declares `fn field_type(…)`, which a bare-name
   `field_type` intrinsic silently hijacks, breaking the self-hosted build. Reflection
   moved to the **`@` namespace** (`@field_type`), which was already a callable form
   (`@address(0x…)`) and cannot collide. `size_of` and friends carry the same latent
   hazard; prefer `@` for anything new.

   **The same hazard recurred at M2, in a new place: a MODULE PATH.** The port mirror of
   `src/comptime.rs` could not be named `comptime.jtr`, because `comptime.run()` parses
   as a comptime block missing its `{` — the module path and the new expression form
   compete for the same token, and the parser resolves it in the keyword's favour. The
   reference is fine only because `comptime` is not a Rust keyword. It is now
   `examples/std/ctfe.jtr`. **Generalised rule: every time the language reserves a word,
   it also forbids that word as the first segment of a module path** — so check the
   keyword list before naming a module, not just before naming a function.
5. **Do not add names to cgen's `is_intrinsic` list casually.** That list stops a *bare
   value reference* being read as a closure capture — and the self-hosted compiler has
   several locals named `field_count`. Adding reflection there would have broken real
   code while buying nothing (reflection is only ever called). A comment now says so
   in place.
6. **`comptime` inherited the block-led statement rule for free**, which will look like
   a bug to the next reader: `comptime { comptime { 3 } + 1 }` is a parse error for the
   same reason `unsafe { unsafe { 3 } + 1 }` is — at statement position a block-led form
   parses as the block alone so a trailing operator cannot extend it. Nest in *value*
   position instead. Pinned in `comptime::tests`.
7. **A computed string needs real C escaping.** A source literal is passed through
   verbatim (Jestyr's escapes are C's), but a comptime-*computed* string has no source
   text. Two C rules bite: hex escapes are maximal-munch (`"\x41" "1"` → `\x411`), so
   non-printables use fixed-width three-digit octal; and `-std=c11` still honours
   trigraphs, so a literal `?` must be escaped. `c_string_literal` in cgen.rs, round-
   tripped through gcc. Note the NUL case can only be checked by *length* —
   `printf("%.*s")` stops at a NUL whatever precision it is given.
8. **Unit tests in the binary crate have no `CARGO_BIN_EXE_*`.** That is an
   integration-test variable. To invoke `jestyrc` as a subprocess from `proptests.rs`,
   walk up from `std::env::current_exe()` (`target/<profile>/deps/` → `../`).
9. **Every new block-led form needs its `dump_types` BLOCK-SHIM entry** (M3, and this
   will recur). The reference stores most blocks as embedded `Block` **structs** — fn
   bodies, if-THEN, `unsafe`/`concurrent`/`region` bodies, `for` body+else, select arms —
   while the port's parser materializes *every* block as a kind-23 arena node. The P3
   typeck golden compares the FULL expression stream in ExprId order, so `dump_types`
   skips the struct-position blocks to keep the two streams aligned. Adding `comptime`
   without adding kind 44 to that skip list shifted every subsequent type by one node per
   comptime block, which reads as a wall of unrelated type mismatches. **The diagnostic
   signature is an accumulating drift**: if the divergence count grows with the number of
   occurrences of the new construct, suspect the shim, not the typing.
10. **A new construct in a corpus file drags in the port's `eval_array_len` too.** A
   `comptime` block is legal as an array length, and the port's `eval_array_len` /
   `push_array_len` accepted only an integer *literal* — the exact pre-G1 reference
   behaviour, silently yielding `[0]T`. Both now route through the interpreter. Anything
   that can appear in a length position has to be folded on both sides.

11. **An unclosed capture segment is a SILENT divergence, not a crash.** `dc_begin`
   without a matching `dc_end` is absorbed by the next `dc_begin`'s `dc_glue`: the text
   is still emitted, at the right offset, with correct buffer coverage — it just loses
   its *name*, becoming anonymous glue that is pinned in place and invisible to
   `dc_find_dep`. That is how the port's slice/genref/array typedefs sat unregistered
   for the whole P5 series (M5). When auditing the port's `Dc` sites, check `dc_end`
   pairing, not just `dc_begin` presence — and remember the reference's `dep_of_cty`
   rule that makes the deps correct: a rendered type containing `*` contributes no edge,
   so a slice/genref typedef has no deps while a *field* of that type does.

12. **Still open (a both-sides gap, not a divergence): a struct declaring an ARRAY field
   whose instance is never constructed gets no `JestyrArr_<T>_<N>` typedef at all**, so
   the emitted C does not compile. `collect_arrays` keys off values (`info.expr_types`,
   fn signatures, monomorphized instances) and never walks struct field *declarations* —
   unlike `collect_slices`, which does scan the type arena. Both implementations agree,
   so every golden is green; `examples/def_order.jtr` sidesteps it by constructing a
   `[4]i64`. The fix is a field-declaration walk in `collect_arrays` + its port mirror.

13. **FUEL ACCOUNTING is part of the mirror contract, not an implementation detail**
   (M6). The budget decides *whether a program folds*, so if the two evaluators spend
   at different rates a borderline input folds under one and is refused by the other —
   a divergence no fixture would find unless it sat exactly at the boundary. The
   invariant that makes them agree is narrow and worth stating: **both spend one step on
   entry to the expression evaluator** (`Interp::eval_expr` / `ev`), and any additional
   per-element or per-iteration spend must be mirrored at the same place. Checked for
   tier 6: ArrayLit costs `1 + n` on both, ArrayRepeat `1 + 1 (count) + 1 (value) + n`
   on both, Index and `.len` `1 + children`. Tier 7 spends per *iteration*, so the
   mirror must too — and the recorded trap there is that an empty loop body evaluates
   nothing, so the per-iteration spend is the only thing charging the budget at all.

---

## 0. Discipline (unchanged — every increment)

- `cargo test` green (**700 default**, 757 with `--features selfhost-fixpoint`) +
  warning-clean; cross-impl goldens behind `--features c-oracle`; the
  fixpoint/self-build/seed family behind `--features selfhost-fixpoint`.
  **Auto-commit each green increment to `master` + `git push origin master`**
  (`git commit -F <file>`, Co-Authored-By trailer).
- One construct per increment with its golden slice. Never a big drop.
- **THE TWO-SIDED TAX (new since self-hosting — this is the thing that silently breaks):**
  any change to emitted C, an intrinsic, or a pass now has TWO implementations — the Rust
  reference (`src/*.rs`) and the port (`examples/std/*.jtr`). The full gate is:
  1. `jestyr_cgen_matches_reference` — **138** corpus files byte-identical;
  2. `jestyr_cgen_concat_matches_reference` + `jestyr_cgen_test_mode_matches_reference`;
  3. `selfhost_fixpoint_full` + `jestyr_driver_builds_itself` (the compiler must still
     compile ITSELF and must not trip its own refusal gates);
  4. `bootstrap_seed_is_current` — **whenever any `examples/std/*.jtr` source changes,
     rerun `REFRESH_SEED=1 cargo test --features selfhost-fixpoint bootstrap_seed_is_current`
     and commit the refreshed `bootstrap/` pair, else the drift guard fails.**
  New intrinsics/runtime fns must be GATED ON USE (the `uses_try_read` pattern) so every
  non-user program's C is byte-identical — and mirrored at all four port sites (gate
  helper + prelude line in cgen.jtr, helper-table entry, closure marker-name string,
  typeck.jtr return arm).
- **`.jtr` subset traps (all hit in real sessions — do not re-derive):** a `for`
  CONDITION may not start with `(` (parses as the zip-binding head — use
  `for i < n { if … { break } }`); a bare `{` block after a call-initializer parses as
  the `Name(args){…}` generic-ctor form (write flat, no scoping blocks); **a statement
  followed by a line starting with `[` parses as an INDEX, not a new statement** —
  `var a = [1, 2]` then `[a[0]]` on the next line is `[1, 2][a[0]]`, which refuses with
  "`a` is not a compile-time constant" and looks exactly like an interpreter bug (both
  implementations agree, because it is a parse; M7 lost time to this in a hand-written
  probe); NEVER chain
  `string_view(x).len` (bind `let v: str` first); `out`/`comptime`/`par`/`select` are
  reserved; author `.jtr` with the Write/Edit tools, never through shell heredocs
  (backslash mangling has produced real newlines inside string literals twice).
- The deep-dive levers: `DUMP_DIVERGE=1` on any cgen golden prints the first differing
  line; `TYPECK_FILE=<basename>` prints the aligned per-expr type stream.

## 1. Current state (what exists, so you don't rebuild it)

- **The self-hosted compiler** `examples/std/cgen.jtr` (**~12.7K lines**) + its 10 imports
  (`mem, intern, fs, env, list, tokens, parser, typeck, escape, sha256`). Full CLI:
  * `jc <file>` — raw C dump, the UNGATED golden mode (error files must keep emitting
    degenerately, so **never** add a refusal gate here);
  * `test [substr]` / `list [substr]` — the `@test`/`@bench` harness and its listing;
  * `build` / `run` — module-loading, refusal-gated, gcc-driving product modes;
  * `attest` — the FULL manifest (header + per-item records);
  * `<old> attest-diff <new>` / `<file> attest-verify <manifest>` — the breaking-change
    gate, exit 1 on a breaking change;
  * `doc` — the Markdown API page (single-file, typeck-free).
- **In-language modules**: the `ml_*` flatten loader (DFS deps-first, token-level
  rewrite, cross-module collision renames `name__<stem>`, `(merged, allsrc)` checkpoint
  map for exact per-file diagnostics).
- **The `_cli` split**: a module with `main` cannot be imported, so every pass that needs
  both a library and a dump driver has one — `parser_cli`, `typeck_cli`, `escape_cli`,
  `doc_cli`. Reach for it rather than adding a dump mode to `cgen.jtr`.
- **Bootstrap**: `bootstrap/jestyr_flat.jtr` (20,857 lines) + `jestyr_seed.c`
  (**31,658 lines**) + README; gcc-only build verified live, seed self-reproduces
  byte-for-byte.
- **Driver v1 limits already recorded** (P5 handoff, increments 44–50): no
  `-Wl,--stack` in driver gcc invocations; exe suffix fixed `.exe`; cmd.exe wants
  backslash paths for `run`; some item-level parse malformations recover Error-node-free
  and degrade to gcc; refusal messages are generic (location exact).
- **`#line` — the one KNOWN C-emission divergence** (surfaced by increment 52's
  `attest-verify`, recorded here because nothing else exposes it): the reference's
  module path (`module::load` → `TypeInfo::debug` → `mark_line`) emits `#line <n>
  "<file>"` directives, and **the port emits none**. No existing golden sees it — all
  134 corpus goldens, the concat, and the fixpoint use debug-free single-file/merged
  ASTs — but it means `jestyrc attest <f>` and `jc <f> attest` disagree on `c-sha256`
  (the reference hashes the `#line`-bearing C; `examples/api_v1.jtr`-shaped files show
  27 such lines), and port-built binaries carry no source-line mapping. `jc attest` /
  `attest-verify` are self-consistent, so the drift gate works — it's only cross-tool
  hash comparison that can't match. **Its own increment when wanted:** the port already
  has the input it needs (the `Ml.map` checkpoint pairs give per-file line/col), but
  there is NO golden for the module path's C today — build that first (`jestyrc emit-c`
  vs `jc <file> build`'s `.c` over a multi-module fixture), then port `mark_line`'s
  placement + dedup, then refresh the seed (the seed's C would gain the directives).

## 2. O-tooling — ✅ COMPLETE (kept as the record of how, not as a worklist)

> Nothing here is outstanding. It stays because the *how* is reusable: both items were
> ports of a Rust pass into `.jtr` under the two-sided tax, and the notes below name the
> arena layouts, the sharing invariant, and the traps that cost time.

### O1. Attest manifest RECORDS + `verify` — **DONE (increments 51–52)**
`jc <file> attest` now emits the FULL manifest — header plus per-item records — byte-equal
to `attest::manifest` (golden `jestyr_driver_attest_manifest_matches_reference`, 10 corpus
files), via the ported `doc::{fn_sig, fn_guarantees, const_sig, extern_sig, ty_str}` family
(`at_*` in cgen.jtr). And `jc <old> attest-diff <new>` / `jc <file> attest-verify
<manifest>` reproduce `attest::diff(…).render()` byte-for-byte, exit code included
(golden `jestyr_driver_attest_diff_matches_reference`, every verdict branch). What follows
is the original plan, kept for its reference pointers:

```
<kind> <name>
  vis: pub|priv
  sig: <reconstructed signature>
  guarantee: <…>          (0..n lines)
```

- The reference collects them in `attest.rs::collect_records`; **`sig` and `guarantees`
  come from `src/doc.rs` (`doc::fn_sig`, `doc::fn_guarantees`) — a signature
  RECONSTRUCTOR from the AST, not a span slice.** Port plan: a `sig`-renderer in `.jtr`
  over the parser's `iar` param 7-tuples + ret TypeId (the port already renders C
  signatures — this is the Jestyr-syntax analogue; `fn_guarantees` reads
  `requires`/`ensures` spans (`far` extras), the error set, `@no_panic`, refined params).
- Records sort by `(kind, name)`; kinds: fn / const / struct / enum / trait / distinct
  (read `collect_records` for the exact set + struct-method handling).
- Then `verify`: `jc <file> attest-verify <manifest>` — re-render and diff (the Rust
  side is `attest --diff` in main.rs; mirror the drift-report format).
- **Golden**: full-manifest byte-compare vs `attest::manifest` over a handful of corpus
  files (contracts.jtr — requires/ensures; records.jtr — methods; errors.jtr — error
  sets), grown file-by-file like every other golden.
- **As built (notes for the next reader):** `collect_records` covers fn/const/enum/
  struct-record-union/extern only — trait/impl/distinct/import are NOT records (the C
  hash attests them), and struct METHODS never get their own record. Sorting is a
  selection sort by (kind, name) emitting each record as its slot settles. The differ
  keeps the manifest text owned by the caller and models `ParsedItem` as spans
  (`struct Am`); the `BTreeSet`/`BTreeMap` fields are re-derived from the guarantee
  lines on demand rather than stored, and set ORDER never matters because the change
  list gets the reference's total `(item, verdict, detail)` re-sort at the end — that
  one observation is what makes a map-free port exact.

### O2. `doc` in-language — **DONE (increments 53–54)**
`jc <file> doc` renders the Markdown API page byte-for-byte identical to
`doc::generate(src, stem, html=false)` over all 134 corpus files (golden
`jestyr_doc_matches_reference`), with the dangling-doc lint located exactly.

- **The blocker was cleared without touching the token stream.** `tokens.jtr` gained
  `pub struct RawDoc` + `pub fn collect_docs`, a SECOND pass that finds comments by
  scanning the **gaps between tokens** — trivia by construction, so it needs none of the
  string/char-literal handling the tokenizer has, and `tokenize` stays byte-identical.
  (The load-bearing case: `"/// not a doc /* nor this */"` inside a string literal is
  never in a gap.) Golden `jestyr_doc_trivia_matches_reference` pins kind, block-ness,
  the comment span and the text span against `Lexer::tokenize_with_docs` corpus-wide,
  plus a fixture for both demotions (`////`, `/***`), `/**/`, nested plain comments and
  a trailing doc. `examples/std/doc_cli.jtr` is its dumper (the `_cli` split again).
- The renderer lives in cgen.jtr beside attest (`dc_*` + the `at_*` sig renderers),
  which is what makes the sharing real: **`at_guarantee_phrases` is now the ONE
  extractor** — attest wraps it as `  guarantee:` lines, doc as the `- ` bullets — so
  the attested ABI can never drift from the rendered docs, exactly the reference's
  design. Added for doc: `at_struct_sig` / `at_enum_sig` (note the reference prints
  `struct` for a `record`/`union` too, and lists fields only, never methods).
- **Not ported** (deliberate, recorded): `--html` mode; the fenced-`Example` extraction
  (collected by the reference for future doctests, never rendered in Markdown — the
  fence STATE is ported, since a `#` inside a fence must not open a section); and the
  reference's snippet decoration on the dangling-doc warning (its message text and
  `file:line:col` are ported, the `|`-gutter source excerpt is not). Doc prose using
  non-ASCII whitespace would trim differently (`char::is_whitespace` vs ASCII).
- **The trap this increment cost time on:** a struct member's tag-1 (method) tuple holds
  its fn `ItemId` in slot **1**, not slot 2 (slot 2 is a field's name-span start). Reading
  the wrong slot indexes the item arena with a source offset — an out-of-bounds read that
  surfaces only as a bare `Assertion failed!` from the generated C.

## 3. Group 3 — the three Rust-side workstreams (each multi-week; nothing blocks on them)

These change the REFERENCE first; the port inherits each construct later as its own
increment chain (same two-sided tax). Recommended order: **G → L → Q** (G unlocks the
most; L wants G's comptime for layout queries; Q builds on both).

### G. CTFE + reflection (~10% — the biggest unlock)
**What exists:** generics/type-param substitution; const-expr DISCRIMINANTS evaluate;
**the comptime interpreter (increment 1 — DONE)**.
**Build (increment chain):**
1. ~~A comptime **interpreter over the AST** (`src/comptime.rs`)~~ — **DONE.**
   `Interp::{eval, eval_usize}` over `Value::{Int, Bool, Str, Unit}`: literals (all
   bases, `_` separators, escapes), unary, checked arithmetic, comparisons,
   short-circuiting `and`/`or`, bitwise/shifts, string concat + compare, `if`/`else`,
   blocks with `let`, `const` references, int-to-int casts, and calls of pure fns with
   recursion. **Totality is the design point** — a compiler that hangs on
   `const A = B; const B = A` is worse than one that errors: a step budget (`FUEL`), a
   call-depth cap, and const-cycle detection make every input terminate with a
   diagnostic. Anything outside the value domain (floats, structs, intrinsics) is a
   clean `EvalError`, never a guess.
   **Its first consumer closed a real silent-miscompile bug:** `typeck::eval_array_len`
   and `cgen::array_len` accepted only an integer LITERAL and silently returned `0`
   otherwise, so `[SIZE]i32` emitted a zero-length array, a type-mismatched
   initialization and `assert(_ix < 0)` on every access — with no diagnostic. Both now
   evaluate, and a length that cannot be evaluated is an error
   (`check_array_len` from `audit_type_id` for signatures, from the `Stmt::Let` arm via
   `check_type_array_lens` for locals — item audits never visit local annotations, which
   is what the first draft got wrong). Zero emission change: a literal folds to the same
   number, so all 134 corpus goldens, the concat (31,658 lines) and the seed are
   untouched, and **no port mirror is needed yet**. Tests:
   `comptime::tests::*` (15, incl. every totality bound) plus
   `comptime_folds_array_lengths_end_to_end` (const / arithmetic / pure-fn-call lengths
   through gcc) and `comptime_rejects_a_non_constant_array_length`.
   **Port note for later:** the moment a corpus `.jtr` uses a non-literal array length,
   the port's `typeck.jtr`/`cgen.jtr` must fold it too (the P3 typeck golden compares
   `[N]T` renderings) — keep such files out of `examples/` until that mirror lands.
2. ~~`comptime { … }` blocks~~ — **DONE (G2, `b063ca4`)**, reference side. Reuses the
   existing `comptime` keyword (it could never start an expression, so no new reserved
   word and no changed parse). Typed as and emitted as the literal it folds to. **No
   `comptime const`** — top-level `const` is already comptime-evaluated, and a second
   way to say it violates §8. The design point: *the body belongs to the interpreter
   alone* — cgen/escape/attrs/dharht never descend into it, which is why non-users are
   byte-identical with no gating flag and why "typechecks" cannot disagree with
   "evaluates". **No corpus file yet, deliberately** (see the port-mirror trigger below).
3. ~~Reflection~~ — **DONE (G3, `4a85bc6`)**, reference side, *declared shape only*:
   `@type_name(T)`, `@field_count(T)`, `@field_name(T, i)`, `@field_type(T, i)`.
   Answered by this compiler and folded to literals; field types render through
   `doc::ty_str`, so reflection cannot drift from the docs. Arguments must be
   compile-time constants — see findings 2–4 above for what that excludes and why.
4. ~~**The executable `build.jestyr`**~~ — **DONE (G4)**, closes K's last leftover.
   `jestyrc plan <script> [--build]`. The plan is a **pure function of an index**
   (`const targets` + `fn source(i)`/`fn output(i)`), not the imperative
   `exe(…)`/`test(…)` shape, because that needs comptime effects. Determinism is
   structural: a non-deterministic build script cannot be *written*, since the
   evaluator has no arm for a clock or an environment read. `examples/build.jestyr` is
   the demo; a `.jestyr` extension means no golden sweeps it.
5. ~~**Tier 5 — bounded generation**~~ — **DONE (G5)**, foundation laid.
   `jestyrc plan … --emit`: a script *computes* an artifact's bytes, the plan records
   it by SHA-256, the driver writes it only on demand. The design point is where the
   boundary sits — **the evaluator gained no new power**; it computed a string, and the
   *driver* places the file. So generation is a pure function the user chooses to
   apply, not an effect a script can perform. Reproducible, attestable, and bounded
   literally (size cap; an absolute or `..` path is refused rather than normalised).
   Deliberately *artifact* generation, not source-string injection: what a script
   produces is a file you can read, diff and hash before anything acts on it.

**The outstanding CTFE work, in the order it should be done:**

| # | Work | Notes |
|---|---|---|
| 1 | **Comptime `for`** | the natural partner to G6: today a table's *values* are computed but its *shape* is spelled out (`[f(0), f(1), …]`). Bounded by the same fuel budget — spend per iteration, exactly as `ArrayRepeat` now does |
| 2 | **Comptime-only functions** | unblocks tier-3 field *iteration* end-to-end (finding 2). Either this or (1) closes the "walk a struct's fields" story |
| 3 | **Port mirrors for G2/G3/G6** | the big one — see below |
| 4 | `@size_of`/`@align_of`/`@offset_of` as comptime values | after **L** (finding 3) |

**On the port mirrors (item 3).** Nothing is broken today: G2–G4 changed no emitted C
for any existing program, so all 134 corpus goldens, the concat, test mode, the
fixpoint, the self-build and the seed are green *without* a mirror. The trigger is
unchanged and still the thing to respect — **the first `comptime`/reflection corpus
`.jtr` file drags in `parser.jtr` + `typeck.jtr` + `cgen.jtr` mirrors and a seed
refresh**, because the P2/P3 goldens sweep every `examples/**.jtr` with no allowlist.
The mirror is genuinely large: it means writing a total comptime interpreter in the
`.jtr` subset (G1's ~700 lines of Rust, plus the tier-2/3 surface) and threading a new
expression kind through the port's integer-tagged AST. Land it as its own increment
chain, and only when a corpus file needs it. `.jestyr` build scripts are exempt — the
goldens key on the `jtr` extension.
**Port impact:** each comptime construct that changes emitted C needs its cgen.jtr
mirror + golden growth; constructs that only FOLD at check time still need typeck.jtr
parity (the P3 golden compares every expression's resolved type).

### L. Memory-layout pass (increment 1 DONE — where systems performance lives)
size/align computation, **field reordering**, **enum niche-packing**, and
pass-large-aggregates-by-`const*` (today `read` params copy).
**The byte-identity constraint is the whole game here:** reordering fields changes the
emitted C for every existing program, which would invalidate 135 golden files, the
concat, the seed, and attest hashes at once. Land it OPT-IN:
1. ~~A pure ANALYSIS pass (`src/layout.rs`) computing size/align/waste + a report mode
   (`jestyrc layout <file>`)~~ — **DONE (L1).** Zero emission change, as predicted.
   Two things the next increment should inherit rather than rediscover:
   * **The C compiler is the authority, and the test says so.** `layout_matches_c_sizeof`
     emits the program's real C, appends a probe `main` printing `sizeof`/`_Alignof`/
     `offsetof`, compiles it with the locked `CC_FLAGS` and diffs against the model. The
     reordering increment will be *reasoning about offsets*, so it needs them to be
     known-true rather than believed. Extend that test's file list with each new shape.
   * **Two gaps are admitted, not guessed** — a record says `(incomplete)` when a
     component is generic/opaque, and when the struct has **bit-fields**, whose packing
     is implementation-defined in C (the model would otherwise report 4 bytes for
     `struct Packed { a: u8 : 1, … }`, which really occupies 1). `compute` takes the AST
     as well as the table precisely because bit widths are syntax, not type — the same
     hook `@layout(auto)` will need.
2. `@layout(auto)` opt-in per struct → reordered emission for annotated types only
   (non-users byte-identical; golden corpus grows annotated files).
3. Niche-packing behind the same attribute; by-`const*` `read` params behind
   `@abi(ref)` or a whole-program flag — each its own increment + port mirror.
**Port impact:** every opt-in construct mirrors into cgen.jtr when its corpus file
lands; the seed refreshes.

### Q. SIMD → GPU (increment 1 DONE — the deterministic-acceleration frontier)

**Check master before building ANY parallelism feature — Q has twice discarded
duplicate builds** (the parallelism memory + `PARALLELISM-HANDOFF.md`). What is
already there, confirmed in-tree this run, so nobody rebuilds it a third time:
`par_map`/`par_scan` (`examples/std/parallel.jtr`), `par_reduce`, the
`par for … reduce(r)` surface (`ExprKind::ParFor`, the four-arm typeck check against
`DETERMINISTIC_REDUCTIONS`), dynamic-N spawn, `@deterministic`, the `par for` SHA
canary, and **`@span`** — the checked work-span model in `attrs.rs` (a `Cost { k, j }`
lattice over the AST's loop structure; a sequential loop multiplies by `n`, a
`par for` contributes `log n`, so serializing a reduction is a compile error).

**What did NOT exist before this run: anything SIMD-shaped at all.** No `@simd`, no
vector lowering, no thermal facet. (`dharht.rs`'s `simd_tier()` is an unrelated
blueprint string behind `--features dharht-experiment`.) So the frontier starts at
increment 1, and the mission is *not* to reinvent data parallelism — it is to make
SIMD deterministic first, then use the proven SIMD contract as the gate to GPU.

#### The promise being extended
Today: **bit-identical across thread counts and chunk sizes**. This workstream adds
**lane widths**, and eventually **legal GPU tile schedules**, to that list — without
loosening the FP contract (`-ffp-contract=off -fno-fast-math`, `CC_FLAGS`, locked by
`fp_contract_tests`) by a single flag.

#### Q-S1 — the checked `@simd` legality pass ✅ **DONE (this run)**
`src/simd.rs` + the `@simd` attribute + `jestyrc simd <file>`. **Zero emission change**,
which is the whole reason it is first: a vector lowering rewrites the emitted C of
every vectorized program at once, invalidating the corpus, the concat, the seed and
every attested hash in one commit. So this increment decides *what is legal*, proves
that decision against the real vector backend, and a later one lowers only what is
already certified. Exactly L1's route (measure the layout, verified by the C compiler,
before reordering a field).

The determinism argument splits cleanly in two, and only the second half was open:

1. **The reduction is lane-count independent** — `par for` already admits only
   `sum`/`min`/`max`/`xor` over `i64`, exactly associative *and* commutative on machine
   integers. A lane tree of any width equals the serial fold, bit for bit. This is why
   float is *excluded* rather than discouraged: `-ffp-contract=off` constrains
   contraction, not order, and a horizontal float add reassociates.
2. **The body is a total, elementwise integer expression** — what `simd::classify`
   decides, via a deliberately small whitelist: literals, names, `+ - * & | ^ ~ ! <<
   >>`, comparisons, `and`/`or`, `if`/`else`, blocks with `let`. Everything else is
   rejected with its **own** named cause (`Call`, `Memory`, `Float`, `Trapping`,
   `ShiftAmount`, `LaneWidth`, `Control`, `Unsupported`) at the **innermost** offending
   span, so the diagnostic points at the division and not at the loop.

`@simd` is a **contract, not a switch** — the `@span` shape, not a `#pragma`: it
changes no C, and a refactor that slips a call, a division or a float into a hot body
becomes a diagnostic instead of a silent fall-off-the-vector-path. `@simd` on a
function with no `par for` is an *error*: an attribute that quietly means nothing reads
like a guarantee.

**Tests (21 new; 785 default green, warning-clean).** `simd::tests` (15) cover each
accept and each distinct rejection; `proptests::simd_legality` (6) cover the attribute,
its diagnostics, that `@simd` emits byte-identical C, and that the shipped
`examples/std/par_for.jtr` is certified — a positive corpus case **without a new corpus
file**. The soundness half is `simd_lanes_match_scalar_bit_for_bit`
(`--features c-oracle`): every certified body is evaluated scalar-wise and through GCC
vector extensions at widths **2, 4 and 8**, reduced by all four declared reductions,
and every answer must be identical. **gcc is the authority**, the same way
`layout_matches_c_sizeof` makes it the authority on layout.

#### Findings from Q-S1 (read before Q-S2 — each cost real time or nearly did)

1. **The scalar tail of a vectorized loop needs the *scalar* lowering of a select.**
   The oracle shares ONE `#define F(x)` between the scalar and vector runs so there is
   no transcription risk — but a select cannot be shared, because a GNU vector
   comparison yields all-ones/all-zeros per lane while a scalar comparison yields `1`,
   and `?:` is not defined on vectors at all. The first draft flipped `SEL` to the mask
   blend for the whole vector section, tail included, and the tail (1003 elements, not
   a multiple of any width) silently computed garbage. **The lowering increment will
   have exactly this bug available to it**: `Q-S2` must emit the scalar select for the
   remainder loop, and `N % W != 0` must be in its first test, not its last. That the
   oracle *caught* it unprompted, with a precise message, is the teeth-verification
   this increment would otherwise have had to stage.
2. **`and`/`or` are legal only because the sublanguage is total.** A lane blend
   evaluates both sides; scalar short-circuits. That is value-preserving *only* because
   every faulting form (`/`, `%`, indexing, calls) is already excluded. **Admit one
   partial operation and the short-circuit operators stop being safe in the same
   motion** — the two rules are a single invariant wearing two hats. Same argument
   licenses `if`/`else` as a select.
3. **Division is the rule with teeth, and the reason is not the obvious one.** A vector
   computes every lane, so a divisor that is zero in a lane the scalar run would have
   skipped becomes a real SIGFPE — plus `INT64_MIN / -1`. `if x != 0 { 100 / x } else
   { 0 }` looks guarded and is not.
4. **A syntactic pass is sufficient here, and the argument for why is load-bearing.**
   Attributes are validated in the *parser*, before types exist (the constraint `@span`
   already works under), so `@simd` cannot consult `TypeInfo`. That is sound anyway:
   the loop variable is `i64`, and the only two routes from an integer to a float — a
   cast and a call — are both rejected, so **no float can depend on the loop variable**,
   and any float that could survive into a certified body is loop-invariant, broadcast,
   never reduced. `no_float_can_depend_on_the_loop_variable` pins both routes. **If
   either is ever admitted, the float exclusion needs types to stay sound.**
5. **Span containment beats a bespoke walker.** `sites_in_span` finds a function's
   `par for` loops by filtering the flat expression arena on span nesting, so no walker
   needs maintaining as new expression forms land — and a form that *contains* a
   `par for` cannot hide one from the check.
6. **ONE classifier, two consumers** — `@simd` and `jestyrc simd` both call
   `simd::classify`, so the report can never certify a loop the attribute rejects. The
   same rule that keeps M4's reflected/documented/attested type renderings from
   drifting (`at_ty`).
7. **The pass is conservative, never optimistic**, and only one direction is a
   soundness claim. A rejected body may well vectorize; nothing depends on that, and it
   is not tested. Say this out loud in any diagnostic-tuning increment.

#### Q-S2 — deterministic SIMD lowering (next; the first emission change)
`@simd` flips from *contract* to *contract + opt-in lowering*, for certified loops
only. Non-annotated programs stay byte-identical, so the corpus moves only for files
that opt in — the `@layout(auto)` discipline.

- Emit GCC vector extensions (`__attribute__((vector_size(N)))`), **not** `#pragma omp
  simd`, no `-march` change, no new `CC_FLAGS`. The lowering must be *chosen*, not
  begged for, or determinism is at the optimizer's discretion.
- **Lane count comes from the ELEMENT type, and Q-W1 made that a real choice**: a
  `par for` over `i32` fills twice the lanes of one over `i64`, `u8` eight times. The
  loop variable already carries the element's own type and only the per-element
  contribution is widened, so the vector body has the narrow type to work with — the
  scalar path is `{ecty} j_x = _pf.ptr[i]; _pm[i] = (int64_t)(body);` and the vector
  form has to preserve exactly that split.
- Shape: vector head + lane fold + **scalar remainder** (finding 1). Lane width is a
  fixed, recorded constant, not a host probe — a host-dependent width would make the
  emitted C depend on the build machine, which the attest hash would (rightly) flag.
- **Two-sided tax applies in full** for the first time in this workstream: an annotated
  corpus file drags in `parser.jtr` (attribute already generic), `typeck.jtr` and
  `cgen.jtr` mirrors plus `REFRESH_SEED=1`. Keep the first annotated file out of
  `examples/` until the mirror lands — the same trigger the CTFE M-series demonstrated.
- The determinism test already exists in the form it will need: extend
  `simd_lanes_match_scalar_bit_for_bit` to compare **`jestyrc run` of the annotated
  program against the unannotated one**, and add the annotated demo to the c-oracle SHA
  canary's demo set so lane width joins thread count and chunk size in the pinned digest.

#### Q-S3 — the `@span` thermal/energy facet (CJC CANA/PINN inheritance)
**Adapt, do not reinvent** (`MOTLEY.md` Part III; CJC `crates/cjc-cana`).

- Feed a `PhysicalCostQuery`-shaped record — flops, bytes read/written, allocation and
  working set, threads/**lanes**, batch/tile shape, and **float-op density**
  (`float_ops/flops`, PINN v2's dominant feature, corr ≈ +0.95) — into the closed-form
  v1 model (`heat = norm(flops)·(1+thread_amp·Δthreads)·(1+batch_amp·Δbatch)`, then
  `thermal = clamp01(heat·(1−cooling_rate))`, `cooling_rate = 0.05`, **no FMA, every
  product named**).
- `simd::Verdict::Legal { ops }` already carries the lane-op count *because* this is
  coming: it is the `flops` input, computed once, by the same classifier.
- **Deterministic or it does not ship**: no profiling, no clocks, no host probes, no
  `Date`/`rand` anywhere in the model. Container/cgroup/TDP inputs are future-facing and
  must be *explicit and recorded* — a normal build may not become host-dependent.
- **Ranking authority only, never legalization.** The facet may rank or diagnose legal
  lowerings ("this vectorization is thermally worse — here is the number"); it may never
  make a nondeterministic one legal. That veto split is CJC's own (`legality.rs` holds
  the veto; `pass_ranker.rs` only scores), and it is the line that keeps a *model* from
  quietly becoming a *policy*.
- Carry CJC's known debt forward rather than rediscovering it: the model is
  **hardware-generic** (per-window heat accumulation, not watts), and the energy
  residual is nonlinear — a linear head caps near R² 0.82.

#### Q-S4 — the GPU contract (design now, implement after Q-S2 is proven)
Write the deterministic contract while SIMD is fresh; build nothing GPU-facing until
the SIMD contract has tests behind it. The contract to state: **bit-identical across
every *legal* tile schedule**, by the same two-part argument — an exactly-associative
reduction plus a total elementwise body — with the tile/block shape occupying the role
lane width has here. `simd::classify`'s whitelist is the natural seed for the kernel
subset; a gather (`Reason::Memory`) is the first rule GPU will want to relax, and it
must be relaxed *with* its determinism argument, not before it.

#### Non-negotiables for every increment in this workstream
No fast-math, no reassociation, no FMA-dependent behaviour, no schedule-dependent float
results. No OpenMP pragmas, no work-stealing runtime, no hidden scheduler, no
nondeterministic reduction (not even opt-in — `PARALLELISM-HANDOFF.md` §"What NOT to
build" is still binding). Every user-visible feature ships with a determinism test that
proves scalar and accelerated paths are bit-identical. Emission changes bring goldens,
port mirrors and a seed refresh with them, in the same commit.

## 4. Sequencing (the one-line plan)

**Done:** ~~O1 records~~ (51–52) → ~~O2 doc~~ (53–54) → **workstream O complete** →
~~G1 the comptime interpreter~~ (`ebf8397`) → ~~G2 `comptime` blocks~~ (`b063ca4`) →
~~G3 reflection~~ (`4a85bc6`) → ~~G4 `build.jestyr`~~ (`29bd2bd`) → ~~G5 bounded
generation~~ (`bce5456`) → ~~G6 aggregate values / comptime tables~~ → ~~G7 comptime
`for` + mutation~~ — **the CTFE tier ladder (0–7) is done on the reference side**, and
documented in `docs/ctfe-tiers.md`.

**Port-mirror state: tiers 2, 3 and 6 are CLOSED on both sides** (M1 parse → M2 the
interpreter → M3 fold + emission → M4 reflection; M5 fixed the def-order divergence M4
surfaced; **M6 aggregates**). `ctfe` is the 12th module of the self-host closure, so the
flattened compiler and the bootstrap seed both contain a comptime evaluator — one that
now computes tables, not only numbers — and the shared type renderer. Tier 7 (the
comptime `for` + mutation) is the one interpreter tier still reference-only; tier 6's
*emission* (an aggregate `const` as a C static) is likewise still reference-only, and is
what will actually grow the corpus.

**Next, in order:**

1. ~~**The G6 port mirror — the representation change.**~~ — **DONE (M6).** The
   prediction held exactly: designing the representation *was* the increment, and the
   evaluation logic followed from the reference. The answer is a pair index into `lidx`
   naming a run of cells in `ltag`/`lval` — a tree in flat arenas, `Value` still a tag
   plus one `i64`. What the design had to decide rather than inherit was **staging**
   (elements go on a stack and move into the arena as one run when the literal closes,
   because a nested literal would otherwise interleave its cells with the run being
   built around it) and **when sharing becomes observable** (it is already present at
   `[v; n]`, and tier 6 simply has no way to detect it).

   ~~**G7 — the comptime `for` + mutation.**~~ — **DONE (M7). The CTFE ladder 0–7 is now
   closed on both sides.** The stated obligation came due exactly as written, and with
   one point more than the note had counted: the deep copy is owed at THREE places, not
   two — reading out of a binding, reading an element, and each element of `[v; n]`.
   That third one is what `[[0; 2]; 3]` turns on. The predicted payoff also landed: a
   `Place` is an env row or a cell index, so the write path is an address computation.

   ~~**Then M8: cgen emission + the first aggregate corpus file.**~~ — **DONE.** The
   brace-vs-statement-expression split was the detail with teeth, exactly as predicted,
   and both paths are mirrored; `examples/comptime_table.jtr` is corpus **138**.

   **The CTFE workstream (G) is therefore COMPLETE on both sides**, tiers 0–7, with the
   ladder documented in `docs/ctfe-tiers.md`.

**Next, in the order the user set:**

1. ~~**The lexer dogfood.**~~ — **DONE (M9).** `tokens.jtr`'s classifiers are one
   comptime-computed 256-entry table, the fixed point still holds (jc lexes its own
   source with a table its own evaluator computed), and the token stream is unchanged
   corpus-wide. **The verdict on the feature was measured, not asserted:** expressiveness
   yes — one declarative table replaces six functions that cross-called each other to
   avoid restating rules, and it forced out a conflation (`_` is an identifier character
   *and* separately the `1_000` separator; the branch chains got the right answer only
   by call order). Performance: a wash. Each classification is now a bounds-checked load
   where it was register comparisons, but both self-hosted compilers time at ~163 vs
   ~170 ms median on a 12.7K-line file with a 148–191 ms spread in both — indistinguish-
   able, because lexing is a small fraction of a compile. **The missing tool this named:
   there is no lexer-only benchmark**, so a per-stage cost like this cannot be resolved
   today; `selfbench` times the Rust reference's stages, not the port's.

2. ~~**Widen `par for` past `i64`.**~~ — **DONE (Q-W1), and the assumed blocker was not
   real.** This note had recorded the cap as following from "`spawn` targets cannot be
   generic", a constraint in workstream N. It does not: `emit_par_for` is
   map-then-reduce, so `core.par_reduce` only ever consumes the `int64_t` buffer and
   never sees the source slice. The restriction was a typeck rule. `par for` now
   iterates any integer element type with the loop variable carrying that type, the
   reduction domain stays `i64`, and the `i64` path is byte-identical — so no port
   mirror or seed refresh was due. **The lesson generalises: when a limit is attributed
   to another workstream's constraint, check whether the lowering actually depends on
   it before planning around it.**

   **Still outstanding from this thread:** the corpus file that exercises a narrow-width
   `par for` end to end, which is also what triggers the port mirror
   (`typeck.jtr` + `cgen.jtr`) and a seed refresh — the same ordering G1 and Q-S1 used.
   Reference-side tests cover types and emitted shape; the gcc run arrives with that file.

3. **L2 `@layout(auto)`** — the first increment that actually changes emission, now that
   L1's offsets are *verified against the C compiler* rather than believed. Opt-in per
   struct, so non-annotated types stay byte-identical; the corpus grows annotated files.
   Then L3 (niche packing, by-`const*` `read` params).
4. **Q-S2 deterministic SIMD lowering** — `@simd` flips from contract to contract +
   opt-in lowering for the loops Q-S1 already certifies. The second increment in this
   workstream that changes emission, and it inherits a ready-made first test case: the
   scalar remainder (`N % W != 0`) needs the *scalar* select, the bug Q-S1's oracle
   caught in its own harness. Then Q-S3 (the `@span` thermal/energy facet — ranking
   authority only, never legalization) and Q-S4 (the GPU contract, written now,
   implemented after Q-S2 is proven).

`@size_of`/`@align_of`/`@offset_of` as comptime *values* come after L (L1 unblocked the
computation; exposing it is its own slice). Comptime-only functions are a *convenience*,
not a blocker (finding 2). `#line` (§1) stays an independent, optional increment.

Keep every increment two-sided-green:
**corpus 138** + concat + test-mode + fixpoint + self-build + refreshed seed.

**The port-mirror trigger — now DEMONSTRATED, not just predicted.** It said: the moment a
`comptime` `.jtr` lands in `examples/`, the P2/P3 goldens sweep it with no allowlist and
the port must parse, check and emit it. That is exactly what M1–M3 did, and the predicted
order (reference side + Rust-only tests → mirror → corpus file) held. **Tier 2 is now
closed on both sides**: `examples/std/ctfe.jtr` is the self-hosted interpreter, `ctfe` is
the 12th module of the self-host closure, `examples/comptime_block.jtr` is corpus file
**135**, and the bootstrap seed carries a comptime evaluator.

What the next mirror should expect, in the order it will hit them: the parser needs its
`ref_dump_expr` arm in the harness (finding 6-adjacent — a missing one reads as a
divergent port); a block-led form needs its `dump_types` **block-shim** entry (finding 9,
whose signature is an *accumulating* drift); anything legal in an array-length position
needs `eval_array_len`/`push_array_len` folding on both sides (finding 10); and a new
`import` in `typeck.jtr`/`cgen.jtr` must be added to `SELFHOST_MODULES` in dependency
order or the concat build fails immediately (fast, not a divergence — a build error).
(`.jestyr` build scripts stay exempt: the goldens key on the `jtr` extension.)

**Tier 6 in the port was a REPRESENTATION change, and it is now done (M6).** `ctfe.jtr`'s
`Value` is still a tag plus one `i64`, Copy and allocator-free; a list payload is a pair
index into `lidx`, naming a run of cells in `ltag`/`lval`, and a cell tagged 4 names
another run — the reference's owned tree with ownership expressed as an index. The two
decisions that were not inherited from the reference: elements are **staged** on a stack
and moved into the arena as one contiguous run when the literal closes (otherwise a
nested literal interleaves its cells with the run being built around it), and **sharing
is already present at `[v; n]`** but is unobservable while nothing can write through a
handle. Tier 7 makes it observable and therefore owes a deep copy at the two points the
reference clones — binding a list, and reading one out of the environment — while
getting an easier write path in exchange, since walking an index path over a flat arena
is an address computation rather than a chain of `&mut` borrows.

## One-line
Self-hosting is finished and productized, **workstream O is complete in-language**, and
**the CTFE tier ladder (0–7) is now done on the reference side** — a total comptime
interpreter that closed a silent zero-length-array miscompile, `comptime { … }` in user
syntax, reflection over the declared shape in the collision-proof `@` namespace, a
`build.jestyr` that is *evaluated, never run*, bounded artifact generation, aggregate
values that make a computed lookup table an ordinary static, and a comptime `for` that
computes a table's **shape** as well as its values — a 256-entry CRC-32 table now
emerges from `for i in 0..256 { t[i] = crc_entry(i) }` as a plain C static, and the same
loop walks a struct's fields to generate a descriptor, which is what tier 3 had been
waiting on. The through-line is that **purity was never traded away to get power**: each
tier added a capability without giving the evaluator an effect, so determinism and
reproducibility stayed properties of the design rather than conventions to police — and
each tier's cost was one line of fuel accounting in a new place (per expression, per
element, per iteration). The ladder is documented in `docs/ctfe-tiers.md`. **Group 3's
other two workstreams have now opened the same way — by measuring before changing
anything**: L1 reports what your types cost with gcc as the authority on layout, and
**Q-S1 decides which `par for` loops may run a SIMD lane at a time, with gcc as the
authority on vectorization** (scalar ≡ 2 ≡ 4 ≡ 8 lanes, bit for bit, across all four
declared reductions) — `@simd` being a *checked contract* in `@span`'s shape, not a
`#pragma`, so it emits nothing and a body that stops vectorizing becomes a diagnostic
rather than a silent performance cliff. That the analysis-first ordering is not merely
cautious is now demonstrated twice over: L1's report is the thing reordering will
justify itself against, and Q-S1's oracle caught a real select-lowering miscompile in
its own harness before any lowering existed to be wrong. **Tier 6 is now closed in the
port too (M6)** — the self-hosted evaluator computes tables, not only numbers, and the
gcc-only bootstrap seed carries it; the representation question the note had flagged as
"the real work" turned out to be exactly that, answered by staging elements before
storing them and by naming precisely when a shared run becomes observable. What's left
is the **G7 port mirror** (the comptime `for` + mutation, which owes the deep copy tier
6 could defer) and **M8** (aggregate emission + the first corpus file), **L2** opt-in
layout, **Q-S2** opt-in SIMD lowering → the thermal facet → the GPU contract, and the
optional `#line` port — each landed increment-by-increment under the two-sided golden
discipline with the bootstrap seed refreshed at every `examples/std` change.
