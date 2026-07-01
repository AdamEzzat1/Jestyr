# Jestyr — Drop/RAII + Explicit Allocator Interface (Session Summary)

A summary of everything accomplished this session on the **Jestyr bootstrap
compiler** (Rust; pipeline `.jtr → C → native`) for **deterministic Drop/RAII**
and the **explicit allocator interface** (Phase 3 of `jestyr-design.md`).

**Headline:** the test suite grew from **413 → 445 passing tests** (+32), every
increment **green, warning-clean, teeth-checked, and auto-committed**. Full
write-up lives in the repo at `DROP-ALLOC-PHASE3.md`; the test inventory is in
`docs/TESTING.md §5.13`.

> Thesis honored throughout: **implicit ≠ hidden.** Destructors run automatically,
> but the ownership model decides liveness *statically* (no runtime drop flags, no
> unwinding) and the inserted glue is *inspectable* via `--show-drops`.

---

## At a glance

| # | Increment | Outcome |
|---|-----------|---------|
| 1 | Scope-exit drop glue + `--show-drops` | Auto-drop at scope exit, reverse order, drop-flag-free |
| 2 | Move analysis / drop-after-move elision | A moved/returned value is dropped by its new owner, never twice |
| 3 | `@no_alloc` enforced contract | A fn proven allocation-free by the escape checker, or a compile error |
| 4 | Reject manual `drop()` + region bulk-drop | Can't hand-call `drop`; region-owned values free in bulk |
| 5 | fn-pointer-vtable `Allocator` + `Layout` | Real Zig-shape allocator value (retires the enum stand-in) |
| 6 | Allocator-parameterized `Vec`, RAII-freed | `IntVec` frees its buffer through its stored allocator at scope exit |
| 7 | Generic `Vec(T, A)` + generic-call move precision | Generic collection, RAII-freed, with precise argument alignment |
| 8 | `std/` ported onto the vtable allocator | `mem` + `list` retire the enum; `List(T)` frees by RAII |
| 9 | Blanket generic `impl[T] Drop for Ctor(T)` | One impl covers every instantiation — no per-type boilerplate |
| — | Atomics (adjacent: concurrency workstream N) | `atomic_store/load/add/sub` over GCC `__atomic` builtins |

---

## Drop / RAII

### 1. Scope-exit drop glue — static, drop-flag-free
A local whose type has an `impl Drop for T` is dropped automatically at scope
exit, in **reverse declaration order**.

- A **drop-scope stack** (one entry per emitted `{ }` block); a local registers
  for drop glue *as its `let` is emitted*, so an early `return` can never drop a
  not-yet-declared local. Liveness is therefore static and per-branch — the Jestyr
  win over Rust's runtime drop flags.
- A **`return`** spills its value to a temp, runs the live drops, then returns — so
  the returned value can't read a local we just dropped. This reuses the existing
  `ensures`/`j_result` spill seam, which is why every `if`/`match`/`block`
  return-tail is covered for free.
- **No unwinding → straight-line drop:** Jestyr aborts on fault, so there are no
  landing pads or double-panic cases; the glue is plain sequential C.
- Demo: `examples/drop.jtr` → `100, 2, 1, 200, 300, 7`.

### 2. Move analysis (drop-after-move elision)
`cgen::collect_moved` computes (over-approximately, hence **leak-safe**) the locals
whose value *escapes* — returned, passed by value to a `take` parameter, captured
into a struct, rebound, or consumed by a `take self` receiver. Those get **no**
drop glue (the new owner drops them), so a value is dropped **at most once** — no
double-free by construction.

A later refinement (Increment 7) made this **precise by call shape**: a call
argument moves *only* when its parameter is `take`; a `read`/`mut`/`out` borrow does
not. This is what lets a value be mutated via `mut`-borrow methods yet still drop.

### 3. `--show-drops` inspection
`jestyrc emit-c <file> --show-drops` annotates each inserted drop with a
`/* drop j_x : T */` comment — implicit control flow made visible.

### 4. Manual `drop()` rejected + region bulk-drop
- A hand-written `value.drop()` (resolving through `impl Drop`'s `drop`) is a
  compile error — the auto-drop still fires, so calling it manually would
  double-free. Reported with a caret at `check` time.
- **Region-integrated bulk drop:** a value owned by a `region { }` block emits
  **zero** per-value drop glue — the arena reclaims it in bulk. Verified
  metamorphically (one drop in a plain block, zero inside a region).

---

## Explicit Allocator Interface

### 5. fn-pointer-vtable `Allocator` + `Layout`
Zig's `std.mem.Allocator` shape as a real first-class value, retiring the
enum-dispatch stand-in:

- An `Allocator` is an opaque `ctx: *mut u8` plus a vtable of thin function
  pointers (`alloc_fn`/`free_fn`), with `Layout { size, align }` carrying the
  request.
- **One** user-facing path (`alloc_n`/`free_n`) runs over *any* allocator —
  `a.alloc_fn(a.ctx, layout)` lowers to a **genuine indirect call**, not a bare
  `malloc`.
- Demo `examples/alloc_vtable.jtr` drives the same path with two strategies
  (system/malloc and a bump arena) → `10, 20, 30, 40`.
- Reused the existing fn-pointer-type machinery — no parallel dispatch invented.

### 6. Allocator-parameterized `Vec`, freed by RAII — the integration forcing function
`IntVec` *stores* its `Allocator` value (Rust's `Vec<T, A>`), grows its buffer
through that allocator's vtable, and its `Drop` impl frees the buffer **at scope
exit, through the very allocator it was allocated from** — no manual free.

- Demo `examples/vec_alloc.jtr` → `5, 10, 50, 99`, then `v` drops.
- Exercises every seam at once: the vtable allocator, scope-exit drop glue, the
  take-vs-borrow move analysis, and `@copy` on the cheap allocator handle so a
  collection may store it without escape.

### 7. Generic `Vec(T, A)`, RAII-freed
The concrete `IntVec` generalized: `Vec(T)` is monomorphized per element type and
RAII-freed through generic, comptime-parameterized operations (`vec_push(i32, v, …)`).

- Generic-struct `Drop` already lowered correctly (the impl-method symbol derives
  from the GenStruct type key on both call and definition side).
- The fix was **move-analysis precision**: `collect_moved` now aligns arguments to
  parameters *by call shape* — a free call skips its leading `comptime` type-argument
  slot so the value lands at its real `mut` parameter (a borrow); a method/impl call
  offsets past the receiver. Without this, the vector's drop was silently suppressed
  the instant it was passed to `vec_push`.
- Demo `examples/vec_generic.jtr` → `5, 10, 50, 99`.

### 8. `std/` ported onto the vtable allocator
Retired the enum-dispatch stand-in across the standard library:

- `std/mem.jtr` is now the real vtable `Allocator` (`alloc`/`free`/`destroy`
  fn-pointers + `Layout`, system + arena strategies).
- `std/list.jtr`'s `List(T)` **stores** its allocator and frees its buffer **by
  RAII** at scope exit.
- Shipped demos unchanged in output: `examples/std/demo.jtr` (`5, 10, 50, 40`, then
  `xs` auto-frees) and `examples/std/alloc_demo.jtr` (`60, 60`).

### 9. Blanket generic `impl[T] Drop for Ctor(T)` — new compiler work
A single impl covers **every** instantiation — no per-type boilerplate:

- **parser/AST:** `impl` gained optional bracket generics (`ImplDecl.generics`),
  reusing `parse_generics`; the printer renders them.
- **cgen:** `generic_drop_impl(ctor)` detects a blanket Drop impl by constructor;
  `drop_key_of` recognises a `Ctor(C)` local as droppable through it;
  `emit_generic_drop_methods` monomorphizes the `drop` method once per concrete
  `struct_instance` (substituting the impl's type parameter, named by the instance's
  type key so the existing scope-exit call site resolves). A concrete
  `impl Drop for Ctor(C)` takes precedence (coherence — no duplicate symbol).
- Verified across two distinct instantiations (`Box(i32)` + `Box(f64)`). Both
  `std/list.jtr` and `examples/vec_generic.jtr` now use a single blanket impl.

---

## Headline guarantees realized

| Guarantee | Status |
|---|---|
| Drop-flag-free, static per-branch liveness | ✅ |
| No-unwinding, straight-line drop | ✅ |
| Reverse-declaration drop order | ✅ |
| Drop exactly once (no double-free) | ✅ + property + teeth |
| Drop-after-move elision | ✅ |
| Region-integrated bulk drop | ✅ + metamorphic property |
| Implicit-but-inspectable (`--show-drops`) | ✅ |
| Can't call `drop` manually | ✅ |
| `@no_alloc` enforced contract | ✅ + sound/complete property + teeth |
| Explicit fn-pointer-vtable `Allocator` + `Layout` | ✅ (retires the enum stand-in) |
| One alloc path over many strategies (system/arena) | ✅ + golden |
| Take-vs-borrow move precision | ✅ + property + teeth |
| Allocator-parameterized `Vec`, RAII-freed | ✅ concrete `IntVec` **and** generic `Vec(T,A)` |
| `std/` (`mem`+`list`) on the vtable allocator, RAII `List` | ✅ + wiring test |
| Blanket generic `impl[T] Drop for Ctor(T)` | ✅ + golden + fuzz + teeth |

---

## Testing discipline

Four layers, every increment; the suite stays toolchain-free and fast by default.

- **Goldens** (`src/cgen.rs::tests`): drop call emitted; reverse order; move
  elision; `--show-drops`; no glue without an impl; region drop-elision; the
  allocator routes through the vtable (not bare malloc); mut-borrowed droppable
  still drops once; taken droppable not dropped by caller; generic-struct
  instantiation drops; blanket impl monomorphizes per instance.
- **Escape unit** (`src/escape.rs::tests`): `@no_alloc` rejects heap alloc / region,
  accepts a clean body, is per-function; manual `drop()` rejected.
- **Wiring** (`src/module.rs::tests`): the ported std `List(i32)` drops by RAII and
  allocation routes through the vtable.
- **Properties** (`src/proptests.rs`): `drop_props` (drops-each-owned-exactly-once,
  move elision, no-double-free, determinism, region bulk-drop metamorphic,
  borrow-passed-still-drops-once, generic-borrow-still-drops-once); `alloc_props`
  (rejected *iff* allocates — soundness + completeness vs an independent oracle).
- **Fuzz** (`bolero`): `fuzz_drop_alloc_pipeline`, `fuzz_drop_alloc_determinism`,
  `fuzz_blanket_drop_impl`.
- **Teeth-checked**: every property/golden was confirmed to *fail* under a targeted
  mutation of the relevant checker/emitter, then pass again on revert.

Run them:
```sh
cargo test                                   # full suite (445)
cargo test drop_props alloc_props            # the Phase-3 properties
cargo run -- emit-c examples/drop.jtr --show-drops   # inspect drop glue
cargo run -- run examples/vec_generic.jtr    # generic RAII Vec → 5, 10, 50, 99
cargo run -- run examples/std/demo.jtr       # ported std → 5, 10, 50, 40
```

---

## Demos shipped this session

| File | Output | Shows |
|------|--------|-------|
| `examples/drop.jtr` | `100, 2, 1, 200, 300, 7` | reverse-order scope-exit drop + move elision |
| `examples/alloc_vtable.jtr` | `10, 20, 30, 40` | vtable allocator, one path / two strategies |
| `examples/vec_alloc.jtr` | `5, 10, 50, 99` | concrete `IntVec`, RAII-freed |
| `examples/vec_generic.jtr` | `5, 10, 50, 99` | generic `Vec(T)` + blanket Drop impl |
| `examples/std/demo.jtr` | `5, 10, 50, 40` | full std on the vtable allocator, RAII `List` |
| `examples/std/alloc_demo.jtr` | `60, 60` | one code path over system + arena |
| `examples/atomics.jtr` | `4` | data-race-free shared counter (adjacent: concurrency) |

---

## Remaining / future work (honestly deferred)

- **Qualified + generic-helper calls inside impl-method bodies.** A
  module-qualified call (`mem.release`) inside an `impl` method is lowered as a field
  access, and the monomorphization worklist doesn't descend into a blanket-impl body —
  so std destructors delegate to a bare, non-generic helper for now. Fixing both
  removes that shim.
- **`region ≡ arena` unification**, `resize`/grow-in-place, a shared `*const VTable`.
- **A leak-catching debug allocator** + a `--features c-oracle` gcc-round-trip
  harness (the Phase-3 exit criterion; the current test layer is pure-Rust).
- **`defer`/`errdefer`**, **owned (`take`) parameter drop**, **linear / must-use
  types**, **`@deterministic` allocators** (registered/reserved), **transitive
  `@no_alloc`**, **conditional (per-branch) move precision**.
- **Concurrency workstream N** (atomics landed; Mutex / channels / task-results-await
  / deterministic par-loop remain).

---

## Commits (this session, chronological)

```
Drop/RAII Phase 3, Increment 1: static scope-exit drop glue + --show-drops
Drop/RAII Phase 3, Increment 3: @no_alloc — the enforced allocation-free contract
Drop/RAII Phase 3: reject manual `drop()` calls
Drop/RAII Phase 3, Increment 4 (partial): region-integrated bulk drop
Drop/RAII Phase 3: DROP-ALLOC-PHASE3.md + TESTING.md §5.13
Drop/RAII Phase 3, Increment 5: fn-pointer-vtable Allocator + Layout
Drop/RAII Phase 3, Increment 6: allocator-parameterized Vec, freed by RAII
Drop/RAII Phase 3: document the vtable allocator + RAII Vec (Increments 5–6)
Concurrency N1: atomics (sequentially-consistent ops on an int64 cell)
Generic Vec(T, A): generic-struct Drop + precise generic-call move analysis
Generic Vec(T, A): document the milestone
Port std/ onto the fn-pointer-vtable Allocator, with RAII-freed List
Blanket generic impl[T] Drop for Vec(T): generic-impl monomorphization
Document the std port + blanket Drop impl
```

*(All on the worktree branch; not pushed.)*
