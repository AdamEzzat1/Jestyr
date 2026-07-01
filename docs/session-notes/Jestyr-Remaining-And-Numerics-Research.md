# Jestyr — Remaining Work & Numerics Research Direction

> Written 2026-06-25, at the close of the QA/property-testing workstream (§5 of
> `docs/TESTING.md`). Two parts:
>
> 1. **What's remaining** — first the marginal QA items, then the honest answer to
>    *"what's left for Jestyr to be a proper systems-level language?"*
> 2. **Numerics research** — how to draw on CJC-Lang (per
>    `Jestyr-CJC-Lang-Inspiration.md`) for the numeric stack while keeping Jestyr's
>    C-like speed, with the one architectural seam that decides whether it works.
>
> Grounded in the actual tree at the time of writing: `src/*.rs`, `docs/TESTING.md`,
> `ROADMAP.md`, `HANDOFF.md`, `jestyr-design.md`, `MOTLEY.md`. Note `ROADMAP.md` is
> *stale on status* (it snapshots "157 tests / ~10K LOC"; the tree is now ~17.4K LOC,
> ~340 tests with `--features c-oracle`, with structs/enums/strings/attributes far
> more complete than it claims) — but its **workstream map A–P is the right skeleton**.

---

## Part 1 — Remaining QA / property-testing items (marginal)

These are the inline `Remaining:` notes left in `docs/TESTING.md §5`. All are
low-priority; the §5 roadmap is otherwise covered end-to-end, and two real codegen
bugs were fixed along the way (if/match value-drop in `let`-initializers `76b2840`;
`from_utf8(bytes(...))` miscompile `7e7d97e`).

| Item | Section | Why it's marginal |
|---|---|---|
| `..`-rest and struct-variant **nested** runtime dispatch | §5.6 | `nested_match.jtr` covers deeper nesting by example; the variant-in-payload case is already runtime-tested (`nested_match_routes_correctly`) |
| **Bolero soak in CI** | §5.10 | `fuzz_determinism`/`fuzz_pipeline` already replay a corpus under `cargo test`; this is a CI-config job (run `cargo bolero test …` for N hours on a schedule), not new code |
| **Per-feature `selfbench` micro-benches** | §5.11 | A generator knob to emit string-/generic-/match-heavy programs so each subsystem's throughput is tracked separately; plus **bytes-per-AST-node** and **emitted-C-bytes-per-source-line** ratios, and a CI **budget canary** (fail if throughput drops below a floor / peak memory above a ceiling) |
| Positional-construction round-trip; `union`/`distinct`/`indirect` in `arb_type_decl` | §5.5 | the generators currently stress the struct/record/enum core + generic functions; these are additive |

**Recommendation:** do §5.11 (the budget canary) next if anything — it converts the
existing `selfbench` baseline into an *enforced* regression gate, which compounds in
value as the compiler grows. The rest are genuinely take-it-or-leave-it.

---

## Part 2 — What's remaining for Jestyr to be a *proper* systems-level language

Jestyr is already a real, featureful bootstrap compiler: `.jtr → C → native`, with
structs/records/enums/ADTs (niche, recursive, generic), MVS ownership + a sound
escape checker, monomorphized generics + methods, slices with bounds-elision,
strings (`str`/`String`/`Builder`/`Cow`/`os_str`, UTF-8, f-strings), regions,
structured concurrency, `extern "c"`, layout attributes, contracts, an attribute
registry, doc generation, and a `@test`/`@bench` runner. Compilation is **provably
deterministic** (the headline `compilation_is_deterministic` property).

What it is **not yet** — the gaps that separate "impressive bootstrap" from "a
language you'd build an OS / a compiler / a numerical runtime in":

### 2.1 Language-semantics gaps (the big ones)

- **Traits / interfaces — 0% (ROADMAP F; no `trait` keyword exists).** This is *the*
  biggest gap. Without it there are **no bounded generics** (`fn max[T: Ord]`), no
  interface polymorphism, no operator traits, no `dyn`. Design §7.3 already specifies
  the shape (traits, *no inheritance*, **constraints checked at definition** — the Zig
  fix). Everything numeric (a `Numeric`/`Float` bound, a pluggable `Reducer`) and
  everything self-hosting (pass/analysis interfaces) waits on this.
- **Function-pointer types — 0% (ROADMAP H).** A `fn(T1, T2) -> R` type, fn values,
  indirect calls. *Cheapest high-leverage win*: unblocks callbacks, a real
  fn-pointer-vtable `Allocator` (retiring the enum-dispatch stand-in), reduction-strategy
  dispatch for numerics, and is the stepping stone to `dyn`. Smaller than traits.
- **Destructors / RAII (`Drop`) — not even on the roadmap.** Today heap cleanup is
  *manual* (`free_ptr`, `string_free`, `gen_free`). A systems language needs a
  deterministic, scope-tied resource-cleanup story (drop glue, or an explicit
  defer/`using`). Without it, every resource is a manual-free footgun and `vec`/`String`
  leak unless freed by hand. This is a genuine semantic gap, not just polish.
- **CTFE / `comptime` — ~10% (ROADMAP G).** Only the generics slice (type-param
  substitution) exists. A small comptime interpreter over the AST unlocks `comptime`
  blocks/consts, reflection-as-comptime, and comptime codegen — and is on Motley's path
  (IR-builder ergonomics).

### 2.2 Codegen completeness (close the "not supported yet" gaps)

The C backend has a tail of `self.diag(... "does not support … yet")` sites
(`src/cgen.rs`). Each is a place a well-typed program is rejected at lowering:

- `Self { … }` literals + fallible generic-struct methods (the two gaps blocking the
  flagship `examples/vec.jtr` end-to-end — HANDOFF §8).
- Ranges as **values**; type-expressions as values; some closures; external-type
  lowering; or-patterns on niche enums; nested patterns on the *flat* switch path
  (the decision-tree `emit_nested_match` handles real nesting, but the flat path bails).
- `concurrent`/`region`/loops in **value position**. **Note:** value-yielding loops
  (`let x = for { break v }`) were *unblocked this session* — the `if`/`match`/block
  value-position lowering (`76b2840`, the `value_sink` + statement-expression machinery)
  is exactly the mechanism a value-yielding loop needs; it's now a small extension.

### 2.3 Runtime / standard library

- **Real I/O.** `print_int`/`print_str`/… are explicitly *"temporary prelude
  intrinsics."* A systems language needs files, stdin/stdout/stderr, process, env, time,
  and (eventually) sockets — behind a clean, allocation-honest API.
- **Collections in-language.** A growable `Vec`-like (the `vec.jtr` flagship), a hash
  map / set, all written *in Jestyr* over the allocator-as-value. (The `dharht`
  experiment is a related deterministic-table study, kept feature-gated.)
- **Number parsing** (`parse(str) -> Result<int|float, …>`) — currently absent.

### 2.4 Backend & determinism (the seam that also governs numerics — see Part 3)

- **The C backend leans on an external `cc` (`gcc -O2 -std=c11`).** For *most* of the
  language this is fine and is a legitimate strategy (Nim, V, early Zig). But it means
  Jestyr does **not control instruction selection or FP contraction** — which is a
  latent threat to the determinism guarantee the moment floating-point enters the
  picture (Part 3 makes this concrete). The C strategy survives, but only if the FP
  flags are *locked* and the numeric-codegen policy is *enforced*, not hoped for.
- **No optimization passes of Jestyr's own** (ROADMAP L, memory-layout pass: field
  reordering, niche-packing, pass-large-aggregates-by-`const*` instead of copying a
  `read` param). Today it defers entirely to the C compiler.
- **No native backend.** Eventually desirable for optimization control, debug info, and
  *full* FP/determinism control — but a large undertaking; not required to be "proper"
  if the C-as-IR contract is pinned.

### 2.5 Concurrency (ROADMAP N, ~50%)

`concurrent { spawn … }` → pthreads + scoped join exists. Missing: atomics, `Mutex`,
channels, task **results** + `await`, escape-checked join-safety, and — the
Motley/numerics one — the **deterministic `par` loop** (thread count is a speed axis
that *never changes the answer*).

### 2.6 Verification (ROADMAP M, 0%)

`@verified` (SMT): turn `requires`/`ensures`/`invariant`/`variant` from runtime asserts
into **static proof obligations**. The design ceiling (ATS/Ada). Long-horizon; the
provability thesis's ultimate expression.

### 2.7 Tooling & modules (ROADMAP O, K)

- Tooling: formatter, LSP (test runner + doc generator already exist). Largely new
  binaries/subcommands → safe parallel work.
- Modules v2: **true per-module namespaces** (today top-level names must be globally
  unique — the flat-namespace limitation), directory-as-module, qualified *type* paths,
  and a real build system (`build.jestyr` + manifest + lockfile + vendored deps).

### 2.8 Self-hosting (ROADMAP P) — the gate

Rewrite the Jestyr compiler in Jestyr. Gated on strings (now largely done), **traits or
fn-pointers** (dispatch tables), and the layout/efficiency work. **Recommended first
step: port the lexer** — small, self-contained, and it surfaces exactly which features
are still missing.

### The short list, ranked by leverage

1. **Function-pointer types (H)** — cheapest unlock; seeds traits, the real allocator
   vtable, and numeric reduction-strategy dispatch.
2. **Traits (F)** — the biggest language gap; everything generic-with-constraints needs it.
3. **Destructors / `Drop`** — the missing safety story for heap resources.
4. **Lock the FP/determinism contract** (Part 3) — small, and it's the prerequisite for
   *any* numeric work that claims reproducibility.
5. Then: codegen-gap cleanup → stdlib/I/O → memory-layout pass → concurrency → self-host.

---

## Part 3 — Numerics: CJC-Lang inspiration at Jestyr's C-like speed

> Goal (your words): use CJC-Lang as inspiration for the numerics, keep Jestyr's C-like
> speed, get determinism + speed, and eventually **rewrite CJC-Lang in Jestyr** — the
> hope being that Jestyr's determinism+speed makes CJC-Lang *better*. This part is the
> research framing. It builds on `Jestyr-CJC-Lang-Inspiration.md` §2 (which already did
> the "what to borrow / avoid" pass) and adds the **one architectural decision** that
> CJC never had to face and Jestyr cannot avoid.

### 3.1 The prime directive, and Jestyr's twist on it

CJC's order is **Determinism > Memory > Latency > Speed** (speed *last*). Jestyr's
move (per the inspiration doc §2.2) is: **keep the determinism guarantee, raise the
lower three** — because most of CJC's perf ceiling comes from being a *dynamically-typed
tree-walking interpreter*, not from determinism itself. Jestyr is a static, layout-pinned
compiler, so it can have determinism *and* C-like speed. That's the whole thesis.

### 3.2 The determinism architecture to adopt (mostly wholesale)

These transfer almost unchanged from CJC and are the *defining* primitives:

- **The binned superaccumulator** (`cjc-runtime/src/accumulator.rs:73`) — the core.
  Each `f64` is binned by its 11-bit exponent into **2048 fixed bins**; within a bin
  values share magnitude so `a+b == b+a` *exactly*; the **merge uses Knuth 2Sum** so it
  is *commutative AND associative*; finalize folds bins in fixed ascending-exponent
  order. Result: a parallel sum is **bit-identical regardless of thread/chunk count**.
  Stack-allocated, zero heap. **This is the single highest-value thing to port** — and
  it needs *neither traits nor fn-pointers*, so it can land first (§3.6).
- **Tiered reductions** — Kahan compensated (serial), pairwise (fixed `len/2` split),
  binned (parallel), strategy-dispatched by `ReproMode {Off,On,Strict} × ReductionContext`.
- **Seeded SplitMix64 RNG with `fork()`** — each lane derives its own stream, so RNG is
  independent of scheduling.
- **No-HashMap-on-ordered-paths** — Jestyr is *already* here: `compilation_is_deterministic`
  proves no `HashMap`/`HashSet` iteration order leaks into output. (The numeric runtime
  must keep that discipline: insertion-order / `BTreeMap`, canonical NaN, little-endian
  canonical byte encoding for any hashing/serialization.)
- **Cross-OS locked-hash canaries in CI** — CJC's `cross-platform-determinism.yml` runs
  ubuntu+windows+macos and asserts 15 locked SHA-256 canaries *every commit*.
  Reproducibility is **gated, not hoped for**. Jestyr should copy this from day one of
  numeric work — it's strictly stronger than unit tests and matches the `c_oracle`
  differential culture already in the tree.

### 3.3 The seam Jestyr *must* own — FP codegen determinism (the new problem)

**CJC never faced this, and Jestyr cannot avoid it:** Jestyr emits C and compiles with
`gcc -O2`. IEEE-754 mandates bit-identical `+ - * /` across x86_64/aarch64 — *but only
if the compiler doesn't contract or reassociate*. The threats:

- **FMA contraction.** `a*b + c` may be fused into a single `fma` instruction with a
  *different rounding* than separate mul+add. GCC/Clang enable `-ffp-contract=fast` by
  default in many configurations, so `a*b+c` silently becomes an FMA — **breaking
  bit-identity between a machine with FMA and one without, and between scalar and SIMD.**
  This is exactly the "no-FMA" pillar from CJC (`accumulator.rs:39`), but for CJC it was
  a *runtime* policy; for Jestyr it's a *codegen* obligation.
- **Reassociation / `-ffast-math`.** Off by default at `-O2` (good), but must be
  *guaranteed* off — one stray `-ffast-math` and every reduction's answer can change.
- **FTZ/DAZ (flush-denormals-to-zero).** A runtime MXCSR state, not a codegen flag — but
  the emitted program must not set it (and the runtime must preserve subnormals).

**The Jestyr-native answer (and why it's a *win*, not a tax):** because Jestyr is a
*provable* language, the no-FMA / no-FTZ / round-to-nearest policy can become a
**checkable invariant of numeric codegen** rather than documentation (inspiration doc
§2.3). Concretely:

1. Emit numeric kernels as **explicit separate mul/add** (never a fused expression the C
   compiler could contract), and where a fused op is genuinely wanted, emit a **software
   two-rounding** (CJC does this) so the result is defined, not hardware-dependent.
2. **Pin the C compile flags** for numeric translation units: `-ffp-contract=off
   -fno-fast-math -frounding-math` (and never `-Ofast`). This is a small change to
   `build_and_maybe_run` / the eventual driver, and it's the *entire* difference between
   "deterministic" and "usually deterministic."
3. **Verify it.** Add a property/golden that the emitted C for a reduction has the
   deterministic shape (no contractable `a*b+c`), and — the strong form — a cross-OS
   canary (§3.2) that locks the SHA-256 of a reference reduction's output. This is the
   same differential-vs-oracle pattern the `c_oracle` suite already uses (`gcc` is the
   oracle there; here a *second platform* is the oracle).

**The strategic call:** *stay C-backed.* The C-as-IR strategy **survives** the
determinism requirement as long as the FP flags are locked and the policy is enforced
and canaried. A native backend (full instruction-selection control) is only worth it if
gcc/clang divergence proves unmanageable in practice — decide that empirically, with the
canaries, not preemptively. This keeps Jestyr's "C-like speed" promise intact.

### 3.4 Where Jestyr *beats* CJC (the headroom to take)

All of these keep determinism untouched while raising memory/latency/speed:

1. **Statically-typed multi-dtype tensors** (native f32/bf16/f16/i8), not CJC's
   everything-promoted-to-f64-plus-a-byte-store. **2–8× memory**, faster compute, and
   **dtype does not change reduction order** → determinism free. CJC's own
   quantized→binned path already proves the pattern.
2. **Correctly-rounded (or fixed-polynomial) transcendental libm.** CJC delegates
   `sin/cos/pow` to the platform libm → its cross-platform bit-identity is *not*
   guaranteed for transcendentals (its documented weak link). Jestyr can ship its own
   correctly-rounded versions and **close the gap** — better portability *and* stronger
   determinism. Distinctly Jestyr, high value.
3. **A deterministic *parallel runtime*, not just parallel kernels.** CJC is `Rc`-bound
   (single-thread) and bolts parallelism onto specific kernels. Jestyr's ownership model
   can **prove disjoint writes** and drive a *general* deterministic task graph (fixed
   reduction order via the binned accumulator) — the `par` loop, scaling everywhere, not
   just in matmul. (Needs ROADMAP N + the disjointness proof from the escape checker.)
4. **Real arena-inline storage via interprocedural escape analysis.** CJC admits arena
   values "still use `Rc` internally" and its escape analysis is intraprocedural. Jestyr's
   stronger static model can do genuine stack/arena inlining — a pure memory/latency win,
   deterministic by construction, a natural extension of the region arenas.
5. **Per-tile Kahan/binned partials** so **tiled matmul == sequential, bit-for-bit** —
   removing CJC's documented "tiled path uses naive accumulation" determinism asterisk at
   negligible cost.
6. **GPU/accelerator offload under a deterministic contract** (fixed tile order → host-side
   binned reduction). CJC has none; the hard part (the order-invariant reduction) is
   already solved by the binned accumulator.

### 3.5 What Jestyr needs *first* before the numeric stack

- **Function-pointer types (H)** → reduction-strategy dispatch (`ReproMode × context →
  accumulator`), pluggable kernels. *Or* traits, but fn-pointers are cheaper.
- **Traits (F)** → a `Numeric`/`Float` element bound for generic tensors, operator
  traits, a `Reducer` interface. The clean long-term shape.
- **Numeric semantics (ROADMAP J)** → *defined* integer-overflow (wrap/saturate/checked —
  determinism cares), the FP policy-as-invariant (§3.3), bit-width-aware literals.
- **CTFE (G)** → comptime tensor shapes / dtype specialization (optional, an optimization).
- The **determinism spine is already strong**: deterministic compilation is proven, the
  no-HashMap discipline is enforced, and the differential-test infrastructure
  (`c_oracle`, `build_and_run_stdout`/`_status`/`_ints`) is *exactly* the shape needed to
  validate numeric kernels against a reference oracle.

### 3.6 A concrete first research slice (do this before traits)

The binned accumulator is the perfect first numeric primitive: it *is* the determinism
core, it's self-contained, it needs **no traits and no fn-pointers**, and it's testable
with the differential infrastructure already in the tree.

- **Step 0 — Lock the FP contract.** Add `-ffp-contract=off -fno-fast-math` to the
  numeric compile path; add a golden that a reference reduction's emitted C contains no
  contractable `a*b+c`; stand up a 2-OS (start small) locked-SHA-256 canary. *Small, and
  it's the prerequisite for everything else claiming reproducibility.*
- **Step 1 — Port `BinnedAccumulatorF64` to Jestyr** as a stdlib value type: 2048
  exponent bins, stack-allocated, 2Sum merge, ascending-exponent finalize. Validate
  **bit-identity vs a Rust oracle** across random value sets *and across chunk splits*
  (the order-invariance property) — the same `proptest` + differential pattern as
  `dharht_memory_matches_hashmap` and the `c_oracle` round-trips.
- **Step 2 — A minimal f64 dense tensor** (row-major + explicit strides, COW buffer) with
  a deterministic `sum`/`dot` routed through the binned accumulator; differential vs a
  Rust reference, and a cross-OS canary on the result hash.
- **Then:** fn-pointers/traits → multi-dtype storage → the deterministic `par` runtime →
  correctly-rounded libm → tiled matmul with per-tile binned partials → (eventually) GPU.

This sequence front-loads the **determinism-defining, dependency-free** primitive,
proves the FP-codegen seam early (where the risk actually lives), and only *then* spends
the big language budget (traits) once the numeric core is known-reproducible.

---

## Appendix — pointers

**This session's reusable test infrastructure (the oracle patterns to mirror for numerics):**
- `src/proptests.rs::c_oracle` — `find_cc`, `compile_c`, `build_and_run_stdout`,
  `build_and_run_ints`, `build_and_run_status` (exit-code/trap oracle), `jestyr_to_c`.
  Feature-gated `c-oracle`; skips without a compiler. **This is the template for
  differential-vs-reference numeric kernel tests.**
- `compilation_is_deterministic` / `fuzz_determinism` — the determinism spine to extend
  to numeric output.
- Known-by-construction oracle pattern (escape/struct/generic props) and the
  teeth-check discipline (deliberately break the checker, confirm the property fails).

**Jestyr planning docs (in-repo):** `ROADMAP.md` (workstream map A–P; status stale),
`HANDOFF.md` (what exists + how), `jestyr-design.md` (the vision), `MOTLEY.md` (the
long-game: Jestyr → self-host → port CJC's CANA/PINN/NSS cost models), `docs/TESTING.md`
(the QA layer this session built out).

**CJC files to read first (per `Jestyr-CJC-Lang-Inspiration.md` appendix), for numerics:**
`crates/cjc-runtime/src/{accumulator,dispatch,runtime_policy,tensor,tensor_simd,
tensor_dtype,quantized}.rs`; `crates/cjc-repro/src/{lib,kahan}.rs`;
`crates/cjc-snap/src/{hash,persist}.rs`; `crates/cjc-ad/src/lib.rs`;
`.github/workflows/cross-platform-determinism.yml`; `docs/memory_model_2_0.md`.
