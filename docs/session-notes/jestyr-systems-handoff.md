> **Internal development log** — kept for provenance. Status lines and counts in this file reflect the moment it was written, not the current state. Start at the repo [README](../../README.md) for current status and verified claims.

# Jestyr — Systems & Verification Handoff (debug info, cross-compile, memory layout, @verified)

> Cold-start handoff for the four remaining "systems-completeness + apex" workstreams,
> in **dependency/ROI order**: (1) **Debug info** (`#line`→DWARF), (2) **Cross-compilation**,
> (3) **Memory-layout pass** (workstream L), (4) **Verification** (workstream M, `@verified`).
> None gate self-hosting; (1) and (2) are cheap table-stakes usability, (3) is the
> optimization Jestyr should own, (4) is the long-horizon thesis apex. Each section is
> self-contained with file:line anchors (verify them — they drift), an increment plan, and
> the **full test rigor** (wiring / unit / property / bolero-fuzz + teeth-verification).
> Everything lands on `master`, one green increment per commit.
>
> **Sequencing vs the live sessions:** O owns `src/main.rs`, N owns the concurrency
> `cgen`/`escape` surface. Debug-info and cross-compile **touch `src/main.rs`** (the cc
> invocation) and debug-info touches `cgen`'s statement emitter — so **run these *after*
> the O and N sessions have merged**, or coordinate, to avoid colliding in `main.rs`/`cgen.rs`.
> Layout (3) and verification (4) are mostly new passes and can start anytime.

---

## Discipline (unchanged — applies to every increment)

Every increment stays `cargo test`-green and **warning-clean**; default `cargo test`
stays toolchain-free (gate anything needing gcc / a cross toolchain / an SMT solver behind
a cargo feature, the way `--features c-oracle` does). Ship the test layers below,
**teeth-verify each new property by mutation** (break it, watch it fail, revert), and
**auto-commit each green increment** (`git commit -F <file>`, one increment per commit), then
fast-forward master: `git -C <repo-root> merge --ff-only <branch>`. Keep all
examples byte-identical (the repo invariant). When a workstream's first cut lands, write a
session summary back to `C:\Users\adame\Downloads\` (one file per workstream, e.g.
`jestyr-debuginfo.md`) and update `ROADMAP.md`.

---

## 1. Debug info — `#line N "file.jtr"` → DWARF (CHEAP, high value)

**State (verified absent).** The emitted C carries no `#line` directives and the cc is
invoked without `-g`, so a debugger/profiler shows generated C, not `.jtr`. Spans already
carry everything needed: `Span` stores byte offsets (`src/span.rs`), and
`span::line_col(src, offset) -> LineCol{ line, col }` (1-based, computed on demand,
`src/span.rs:56`) turns an offset into a line. The module loader keeps **per-file regions**
for diagnostic rendering (`src/module.rs` — `regions`, `module_of`/`render`), which is the
span→(file, local-offset) map you need for multi-file programs.

**Design.**
- Add a helper `span_to_file_line(span) -> (path, u32)`: find the span's owning region
  (file) + local offset via the `Modules` region table, then `line_col` on *that file's*
  source. (A bare global `line_col` on the concatenated buffer gives the wrong line for any
  imported module — must resolve the region first.)
- In `cgen`, emit a C `#line <line> "<file>"` directive at the statement emitter
  (`self.line(...)`, `src/cgen.rs:576`) — MVP: once per function at entry; full: before
  each statement whose span jumps to a new line. Keep it cheap: only emit when the line
  changes, and normalize the path (forward slashes; C `#line` wants a string literal).
- In `src/main.rs`, add **`-g`** to the cc args next to `CC_FLAGS` (`src/main.rs:649`,
  applied at `:671`). gcc then carries `#line` into DWARF, so gdb/lldb/perf/Valgrind map
  back to `.jtr`. (Determinism note: `-g` does not change codegen; the locked
  `-ffp-contract=off`/`-fno-fast-math` invariant is untouched — assert that in a test.)

**Increment plan.** (a) per-function `#line` + `-g`; (b) per-statement `#line`; (c) optional
`#line` for the `assert()`s that lower `requires`/`ensures` so a contract failure points at
the `.jtr` contract, not generated C.

**Anchors.** `src/cgen.rs:576` (`line()` emitter) + the function/statement emit sites;
`src/span.rs:56` (`line_col`); `src/module.rs` (`regions`/`module_of` for span→file);
`src/main.rs:649` (`CC_FLAGS`), `:671` (cc args — add `-g`), `:712` (`find_c_compiler`).

**Tests.**
- **Wiring ("plumbed-in"):** `emits_line_directives` — emit C for an example and assert it
  contains `#line <N> "<...>.jtr"` with the *correct* N for a known construct;
  `cc_invocation_includes_g` — the built cc command contains `-g`; for multi-file,
  `line_directive_points_at_the_imported_file` — a construct from an imported module gets
  that file's path + line, not the root's.
- **Unit:** `span_to_file_line` maps hand-chosen offsets to the right `(file, line)`,
  including the first/last byte of a file and a newline boundary.
- **Property** (`proptests.rs::mod prop`, `arb_*_program`): every emitted `#line` has
  `line >= 1` and `<= file_line_count`; **behavioral invariance** — the program's runtime
  output is byte-identical with `#line` emission on vs off (debug info must never change
  results); **determinism preserved** — same source → byte-identical C *modulo* the `#line`
  lines, and the FP flags are still present.
- **Bolero fuzz** (`mod fuzz`): `fuzz_line_directives` — over arbitrary programs, `#line`
  emission never panics and never produces a malformed directive (line ≥ 1, path quoted, no
  embedded newline).
- **Teeth:** introduce an off-by-one in `span_to_file_line` → `emits_line_directives` fails;
  revert.

---

## 2. Cross-compilation — thread a target triple to the cc (CHEAP-ish, the Zig-style edge)

**State (verified absent).** `find_c_compiler` (`src/main.rs:712`) picks a host cc; there is
no `--target`. `CC_FLAGS` (`:649`) are host flags.

**Design.** Add a `--target <triple>` flag threaded into the build command:
- **Default easy button: `zig cc -target <triple>`** if `zig` is on PATH — it bundles cross
  libcs for dozens of targets, which is what makes this "mostly free." Fall back to a
  `<triple>-gcc`/`<triple>-clang` cross toolchain, else `clang --target=<triple>`.
- A small `select_cc(target) -> (program, extra_args)` keeps the policy in one place; the
  existing `CC_FLAGS` ride along unchanged for **every** target (this is a determinism
  obligation — cross-compiling must not silently drop `-ffp-contract=off`).
- Surface the chosen target in O's `jestyr attest` manifest so a build's target is part of
  its provenance.

**Caveat to document loudly.** Even with `zig cc`, a target needs the right libc and any
target-specific runtime; 32-bit x86 also reopens the x87-vs-SSE determinism question
(prefer `-mfpmath=sse -msse2` there, per the FP-determinism notes). Keep the supported-target
list explicit.

**Increment plan.** (a) `jestyr build --target <triple>` via `zig cc` with a `<triple>-gcc`
fallback + target validation; (b) per-target `CC_FLAGS` tweaks (32-bit SSE); (c) attest
integration.

**Anchors.** `src/main.rs:712` (`find_c_compiler` → generalize to `select_cc`), `:671` (cc
args), `:649` (`CC_FLAGS`).

**Tests.** (Actual cross-*execution* needs toolchains → gate behind a feature like
`c-oracle`; the always-on tests inspect *command construction*, not execution.)
- **Wiring:** `target_threads_to_cc` — with `--target X`, the constructed cc command
  contains the target selector (`-target X` for zig, or `X-gcc`); `host_build_unchanged` —
  no `--target` produces today's exact command.
- **Unit:** triple parse/validation (accept `x86_64-linux-gnu`, reject garbage);
  `select_cc` picks zig-cc when available else the prefixed cross gcc.
- **Property:** for any valid triple, the command is well-formed **and still contains every
  `CC_FLAGS` entry** (the determinism-flags-survive-cross-compile property — this is the
  important one); malformed triples are rejected, never silently host-built.
- **Bolero fuzz:** `fuzz_target_selection` over arbitrary target strings — `select_cc`/parse
  never panic, never emit a command missing the FP flags.
- **Teeth:** drop a flag in the cross path → the "FP flags survive" property fails; revert.

---

## 3. Memory-layout pass (workstream L) — the optimization Jestyr should own (DEFERRABLE)

**State.** gcc lays out the emitted structs today; only `@packed`/`@align(n)`/`@layout(c)`
are translated (`src/cgen.rs:1490` → GNU `__attribute__((packed/aligned(n)))`). No field
reordering, no niche-packing beyond what already exists for `Option(*T)`, and `read`
aggregate params are passed **by value** (copied) — which bites self-host perf (AST nodes
copied on every `read`).

**Design.** A new analysis pass (size/align per type) feeding three independent
optimizations, in ROI order:
1. **Pass large `read` aggregates by `const*`** — above a size threshold, lower a `read T`
   param as `const T*` and pass `&arg` at the call (small types stay by-value). The
   highest-leverage, most isolated piece; pure calling-convention change in `cgen`.
2. **Field reordering** to minimize padding — **only for Jestyr-native aggregates**; never
   reorder a `@packed`/`@align`/`@layout(c)`/`extern "c"` type (those are ABI/FFI-stable by
   contract). Deterministic order (e.g. descending align, then declaration order as
   tiebreak) so the pass is a pure function of the type.
3. **Enum niche-packing** — generalize the existing `Option(*T)`-niche to other
   single-niche enums. Last; the fiddliest.

**Correctness framing.** This is *pure optimization*: it must be **observationally
invariant** (same program output with the pass on or off) and **deterministic** (same source
→ same layout). It is never a correctness gate — which makes the invariance property (below)
the whole game.

**Increment plan.** (1) by-`const*` large `read` params; (2) field reorder for native
structs; (3) niche-packing. Each is a separate commit.

**Anchors.** `src/cgen.rs:1490` (layout-attr translation — the existing seam), the struct
emission + the `read`-param lowering in `cgen`; `src/types.rs` (`Ty`, for size/align).

**Tests.**
- **Wiring:** `large_read_aggregate_passed_by_const_ptr` — a fn with a big `read` struct
  param emits `const T*` and the call site passes `&arg`; `small_read_stays_by_value` — a
  scalar/small struct is unchanged; `packed_type_is_not_reordered` — a `@packed`/`extern`
  type keeps declaration field order.
- **Unit:** size/align computed correctly for known types (incl. nested + arrays); the
  reorder function returns the expected min-padding order and is a no-op on FFI types.
- **Property** (the star): **behavioral invariance** — for `arb_*_program`, the program's
  output is identical with the layout pass enabled vs disabled (run both, diff); **layout
  determinism** — same source → identical layout decisions; **FFI safety** — a generated
  `@packed`/`@align` type's field order/offsets are never altered.
- **Bolero fuzz:** `fuzz_layout_pass` — never panics, never reorders an FFI/packed type,
  always emits valid C; size/align never overflow.
- **Teeth:** force a reorder on a `@packed` type → `packed_type_is_not_reordered` +
  the FFI-safety property fail; revert.

---

## 4. Verification (workstream M, `@verified`) — the thesis apex (LONG-HORIZON / RESEARCH)

**State.** `requires`/`ensures` exist on `FnDecl` (`src/ast.rs:403-407`) and lower to
**runtime `assert()`s** (`src/cgen.rs:1713`, `:1756`); `invariant`/`variant` likewise for
loops. `@verified` would turn those from runtime checks into **static proof obligations**
discharged by an SMT backend — and, on success, *elide* the runtime assert (proven, not
checked).

**Design (scope the first slice tiny — this is XL).** A verification pass behind a cargo
feature (it needs an external solver):
- **Subset first:** `@verified` on **straight-line, pure, integer** functions — no loops, no
  heap/pointers, no calls (or only to other `@verified` pure fns). This is the Dafny-lite /
  SPARK-analyzable core.
- **Mechanism:** generate verification conditions by **weakest-precondition** over the body,
  emit **SMT-LIB**, shell out to **Z3 (or CVC5)**, parse `unsat` (VC negation unsat ⇒
  proved) / `sat` (+ a counterexample model) / `unknown`. On proved, drop the runtime
  assert; on `sat`, a **static error** with the counterexample; on `unknown`/no-solver,
  keep the runtime assert (sound fallback).
- **Later:** loops (use the existing `invariant`/`variant` as the inductive
  invariant/termination measure), then arrays/heap (separation-logic-lite) — each a major
  increment. Ties into the long-term `@deterministic`/Ravenscar concurrency subset and the
  Motley "verify the compiler's own passes" goal.

**Increment plan.** (a) WP + SMT-LIB emission + solver-result parsing for straight-line
integer `requires`/`ensures`, behind `--features verify`; proved ⇒ assert elided; (b) loop
invariants/`variant` termination; (c) arrays/heap. Stop after (a) for a long time — it's
already a real, demoable result.

**Anchors.** `src/ast.rs:403-407` (`requires`/`ensures`); `src/cgen.rs:1713`/`:1756` (the
`assert()` lowering to elide on proof); the contract typeck handling in `src/typeck.rs`.

**Tests.** (Gate behind `--features verify`; needs Z3 on PATH, like `c-oracle` needs gcc.)
- **Wiring:** `verified_fn_with_proved_ensures_elides_the_runtime_assert` (no `assert(` for
  the proven postcondition in the emitted C); `verified_fn_with_false_ensures_is_a_static_error`
  (with a counterexample in the message); `no_solver_falls_back_to_runtime_assert`.
- **Unit:** WP generation for a handful of straight-line fns; SMT-LIB emission for
  arithmetic/comparison/boolean ops; the solver-output parser (`sat`/`unsat`/`unknown`).
- **Property** (the crucial one — **soundness**): for `arb_*` straight-line integer fns the
  verifier *claims to prove*, run the compiled fn on random inputs **satisfying `requires`**
  and assert `ensures` **never** trips at runtime. An unsound verifier (one that "proves" a
  false postcondition) is caught here — this is the teeth of the whole feature. Plus a
  cheap completeness check: a trivially-true `ensures` (e.g. `result == result`) always
  proves.
- **Bolero fuzz:** `fuzz_wp_smt` — WP + SMT-LIB emission over arbitrary integer expressions
  never panics and always emits syntactically valid SMT-LIB; the result parser handles
  arbitrary solver output.
- **Teeth:** weaken a WP rule so a false `ensures` is "proved" → the soundness property's
  runtime differential fails; revert.

---

## Pointers (verify line numbers; search the symbol)

| Thing | Where |
|---|---|
| Statement emitter (inject `#line`) | `src/cgen.rs:576` (`line()`); function/stmt emit sites |
| Offset → line/col | `src/span.rs:56` (`line_col`), `Span` (byte offsets) |
| Span → file (multi-file) | `src/module.rs` (`regions`, `module_of`/`render`) |
| cc invocation (add `-g` / `--target`) | `src/main.rs:671` (cc args), `:712` (`find_c_compiler`), `:649` (`CC_FLAGS`) |
| FP-determinism flags + lock test | `src/main.rs:649` (`CC_FLAGS`), `mod fp_contract_tests` (`:846`) |
| Layout-attr translation (extend for L) | `src/cgen.rs:1490` (`@packed`/`@align`/`@layout(c)`) |
| Contracts (for `@verified`) | `src/ast.rs:403` (`requires`/`ensures`); `src/cgen.rs:1713`/`:1756` (`assert()` lowering) |
| Test-layer conventions | `docs/TESTING.md`; `src/proptests.rs` (`mod prop`/`mod fuzz`/`arb_*`/`mod sha256`/`--features c-oracle`) |
| Roadmap entries | `ROADMAP.md` workstreams L, M (+ the §3 detail) |

## One-line summary

Do **debug info** (`#line`→DWARF + `-g`) and **cross-compile** (`--target` via `zig cc`,
FP flags preserved) first — cheap, table-stakes, but they touch `main.rs`/`cgen`, so run
them after O/N merge. **Memory-layout** (pass big `read` aggregates by `const*`, then
reorder native structs, then niche-pack) is a deferrable pure-optimization pass whose whole
correctness story is the *behavioral-invariance* property. **`@verified`** is the apex:
start with a tiny straight-line-integer WP→SMT(Z3) slice that elides proven asserts, and the
load-bearing test is the **soundness** property (proved ⇒ runtime never trips). Full rigor
(wiring + unit + property + bolero fuzz + teeth) on every increment; one green commit per
increment to master; session summaries back to Downloads.
