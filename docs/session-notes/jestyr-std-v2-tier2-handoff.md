# Std v2 Tier 2 — what remains

Cold-start note. **Remaining work first (§1), then what is deliberately deferred and why
(§2).** What is already built is in §3 — read it only to avoid rebuilding something.

Everything is on `master`, clean tree. `git pull` and go; there is no branch to chase.

Baseline before you change anything, so a later failure is yours:

```bash
cargo build --release && cargo test --release --features "c-oracle,selfhost-fixpoint"
```

**1195 passed, 0 failed, 3 ignored** (~300 s). The 3 ignored are deliberate slow numeric
sweeps (`dragon_matches_std_thorough`, `slow_parse_matches_std_thorough`,
`dump_pow10_table`), not breakage.

---

## §1. REMAINING

Ranked by value per unit of risk. Each has a concrete first increment, because "design
Collections v2" is not a task and "a deterministic `HashMap(K,V)` using `strmap`'s probing
with a canaried hash" is.

### §1.1 — Collections v2 (Tier 2 area 4). ONE container built; the area stays open

**`std/hashmap` exists** — a generic, deterministic `HashMap(K, V)` with a real consumer
(`hashmap_demo`, a byte histogram). What follows is the ORIGINAL plan, kept because two
of its four points turned out to be wrong in ways the next container will hit too:

* Point 1 (**decide the hash**) was already answered: `strmap.jtr` had FNV-1a +
  SplitMix64, fixed constants, no seed. `hashmap` reuses the finalizer and is checked
  against an independent Rust SplitMix64 oracle rather than against its own output.
* Point 2 (**`[K: Hash + Eq]`**) is **not expressible**, and both halves fail separately:
  a bracket parameter takes exactly ONE bound (`+` is a parse error), and `Self` is not a
  legal type in a trait method's parameter list — so one combined `MapKey` trait cannot
  say `fn eq(read self, read other: Self)` either. Hashing and equality are therefore
  **stored function pointers**, the `mem.Allocator` shape.
* Point 3 (**copy `List(T)`**) held, with one forced change: a generic type **cannot hold
  a pointer to another generic type** (`slots: *mut Slot(K,V)` → "the C backend does not
  support type-expressions yet"). So the map is **struct-of-arrays** — four parallel
  columns — which is the better probe layout anyway.
* Point 4 (**pick one**) held and still holds.

**`remove`, enumeration and `std/set` have since landed too.** What is left in this
area: **`Deque(T)`** (a ring buffer — a genuinely separate engine, not a variation on
this one) and **`SmallVec`** (inline storage, which needs a `[N]T` field inside a
generic type — expect the same "type-expressions" refusal that forced struct-of-arrays,
so PROBE IT FIRST). Neither is urgent; each wants a consumer.

Two limits found while adding `set`, both worth knowing before designing a container:

* **A type-returning fn must literally `return struct { … }`.** `pub fn Set(comptime T:
  type) -> type { return hashmap.HashMap(T, bool) }` is refused — a generic type cannot
  ALIAS another. Combined with "a generic type cannot hold a pointer to another generic
  type", there is no way to wrap a container in a newtype today. `std/set` therefore has
  no `Set(T)` type: it is free functions over `HashMap(T, bool)`, which costs the
  distinction between a set and a map-to-bool and buys one engine instead of two.
* **The map had no way to ENUMERATE what you stored**, which is a bigger gap than `Set`
  was. Now `slots`/`slot_live`/`slot_key`/`slot_val` — indexed access to the slot array,
  deliberately not dressed up as an iterator, because the cost is O(capacity) and a
  nicer API would hide that.

The original text follows.

**The roadmap's stated blocker is narrower than it reads.** It says generic containers
"keep colliding with the escape checker's treatment of opaque `T` as non-`Copy`; each new
one is a fight". Directionally true, but `genlist.jtr`, `vec_generic.jtr` and `list.jtr`
all exist, work, and are in the byte-identity allowlist. `container.jtr` is a **deliberate
rejection demo** — what is refused is storing a *borrow* in a collection, which is correct
and should stay refused. So this is a design job, not a fight with the checker.

Four things to settle before writing code:

1. **The hash function, decided and canaried first.** Tier 2 wants deterministic by
   default, randomized only explicitly. `sha256.jtr` is in-language and already the
   cross-OS determinism canary, so a deterministic hasher has a proven byte-exact
   primitive to build on. Pin the chosen function in that canary so changing it is a
   visible break rather than a silent rehash.
2. **`K` needs `Hash + Eq` as a trait bound.** Bounds work (`ledger.jtr`'s
   `[T: Account]`). **Verify the bound composes for a *generic struct's* type parameter
   before committing** — `gen_vtable.jtr` is the nearest precedent and the likeliest bite.
3. **Allocator-explicit, copying `List(T)` exactly.** `List` stores its `Allocator` and
   frees through a blanket `impl[T] Drop for List(T)`. That shape is proven; do not invent
   a second one.
4. **Pick ONE container.** The "no generic collections zoo" objection is about breadth,
   not existence. One deterministic `HashMap(K,V)` with a real consumer is a slice; four
   containers with none is the zoo.

**First increment:** `HashMap(K,V)` modelled on `strmap.jtr`'s open addressing (it already
caches hashes so a grow re-places without rehashing), generic in the key, hash fixed and
canaried. Give it a real consumer on day one — `strmap`'s users, or the compiler's symbol
table. Every slice on this branch that went well had one.

### §1.2 — A partial-read intrinsic. The thing that unblocks a streaming `Reader`

`std/cursor` is the whole reader that can honestly exist today, because the intrinsics only
offer `read_file`/`try_read_file`, which slurp. There is no partial read, no file handle, no
`read_line`. **The next increment in the IO area is that intrinsic, not more library code** —
anything else is an API pretending to stream.

Pays the eleven-site recipe (§4) plus a reseed. Design it before writing it: a file handle
plus `read_into(handle, buf) -> usize` is the shape a real `Reader` needs, and it is a bigger
surface than the one-call intrinsics added so far (`env_var`, `mono_nanos`), because it
introduces a *resource with a lifetime* — which is a `Drop`/RAII question as much as an
intrinsic one.

### §1.3 — Three compiler follow-ups, each independent

**1. Normalize `run_command`'s exit status.** The runtime helper is
`return (int32_t)system(cp)` — raw. Windows gives the exit code; POSIX specifies a wait
status with the code in the high byte, so `exit 3` is 3 on one platform and 768 on the
other. `std/process` works around it by making `run_ok` (`== 0`, which coincides) the
portable API and documenting `run`'s value as platform-specific. Fix is `WEXITSTATUS` in the
helper. Runtime change → mirror + reseed. Also the clearest concrete argument for `sys`.

**2. Close the pointer-to-slice assignability hole.** `fn f(read s: []u8)` called as `f(raw)`
with `raw: *mut u8` **passes typeck** — `jestyrc check` prints "assignability … passed" — and
fails only in gcc as `incompatible type for argument 1`. That is the "degrades to gcc"
failure mode the port work spent real effort eliminating, and it makes `check` a false
negative for an easy mistake (I made it, writing `test_report.finish(c, raw)` against a
changed signature). Probably one arm in the assignability check. Same family as the **OPEN
int→int conversion decision** (`[[jestyr-typeck-assignability]]`) — settle both together.
Typeck change → mirror + reseed.

**3. Stop emitting `@test`/`@bench` items in non-test mode.** The proper fix for the leak in
§5, and it would let library tests be colocated again. Bigger than one predicate: `uses_*`
helper gating, forward declarations and generic-instance collection all scan `@test` bodies,
and it moves the non-test golden for the corpus files carrying `@test` items. Mirror +
reseed.

### §1.4 — Typed `Path` (Tier 2 area 2, the blocked half)

**Do not build this on `distinct` yet.** Probed: `distinct Path = str` compiles, and then
passing a bare `str` where a `Path` is wanted is **accepted** — as is passing an `AccountId`
where a `UserId` is wanted. `distinct` today gives a *name* with **no check**, which is worse
than nothing because it reads as safety.

Enforcement has to come from assignability, which means resolving the open int→int question
first (§1.3.2 — they are the same rule). Until then a typed `Path` ships an API whose central
claim is unenforced.

The language is *ahead* of the library here: `os_str` is already a real distinct primitive
(`os_from_bytes`, `to_str_lossy`, participating in the text-family conversion rules;
`examples/os_str.jtr`).

---

## §2. DEFERRED — with the reason, so it is not re-litigated

Each was considered and rejected *for now*. The reason matters more than the verdict: when
the reason stops holding, the item becomes live.

| Deferred | Why, precisely | Becomes live when |
|---|---|---|
| **Streaming `Reader`** | No partial-read intrinsic, no file handle. Wrapping `read_file`'s slurp in a `Reader` trait ships an API whose central promise — that it streams — is false, and gets rebuilt immediately | §1.2 lands |
| **`BufWriter`** | A handle cannot own borrowed storage, so it needs an `Allocator` → `mem` tier, not `core`. And the one destination that would benefit is stdout, which **already buffers in C stdio** — wrapping it is double buffering with a second copy | A real case appears (socket, compressor) |
| **Error sets on writes** | Fifty `?`s down a formatter is how errors get swallowed, not handled. The one genuinely fallible operation is a final `flush`, and that is where an error set belongs — one fallible call, not N | A fallible write intrinsic exists |
| **`failed()` on `Writer`** | Removed, not shipped: `print_str`/`eprint_str` return nothing so a stream write has no detectable failure, and sink overflow is deliberately the *sink's* business. It could only ever answer `false`, and a query that always says "fine" invites a caller to believe it checked something | Same as above |
| **`sys` tier** | Genuinely blocked on `extern "c"` (design §14, 📐). Until then it is a wrapper around a wrapper — today the platform boundary *is* the closed intrinsic list | `extern "c"` lands |
| **Typed `Path` on `distinct`** | `distinct` is not enforced at argument positions (§1.4) | The int→int assignability decision is settled |
| **A generic collections zoo** | A breadth objection, not an existence objection. One container with a real consumer is a slice | Never as a zoo; one at a time (§1.1) |
| **A package manager** | `ROADMAP.md` calls it ecosystem-premature, and the module-manifest hash DAG covers the real need (a lockfile-lite pinning the build graph) | Deliberately open-ended |
| **Networking / HTTP / TLS** | No async story (📐), no `extern "c"`, and the moment a socket lands the platform boundary stops being optional | After `sys` |
| **JSON / serialization** | Wants the string tier settled; a serializer on today's primitives would be rewritten | After `fmt` (roadmap slice 8) |
| **Iterators / lazy sequences** | A language design question (traits + closures + lifetimes) in a library costume. Answer it in the design, not by shipping a shape we would have to break | A design answer exists |
| **A logging framework** | Wants formatting, time and a global — the third is deliberately absent | Probably never as stated |
| **`unwrap`-style panicking helpers** | Would undercut the error-set design that payloads and `catch \|e\|` exist to serve | Never |

Two refs are safe to delete when you are satisfied: `backup/pre-integration-2026-08-14` and
`claude/jestyr-std-v2-design-4aeb42` (local + remote). The other ~46 `claude/*` branches
predate this work.

---

## §3. DONE — do not rebuild

| Tier 2 area | State |
|---|---|
| 1. Capability handles | ✅ **all four** — `process.Process`, `fs.Fs`, `time.Clock`, `env.Env` |
| 3. Reader / Writer | ✅ **Writer complete** (`sink` core + `writer` std); **Reader is memory-only** (`cursor`) — streaming is §1.2, not a gap in the design |
| 5. Testing / golden | ✅ `std/test` (core) + `test_report` (prints) + `test_fixture` (fetches) |
| 7. Package / build | ✅ *as scoped* — `build.jestyr` (CTFE, effect-free by construction) + module-manifest hash DAG |
| 6. No-std contract | ✅ **both axes checked** — `@no_alloc` *and* `@no_os` (below) |
| 2. Typed path / OsStr | 🟡 `os_str` is a real primitive; **`PathBuf` is BUILT** (below); typed `Path` is §1.4 |
| 4. Collections v2 | 🟡 **`HashMap(K,V)` + `remove` + enumeration + `std/set` BUILT**, with a real consumer; only `Deque(T)` / `SmallVec` remain — §1.1 |

### `std/hashmap` — the generic deterministic map (Tier 2 area 4, first container)

`HashMap(K, V)`: open addressing, power-of-two capacity, cached hashes, 0.7 load cap,
seedless hash — `strmap`'s proven engine, made generic. Consumer on day one:
`hashmap_demo count <file>`, a byte histogram, which is also the CLI the SplitMix64
oracle drives.

Three language limits shaped it, all **probed rather than assumed** — see §1.1 for the
detail. Briefly: multi-bounds don't parse; `Self` isn't a trait-method parameter type; a
generic type can't hold a pointer to another generic type. Hence fn-pointer hash/eq and a
struct-of-arrays layout.

Two smaller findings worth carrying:

* **`take` is what makes an opaque-`V` default returnable.** `get(…, default: V) -> V`
  is refused — the default convention is `read`, and a `read` parameter is a second-class
  borrow that may not outlive the call, which for an opaque `V` the checker cannot see
  past. `take default: V` fixes it. This is the "opaque `T` is non-`Copy`" collision the
  roadmap warns about, in its mildest form: a one-keyword fix, not a fight.
* **A real cgen bug fell out of it** (fixed here, see below): `&mod.f` emitted `(&j_f)`.

### `&mod.f` — taking the address of a module-qualified function (cgen fix)

`&hashmap.hash_i64` lowered to `(&j_hash_i64)` — the spelling of a *local* named
`hash_i64` — because the `UnOp::Ref` arm handled only `ExprKind::Name`, so a qualified
path fell through to the generic field-access path. Resolution, typeck and the escape
checker all passed; **gcc** then failed with "undeclared identifier".

That is precisely the *degrades-to-gcc* failure mode §1.3.2 complains about, and it was
reached by an ordinary API rather than an exotic one — a map parameterized by
`&hashmap.hash_i64` is the natural spelling. Fixed by reusing `info.qualified(id)`, the
same resolution a module-qualified **const** (`mem.PAGE`) already consumed two arms away.

Emission change → **port mirror + reseed**, both paid. The port's `&fn` path took the
same shape (a Field node carries its name span in `(x, y)`, so the module base is simply
skipped — after flattening there are no modules left). Known, pre-existing asymmetry
left alone: neither side canonicalises a *colliding* name in address position, which the
call path does via `call_sym`.

### `std/pathbuf` — the owned, growable path (Tier 2 area 2, the unblocked half)

**The handoff that proposed this was wrong about why it was cheap, and the correction is
the module's whole point.** It said `String` is owned so B1's field auto-drop "frees it".
It does not: B1 recurses into fields that are themselves *droppable*, and `String` is a
primitive with a manual `string_free`. `struct PathBuf { s: String }` with no `Drop` impl
compiles, runs, gives right answers, and **leaks** — measured as zero
`jestyr_rt_str_free` CALL SITES in the emitted C. So `PathBuf` is RAII on a `String`; the
path API is what makes it worth having one.

Sixteen functions, every one `@no_os` (it allocates and never syscalls — the second live
example of the two axes being independent, after `sha256`). Every query DELEGATES to
`std/path`, and `queries_agree_with_std_path` runs both APIs over the same inputs so the
owned type cannot drift from the borrowed one. `set` fills the fresh buffer *before*
freeing the old, which is what makes `pop`/`set_ext` safe when the new value is a view
into the buffer being replaced — pinned by `set_survives_aliasing_its_own_storage`.

New module, imported by nothing in the closure → **no mirror, no reseed**. Added to
`CGEN_GOLDEN_ALLOWLIST`; byte-identical against the self-hosted backend first try.

**One trap worth carrying forward, because it is the second time:** asserting
`c.contains("jestyr_rt_str_free")` to prove the buffer is freed **passes for the leaking
version too**, because the runtime prelude *defines* that helper in every program that
mentions a `String`. Count CALL SITES, not substrings. This is the same shape as the
`memcpy` absence already recorded in §5 — the lesson generalises past `memcpy` to any
runtime helper.

### `@no_os` — the freestanding contract, checked (closed Tier 2 area 6)

`@no_alloc`'s sibling on the other axis, built the same way: a per-function flag in the
escape checker, a direct rule, and a transitive closure by free-function name. Both
closures now come from **one** probe pass over the program (`effect_closures` +
`shortest_chains` in `src/escape.rs`), so adding a third proven-absence contract costs a
seed set, not another traversal.

The OS boundary is a closed list — with no `extern "c"`, the intrinsic set *is* the
platform: files, process/args/env, the clock, the print family, **and threads**
(`spawn`, `par for`). Carried on every function in `core`, `sha256`, `path`, `str`,
`test`, `sink`, `cursor`. Full table in `docs/attributes.md`.

**Two things worth knowing before extending it:**

* **The threads row was not in the original plan, and it is the whole reason the
  attribute earned its keep.** Annotating `core.jtr` wholesale is what surfaced that
  `par_binned_sum` and `par_reduce` spawn workers — so the first definition
  (files/process/env/clock/stdio) would have certified "freestanding" for a function
  starting four pthreads. They are now the two argued exceptions, each with a `@no_os`
  serial twin (`f64_binned_sum`, `serial_reduce`) that is bit-identical. The same two
  functions also allocate, so `core.jtr`'s old header — "nothing here allocates" — had
  been false for as long as they had been in the file.
* **`@no_os` and `@no_alloc` are orthogonal and must stay so.** `sha256` allocates and
  is OS-free; `sink` is neither. `neither_contract_judges_the_other_axis` pins both
  directions, so neither contract can acquire the other's rules by osmosis.

The port mirror **was** owed and is built: `escape.jtr` already mirrored `@no_alloc`'s
direct rule, so `@no_os`'s direct rule is mirrored to match, leaving the asymmetry at
exactly "transitive is reference-only" for both. `jestyr_no_os_matches_reference` proves
agreement on all three enforcement shapes (an intrinsic call, `spawn`, `par for`) plus a
clean control.

Tests: `escape::tests::no_os_*` (11), `no_os_props` (3 properties incl. every intrinsic
name), `no_os_tier` (the library-coverage pin + its anti-vacuity control),
`no_os_does_not_see_through_a_trait_method_either`, `jestyr_no_os_matches_reference`.

**The roadmap's priority list is out of free slices.** Slices 1–6 (`path`, `test`,
`process`, `str`, `env`, `time`) are done; 7 (`fs` expansion), 8 (`fmt`) and 9 (`sys`) each
pay a new intrinsic or are blocked.

Also landed, because the library work forced them: `jestyrc test <file>` scoped to the named
module; module `const`s canon'd by module; `[]T` range-slicing; `std/path`'s suite moved to
a sibling file. And from the other line of work that merged in: **a `take` parameter is
dropped by its callee** (`3195ee1`) and **use-after-`take` of a droppable is refused**
(`c341703`) — relevant because it changes RAII behaviour under anything new you write that
moves ownership into a function.

Three documents carry the arguments, worth reading before touching their areas:
`docs/stdlib-roadmap.md` (tiers, priority, what stays out), `docs/io-design.md` (the four IO
decisions **and the two that implementation corrected**), and this note.

---

## §4. The verification gate

**Verify the tax rather than assuming it** — four compiler changes on this branch, and only
one owed a mirror.

| Change | Port mirror in `cgen.jtr`? | Reseed? |
|---|---|---|
| New `examples/std/*.jtr` that no closure module imports | no | no |
| **Library** code added to a closure module (`fs`, `env`, …) | **no** | **yes** |
| **Emission or typeck behaviour** change | **yes** | **yes** |
| New intrinsic | yes (the eleven-site recipe) | yes |

The closure is exactly twelve modules, hardcoded as `SELFHOST_MODULES` in
`src/proptests.rs`: `mem, intern, fs, env, list, tokens, parser, ctfe, typeck, escape,
sha256, cgen`. Library code in one changes the *flattened source*, not the compiler's
behaviour — hence reseed, no mirror.

```bash
REFRESH_SEED=1 cargo test --release --features "c-oracle,selfhost-fixpoint" bootstrap_seed_is_current
```

Two facts worth having: a **comment-only** edit to a closure module still forces a reseed
(`flatten_selfhost_concat` edits raw source spans, so it preserves comments); and
`docs/TESTING.md`'s "any change to `examples/std/*.jtr` forces a reseed" is conservative
shorthand that is technically wrong.

New modules should be added to `CGEN_GOLDEN_ALLOWLIST` — opt-in byte-identity against the
self-hosted backend. Measure rather than assume; every file added on this branch passed
first try. **Treat that list carefully: a dropped entry does not error, it silently stops
verifying a file.**

---

## §5. Traps

* **A colocated `@test` is emitted into every consumer.** A `@test` fn is an ordinary
  function with an attribute and there is no dead-code elimination in the C backend.
  Measured on `path_demo.c`: **1,087** lines with `std/path`'s original colocated tests →
  **2,789** once they used `std/test_report` (pulling in `printf`) → **744** with the suite
  in a sibling `path_test.jtr`. A `core` module's own test scaffolding silently breaks its
  tier claim. **Convention: a module with non-test consumers puts tests in a sibling
  `*_test.jtr`;** a module only ever imported *by* tests (`std/test`) may colocate. Proper
  fix is §1.3.3.
* **`@no_alloc` passes VACUOUSLY through a trait method.** It accepts a `@no_alloc` function
  writing through a trait whose impl allocates on every call, while correctly rejecting the
  direct-call control. Not a weak proof — a *false* one. This is why the `Writer` trait is
  `std` while `Sink`/`Cursor` are `core`. Pinned by
  `no_alloc_does_not_see_through_a_trait_method`.
* **A handle cannot own borrowed storage.** A Jestyr borrow is second-class, so storing a
  `mut []u8`/`read str` **parameter** in a struct is refused. Counters in the handle, storage
  with the caller. The field *declaration* is fine — the refusal is at the store.
* **Keywords cost API names.** `out` and `take` are reserved; a *local* named `out` gives
  E0007 at every use, and `fn take(…)` is a parse error. **Enum variants are bare identifiers
  in a shared namespace** and cannot be keywords — hence `stdout_target`/`stderr_target`
  rather than claiming `stdout` from every consumer.
* **Some builtins are for-loop forms, not calls** — `split` and `graphemes`. Do not
  reimplement them and do not name a function either (`grapheme_count` is the counting
  spelling). `std/str`'s header documents `split`'s measured semantics.
* **`env.argc()`/`argv()`/`program()` read 0/empty inside a `@test`** — the harness emits
  `int main(void)`, so the runtime never records arguments. Environment *variables* are
  unaffected (`getenv` bypasses `main`).
* **A closure module's NAME is reserved across the whole flattened compiler.** Grep
  `\bNAME\.` in every closure module before adding an import — the `cgen.jtr` → `std/path`
  migration was tried and reverted for exactly this.
* **Commit a resolved merge before doing anything else.** The resolution lives only in the
  index, so `merge --abort` destroys it with nothing in the reflog. When several worktrees
  share one clone, `git status` in the *other* checkout is part of reading the situation —
  `MERGE_HEAD` existing is a very different fact from "22 dirty files".
* **Generated files can auto-merge correctly — check, don't assume either way.** I expected a
  3-way merge of two independently regenerated 28,000-line bootstrap files to be garbage. It
  was exact, and `bootstrap_seed_is_current` proves it in 18 s. (Converse still holds: if it
  fails, regenerate, never hand-merge.)
* **Don't assert a global absence to prove a local property.** Asserting the emitted C had no
  `memcpy` to prove a slice view is copy-free failed — the runtime prelude has `memcpy`.
  Assert the *presence* of `.ptr + _lo`.
* **A differential test cannot catch a bug both sides share.** `normalize` compared the
  output's last two bytes instead of the whole segment; the Rust oracle written from the same
  spec had the identical flaw. Keep worked examples and adversarial reading alongside
  differential agreement.
* **A differential can also be wrong about a correct module.** `str`'s first version stripped
  trailing `\r`/`\n` from stdout greedily, so `after("\r", "")` — correctly the whole string
  — compared as empty and failed correct code. Strip exactly the one terminator `print_str`
  appends.
* **Pair every refusal test with a positive control.** "The write was refused" means nothing
  unless the same write through a `host()` handle lands. Flipping `process.denied()` to
  permit kills 4 of 7 tests — that is the mutation check worth running on any new capability.
* **Write tests that check YOU, not just the code.** Two on this branch earned their keep:
  one pinned a hand-written ASCII whitespace set against the `trim` intrinsic's definition
  (they agreed — the value is that they now cannot drift), the other pinned `split`'s
  documented semantics so prose cannot drift from the language.
* **When a documented limitation is load-bearing, pin it with a test** that must be changed
  deliberately (`diff_count_is_aligned_not_an_edit_script`,
  `array_range_slicing_is_still_refused`).
* **A header comment claiming a property is evidence the property is false.** `core.jtr`
  said "nothing here allocates and nothing here syscalls"; annotating it proved both
  halves wrong for the same two functions (`par_binned_sum`, `par_reduce`). The comment
  had survived every review of the module *because* reviewers read it as settled. When
  you find a prose claim doing load-bearing work, the cheapest way to find out whether it
  is true is to make the compiler assert it and see what falls over.
* **An effect contract that inspects only call *names* misses effects carried by
  syntax.** `@no_alloc` hooks `region` blocks as well as intrinsics; `@no_os` had to hook
  `spawn` and `par for` for the same reason. Ask "what can this effect ride in on that
  is not a call?" before declaring the intrinsic list closed.
* **A closed list is a vacuity hazard.** A name mistyped into `is_os_intrinsic` would not
  fail to compile — it would silently stop being checked, for that intrinsic only.
  `no_os_props::os_touching_body_is_always_rejected` exercises every name against the
  real checker, which is the only thing that turns that into a red test.
* .jtr subset traps for closure modules: a `for` condition cannot start with `(`; a bare `{`
  after a call-init parses as the ctor form; never chain `string_view(x).len`. **Author `.jtr`
  with Write, not shell heredocs** — heredocs mangle backslashes.
* **Windows has no `python3`** in this environment, and multi-edit scripting through it
  fails silently late. Use the editing tools or `sed`.

---

## §6. Suggested order

1. **The assignability hole + int→int decision (§1.3.2)** — settle them together; it also
   unblocks §1.4.
2. **The partial-read intrinsic (§1.2)** — the largest, and the only route to a real
   streaming `Reader`.

Leave `sys` and typed `Path`-on-`distinct` alone until their blockers actually move. Both
look like library work and are not.
