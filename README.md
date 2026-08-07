# Jestyr

Jestyr is a from-scratch low-level systems language built as a research
vehicle for one question: **how much of Rust-grade memory safety and
C-grade performance can a language keep while dropping lifetimes
entirely?** Its answer is a tiered reference model — second-class borrows
that provably cannot outlive their frame, generation-checked references
(`genref`) that turn use-after-free into a deterministic fault, and
scope-bounded `region` allocation — validated at real scale: **the
compiler is written in Jestyr and compiles itself.**

The project ships **two independent implementations of the same
compiler** — a ~52,000-line Rust reference and a ~25,000-line self-hosted
Jestyr port — held **byte-identical in their C output** over a 148-file
test corpus, plus a committed bootstrap seed that builds the whole
toolchain with nothing but a C compiler.

This is a **research release** (`v0.1.0-research`), not a production
tool. See [Scope](#scope-what-this-is-and-isnt).

## Try it in two commands (no Rust required)

The repository commits the compiler's own C output for its own source.
Build it, then watch it regenerate that seed byte-for-byte:

```bash
gcc -O2 -std=c11 -ffp-contract=off -fno-fast-math -o jc bootstrap/jestyr_seed.c
```

```bash
./jc bootstrap/jestyr_flat.jtr > regen.c && diff regen.c bootstrap/jestyr_seed.c
```

An empty diff is the self-hosting fixed point: the compiler you just
built from the seed reproduces the seed. `./jc examples/hello.jtr run`
compiles and runs a program (cold clone to working compiler: ~21 s on a
laptop). On Windows/MinGW add `-Wl,--stack,67108864` to the gcc line and
see [bootstrap/README.md](bootstrap/README.md).

## The language, briefly

```
fn main() {
    let xs = List(i32).new(system)   // allocator is a VALUE you pass
    xs.push(10)
    for xs read x { print(x) }       // borrow, provably frame-bounded
}                                     // RAII drop, no lifetimes anywhere
```

* **Ownership without lifetimes.** Values are owned and moved; borrows
  (`read`/`mut`) are second-class — they can be passed down but never
  stored or returned, so the escape checker's guarantee is structural.
  For data structures that *need* stored references there are `genref`s
  (generation-checked at every deref) and `region` arenas (escapes are
  compile errors).
* **Errors as sets, with payloads.** `fn parse(s: String) -> i32 !{ Io,
  Parse(i32) }` — error sets are checked soundly through calls, `?`, and
  trait dispatch; payloads are stack-only (no allocation, no dynamic
  dispatch); `catch |e| match` extracts them exhaustively.
* **Checked cost models.** `@span(work, span)` classifies parallel code
  and makes *serializing a `par for` a compile error*; `@simd` is a
  checked legality claim, not a hint; `@no_alloc` is transitive;
  `@deterministic` rejects non-deterministic reductions at compile time.
  See [docs/attributes.md](docs/attributes.md).
* **Deterministic floating point** by construction: locked compile
  flags, our own correctly-rounded parse/format, reproducible parallel
  reductions (binned superaccumulators).
* **Compile-time function evaluation** (a total interpreter with step
  budgets), structured concurrency lowered to pthreads, contracts
  (`requires`/`ensures`), and a C backend that hands the ownership
  model's non-aliasing guarantees to the optimizer as `restrict`.

## The claims, precisely scoped

The repo's credibility asset is that it does not overclaim. Each claim
below states its scope and the command that checks it.

**1. Dual-implementation byte-identity.** The Rust reference and the
self-hosted port emit byte-identical C over the 148-file corpus, the
concatenated self-hosting build, and the compiler's own ~25K-line
source. *Scope:* this holds on the single-file / concatenated emission
path (which is what the corpus, the fixed point, and the seed use). The
module-loader path has three known, documented divergences (the port
emits no `#line` directives, per-type artifact ordering, offset-derived
spawn symbol names), pinned by the test
`jestyr_module_cgen_matches_reference_except_line_directives`.
*Verify:* `cargo test --features c-oracle,selfhost-fixpoint`.

**2. The self-hosting fixed point.** The Jestyr-written compiler,
compiled by itself, reproduces its own C output exactly (jc2 ≡ jc1 over
all ~25K lines). *Verify:* the two-command demo above, or the
`selfhost_fixpoint_full` test.

**3. gcc-only bootstrap.** Building Jestyr from scratch requires only a
C compiler — no Rust. The committed seed is pinned against the live
sources by the `bootstrap_seed_is_current` test.

**4. Floating-point determinism.** Compile flags are locked and tested;
primitives (parse, format, reductions) are deterministic by
construction; a purified SHA-256 canary (integer + own-formatter output
only) locks the observable behavior, including SIMD lane width. *Scope:*
the canary digest (`4389bf83…`) is verified identical on **Windows 11 +
gcc (MinGW)** and **Ubuntu 24.04 + gcc 13.3 (glibc)** — two OSes, two
libcs, one digest — and CI re-checks it on every push. macOS/clang is
untested. Details and verified-platform record:
[FP-DETERMINISM-CONTRACT.md](FP-DETERMINISM-CONTRACT.md).

**5. Safety enforcement.** Raw-pointer operations outside `unsafe` are
compile errors on both toolchains ([docs/unsafe-contract.md](docs/unsafe-contract.md));
bounds checks elide only under refinement proof; `unsafe` has a written
contract with a completed enforcement ladder. The escape checker's
guarantee is stated precisely in
[docs/escape-guarantee.md](docs/escape-guarantee.md).

## How to verify (the ladder)

| Step | Command | Needs | Time |
|---|---|---|---|
| Unit + property + fuzz tests | `cargo test` | Rust | minutes |
| C oracle: run the demos through gcc, check outputs + the determinism canary | `cargo test --features c-oracle` | + gcc on PATH | minutes |
| Full corpus goldens + self-hosting fixed point + seed drift guard | `cargo test --features c-oracle,selfhost-fixpoint` | + gcc | ~10 min |
| gcc-only bootstrap + fixed point | the two commands above | gcc only | ~30 s |

Current state (verified 2026-08-07, this commit): **914 default tests
pass** (0 failed, 3 ignored, ~35 s), 148-file byte-identical corpus,
warning-clean build. CI runs the first three steps on Ubuntu and
Windows.

## Where things live

| Path | What |
|---|---|
| [jestyr-design.md](jestyr-design.md) | the language design doc (draft; not everything in it is built) |
| [DESIGN-STATUS.md](DESIGN-STATUS.md) | the one-screen implemented-vs-designed matrix |
| [ROADMAP.md](ROADMAP.md) | per-workstream status |
| [docs/technical-report.md](docs/technical-report.md) | the release's companion report: the contributions, ranked, with their scopes |
| [docs/escape-guarantee.md](docs/escape-guarantee.md) | the escape checker's guarantee, stated precisely |
| [docs/TESTING.md](docs/TESTING.md) | the verification ladder with runtimes |
| [docs/](docs/README.md) | topic docs: attributes, errors, unsafe contract, CTFE tiers, obligations, … |
| [examples/](examples/README.md) | ~93 example programs, indexed by feature |
| `examples/std/` | **the self-hosted compiler's own source** (plus the stdlib) — the `.jtr` files here *are* the compiler |
| [bootstrap/](bootstrap/README.md) | the gcc-only bootstrap seed |
| `src/` | the Rust reference compiler |
| [HANDOFF.md](HANDOFF.md), [docs/handoffs/](docs/handoffs/), [docs/session-notes/](docs/session-notes/) | internal development logs, kept for provenance (numbers inside are historical) |

## Scope (what this is and isn't)

This is a single-author research project released for inspection,
reproduction, and criticism. Explicitly **not** here: a GPU backend, the
thermal/energy cost-model facet, a package registry, an LSP, or any
claim of production readiness. The C backend targets gcc/clang (MSVC's
`cl.exe` is not supported). Known gaps are documented in place rather
than hidden — start with ROADMAP.md's Status column and the scoping
notes above.

## License

Dual-licensed under [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE),
at your option.
