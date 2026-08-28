# Tier 5 — the systems-language research pass

Research note, not a build log. Twelve areas were asked about; each section states what
the tree actually holds today (with `file:line`), the precise gap, the shape of the
smallest increment that closes it, whether it owes a self-hosted port mirror, and — the
question that orders everything — **does Tier 5's definition of done actually require
this, or is it adjacent?**

Tier 5's definition of done, as given, and used as the only ruler in this note:

> a service can start, report health, run background work, shut down gracefully, and be
> tested deterministically; observability is injectable and assertable; config errors
> point to their source; subprocesses are bounded/captured/killable; package primitives
> are reproducible; storage survives tested crashes; compatibility tooling detects
> breakage.

Predecessors: `docs/session-notes/jestyr-tier4-language-work-handoff.md` (§5 is the live
queue), `docs/session-notes/jestyr-std-v4-tier4-complete-handoff.md`,
`docs/safety-mosaic-next.md`, `docs/session-notes/jestyr-next-frontier-handoff.md`.

---

## §0. START HERE — the shortlist, dependency-ordered

Twelve areas produced **twelve blocking increments and ten good-someday ones.** The split
is not by how interesting the work is; it is by whether a sentence in the definition of
done is false without it.

### TIER-5-BLOCKING, in dependency order

| # | Increment | Area | Why it blocks | Port mirror? | Size |
|---|---|---|---|---|---|
| 1 | Sanitizer + clang + warning legs in CI | §8 | "tested deterministically" and "survives tested crashes" are claims about the *runtime* of emitted C, and **nothing checks emitted C for UB today** | no | S |
| 2 | Channel `close` + `try_recv` | §3 | a worker blocked in `channel_recv` cannot be woken; "shut down gracefully" is unreachable | no (library) — reseed | S |
| 3 | `@move` containment check | §1 | a struct holding a `@move` handle is freely copyable; every service type is that struct | reference-only (attrs) | S |
| 4 | Refuse a `@move` type in a `spawn` target's parameter list | §2 | the only rule at `spawn` is about `mut` slices; a socket crosses into a task with no diagnostic | **yes** (escape) | S |
| 5 | Refuse an unannotated `extern` parameter | §6 | it silently lowers to C `int` — a degrades-to-gcc row in the tier's own foundation | probe first (see §final) | XS |
| 6 | `sysnet.Net` capability handle | §12 | the network is a service's headline effect and the one domain with no handle | no (library) — reseed | S |
| 7 | Per-domain `@no_fs` / `@no_net` / `@no_process` | §12 | turns "visible in the signature" into "checked", reusing `@no_os`'s existing machinery | reference-only (escape refusal) | M |
| 8 | attest: record methods/traits, record `@deprecated`, exact version-prefix | §11 | removing a `pub` method is currently classified **Compatible** — a false negative in the compatibility tool itself | **yes** + reseed | M |
| 9 | `jestyrc manifest` / `--verify` CLI + additions detection | §9 | "package primitives are reproducible" — the lockfile exists and is `#[allow(dead_code)]` | reference-only | XS |
| 10 | `#line` port + `-g` in the port driver | §10 | a `jc`-built binary has no source mapping by either route, and the two toolchains' `attest` hashes disagree | **yes** + reseed | M |
| 11 | `select` default / timeout arm | §3 | graceful shutdown needs a receive that can give up; today `select` busy-spins forever | **yes** + reseed | M |
| 12 | Seeded RNG intrinsic + `hashmap` seeding | §12 | `std/hashmap` has fixed constants and no seed — a service taking untrusted keys has no HashDoS answer | eleven-site intrinsic + reseed | M |

**The one real dependency in that list is 3 → 4.** The spawn rule reuses `@move` as its
marker, so a `Server { sock: Socket }` that is not itself `@move` is invisible to it.
Land containment first or the second rule is half a rule. Everything else is independent;
1 is first because it is the thing that would catch a mistake in any of the other eleven.

### GOOD-SOMEDAY — real, ranked, and not required by the definition of done

| Area | Item | Why it is adjacent |
|---|---|---|
| §1 | linearity — a `@move` value must be *consumed* | catches "you forgot to close it"; mosaic item 7 (`docs/safety-mosaic-next.md:475-482`) defers on the `?`/early-return interaction, correctly |
| §2 | a real `Send`/reference-capability system | mosaic item 8's warning applies with full force (`docs/safety-mosaic-next.md:484-491`): if it cannot be stated as two or three named opt-in capabilities, do not build it |
| §4 | `uninit(T,n)` + initialized-prefix tracking | the fake-default is an ergonomic wart, never a correctness one — the seed is never read back (`examples/std/smallvec.jtr:36-42`) |
| §5 | borrowed projection `-> read T from xs` | large two-sided tax, and it moves attest hashes; `List.get_ref` buys the measured 5–11% without it |
| §6 | C header emission, foreign-struct layout verification, callback ownership conventions, `environ` | none appears in the definition of done; the stdlib already pays the layout cost by hand and pins it |
| §7 | target triples, `--target`, `zig cc` | no Tier-5 line item mentions cross-compilation |
| §9 | a content-addressed C/object cache | real incrementality has no Tier-5 consumer; the hashes it would need already exist |
| §10 | profiling hooks, backtraces, a real panic runtime | `std/log` + `std/diag` already give injectable, assertable observability at the right layer |
| §11 | `@unstable`/`@since` stability tiers; a normative spec | `DESIGN-STATUS.md` is honest in the meantime, and `docs/frontend-grammar.md:300-301` already defers to the code |
| §12 | capability attenuation, revocation, unforgeability | the lattice warning again; and nothing in Tier 5 needs a handle that cannot be forged |

---

## §1. Move-only / affine handles — what `@move` does NOT give

### What exists

`@move` is a struct-only attribute (`src/attrs.rs:116`, `targets: &[Target::Struct]`),
set on the type declaration at exactly one site (`src/typeck.rs:440`), read back by
`move_only_ty` (`src/escape.rs:1862-1877`), and folded with `droppable_ty` into
`owns_resource` (`src/escape.rs:1892-1894`). Three ownership sites consult it: the
rebinding rule (`src/escape.rs:657-664`), and the two halves of the `take`-argument rule
(`src/escape.rs:1912`, `src/escape.rs:1937`).

Eight adopters, all in the `sys` tier: `examples/std/sysnet.jtr:337` (`Socket`),
`examples/std/sysdir.jtr:85` (`Dir`), `examples/std/file.jtr:242`/`:379`
(`Reader`/`Writer`), `examples/std/syswatch.jtr:295` (`Watcher`),
`examples/std/alog.jtr:182` (`Log`), `examples/std/plugin.jtr:225` (`Host`),
`examples/std/sysproc.jtr:334` (`Child`).

### The gap, in four separable pieces

**(a) Containment does not propagate.** `move_only_ty` reads the declaration's own
`is_move` flag and nothing else. A struct with a `@move` field is freely copyable unless
someone *remembers* to write `@move` on the wrapper too. `alog.Log` and `plugin.Host`
carry the attribute by hand for exactly this reason — that is a convention, not a check.
The contrast is exact and it is in the same tree: `cgen.rs`'s `needs_drop`
(`src/cgen.rs:3007-3021`) already recurses into owned fields and live enum payloads, so
the *destructor* side of resource-ness propagates and the *duplication* side does not.

This is the piece that matters for Tier 5, because a service type is precisely
`struct Server { listener: sysnet.Socket, log: alog.Log }`, and today that value copies.

**(b) There is no linearity.** Nothing requires a `@move` value to be consumed. Grep of
`src/escape.rs` for `unconsumed` / `must be consumed` / `leaked` returns nothing. The
attribute is **affine, not linear**: it forbids a second name, not a forgotten one.
`@must_use` covers the *return value* of a close, never the handle itself.

**(c) Borrowing is untouched, on purpose.** The default parameter convention is `read`
(`docs/escape-guarantee.md:28-30`), so `f(sock)` borrows and any number of names may use
one handle at once. `@move` says nothing about this — which is right for a single frame
and wrong once §2's `spawn` enters the picture.

**(d) Not on enums, not on `distinct`, and generic `@move` is untested.** `Target::Struct`
only; `Item::Enum` never sets `is_move` (`src/typeck.rs:451-461`); `Item::Distinct`
hardcodes `is_move: false` (`src/typeck.rs:485`) — so `distinct Fd = i64` is not a
resource and a `distinct` over a `@move` struct does not inherit. The port records its own
residue: its scan is `Named`-only while the reference resolves a `GenStruct` through its
ctor, and no corpus file declares a generic `@move`
(`examples/std/escape.jtr:622-628`).

### The increment

Do **(a) as a declaration check, not an escape change.** `attrs::validate_struct`
(`src/attrs.rs:396`) already has the struct body in hand and already refuses `@move` +
`@copy` together (`src/attrs.rs:401-418`). Add: a struct that is neither `@move` nor
`@copy` and has a field whose type is `@move` is an error naming the field and the fix.
It fires at the declaration, needs no type-of-expression, and — like `@must_use` and the
surplus-field rule — is a refusal that no corpus file triggers, so it is
**reference-only, no port mirror, no reseed**.

Deliberately *not* making `move_only_ty` recursive in v1: that changes escape diagnostics,
which are compared whole-corpus with an empty denylist
(`src/proptests.rs:13852`, `:13909`), and the corpus is structurally blind to it (nothing
rebinds or `take`s a wrapper). By the Tier-4 §6 trap that means it would pass vacuously
while owing a mirror it did not get — the exact shape of the pre-existing half-mirror
`@move` itself uncovered. If it is built later, mirror it in `examples/std/escape.jtr` in
the same commit and carry it with a differential probe, not a golden.

**Verdict: (a) TIER-5-BLOCKING. (b), (c), (d) good-someday.**

---

## §2. `Send` / thread-transfer — there is one rule, and it is about slices

### What exists

`ExprKind::Spawn` runs exactly two checks (`src/escape.rs:1011-1042`): the `@no_os`
report, and `check_spawn_no_shared_mut_slice` (`src/escape.rs:1420-1450`), which refuses a
spawn target with a `mut`/`out` **slice** parameter. Its own comment states the scope
(`src/escape.rs:1012-1024`): "in the safe subset the only shareable mutable handle is a
slice… shared mutable state across tasks must go through a raw `*mut T` in `unsafe`."
That is pinned in both directions —
`rejects_spawn_with_mut_slice_param` (`src/escape.rs:3508`) and
**`accepts_spawn_with_raw_pointer_and_read_slice`** (`src/escape.rs:3519-3527`).

Channels get their race-freedom from a different mechanism entirely: `channel_send` takes
`take v: T` (`examples/std/sync.jtr:125`) and the give-away route
(`docs/escape-guarantee.md:49-52`) forbids handing a borrow to a `take` parameter. That is
the recorded reason Jestyr has no `Send` (`ROADMAP.md:501-508`), and it is a correct
argument **about channels**.

### The gap

The give-away argument covers the channel and nothing else. A `spawn` argument is an
ordinary call argument at the default `read` convention, so:

* an OS handle crosses into a task as a borrow and both threads may use it;
* a `mem.Allocator` (`@copy`, `examples/std/mem.jtr:23`) crosses by copy into N tasks;
* a raw `*mut T` crosses and is *explicitly accepted* (the test above).

`@move` does not help, because `@move` forbids duplication of *ownership* and this is
concurrent *borrowing*. And the standing constraint that **`spawn` targets cannot be
generic** (`docs/safety-mosaic-next.md:71-73`) means a `Send`-bounded generic API is not
expressible even in principle today.

### The increment

**Reuse `@move` as the marker.** A `@move` type names a resource; a resource crossing into
a concurrently-running frame is the hazard. The rule: *a `@move` type may not appear in a
`spawn` target's parameter list, in any convention.* Zero new syntax, and adoption already
exists on eight types.

Shape: a sibling of `check_spawn_no_shared_mut_slice` in `src/escape.rs`, resolving the
callee the same way (`resolved_callee_name`, per the recorded qualified-callee fix at
`src/escape.rs:1425-1426`) and walking `FnSig` params through `move_only_ty`. **Owes a
port mirror** in `examples/std/escape.jtr` — escape diagnostics are the compared surface —
and the corpus is blind, so it also owes an anti-vacuity differential probe in the shape
of `jestyr_move_only_matches_reference` (`src/proptests.rs:14068`).

The full item — reference capabilities, isolated/immutable/shared — is mosaic item 8 and
should stay design-only for the reason written there: it is the item most likely to become
a lattice every programmer must learn.

**Verdict: TIER-5-BLOCKING at the narrow slice above. The general Send system is
good-someday.**

---

## §3. Typed join handles and channel close

### What exists — and one premise to correct

**Join handles are already typed.** `let h = spawn f(a)` binds `Task(T)` where `T` is
`f`'s return type (`src/typeck.rs:4084-4089`); `await h` unwraps it, and awaiting a
non-task is a diagnostic (`src/typeck.rs:4091-4103`). The i64-only constraint belongs to
`select` and `par_reduce`, not to `Task`.

What the handle cannot do: `await` accepts **only a bare name bound in the enclosing
`concurrent` block** (`src/cgen.rs:5871-5888`), so a handle cannot be stored in a struct,
passed to a function, or returned. Never awaiting is defined and safe — the nursery's
`_jd` guard joins at the closing brace (`src/cgen.rs:8262`, `:8289-8294`) and the result is
discarded.

**Channels have no close.** `examples/std/sync.jtr:102-176`: a bounded ring over the
spinlock; `channel_send(take v)` at `:125`; `channel_recv` at `:144`. There is no
`channel_close`, no closed flag, no sender/receiver split, and no `Option` return. A
receive from an empty channel is `for { lock; if count > 0 {…} unlock }`
(`sync.jtr:145-157`) — an unbounded spin with no timeout and nothing that can wake it.
`channel_free` documents that buffered values are leaked (`sync.jtr:171-172`). Every demo
and every `select` loop counts a known number of items
(`examples/std/channel.jtr:60`, `:74`; `examples/std/select.jtr:29`) **because there is no
EOF to detect.**

`select` is `Channel(i64)`-only (`src/typeck.rs:4185-4207`), single-consumer, statement
position only (`src/cgen.rs:5896-5899`), lowered to a busy `while (!_seldone)` chain
(`src/cgen.rs:8324-8340`). No `default` arm, no timeout arm, no send arm.

Also absent from `examples/std/sync.jtr`: `Once`, `RwLock`, `CondVar`, `Barrier`,
`Semaphore`. Atomics are five bare intrinsics on `i64` cells
(`src/cgen.rs:6203-6228`) with **no compare-exchange**.

`examples/std/runtime.jtr` is the counter-example that shows the right shape already
exists at library tier: a `Runtime` you name and step, cancellation as a token
(`runtime.jtr:271`), deadline-ordered firing with insertion-order tie-break, a `Poller`
seam so the loop never touches the OS, and `poll_for`/`run_for` budgets
(`runtime.jtr:588`, `:633`). It is testable under `time.manual()`. It just has nothing to
do with threads.

### The gap, stated as the Tier-5 failure

A service starts background workers, each blocked in `channel_recv`. Shutdown arrives.
There is no way to tell them to stop: no close, no timeout, no cancellation reaching a
blocked receive. The `concurrent` brace then blocks forever joining them. **"Shut down
gracefully" is not merely untested — it is unreachable.**

### The increment

Two, and the first is free.

1. **Library.** A `closed` byte in the `ctrl` block (`sync.jtr:105` already packs four
   slots), `channel_close`, and `channel_try_recv` returning `Option(T)` — with `recv`
   returning a closed verdict rather than spinning. Zero compiler change, exactly like
   `Mutex` and the channel itself (`ROADMAP.md:508`). Owes a **reseed**, no mirror.
   The close-side ordering rule is the one thing to get right and write down: a close
   must be visible to a spinning receiver before the sender's last value is, or a
   shutdown drops the tail.
2. **Compiler.** A `default` / timeout arm on `select` (`src/cgen.rs::emit_select`,
   `:8308-8343`). Changes emitted C ⇒ **port mirror + corpus file + reseed**.

Storing a `Task(T)` outside its `concurrent` block is *not* on this list: the constraint
is a clean diagnostic, and a service's workers live for the process, so the nursery is the
right shape. Say so rather than widening it.

**Verdict: TIER-5-BLOCKING. The library half is the cheapest blocking increment in this
note.**

---

## §4. Uninitialized memory and partial initialization

### What exists

There is no uninitialized-memory concept. `mem.allocate`
(`examples/std/mem.jtr:41`) dispatches to the allocator vtable; the system strategy is
`alloc` → `malloc` (`src/cgen.rs:6543`), so returned bytes are indeterminate.

Three patterns, all in the tree:

* **Never initialize, never read.** `examples/std/list.jtr:32-40` allocates, copies
  `[0, len)`, writes the new element through `unsafe { (l.ptr + l.len).* = x }`, and
  simply never reads `[len, cap)`. `get`/`set` are documented caller-checked
  (`list.jtr:44`, `:49-53`); `truncate` forgets rather than drops (`list.jtr:55-59`).
* **The fake default.** `examples/std/smallvec.jtr:77-78` — `make(T, a, zero: T)` fills
  eight inline slots with a caller-supplied seed, and the header argues it honestly
  (`smallvec.jtr:36-42`): "an opaque `T` has no zero this module can invent… it exists
  because the array must hold *something*." It is never read back.
* **Hand zeroing for FFI.** `examples/std/sysproc.jtr:405-413` writes `zero_bytes` with
  the reason at `:405-406` — "`alloc` is `malloc`, not `calloc`, and a `STARTUPINFOA` with
  uninitialised `dwFlags` tells `CreateProcess` to read std-handle fields that hold
  garbage."

**The unsafe contract does not name it.** `docs/unsafe-contract.md:32-46` requires a deref
target to be live, sized and aligned, and says nothing about being *initialized*. Reading
an indeterminate byte is UB in the emitted C and appears nowhere in the contract the
enforcement ladder was built to enforce.

The destructor side is the hard part the question identifies, and today it is hard because
nothing tracks a prefix: `needs_drop` / `emit_drop_place` (`src/cgen.rs:3007-3021`,
`:3130-3175`) recurse into every owned field and every live payload **unconditionally**.
There is no "initialized up to `n`" notion for them to consult, which is exactly why a
partially built aggregate cannot exist safely and why nothing has broken.

### The increment, and the recommendation not to build most of it

If built: an `uninit(T, n) -> *mut T` intrinsic through the eleven documented sites
(`docs/stdlib-roadmap.md:316-324`), a `mem.Uninit(T)` wrapper, and an `init_at` op. The
destructor rule should stay a **library obligation, not a language one** — `List` already
owns "elements `[0, len)` are live" and enforces it by never reading past `len`; teaching
`needs_drop` a runtime length would put a dynamic fact into a static walker. Write the
rule down in `mem.jtr` and keep the compiler out of it.

What *is* worth doing now, at zero code cost: **add initialization to
`docs/unsafe-contract.md` §1.** Tier 5 claims a documented unsafe boundary; a contract with
a hole a reader can fall through is the failure mode `docs/safety-mosaic-next.md:513`
already forbids ("do not make raw pointers *look* checked").

**Verdict: GOOD-SOMEDAY.** No Tier-5 line item needs it. The contract sentence is a
five-minute blocking-adjacent fix and should ride along with anything else touching
`docs/`.

---

## §5. Borrowed projection and lifetime expressiveness

### What exists

`-> read T` is accepted and is the current answer: `FnSig::ret_conv` carries
`Read`/`Mut`/`Out`, and returning a borrow when the return convention is itself a borrow
is one of the two explicitly-allowed shapes (`docs/escape-guarantee.md:58-60`,
`docs/safety-mosaic-next.md:63-66`). Tier 4 recorded that the refusal's diagnostic already
names the fix and that it was worked around twice before being read
(`jestyr-std-v4-tier4-complete-handoff.md:194-199`).

### The cliffs, all measured rather than reasoned

1. **The return says "a borrow", not "of what."** Mosaic item 2
   (`docs/safety-mosaic-next.md:249-280`). Its third stdlib consumer is now recorded in
   the module itself: `env.lookup_or -> read str` may borrow from the environment *or*
   from `fallback` and has no way to say which (`examples/std/env.jtr:93-95`).
2. **A function cannot return two borrows.** A pair holding borrows is escape route 2,
   capture (`docs/safety-mosaic-next.md:73-74`) — the fact that forced `split_mut` into
   continuation-passing rather than a returned pair.
3. **A struct may not hold a slice.** Measured in the benchmark itemization: the
   transient-borrow case's `World` carries `ptr+len` and rebuilds the view three times
   (`benchmarks/rust_vs_jestyr/results/ANALYSIS.md:154`). This is the cliff a service type
   hits first.
4. **The value-return fallback does not scale.** Measured free at 24-byte `@copy`
   elements (95.8 vs 92.2 ms) and explicitly recorded as not scaling to large or
   non-copyable ones (`ANALYSIS.md:43-47`). The `std/list` variant pays ~11% for
   call-shaped access against a bounds-elided slice loop (`ANALYSIS.md:99`, `:119-128`),
   and the API gaps that cause it are named: **no `reserve`, no `get_ref`, no iteration
   support** (`ANALYSIS.md:126-127`).

### The proposal — simpler than lifetimes, and simpler than `from` too

The repo's own answer is `-> read T from xs` (item 2), and the note's honest question is
whether anything consumes the extra precision today
(`docs/safety-mosaic-next.md:277-280`). The answer is now *yes, one thing*:
`env.lookup_or`. One consumer does not pay a two-sided tax that includes parser, AST,
`FnSig` on both sides, a P2 parse mirror, a P3 signature-rendering mirror, `doc::fn_sig`,
and **moved attest hashes**.

The cheaper thing that closes the measured cost: **`List.get_ref(l, i) -> read T`**. One
parameter, one unambiguous source, no new syntax, and it is exactly what the 5–11% gap was
attributed to. Pure library, reseed only. Do that; leave `from` design-only until a second
and third consumer exist, and record `env.lookup_or` as consumer #1 when they do.

**Verdict: GOOD-SOMEDAY.** Tier 5 needs no returned borrows.

---

## §6. FFI/ABI seriousness

### What exists

`ExternFn` (`src/ast.rs:563-594`) carries `abi: String` and `c_name: Option<String>` — the
declared alias, motivated in place by the keyword collision (`src/ast.rs:576-588`). The
`.h` suffix is the switch in codegen: `if e.abi.ends_with(".h") { continue; }`
(`src/cgen.rs:2646`) suppresses the prototype on the theory the header supplies it,
otherwise a prototype is emitted under the item's `@cfg` guard using
`c_name.unwrap_or(name)` (`src/cgen.rs:2659-2661`). Headers named by externs are collected,
deduplicated and emitted with `winsock2.h` force-sorted first (`src/cgen.rs:1116-1161`).

`cptr` is the opaque handle: `void*` (`src/cgen.rs:10962-10964`), pointer-sized in the
layout model (`src/layout.rs:114-115`), no arithmetic (`src/typeck.rs:3177-3188`), no
deref (`src/typeck.rs:3666-3672`), its own `PrimFamily::Opaque` so it cannot slide into the
text family (`src/typeck.rs:5155-5170`) — but a typed pointer *may* widen to it
(`src/typeck.rs:1485-1489`), which is the documented route for a buffer to reach `fread`.

`@no_mangle` is the export direction (`src/attrs.rs:282-289`), refused on generics
(`src/attrs.rs:504-508`).

### The gaps

* **Function form only.** `src/parser.rs:645` unconditionally expects `fn` after the ABI
  string. There is no extern global in the grammar, the AST, or the checker — which is
  §5.4's recorded `environ` gap, and it is why a POSIX child gets an empty environment
  where a Windows child inherits (Tier-4 note §2).
* **No FFI type restriction whatsoever.** `src/typeck.rs:673-697` lowers every extern
  parameter through the ordinary `lower_type`; the only diagnostic is duplicate-definition
  (`:690`). Any Jestyr type — `str` as `JestyrStr`, a struct by value, a slice, a thin
  `Ty::Fn` — may be written in an extern signature and will lower through `c_ty_ast`.
* **An unannotated parameter silently becomes C `int`.** `src/cgen.rs:2678`:
  `None => "int".to_string()`. No diagnostic anywhere. This is the §4d shape exactly — a
  mistake whose only witness is the C compiler, and here not even that, because the
  emitted prototype is self-consistently wrong.
* **No struct-layout verification against anything foreign.** `@layout(c)` is
  byte-identical to the default (`src/cgen.rs:12912-12922`) — "the default said out loud",
  not a check. `layout_matches_c_sizeof` (`src/layout.rs:25-31`) validates Jestyr's model
  against gcc, never a Jestyr struct against a struct in a header. The stdlib pays by
  hand and pays honestly: `examples/std/syspoll.jtr:20-35` writes out `struct pollfd`'s
  two platform layouts in a comment and pins them with an end-to-end readiness test;
  `sysproc`'s Windows offsets are re-measured against `<windows.h>` while *parsing the
  constants out of the shipped source*; `std/sysfs` refuses to read `struct stat` by
  offset at all.
* **No `.h` emission.** Nothing in the tree produces a header for a Jestyr module.

### The increment

Only one of these blocks Tier 5, and it is the smallest: **refuse an `extern` parameter
with no type annotation.** It is a typeck-seam refusal in the shape of the surplus-field
rule, and it removes a silent-miscompile class from the tier's own foundation. See
§final — whether it is reference-only depends on whether any shipped extern is currently
unannotated, which must be probed before it is written.

The rest is real work with no Tier-5 consumer. If `environ` is wanted for its own sake,
the honest cheap version is a `getenv`-loop in `sysproc` rather than a new extern form —
the asymmetry is already documented, and a new item kind is a two-sided tax for one
caller.

**Verdict: the untyped-parameter refusal is TIER-5-BLOCKING. Header generation, layout
verification, callback conventions and `environ` are good-someday.**

---

## §7. Cross-compilation and target triples

### What exists

Nothing named a target. `find_c_compiler` (`src/main.rs:1174-1188`) tries the hardcoded
list `["cc", "gcc", "clang"]` and returns the first that answers `--version`. **There is
no `CC` env override, no flag, no config file.** `CC_FLAGS` (`src/main.rs:1071`) is fixed
and doubles as the FP-determinism seam and the attest provenance (`src/attest.rs:74`,
`:86`). `cc_base_flags` (`src/main.rs:1087-1106`) adds `-g` and, **gated on `cfg!(windows)`
— the host running `jestyrc`** — `-D_WIN32_WINNT=0x0600`.

Link libraries are chosen by substring search over the emitted C text: `-pthread` if it
contains `"pthread"` (`src/main.rs:940-942`, `:1130-1132`); `-lws2_32` if the host is
Windows *and* the text contains `"winsock2.h"` (`src/main.rs:957-960`, `:1134-1137`),
appended after the source because GNU ld resolves left-to-right. `-lm` is never added.

The `@cfg` vocabulary is a closed four (`src/attrs.rs:658`) with a hardcoded two-entry
specificity table (`src/attrs.rs:675-677`), mirrored at `examples/std/cgen.jtr:8208-8238`.
The port's driver **hardcodes `gcc` on Windows with no probe at all**
(`examples/std/cgen.jtr:14601-14614`) and **omits `-g`** (`:14615`).

The only `--target` mentions in the tree are aspirational: `ROADMAP.md:316`,
`jestyr-design.md:432`, `docs/session-notes/jestyr-debuginfo.md:133-134`.

### The observation worth recording

**`@cfg` has already paid cross-compilation's hardest prerequisite.** Because a `@cfg` item
is *not removed* — it is emitted under a preprocessor guard so emission stays a pure
function of the source (`src/attrs.rs:136-147`) — both platforms are type-checked, escape-
checked and error-set-checked on every host. "Does the Windows branch still compile" is
already answered on Linux, which is strictly better than a real `cfg`. What remains is
purely toolchain plumbing.

### The increment

Not a triple. The two things that fail *silently* today:

* `JESTYR_CC` env override in `find_c_compiler`, so a container or a clang leg (§8) can
  be selected without editing the compiler;
* `-D_WIN32_WINNT` keyed on the emitted guard set rather than on `cfg!(windows)`, matching
  how `-lws2_32` is already double-gated.

Both are `src/main.rs`, both reference-only. Rank them low: they are hygiene, not a tier
requirement.

**Verdict: GOOD-SOMEDAY.**

---

## §8. Sanitizers, fuzzing, differential testing — the highest-leverage item in this note

### What exists, and it is a lot

No `tests/` directory; the whole harness is `src/proptests.rs`, ~20,300 lines
(`src/main.rs:71-72`, `docs/TESTING.md:30-45`). Features: `c-oracle`,
`selfhost-fixpoint` (implies `c-oracle`) — `Cargo.toml:22-40`.

* **~110 proptest properties**, spanning totality (`lexer_is_total`
  `src/proptests.rs:2954`, `pipeline_is_total` `:2966`), determinism (`:3066`, `:3074`,
  `:5637`), emission invariance (`:3089`, `:3096`, `:3695`), drop/RAII (`:3890-4053`),
  coherence (`:3207-3332`), CTFE (`:6168-6312`), modules (`:5623-5760`), and stdlib
  reference oracles (`:6833-7712`).
* **~35 bolero targets** in `mod fuzz` (`src/proptests.rs:7718`), covering the pipeline,
  spans, comptime eval, module hashing, drop glue, traits/dyn, attest and attest-diff.
* **Five pinned counterexamples** in `proptest-regressions/proptests.txt:7-11`.
* **Per-phase whole-corpus goldens with EMPTY denylists** — parser module dump
  (`:13630`, denylist `:13622`), typeck (`:13753`, `:13745`), escape diagnostics
  (`:13909`, `:13852`).
* **cgen byte-identity** behind `CGEN_GOLDEN_ALLOWLIST` (`:17437-17704`), 249 unique
  entries against 274 `.jtr` files; the four exclusion categories are each argued in place
  as degradation-of-erroneous-programs and unreachable from a program that compiles
  (`:17705-17793`). An integrity gate scans the corpus for `@cfg` and asserts every such
  file is allowlisted, reading `proptests.rs` **as text** so it holds without the feature
  (`:2538`, `:2559-2563`).
* **A seeded, replayable differential fuzzer** `jestyrc` vs `jc` over grammar-directed and
  token-mutated input (`:13211-13509`), deliberately not proptest so a CI divergence
  replays exactly (`:13225-13231`). It found two real `eat_ident` bugs in the port
  (`:13379-13385`).
* **`jc_build_matrix`** 63/63 (`:638`, `docs/jc_build_matrix.txt`) and
  `selfhost_fixpoint_full` / `_subset` / `bootstrap_seed_is_current` (`:18239`, `:18340`,
  `:18301`).
* CI: `cargo test` on Ubuntu + Windows; `--features c-oracle,selfhost-fixpoint` on Ubuntu;
  a Rust-free `bootstrap` job that gcc-builds the seed, regenerates it, and diffs
  (`.github/workflows/ci.yml:27-90`). The Ubuntu `c-oracle` run *is* the cross-OS
  determinism canary (`ci.yml:7-10`, `src/proptests.rs:19928`).

### What does not exist — and this is the finding

* **Zero sanitizers.** A repo-wide grep for `fsanitize|asan|ubsan|tsan|valgrind` returns
  only prose (one doc comment at `src/main.rs:1074`). Nothing has ever run ASan, UBSan or
  TSan over emitted C.
* **Zero warning flags.** `CC_FLAGS` is `-O2 -std=c11 -ffp-contract=off -fno-fast-math`
  (`src/main.rs:1071`) — no `-Wall`, no `-Wextra`, no `-Werror`, on any path. gcc's warning
  about `combinators` emitting an empty-bodied non-`void` function went to nobody; the
  matrix recorded `BUILD_OK` (`docs/jc_build_matrix.txt` header).
* **One C compiler, ever.** `find_c_compiler` is first-match-wins; on `ubuntu-latest` `cc`
  resolves to gcc, so `clang` is never reached. CI's `c-oracle` and `bootstrap` jobs are
  ubuntu+gcc (`ci.yml:51`, `:75`, `:81`). No clang, no tcc, no MSVC. Every test-side C
  invocation routes through that one function (`src/proptests.rs:11470`, `:11530`,
  `:17865`, `:18250`, `:18342`, …).
* **No fuzz target ever compiles the emitted C.** Every bolero body stops at emission or
  diagnostics.
* **`jc_build_matrix` compares only whether a binary was produced.** The behavioral
  complement — run both binaries, compare bytes — covers **6 of the 63**
  (`src/proptests.rs:707-712`).
* `selfhost_fixpoint_subset` skips every file containing `import "` (`:18355`) and asserts
  only `checked >= 5` (`:18405`).
* **A stale claim in `README.md:164-165`** — it says CI runs the first three steps on
  Ubuntu *and Windows*; `ci.yml` runs only `cargo test` on Windows. `README.md:163` also
  says "148-file byte-identical corpus" against an allowlist that now holds 249.

### The increment

Three CI legs, no language work, and one of them is nearly free because the harness
already does the hard part.

1. **Sanitizers over `selfhost_fixpoint_subset`.** That test already gcc-builds and *runs*
   every allowlisted program and compares stdout and exit code
   (`src/proptests.rs:18401-18402`). Adding `-fsanitize=address,undefined` is a second
   flag list and a second job. It must not touch `CC_FLAGS` — that is the attest seam, and
   `debug_flag_is_carried_and_separate_from_the_determinism_seam`
   (`src/main.rs:1362`, exact-count assertion `:1356-1360`) will correctly fail if anyone
   tries. Pass them on the oracle path only.
2. **A clang leg.** `JESTYR_CC` (§7) plus a second `c-oracle` job. This is the only way
   `@cfg` guards, extern prototype suppression, statement-expression lowerings and the
   GCC vector extensions `@simd` emits get a second opinion.
3. **`-Wall -Wextra` on the oracle path**, failing on the categories `jc_build_matrix` is
   structurally blind to — starting with the empty-body class it already documents.

Everything else here (grammar-aware corpus-persisting fuzzing, fuzzing `jc` itself,
widening the behavioral matrix past 6/63) is good work with a lower ratio.

**Verdict: TIER-5-BLOCKING, and first in the order.** "Tested deterministically" and
"storage survives tested crashes" are claims about the runtime behaviour of emitted C.
`std/alog` makes a crash-safety claim it checks *at the record level*
(`examples/std/alog.jtr:8-18`) — a torn record is detected and the discard is reported —
and that claim is currently defended by zero memory-safety instrumentation.

---

## §9. Incremental build and compiler cache

### What exists

Content hashing is real and better than it needs to be: `compute_hashes`
(`src/module.rs:306-330`) hashes each module's **normalized post-parse form** — every
item pretty-printed via `printer::print_item` and then `sort`ed (`:319`, `:323`) — so a
comment, a whitespace edit or a declaration reordering does not move the hash while a
semantic edit does. `module_hash` (`:334-358`) appends each import's binding and hash,
sorted by binding, then SHA-256s the whole thing with the in-tree hasher
(`src/sha256.rs`). The hash is therefore transitive over the DAG. `import "x" = "<sha>"`
pins a dependency and is verified after load (`src/module.rs:259-274`).

`render_manifest` / `verify_manifest` (`src/module.rs:105-118`, `:126-147`) emit and check
the `jestyr-manifest/v1` DAG.

### The gaps

* **Both manifest functions are `#[allow(dead_code)]` and wired to no CLI**
  (`src/module.rs:104`, `:125` — "surfaced via tooling (O's main.rs) later"). Grep of
  `src/main.rs` finds neither. They exist only for two tests (`:1627`, `:1649`).
* **`verify_manifest` cannot detect an addition.** It parses only `module <name> <hash>`
  lines and reports drift or "no longer loaded" (`:134-143`). A module added without
  touching the manifest verifies clean.
* **There is no on-disk cache of anything.** `build_and_maybe_run`
  (`src/main.rs:1109-1116`) writes `<tempdir>/jestyr_<stem>.c` unconditionally — no hash
  key, no staleness check, no object reuse. No `.jestyr-cache` anywhere.
* **No timestamps in the compiler at all** — a grep of `src/*.rs` (excluding tests) for
  `SystemTime|modified()|mtime|UNIX_EPOCH` returns nothing. That is a *feature* for
  reproducibility and the reason a cache would have to be content-addressed.
* Cost of the absence, measured: the import closure is re-tokenized on every build
  (`ANALYSIS.md:130-134` — 612→673 ms and 883→1290 ms once std is imported).

### The increment

Split the item. Real incrementality — a content-addressed cache keyed on the module hash,
skipping re-tokenization and reusing emitted C — is a large increment with **no Tier-5
consumer**, and the compile times above are not a service's problem.

What Tier 5 does need is that "package primitives are reproducible" be *checkable by a
user*: **`jestyrc manifest <root>` and `jestyrc manifest --verify <file>`.** That is ~20
lines in `src/main.rs` over two functions that already exist and are already tested, and
it turns a dead-code lockfile into an artifact CI can gate on. Fold in the additions check
while touching it — five lines, and without it the lockfile has a hole in the exact
direction a dependency gets added.

Reference-only, no mirror, no reseed.

**Verdict: the manifest CLI is TIER-5-BLOCKING. Real incrementality is good-someday.**

---

## §10. Debug info and profiling

### What exists

The reference emits `#line N "file.jtr"` per function and per statement, and on
`requires`/`ensures` asserts: `mark_line` (`src/cgen.rs:1054-1064`), deduped through
`dbg_last` (`:704-708`, reset per function at `:2847`), per-statement site at `:3945`,
paths normalized to forward slashes so a Windows path is not read as C escapes. `-g` is
`DEBUG_FLAG` (`src/main.rs:1080`) and is **deliberately kept out of `CC_FLAGS`** because
that constant is both the FP seam and the attest provenance — pinned by an assertion
(`src/main.rs:1376`). Byte-identity is preserved by construction: an empty `DebugInfo` on
the single-file path emits no `#line` at all
(`docs/session-notes/jestyr-debuginfo.md:33-38`).

### The gaps

* **The port emits no `#line`, and its driver also omits `-g`.** The `#line` gap is
  recorded (`docs/session-notes/jestyr-next-frontier-handoff.md:472-485`); the `-g` gap is
  visible in the port's flag string (`examples/std/cgen.jtr:14615`). So a `jc`-built binary
  has **no source mapping by either route**, and `jestyrc attest` and `jc attest` disagree
  on `c-sha256` for any module-path file.
* **The only failure mechanism in the language is C `assert()`.** `assert.h` is in the
  fixed prelude (`src/cgen.rs:1079`) and every checked construct lowers to it — contracts
  (`:2876`, `:2955`), loop invariants (`:8948`, `:9027`), slice and array bounds
  (`:5353`, `:5368`, `:5512`, `:5532`, `:10051`, `:10064`), the genref generation check
  (`:5593`, `:10081`), UTF-8 validation (`:6377`). There is **no `jestyr_panic`, no abort
  handler, no `__builtin_trap`, no backtrace** — grep returns nothing. A production
  failure prints glibc's `file:line: func: Assertion 'expr' failed.` and stops.
* `--error-traces` (`src/cgen.rs:1163-1184`) is the only trace-like runtime: opt-in,
  error-path only, a fixed 64 entries, stderr, allocation-free.
* **No profiling of any kind.** No `-pg`, no perf hooks, no instrumented build. The
  `--audit` build proposed at `docs/session-notes/Jestyr-Testing-Strategy.md:80,169` was
  never built. `jestyrc selfbench` (`src/main.rs:142-214`) times the compiler, not user
  programs.
* No `--debug`/`--no-debug` toggle and no `-fdebug-prefix-map` analogue
  (`jestyr-debuginfo.md:119-127`), so emitted C is a function of the invocation path — a
  subtlety four determinism proptests had to be restructured around (`:66-78`).

### The increment

Observability at the *library* layer is already right and already Tier-5-shaped:
`std/log` takes its `Clock` and `Writer` as parameters and has no ambient logger
(`examples/std/log.jtr:1-14`), `std/diag` renders into a caller `Sink` and owns no
destination, and `writer.to_buffer()` makes both assertable in a test. That is the
"injectable and assertable" line item, delivered.

What blocks is narrower and it is the compatibility consequence, not the debugging one:
**the `#line` port plus `-g` in the port's driver.** Two toolchains that disagree on
`c-sha256` mean the ABI gate answers differently depending on which compiler ran it, and
"compatibility tooling detects breakage" is a Tier-5 line item. The prerequisite is
already built — `jestyr_module_cgen_matches_reference_with_line_directives`
(`src/proptests.rs:17304`) — and the port already has its input (the `Ml.map` checkpoint
pairs give per-file line/col). Port `mark_line`'s placement and dedup, then tighten the
golden in the three recorded steps. **Mirror + reseed by definition** (the seed's own C
gains the directives).

A panic runtime, a backtrace, and profiling hooks: good-someday, and the assert-only
design is defensible while `#line` works.

**Verdict: the `#line`/`-g` port is TIER-5-BLOCKING. Profiling and backtraces are
good-someday.**

---

## §11. Public language spec and compatibility profile

### What exists

`attest` is real and is the closest thing in the tree to a compatibility profile. The
manifest is `jestyr-attest/v1` (`src/attest.rs:55`): source id, `c-sha256` of the emitted
C, `cc-flags`, then per-item records sorted by `(kind, name)` (`:79-93`). `diff` classifies
sixteen distinct edits (`:441-504`) and `jestyrc attest --diff` exits non-zero iff any
change is breaking (`src/main.rs:1002-1004`). The guarantee phrases are produced by the
*same* extractor the doc generator uses (`src/doc.rs:446-467`), so the attested behavioural
ABI cannot drift from the rendered docs — and the self-hosted side shares one extractor
too (`at_guarantee_phrases`).

### The gaps

* **Traits, impls and methods have no per-item record.** `collect_records` skips
  `Item::Trait | Item::Impl | Item::Distinct | Item::Import` outright
  (`src/attest.rs:152`). A service's public API is methods. **Removing a `pub` method
  today produces no change record at all** — a false negative inside the tool whose whole
  job is detecting breakage.
* **`@deprecated` is invisible to attest.** It is a real, `Active` attribute
  (`src/attrs.rs:276`) that lowers to `__attribute__((deprecated))` (`src/cgen.rs:3601-3609`)
  — and it appears in neither `doc::fn_guarantees` nor `attest::ParsedItem`
  (`src/attest.rs:202-214`), so `--diff` cannot see a deprecation land or be removed.
* **The version-prefix refusal the code promises is not implemented.**
  `src/attest.rs:53-54` says the version is "bumped if the on-disk shape changes (so
  `--diff` can refuse to compare across incompatible versions)"; `parse_manifest` checks
  only the `jestyr-attest/` prefix (`:283`), so a `v2` manifest parses.
* **There are exactly two verdicts** — `Breaking` and `Compatible` (`:334-338`); "added"
  and "removed" are `detail` strings, and an added item is unconditionally `Compatible`
  (`:416-420`). That is defensible, but it means the exit code cannot distinguish "nothing
  changed" from "the API grew".
* **No stability marking of any kind.** No `@unstable`, no `@since`, no
  experimental/provisional tier; grep across `src/`, `docs/` and root finds only
  `@deprecated`. `ParsedItem` has no stability field.
* **No `--version` flag.** `src/main.rs:1191` is a hardcoded banner string;
  `CARGO_PKG_VERSION` is never read. No edition or language-version concept exists
  anywhere.
* **No normative spec, and the tree says so honestly.** `jestyr-design.md:5` — "a
  research-driven proposal, **not a specification**". `docs/frontend-grammar.md:1-8`
  documents what the parser accepts and closes with "the parser is right and this file is
  a bug" (`:300-301`); its conformance suite is described as "a tripwire, **not a proof**"
  (`:303-313`). `DESIGN-STATUS.md` is the reconciliation table and is the honest surface.

### The increment

Three, in one arc, because the manifest's diff surface should settle once:

1. `@deprecated` becomes a recorded guarantee phrase — one line in `doc::fn_guarantees`,
   one arm in `absorb_guarantee`, and a `diff_item` rule (gaining it is `Compatible`,
   losing it is `Compatible` too, but both are *visible*).
2. Method and trait records, so removing a `pub` method is `Breaking`.
3. `parse_manifest` matches the version exactly.

(1) and (2) both change manifest bytes, which the port reproduces byte-equal
(`jestyr_driver_attest_manifest_matches_reference`) and the doc golden also sees —
so **port mirror + reseed owed**. (3) is reference-only.

Stability tiers and a normative spec: good-someday. `DESIGN-STATUS.md` plus a
CHANGELOG that already carries a `### Changed — may reject code that previously compiled`
section (`CHANGELOG.md:736`) is a more honest compatibility story than most pre-1.0
languages ship, and inventing `@unstable` before there is a downstream consumer would add
a signature-visible marker nobody reads.

**Verdict: (1)(2)(3) are TIER-5-BLOCKING. Stability tiers and the spec are good-someday.**

---

## §12. Capability-oriented security model

### What exists — further along than the question assumes

Four capability handles are built, each living *inside* the module whose domain it guards
rather than in a parallel `*_cap` universe: `fs.Fs` (`examples/std/fs.jtr:63-153`),
`time.Clock` (`examples/std/time.jtr:67-207`), `env.Env` (`examples/std/env.jtr:65-114`),
`process.Process` (`examples/std/process.jtr:58-134`), plus `sysproc.Spawner`
(`examples/std/sysproc.jtr:304-325`). Each restricted mode is chosen for a *different*
reason and the asymmetry is argued (`docs/stdlib-roadmap.md:238-243`): `Clock.manual` for
determinism (a denied clock is useless), `Env.sealed` to prove a negative while still
counting lookups, `Fs.read_only` because "read but don't modify" is a real need,
`Process.denied` for refusal-with-a-count.

`examples/std/caps_demo.jtr` is the argument rather than a tour: one `stamp` function whose
signature names every effect (`:94`), producing byte-identical output on two runs with
deterministic handles (`:117-132`) and varying under `host()` ones — with the counters as
evidence (`:135-137`).

**And an enforcement axis already exists, at coarse granularity.** `@no_os`
(`src/attrs.rs:192-197`) is escape-checked and covers files, process, args, environment,
the clock, stdout/stderr **and threads** (`src/escape.rs:1031-1038`). Every `core` module
carries it, and annotating `core.jtr` immediately falsified that module's own header —
`par_binned_sum` and `par_reduce` spawn threads and allocate
(`docs/stdlib-roadmap.md:69-76`). That is the model for what a checked capability buys.

### The gaps, precisely

* **Nothing between `@no_os` and nothing.** The modules say so themselves:
  `examples/std/env.jtr:46-63` — "Everything above is ambient… **Not enforcement —
  `env_var` stays reachable.**"; `examples/std/time.jtr:52-65` — "`now_nanos()` is still
  reachable from anywhere." The recorded conclusion is "real enforcement needs effects in
  the type system, which is a language question" (`docs/stdlib-roadmap.md:595-596`).
* **No handle for the network.** `std/sysnet` opens sockets through free functions; there
  is no `Net`. It is the one domain with no capability and the one a service uses most.
* **No randomness at all.** No rng module, and no randomness intrinsic — the builtin
  return-type table lists only `arg`, `env_var`, `mono_nanos` (`src/typeck.rs:5360-5370`).
  Consequence, stated in the module: `std/hashmap` is FNV-1a with a SplitMix64 finalizer,
  "the constants are fixed and there is no seed" (`examples/std/hashmap.jtr:5`, `:94`). A
  service taking untrusted keys has no HashDoS answer.
* **No attenuation, revocation or unforgeability.** Any code may call `fs.host()`
  (`fs.jtr:72`), `time.host()` (`time.jtr:74`), `env.host()` (`env.jtr:71`). Handles are
  plain structs with counters, copied by value, so counter state is per-copy and nothing
  exercises a handle copied into two callees.

### The increment

1. **`sysnet.Net`**, matching the other four: `host()`, `denied()`, `connect`/`listen`
   through the handle, counted refusals. Pure library, reseed only. It also gives §12's
   next item its fifth domain.
2. **Per-domain `@no_fs` / `@no_net` / `@no_process` / `@no_clock`.** `@no_os` is
   implemented as a per-function escape flag plus a per-op rejection
   (`src/escape.rs:1032-1038`); splitting the intrinsic set by domain is a **table, not a
   new pass**. It turns "the authority is visible in the signature" into "the absence of
   the authority is checked", which is the honest answer to the recorded "needs effects in
   the type system" at roughly a tenth of the cost, and it stays inside
   `docs/safety-mosaic-next.md:513`'s constraint — named, opt-in, and never appearing in a
   signature that does not need it.
   Refusal-only ⇒ **reference-only, no mirror, no reseed**, but see §final: `@no_os`'s
   call graph resolves free functions **by name only** (`docs/stdlib-roadmap.md:78+`), so
   the same blind spot inherits and must be measured before the split is claimed as a
   proof.
3. **A seeded RNG intrinsic + `hashmap` seeding.** Eleven sites plus a reseed. Rank it
   after 1 and 2; note that the seed must be a *capability*, not an ambient call, or it
   undoes `caps_demo`'s reproducibility claim in one line.

Attenuation, revocation and unforgeability: good-someday, and the lattice warning applies.

**Verdict: (1) and (2) are TIER-5-BLOCKING; (3) is blocking-adjacent and cheap.
Attenuation and unforgeability are good-someday.**

---

## §final. What could not be determined from the source — probes owed

Each of these changes a decision above, and none can be settled by reading. No `cargo`
command was run for this note (a baseline ladder held the target lock), so anything below
that needs a build is listed rather than answered.

| # | Question | What it decides |
|---|---|---|
| 1 | Does **any** shipped `extern` declaration have an unannotated parameter? | Whether §6's refusal is reference-only or a breaking change to `examples/std`. Grep alone cannot answer it — the parser accepts `(fd, buf, n)` and `cgen` silently types them `int`, so a wrong binding looks identical to a right one in source. |
| 2 | Would a recursive `move_only_ty` fire on **any** corpus file? | Whether §1's escape variant is vacuous. If it is (likely — nothing rebinds or `take`s a wrapper), the anti-vacuity probe is mandatory, per the Tier-4 §6 trap. |
| 3 | Are `attrs::validate*` diagnostics inside **any** two-sided golden? | Whether §1(a) and §12(2) are genuinely reference-only. The escape and typeck goldens are confirmed compared; the attribute-validation ones were not traced to a comparator. |
| 4 | Does `-fsanitize=address,undefined` survive today's emitted C under **clang**? | §8's first two legs. The C uses `__auto_type`, statement expressions and GCC vector extensions (`@simd`); clang accepts all three, but the combination with ASan on the `selfhost_fixpoint_subset` programs is untested. |
| 5 | How many warnings does `-Wall -Wextra` produce on the current corpus? | Whether §8's third leg is a one-commit gate or a migration. The one known instance (`combinators`' empty body) suggests few; that is a sample of one. |
| 6 | What is `selfhost_fixpoint_subset`'s actual `checked` count? | The assertion floor is `>= 5` (`src/proptests.rs:18405`). If the real number is ~200 the test is strong and under-claimed; if it is ~8 the sanitizer leg in §8 covers far less than it appears to. |
| 7 | Does `@move` on a **generic** struct work on the reference side? | §1(d). The reference resolves through `GenStruct`'s ctor (`src/escape.rs:1868-1874`) and the port's scan is `Named`-only (`examples/std/escape.jtr:624`); no corpus file declares one, so both sides currently agree by producing nothing. |
| 8 | Can a closed-channel test be written **without real threads**? | §3's cost. `channel_recv` spins on a raw spinlock with no clock; if a single-threaded close/`try_recv` test is expressible, the library increment ships with a toolchain-free test, otherwise it needs the `c-oracle` gate. |
| 9 | How many stdlib OS call sites go through a **method or fn-pointer** rather than a free function? | §12(2). `@no_os`'s call graph is free-function-by-name only; if a meaningful fraction of `sys`-tier access is method-shaped, a per-domain split would ship a marker that proves less than it says — the exact failure `docs/io-design.md`'s `no_alloc_does_not_see_through_a_trait_method` already records for `@no_alloc`. |
| 10 | Does the `README.md:163-165` drift (Windows CI scope; "148-file corpus" vs 249 allowlist entries) reflect a stale doc or a regressed workflow? | Whether §8 also owes a workflow fix or only a doc correction. |
