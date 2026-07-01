# Jestyr self-host — Tier-2 ergonomic unblockers B4/B5/B3 DONE (session summary)

**Status: the three Tier-2 self-host unblockers from `jestyr-selfhost-blockers-handoff.md`
(§3) are landed on `master`, green, warning-clean.** With B1 (field/payload auto-drop,
prior session) these clear every non-port self-host blocker: **only the port (P1–P5) and
the fixpoint test (R2) remain.**

## Commits (on master, in order)

- `12cf48d` self-host(P/B4): `unsafe`/block as a value (let/var initializer)
- `23ad174` self-host(P/B5): type inline `slice(T, …)` as `[]T` in argument position
- `11a75a9` self-host(P/B3): recoverable `try_read_file -> String !IoError`

Suite grew 655 → 676 tests; all green, clippy unchanged (15 baseline warnings, none new).
gcc-oracle demos added for each (behind `--features c-oracle`).

## B4 — `unsafe`/block as a `let`/`var` initializer

**The handoff mis-located this.** It predicted parser+typeck; empirically both already
accept `let v = unsafe { … }`. The real gate was **cgen** (`ExprKind::Unsafe|Block` in
value position → "only supported in statement or return position"). Lesson: locate a
blocker by *running it* and reading the actual error.

`unsafe` is a compile-time permission marker with **zero** runtime effect, so a
value-position `unsafe { E }` is exactly `E`. The cgen arm now yields the block's single
tail expression (`if let [Stmt::Expr(e)] = b.stmts …`). `If` keeps its statement/return
diagnostic; a multi-statement value block stays a clear error (the GNU statement-expression
form with drop-safe spilling is future work). Additive: previously such programs were an
error, so nothing valid changed.

- Files: `cgen.rs` (the value-position arm); `examples/std/unsafe_init.jtr`; caveats
  flipped in `std/intern.jtr` + ROADMAP §P.
- Tests: wiring (deref lowering + metamorphic `unsafe { E }`/`{ E }` ≡ `E` + negative),
  property `unsafe_init_props` (transparency + determinism), fuzz `fuzz_unsafe_blocks`,
  oracle `unsafe_init_demo` (→ 42), teeth (wrong value fails 3 wiring tests).

## B5 — inline `slice(T, buf, n)` types as `[]T` in argument position

`slice`/`alloc` are **cgen-only intrinsics with no typeck return type** — they relied on a
`let` annotation. Unannotated (e.g. `from_utf8(slice(u8, buf, n))`) the call fell through
to `Ty::Unknown`, which cgen lowers to `int` → the temp `int _u = (JestyrSlice_u8){…}`
failed to compile. Fix: a typeck arm types `slice(T, buf, n)` as
`Ty::Slice(eval_type_expr(first arg))`, reading the element type from the first (type)
argument. Additive: annotated `slice(…)` usages drive their type from the annotation, so
all existing std code (binned/core/par_*/reductions — 141 slice-heavy oracle demos) emits
byte-identical C.

- Files: `typeck.rs` (the `slice` call arm); `examples/std/slice_utf8.jtr`.
- Tests: wiring (`JestyrSlice_u8 _u`, never `int _u`; inline ≡ annotated-workaround),
  property `slice_typing_props` (typed temp + determinism over buffer size), fuzz
  `fuzz_slice_arg_typing`, oracle `slice_utf8_demo` (→ "Hi", 2), teeth (arm → `Ty::Unknown`
  fails wiring + property).

## B3 — recoverable `try_read_file -> String !IoError`

Lets a compiler *report* a missing/unreadable source file instead of the silent empty
String from `read_file`. Full four-point intrinsic seam:
- typeck `io_intrinsic_ret`: `try_read_file -> Ty::Result(String)`.
- cgen `is_intrinsic` + emit-call: lowered inline (like `try_from_utf8`) to a
  statement-expression; runtime `jestyr_rt_try_read_file(path, JestyrString* out) -> bool`
  reports failure via its return and writes the file into the out-param, wrapped into the
  tagged `JestyrResult_String` (`.err = 1` = `IoError`). The out-param means the runtime
  helper needs **no** forward reference to `JestyrResult_String` (which lives in
  `result_defs`), so it sits in the prelude with no ordering headache.
- std wrapper `fs.try_read_text` — forwards with a **plain** `return try_read_file(path)`.

**Byte-identical by gating.** Both the runtime helper and the `JestyrResult_String` typedef
are emitted only when `Cgen::uses_try_read` (an AST scan for the call) is set — so programs
that don't use it are unchanged (files/intern read_file demos green).

**Two gotchas worth remembering:**
1. `let r: String !IoError = …` is **not** valid — `!{E}` is fn-return-only syntax. Use an
   unannotated `let r = try_read_file(p)`; inference gives the Result.
2. `String` exposes `.len` directly; `unwrap(r)` gives a `String`, then `s.len`.
3. The fallible wrapper forwards with a **plain** `return f(path)` (same `!E` result type);
   `return f(path)?` does **not** re-wrap (a fallible-fn-return interaction) and fails to
   compile — plain forward passes the result through.

- Files: `cgen.rs` (`uses_try_read` field+scan, prelude helper, result typedef, emit-call
  arm, is_intrinsic), `typeck.rs` (`io_intrinsic_ret`), `std/fs.jtr` (`try_read_text`),
  `examples/std/try_read.jtr`, ROADMAP §P.
- Tests: wiring (result typedef + helper + IoError tag; the additive gate), property
  `try_read_props` (lowers+deterministic over paths; unrelated programs have no try_read),
  fuzz `fuzz_fs_try`, oracle `try_read_demo` (["true","13","true"]), teeth (force ok-branch
  → missing-file check flips to "false", oracle fails).

## Discipline followed (every increment)

`cargo test`-green + warning-clean; four test layers + teeth-by-mutation; toolchain-free
default suite (gcc-oracle behind `--features c-oracle`); byte-identical for programs not
using the feature (proven by 141+ unchanged oracle demos + the gating tests); one green
commit per increment to `master` with the `Co-Authored-By` trailer; demo `.jtr` per
feature.

## What's next for self-host (unchanged, the gate)

Per the blockers handoff §4–§6, the remaining work is the **port itself**, staged by
compiler layer, each gated by cross-implementation equivalence on a shared corpus:
1. **P1 — finish the lexer** (`examples/std/lexer.jtr` is a subset): full token set —
   float/hex/bin/oct literals, nested block comments, string/char escapes, every
   operator/punctuator in `token.rs`. Golden: Jestyr lexer's token stream ≡ Rust lexer's.
2. **P2 parser → P3 typeck → P4 escape → P5 cgen** — port each pass; wiring test =
   cross-impl equivalence (AST dump, then resolved-type dump, then emitted C) on the corpus.
3. **R2 fixpoint** (`--features selfhost-fixpoint`): stage-1 Rust builds `jc1`; `jc1` builds
   `jc2`; `jc2` builds `jc3`; assert `jc2 ≡ jc3` byte-for-byte. Stand it up as soon as a
   partial stage-1 self-build exists. Module content-hash + `attest` are the cache key.
4. **R1** — grow a "large input" corpus early (a `huge_program_compiles` test) to surface
   scaling bugs the 27K-line input will hit; profile cgen if compile time bites.

Optional post-self-host niceties (unchanged): B6 (`vec.jtr` `Self{}`/fallible methods),
`StrMap(V)` generic map, generic-struct collisions, executable `build.jestyr`.

**Also decided (B2):** adopt the intern + id-indexed-arrays discipline (rustc `Symbol`
pattern) as the self-host table strategy — zero new language feature; `StrMap(V)` stays a
post-self-host nicety.
