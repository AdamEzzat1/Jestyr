# Jestyr Testing Strategy

> Companion to `Jestyr-Loops-Future.md` and `Jestyr-CJC-Lang-Inspiration.md`. Research
> notes on building Jestyr's test infrastructure: what to borrow beyond Rust, and the
> testing capabilities that **only Jestyr can have** because of what it guarantees.
>
> _Draft — 2026-06-22. Current footing: `src/proptests.rs`, `proptest-regressions/`, the
> cgen/parser unit tests. This is what to layer on._

---

## 0. The governing principle: **test the guarantees**

Generic property/fuzz tests over behavior are table stakes — Rust's `proptest`/`cargo-fuzz`
cover that substrate well, and you already use them. The leverage is elsewhere:

> The highest-value tests are the ones that test **what makes Jestyr Jestyr** — determinism,
> the bounds-elision / `@no_panic` proofs, the escape/region checker, and the transparent C
> lowering. No other language can test those the way Jestyr can, because no other language
> *makes those promises in a falsifiable form*.

So the strategy has two halves:
1. **Borrow the compiler-testing canon** (Part 1) — you're testing a *compiler*, so Csmith,
   C-Reduce, PLT Redex, EMI, and SQLite's discipline matter more than another property
   library.
2. **Turn every Jestyr guarantee into something a fuzzer can falsify** (Part 2) — this is the
   original, high-value work, and it's only possible because Jestyr's optimizations and
   safety properties are *proof-derived*, not best-effort.

---

## 1. What to borrow beyond Rust

Rust gives the substrate: `proptest`/`quickcheck` (property testing + shrinking),
`cargo-fuzz`/libFuzzer (coverage-guided fuzzing), `insta` (snapshots), `criterion`
(benchmarks), rustc's `.stderr` UI tests, `miri` (UB detection). Beyond it:

| Source | Tool / idea | What to take | Fit for Jestyr |
|---|---|---|---|
| **C / LLVM research** | **Csmith + C-Reduce** | Random *valid-program* generator + automated **test-case reduction** to a minimal repro | The single highest-leverage thing: a "Csmith-for-Jestyr" + auto-reducer |
| **C / LLVM** | **EMI — Equivalence Modulo Inputs** | Mutate provably-dead code so observable behavior is unchanged; assert equivalent output — **metamorphic testing, no oracle needed** | Jestyr's ownership/effects can prove dead/pure code *soundly*, so EMI is stronger here than on C |
| **Intel** | **YARPGen** | Generated programs carry a **computed checksum** and self-check | Dissolves the oracle problem — the program reports its own miscompilation |
| **Racket / PLT** | **PLT Redex** | A DSL for testing a language's **operational semantics**; generate terms, check **type soundness** as a property | Purpose-built for your exact problem. Famously found bugs in *published* soundness proofs |
| **Haskell** | **Hedgehog** | **Integrated shrinking** (shrinks respect generator invariants automatically) | Cleaner reference model than classic QuickCheck |
| **Haskell** | **SmallCheck / LeanCheck** | **Exhaustive small-scope** enumeration up to a size bound | "Small-scope hypothesis": most compiler bugs surface on tiny ASTs — complements random-large |
| **Erlang** | **QuviQ QuickCheck** (J. Hughes) | **Stateful / model-based** testing (`eqc_statem`) against an abstract model | Canonical for stateful subsystems — module/symbol tables, the arena allocator |
| **Coq** | **QuickChick** | Property testing **fused with a proof assistant**: test a conjecture, then discharge the *same statement* as a proof | Directly bridges the test → `@verified` pipeline (§2.1, §2.2) |
| **OCaml (Jane St.)** | **Crowbar** + **ppx_expect** | **Coverage-guided fuzzing driving property tests**; auto-updating **inline expect tests** | Crowbar = the fuzz/property fusion you want; expect-tests = ideal for golden-C / diagnostics |
| **Go** | native `go test -fuzz` | **Fuzzing as a zero-friction, first-class toolchain feature** with a seed corpus | The *ergonomics* lesson: make `jestyrc test --fuzz` trivial, not a separate ritual |
| **SQLite** | "How SQLite Is Tested" | **MC/DC branch coverage, malloc-failure injection, differential testing, billions of cases** | The engineering-discipline bar. **Malloc-injection** is gold for the arena/alloc paths |
| **TLA+ / Alloy** | model checking | **Model-check the *design*** of a subsystem (region/ownership calculus) independent of code | Check the core rules have no counterexample *before* implementing — fits a provable language |

**Priority for a compiler specifically:** Csmith+C-Reduce → PLT Redex → EMI → QuickChick →
SQLite's discipline. These are worth more than another property-testing library.

---

## 2. Unique ideas — exploit what only Jestyr can do

Roughly in order of value. The throughline: **make each guarantee falsifiable, then fuzz to
falsify it.**

### 2.1 Verification annotations as test specs, generators, AND oracles — for free
You write `requires`/`ensures`/`invariant`/`variant` for *provability*. Harvest them for
*testing* at zero extra authoring cost:
- `requires x in 0..n` → a **contract-directed generator** producing only valid inputs
  (QuickChick-style derived generators). The precondition tells the fuzzer the exact input
  space.
- every `invariant` → a checked assertion under test/fuzz (already lowered to `assert`).
- every `variant` → a **termination monitor**: fail if it ever fails to strictly decrease.
- `ensures` → the postcondition *is* the property oracle.

No other systems language has this, because the contracts aren't in the source. This is the
deepest synthesis: the annotations that make Jestyr provable double as its test harness.

### 2.2 Differentially test the **prover itself**, not just the program
Jestyr elides bounds checks when it proves `i < len`, and `@no_panic` claims no fault path.
Add a **`--audit` build** that re-inserts every elided check as a trap and instruments every
`@no_panic` function, then fuzz:
- an elided check ever fires → **the elision proof was unsound** (a compiler bug, caught
  dynamically).
- a `@no_panic` function reaches a fault path → the proof lied.

This tests the *compiler's reasoning*, not program behavior — possible only because Jestyr's
optimizations are proof-derived and thus falsifiable. **Build this first after the basics.**

### 2.3 Determinism as a universal metamorphic oracle
Because Jestyr *promises* determinism, "the output didn't change" is a free, powerful oracle
other languages can't use (their output legitimately varies). Auto-generate metamorphic
tests asserting **bit-identical** results across:
- the **same program run twice**,
- **opt vs no-opt**, **C-backend vs interpreter** (§2.4), **thread-count N vs M**,
- reduction strategies (`@reduce(strict)` sum at 1 core == 8 cores; serial-Kahan == binned),
- and pin output hashes as **cross-OS CI canaries** (CJC's locked-SHA-256 model).

A `@deterministic` function gets all of this generated automatically. Determinism turns from
a *claim* into a continuously-checked invariant.

### 2.4 A reference interpreter as a differential oracle
Ship a dead-simple tree-walker purely as a test oracle. Compile-and-run (C backend) vs
interpret → must agree **bit-for-bit** (the determinism guarantee makes this exact, not
approximate). The CompCert/CakeML/CJC trick; catches codegen bugs cheaply, and the
interpreter doubles as the EMI/metamorphic baseline.

### 2.5 Golden-C + UI-diagnostic tests that lock in transparency
Transparency is a selling point — make it a *tested contract*. Snapshot the generated C for a
representative corpus (auto-updating expect-tests) and add rustc-style `.stderr` golden tests
for diagnostics. Now a codegen change that alters the lowering produces a *visible, reviewed
diff* instead of a silent regression. (The cgen unit tests already do this in spirit —
systematize it.)

### 2.6 Mutation-test the compiler itself
Run mutation testing on the *Rust source of the Jestyr compiler*: does the suite catch a `<`
flipped to `<=` in the elision logic? Measures whether the tests actually guard the
guarantees — high-leverage "who tests the tests," especially around the proof code.

### 2.7 Escape-checker: negative corpus + runtime canary
The escape/region checker is the novel, risky piece. Test both directions:
- (a) a large **must-be-rejected** corpus (rustc-style UI tests), and
- (b) an **escape-sanitizer** mode that tags region/`scratch` allocations and traps if a
  reference outlives its region at runtime under fuzzing — confirming the *static* rule
  matches *dynamic* reality.

### 2.8 Csmith-for-Jestyr with checksums + auto-reduction (the workhorse)
A generator that emits only **well-typed, ownership/region-valid** Jestyr programs (so they're
guaranteed to compile) carrying a **self-checking computed checksum** (YARPGen-style). Run
them; assert the printed checksum. Pair with **C-Reduce-style automatic reduction** of any
failure to a minimal repro. This is the compiler-fuzzing standard, specialized to Jestyr, and
it feeds 2.2–2.4 with endless inputs.

---

## 3. CJC-Lang is prior art you can copy

The sibling project (`C:/Users/adame/CJC`) already implements several of these patterns —
proven, and directly portable:

- **Backend differential / parity gate** — its `eval`-vs-`MIR-exec` "Parity Gate G-1/G-2"
  asserts both engines agree for every program+seed (this is §2.4).
- **Cross-OS determinism canaries** — `.github/workflows/cross-platform-determinism.yml` runs
  ubuntu+windows+macos and asserts 15 locked SHA-256 hashes every commit (this is §2.3's
  canary half).
- **Metamorphic numeric regressions** — `tests/test_repro_regressions.rs` asserts
  Kahan == binned across all dispatch contexts (this is §2.3's reduction-strategy half).
- **Content-addressed snapshots** — `cjc-snap` (SHA-256 manifests) as a reproducibility ledger.

Read these before building from scratch; the determinism-testing machinery is the most
reusable part.

---

## 4. Suggested build order

```
Foundation (have): proptest + regression corpus + cgen/parser unit tests
   │
Phase 1 — cheap, high-value, no new infra:
   • §2.5 Golden-C + UI diagnostic tests (lock in transparency)
   • §2.1 Harvest invariant/variant into checked asserts under test
   • §2.4 Reference interpreter as differential oracle
   │
Phase 2 — generation:
   • §2.8 Csmith-for-Jestyr (valid-program gen + checksum) + C-Reduce-style reducer
   • §2.1 Contract-directed generators from requires/ensures
   │
Phase 3 — test the guarantees (needs elision/@no_panic/determinism mature):
   • §2.2 --audit build: re-insert elided checks, instrument @no_panic, fuzz the PROVER
   • §2.3 Determinism metamorphic oracle + cross-OS SHA-256 canaries
   │
Phase 4 — depth:
   • §2.7 Escape-sanitizer + negative corpus
   • §2.6 Mutation-test the compiler
   • §1 EMI, PLT-Redex-style semantics testing, TLA+/Alloy model of the core calculus,
     QuviQ-style stateful tests for the allocator/module tables
```

Phase 1 is buildable now and pays immediately. Phases 2–3 are where Jestyr's testing becomes
*distinctively strong*. Phase 4 is the long-tail rigor (the SQLite bar).

---

## 5. Anti-goals

- **Don't let determinism testing become flaky.** If a metamorphic determinism test ever
  fails *nondeterministically*, that's either a real determinism bug (good — you caught it) or
  a harness bug — never paper over it with a retry. Determinism tests must be exact or they're
  worthless.
- **Don't test only the happy path of the prover.** The `--audit` build (§2.2) and the
  must-be-rejected corpus (§2.7) are the point — a prover tested only on programs it accepts is
  untested.
- **Don't hide the oracle.** Prefer self-checking programs (checksums), differential oracles
  (interpreter, opt-levels), and metamorphic relations (determinism) over hand-written
  expected values — they scale with generation; hand-written oracles don't.
- **Don't separate fuzzing from the workflow.** Per the Go lesson, `jestyrc test --fuzz` should
  be one flag, sharing the seed corpus with CI — friction kills fuzzing adoption.
