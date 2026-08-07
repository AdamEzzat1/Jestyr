> **Internal development log** — kept for provenance. Status lines and counts in this file reflect the moment it was written, not the current state. Start at the repo [README](../../README.md) for current status and verified claims.

# Jestyr — the road ahead: self-hosting remainder, systems-language gaps, cgen hardening, and the assembly backend

> A three-horizon handoff, written at P5-increment 21 (cgen.jtr **44/130 corpus files
> byte-identical**, all on `master`). Horizon 1 is the concrete self-hosting worklist
> (finish P5 → R2 fixpoint). Horizon 2 is what Jestyr still needs to be a *proper*
> systems programming language. Horizon 3 is the backend road: hardening the C
> transpiler, then the path from C emission to native assembly (the Motley plan).
>
> **Read alongside:** `jestyr-selfhost-P5-cgen-R2-handoff.md` (the per-increment
> progress log + porting mechanics), `ROADMAP.md` (workstreams A–Q), `MOTLEY.md`
> (IR/backend strategy), `jestyr-design.md` (the language contract).

---

## Horizon 1 — Self-hosting: exactly what remains

### Where P5 stands (increment 31, master `4df4411`)

`examples/std/cgen.jtr` (~6.3K lines vs the ~10.8K-line Rust reference) emits
byte-identical C for **65 of 130** corpus files (50% by file count; ~65% by
construct machinery). Done since increment 21: clusters 1–5 complete (drop
glue + impl sections, genrefs, loop-else/labels, regions/arenas + zip/step/
variant, codepoints), generics SLICE 1 (LIFO instance worklist, bracket
inference, subst-aware sigs, erased comptime args), impl/BOUND/operator
dispatch (new typeck `bcalls` arena; operator resolutions recorded in
`icalls` on Binary ids), file I/O + gated try_read, split iteration,
ArrayRepeat + array-index writes, and **fn-pointer types** (cluster 11 core —
`JestyrFn_<sig>` typedefs, indirect calls by callee type, `&fn` values).
**Corrections:** `generics.jtr` does not exist — the diff-ranked probe IS the
worklist; module-importing drivers (`try_read`, `demo`) are permanent golden
non-targets (single-file path degenerates both sides).

**Remaining, ranked by leverage:** closures (unlocks `fn_ptr` 10-line diff,
`gen_vtable`, `closure_run`, `files`); niche + nested match (cluster 7:
`option`, `niche`, `nested_match`, `struct_variant`, `exhaustive_check`);
`attributes` (fn attr prefix + `@no_mangle` bare names); concurrency (cluster
10: `dynamic_spawn` → `atomics` → `concurrent`/`await` → `sync`/`select`/
`parallel`/`mutex`/`channel`/`par_*`); the generics completion (nested
instances in template bodies, structural unify, body-let substs,
generic-struct methods → `genlist` 70, `vec_generic` 82, **`list.jtr` 95 —
the first compiler-source dependency**, `strmap`, `intern`); **test mode**
(cluster 12 — most ~2000-line diffs are `@test` files whose whole harness
main differs; `emit_tests`/`test_main` unlocks them in one stroke); then the
compiler sources themselves (`tokens` 614 → `lexer` 1439 → `parser` 4028 →
`typeck`/`escape`/`cgen`) and R2-full. **Done:** the prelude + section
skeleton, params/binops/calls, let (annotated + inferred), casts, distinct types,
structs + struct methods, the whole flat **match** family (tagged switch, guarded
if-chain, scalar if-chain, or-patterns), **generic-enum instances** (the first
monomorphization), slices (typedefs + bounds-check spill + refinement elision),
arrays (incl. the ghost pre-adoption typedef), error sets + `?`, the full `for`
family (range/while/infinite/slice/array/str), owned String/Cow/Builder, the
alloc/utf8 boundary, f-strings (token re-scan), consts, extern "c", contracts,
attributes, template-fn suppression, **RAII drop glue** (cluster 1 — increment
22: `needs_drop`/`emit_drop_place` field+payload recursion, the flattened
drop-scope stack, `collect_moved`, spilled returns), and **concrete trait-impl
sections** (`emit_impl_protos`/`emit_impl_defs`, a first slice of cluster 9).
R2's subset harness is green (every allowlisted runnable program: jc1-compiled
C ≡ Rust-compiled behavior — the drop files run through gcc + execution too).

### The remaining construct clusters (in leverage order)

| # | Cluster | Target files (last-measured diff) | Reference machinery | Size |
|---|---------|-----------------------------------|---------------------|------|
| 1 | ~~**RAII drop glue**~~ **DONE (incr 22)** — `drop.jtr` + `drop_nested.jtr`; blanket generic `Drop` impls + take-self method moves deferred to the generics cluster | | | |
| 2 | ~~**Genrefs `&T`**~~ **DONE (incr 23)** — `genref.jtr` | | | |
| 3 | ~~**Loop-else + labels**~~ **DONE (incr 24)** — `loops_else.jtr` | | | |
| 4 | ~~**Regions / arenas**~~ **DONE (incr 25)** — `region.jtr` + `region_string.jtr` + `loops_advanced.jtr` (incl. zip/step/`variant` trackers) | | | |
| 5 | ~~**Codepoint iteration**~~ **DONE (incr 26)** — `codepoints.jtr` | | | |
| 6 | **Generic FUNCTIONS** | `bracket_generic.jtr` (18), `generics.jtr` | `collect_all_instances` worklist (fns + methods pull each other in), `mangle` (`jestyr_<name>__<types>`), `make_subst`, `emit_generic_call` with erased comptime args, monomorphized sigs contributing slice/array instances | **L — the hardest infra** |
| 7 | **Nested match + niche enums** | `nested_match.jtr`, `exhaustive_check.jtr`, `niche.jtr` | `pat_needs_nesting` → `emit_nested_match` decision tree; `NicheInfo` (Option(*T) ≡ bare pointer, NULL = none) — niche changes *type rendering*, construction, and match | L |
| 8 | **Closures** | `closures.jtr`, `closure_run.jtr` | `collect_closures` lambda-lifting: `JestyrEnv_<id>`/`JestyrClosure_<id>`/`jestyr_lam_<id>`, capture sets, fn-ptr coercion for capture-free closures | M–L |
| 9 | **Traits / impls / dyn** | `shapes_trait.jtr`, `dyn_*.jtr` | impl protos/defs, operator-trait dispatch (needs typeck.jtr to expose richer `impl_calls`), dyn vtable structs + fat pointers + shims | L |
| 10 | **Concurrency** | `atomics.jtr`, `spawn_*.jtr`, `select.jtr` | pthread lowering: `SpawnSite` arg-structs + trampolines, `spawn_runtime`, task handles, `concurrent`/`await`/`par for`/`select`, atomics intrinsics | L |
| 11 | **Fn-pointer types** | `fnptr.jtr`, `alloc_vtable.jtr` | `fn_type_typedefs` (`JestyrFn_<sig>`), fn-ptr fields/calls, `&fn` values | M |
| 12 | **Test mode** | (the `jestyrc test` harness) | `emit_tests`/`test_main` — needed for R2 to self-host the test runner | S |
| 13 | Leftover small forms | various | ArrayRepeat `[v;N]`, value-position Range, `@address` MMIO writes, def-capture topological flush (only when a by-value dep forces reorder) | S each |

**Standing invariants (violating any = every later temp desyncs):**
- The global temp counter must consume in **exact lockstep** with the reference
  (guarded/scalar matches eat 2; `is_utf8`/`from_utf8`/`os_from_bytes` eat 0).
- Copy programs stay byte-identical — every new construct must be inert for
  non-users (gate on use, exactly like the reference).
- `DUMP_DIVERGE=1` + the corpus probe after every construct; files unlock in
  clusters, so always re-probe the whole corpus.

**typeck.jtr side-tables still to expose** (add `pub` flat arenas right before the
construct that reads them, the P4 pattern): richer `impl_calls` (operator traits),
`dyn_coercions`, `call_sym` (colliding fns), `qualified` (module-qualified consts),
the closure index, niche info.

### R2 — from subset harness to the true fixpoint

The subset harness (`--features selfhost-fixpoint`) already proves jc1-compiled
programs behave identically to Rust-compiled ones. The **full** fixpoint
(jc2 ≡ jc3) needs:

1. **cgen.jtr must compile the compiler sources** — parser.jtr + typeck.jtr +
   escape.jtr + cgen.jtr use most of clusters 1–11 above (notably generics — every
   `list.get(T, …)` call — plus drop glue and closures).
2. **The multi-module wrinkle:** the golden path is single-file (`parse()` +
   `typeck::check`); the compiler sources are multi-module (`import "parser"` etc.).
   R2-full needs either (a) the module-merge behavior ported (per-module namespaces,
   canon names — the K machinery), or (b) a concatenated-source build of the
   compiler as one translation unit (the pragmatic first cut — flatten the imports,
   dedupe the std modules, accept the single-namespace constraint the sources
   already respect).
3. **The assertion:** `sha256(jc2's C for X) == sha256(jc3's C for X)` for X = the
   concatenated compiler source itself. `src/attest.rs` already provides the C-hash.

**Milestone order:** finish clusters 1–6 → probe whether `std/list.jtr` +
`std/tokens.jtr` emit byte-identical (they're the simplest compiler-source
dependencies) → stand up the concatenated build → grow to lexer/parser → jc2≡jc3.

---

## Horizon 2 — What Jestyr still needs to be a *proper* systems language

### What already clears the bar (don't rebuild)

Tiered references (`&T` gen-checked / `&[r]T` region / `*T` raw), ownership +
escape checking with RAII drop glue, monomorphized generics, traits A–F + `dyn`,
error sets + `?`, contracts (`requires`/`ensures`/`invariant`/`variant`), slices
with proven bounds elision, structured concurrency + `@deterministic` + `par for`
with compile-time reduction checking, `extern "c"` both directions
(`@no_mangle`), layout attributes (`@packed`/`@align`/`@section`), bit-fields,
unions, `@volatile` + `@address` MMIO, fixed arrays, modules with content-hash
manifests, allocator-as-value, the FP-determinism contract, `#line`→DWARF debug
mapping, doc comments + `jestyrc doc`, and a test/bench harness.

### The gaps, grouped and prioritized

**A. Language-core gaps (tracked in ROADMAP, sequenced):**

1. **CTFE / comptime interpreter (G, ~10%)** — the single biggest structural gap.
   Unlocks: the executable `build.jestyr` (build system!), reflection as comptime
   calls, comptime codegen, Motley's IR-builder ergonomics. An AST-walking
   interpreter over the existing arenas is enough to start.
2. **Defined integer-overflow semantics (J)** — a systems language must *choose*:
   wrapping/saturating/checked/trapping (and per-op opt-ins like `+%`). Today the
   emitted C inherits C's UB on signed overflow — unacceptable for the determinism
   spine. Decide the default (trap in debug, wrap documented in release — or
   Zig-style explicit operators), then make cgen emit it.
3. **Memory-layout pass (L, 0%)** — size/align computation in the compiler (not
   deferred to the C compiler), field reordering, enum niche-packing beyond
   `Option(*T)`, pass-large-aggregates-by-`const*` (today `read` params copy
   whole structs). Prerequisite for a native backend (Horizon 3) — assembly
   emission needs authoritative layout.
4. **Error-handling polish (I, ~70%)** — `catch` (reserved), error traces,
   fallible methods, richer payloads.
5. **`@verified` SMT (M)** — long-horizon; contracts become static proofs.

**B. Systems-programming table stakes not yet on any workstream:**

6. **Inline assembly** (`asm { … }` with operand constraints) — non-negotiable for
   a systems language (syscalls, CPU feature probes, atomics beyond libatomic).
   Cheap first cut: a `@naked`/`asm_str` escape hatch lowering to GCC extended asm.
7. **Freestanding / `no_std` target** — a build mode with no libc: user-supplied
   panic handler, no malloc in the prelude (the arena/allocator-as-value story
   already points here), `@section`-driven linker placement. This is what makes
   kernels/firmware writable. The prelude must split into `core` (freestanding)
   vs `std` (hosted) halves.
8. **Atomics memory-order story** — `atomic_*` intrinsics exist but the ordering
   model (SeqCst-only today?) must be specified and extended (acquire/release at
   minimum), with a documented mapping to C11 atomics.
9. **Globals & thread-locals** — mutable `static` with defined init order (or
   const-only + explicit init, the safer choice), `@thread_local`.
10. **Cross-compilation & target triples** — `jestyrc build --target=…` selecting
    prelude variants + cc toolchain; pointer-width/endianness as comptime queries.
11. **SIMD** — Q's roadmap already names it; comptime-width vectors lowering to
    GCC vector extensions first.
12. **Pointer-provenance + aliasing rules in the spec** — the design doc implies
    them (ownership gives `restrict`); write them down as normative rules so the
    native backend can rely on them.

**C. Toolchain / ecosystem:**

13. **Build system** — `build.jestyr` as an executable comptime program (blocked
    on CTFE, G). Until then: the manifest/lockfile-lite (K) covers pinning.
14. **Package management** — the content-hash DAG (`Modules::render_manifest`) is
    already a lockfile; add fetch/registry semantics later (design exists in K).
15. **LSP + formatter** — the printer exists (`src/printer.rs`); a `jestyrc fmt`
    is cheap. LSP needs the compiler-as-library shape self-hosting already forces.
16. **Std growth** — generic `StrMap(V)`/HashMap, sort, math, time, path/dir I/O,
    process spawning, networking (sockets via extern "c" first).
17. **Fuzzing + differential testing as a product feature** — the proptest/c-oracle
    discipline exists in-tree; expose `jestyrc fuzz` over user `@test`s later.

**Suggested order:** J-overflow (small, determinism-critical) → G-CTFE (unlocks
build system + reflection + Motley IR builder) → L-layout (unlocks the native
backend) → freestanding/`no_std` + inline asm (the systems credibility items) →
I-polish → SIMD → M-verified.

---

## Horizon 3 — The backend road

### 3a. Hardening the C backend (near-term, cheap, compounding)

The C backend stays **forever** as the reference oracle (Motley's own decision:
"bootstrap via C"). Worth hardening:

1. **Portability of the emitted C.** Today it requires GCC/Clang extensions:
   statement-expressions `({ … })`, `__typeof__`, `__builtin_unreachable()`,
   `restrict` usage is fine (C99). Either (a) document "GNU C11 required" as a
   contract, or (b) add a `--pedantic-c` mode lowering statement-expressions to
   declared temps + real statements (mechanical: every `({ T t = e; …; v; })`
   becomes a hoisted temp before the containing statement). MSVC support is (b)
   plus `unreachable()`/C23 or `__assume(0)`.
2. **Pass-by-`const*` for large aggregates** (workstream L) — today `read` params
   copy whole structs; the ownership model already licenses the pointer.
3. **Niche-packing generalization** — beyond `Option(*T)`: any enum with one
   nullary + one non-nullable-payload variant; then multi-variant tag compression.
4. **Bounds-check elision growth** — the refinement machinery (`cur_refines`)
   only proves `for i in 0..s.len` today; extend to transitive facts (`i < n`,
   `n <= s.len`) — a mini abstract interpreter, also reusable by `@verified`.
5. **Emitted-code ergonomics** — `--show-drops` exists; add `--annotate-costs`
   (the CJC cost-feature comments per function) as the first Motley cost hook.
6. **Runtime split** — extract the `jestyr_rt_*` prelude into a versioned
   `jestyr_rt.h`/`.c` pair compiled once (faster builds, and the native backend
   will link the same runtime).
7. **Integer-overflow semantics** (Horizon 2 item 2) lands here as emitted
   `__builtin_add_overflow` checks or wrapping casts.

### 3b. From C transpilation to assembly — the staged Motley plan

MOTLEY.md's backend decision is explicit: **not LLVM, not Cranelift** — an owned
native backend, with C as the bootstrap. The staged path that keeps every step
verifiable (the same golden discipline that got P5 this far):

**Stage 0 — the oracle stays.** The C pipeline is the differential-testing anchor
for everything below. Never delete it; every native-backend milestone is measured
as `native_exe(P) ≡ c_exe(P)` (stdout + exit code) over the whole corpus — the R2
harness transplanted.

**Stage 1 — define Motley IR** (MOTLEY §6, the contract already written):
- Typed, region-annotated, SSA-ish. Jestyr's ownership/escape results become
  first-class alias info (`noalias` facts the optimizer never re-derives).
- **Cost features in the IR contract**: every function carries
  `{flops, bytes_r/w, alloc, working_set, loop_depth, float_density}` — this is
  what makes thermal/energy scheduling possible later (CJC's `PhysicalCostQuery`).
- Two textual forms: human-auditable + compact content-addressed binary.
  Determinism: BTreeMap ordering, stable hashing (the attest discipline).
- Multi-language-neutral core (no Jestyr-isms) — Jestyr *lowers to* it.

**Stage 2 — AST→IR lowering pass** beside cgen (not replacing it). Golden: an IR
*interpreter* (cheap, a week of work) executing the corpus ≡ the C-compiled
binaries. This catches lowering bugs before any machine code exists.

**Stage 3 — IR→C backend** replacing the direct AST→C emission. Golden: the new
pipeline's C behavior ≡ the old pipeline's (behavioral, not byte-level — layout
of the C will differ). This proves the IR is *complete* while the C oracle still
does all the heavy lifting (regalloc, scheduling, peepholes come free from cc).
Only after this does cgen.rs/cgen.jtr become "the IR→C printer".

**Stage 4 — native codegen MVP**, one target first. Recommendation: **x86-64**,
both ABIs eventually but start with the dev box (Windows x64 ABI) or SysV under
WSL — whichever the CI runs. Scope the MVP ruthlessly:
- **Instruction selection:** maximal-munch tree matching over IR — no DAG ISel.
  (QBE proves ~10K LOC of this gets ~70% of LLVM's performance; that's the
  correct ambition level for v1.)
- **Register allocation:** linear scan first (with the pre-colored ABI registers);
  graph coloring is a v2 refinement.
- **Frame layout:** the L-workstream layout pass supplies size/align; spills +
  locals in one frame map; prologue/epilogue per ABI.
- **Emission:** textual `.s` assembled by the host assembler (`as`/`ml64`) —
  writing an object-file writer (ELF/COFF) is a later, separable milestone.
- **Runtime:** link the same `jestyr_rt.c` runtime (Stage 3a-6) — the native
  backend only replaces *user-code* generation, never the runtime, so pthread/
  libc concerns don't block it.
- **Debug info:** start with line tables only (the `#line` discipline maps
  directly to DWARF `.loc` directives in textual asm).

**Stage 5 — the differential harness as the acceptance bar.** Whole corpus:
`jestyrc build --backend=native` vs `--backend=c`, identical stdout/exit;
`compilation_is_deterministic` extended to the `.s` output; fuzz the IR with the
existing proptest infrastructure. A native backend earns trust exactly the way
cgen.jtr did — file by file against an oracle.

**Stage 6 — the Motley novelties** (only after 4–5 are green): thermal/energy-
aware instruction *scheduling* (the differentiator — pass benefit =
`base_reward − physical_penalty`), content-addressed incremental codegen,
provenance records per emitted function, and the classic pass set (DCE, CSE,
LICM, mem2reg first — mem2reg matters most since the naive lowering will be
stack-heavy).

**Sequencing note:** Stages 1–3 are pure-Rust (or post-self-host, Jestyr) work
that can start any time after P5 stabilizes; Stage 4 hard-depends on the
**L memory-layout pass** (Horizon 2 item 3) and the **overflow-semantics
decision** (item 2) — both are therefore on the critical path to assembly, which
is why they lead the Horizon-2 priority order.

---

## One-line

Self-hosting = finish the 13 P5 clusters (drop glue → genrefs → loops-else →
regions → **generic fns** → nested/niche match → closures → traits/dyn →
concurrency) then R2's jc2≡jc3 on a concatenated compiler build; systems-language
credibility = overflow semantics + CTFE + layout pass + freestanding/inline-asm;
the backend road = harden C (portability, by-ref aggregates, runtime split) →
Motley IR with cost features → IR→C swap → maximal-munch + linear-scan x86-64 MVP
proven by the same differential-oracle discipline that is proving P5.
