# Motley — Architecture & Handoff Note

> A long-term-vision handoff for **Motley**, a deterministic, thermal/energy-aware
> compiler-infrastructure layer (an LLVM *alternative*, not a clone). Read together
> with [`HANDOFF.md`](HANDOFF.md) (Jestyr, Motley's intended implementation language)
> and `jestyr-design.md`. Motley is a **long-term goal** — this note exists so the
> vision, the ecosystem connections, and the build order survive across sessions.

**Status:** vision + architecture; **no Motley code exists yet.** What *does* exist
and de-risks it: **Jestyr** (a working systems language → C backend, this repo) and
**CJC-Lang** (`C:\Users\adame\CJC`), which already ships a production-grade
deterministic **thermal/energy cost-model framework** (CANA / PINN / NSS) that is
*directly transferable* to Motley's thermal strategy (see Part III).

---

## Part 0 — The ecosystem at a glance

```
            ┌──────────── languages / frontends ────────────┐
            │   Jestyr        Carcosa        CJC-Lang        │
            │  (systems)     (?, future)   (deterministic    │
            │                                numerics)        │
            └──────┬──────────────┬───────────────┬──────────┘
                   │              │               │  emit
                   ▼              ▼               ▼
            ┌───────────────────────────────────────────────┐
            │                  M O T L E Y                   │
            │  shared compiler infrastructure (LLVM-role)    │
            │  • Motley IR  • passes  • codegen  • backends   │
            │  • determinism • provenance • thermal/energy   │
            └───────────────────────────────────────────────┘
                   │
                   ▼  native code (+ provenance + audit log)
            ┌───────────────────────────────────────────────┐
            │  targets: x86-64, aarch64, riscv, bare metal   │
            └───────────────────────────────────────────────┘

  Bootstrapping order:  Jestyr (→C, today) ──► Jestyr self-hosts ──►
                        Jestyr implements Motley ──► CJC-Lang/Carcosa retarget to Motley
```

Three facts shape everything:
1. **Jestyr is Motley's implementation language.** Motley should be *written in
   Jestyr* once Jestyr self-hosts. So Jestyr's roadmap (modules, stdlib, self-host)
   is on Motley's critical path.
2. **CJC-Lang has already built the hard, novel part** — a deterministic, auditable,
   trained thermal/energy cost model (CANA/PINN/NSS). Motley's differentiator is
   *generalizing that into shared infrastructure*, not inventing it.
3. **Determinism is the spine, not a feature.** Both Jestyr (no hidden control flow,
   ownership model) and CJC-Lang (10-invariant determinism contract) already treat
   determinism as foundational. Motley inherits that culture.

---

## Part I — The 28 architecture deliverables (condensed)

### 1. Executive Summary
Motley is a **shared, deterministic, auditable compiler-infrastructure layer** that
fills LLVM's role for the Jestyr/Carcosa/CJC-Lang ecosystem, but with four things
LLVM treats as afterthoughts promoted to *architectural primitives*: **determinism,
provenance/explainability, thermal-awareness, and energy-awareness**. It is *not* an
LLVM clone and *not* a research toy — it is sized for a small open-source team,
foundations-first, with ML kept strictly subordinate to deterministic rules.

### 2. Vision Statement
> *Every optimization Motley performs is reproducible bit-for-bit, explainable in one
> sentence, auditable to a provenance record, and aware of its thermal and energy
> cost — and none of that is allowed to slow the common path or compromise
> correctness.* Motley aims at **LLVM-class performance for specific domains**
> (deterministic numerics, systems, embedded/thermal-constrained), not universal
> parity.

### 3. Architectural Principles
1. **Determinism overrides ML, always.** ML *recommends*; deterministic legality
   gates *decide*. (CJC-Lang invariant: "model identity flows into report hashes".)
2. **Every optimization is explainable + auditable.** A pass that can't emit a
   one-line rationale + a provenance entry doesn't ship.
3. **Reproducibility is first-class.** Same inputs + same Motley version → bit-identical
   output *and* bit-identical audit log.
4. **Thermal/energy are core IR-level concepts**, carried as analyses, not bolted on.
5. **Memory efficiency at every layer** (IR node size, pass working sets, arena
   allocation — Jestyr's region refs map straight onto this).
6. **Small-team implementable.** No feature lands without a maintenance story.
7. **No research-for-its-own-sake.** Quantum-inspired / exotic ML stays in a sandbox
   until it beats a deterministic baseline on a real benchmark.
8. **Foundations before ML.** IR + pass manager + determinism + provenance *first*;
   trained heads *last*.
9. **Multi-language from day one in the IR contract**, even while only Jestyr targets it.

### 4. System Architecture (ASCII)
```
  Frontends (Jestyr / Carcosa / CJC-Lang)
        │  lower to
        ▼
  ┌─────────────────── Motley IR (MIR) ───────────────────┐
  │  typed SSA-ish IR + region/effect annotations         │
  │  + cost-relevant features (flops, bytes, alloc, loops) │
  └──────────────┬────────────────────────────────────────┘
                 ▼
  ┌──────── Analysis Framework ────────┐   ┌─── Provenance System ───┐
  │ dataflow, alias, loops, effects,   │──▶│ every decision → record  │
  │ PHYSICAL COST (thermal/energy/mem) │   │ (reason, inputs, hash)   │
  └──────────────┬─────────────────────┘   └──────────────────────────┘
                 ▼
  ┌──────── Pass Manager (deterministic schedule) ────────┐
  │  ML Recommender ──▶ Legality Gate ──▶ apply | veto     │
  │      (advisory)         (authority)                    │
  └──────────────┬────────────────────────────────────────┘
                 ▼
  ┌──────── Codegen / Backend(s) ────────┐  ┌─ Deterministic Build ─┐
  │ instr-sel, regalloc, schedule        │  │ content-addressed, hermetic │
  │ (thermal/energy-aware ordering)      │  │ incremental cache           │
  └──────────────┬───────────────────────┘  └─────────────────────────────┘
                 ▼
         native code + audit log + provenance bundle
```

### 5. Component Breakdown
| Component | Role | Inherit from |
|---|---|---|
| **Motley IR** | typed, region/effect-annotated, cost-feature-carrying | Jestyr AST handle-arena idiom; CJC MIR |
| **Pass Manager** | deterministic schedule; ML-advises/gate-decides | CJC `PassRanker` + `legality.rs` |
| **Analysis Framework** | dataflow/alias/loops/effects + **physical cost** | CJC `features.rs`, `physical_cost.rs` |
| **Provenance System** | per-decision record (reason, inputs, hash) | CJC `CanaReport.canonical_bytes()` |
| **Cost / Optimization Framework** | benefit − physical-penalty per pass | CJC `linear_cost_model.rs`, `pinn_cost_model.rs` |
| **Thermal Framework** | per-fn thermal pressure prediction | CJC `pinn_thermal_v2.rs`, `thermal_cost_model.rs` |
| **Energy Framework** | per-plan energy prediction | CJC `pinn_energy_v1.rs` |
| **Deterministic Build / Incremental** | content-addressed, hermetic, cached | CJC `cjc-repro`, profile DB |
| **ML Recommendation Framework** | trained heads, *advisory only* | CJC PINN training harness |
| **Verification Framework** | parity (two backends ≡), shadow gates | CJC parity + stress tests |
| **Codegen / Backend Arch** | instr-sel/regalloc/schedule, thermal-aware | new (no LLVM) |
| **Profiling / Telemetry** | post-hoc, byte-equality-gated, never in decisions | CJC determinism invariant #7 |
| **Sandbox / Capability** | untrusted passes/plugins run capability-scoped | new |
| **Container-aware compilation** | detect cgroup/TDP limits → cost-model inputs | new (ties to thermal) |

### 6. IR Design Strategy
- **Typed, region-annotated, SSA-ish.** Carry Jestyr's region/effect info into the IR
  so the optimizer *knows* aliasing and lifetimes without re-deriving them — Jestyr's
  ownership model is free alias information for Motley (the `restrict`/noalias story).
- **Cost features are part of the IR contract**, not a side analysis bolted on:
  every function carries `{flops, bytes_r/w, alloc, working_set, loop depth, float
  density}` (CJC's `PhysicalCostQuery` shape). This is what makes thermal/energy
  first-class.
- **Two textual forms**: a human-auditable form (for provenance review) and a compact
  binary (content-addressed). Determinism: `BTreeMap` ordering, stable hashing.
- **Multi-language neutral**: no Jestyr-isms in the core IR; frontends lower to it.

### 7. Optimization Strategy
- Classic passes first (CF, DCE, CSE, LICM, SR, inlining, mem2reg) — the CJC set is a
  proven starting vocabulary.
- Each pass declares: legality preconditions, cost-feature deltas, and a one-line
  rationale template.
- **Benefit = base_reward − physical_penalty(thermal, energy, memory, locality).**
  ML refines the reward; a **legality gate** has veto authority (CJC: strict-reduction
  passes can't reorder; `nogc` fns block allocating passes).
- Aggressive passes (unroll, vectorize, specialize, monomorphize) are **gated on
  thermal pressure** (CJC `THERMALLY_AGGRESSIVE_PASSES`, 0.5× benefit when hot).

### 8. Determinism Strategy
Adopt CJC-Lang's **10-invariant determinism contract** wholesale (it's proven over
~100K LOC / 6.7K tests):
`BTreeMap` everywhere · Kahan/compensated reductions · **no FMA** (named intermediate
per product) · seeded SplitMix64 · FNV-1a hashing · `f64::total_cmp` ordering · **no
wall-clock/sensors in decision paths** (telemetry is post-hoc, byte-gated) · model
identity flows into output hashes · parity between two backends as final authority.

### 9. Memory Efficiency Strategy
- **IR node size budget** (small handles, not pointers — Jestyr's `u32`-handle arena
  idiom).
- **Arena/region allocation** for compiler data structures (Jestyr's `&[r]T` regions
  are the user-facing version of the same idea; Motley uses arenas internally).
- **Pass working-set budgets**, measured and regression-tracked.
- **Memory-pressure as a cost axis** (CJC already predicts `memory_pressure`).

### 10. Thermal Control Strategy  ← *the differentiator; see Part III*
A multi-layer deterministic thermal cost model, inherited from CJC-Lang:
**(L1)** closed-form physical model (flops/threads/batch → heat, with a `cooling_rate`);
**(L2)** static feature analysis (float density via type-mix propagation);
**(L3)** a *trained* linear thermal head (offline ridge regression, loaded read-only);
**(L4)** optional runtime-dynamics refinement (NSS). `thermal = max(L1, L3, L4)`.
Thermal pressure ∈ [0,1] per function; aggressive passes penalized when hot.

### 11. Energy Optimization Strategy
Per-plan energy prediction (CJC `pinn_energy_v1`, R²≈0.82 on divergent rows):
log-target (`ln(score)`) linear head over workload + structural (loop) + per-pass
features. Energy is a *ranking* signal for pass plans, gated by determinism. Known
limit: residual is nonlinear; an MLP head is justified but deferred (Phase 6).

### 12. Provenance & Explainability Strategy
- **Every decision emits a record**: `{pass, target, reason (1 line), input features,
  model identity+version, output hash}`. CJC's `CanaReport.canonical_bytes()` is the
  template — two runs with different models are *distinguishable by hash*.
- **Explainability is a gate**: an opaque ML recommendation that can't be reduced to a
  feature-attribution + a deterministic rationale is *not applied*.
- The audit log is itself reproducible (part of the build output).

### 13. ML Integration Strategy
- ML is **advisory and offline-trained**. Weights are fit deterministically (ridge
  regression today), committed as read-only artifacts, loaded with hard-fail on
  corruption (CJC `--pinn-weights`, CPB bundle format).
- **Shadow gating**: a new model must beat the old on held-out *selector regret*
  before promotion — not just lower MAE (CJC found MAE and regret disagree).
- **Determinism gate is upstream of ML output**: ML never sees wall-clock or sensors.

### 14. Multi-Language Strategy
- The IR is language-neutral; frontends (Jestyr first, then CJC-Lang/Carcosa) lower to
  it. CJC-Lang already has a mature MIR + cost model — its frontend is a strong second
  adopter and a forcing function for IR generality.
- A small **frontend conformance suite** (same IR semantics across frontends).

### 15. Backend Strategy
- **Bootstrap via C** (exactly what Jestyr does today): Motley's first backend emits C,
  reusing the host C compiler — fastest path to running code, lowest risk.
- A **native backend** (instruction selection + register allocation + scheduling)
  comes later, with thermal/energy-aware *instruction scheduling* as the novel hook.
- **Explicitly not LLVM/Cranelift** (the "Motley" name *is* the own-backend decision).

### 16. Incremental Compilation Strategy
- **Content-addressed everything** (IR, analyses, codegen): a change recompiles only
  the affected content hashes. CJC's profile DB + report-hash discipline is the seed.
- Deterministic cache keys = (IR hash, pass-plan hash, Motley version).

### 17. Profiling & Telemetry Strategy
- Telemetry is **post-hoc and byte-equality-gated** (CJC invariant #7): it can *inform
  the next build's* cost model but never the *current* deterministic decisions.
- Profile-guided optimization persists measured runtime/energy back into the corpus
  (CJC Phase 5 design).

### 18. Security & Sandbox Strategy
- Passes/plugins run **capability-scoped** (no ambient filesystem/network). Untrusted
  ML models load read-only and can only *recommend*.
- Reproducible builds are themselves a supply-chain defense (bit-identical output is
  auditable).

### 19. Container Strategy
- **Container-aware compilation**: detect cgroup CPU/memory limits and (where exposed)
  thermal/TDP envelopes, and feed them as cost-model inputs — so a build inside a
  thermally-throttled container optimizes differently and *records why*.

### 20. Risk Analysis
| Risk | Severity | Mitigation |
|---|---|---|
| Building a backend without LLVM | **High** | Bootstrap via C first; native backend is Phase 2+ |
| Jestyr not mature enough to implement Motley | **High** | Jestyr modules/stdlib/self-host are prerequisites (Part VI) |
| ML nondeterminism leaks into builds | High | Determinism gate upstream of ML; parity tests |
| Thermal model is hardware-generic (not real watts) | Medium | CJC's known limitation; per-platform retraining + container TDP inputs |
| Small team, large surface | **High** | Phase-gated; postpone list (Part V) is aggressive |
| Provenance overhead slows builds | Medium | Audit records are append-only + content-addressed; sample in `-O0` |

### 21. Technical Debt Analysis
- **Inherited debt is low** because the novel pieces (thermal/energy/determinism) are
  *already production-tested in CJC-Lang* — Motley adapts, not invents.
- **New debt risk**: the native backend (regalloc/scheduling) is where complexity
  accretes; keep the C backend as a permanent fallback/oracle.
- The thermal model's hardware-generic coefficients are *known* debt — track them
  explicitly; don't let "good enough generic" calcify.

### 22. Research Opportunities
- **Thermal/energy-aware instruction scheduling** (genuinely novel vs LLVM).
- **Auditable ML optimization** as a publishable methodology (determinism + provenance
  + shadow-gating).
- NSS-style **runtime-dynamics-informed compilation** (CJC's `cjc-nss`).
- Compression-aware IR; PINN-based cost models; *deterministic* ML — all CJC research
  threads that generalize.

### 23. vs LLVM
LLVM wins on: maturity, target breadth, optimization depth, ecosystem. Motley wins on:
**determinism/reproducibility, provenance/auditability, thermal/energy-awareness as
primitives, and a smaller, comprehensible codebase** for a focused domain. Motley does
*not* try to match LLVM's universality.

### 24. vs Cranelift
Cranelift: fast JIT/baseline codegen, simpler than LLVM, Rust-native. Motley overlaps
on "simpler than LLVM" but diverges hard on the determinism/provenance/thermal axes and
on the multi-language-shared-infra goal. Cranelift is a useful *design reference* for a
lean backend; Motley is not a JIT.

### 25. vs GCC
GCC: enormous, mature, GPL, great codegen, opaque decisions. Motley's pitch is the
inverse: small, auditable, every decision explained — for domains where *why the
compiler did that* matters (safety-critical, energy-constrained, reproducible science).

### 26. Open-Source Community Strategy
- Lead with the **differentiator demos**: "show me *why* the compiler made this choice"
  and "this build is bit-identical on your machine and mine."
- Keep the core small and the contribution surface modular (passes + cost models +
  frontends are separable).
- Determinism + provenance make CI trivially trustworthy — a community selling point.

### 27. Adoption Strategy
- **Dogfood**: Jestyr → Motley, then CJC-Lang → Motley. Two real in-house frontends
  before courting external ones.
- Target **niches LLVM underserves**: reproducible numerics (CJC's domain),
  thermal/energy-constrained embedded, safety-critical/auditable builds.

### 28. Success Metrics
- **Correctness**: 100% parity (two backends ≡) on the conformance suite.
- **Determinism**: bit-identical output + audit log across machines/runs (gated in CI).
- **Performance**: ≥ 0.9× hand-C on the benchmark suite for the target domain (Jestyr
  already hits 0.985× — the bar is "don't regress it").
- **Explainability**: 100% of applied optimizations have a one-line rationale + record.
- **Thermal/energy**: measurable reduction in modeled peak thermal/energy on a fixed
  corpus vs the un-gated baseline.

---

## Part II — (covered inline above as deliverables 1–28)

---

## Part III — The CJC-Lang thermal/energy inheritance (Motley's head start)

**This is the most important section.** CJC-Lang (`C:\Users\adame\CJC`, a large
multi-crate Rust workspace — **31 crates** incl. `cjc-cana`, `cjc-nss`, `cjc-cana-nss`,
`cjc-mir`, `cjc-repro`) has *already built* a deterministic, trained, auditable
thermal/energy cost-model framework — the exact thing Motley's thermal and energy
strategies need. Motley should **adapt CANA/PINN/NSS, not reinvent them.**
*(Claims below were verified against the actual source on 2026-06-20; see the
reconciliation note at the end of this section for what was corrected.)*

### What CJC-Lang already has (production-ready)
- **CANA** (Compiler-Aware Neural Architecture) in `crates/cjc-cana` — the cost-model
  + pass-ranking framework.
- **PINN v1** (`physical_cost.rs`): a deterministic *closed-form* physical model.
  Input `PhysicalCostQuery {flops, bytes_r/w, alloc, working_set, threads, batch,
  float_ops, …}` → output `PhysicalCostEstimate {thermal_pressure, memory_pressure,
  bandwidth_pressure, energy_estimate, locality_risk, confidence}`, all ∈ [0,1].
  Heat model: `heat = norm(flops)·(1+thread_amp·Δthreads)·(1+batch_amp·Δbatch) +
  decompress_heat`, then `thermal = clamp01(heat · (1−cooling_rate))`. Default
  `cooling_rate = 0.05`. **No FMA, every product named** (determinism).
- **PINN v2** (`pinn_thermal_v2.rs`): a *trained* linear thermal head (offline ridge
  regression), **7 features** incl. **float density** (`float_ops/flops` — the dominant
  signal, corr ≈ +0.95 with the label). Held-out **R² jumped from −0.05 → ≈0.96** once
  the type-blind feature gap was closed (`TypeMix → float_ops_estimate`); fit on a
  **1,474-row** ablation corpus (n=134 programs) — linear *saturates* there, so an MLP
  chasing the residual would overfit. Shadow-gated before promotion; loaded read-only
  (CPB0 bundle); hard-fail on corrupt. *(Verified. The earlier draft's "R² 0.9558 / MAE
  0.0336 / 9× MAE improvement" was false precision/fabricated — the source frames the
  win as the R² −0.05 → ≈0.96 jump, not an MAE ratio.)*
- **PINN energy v1** (`pinn_energy_v1.rs`): trained linear head predicting `ln(score)`
  (log baseline-relative energy), R²(test) **0.82** on divergent rows. Key finding:
  raw-energy targets fail (heavy tail); **log target + loop/structural features** work.
- **NSS** (`crates/cjc-nss`): a Neural Systems Simulator modeling infrastructure as a
  dynamic pressure system — 9 pressure kinds incl. **Thermal**, temporal scales,
  failure prediction, causal attribution; spectral-norm-bounded (stable by
  construction), fully deterministic. Phase-4 wiring (`crates/cjc-cana-nss`) projects
  compile-time features onto NSS topology.
- **Determinism contract** (`docs/cana/DETERMINISM_CONTRACT.md`): the 10 invariants
  Motley adopts wholesale (Part I §8).
- **Verification**: parity gate (AST-interp ≡ MIR-exec, byte-identical), 50-seed FP
  stress, shadow-energy selector-regret gate.

### The decision pipeline Motley should copy
```
features (CFG + memory proxy + type-mix float propagation)
   → PhysicalCostQuery
   → PINN v1 closed form  (thermal, memory, bandwidth, energy, locality, confidence)
   → PINN v2 thermal head (refines thermal only, if attached)
   → NSS                  (refines all axes with runtime dynamics, Phase 4)
   → thermal = max(closed_form, v2, nss)
   → PassRanker (benefit − penalty) → Legality Gate (veto authority) → PassPlan
```

### What Motley reuses vs must adapt
| Reuse directly | Adapt for Motley |
|---|---|
| Determinism contract (10 invariants) | IR mapping: `PhysicalCostQuery` extraction is MIR-specific → re-target to Motley IR |
| PINN closed-form equations + coefficient defaults | Retrain coefficients/heads on Motley's passes + target hardware |
| Trained-head workflow (offline fit, shadow gate, read-only load, CPB format) | Pass-vocabulary alignment (energy head is per-pass) |
| NSS integration trait surface (`PressurePredictor`) + projection design | Hardware profiling: real TDP/thermal time-constants (CJC is hardware-*generic*) |
| Parity + shadow-gating verification pattern | — |

### Known limits to carry forward (don't re-discover them)
- CJC's thermal model is **hardware-generic** (per-window heat accumulation, not real
  wattage). Motley needs per-platform retraining + **container/cgroup TDP inputs**
  (Part I §19) to make it physical.
- **Single-threaded determinism** today; multi-threaded deterministic compilation is
  open.
- Energy residual is **nonlinear** — a linear head caps out ~0.82 R²; an MLP is
  justified but is Phase-6 work (and must stay deterministic + auditable).

### Key CJC file map (for the future Motley implementer)
`crates/cjc-cana/src/physical_cost.rs` (thermal v1, `PhysicalCostQuery`, `cooling_rate
= 0.05`) · `pinn_thermal_v2.rs` (thermal v2) · `pinn_energy_v1.rs` (energy) ·
`type_mix.rs` (float propagation) · `features.rs` · `pass_ranker.rs` ·
`legality.rs` (veto gate) · `report.rs` (`CanaReport`) · `hash.rs` (FNV-1a `CanaHasher`)
· `cjc-cana/tests/determinism.rs` · `docs/cana/DETERMINISM_CONTRACT.md`.

### Reconciliation note (verification on 2026-06-20)
The background agent that originally surveyed CJC-Lang **failed to complete cleanly**,
so this section was re-verified directly against the source. Outcome: the
**architecture, file map, cost-model pipeline, the energy findings (R² 0.82, `ln(score)`
target, raw-target-fails-on-heavy-tail, regret≠MAE), `cooling_rate = 0.05`, the
float-density signal, and the entire 10-invariant determinism contract** (no-FMA,
Kahan, no-reassociation, SplitMix64, FNV-1a, `total_cmp` — all in
`DETERMINISM_CONTRACT.md`) are **accurate**. **Corrected:** the thermal head's
"R² 0.9558 / MAE 0.0336 / 9× MAE" figures (false precision/fabricated → real is the
R² −0.05 → ≈0.96 jump) and the workspace size ("22 crates" → **31**). The "~100K LOC /
6,715 tests" counts were **unverified** and have been dropped. Treat any remaining bare
number here as "verify before quoting."

---

## Part IV — Roadmap (Phases 0–8)

For each phase: **Goals · Deliverables · Arch changes · Risks · Dependencies ·
Complexity · Benefits · Testing · Benchmarking · Exit.**

### Phase 0 — Foundations
- **Goals:** Motley IR v0 + deterministic pass manager skeleton + provenance record.
- **Deliverables:** typed IR (text+binary), `BTreeMap`-deterministic pass schedule, a
  no-op pass that emits a provenance entry, content-addressed IR hashing.
- **Arch:** none yet — establish the contracts.
- **Risks:** over-designing the IR. **Mitigation:** copy CJC's MIR + Jestyr's handle
  arena.
- **Deps:** none (can prototype in Rust before Jestyr self-hosts).
- **Complexity:** Medium. **Benefits:** the spine everything attaches to.
- **Testing:** IR round-trip (text↔binary↔hash) determinism. **Bench:** IR build time.
- **Exit:** lower a trivial Jestyr program to Motley IR and back to C, with a stable
  hash + a provenance log.

### Phase 1 — Minimal Viable Motley
- **Goals:** end-to-end Jestyr → Motley IR → **C backend** → binary, with 3–4 classic
  passes (CF, DCE, CSE) each emitting rationale + provenance.
- **Deliverables:** C backend; legality-gated pass manager; provenance bundle.
- **Arch:** add Analysis Framework (dataflow, loops) + Cost feature extraction.
- **Risks:** scope creep into a native backend. **Mitigation:** C only.
- **Deps:** Phase 0; Jestyr stable enough to be a frontend.
- **Complexity:** Medium-High. **Benefits:** a real, auditable compiler.
- **Testing:** parity (Motley-C output ≡ Jestyr-direct-C output) on the examples.
- **Bench:** ≥ 0.9× hand-C; build-time budget. **Exit:** all Jestyr examples compile
  through Motley and match.

### Phase 2 — Production Compiler Infrastructure
- **Goals:** incremental compilation, full classic pass set, native backend *spike*.
- **Deliverables:** content-addressed incremental cache; LICM/SR/inlining/mem2reg;
  prototype instruction selection.
- **Arch:** Incremental system; backend abstraction (C *and* native behind one iface).
- **Risks:** regalloc/scheduling complexity. **Mitigation:** keep C backend as oracle.
- **Deps:** Phase 1. **Complexity:** High. **Benefits:** real-world usable.
- **Testing:** incremental-rebuild determinism (cache hit ≡ cold build). **Bench:**
  incremental rebuild time; native vs C codegen quality. **Exit:** native backend
  passes parity on a subset; incremental builds are bit-identical.

### Phase 3 — Multi-Language Infrastructure
- **Goals:** CJC-Lang frontend targets Motley IR; frontend conformance suite.
- **Deliverables:** CJC-Lang→Motley lowering; shared IR conformance tests.
- **Arch:** harden IR language-neutrality; effect/region annotations generalized.
- **Risks:** IR Jestyr-isms leak. **Mitigation:** two frontends force generality.
- **Deps:** Phase 2; CJC-Lang MIR (exists). **Complexity:** High. **Benefits:**
  proves the "shared infra" thesis. **Testing:** same-IR-same-result across frontends.
- **Bench:** CJC numeric corpus through Motley. **Exit:** a CJC program optimizes +
  runs through Motley, deterministically.

### Phase 4 — Deterministic Optimization Platform
- **Goals:** the full determinism + provenance story productionized; CJC determinism
  contract enforced in CI.
- **Deliverables:** reproducible-build verifier; cross-machine bit-identity gate; full
  audit log.
- **Arch:** Provenance System hardened; telemetry strictly post-hoc.
- **Risks:** provenance overhead. **Mitigation:** content-addressed, sampled at `-O0`.
- **Deps:** Phase 3. **Complexity:** Medium. **Benefits:** *the* differentiator
  shipped. **Testing:** two machines → identical output+log. **Bench:** provenance
  overhead < 5%. **Exit:** bit-identical builds gated in CI.

### Phase 5 — Thermal & Energy-Aware Infrastructure
- **Goals:** port CANA/PINN/NSS thermal+energy cost models onto Motley IR; thermal-aware
  scheduling.
- **Deliverables:** physical cost analysis (L1 closed form); trained thermal head (L3);
  energy head; thermal-gated aggressive passes; **container/cgroup TDP inputs**.
- **Arch:** Thermal + Energy frameworks as first-class analyses; backend scheduling
  consults them.
- **Risks:** hardware-generic coefficients. **Mitigation:** per-platform retraining +
  container TDP; keep models advisory (gate decides).
- **Deps:** Phase 4 (determinism) + CJC CANA port. **Complexity:** Medium (the science
  is done in CJC). **Benefits:** the *novel* capability vs LLVM. **Testing:** thermal
  model determinism; shadow-gate before promotion. **Bench:** modeled peak thermal/energy
  reduction on a fixed corpus. **Exit:** thermal/energy-gated optimization measurably
  lowers modeled peak heat without regressing correctness or speed.

### Phase 6 — Auditable ML Optimization
- **Goals:** ML recommenders productionized — *advisory, offline, explainable, gated*;
  possibly an MLP energy head.
- **Deliverables:** trained-head load path; shadow-gating CI (selector regret, not just
  MAE); feature-attribution per recommendation.
- **Arch:** ML Recommendation Framework formalized; determinism gate upstream of ML.
- **Risks:** ML nondeterminism / unexplainable outputs. **Mitigation:** the two
  hard rules — determinism overrides ML, no opaque recommendation applied.
- **Deps:** Phase 5. **Complexity:** Medium-High. **Benefits:** better pass selection,
  *auditably*. **Testing:** ML-on ≡ ML-off determinism; explanation-coverage = 100%.
- **Bench:** pass-selection regret vs deterministic baseline. **Exit:** an ML head beats
  the deterministic ranker on held-out regret *and* every recommendation is explained.

### Phase 7 — Ecosystem Expansion
- **Goals:** Carcosa frontend; plugin/pass SDK; sandbox/capability framework; package
  story.
- **Deliverables:** capability-scoped pass plugins; third frontend; docs.
- **Risks:** community surface area. **Mitigation:** modular, small core.
- **Deps:** Phase 6. **Complexity:** Medium. **Exit:** a third frontend + an external
  pass plugin run sandboxed.

### Phase 8 — Long-Term Research
- **Goals:** the speculative threads — compression-aware IR, quantum-inspired
  optimization, deeper PINNs/NSS, formal verification of passes.
- **Constraint:** each lives in a **sandbox** and must beat a deterministic baseline on
  a real benchmark before touching the default path.
- **Exit:** none (ongoing) — graduate ideas into earlier phases when they prove out.

---

## Part V — Decisions

### Features to POSTPONE (valuable, but not now)
- Native backend *depth* (advanced regalloc/scheduling) — Phase 2+; C backend first.
- MLP/nonlinear ML heads — Phase 6; linear heads first.
- Multi-threaded deterministic compilation — after single-threaded is rock-solid.
- Full NSS runtime instrumentation (Option B) — analysis-only first.
- Carcosa frontend — after Jestyr + CJC-Lang prove the IR.

### Features to NEVER implement
- **An LLVM-bitcode-compatible IR** (defeats the purpose; you'd just be LLVM).
- **ML with decision authority** (must always be advisory under a deterministic gate).
- **Wall-clock / live sensors in the optimization decision path** (kills reproducibility).
- **Hidden/ambient global state** (allocators, config) in the IR or passes.
- **Speculative research on the default path** before it beats a deterministic baseline.

### Recommended BUILD ORDER
1. **Mature Jestyr** to self-hosting (modules → stdlib → self-host — see Part VI).
2. **Motley IR v0 + pass manager + provenance** (Phase 0).
3. **C backend + classic passes** (Phase 1).
4. **Incremental + native backend spike** (Phase 2).
5. **CJC-Lang frontend** (Phase 3 — second adopter forces IR generality).
6. **Determinism/provenance productionized** (Phase 4 — the differentiator).
7. **Port CANA/PINN/NSS thermal+energy** (Phase 5 — the *other* differentiator).
8. **Auditable ML** (Phase 6), then **ecosystem** (7) and **research** (8).

### Recommended contributor profile
Rust systems engineers comfortable with: IR/SSA design, deterministic numerics (FP
reproducibility), and a *taste for auditability over cleverness*. One contributor who
has shipped a small backend (instr-sel/regalloc). The thermal/energy ML work needs
*one* person fluent in deterministic ML (offline ridge/MLP, shadow-gating) — not an
ML researcher chasing SOTA. Culture fit: "determinism and explainability are
non-negotiable."

### Highest-ROI investments (ranked)
1. **Motley IR + determinism + provenance** (Phase 0/1/4) — the entire value prop.
2. **C-backend bootstrap** — fastest path to a real compiler, lowest risk.
3. **Porting CJC's thermal/energy framework** — the novel capability is *already built*;
   porting it is the highest leverage per unit effort.
4. **Jestyr self-hosting** — unlocks Motley-in-Jestyr (the long-game).
5. **Incremental compilation** — practical adoption.

### Most technically RISKY initiatives (ranked)
1. **Native backend** (regalloc/scheduling) — where compilers go to die; mitigate with
   the C oracle.
2. **Jestyr maturity on Motley's critical path** — if Jestyr stalls, Motley stalls.
3. **Hardware-physical thermal modeling** — moving from generic coefficients to real
   watts/TDP is hard and platform-specific.
4. **Multi-threaded determinism** — deep, easy to get subtly wrong.
5. **Auditable ML that actually helps** — easy to build, hard to make *worth it*.

### Final assessment
**Can Motley become a compelling LLVM alternative *for specific domains*? Yes —
realistically, for a focused set.** It will not beat LLVM at universal codegen breadth,
and shouldn't try. But for **reproducible numerics, thermal/energy-constrained and
embedded targets, and safety-critical/auditable builds**, Motley's primitives
(determinism, provenance, thermal/energy-awareness) are things LLVM structurally
*doesn't* offer — and the hardest, most novel component (the deterministic thermal/energy
cost model) **already exists and is tested in CJC-Lang.** The binding constraints are
scope discipline and Jestyr's maturity, not feasibility of the core idea. Verdict:
**ambitious but credible, *if* foundations-first discipline holds and Jestyr reaches
self-hosting.**

---

## Part VI — How Jestyr connects (the bridge) + Jestyr's remaining work

Motley is meant to be **written in Jestyr**, so Jestyr's roadmap is Motley's
prerequisite path. Jestyr today (see [`HANDOFF.md`](HANDOFF.md)): a working
systems language → C backend, ~90+ tests, the **tiered-safety reference model**
(`&T` gen-refs + `&[r]T` region-refs — region-ref codegen was the most recent landing
and should be confirmed green), restrict-based speed (0.985× hand-C, verified), slices +
bounds-check elision, structured concurrency, contracts, C interop, layout attributes,
MVS defaults. What Jestyr still needs **before it can implement Motley**:

| Jestyr item | Why Motley needs it | Status |
|---|---|---|
| **K — Module/package system** | can't write a compiler in one file | *not started — the gate* |
| **I — Stdlib + allocator-as-value** | collections, I/O, arenas for the compiler | not started |
| **G — CTFE + reflection** | comptime codegen / IR-builder ergonomics | only the generics slice |
| **N — Self-hosting** | Motley-in-Jestyr requires it | not started |
| Traits/`dyn` | pass/analysis interfaces | not started |
| `@verified` (SMT) | verify Motley's own passes | not started |

And the **memory-efficiency workstream** (a layout pass → field reordering, enum
niche-packing, pass-large-aggregates-by-ref, arenas) directly serves Motley's
"memory efficiency at every layer" principle — Jestyr's `&[r]T` regions are already
the user-facing arena story.

**Bottom line for sequencing:** the next Jestyr work that *most* advances Motley is
**K (modules) → I (stdlib) → N (self-hosting)** — the systems tier. The language
theory (D/E/etc.) is now largely in place; the gate to Motley is making Jestyr big
enough to build a compiler in.
