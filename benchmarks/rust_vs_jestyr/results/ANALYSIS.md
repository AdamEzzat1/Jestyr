# Analysis — all nine cases

Measured 2026-08-13 on rustc 1.97.1 / cargo 1.97.1 (MSVC target), Jestyr
branch `claude/rust-jestyr-ownership-benchmark-1bd785` via gcc 8.3.0
`-O2`. Tables in `latest.md`/`latest.json` and `compiler_memory.md`;
this file is the part a table cannot hold. Every claim is scoped to
these workloads — METHODOLOGY rule 7. All 22 track-runs across 9 cases
print byte-identical output; every timing row below is gated on that.

## The research question, answered per pattern

**Where Jestyr expressed the safe program with less machinery:**

- **Observer/stale-handles (6)** — no registry at all: the generation
  check is the pointer type, `with alive … else` is the whole stale
  story. Rust std hand-rolls a 45-line arena; idiomatic Rust imports
  `slotmap`. Cost: 1.3–1.4× runtime (70.3 vs 56.0/49.4 ms).
- **Doubly linked list (5)** — link surgery is ordinary field writes;
  enum `@copy` (the niche Link) retired the take-ceremony the older
  `dlist_genref.jtr` needed. The std twin is only "safe" because it
  escaped into unchecked indices — a stale u32 silently reads whatever
  occupies the slot; Jestyr's stale handle FAULTS deterministically,
  slotmap's key misses. Cost of the checked pointer: 3.2× (51.8 vs
  16.2 ms).
- **Structured concurrency (8)** — one `par for … reduce` line against
  ~20 lines of scope/chunk/join (std). rayon matches the brevity but
  not the guarantee: both Rust twins are deterministic only because i64
  `+` happens to be associative; the f64 version would silently go
  schedule-dependent, where Jestyr refuses it at compile time, and
  `@span(log)` makes accidental serialization a compile error. Runtime
  95–106 ms across all three — parity.
- **Unsafe boundary (9)** — parity of mechanism (2 fenced blocks each,
  both compiler-required), with one Jestyr-specific edge: the bounds
  precondition is a `requires` contract on the signature, not an
  `assert!` in the body. ~8% runtime cost (110.5 vs 102.6 ms).
- **Transient borrowing (1)** — annotation parity, zero call-site
  sigils vs Rust's `&`/`&mut` at every use. Runtime parity (the leader
  swapped between sessions inside the noise band: 452–494 ms both
  sides).

**Where Rust has the stronger story:**

- **Borrowed projection (2)** — Rust returns `&Token` natively; Jestyr
  cannot return a borrow into a parameter (`-> read T from xs` is
  design-only, mosaic item 2). The `@copy` value-return fallback
  measured free at 24-byte elements (95.8 vs 92.2 ms) but does not
  scale to large or non-copyable elements.
- **Resource capabilities (7)** — runtime parity (20.9 vs 24.0 ms) and
  matching shapes (`take` ≡ by-value `mut d`), but Rust's affine story
  is complete and Jestyr's is not. Two gaps MEASURED by this suite's
  probes:
  1. use-after-take compiles (`resource_capabilities_gap.jtr` is the
     pinned witness) — the giver's binding is not poisoned; Rust
     refuses the same program with E0382;
  2. a take parameter the callee does not return is never dropped —
     with a Drop impl, move-in-and-discard runs zero drops anywhere
     (caller correctly elides as moved-out; callee never inserts one).
     A moved-in resource leaks.
  Both are exactly the data mosaic item 7 (linear capabilities) needs;
  a fix task has been spun off separately.
- **Arena AST (4)** — the biggest performance gap in the suite: 5.4×
  (210.1 vs 39.2/38.6 ms). Jestyr's only way to carry cross-links today
  is genrefs — one `gen_new` heap allocation per node (1,048,575 of
  them) plus a generation check per hop, against one `Vec` (std) or
  arena chunks (typed-arena). Region allocation cannot carry the case:
  region refs cannot live in struct fields. Expressiveness-wise Jestyr
  is *simpler* here (no `'a`, no `Cell`) — the cost is all in layout
  and allocation, which is mosaic item 6's exact design target.

## The graph tax, named precisely

Cases 4 and 5 isolate what the checked-pointer model costs when the
data structure IS pointers: 3.2–5.4×, from (a) per-node individual heap
allocation with a generation header vs contiguous/arena placement, and
(b) a checked deref per hop vs a bare load. Case 6 shows the same model
at 1.3–1.4× when access is sweep-shaped rather than chase-shaped. The
flat-buffer cases (1, 2, 3, 8, 9) show ~0–17%. Nobody should quote
"genrefs are 5× slower" without the second half: the std-Rust twin that
wins case 4 by 5.4× verifies NOTHING about its indices, and the twins
that do verify (typed-arena via lifetimes, slotmap via generations)
each import a crate and their own ceremony.

## Case 3's 17%, confirmed in the emitted C

The elision hypothesis from the first pass is now confirmed, not
assumed: in `emit-c` output, `bump` and `scale` (loops bounded by
`0..half.len`) contain ZERO asserts, while `add_into` — whose loop
bound is `min(dst.len, src.len)`, a derived value the prover does not
connect to either slice — carries THREE live asserts per iteration
(read dst, read src, write dst), ~300M across the run. Also visible in
the same emission: `mut []i64` params lower to `JestyrSlice_i64*
restrict` — Jestyr's exclusivity handed to gcc as an aliasing
guarantee, the same promise rustc makes LLVM via noalias.

## Annotation itemization (hand-audited)

First pass:

| case | Rust ceremony | Jestyr ceremony |
|---|---|---|
| transient_borrow | 0 lifetimes; 4 signature borrows; 5 call-site/loop sigils | 4 signature modes; 1 loop mode; 0 call-site sigils; 3 `slice()` view rebuilds (a struct may not hold a slice, so World carries ptr+len) |
| borrowed_projection | 0 lifetimes (elided); 7 signature `&`; 3 call-site `&toks` | 1 load-bearing `@copy`; 4 `read` modes; every projection a 24-byte copy |
| disjoint_mutation | 4 signature borrows; `split_at_mut` + tuple destructure; 1 `&xs` | 4 + 2 callback modes; 1 import; CPS inversion (named `round_phases` + `&round_phases`) |
| observer_registry (std) | `#[derive(Clone, Copy)]`; ~45-line Registry; `Option`/`match` per access | 1 `@copy`; 3 `with alive … else` sites; no registry type at all |
| observer_registry (slotmap) | 1 crate dep; registry vanishes | — |

Second pass:

| case | Rust ceremony | Jestyr ceremony |
|---|---|---|
| arena_ast (std) | 0 lifetimes, 0 borrows in the graph — indices carry no proof | 1 `@copy` enum; `read` modes; genref `.*` derefs |
| arena_ast (typed-arena) | **7 `'a` occurrences** (the suite's first named lifetimes), 3 `Cell<…>` wrappers, `.get()`/`.set()`/`.unwrap()` at every back-link | back-links are plain field writes (`a.*.parent = at(p)`) — no interior-mutability concept exists to need |
| dlist (std) | NIL sentinel discipline, free-list omitted (documented) | 1 `@copy` enum; `gen_free` at delete; per-hop `match` on Link (the nullable-link ceremony `dlist_genref.jtr` recorded) |
| dlist (slotmap) | `Option<DefaultKey>` links, crate dep | — |
| resource_capabilities | 2 `mut` value params, 1 `&` audit | 3 `take` modes, 1 `read` — one-for-one with Rust |
| structured_concurrency (std) | scope closure, `move`, manual chunk math, join loop | `@span(log)`+`@span(linear)`, 1 import, 1 `par for` line |
| structured_concurrency (rayon) | crate dep + prelude import | — |
| unsafe_boundary | 2 `unsafe` blocks, 2 body `assert!`s, SAFETY comment | 2 `unsafe` blocks, 2 signature `requires` contracts, SAFETY comment |

`unsafe` in program sources across all nine cases: **2 blocks per side
in case 9 (the case about unsafe), zero everywhere else** — on every
track. Library-internal unsafe (std's `split_at_mut`, slotmap, rayon,
Jestyr's `std/parallel`) noted, not counted, per METHODOLOGY.

## Rejection probes (verbatim diagnostics)

Nine probes, all verified refused:

- Rust: E0502 (read-while-mut), E0597 (dangling projection), E0499
  (double `&mut`), E0382 (use after move), E0133 (raw deref outside
  unsafe).
- Jestyr: borrow-return refusal ("a second-class `read`/`mut`/`out`
  borrow may not outlive its call"), same rule for a non-copy element
  leaving a read slice, mut-slice exclusivity ("cannot pass `q` to two
  writable slice parameters of `g` in one call … divide the buffer with
  `split_mut` instead"), unsafe ladder ("a raw-pointer deref belongs in
  an `unsafe` block").

Plus one KNOWN-GAP file that compiles on purpose
(`resource_capabilities_gap.jtr` — Jestyr's missing E0382, see case 7).

Standing asymmetries: Rust's aliasing refusals fire at the borrow site
and are deep; Jestyr's exclusivity rule is lexical and call-site (an
aliased root dodges it). Jestyr deliberately ALLOWS the read+mut
overlap Rust's E0502 refuses. Jestyr's stale-handle story has a
behavior class Rust cannot express in std: deterministic fault on bare
deref, checked else-arm via `with alive`.

## Toolchain numbers (contextual, not rankings)

- **Compile time** (median of 3, cold package): Jestyr 261–1447 ms vs
  Rust 506–1349 ms. Jestyr's slowest case (structured_concurrency,
  1447 ms) is the one that imports std modules and threads gcc through
  pthread lowering.
- **Peak compiler memory** (`compiler_memory.md`): real rustc (not the
  rustup shim — the first measurement caught the shim's flat 11 MB and
  was redone) 50–66 MB per case; jestyrc 2.6–8 MB WITHOUT gcc, which
  it forks and which is not measured. Different pipelines; footnotes
  are load-bearing.
- **Binary size**: Jestyr/MinGW 61–101 KB vs Rust/MSVC ~127–131 KB on
  flat cases; the threaded pair inverts (Jestyr 226 KB vs std 160 KB,
  rayon 213 KB). Toolchain-confounded; raw.

## Surprises (second pass)

1. **The suite's first named lifetimes appeared only in case 4's
   typed-arena twin** — and immediately brought `Cell` with them.
   Everywhere else, elision + modes kept both languages annotation-flat.
2. **resource_capabilities ran slightly FASTER in Jestyr** (20.9 vs
   24.0 ms) — a `take`-and-return pipeline through gcc optimizes to
   nothing just as well as Rust's moves through LLVM.
3. **The three-way concurrency tie** (94.7 / 100.5 / 106.4 ms): the
   fused `par for` worker, hand-chunked scoped threads, and rayon's
   work-stealing all hit memory bandwidth on this workload. The
   differentiator is the compile-time determinism check, which only one
   language has — and it costs nothing at runtime here.
4. **Use-after-take compiling was not known before this suite** — a
   benchmark probe, not a fuzzer or an audit, surfaced it (plus the
   never-dropped take param). Twin-writing is an effective adversarial
   review of the ownership model.

## Remaining follow-ups

- The spun-off compiler task: take-param drops + use-after-take
  poisoning (feeds mosaic item 7).
- Mosaic item 6 (region-scoped cells) now has its target numbers: beat
  210 ms/5.4× on arena_ast and 52 ms/3.2× on dlist while keeping the
  checked-or-faulting guarantee.
- A stale-deref fault demo (bare `.*` after `gen_free`) as a recorded
  runtime-behavior probe.
- gcc-side memory for the Jestyr pipeline if a fair job-object-based
  measurement is ever wanted.
