# Jestyr — B1 Done: Deferred Item & Next Steps

> Written 2026-06-29, after landing **B1 (field/payload auto-drop)** on `master`
> (commit `9bdd8ef`). This note records the one deferred sub-case of B1 and the
> recommended ordering for what comes next, per the self-host blockers handoff.

---

## Deferred (no blocker)

A **generic container with no own `Drop` impl but a droppable field** is the one
case the B1 recursion does not yet descend into structurally. This is **not a
blocker** for self-hosting, because std's `List(T)` (and the other generic
containers) supply their **own `Drop` impl**, which the drop walker reaches
**directly** via `drop_key_of` — so a `List` of droppable elements, or a struct
holding a `List`, is freed correctly today. The deferred work is only the narrow
shape "a *generic* struct that has no destructor of its own *yet* transitively
owns a droppable field," which std deliberately avoids by giving its containers
real `Drop` impls. The self-host path is therefore unblocked.

*(Why it's deferred: the comptime-fn constructor that defines a generic struct's
fields needs `dup_fns` canonicalization threaded through cgen's AST-walking
instance collection — a deeper change than the concrete struct/enum case, and one
with no consumer on the self-host path.)*

---

## Recommended next steps (handoff ordering)

With B1 (the single genuine correctness blocker) cleared, the remaining work is
mostly *labor and ergonomics*, not fundamental capability:

1. **B2(a) — table strategy decision (no code).** Commit to the
   **intern + id-indexed-arrays** discipline (the rustc `Symbol` pattern): intern
   every name to a dense id, keep per-table `List(V)` arrays indexed by that id.
   This is zero-risk and already supported; it avoids needing a generic-value
   `StrMap(V)` for the port. *(Fix the table strategy before starting the port.)*

2. **B3 / B4 / B5 — small, independent ergonomic fixes** that make the port
   pleasant. Each is a tidy four-layer increment:
   - **B3** — recoverable file I/O: `read_file -> String !IoError` (a fallible
     intrinsic variant) so the compiler can *report* a missing file, not crash.
   - **B4** — allow `unsafe { … }` as a `let`/`var` initializer (today you route
     through a tail-`unsafe` helper fn).
   - **B5** — fix inline `slice(u8, buf, n)` passed straight into `from_utf8(...)`
     mis-typing its temp as `int`.

3. **The port (P1 → P5) — the dominant cost (~27K lines).** Stage it the way the
   compiler is layered, each stage gated by **cross-implementation equivalence** on
   a shared corpus (Jestyr-`<pass>` output ≡ Rust-`<pass>` output):
   - **P1** finish the lexer (full token set: floats/hex/binary/octal, nested block
     comments, string/char escapes, every operator/punctuator).
   - **P2** parser → **P3** typeck/resolve → **P4** escape → **P5** cgen.

4. **R2 — the self-host fixpoint test.** Stand it up as soon as a partial stage-1
   self-build exists: stage-1 (Rust builds `jc1`) → stage-2 (`jc1` builds `jc2`) →
   stage-3 (`jc2` builds `jc3`), asserting **`jc2` ≡ `jc3` byte-for-byte**. Gate it
   behind `--features selfhost-fixpoint` (outside the toolchain-free default suite).

5. *(Optional, post-self-host)* B6 (`vec.jtr` niceties), a generic-value
   `StrMap(V)`, generic-struct collisions, and the executable `build.jestyr`.

---

## Status at a glance

| Tier | Item | State |
|---|---|---|
| 1 | **B1** field/payload auto-drop | ✅ **Done** (`9bdd8ef`) — the only true correctness blocker |
| 2 | B2(a) table discipline | Decision pending (no code) |
| 2 | B3 / B4 / B5 ergonomics | Open (small) |
| 2 | B6 `Self{}` / fallible methods | Optional (comfort) |
| 3 | P1–P5 the port (~27K lines) | Open — the dominant cost |
| 4 | R2 fixpoint acceptance test | Not yet stood up |

See repo `ROADMAP.md` (§P, §B), `DROP-ALLOC-PHASE3.md`, and the self-host blockers
handoff for full detail.
