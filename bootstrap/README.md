# Bootstrapping Jestyr without Rust

This directory holds the **bootstrap seed** — the Jestyr compiler's own C output for
its own (flattened, single-file) source. With it, building Jestyr from scratch needs
only a C compiler; the Rust bootstrap compiler is never required.

| file | what it is |
| --- | --- |
| `jestyr_flat.jtr` | the whole self-hosted compiler (lexer→parser→typeck→escape→cgen + driver) flattened into one file, generated from `examples/std/*.jtr` |
| `jestyr_seed.c` | the compiler's own C for `jestyr_flat.jtr` — emitted by the compiler itself (the committed fixed point) |

## Build

```bash
gcc -O2 -std=c11 -ffp-contract=off -fno-fast-math -o jc bootstrap/jestyr_seed.c
```

On Windows (MinGW), add stack headroom for the compiler's per-expression recursion:

```bash
gcc -O2 -std=c11 -ffp-contract=off -fno-fast-math -Wl,--stack,67108864 -o jc.exe bootstrap/jestyr_seed.c
```

`jc` is a working Jestyr compiler:

- `jc file.jtr` — emit C on stdout
- `jc file.jtr build` / `jc file.jtr run` — compile via gcc (escape-checked, diagnostics on stderr)
- `jc file.jtr test [substr]` / `jc file.jtr list [substr]` — the `@test`/`@bench` harness
- `jc file.jtr doc` — the Markdown API page: signatures, `///` prose, and a **Guarantees**
  block reconstructed from the contracts the compiler proved
- `jc file.jtr attest` — the attestation manifest (the C's SHA-256, the locked compile
  command, and every item's signature + machine-checked guarantees)
- `jc old.manifest attest-diff new.manifest` — classify each API change breaking or
  compatible; `jc file.jtr attest-verify old.manifest` does the same against a fresh
  render. Either exits non-zero when something breaks, so both drop into CI as a gate.

The driver adapts to its host: on POSIX it probes for a C compiler in the
reference driver's order (`cc`, `gcc`, `clang`), names the output without a
suffix, and anchors a separator-free path with `./` before running it; on
Windows it uses `gcc` (MinGW), names the output `<stem>.exe`, and anchors with
`.\`. On Windows, pass the file with backslashes (`jc examples\hello.jtr run`) —
`run` hands the produced exe path to `cmd.exe`, which rejects forward slashes
in command position.

## Verify the fixed point

The seed is self-reproducing — the compiler you just built must emit the seed back,
byte-for-byte:

```bash
./jc bootstrap/jestyr_flat.jtr > regen.c && diff regen.c bootstrap/jestyr_seed.c
```

(On Windows the redirect is CRLF; compare with a newline-normalizing diff.)

## Keeping the seed current

The artifacts are pinned by the `bootstrap_seed_is_current` test
(`cargo test --features selfhost-fixpoint`): it regenerates both from the live
`examples/std` sources and fails if the committed copies drifted. After changing the
compiler, refresh with:

```bash
REFRESH_SEED=1 cargo test --features selfhost-fixpoint bootstrap_seed_is_current
```
