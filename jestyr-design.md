# Jestyr — A Language Design Document

*A low-level systems language synthesizing lessons from C, Rust, Zig, Odin, D, C++, Swift, Ada/SPARK, Cyclone, ATS, and Vale.*

Status: **Design draft v0.1** — a research-driven proposal, not a specification.

---

## 1. Thesis

> **Jestyr is a systems language built on mutable value semantics: a world where data has a single owner, references are *borrowed capabilities* that mostly never escape, and the rare reference that must outlive a call pays an explicit, visible price. Safety is tiered, allocation is first-class and never hidden, and the machine is always in view.**

The bet behind Jestyr is that most of Rust's borrow-checker friction comes not from *ownership* but from *escaping references* — references stored in structs, returned from functions, and threaded through data structures. Lifetimes are the bookkeeping that makes escaping references sound. If you make references **second-class by default** (passable down the stack, but not freely storable), the bookkeeping largely evaporates, and you can spend your remaining complexity budget on the genuinely hard 10%.

Jestyr keeps Rust-class performance and control, but trades Rust's *uniform* ownership discipline for a *tiered* one: cheap-and-static for the common case, checked-at-runtime for the escape hatch, raw-and-unsafe for the metal.

---

## 2. What Jestyr Is and Is Not

**Jestyr is:**

- A compiled, statically typed, ahead-of-time systems language with no required runtime.
- Memory-safe by default, with an explicit and auditable unsafe boundary.
- Allocation-explicit: every heap allocation flows through a visible allocator.
- Close to the machine: predictable layout, no hidden dispatch, no hidden allocation, no hidden control flow.
- Correctness-oriented: contracts, refinement types, and an optional formal-verification subset.
- Designed for fast compilation and teaching-quality diagnostics.
- Small enough to hold in your head, but expressive enough to write an OS, a database, a game engine, or firmware.

**Jestyr is not:**

- Not garbage-collected, and not reference-counted by default.
- Not a Rust reskin: it replaces lifetimes-everywhere with value semantics + tiered references.
- Not a C++ successor by accumulation: it refuses feature sprawl and has no inheritance, no implicit conversions, no overload-resolution labyrinth.
- Not dynamically typed at the boundary, not duck-typed in generics (unlike Zig comptime, constraints are checked at definition).
- Not an exceptions language: no unwinding by default, no hidden throw paths.
- Not a "safe scripting" language: `unsafe` exists, is honest, and is necessary.
- Not opinion-free: it has defaults and a point of view, like Odin and Go.

---

## 3. Design Principles

1. **Single ownership, value semantics.** Every value has exactly one owner (its *custodian*). Assignment moves; copying is explicit for non-trivial types. Aliasing of mutable state is the exception you opt into, not the default you opt out of.
2. **References are capabilities, not addresses.** A borrow grants the *right to read or mutate* for a bounded duration. Most borrows are second-class and never need naming.
3. **No hidden costs.** No hidden allocation, no hidden dispatch, no hidden control flow, no hidden copies of non-trivial types, no hidden runtime. If it costs something, you can see it in the source.
4. **Explicit where it matters, quiet where it doesn't.** Low-level behavior (layout, allocation, aliasing, effects) is explicit. Boilerplate that carries no information (most lifetimes, most type annotations on locals) is inferred away.
5. **Tiered safety.** You choose the cost/guarantee tradeoff per use site: static-and-free, checked-and-cheap, or raw-and-unsafe — and the choice is visible at the use site.
6. **One metaprogramming mechanism.** Compile-time evaluation (`comptime`) subsumes templates, macros, and generics. There is no second meta-language.
7. **Correctness is a first-class feature, not a linter.** Contracts and refinement types are part of the type system, checked statically where decidable and at runtime where not, and erasable in release.
8. **Errors teach.** Diagnostics explain the rule, point at the fix, and link deeper docs — without lecturing on every line.
9. **Compile fast.** Compilation speed is a language-design constraint, not a backend afterthought. Features that would tax the compiler (textual includes, unconstrained template instantiation, global type inference) are rejected on those grounds.
10. **The standard library is layered and honest.** `core` assumes nothing; `std` assumes an allocator and an OS. Collections take allocators. Nothing in `core` allocates behind your back.

---

## 4. Memory and Ownership Model

This is the heart of Jestyr. It is built from four ideas, drawn from four traditions, fused into one model.

### 4.1 The four pillars

| Pillar | Source of inspiration | Jestyr's adaptation |
|---|---|---|
| **Single ownership / affine moves** | Rust, C++ move semantics | Values move by default; `Copy` is a property, not a default; destructors (`drop`) run deterministically. |
| **Mutable value semantics** | Hylo/Val, Swift | Mutation happens through *passing conventions*, not through stored aliases. No two mutable paths to the same object exist at once — enforced structurally, not by a separate borrow checker pass. |
| **Generational references** | Vale | The escape hatch for *stored* references: a fat reference carrying a generation tag, checked cheaply at runtime. Safe, but not free. |
| **Regions** | Cyclone, ATS | The escape hatch for *zero-cost* stored references and bulk lifetime reasoning: an explicit, named region whose contents share a lifetime, freed all at once. |

### 4.2 Custody: who owns what

A binding either **owns** a value or **borrows** it.

```jestyr
let buf = Buffer.with_capacity(alloc, 4096)   // `buf` is the custodian of a Buffer
let twin = buf                                // ERROR: move out of `buf`; `buf` is now invalid
let twin = buf.clone(alloc)                   // explicit deep copy; both valid
```

- **Move is the default** for non-`Copy` types. Use-after-move is a compile error.
- `Copy` is a trait a type *has* (trivially copyable, no destructor, e.g. `i32`, `Point`). Copy types are duplicated implicitly; everything else moves.
- When a custodian goes out of scope, its `drop` runs (RAII, deterministic, C++/Rust style). No GC, no finalizer thread.

### 4.3 Passing conventions: the elegant core

Functions declare *how* they take each parameter. This single mechanism replaces `&`, `&mut`, lifetimes-in-signatures, and move-vs-borrow ambiguity.

| Convention | Meaning | Cost | Rust analog |
|---|---|---|---|
| `read x: T` | Immutable borrow for the call's duration | zero | `x: &T` |
| `mut x: T` | Exclusive mutable borrow for the call's duration | zero | `x: &mut T` |
| `take x: T` | Consumes ownership (sink) | move | `x: T` (by value) |
| `out x: T` | Uninitialized on entry, initialized on return | zero | `x: &mut MaybeUninit<T>` |

```jestyr
fn area(read r: Rect) -> f64 { r.w * r.h }          // borrows, cannot mutate, cannot store r
fn scale(mut r: Rect, by: f64) { r.w *= by; r.h *= by }
fn consume(take s: String) -> Bytes { s.into_bytes() }  // s is gone at the call site
fn make(out p: Point) { p = Point{ x: 0, y: 0 } }       // out-parameter, no return copy
```

The crucial property: **`read`/`mut`/`out` references are second-class.** They can be passed *further down* the call stack, but they cannot be:
- stored in a struct field,
- returned from the function,
- captured by an escaping closure,
- put into a collection.

Because second-class references provably never outlive the call frame that created them, **they need no lifetime annotations at all.** The compiler's check is a local, near-linear analysis ("does this borrow escape?"), not a whole-program lifetime inference. This is the single biggest ergonomic departure from Rust.

Exclusivity (the "one mutable path" rule) also falls out structurally: while a value is lent as `mut`, the lender cannot touch it, and you cannot form a second `mut` to the same value, because there is no way to *name* a stored alias. No `RefCell`, no interior-mutability dance for the common case.

> **Lesson adapted:** Rust proves *non-aliasing of escaping references* with lifetimes. Jestyr *prevents escape* so the proof is unnecessary. Hylo/Val showed this is viable; Jestyr adds tiers for when you genuinely need escape.

### 4.4 When references must escape: the two escape hatches

Real systems need stored references: a parent pointer, a cache that hands out handles, a graph, an observer list. Jestyr offers two opt-in tiers, each visible at the use site.

**Tier 2a — Generational references (`&T`), safe and cheap.**
Inspired by Vale. A `&T` is a *fat* reference: a pointer plus a generation tag. The referent's allocation carries a generation counter; freeing it bumps the counter. Dereferencing checks that the reference's tag still matches.

```jestyr
struct Node {
    value: i32,
    parent: ?&Node,        // an optional stored reference into the same arena
}
```

- Cost: one extra word per reference, one compare-and-branch per *first* deref in a scope (the check hoists; repeated derefs in a tight loop check once).
- Guarantee: use-after-free becomes a deterministic, catchable fault (`panic` in safe builds, trap on embedded), never undefined behavior.
- You opt in by typing `&T` for a *stored* reference; the compiler knows it must be checked.

**Tier 2b — Region references (`&[r] T`), zero-cost, statically proven.**
Inspired by Cyclone/ATS. When you can prove (or are willing to assert via region scoping) that a set of objects shares one lifetime, you put them in a named region and take zero-cost references that the compiler proves cannot dangle.

```jestyr
region r {
    let arena = Arena.in(r)
    let a = arena.alloc(Node{ value: 1, parent: none })
    let b = arena.alloc(Node{ value: 2, parent: some(a) })   // &[r] Node, no generation tag
    process(a, b)
}   // entire region freed at once; a, b, and all &[r] references die together, checked at compile time
```

Region references carry a *region name* (an ordinary identifier, not a tick-mark) only when a function must relate two regions; elision handles the common case. They are the answer for arena-heavy, data-oriented code where generational tags would be wasteful.

> **Why this is more elegant than Rust:** Rust forces every long-lived reference through the *same* lifetime machinery whether it's a transient borrow or a graph edge. Jestyr matches the mechanism to the need: nothing for transient borrows, a tag for irregular graphs, a region for bulk arenas, a raw pointer for the metal.

### 4.5 The escape ladder (cost is always visible)

```
value (owned)        →  no aliasing, you hold the data
read/mut/out param   →  second-class borrow, zero cost, no annotation, cannot escape
&[r] T  (region)     →  first-class, zero cost, compile-time proven, arena-scoped
&T      (generational)→ first-class, ~1 word + 1 branch, runtime-checked, general graphs
*T / *mut T (raw)    →  unsafe, zero cost, zero guarantee, for FFI and the metal
```

Reading a type tells you its safety/cost tier. There are no invisible reference kinds.

### 4.6 The classic hard cases

- **Doubly linked lists / graphs / parent pointers:** arena + `&T` generational references, or arena + integer indices (the data-oriented idiom Jestyr's stdlib encourages). Region references when the lifetime is bulk-uniform.
- **Self-referential structs:** disallowed by value (moves would invalidate internal pointers); expressed via arena + handles, or pinned allocation in `unsafe` for the rare case.
- **Caches / observer registries:** generational references; a stale entry faults deterministically instead of dangling.
- **Cyclic ownership:** broken by construction — ownership is a tree. Cycles live in arenas/regions and are freed in bulk, sidestepping the need for cycle-collecting reference counts.

---

## 5. Pointer / Reference Model

Jestyr distinguishes **references** (safe, checked or proven) from **pointers** (raw, unsafe), and never conflates them.

| Type | Safety | Nullable | Aliasing | Use |
|---|---|---|---|---|
| `T` | owned value | n/a | single custodian | normal data |
| `read/mut/out` param | safe, second-class | no | exclusive while `mut` | passing data into calls |
| `&[r] T` | safe, region-proven | via `?&[r] T` | shared read / exclusive write | arena graphs |
| `&T` | safe, generational | via `?&T` | shared read / exclusive write | general stored refs |
| `*T`, `*mut T` | unsafe | yes | unchecked | FFI, MMIO, allocators |
| `[]T` (slice) | safe, fat (ptr+len) | via `?[]T` | bounds-checked | arrays, buffers |
| `^T` | unsafe single-element raw, C-pointer-shaped | yes | unchecked | C ABI scalars |

Design choices:

- **No null in the safe world.** Absence is `?T` (optional). `none`/`some(x)`, pattern-matched. Null exists only behind raw pointers.
- **Slices are fat and bounds-checked** by default; checks are elidable in release or where a refinement type proves the index in range (see §7).
- **Provenance is explicit.** Converting between `&T`, `*T`, and integers requires `unsafe` and a named operation, so the compiler's aliasing model is never silently violated (a deliberate answer to C's provenance and Rust's Stacked/Tree-Borrows headaches — see open questions).
- **No pointer arithmetic on safe references.** Raw pointers support arithmetic; references do not.

---

## 6. Error Handling Model

Jestyr rejects exceptions (hidden control flow) and rejects forcing every error into a heap-allocating boxed trait object. It synthesizes **Zig's error sets** with **Rust's `?` ergonomics** and **Ada's contracts**, splitting "expected failures" from "bugs."

### 6.1 Two categories, deliberately separated

- **Errors** are expected, recoverable outcomes (file not found, parse failure, timeout). They are values, propagated explicitly.
- **Faults** are bugs and broken invariants (index out of bounds, failed contract, generational-reference violation). They abort the current *failure domain* (by default the thread/task), are not catchable as ordinary control flow, and in release-with-checks-off may be UB-free traps. This mirrors Rust's panic/Result split and Ada's distinction between exceptions and assertions, made crisp.

### 6.2 Error sets and `!`

An error type is a *set of tags* (a lightweight, closed-or-open sum), not a user-defined enum you must declare up front.

```jestyr
fn read_config(read path: Path) -> Config !{ Io, Parse } {
    let text = fs.read_to_string(alloc, path)?      // propagates Io
    let cfg  = parse(text)?                          // propagates Parse
    return cfg
}
```

- `T !E` is the type "produces `T` or fails with an error in set `E`."
- `?` (the *try* operator) propagates the error, widening the caller's error set automatically. Error sets **infer and compose** — you rarely write them out; the compiler unions them.
- You can name and document a set when it's part of a public API:

```jestyr
error FsError = { NotFound, Permission, Io }
```

- **Typed propagation, no boxing.** Error sets are integer-tag-sized unless they carry payloads; no heap allocation, no dynamic dispatch. Contrast with `Box<dyn Error>`.
- **Error return traces** (Zig-style): in debug builds, a propagated error accumulates a cheap trace of where it traveled, without unwinding machinery.

### 6.3 Handling

```jestyr
match read_config(path) {
    ok(cfg)        => run(cfg),
    err(NotFound)  => run(Config.default()),
    err(e)         => log.fatal("config: {e}"),
}

let cfg = read_config(path) catch Config.default()   // Swift-ish shorthand for a default
let cfg = read_config(path) catch |e| return e        // explicit propagate
```

### 6.4 Contracts and faults (Ada/SPARK influence)

```jestyr
fn sqrt(x: f64) -> f64
    requires x >= 0.0
    ensures  result >= 0.0
{ ... }
```

- `requires`/`ensures`/`invariant` are part of the signature/type.
- In **debug**, they compile to checks that raise faults.
- In **release**, they are elided — *unless* the function is marked `@verified`, in which case the compiler must prove them statically (SPARK-style) and they cost nothing because they're proven, not checked.
- This gives a smooth ladder: assertion → checked contract → proven contract, same syntax.

> **Lesson adapted:** Zig's error sets are great but duck-typed and untyped at the boundary; Jestyr keeps their lightness but gives them inference + optional naming + typed propagation. Ada's contracts are powerful but verbose and runtime-only without SPARK; Jestyr folds them into the signature and makes verification an opt-in tier.

---

## 7. Type System Direction

Static, strong, inferred locally (bidirectional, never global Hindley-Milner across function boundaries — for compile speed and good errors). The flavor is **algebraic + refinement**, no inheritance.

### 7.1 Building blocks

- **Structs** (product types) with explicit layout control (`@layout(c)`, `@packed`, `@align(n)`).
- **Enums** (sum types / tagged unions) with exhaustive `match`. Payload-carrying, niche-optimized (`?&T` is one word).
- **Unions** (untagged, `unsafe` to read) for C interop and manual layout.
- **Tuples** and anonymous structs for lightweight grouping.
- **Slices `[]T`**, **arrays `[N]T`**, **fixed strings**, all value types.
- **Distinct/newtypes** for zero-cost strong typing (Ada, Odin): `type Meters = distinct f64` — not interchangeable with `f64` or `Seconds`.

### 7.2 Refinement and subrange types (Ada + ATS, made digestible)

```jestyr
type Percent = i32 in 0..=100
type NonEmpty[T] = []T where len > 0
type Even = i32 where value % 2 == 0
```

- Refinements are checked at the boundary where a wider type narrows (construction, assignment), statically when the compiler can prove it, with an inserted check otherwise.
- They flow into contracts and into **bounds-check elision**: indexing `arr[i]` with `i: i32 in 0..len(arr)` needs no runtime check.
- This is *lightweight dependent typing* — deliberately less powerful than ATS's full dependent types (which are expert-only) but covering the 90% that matters for systems code: ranges, lengths, non-null, non-zero.

### 7.3 Polymorphism: traits, no inheritance

- **Traits** (interfaces / Swift protocols / Rust traits) describe behavior. Types *implement* traits; there is no subclassing, no base-class fragility.
- **Static dispatch by default** (monomorphized, zero-cost).
- **Dynamic dispatch is explicit and visible:** `dyn Trait` is a fat pointer (data + vtable). You never get a vtable you didn't ask for.
- **No implicit conversions, no operator-overload free-for-all.** Operators map to traits (`Add`, `Ord`, …) but overloading is constrained and never changes evaluation order or introduces temporaries you can't see.

### 7.4 What's deliberately excluded

- No inheritance, no virtual-by-default, no implicit numeric coercions, no exceptions-in-the-type-system, no global type inference, no higher-kinded types in v1 (revisit later), no implicit `Deref` coercion chains.

---

## 8. Generics and Metaprogramming

**One mechanism: `comptime`.** Compile-time evaluation is the substrate for generics, constants, reflection, and code generation. There is no separate template language and no separate macro language. This is Zig's best idea, with Zig's biggest weakness fixed.

### 8.1 Types are comptime values

```jestyr
fn List(comptime T: type) -> type {
    return struct {
        items: []T,
        len:   usize,
        cap:   usize,
    }
}

let xs: List(i32) = List(i32).empty()
```

A "generic type" is a `comptime` function returning a `type`. A "generic function" is a function with `comptime` parameters. Constant folding, lookup tables, and config-driven code generation use the *same* evaluator.

### 8.2 The fix for Zig: constraints checked at definition

Zig's comptime generics are duck-typed — errors surface deep inside instantiation, with poor messages. Jestyr adds **explicit, definition-site-checked constraints** (Rust's trait bounds, Swift's `where`):

```jestyr
fn max[T](read a: T, read b: T) -> T
    where T: Ord
{ if a > b { a } else { b } }
```

- With a `where` bound, the body is type-checked **against the constraint, once, at definition** — so the error blames *your* generic code, not the caller's instantiation. This directly serves the "errors that teach" goal.
- Without a bound, you can still drop to raw comptime duck-typing for the rare meta-heavy case, but you opt into the worse error messages knowingly.

### 8.3 Reflection and codegen, bounded

- **Compile-time reflection:** iterate fields, read type info, generate serializers — all in `comptime`, all in normal Jestyr (no string-pasting macros).
- **Quasiquote/AST building** exists as a *restricted `comptime` library*, not a separate hygienic-macro sublanguage, and is the last resort. This is a deliberate guard against the D/C++/Rust "macro and template are different universes" sprawl.
- **No `#include`, no text substitution, no preprocessor.** Ever.

> **Lesson adapted:** C++ has templates *and* macros *and* `constexpr` *and* concepts; D has templates *and* mixins *and* CTFE *and* `static if`; Rust has generics *and* `macro_rules!` *and* proc-macros. Jestyr collapses this to **comptime + trait bounds**, accepting that a few exotic patterns get slightly more verbose in exchange for one mental model.

### 8.4 The compile-speed tax, acknowledged

Monomorphization and comptime both cost build time. Jestyr's mitigations:
- Comptime runs in a fast **bytecode interpreter**, cached across builds (not re-evaluated every compile).
- Monomorphized instantiations are **cached and deduplicated** content-addressably.
- `dyn Trait` is the escape valve when you'd rather erase than monomorphize (smaller binaries, faster builds, explicit cost).

---

## 9. Module and Package System

Optimized for fast compilation and zero ceremony. Inspired by Odin (directory = package) and Zig (no headers), explicitly rejecting Rust's `mod` tree friction and C's textual includes.

- **A directory is a module.** All `.jestyr` files in a directory share a namespace; no per-file `mod` declarations, no re-export plumbing to wire files together.
- **No header files, no forward declarations.** Top-level declarations are order-independent within a module (C's two-pass pain, removed).
- **Explicit imports** by module path; nothing is in scope unless imported. No glob-by-default.
- **No circular module dependencies** — enforced, which keeps the dependency graph a DAG and enables parallel + incremental compilation.
- **The build is described in Jestyr itself** (a `build.jestyr`, à la `build.zig`) — same language, comptime-driven, no separate DSL — *plus* a simple declarative `manifest` for the dependency list so tooling can resolve packages without executing arbitrary build code.
- **Content-addressed, vendored-by-default dependencies** with a lockfile. Reproducible builds; no network at build time unless fetching.
- **Visibility** is `pub` (module-public) vs default (module-private). Coarse and predictable; no four-level visibility lattice.

---

## 10. Concurrency Model

Jestyr's value-semantics foundation does a lot of the work for free, and the language is honest about what remains hard.

### 10.1 Data-race freedom falls out of the model

Because mutable state has no stored aliases (§4.3), **sharing mutable data across threads requires an explicit, typed handoff.** There is no ambient way to alias a mutable object from two threads.

- A value is **sendable** (movable to another thread) if it owns its data and holds no thread-bound resources — derived automatically from its structure, like Rust's `Send`, but mostly inferred rather than annotated.
- **Sharing** read-only data across threads is free. Sharing *mutable* data requires a synchronization type (`Mutex[T]`, `Atomic[T]`, channel), and the type system tracks it — you cannot reach the inner `T` of a `Mutex` without locking.

### 10.2 Structured concurrency

Concurrency is **scoped**: tasks are spawned into a nursery/scope and cannot outlive it (Swift task groups, Trio's nurseries). No detached fire-and-forget by default; no leaked tasks.

```jestyr
scope concurrent (s) {
    let a = s.spawn(|| fetch(url_a))
    let b = s.spawn(|| fetch(url_b))
    let (ra, rb) = await (a, b)
}   // scope joins all tasks here; errors propagate out as values
```

### 10.3 Async without forced coloring (the honest part)

- **Synchronous by default.** Blocking I/O is a normal call; no `async` keyword needed for ordinary code.
- **Async is an explicit capability** backed by a user-chosen executor (no built-in runtime — honors "no required runtime"). Async I/O uses stackless coroutines.
- To fight function-coloring, Jestyr aims for **effect-polymorphic functions**: a routine can be generic over whether it runs sync or async (a "keyword-generic"-style parameterization), so libraries don't fork into sync and async copies.
- **This is flagged as not-yet-solved.** Colorblind async is an active research area (Zig tried and removed it); Jestyr commits to *not* baking coloring into the syntax prematurely, and treats the final mechanism as open (see §17).

### 10.4 No hidden runtime

- Threads map to OS threads. No green-thread scheduler is imposed.
- An executor is a library you link, instantiate, and pass — visible, swappable, omittable on bare metal.

---

## 11. Unsafe and Low-Level Escape Hatches

The unsafe boundary is explicit, narrow, and auditable — the thing C lacks and Rust mostly gets right.

- **`unsafe { … }` blocks** enable: raw pointer deref and arithmetic, `union` field reads, `*T ↔ &T ↔ usize` conversions, calling `unsafe`/`extern` functions, inline assembly, volatile/MMIO access, and bypassing refinement/bounds checks.
- **`unsafe fn`** marks a function whose *contract* the caller must uphold; calling it requires `unsafe`.
- **`trusted` blocks** are a distinct, stronger marker: "this unsafe code is asserted to uphold the safety contract that the surrounding safe abstraction promises." Splitting `unsafe` (raw operation) from `trusted` (audited assertion) makes the *audit surface* explicit and grep-able — the thing you most want to review.
- **`@volatile`, `@align`, `@packed`, placement at fixed addresses**, and **inline `asm`** are first-class for drivers and MMIO.
- **A documented aliasing model.** Jestyr specifies what raw pointers may assume (a single, teachable model — learning from the Stacked/Tree Borrows saga that "we'll define it later" is a mistake). The model is conservative and explicit so `unsafe` code can be written *correctly*, not just compile.

---

## 12. C Interop Story

A headline feature, not a binding generator bolted on. Inspired by Zig's `@cImport` and Odin's pragmatic FFI.

- **Consume C headers directly.** `import c "stdio.h"` translates the header at `comptime` into Jestyr declarations — no hand-written bindings, no `bindgen` step.
- **C ABI is native.** `extern "c"` on declarations; `@layout(c)` structs match C layout, alignment, and padding exactly; enums and bitfields map predictably.
- **Export to C.** `pub extern "c" fn …` produces unmangled, C-callable symbols and an auto-generated header, so Jestyr can be a drop-in C library.
- **No name mangling on the extern boundary**, predictable symbol names, predictable struct layout — so other languages' FFI sees Jestyr as "just a C library."
- **`^T` and `*T`** are the C-pointer-shaped types; `[]T` lowers to `(ptr, len)` only across `extern` if you ask, otherwise you pass `ptr`/`len` explicitly to match C signatures.
- **errno, varargs, `void*`, opaque handles** all have explicit, documented representations.

> The bar: porting a C project file-by-file, calling into the not-yet-ported half with zero glue, must be smooth. C interop is a migration path, not just a feature.

---

## 13. Embedded and Bare-Metal Story

Jestyr targets microcontrollers and kernels as first-class citizens.

- **`core` needs nothing** — no OS, no allocator, no `std`. Freestanding targets link only `core`.
- **No hidden allocation, ever.** Allocators are explicit (§14), so a no-heap firmware build simply never constructs one; anything that needs the heap won't compile in.
- **`@no_panic` mode:** a function or module can forbid faults; if the compiler can't prove a path is fault-free (e.g., an un-elided bounds check), it's a *compile error*. Critical for hard-real-time and safety-critical firmware.
- **MMIO as typed registers** generated at comptime from a peripheral description; volatile and bit-field access are explicit and checked.
- **Interrupt handlers, custom linker sections, fixed-address placement, packed register structs** are first-class.
- **Stack-usage analysis** (Ada/SPARK influence): the compiler can report worst-case stack depth for `@no_recursion` code, essential where there's no MMU.
- **Trivial cross-compilation** (Zig's superpower): the compiler ships the backends and target definitions; `--target thumbv7em-none` just works, no host toolchain hunting.
- **Deterministic everything:** no GC pauses, no hidden runtime threads, generational-reference faults become hardware traps you can route.

---

## 14. Standard Library Philosophy

Layered, allocator-explicit, small, data-oriented.

- **`core`** — types, traits, slices, `Option`/`Result`, math, atomics, comptime utilities. *No allocation, no OS.* Usable on bare metal.
- **`std`** — collections, I/O, filesystem, threads, allocators, formatting. Assumes an allocator and (usually) an OS.
- **Allocators are first-class** (Zig/Odin): every allocating API takes an `Allocator`. The stdlib ships arena, pool, fixed-buffer, page, and general-purpose allocators, plus a debug-instrumented one that detects leaks and double-frees.
- **An ambient allocator *context*** (Odin's `context.allocator`) reduces threading boilerplate: there's a current allocator you can rely on, but it is **passed as an explicit, visible part of the call convention** — convenient *and* honest, never a hidden global.
- **Data-oriented support is built in:** struct-of-arrays helpers, explicit-SoA collections, handle/index-based containers, and arena-friendly data structures, because the ownership model already pushes you toward arenas + indices.
- **Small and orthogonal.** No two-runtime split (D's lesson), no kitchen-sink STL (C++'s lesson). The surface stays auditable.
- **`format`/`print` do no hidden allocation** in `core`; formatting into a fixed buffer is always available.

---

## 15. Toolchain Goals

One tool, fast, teaching-quality.

- **A single `jestyr` binary**: compiler, build system, package manager, formatter, test runner, doc generator, language server (Cargo/Zig/Go unification — a massive ergonomic multiplier).
- **Fast compilation is a first-class metric.** Incremental, parallel (DAG modules), cached comptime, deduplicated monomorphization, and a **fast debug backend** (own codegen or Cranelift-class) alongside **LLVM for release**. Build speed is tracked in CI like a test.
- **Cross-compilation built in**, backends and target defs shipped with the toolchain.
- **Teaching diagnostics:** every error has a code, a one-paragraph explanation of the *rule*, a pointer to the offending value's history (e.g., "moved here, used here"), and a suggested fix — but only *one* primary message per error, no cascading noise. Errors should make the programmer better, not defensive.
- **Built-in test + bench + doc + fmt.** Tests live next to code; docs are extracted and runnable.
- **Optional verification mode:** `@verified` functions are discharged by an SMT backend; failures are reported as counterexamples, not walls of solver output. (Ambitious; scoped — see §17.)
- **Debugger-first:** clean DWARF, stable layouts in debug, pretty-printers shipped.

---

## 16. Example Syntax Sketches

> Syntax is illustrative, chosen for readability (Odin/Swift influence) with systems-grade explicitness. `fn` declares functions; `let`/`var` bind immutably/mutably; `->` returns; `?T` is optional; `T !E` is fallible.

### 16.1 A generic, allocator-aware dynamic array

```jestyr
fn Vec(comptime T: type) -> type {
    return struct {
        ptr: *mut T,
        len: usize,
        cap: usize,

        fn empty() -> Self { Self{ ptr: null, len: 0, cap: 0 } }

        fn push(mut self, alloc: Allocator, take value: T) !{ OutOfMemory } {
            if self.len == self.cap {
                self.grow(alloc)?
            }
            unsafe { (self.ptr + self.len).* = value }   // explicit raw store
            self.len += 1
        }

        fn get(read self, i: usize in 0..self.len) -> read T {
            // refinement on `i` elides the bounds check
            unsafe { (self.ptr + i).* }
        }

        fn drop(take self, alloc: Allocator) {
            alloc.free(self.ptr, self.cap)
        }
    }
}
```

### 16.2 Enums, pattern matching, errors

```jestyr
enum Shape {
    circle(r: f64),
    rect(w: f64, h: f64),
    none,
}

fn area(read s: Shape) -> f64 {
    match s {
        circle(r)   => 3.14159 * r * r,
        rect(w, h)  => w * h,
        none        => 0.0,
    }
}

error ParseError = { Empty, BadDigit, Overflow }

fn parse_u32(read s: []u8) -> u32 !ParseError {
    if s.len == 0 { return err(Empty) }
    var acc: u32 = 0
    for c in s {
        if c < '0' or c > '9' { return err(BadDigit) }
        acc = acc.checked_mul(10).checked_add(c - '0') catch return err(Overflow)
    }
    return ok(acc)
}
```

### 16.3 Contracts and a verified function

```jestyr
@verified
fn clamp(x: i32, lo: i32, hi: i32) -> i32
    requires lo <= hi
    ensures  result >= lo and result <= hi
{
    if x < lo { lo } else if x > hi { hi } else { x }
}
```

### 16.4 Region-scoped arena graph (zero-cost stored references)

```jestyr
fn build_tree() {
    region r {
        let a = Arena.in(r)
        let root = a.new(Node{ value: 0, kids: a.slice(Node, 2) })
        root.kids[0] = Node{ value: 1, kids: a.empty() }   // &[r] Node, no tag, no check
        root.kids[1] = Node{ value: 2, kids: a.empty() }
        walk(root)
    }   // whole arena freed; references proven dead at compile time
}
```

### 16.5 Generational reference (general graph, runtime-checked)

```jestyr
struct Observer { name: String }

struct Subject {
    watchers: Vec(&Observer),     // stored references → generational, checked on deref
}

fn notify(read s: Subject) {
    for w in s.watchers {
        match w.try_read() {                 // returns ?read Observer
            some(o) => log.info("notify {o.name}"),
            none    => {}                     // watcher was freed; deterministic skip, not UB
        }
    }
}
```

### 16.6 MMIO register, bare metal

```jestyr
@layout(c) @packed
struct UartRegs {
    data:   @volatile u32,
    status: @volatile u32,
    ctrl:   @volatile u32,
}

const UART0: *mut UartRegs = @address(0x4000_C000)

fn putc(c: u8) {
    unsafe {
        while (UART0.status.* & TX_FULL) != 0 {}
        UART0.data.* = c as u32
    }
}
```

### 16.7 C interop

```jestyr
import c "math.h"

fn hypot_demo(a: f64, b: f64) -> f64 {
    return c.sqrt(a*a + b*b)      // calls libc directly, no binding shim
}

pub extern "c" fn jestyr_add(a: i32, b: i32) -> i32 {  // exported, C-callable
    return a + b
}
```

### 16.8 Structured concurrency

```jestyr
fn fetch_all(read urls: []Url) -> Vec(Response) !NetError {
    scope concurrent (s) {
        var handles = Vec(Task(Response !NetError)).empty()
        for u in urls { handles.push(alloc, s.spawn(|| http.get(u))) }
        return collect(await handles)?     // joins at scope end; first error propagates
    }
}
```

---

## 17. Major Tradeoffs

| Decision | What you gain | What you give up |
|---|---|---|
| Second-class references by default | ~No lifetime annotations; local, fast checks; gentle learning curve | Can't freely return/store references without opting into a tier |
| Generational references for stored refs | Safe graphs/caches without a borrow checker | A word of memory + a branch per deref; not zero-cost |
| Regions for bulk lifetimes | Zero-cost arena references, great for data-oriented code | Coarser lifetimes; you must structure data into arenas |
| Comptime as the only metaprogramming | One mental model; immense power; no macro/template split | Build-time cost; a few exotic patterns are verbose |
| Constraints checked at definition | Teaching-quality generic errors | Slightly less flexible than pure duck-typed comptime |
| Contracts + refinement types | Correctness ladder up to formal proof; bounds-check elision | Runtime cost in debug; verification is hard and scoped |
| No inheritance, no exceptions | Predictable layout/control flow, simpler model | Some OOP/exception idioms must be re-expressed |
| Explicit allocators everywhere | No hidden allocation; embedded-friendly | More parameters (mitigated by allocator context) |
| Monomorphization by default | Zero-cost generics, fast runtime | Build time and binary size (mitigated by `dyn`, caching) |
| Small surface, opinionated defaults | Holds in your head; fast to learn | Won't satisfy every "but my language has X" |

---

## 18. Risks and Open Research Questions

1. **How far do second-class references actually go?** MVS is proven in research (Hylo) but unproven at OS/database scale. Where exactly do users hit the wall and reach for a tier — and is that boundary teachable? *Needs real programs, not thought experiments.*
2. **Colorblind async.** Effect-polymorphic sync/async functions are not a solved problem (Zig tried, retreated). Can Jestyr deliver non-viral async without a runtime and without coloring — or does it accept colored async as a pragmatic floor?
3. **Generational references vs. arenas — memory and cache cost.** Fat references and per-allocation generations have real overhead and cache effects. When does the model push you to arenas+indices instead, and can the stdlib make that the path of least resistance?
4. **Region inference and error quality.** Region systems (Cyclone) historically produced confusing errors. Can region elision + diagnostics stay teachable, or do regions become the new lifetimes?
5. **Refinement-type decidability.** Where's the line between "the compiler proves your index in range" and "you get an inscrutable solver timeout"? The refinement fragment must stay decidable and predictable.
6. **Scope of `@verified`.** SMT-backed verification is powerful but brittle at scale (SPARK's experience). Which subset is verifiable in practice, and how are counterexamples surfaced without dumping solver internals?
7. **A teachable aliasing model for `unsafe`.** Rust's Stacked/Tree Borrows shows how hard this is. Jestyr commits to specifying one up front — but *which* model balances optimizer freedom against writability?
8. **Compile speed vs. comptime + monomorphization.** These pull against the fast-compilation goal. Are caching, a comptime interpreter, and `dyn` enough, or is there a deeper tension?
9. **Move-by-default + value semantics + large structs.** Without escaping references, do we generate too many large copies? How well do `mut`/`out` conventions and copy elision keep this zero-cost in practice?
10. **Migration ergonomics from C/Rust.** Is the tiered model intuitive to people coming from `&`/`&mut`, or does "which tier do I use?" become its own learning cliff?

---

## 19. Roadmap for Prototyping

The strategy: **prove the ownership model on real code before building anything else.** Everything in Jestyr is negotiable except the value-semantics core, so the core gets tested first.

- **Phase 0 — Core semantics on paper (and a checker).** Write a small-step operational semantics for ownership, passing conventions, escape analysis, and the two reference tiers. Build a standalone *escape/exclusivity checker* over a toy AST. *Exit:* the model type-checks a hand-written corpus of tricky programs (linked list, cache, graph, iterator) with the expected accept/reject verdicts.
- **Phase 1 — Minimal end-to-end compiler.** Lexer, parser, bidirectional type-checker, ownership checker, lowering to **C** (fastest path to running code) — *no generics, no async, no contracts.* Structs, enums, `match`, slices, `read/mut/take/out`, raw pointers, `unsafe`. *Exit:* port ~5 small real programs (a JSON parser, an arena allocator, a hash map, an "blinky" for a simulator, a CLI) and run them.
- **Phase 2 — Comptime + generics + traits.** Add the comptime interpreter, `comptime`-functions-returning-`type`, trait bounds checked at definition, `dyn`. *Exit:* a generic `Vec`/`HashMap`/`Option` in user space; teaching-quality errors on a misused generic.
- **Phase 3 — Errors, contracts, allocators, `core`.** Error sets + `?` + error traces; `requires`/`ensures` as runtime checks; the allocator interface + arena/pool/fixed-buffer/debug allocators; ship `core`. *Exit:* the corpus is rewritten idiomatically; the debug allocator catches a planted leak.
- **Phase 4 — Regions + generational references.** Implement both stored-reference tiers and their checks; refinement/subrange types with bounds-check elision. *Exit:* a doubly linked list (generational), an AST arena (regions), and a SoA particle system, all safe, benchmarked against C equivalents.
- **Phase 5 — C interop + cross-compilation + `std`.** `import c`, `extern "c"` export with header gen, shipped cross-targets, an LLVM release backend alongside the fast debug backend. *Exit:* call into a real C library and compile a bare-metal target unchanged on three host OSes.
- **Phase 6 — Concurrency + verification + self-host.** Structured concurrency + sync types; prototype effect-polymorphic async (or commit to a pragmatic model); a scoped `@verified` subset over an SMT backend; begin self-hosting the compiler. *Exit:* a self-hosted front-end, a verified `clamp`/`ring-buffer`, and a structured-concurrency download tool.
- **Continuous validation:** at every phase, keep porting representative programs — an allocator, a data-structure library, a device driver, a parser, a tiny kernel — and treat *compile speed* and *diagnostic quality* as tracked, regressible metrics, not aspirations.

---

## Appendix A — Inspiration Sources: Lessons Extracted

**C.** *Keep:* simple, stable ABI; transparent pointer/layout model; fast separate compilation; close-to-the-metal feel; minimal runtime. *Reject:* textual `#include`/preprocessor; undefined-behavior minefields; null-everywhere; no bounds info; manual, error-prone memory with no safety net. *Adapt:* C's predictable layout becomes `@layout(c)`; C's ABI becomes a native target, not an afterthought.

**Rust.** *Keep:* affine ownership/moves, deterministic `drop`/RAII, exhaustive enums + `match`, traits, `Result`+`?`, zero-cost abstraction, honest `unsafe`. *Make more elegant:* lifetimes (replaced by second-class refs + tiers), interior-mutability ceremony (mostly removed by MVS), macro/trait/lifetime learning cliff, `Box<dyn Error>` boxing (replaced by error sets), async coloring (treated as open, not baked in).

**Zig.** *Keep:* explicit allocators, `comptime` as the one metaprogramming tool, trivial cross-compilation, error sets + error return traces, "no hidden control flow," minimal runtime, `@cImport`-style header consumption. *Adapt/fix:* duck-typed comptime → constraints checked at definition for better errors; untyped error boundaries → inferred-but-typed error sets; add a real safety story (Zig is not memory-safe) via the tiered reference model.

**Odin.** *Keep:* directory-as-package simplicity, readability, data-oriented defaults, the allocator *context*, low-friction "just write systems code" ergonomics, distinct types. *Adapt:* the allocator context becomes explicit-but-ambient (visible in the convention); data-oriented helpers become first-class stdlib; keep Odin's lack of ceremony as a north star.

**D.** *Keep:* powerful CTFE and compile-time reflection; clean module system; expressive generics. *Reject:* feature sprawl (templates *and* mixins *and* `static if` *and* two runtimes/GC-by-default); the "too many ways" problem. *Adapt:* fold all metaprogramming into one `comptime` mechanism; refuse to add a second way to do a thing without removing one.

**C++.** *Keep:* RAII/deterministic destructors, move semantics, value types, zero-cost abstraction, templates' power (re-expressed as comptime). *Reject:* accumulated complexity, implicit conversions, overload-resolution + ADL mazes, exceptions-by-default, header model, fragile inheritance, UB sprawl. *Adapt:* RAII via `drop`; moves via custody + `take`; templates via comptime + checked constraints; *no* inheritance.

**Swift.** *Keep:* value semantics as a default worldview, protocol-oriented (no inheritance) design, readable syntax, optionals over null, recent ownership work (`borrowing`/`consuming`), structured concurrency + task groups. *Adapt:* passing conventions (`read/mut/take/out`) generalize Swift's `borrowing/consuming`; structured concurrency model is borrowed almost directly but without a mandatory runtime.

**Ada/SPARK.** *Keep:* strong/distinct typing, subranges, contracts (`requires`/`ensures`), the assertion-vs-exception split, safety-critical discipline, formal verification as an *option*, stack-usage analysis. *Adapt:* subranges → refinement types feeding bounds-check elision; contracts fold into signatures with a debug→checked→`@verified` ladder; SPARK-style proof becomes an opt-in tier, not a separate toolchain.

**Cyclone.** *Keep:* region-based memory management, fat/bounds-checked pointers, non-null pointer types, "safe C." *Adapt:* regions become the zero-cost stored-reference tier (`&[r] T`); fat bounds-checked pointers become safe slices; non-null becomes the default (null only behind raw pointers).

**ATS.** *Keep:* linear types and dependent types for proof-carrying systems code. *Adapt:* take the *digestible* slice — affine moves + lightweight refinement types covering ranges/lengths/non-null — while deliberately *not* importing full dependent types (too expert-only for the language's audience). ATS sets the aspirational ceiling for `@verified`.

**Vale.** *Keep:* generational references as a cheap, safe alternative to a borrow checker for stored references; the insight that hybrid models (static + light runtime checks) can beat all-static for ergonomics. *Adapt:* generational references become exactly Jestyr's Tier-2a escape hatch, sitting between zero-cost regions and unsafe raw pointers — the crux of the "tiered safety" thesis.

---

*End of design draft v0.1.*
