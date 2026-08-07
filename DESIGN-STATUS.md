# Design → implementation status

[jestyr-design.md](jestyr-design.md) is a *vision* document ("draft, not a
specification") and deliberately describes more language than exists. This
page is the one-screen map of which designed features are real. A skimming
reviewer should trust this table over any individual doc's prose;
per-workstream detail lives in [ROADMAP.md](ROADMAP.md) §2.

**Legend** — ✅ implemented (both toolchains unless noted) · 🟡 partial ·
📐 designed only (no implementation).

| Design area (§ in jestyr-design.md) | Status | Notes |
|---|---|---|
| Ownership: move-by-default, `read`/`mut`/`out`/`take` conventions (§4) | ✅ | the escape checker; see [docs/escape-guarantee.md](docs/escape-guarantee.md) |
| Second-class borrows, no lifetimes (§4–5) | ✅ | validated at self-hosting scale |
| Generational references `genref` (§5) | ✅ | checked deref, deterministic fault |
| `region` arenas + region refs (§5) | ✅ | escapes are compile errors |
| RAII `Drop`, recursive field/payload auto-drop (§4) | ✅ | |
| `@copy` opt-in copyability (§4) | ✅ | |
| Error sets `!{ … }` + `?` (§6) | ✅ | sound set inclusion checking |
| Payload-carrying errors, `catch \|e\| match` (§6) | ✅ | stack-only payloads |
| Error sets in trait signatures / through `dyn` (§6) | ✅ | |
| Owning payloads, named error sets (§6) | 📐 | future work, see docs/error-payloads.md |
| Structs, enums (payloads, niche opt), exhaustive `match` (§7) | ✅ | |
| `distinct` types, `record` immutability (§7) | ✅ | |
| Slices `[]T`, bounds checks + refinement elision (§7) | ✅ | elision only under proof |
| Refinement types beyond bounds-elision (§7) | 📐 | |
| Generics + monomorphization, trait bounds (§8) | ✅ | |
| Traits, `dyn` dispatch, operator traits (§7–8) | ✅ | |
| CTFE / `comptime` (§8) | ✅ | tiers 0–7, both toolchains; dogfooded in the compiler's lexer |
| Modules: `import`, visibility, content hashing, manifest verify (§9) | ✅ | generic-struct cross-module collisions still open |
| Package registry / build system beyond the manifest (§9) | 📐 | executable `build.jestyr` needs CTFE wiring |
| Structured concurrency (`concurrent`/`spawn`/`await`) (§10) | ✅ | pthreads backend |
| Channels, `select`, `Mutex`, atomics (§10) | ✅ | `spawn` targets can't be generic |
| Data parallelism: `par for … reduce`, SOACs, `@simd` (§10) | ✅ | GPU backend 📐 (out of scope for this release) |
| Checked cost models: `@span`, `@no_alloc`, `@deterministic` (§10, §15) | ✅ | see [docs/attributes.md](docs/attributes.md) |
| Async / effects (§10) | 📐 | |
| `unsafe` blocks, enforced boundary (§11) | ✅ | the completed ladder, [docs/unsafe-contract.md](docs/unsafe-contract.md) |
| `trusted` blocks (§11) | 📐 | |
| `extern "c"` calls out (§12) | ✅ | |
| `import c` (header ingestion) (§12) | 📐 | |
| Bare metal: `@volatile`, `@address`, `@packed`/`@align` (§13) | ✅ | |
| Stdlib: allocator-as-value, `core`/`std` split, `List(T)`, strings (§14) | ✅ | growing; written in Jestyr |
| Deterministic FP: locked flags, parse/format, reductions (§15) | ✅ | cross-OS digest: see FP-DETERMINISM-CONTRACT.md |
| Contracts `requires`/`ensures` (§15) | ✅ | lowered to debug asserts; obligations report exists |
| `@verified` / SMT discharge (§15) | 📐 | `jestyrc obligations` counts them; no solver |
| Tooling: `test`, `doc`, `attest`/`attest-diff` (§15) | ✅ | all four also self-hosted |
| Formatter, LSP (§15) | 📐 | |
| Self-hosting (§19) | ✅ | fixed point + gcc-only bootstrap seed |

Anything not listed follows the design doc's own caveat: treat it as an
idea until this table says otherwise.
