# Drop/RAII + the Explicit Allocator Interface — Phase 3

A handoff for the **deterministic Drop/RAII** and **explicit allocator** workstream
(design `jestyr-design.md` Phase 3; the gaps named in `NUMERICS-RESEARCH.md` §2.1).
Read with [`HANDOFF.md`](HANDOFF.md) (the compiler) and [`docs/TESTING.md`](docs/TESTING.md)
§5.13 (the test layer for this work).

The thesis: *implicit ≠ hidden*. Destructors run automatically, but the ownership
model lets us decide liveness **statically** — no runtime drop flags, no unwinding,
no hidden per-value bool — and the inserted glue is **inspectable** (`--show-drops`).
The allocator half ("allocators are explicit values, not a hidden global") already
has an enum-dispatch stand-in in `std/`; retiring it for a fn-pointer vtable is the
main remaining piece.

---

## Achieved

Seven green, warning-clean, auto-committed increments. **438 tests pass** under the
default toolchain-free `cargo test` (up from 413 at the start: +25 unit/property/fuzz).

### Drop / RAII

- **Scope-exit drop glue, drop-flag-free (Increment 1).** A local whose type has an
  `impl Drop for T` is dropped automatically at scope exit, in **reverse declaration
  order**. The implementation (`src/cgen.rs`):
  - a **drop-scope stack**, one entry per emitted `{ }` block; a local registers for
    drop glue *as its `let` is emitted*, so an early `return` can never reference a
    not-yet-declared local. Liveness is therefore static and per-branch — the Jestyr
    win over Rust's runtime drop flags.
  - a **return** spills its value to a temp, runs the live drops (innermost scope
    first, reverse within a scope), then returns — so the returned value can't read a
    local we just dropped. This reuses the existing `ensures`/`j_result` spill seam,
    which is why every `if`/`match`/`block` return-tail is covered for free.
  - **No unwinding → straight-line drop:** Jestyr aborts on fault, so there are no
    landing pads or double-panic cases; the glue is plain sequential C.
- **Move analysis / drop-after-move elision.** `cgen::collect_moved` computes (over-
  approximately, hence leak-safe) the locals whose value *escapes* — returned, passed
  by value to a call, captured into a struct, rebound, or consumed by a `take self`
  receiver. Those get **no** drop glue (the new owner drops them), so a value is
  dropped **at most once** — no double-free by construction.
- **`--show-drops` inspection.** `jestyrc emit-c <file> --show-drops` annotates each
  inserted drop with a `/* drop j_x : T */` comment. Implicit control flow is made
  visible — the "transparent cost" thesis.
- **Manual `drop()` is rejected.** A hand-written `value.drop()` (resolving through
  `impl Drop`'s `drop`) is a compile error — the auto-drop still fires, so calling it
  manually would double-free. The escape checker reports it with a caret.
- **Region-integrated bulk drop (Increment 4, partial).** A value owned by a
  `region { }` block emits **zero** per-value drop glue — the arena reclaims it in
  bulk at region end. The allocator/region *determines* the drop strategy. Verified
  metamorphically: the same droppable emits one drop in a plain block, zero in a region.
- Demo: [`examples/drop.jtr`](examples/drop.jtr) — `jestyrc run` prints
  `100, 2, 1, 200, 300, 7` (work, then reverse-order drop, then a moved-out value
  dropped exactly once by its new owner).

### Allocator interface

- **`@no_alloc` — the enforced allocation-free contract (Increment 3).** The
  `@no_panic` analog for memory: a `@no_alloc` function must be *proven* allocation-
  free by the escape checker, or it is a **compile error** (`src/escape.rs`, a
  per-function flag saved/restored around nested method bodies). It rejects:
  - a call to any allocation intrinsic — `alloc`/`realloc`/`arena_open`/`arena_alloc`/
    `region_*`/`gen_new` — bare *or* module-qualified (`mem.allocate` resolves to its
    bare name);
  - a `region { }` block (opens an arena);
  - a region-scoped `for` loop (per-iteration scratch arena).

  The diagnostic fires at `jestyrc check` time, before any C is emitted. Gold for
  real-time / embedded / kernel paths. `@deterministic` is **registered (reserved)**
  in `attrs.rs` for the allocator-determinism contract.
- **The fn-pointer-vtable `Allocator` value + `Layout` (Increment 5).** Zig's
  `std.mem.Allocator` shape as a real first-class value, retiring the enum-dispatch
  stand-in: an `Allocator` is an opaque `ctx: *mut u8` plus a vtable of thin function
  pointers (`alloc_fn`/`free_fn`), with `Layout { size, align }` carrying the request.
  **One** user-facing path (`alloc_n`/`free_n`) runs over *any* allocator —
  `a.alloc_fn(a.ctx, layout)` lowers to a genuine **indirect call**, not a bare
  `malloc`. [`examples/alloc_vtable.jtr`](examples/alloc_vtable.jtr) drives the same
  path with two strategies (system/malloc and a bump arena) → `10, 20, 30, 40`.
  This reuses the existing fn-pointer-type machinery; no parallel dispatch was invented.
- **Allocator-parameterized `Vec`, freed by RAII (Increment 6) — the integration
  forcing function.** `IntVec` *stores* its `Allocator` value (Rust's `Vec<T, A>`),
  grows its buffer through that allocator's vtable, and its `Drop` impl frees the
  buffer **at scope exit, through the very allocator it was allocated from** — no
  manual free. [`examples/vec_alloc.jtr`](examples/vec_alloc.jtr) →
  `5, 10, 50, 99` then `v` drops. This exercises every Phase-3 seam at once: the
  vtable allocator, scope-exit drop glue, the **take-vs-borrow** move analysis (the
  vector is mutated via `mut`-borrow `push` calls yet still drops exactly once), and
  `@copy` on the cheap allocator handle so a collection may store it without escape.
- **Take-vs-borrow move precision (Increment 5).** `cgen::collect_moved` treats a call
  argument as a move **only when its parameter is `take`** (resolving the callee's
  `FnDecl`); a `read`/`mut`/`out` borrow does not move, so a droppable handed to a
  borrowing method still drops at scope exit. Generic callees stay conservative (their
  comptime type-args occupy argument slots) — leak-safe.
- **Static leak prevention is already real.** The escape checker's ownership/region
  proof statically kills a whole class of leaks/escapes; `@no_alloc` adds a checkable
  "this path allocates nothing" property on top. (The residual dynamic catch — a
  debug allocator for FFI/unsafe escapes — is future work; see below.)

### Headline guarantees realized

| Guarantee | Status |
|---|---|
| Drop-flag-free, static per-branch liveness | ✅ (registration-as-emitted) |
| No-unwinding, straight-line drop | ✅ (abort-on-fault model) |
| Reverse-declaration drop order | ✅ + golden |
| Drop exactly once (no double-free) | ✅ + property + teeth |
| Drop-after-move elision | ✅ + property |
| Region-integrated bulk drop | ✅ + metamorphic property |
| Implicit-but-inspectable (`--show-drops`) | ✅ |
| Can't call `drop` manually | ✅ |
| `@no_alloc` enforced contract | ✅ + sound/complete property + teeth |
| Explicit fn-pointer-vtable `Allocator` + `Layout` | ✅ (retires the enum stand-in) |
| One alloc path over many strategies (system/arena) | ✅ + golden |
| Take-vs-borrow move precision | ✅ + property + teeth |
| Allocator-parameterized `Vec`, freed by RAII | ✅ (concrete `IntVec`; generic `Vec(T,A)` future) |

### Tests (this workstream)

- **Goldens** (`src/cgen.rs::tests`): drop call emitted; reverse order; move elision;
  `--show-drops` comment; no glue without a `Drop` impl; region drop-elision; the
  allocator routes through the vtable (not bare malloc); a `mut`-borrowed droppable
  still drops once; a `take`-consumed one is not dropped by the caller.
- **Escape unit** (`src/escape.rs::tests`): `@no_alloc` rejects heap alloc / region,
  accepts a clean body, is per-function; manual `drop()` rejected.
- **Properties** (`src/proptests.rs`): `drop_props` (drops-each-owned-exactly-once,
  move elision, no-double-free, determinism, region bulk-drop metamorphic,
  borrow-passed-droppable-still-drops-once); `alloc_props` (rejected *iff* allocates —
  soundness + completeness vs an independent oracle).
- **Fuzz** (`bolero`): `fuzz_drop_alloc_pipeline`, `fuzz_drop_alloc_determinism`.
- **Teeth-checked**: suppressing the drop emitter fails the drop count properties +
  goldens; neutering `is_alloc_intrinsic` fails the `@no_alloc` rejection tests — both
  pass again on revert.

Run them: `cargo test drop_props alloc_props`, `cargo test --test` is not needed
(in-crate). gcc round-trip is exercised by `cargo run -- run examples/drop.jtr`.

---

## Limitations

Honest accounting of what is stubbed, deferred, or only partially covered.

- **No `defer` / `errdefer` yet.** The explicit point-/error-path cleanup escape hatch
  is not implemented. Adding it cleanly means a new `ExprKind::Defer` threaded through
  *every* expr-walker (`find_calls_expr`, `collect_structs_in_expr`, `find_closures_*`,
  `find_spawns_*`, `collect_refs`, `collect_moved`, the escape walk, the printer) — miss
  one and a generic call or closure inside a `defer` silently vanishes from codegen. It
  was scoped out of this pass to avoid a half-applied ripple; the drop-scope stack it
  would plug into already exists.
- **The vtable `Allocator` is built but `std/` is not converted.** The real fn-pointer
  vtable `Allocator` + `Layout` ship in [`examples/alloc_vtable.jtr`](examples/alloc_vtable.jtr)
  / [`examples/vec_alloc.jtr`](examples/vec_alloc.jtr), but the **enum-dispatch stand-in**
  in [`examples/std/mem.jtr`](examples/std/mem.jtr) (`enum Allocator { system, arena(h) }`)
  and `std/list.jtr` have not yet been ported onto it. `resize`/grow-in-place and a
  separate-vtable (vs. inlined-fn-pointers) layout are also future refinements.
- **`Vec` is concrete (`IntVec`), not generic `Vec(T, A)`.** The allocator-parameterized
  collection works end-to-end for `i32`; a *generic* `Vec(T)` needs generic-struct `Drop`
  impl monomorphization (`impl Drop for Vec(i32)`), which the drop machinery does not yet
  do (it recognises concrete named/primitive receivers only).
- **A droppable `take` *parameter* is not dropped inside its callee.** Drop glue is
  registered for `let`/`var` locals, not for owned (`take`) parameters — so a consumed
  value that is neither moved-on nor returned leaks at the callee's scope exit
  (leak-safe). Owned-parameter drop is a small future extension.
- **No debug / fixed-buffer allocators.** The leak/double-free/UAF-catching debug
  allocator (the dynamic companion to the static escape proof) and `FixedBufferAllocator`
  are unbuilt — so there is **no `--features c-oracle` gcc-round-trip harness** yet
  (the current test layer is pure-Rust, asserting on emitted C; it never compiles the C).
- **`@deterministic` is reserved, not enforced.** It parses and validates but does not
  yet check the "same alloc sequence ⇒ same slot layout" property.
- **No linear / must-use types.** A type that *must* be consumed (dropping it is an
  error) — for locks/transactions/protocol handles — is not implemented. Today an
  unconsumed droppable is silently auto-dropped, which is also why region drop-elision
  trusts the programmer that a region local is arena-owned (a heap-owning struct placed
  in a region would leak — linear typing would catch it).
- **Move analysis is conservative, not per-branch-precise.** A value moved on *one*
  branch but not another is treated as moved everywhere (leak-safe, never double-free) —
  so it may be left undropped on the non-moving path. True conditional-move liveness is
  future work.
- **`@no_alloc` is direct-only.** It catches allocation *in the body*, not transitively
  (calling a helper that allocates) — matching the `@no_panic` analog. A call-graph
  "allocates" closure would make it transitive.
- **Drop coverage is concrete + top-of-block.** Drop is recognized for concrete
  named/primitive receivers (no generic `Drop(T)` impl monomorphization yet); locals in
  loop bodies drop per-iteration but `break`/`continue` short-circuit that (leak-safe).

None of these are soundness holes in the *no-double-free* sense — the conservative
choices all fail toward **leaking, never double-freeing**.

---

## Future plans

In rough priority order toward the Phase-3 exit criterion and self-hosting:

1. **Generic `Vec(T, A)` + port `std/` onto the vtable allocator.** Generalize the
   working concrete `IntVec` to a generic `Vec(T)` (needs generic-struct `Drop` impl
   monomorphization), and convert `std/mem.jtr` + `std/list.jtr` off the enum stand-in.
   Then **unify region ≡ arena allocator**: a `region r { }` *is* a scoped default
   `Allocator` value, plus the compile-time escape proof Zig's ArenaAllocator lacks.
   Add `resize`/grow-in-place and consider a separate `*const VTable` (vs. the inlined
   fn-pointers) so one vtable instance is shared across allocators.
2. **Owned-parameter drop.** Register `take` parameters for scope-exit drop glue (a
   small extension of the local registration), closing the one remaining leak class.
3. **The debug allocator + `--features c-oracle` harness.** A leak/double-free/UAF-
   catching allocator, wired to a gcc-compiling test layer: a full `Vec(T,A)` program
   pushes N, reads them back, and on scope exit the debug allocator **reports zero
   leaks** (the exit criterion); plus a planted-leak differential via
   `build_and_run_status`. Then `FixedBufferAllocator`.
4. **`defer` / `errdefer`** plugging into the existing drop-scope stack (LIFO with the
   implicit drops).
5. **Linear / must-use types** — a tier above affine: dropping one is a compile error.
   Rides the ownership + provability model; also hardens region drop-elision.
6. **`@deterministic` allocators** — enforce "same alloc sequence ⇒ same (page,offset)",
   feeding the numeric-reproducibility + content-addressed-snapshot story
   (`NUMERICS-RESEARCH.md` §2.4).
7. **Convergence with `core`/`std`** — `Vec`/`HashMap`/`String` over allocator-as-value,
   retiring every enum/intrinsic stand-in. This is the allocation substrate the
   self-hosted compiler will run on.
