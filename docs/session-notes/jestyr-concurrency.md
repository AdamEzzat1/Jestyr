# Jestyr Concurrency (workstream N) — session summary

Building out Jestyr's concurrency story on top of the existing structured-concurrency +
atomics machinery. One green increment per commit to `master`. The headline target is a
**deterministic `par` reduction loop** (bit-identical to serial, enforced by the compiler);
this doc tracks each slice as it lands.

## Starting point (already in-tree)
- **Structured concurrency**: `concurrent { spawn f(args) }` → pthread-per-spawn + join at
  the closing brace (`cgen::emit_concurrent`/`spawn_runtime`). No task outlives its scope.
- **Atomics**: seq-cst `__atomic_*` ops on an `int64` cell (`atomic_store/load/add/sub`).
- **Proven deterministic parallel reduction**: `core.par_binned_sum` — splits a slice across
  workers, each binning into a *disjoint* accumulator region, merged by integer add and
  finalized once; bit-identical to serial for any split. The template for the headline.
- **Data-race rule**: `escape::check_spawn_no_shared_mut_slice` rejects spawning a target
  that takes a `mut`/`out` **slice** (the one safe-subset race). Shared mutable state must go
  through a raw `*mut T` in `unsafe`, each task a disjoint region.
- **Determinism contract**: locked `CC_FLAGS = -O2 -std=c11 -ffp-contract=off -fno-fast-math`
  + the cross-OS SHA canary make "bit-identical across schedules" a *testable* claim.

---

## Increment 1 — Mutex as an Ada-style protected object ✅

**What it is.** `std/sync.jtr`'s `Mutex(T)` bundles the guarded value, its lock, and the
operations into one unit. The only way to reach the value is through `mutex_with` (the
protected *procedure*) or `mutex_get` (the protected *function*), each of which brackets the
access between a lock acquire and release. **You cannot forget to lock** — there is no public
path that touches the value without holding the lock, so mutual exclusion is *structural*,
not a convention. This is the Ada/Ravenscar protected-object idea: a better Mutex.

**API (`examples/std/sync.jtr`):**
```
pub fn lock_acquire(lock: *mut i64)            // test-and-set spin until free
pub fn lock_release(lock: *mut i64)            // seq-cst store 0
pub fn Mutex(comptime T: type) -> type         // { data: *mut T, lock: *mut i64 }
pub fn mutex_make(comptime T: type, init: T) -> Mutex(T)
pub fn mutex_with(comptime T: type, read m: Mutex(T), op: fn(*mut T))  // exclusive access
pub fn mutex_get (comptime T: type, read m: Mutex(T)) -> T            // locked snapshot
pub fn mutex_free(comptime T: type, read m: Mutex(T))
```

**How it works.** The lock is a **test-and-set spinlock over a single atomic `int64`**:
`lock_acquire` atomically swaps in `1` and inspects the previous value — `0` means the lock
was free and is now ours, anything else means spin. `lock_release` is a seq-cst store of `0`,
which publishes every write made under the lock to the next acquirer. The guarded value and
lock word live on the heap; every task holding a copy of the `Mutex` shares those same two
cells (the pointers alias *deliberately* — that is the shared state — and the lock serializes
all access). Combined with `concurrent { … }` (which joins every task before the value is
freed), a Mutex can't be used after free either.

**Why it fits the theses.** The whole primitive is **library Jestyr over the existing
atomics** — no OS mutex, no `pthread_mutex_t` of platform-specific layout embedded in a
generated struct, no special type. It needed exactly **one** new compiler primitive.

### Compiler changes (minimal, additive)
- `cgen.rs`: new intrinsic `atomic_xchg(p, v)` → `__atomic_exchange_n((int64_t*)(p),
  (int64_t)(v), __ATOMIC_SEQ_CST)` — the one indivisible read-modify-write a spinlock needs.
  (A new match arm beside the other atomics; *not* a new `ExprKind`, so none of the five cgen
  walkers change.)
- `typeck.rs`: new `atomic_intrinsic_ret` types all atomics (`load/add/sub/xchg`) as `i64`,
  so the spinlock's `atomic_xchg(lock,1) != 0` comparison types without a cast or annotation.
- **No new syntax. `module.rs` and `main.rs` untouched** (the parallel-session contract).

### Test rigor (all green, `cargo test` stays toolchain-free, warning-clean)
1. **cgen unit** — `atomic_xchg_lowers_to_exchange_builtin`: lowers to `__atomic_exchange_n`.
2. **Wiring (toolchain-free)** — `sync_props::mutex_example_compiles_clean`: the shipped demo
   (`import "sync"` + `Mutex(i64)` shared across `concurrent { spawn … }`) lowers with zero
   diagnostics through the real module pipeline — proving a `read Mutex(T)` spawn arg is
   *accepted* (the sanctioned sharing path) while the `mut`-slice spawn rule stays in force.
3. **Mutual-exclusion property (pure Rust)** — `sync_props::tas_lock_serializes_increments`:
   a model of the emitted test-and-set lock + guarded counter; `n` threads × `k` increments,
   each the four-step critical region the lowering produces (acquire → read → add → write →
   release), driven by an arbitrary generated interleaving. The result is **exactly `n*k`
   for every schedule** — no lost updates. (Schedule-independent, mirroring the
   determinism-property philosophy rather than running real threads in a unit test.)
   - **Teeth** — `unlocked_increments_lose_updates`: disable the lock in the model and an
     interleaving loses an update (< `n*k`), proving the property is enforced by the lock.
4. **Fuzz** — `fuzz::fuzz_concurrency_pipeline`: arbitrary bytes dropped into a spawn body and
   a `concurrent { spawn … }` arg slot (with atomics + `atomic_xchg`); lowering stays total
   *and* deterministic, and the escape checker never silently accepts a `mut`-slice spawn.
5. **Real-thread proof** (`--features c-oracle`) — `c_oracle::mutex_demo`: builds and runs
   `examples/std/mutex.jtr` (8 threads each incrementing one guarded counter through
   `mutex_with`), asserting `8` — repeated 8× to shake out races. Deterministic, so it pins
   cross-OS like the parallel-reduction demo.
6. **Teeth-verified live**: temporarily breaking `lock_acquire` (no-op) dropped the
   many-increment demo from the exact total to a non-deterministic <N (e.g. 329488, 372644,
   284733 of 400000); reverted.

**Demo:** `examples/std/mutex.jtr` → `8`.

---

## Increment 2 — Move-only channels (share by communicating) ✅

**What it is.** `std/sync.jtr`'s `Channel(T)` is a bounded ring buffer guarded by the
spinlock from increment 1. The headline is its `send`: it takes the value by **`take`**, so
ownership *moves into* the channel and no alias survives in the sender. Race-freedom then
falls out of the *existing* escape analysis — **no `Send`/`Sync` marker traits, no runtime
detector**: the give-away rule already forbids handing a *borrow* to a `take` parameter, so
you can only send something you own, and once sent it is the receiver's. This is the
Erlang/Pony share-nothing model expressed through Jestyr's second-class refs (a uniquely-owned
`take` value ≈ Pony `iso`); combined with `concurrent { … }`, a message can't be touched by
its sender after it leaves.

**API (`examples/std/sync.jtr`):**
```
pub fn Channel(comptime T: type) -> type            // { buf: *mut T, ctrl: *mut i64, cap }
pub fn channel_make(comptime T: type, cap: i64) -> Channel(T)
pub fn channel_send(comptime T: type, read ch: Channel(T), take v: T)   // move in; blocks if full
pub fn channel_recv(comptime T: type, read ch: Channel(T)) -> T         // move out; blocks if empty
pub fn channel_len (comptime T: type, read ch: Channel(T)) -> i64
pub fn channel_free(comptime T: type, read ch: Channel(T))
```

**How it works.** The control word packs `[head, tail, count, lock]` into one `[4]i64` cell,
so a channel handle is two heap allocations (buffer + control), copied by value into each task
— every copy shares the same buffer, indices, and lock. `channel_send` spins until `count <
cap`, then writes the value into `buf[tail]` *under the lock* (the single move) and advances
the tail. `channel_recv` spins until `count > 0`, reads `buf[head]` out **under the lock** (so
no concurrent sender can overwrite the slot mid-read), advances the head, and returns by value.
Bounded capacity gives natural backpressure between producer and consumer.

**Why it fits the theses.** It is **pure library Jestyr over the spinlock** — like the Mutex,
no new syntax and no codegen change. The only compiler touch was an escape-checker *fix*.

### Compiler change (one additive escape fix)
- `escape.rs`: the give-away route (route 4) now resolves **module-qualified** callees via
  `info.qualified` (a valid `table.fns` key), so a `take` parameter reached through a
  qualified generic call — `mod.f(T, take v)`, which is *every* channel `send` — is checked.
  Previously the free-call path only handled bare `Name` callees, so a qualified generic call
  silently skipped the give-away check and a borrow could be sent. Arg/param indices already
  align positionally (both include the comptime type slot), so the existing logic applies.
- **Teeth-verified live**: before the fix, `sync.channel_send(Box, ch, borrowed)` compiled;
  after, it errors at the argument. `module.rs`/`main.rs` untouched.

### Test rigor (all green, toolchain-free `cargo test`, warning-clean)
1. **Wiring (toolchain-free)** — `sync_props::channel_example_compiles_clean`.
2. **Move-on-send soundness** — `sync_props::qualified_take_of_borrow_is_rejected`: sending a
   *borrow* through a qualified `take` is rejected; teeth `…_of_owned_is_accepted` shows an
   owned value compiles clean (rejects only borrows, never legitimate moves).
3. **Ring-buffer model property (pure Rust)** — `sync_props::channel_ring_preserves_every_
   value`: a model of the send/recv index math; every sent value is received exactly once
   (multiset-preserving) for any capacity and any send/recv interleaving — the buffer never
   drops, duplicates, or corrupts an item.
4. **Real-thread proof** (`--features c-oracle`) — `c_oracle::channel_demo`: builds and runs
   `examples/std/channel.jtr` (multi-producer fill+drain → `264`; cap-2 concurrent
   producer+consumer with real backpressure → `36`), repeated 8×. Order-independent sums, so
   deterministic.

**Demo:** `examples/std/channel.jtr` → `264`, `36`.

**Noted limitation.** Use-after-move of an *owned local* is not yet a compile error
language-wide (the bootstrap tracks moves only for drop-glue, not for diagnostics), so the
"no alias survives the send" guarantee currently rests on "can't *send* a borrow" plus
structured-scope join. Full use-after-move tracking is a separate language-wide item (akin to
the tracked "no struct-field auto-drop" gap), not channel-specific.

---

## Increment 3 — `par_reduce` library (the headline, library tier) ✅

**What it is.** `core.par_binned_sum` proved *one* bit-reproducible parallel reduction;
`core.par_reduce` generalizes the **shape** to any reduction declared as a value. A
`Reduction` carries an `identity`, an `accumulate` (fold one element into a worker-local
accumulator), and an order-independent `combine` (merge two accumulators). `par_reduce`
partitions the slice across `PAR_WORKERS`, each worker folds its chunk into a **disjoint** slot,
then the slots merge with `combine` — **bit-identical to the serial fold for any chunk split or
thread schedule**, because the built-in integer ops (+, min, max, xor) are associative *and*
commutative at the machine-integer level.

**API (`examples/std/core.jtr`):**
```
pub struct Reduction { identity: i64, accumulate: fn(i64,i64)->i64, combine: fn(i64,i64)->i64 }
pub fn sum_reduction() -> Reduction      // identity 0,        + 
pub fn min_reduction() -> Reduction      // identity i64::MAX, min
pub fn max_reduction() -> Reduction      // identity i64::MIN, max
pub fn xor_reduction() -> Reduction      // identity 0,        xor
pub fn serial_reduce(read s: []i64, r: Reduction) -> i64    // left fold; the in-program oracle
pub fn par_reduce   (read s: []i64, r: Reduction) -> i64    // == serial_reduce, bit-for-bit
```

**Why `i64` (not generic over the accumulator).** `spawn` targets **cannot be generic** —
verified directly: a generic worker fails with *"the C backend cannot lower the external type
`A`"* because the spawn arg-struct carries no monomorphization context. Specializing the
accumulator to `i64` keeps the worker monomorphic with **zero compiler change** (the increment's
hard requirement) and covers every deterministic integer reduction. A naive `f64 +` reduction
is *deliberately omitted* — it reassociates under parallelism, so it is the **rejection target**
for the `par for … reduce(r)` surface (tier 2); the bit-exact float case stays `par_binned_sum`.

### Test rigor (all green, toolchain-free `cargo test`, warning-clean)
1. **Determinism star (pure Rust)** — `core_props::par_reduce_is_split_independent`: for each
   built-in, whole-fold == chunked-fold-then-merge for *any* split into `nchunks` pieces
   (mirrors `binned_sum_is_chunk_independent`). Inputs bounded so the sum stays in the exact,
   associative i64 regime.
2. **Real-thread proof** (`--features c-oracle`) — `c_oracle::par_reduce_int_demo`: builds and
   runs `examples/std/par_reduce_int.jtr`, pinned cross-OS to `153 1 17 1  1 1 1 1`
   (sum/min/max/xor of 1..=17, then four par==serial equality flags).

**Demo:** `examples/std/par_reduce_int.jtr` → `153, 1, 17, 1, 1, 1, 1, 1`.

**Spawn-is-not-generic finding (for the next sessions).** Because `spawn` cannot target a
generic function, the `par for … reduce(r)` surface (tier 2) and dynamic-N spawn will either
desugar to monomorphic-per-type workers or need the spawn lowering taught to carry the
monomorphization substitution. The `i64` `par_reduce` is the runtime engine the checked `par
for` surface will desugar onto.

---

## Increment 4 — Task results + `await` (the first non-library slice) ✅

**What it is.** `let h = spawn f(args)` now binds an **awaitable handle** of type `Task(T)`
(T = f's return type); `await h` joins the task and yields its result. The first increment to
thread a new AST node (`ExprKind::Await`) **and** a new `Ty` (`Ty::Task`) through all six files
(`ast → parser → typeck → escape → cgen → printer`). Bare `spawn f(args)` stays fire-and-forget.

```
concurrent {
    let lo = spawn sum_squares(1, 6)        // task → Task(i64)
    let hi = spawn sum_squares(6, 11)
    let total: i64 = await lo + await hi     // join both, combine → 385
    print_int(total as i32)
}
```

### How it lowers (structured, over the existing nursery)
- **Result-passing.** Each spawn site's task box (`struct _jsp_<id>`) gains a `ret` field of
  the target's return type; the trampoline writes `_a->ret = f(args)` (void targets keep the
  old `f(args)` form and no `ret` field).
- **`await h`** is a GCC statement-expression `({ if (!_jd) { pthread_join(_jt, NULL); _jd = 1; } _ja.ret; })`
  — join-once then read the stored result. A per-handle `_jd` flag guards against a double
  join: the `concurrent` brace still emits a **safety-net join**, now `if (!_jd) …` for
  awaitable handles (bare spawns join unconditionally as before). So an un-awaited, a
  once-awaited, and a conditionally-awaited handle are all correct.
- **`Ty::Task(T)`** is non-`Copy` and **never materializes as a runtime value** — `spawn`/`await`
  are resolved to the scope's thread vars by the backend (a `HashMap<name, TaskHandle>` on the
  codegen, saved/restored across nested `concurrent` blocks). So `c_type`'s catch-all suffices;
  only `is_copy`/`display` needed the new arm.

### Parsing — precedence
`await` parses its operand at the **postfix** level, so it binds tighter than `as` and binary
operators: `await a + await b` is `(await a) + (await b)`, and `await t as i32` is `(await t) as
i32` (not `await (t as i32)` — the bug the first attempt hit).

### Test rigor (all green, toolchain-free `cargo test`, warning-clean)
1. **Parser** — `parses_spawn_binding_and_await`; `await_binds_tighter_than_cast_and_binary`
   (asserts a `Cast` wrapping an `Await`).
2. **Typeck** — `spawn_yields_a_task_handle_and_await_unwraps_it` (`spawn sq(3)` → `Task(i64)`,
   `await h` → `i64`); `await_of_a_non_task_is_a_type_error`.
3. **Cgen lowering** — `lowers_spawn_result_and_await_to_join_and_read` (asserts the `ret`
   field, the trampoline store, the `_jd0`-guarded join-once, the `.ret` read, the guarded
   brace-join). **Teeth-verified by mutation**: dropping the trampoline store makes it fail.
4. **Real-thread proof** (`--features c-oracle`) — `await_demo`: builds and runs
   `examples/std/await.jtr` (×8), pinned to `385 14`.

**Demo:** `examples/std/await.jtr` → `385, 14`.

**Scope note.** `await` resolves a handle bound by `let h = spawn …` in the *same* `concurrent`
scope; a handle is await-only (not stored in a struct, returned, or passed to another function).
Cross-scope / first-class task values would need a heap-boxed handle and are deferred.

---

## Increment 5 — `par for … reduce(r)`: the headline checked guarantee ✅

**What it is.** `par for x in xs reduce(r) { body }` maps each element through `body` and
reduces the results **in parallel** — with the marquee guarantee: the compiler **accepts only
declared deterministic reductions** and **rejects a non-deterministic one at compile time**.
*Parallelism that cannot change your answer*, enforced as a compile error.

```
let total: i64 = par for x in xs reduce(core.sum_reduction()) { x * x }   // parallel sum-of-squares
```

A new `ExprKind::ParFor` threaded through all six files; a new `par` keyword; `reduce` is a
contextual keyword (like `step`). The loop variable, body, element type, and result are `i64`
today (matching the `par_reduce` engine).

### The checked guarantee (the headline)
Typeck requires the `reduce(r)` constructor to be a **declared deterministic reduction** — one
of `core`'s `sum_reduction` / `min_reduction` / `max_reduction` / `xor_reduction`, whose
`combine` is associative *and* commutative at the machine-integer level. Anything else is a
compile error:
```
error: `par for` requires a declared deterministic reduction (one of: sum_reduction,
min_reduction, max_reduction, xor_reduction); `my_reduction` is not one. A non-deterministic
reduction (e.g. a naive float `+`, which reassociates under parallelism) would make the result
depend on the thread schedule — exactly what `par for` exists to prevent.
```
The trusted set is the four `core` built-ins (a `@deterministic` attribute admitting
*user-declared* reductions is future work — the conservative default refuses what it can't
certify). This is the property **no other systems language makes a *checked* one**: Rayon/
OpenMP/Chapel give parallel reductions, but IEEE-754 reassociation means "deterministic" is
hoped-for, not compiler-enforced.

### How it lowers
Desugars onto `core.par_reduce` (increment 3, the tested deterministic engine): a statement-
expression that (1) serially maps each element through `body` into a scratch `int64_t[]` (an
element-wise map is *always* deterministic), then (2) runs `jestyr_par_reduce` over the mapped
buffer — the parallel, reassociation-sensitive step, already proven bit-identical-to-serial.
So map-then-reduce is fully deterministic. Requires `import "core"` (the reduction value comes
from there, so it always holds); `core.par_reduce` is emitted because every non-generic module
function is.

### Test rigor (all green, toolchain-free `cargo test`, warning-clean)
1. **Parser** — `parses_par_for_reduce` (the `ParFor` node + its loop variable).
2. **Typeck** — `par_for_accepts_a_deterministic_reduction_and_types_as_i64`;
   `par_for_rejects_a_non_deterministic_reduction` (**the headline**, **teeth-verified by
   mutation**: relax the allowlist → the reject test fails).
3. **Cgen lowering** — `lowers_par_for_to_serial_map_plus_parallel_reduce` (the scratch
   `malloc`, the `jestyr_par_reduce(` call, the mapped slice).
4. **Determinism** — inherited from `core_props::par_reduce_is_split_independent` (the engine
   `par for` desugars onto).
5. **Real-thread proof** (`--features c-oracle`) — `par_for_demo`: builds and runs
   `examples/std/par_for.jtr` (×8), pinned to `819 1 13`.

**Demo:** `examples/std/par_for.jtr` → `819, 1, 13` (parallel sum-of-squares; bit-identical to
serial; parallel max).

**Coordination note.** This surface is the shared headline of *both* workstream N (concurrency)
and workstream Q (parallelism). N landed it first (the "land the AST shape, others rebase"
strategy); Q's `std/parallel.jtr` SOACs and the future schedule-split (`with schedule(threads,
chunk)`) build on the same `ExprKind::ParFor` + the `par_reduce` engine. The schedule-split
needs **dynamic-N spawn** (the shared `emit_concurrent` change) — the natural next increment for
either stream.

---

## Increment 6 — dynamic-N spawn (runtime worker count) ✅

**What it is.** A `spawn` *inside a loop* now launches a number of tasks chosen at **runtime**,
not pinned at compile time. The `concurrent { … }` nursery collects every spawned thread on a
**growable handle array** and joins them all at the closing brace — structured concurrency with
a dynamic worker count. This is the building block the parallelism workstream's tunable `with
schedule(threads, chunk)` split needs.

```
concurrent {
    var w: i64 = 0
    for w < n { spawn worker(buf, w)  w = w + 1 }   // n tasks, n a runtime value
}                                                   // all joined here
```

**How it lowers (`cgen::emit_concurrent` + `emit_dyn_spawn`).** `emit_concurrent` scans the
block: a `spawn` nested in a loop/`if` (not at the top level) flips the block into *dynamic
mode*, declaring a growable array once: `pthread_t* _dt; void** _da; size_t _dn, _dc;`. Each
dynamic `spawn` (intercepted in `emit_stmt` when `dyn_spawn_active`) pushes: grow on demand
(`realloc`), **heap-allocate the arg box** (`malloc(sizeof(struct _jsp_<id>))` — a *stable*
address the thread reads, since the arrays may `realloc`-move), `pthread_create`, `_dn++`. At
the brace: `for (_dk…) { pthread_join(_dt[_dk]); free(_da[_dk]); } free(_dt); free(_da);`. The
fixed numbered-handle path (top-level `spawn` / `let h = spawn`) is untouched and coexists.

**Why heap arg boxes (not an array of boxes).** A growable `struct _jsp_<id>*` array would
`realloc`-move after threads captured `&_args[k]`, dangling those pointers. Per-task `malloc`
gives each box a stable address; the box is freed after its `pthread_join`. No trampoline
change (the existing trampoline reads its box; the nursery frees it).

### Test rigor (all green, toolchain-free `cargo test`, warning-clean)
1. **cgen lowering** — `lowers_dynamic_spawn_in_a_loop_to_a_growable_handle_array`: the growable
   array, per-task heap arg box, push, and the join-and-free loop are all emitted.
2. **Real-thread proof** (`--features c-oracle`) — `dynamic_spawn_demo`: a runtime worker count
   (10, then **64** threads) each writing a disjoint slot, summed deterministically → `285`,
   `85344`, repeated 8× to shake out races.
- The existing escape data-race rule still guards each spawn (a `mut`/`out` slice target is
  rejected), so dynamic spawns are as race-safe as fixed ones.

**Demo:** `examples/std/dynamic_spawn.jtr` → `285`, `85344`.

**Surface-syntax note.** `par` is a **contextual** keyword (only special immediately before
`for`), so `par` remains a valid ordinary identifier — discovered when reserving it outright
broke `let par = …` in the binned `par_reduce.jtr` / `numerics_canary.jtr` demos.

---

## Increment 7 — the `@deterministic` contract (schedule-independence) ✅

**What it is.** A `@deterministic` function is *certified by the compiler* to produce a
**schedule-independent** result. Inside it, the raw concurrency primitives whose result can
depend on the thread schedule — `concurrent`/`spawn` and the `atomic_*` ops — are **compile
errors**; parallelism is permitted only through the *checked* deterministic `par for …
reduce(r)`. This is the Ada/Ravenscar "provable subset" fused with Jestyr's determinism thesis —
the `@verified` tie-in: "this function's answer cannot change with the schedule," enforced.

```
@deterministic fn sum_of_squares(read s: []i64) -> i64 {
    return par for x in s reduce(core.sum_reduction()) { x * x }   // OK — checked deterministic
    // concurrent { spawn … }   // compile error
    // atomic_load(c)            // compile error
}
```

**How it's enforced (`escape.rs`).** Mirrors the `@no_alloc` machinery exactly: a per-function
`self.deterministic` flag (set from `f.has_attr("deterministic")`, saved/restored around nested
bodies), and per-op rejection — the `Concurrent` arm errors, and a `check_deterministic_call`
rejects the `atomic_*` intrinsics (bare or qualified). `par for` is explicitly allowed. The
*transitive* closure ("calls a function that uses atomics," so a `Mutex`/`Channel` op is caught)
is future work, exactly as for `@no_alloc`.

**Attribute status.** `@deterministic` was already registered but `Reserved` (numerics had
penciled it for an *allocator*-determinism contract). It is now `Active` enforcing
schedule-determinism. The two are complementary facets of "deterministic" and compose — both
only *reject* non-deterministic code, so a pure-float numeric `@deterministic fn` still passes,
and the allocator-layout facet can layer onto the same attribute later.

### Test rigor (all green, toolchain-free `cargo test`, warning-clean)
1. **Escape unit tests** — `deterministic_accepts_a_par_for`; `deterministic_rejects_raw_concurrent`;
   `deterministic_rejects_atomics`.
2. **Real-thread proof** (`--features c-oracle`) — `deterministic_demo`: the certified
   sum-of-squares runs → `385`.

**Demo:** `examples/std/deterministic.jtr` → `385`.

---

## Increment 8 — `select` over channels (CSP ergonomics) ✅

**What it is.** `select { recv(ch) => x { … } … }` waits on several channels and runs the arm of
whichever has a value ready — the Crystal/Go "share by communicating" surface, here over
Jestyr's *move-only* `Channel(i64)`. It's the ergonomic capstone of the channel work.

```
select {
    recv(a) => x { total = total + x }
    recv(b) => y { total = total + y }
}
```

**How it lowers (`cgen::emit_select`).** Hoist each channel to a local (evaluated once), then spin
on a done-flag loop with an `else if` chain so exactly one ready arm fires per pass:
```
{ Channel_i64 _sel0 = a; Channel_i64 _sel1 = b; int _seldone = 0;
  while (!_seldone) {
    if      (channel_len_i64(_sel0) > 0) { int64_t j_x = channel_recv_i64(_sel0); …; _seldone = 1; }
    else if (channel_len_i64(_sel1) > 0) { int64_t j_y = channel_recv_i64(_sel1); …; _seldone = 1; }
  } }
```
The lowered code calls **non-generic** `channel_len_i64`/`channel_recv_i64` wrappers added to
`std/sync.jtr` — so cgen needn't synthesize a monomorphized generic call (the same constraint
that fixed `par_reduce` to `i64`). New `ExprKind::Select` + `SelectArm` thread all six files; new
`select` keyword + contextual `recv`.

**Scope (v1).** Single-consumer (the `len > 0` then `recv` is race-free when this is the only
receiver), recv-only, `Channel(i64)`. Forbidden inside a `@deterministic` function — a
`select`'s choice depends on the schedule. Multi-type / send-arms / multi-consumer are future
work.

### Test rigor (all green, toolchain-free `cargo test`, warning-clean)
1. **Parser** — `parses_select_with_recv_arms`.
2. **Typeck reject** — `select_rejects_a_non_channel_arm` (an `i64` arm errors).
3. **cgen lowering** — `lowers_select_to_a_poll_loop` (done-flag wait loop + the i64 wrappers).
4. **Real-thread proof** (`--features c-oracle`) — `select_demo`: two spawned producers fill two
   channels, the main thread drains all four via `select` → `66`, ×8.

**Demo:** `examples/std/select.jtr` → `66`.

---

## Deferred (with reasons — do NOT build here)
- **Green-thread / fiber (M:N) scheduler** — a large hidden runtime that fights the
  no-ambient-machinery / deterministic thesis. Jestyr stays OS-thread + structured scope.
- **Effect-polymorphic / colorless async** — design §10.3 flags it as explicitly unsolved.
  `await` on a joined task result is fine; a full async effect system is separate research.
- **Lock-free general data structures** — the determinism + disjoint-region model already
  covers the parallel-reduction use case race-free; weak thesis-fit for now.

## Up next (nice-to-have — the core roadmap is complete)
- `spawn` of closures (today's targets are direct named calls); multi-type / send-arm /
  multi-consumer `select`; a `@deterministic` *block* (today it's a function attribute). Q's
  `with schedule(threads, chunk)` split builds directly on dynamic-N spawn.
