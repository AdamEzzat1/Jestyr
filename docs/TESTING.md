# Testing — the verification ladder

How to check, on your machine, every claim the repo makes — ordered from
free to thorough. Wall-clock times are from a mid-range laptop (Windows,
gcc/MinGW); Linux is comparable. CI (`.github/workflows/ci.yml`) runs
rungs 1–3 on every push.

## The ladder

| Rung | Command | Needs | ~Time | What it proves |
|---|---|---|---|---|
| 1 | `cargo test` | Rust only | ~35 s | the full default suite: unit tests for every stage, `proptest` properties (incl. `compilation_is_deterministic` — byte-identical emission), `bolero` fuzz smoke, corpus sweeps (JSON diagnostics well-formed, unsafe census total) |
| 2 | `cargo test --features c-oracle` | + gcc/cc/clang on PATH | minutes | the emitted C actually compiles and runs: every `examples/std` demo through a real C compiler with output asserts, **plus the locked SHA-256 determinism canary** (`4389bf83…` — integer + own-formatter output only, SIMD lane width pinned) |
| 3 | `cargo test --features c-oracle,selfhost-fixpoint` | same | ~15 min | everything above **plus** the self-hosting evidence: the corpus goldens (the Jestyr-written compiler's C byte-identical to the reference's over every corpus file), the concatenated build, `selfhost_fixpoint_full` (jc2 ≡ jc1 on the compiler's own source), the test-mode goldens, and `bootstrap_seed_is_current` (the committed seed regenerates exactly from live sources) |
| 4 | the two commands in [bootstrap/README.md](../bootstrap/README.md) | gcc only, **no Rust** | ~30 s | the gcc-only bootstrap: build the committed seed, watch the resulting compiler reproduce the seed byte-for-byte, then compile and run programs with it |

Notes:

* Rungs 2–3 are opt-in features precisely so rung 1 stays toolchain-free.
* `REFRESH_SEED=1 cargo test --features c-oracle,selfhost-fixpoint
  bootstrap_seed_is_current` rewrites the committed seed after a compiler
  change — the **two-sided tax**: any change to `examples/std/*.jtr` or to
  emission must land with a refreshed seed in the same commit, or rung 3
  fails.
* The determinism canary's cross-OS status is tracked in
  [FP-DETERMINISM-CONTRACT.md](../FP-DETERMINISM-CONTRACT.md) — a digest
  mismatch on a new platform is a *finding*, not a flake: triage it, don't
  re-lock.

## Test geography

All tests live inside `src/` (no `tests/` directory):

* per-stage unit tests in each stage's own module (`lexer.rs`,
  `parser.rs`, `typeck.rs`, `escape.rs`, `cgen.rs`, …);
* properties, fuzzers, goldens, and the oracle/fixpoint harness in
  `src/proptests.rs` (the feature-gated `c_oracle` module holds rungs
  2–3);
* `proptest-regressions/` pins previously-found counterexamples so they
  re-run forever.

The corpus is the `examples/` tree: every feature has a runnable example,
rejection demos assert their diagnostics, and `examples/std/` — the
self-hosted compiler's own source — is simultaneously the largest test
input. See [examples/README.md](../examples/README.md).

## Writing tests for a new feature

The house pattern, visible throughout the suite:

1. a **unit test per behavior** at the stage that owns it (parse arm,
   `infer` arm, escape route, emission shape), including the rejection
   case with its message text;
2. a **runnable example** under `examples/` with expected output in its
   header comment;
3. if the feature changes emission: the change must be **gated on use**
   (programs not using the feature emit byte-identical C — the corpus
   goldens enforce this), the port must mirror it, and the seed must be
   refreshed in the same increment;
4. if it adds a diagnostic only: no port mirror is due (diagnostics owe
   no two-sided tax), but corpus sweeps must stay green.

## 6. Experiment — D-HARHT (Memory profile) vs `HashMap`

Could the compiler's randomized `HashMap` symbol tables be replaced by **D-HARHT**, a
deterministic hash/radix table whose "seal then look up" model matches a compiler's *build-in-
typeck / read-in-cgen* access pattern? (D-HARHT is a design from the author's CJC-Lang
project; the self-contained implementation lives at [`src/dharht.rs`](../src/dharht.rs)
behind `--features dharht-experiment`, with a comparison
benchmark (`jestyrc dharht-bench`), a differential property test
(`proptests::dharht_experiment::dharht_memory_matches_hashmap` — a sealed D-HARHT must agree with
`HashMap` on every key), and its unit tests.)

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
   `compilation_is_deterministic` property shows the compiler emits byte-identical C today,
   so there's no determinism gap to close here. (If one ever appeared, the cheaper fix is
   `BTreeMap`/sorted iteration, not a radix table.)

**Where D-HARHT *would* shine:** large, **byte-addressable / prefix-heavy**, build-once/lookup-many
tables — e.g. a future self-hosted Jestyr's *runtime* string-interner or path index, exactly the
"byte-first, view-second" workload it was designed for. The experiment, benchmark, and differential
property test stay in-tree (feature-gated, zero cost to the default build) so that case can be
re-measured when it arises.
