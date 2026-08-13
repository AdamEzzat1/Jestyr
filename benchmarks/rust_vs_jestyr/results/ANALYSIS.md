# First-pass analysis — cases 1, 2, 3, 6

Measured 2026-08-13 on rustc 1.97.1 / cargo 1.97.1 (MSVC target), Jestyr
`claude/rust-jestyr-ownership-benchmark-1bd785@f675669` (= master `f675669`)
via gcc 8.3.0 `-O2`. Numbers in `latest.md`/`latest.json`; this file is the
part a table cannot hold. Every claim below is scoped to these four
workloads — see METHODOLOGY rule 7.

## Headline answers to the research question

Patterns where Jestyr expressed the safe program with **less** machinery
than Rust:

- **Observer/stale-handles (case 6)** is the clear one. Rust std needs a
  45-line hand-rolled generational arena; idiomatic Rust outsources it to
  `slotmap`. Jestyr needs **no registry at all** — the generation check
  lives in the pointer type (`&Obj`), and `with alive h as read o { }
  else { }` is the entire stale story. Cost: 1.3–1.4× runtime (70.7 ms vs
  56.1/50.0) for per-access generation checks against slotmap's
  slab layout. Fewer concepts, fewer lines, slower dereference.
- **Transient borrowing (case 1)** is annotation-parity with a sigil
  asymmetry: Rust spells the borrow at both the signature AND every call
  site (`advance(&mut w)`, `total_score(&w)`, `for p in &mut w.players`);
  Jestyr spells the mode only in the signature (`advance(w)` at the call
  site). Zero lifetimes on either side. Runtime parity (452.5 vs 468.8 ms
  — single-digit %, i.e. noise).

Patterns where **Rust** has the stronger story:

- **Borrowed projection (case 2)**: Rust returns `&Token` natively with
  elided lifetimes. Jestyr cannot return a borrow into a parameter — its
  `-> read T` annotation names no source, and the `-> read T from xs`
  design (safety-mosaic item 2) is unimplemented. The idiomatic fallback
  (mark the 24-byte `Token` `@copy`, return by value) measured **free**
  at this scale (99.4 vs 96.2 ms), but it is a real expressiveness gap:
  it only works because the element is small and copyable. A projection
  into a large or non-copyable element has no Jestyr spelling today
  beyond returning an index.
- **Disjoint mutation (case 3)**: Rust's `split_at_mut` returns the two
  halves as values; Jestyr's `std/parallel.split_mut` must invert control
  (continuation-passing) because returning a pair of borrows is escape
  route 2 by design. Same guarantee, clumsier shape: the round body
  becomes a named top-level function. Jestyr also ran 17% slower here
  (559 vs 479.4 ms) — see the performance note below.

## Annotation itemization (hand-audited, per METHODOLOGY)

Units differ per language by design; itemized so readers can re-weigh.

| case | Rust ceremony | Jestyr ceremony |
|---|---|---|
| transient_borrow | 0 lifetimes; 4 signature borrows (`&World`×2, `&mut World`, `&mut Player`); 5 call-site/loop sigils (`&mut w`, `&w`×2, `&w.players`, `&mut w.players`) | 4 signature modes (`read`×2, `mut`×2); 1 loop mode (`for mut p`); 0 call-site sigils; **3 `slice()` view rebuilds** (structural: a struct may not hold a slice, so `World` carries ptr+len) |
| borrowed_projection | 0 lifetimes (all elided); 7 signature `&` (3 param, 3 return, 1 field-fn); 3 call-site `&toks` | 1 load-bearing `@copy`; 4 `read` modes; 0 call-site sigils; every projection is a 24-byte copy instead of a pointer |
| disjoint_mutation | 4 signature borrows (`&mut [i64]`×3, `&[i64]`); 1 `split_at_mut` + tuple destructure per round; 1 `&xs` call site | 4 signature modes + 2 callback modes; 1 import; **1 CPS inversion** (named `round_phases` + `&round_phases` fn-pointer) |
| observer_registry (std) | `#[derive(Clone, Copy)]` on Handle; ~45-line Registry type (2 structs + 3 methods + free list); `Option`/`match` at every access | 1 `@copy` on Handle; 3 `with alive … else` sites; **no registry type at all** (`gen_new`/`gen_free` are the language) |
| observer_registry (slotmap) | 1 crate dependency; registry vanishes into `SlotMap` | — |

`unsafe` count: **0 in all eight program sources** on every track. All
three languages' internal unsafe lives in libraries (Rust std's
`split_at_mut`, slotmap's slots, Jestyr's `std/parallel` slice surgery) —
the boundary sits below the program on every side, which is itself a
result: none of these four patterns forced unsafe to the surface anywhere.

## Rejection twins (diagnostics verbatim)

Rust (`rejected.rs`, one per case dir):

- overlap: `error[E0502]: cannot borrow 'w' as immutable because it is
  also borrowed as mutable`
- dangling projection: `error[E0597]: 'toks' does not live long enough`
- double-mut: `error[E0499]: cannot borrow 'xs' as mutable more than once
  at a time`

Jestyr (`*_rejected.jtr`):

- borrow escaping by return: `error: cannot return borrow 'p': a
  second-class 'read'/'mut'/'out' borrow may not outlive its call (pass
  it further down, or declare the return as 'read'/'mut'/'out')`
- non-copy element leaving a read slice: same rule, at `return xs[i]`
- aliased writable slices: `error: cannot pass 'q' to two writable slice
  parameters of 'g' in one call: the two views would alias every element
  — divide the buffer with 'split_mut' instead`

Three asymmetries worth recording rather than averaging away:

1. Rust's E0499 fires at the *borrow site*; Jestyr's exclusivity rule
   fires at the *call site* and is lexical (an aliased root dodges it —
   documented in the checker's own notes). Rust's guarantee is deeper.
2. Rust rejects the *read-while-mut overlap* (E0502); Jestyr deliberately
   **allows** `mut`+`read` of one place at one call (in-place idioms).
   Same program, opposite verdicts, both by design — this is the sharpest
   philosophical divergence the first pass found.
3. Jestyr's stale-handle story has a class Rust std cannot express at
   all: a *bare* stale dereference is a deterministic runtime fault
   (generation assert), never UB and never a silent `None` — with
   `with alive … else` as the non-faulting query form. Rust encodes
   staleness in `Option` and relies on the caller not to `unwrap`.

## Performance notes (implementation, not language)

- **Parity where traversal is simple** (cases 1, 2): Jestyr's emitted C
  under gcc `-O2` matches rustc/LLVM within noise, *with live bounds
  checks* on the Jestyr side.
- **The 17% loss in case 3** is consistent with the known bounds-check
  elision boundary: `bump`/`scale` loop over `0..half.len` (provable,
  elided), but `add_into` loops to `min(dst.len, src.len)` — a derived
  bound the prover does not connect back to either slice, so ~200M
  checked accesses stay live across the 25 rounds. Confirming via
  `emit-c` diff is a recorded next step, not assumed.
- **The 1.3–1.4× loss in case 6** buys the checked-pointer model:
  every sweep access pays a generation compare against a header word,
  and every spawn is an individual `gen_new` heap allocation, vs
  slotmap's contiguous slab. Nobody should quote this as "genrefs are
  slow" — it is the cost of *not writing* the arena.
- **Compile time**: Jestyr (parse→check→emit-C→gcc) beat cold per-crate
  cargo builds on every case (342–687 ms vs 499–1033 ms). Different
  pipelines doing different work; recorded, not ranked.
- **Binary size**: Jestyr's gcc/MinGW exes are ~55–80 KB vs Rust/MSVC's
  ~128 KB. Toolchain-confounded (see METHODOLOGY); recorded raw.

## Surprises

1. The borrowed-projection gap **cost nothing at runtime** here — a
   24-byte copy per lookup is invisible next to the cache miss that
   fetches the token. The gap is real but its price, at this element
   size, is zero.
2. Hand-rolled Rust arena vs slotmap: 56.1 vs 50.0 ms — the crate is
   slightly faster than the obvious hand-rolled version, and 46 lines
   shorter. The std-only track's real cost is the code, not the speed.
3. Zero lifetime annotations appeared on the Rust side across all four
   cases — elision covered everything. The ceremony difference between
   the languages in these patterns is call-site sigils vs signature
   modes, not lifetimes vs modes. Harder cases (4, 5, 7) are where named
   lifetimes would start appearing in Rust.

## Next steps

- Cases 4 (arena AST), 5 (doubly linked list — `dlist_genref.jtr` is a
  ready-made Jestyr side), 7 (resource capabilities, part design-only),
  8 (structured concurrency — reuse `heavy_parsum` discipline), 9
  (unsafe boundary).
- `emit-c` diff to confirm the case-3 bounds-check attribution.
- Peak compiler memory measurement (deferred from first pass).
- A stale-deref *fault* demo (bare `.*` after `gen_free`) as a recorded
  runtime-behavior probe alongside the compile-time rejections.
