# Std v2 Tier 2 — what remains, and what I verified about it

Cold-start note for the next session. Four of the seven Tier 2 areas are done; this
covers the other four, plus two standing compiler follow-ups. Written immediately
after finishing the capability handles, so the measurements in it are fresh.

**Read §0 first — the work is on a branch that has not merged.**

---

## §0. READ FIRST: the branch state

Everything below assumes you are working from **`claude/jestyr-std-v2-design-4aeb42`**,
which holds **nine commits that are not on `master`**:

```
7618933  stdlib: Fs, Clock and Env -- all four Tier 2 capability handles now exist
897d35d  stdlib: std/test_fixture + every-differing-line goldens -- the last three gaps
fe49492  cgen: range-slice a []T, and take the raw pointer out of std/test_report
e7462b6  cgen: canon module consts, so two modules may share a const name
cce5df6  stdlib: std/process -- the first Tier 2 capability handle
c194a73  stdlib: adopt std/test in std/path -- and stop leaking test code into consumers
5b7062e  test harness: scope `jestyrc test <file>` to the named module
2b0ff09  stdlib: std/test -- expectations and golden compare, split across the tier line
         (8ac867c = master's tip when this started)
```

They are a clean linear fast-forward. The merge was **blocked, not forgotten**: the
main checkout at `C:/Users/adame/Jestyr` was mid-flight on its own closure change —
22 dirty files including `src/cgen.rs`, `examples/std/cgen.jtr`, an in-progress
bootstrap reseed, and **`src/proptests.rs`, which these commits also touch**. Merging
into that tree would have conflicted and risked corrupting the reseed.

**First action of the next session:**

```bash
cd C:/Users/adame/Jestyr && git status --short && git log --oneline -1
```

* Clean and still at `8ac867c` → `git merge --ff-only claude/jestyr-std-v2-design-4aeb42`.
* Clean but *ahead* → that session landed. Rebase this branch onto it. Expect
  conflicts in `src/proptests.rs` and `examples/std/cgen.jtr`, and note that
  `bootstrap/jestyr_seed.c` + `jestyr_flat.jtr` are **generated** — do not hand-merge
  them, take either side and re-run `REFRESH_SEED=1` (§2).
* Still dirty → leave it alone and keep working on the branch.

---

## §1. What is already done — do not rebuild it

| Tier 2 area | State |
|---|---|
| 1. Capability handles | ✅ **all four.** `process.Process`, `fs.Fs`, `time.Clock`, `env.Env` |
| 5. Testing / golden | ✅ `std/test` (core) + `test_report` (prints) + `test_fixture` (fetches) |
| 7. Package / build | ✅ *as scoped* — `build.jestyr` (CTFE, effect-free by construction) + module-manifest hash DAG. A package manager is deliberately out |
| 6. No-std contract | 🟡 documented, partly checked — §3.1 |
| 2. Typed path / OsStr | 🟡 language ahead of library — §3.2 |
| 3. Reader / Writer | 🔴 nothing built — §3.3 |
| 4. Collections v2 | 🔴 nothing built — §3.4 |

Also landed on the way, because the library work forced them: `jestyrc test <file>`
scoped to the named module; module `const`s canon'd by module; `[]T` range-slicing;
and `std/path`'s suite moved out of the module (see the leak trap in §5).

The narrative for all of it is in `docs/stdlib-roadmap.md`, which is the authoritative
document — this note is the *forward-looking* half and will go stale faster.

---

## §2. The verification gate

Build and the three rungs:

```bash
cargo build --release
cargo test --release --features "c-oracle,selfhost-fixpoint"
```

Baseline at the time of writing: **1135 passed, 0 failed, 3 ignored** (~350 s).
Reseed, when owed:

```bash
REFRESH_SEED=1 cargo test --release --features "c-oracle,selfhost-fixpoint" bootstrap_seed_is_current
```

### The tax rules, corrected

I got this wrong once mid-session, so it is stated carefully:

| Change | Port mirror in `cgen.jtr`? | Reseed? |
|---|---|---|
| New `examples/std/*.jtr` no closure module imports | no | no |
| **Library** code added to a closure module (`fs`, `env`, …) | **no** | **yes** |
| **Emission or typeck behavior** change | **yes** | **yes** |
| New intrinsic | yes (the 11-site recipe) | yes |

The closure is exactly twelve modules, hardcoded as `SELFHOST_MODULES` in
`src/proptests.rs`: `mem, intern, fs, env, list, tokens, parser, ctfe, typeck,
escape, sha256, cgen`. Adding library code to one changes the *flattened source*, not
the compiler's behavior — that is why it owes a reseed but no mirror. `Fs` + `Env`
cost +192 lines of flat source and +163 of seed C; `time` was free (not in the
closure).

Two further facts worth having: a **comment-only** edit to a closure module still
forces a reseed (`flatten_selfhost_concat` applies span edits to raw source, so it
preserves comments); and `docs/TESTING.md`'s "any change to `examples/std/*.jtr` forces
a reseed" is conservative shorthand that is technically wrong.

---

## §3. The four remaining areas

### §3.1 — No-std contract (area 6): documented, partly checked

**State.** The tier model (`core` / `mem` / `std` / `sys` / `parallel`) is written up in
`docs/stdlib-roadmap.md`, and parts of it are now *enforced* rather than asserted:

* `@no_alloc` on every function in `path.jtr` and `test.jtr` makes "never allocates" a
  compiler-checked property. Its blind spot is real and documented: it resolves the
  call graph **by free-function name**, so it does not see through a method, a
  closure, or a `fn(…)` pointer — code allocating through the `Allocator` vtable
  passes.
* `path_stays_a_leaf_module` and `process_ships_no_tests_in_the_module` check the
  *leak* boundary (§5).

**What is missing is a checked OS boundary.** There is no `@no_os` analogue, so
"`core` links on a freestanding target" is convention. `sys` does not exist and is
genuinely blocked on `extern "c"` (design §14, 📐) — until then it would be a wrapper
around a wrapper, which the roadmap argues at length.

**First increment, if you want progress here without waiting for `extern "c"`:** add a
`@no_os` attribute mirroring `@no_alloc`'s implementation — the intrinsic list is
closed and short (`arg`, `arg_count`, `read_file`, `try_read_file`, `write_file`,
`file_exists`, `remove_file`, `run_command`, `eprint_str`, `mono_nanos`, `env_var`,
plus the print family), so the check is "does the call graph reach one of these".
Then put it on `core.jtr`, `path.jtr`, `test.jtr`, `sha256.jtr` and the tier claim
becomes checked. Cost: an attribute + a checker pass, both sides, **emission-neutral**
so probably no mirror — but `attrs.rs` is reference-only today, so verify whether
`escape.jtr` needs the mirror before assuming.

Expect the same blind spot as `@no_alloc` and say so in the docs rather than
overclaiming.

### §3.2 — Typed path / OsStr (area 2): the language is ahead, and `distinct` will not do it

**Verified this session, and it changes the plan:**

* `os_str` is **already a real distinct primitive** in the compiler, not a library
  idea: `os_from_bytes(…) -> os_str`, `to_str_lossy(…) -> String`, and it participates
  in the text-family conversion rules (`src/typeck.rs`, the `str`/`String`/`cstr`/
  `os_str`/`Builder`/`Cow` family). `examples/os_str.jtr` is the demo — WTF-8 platform
  bytes you have not proven valid.
* **`distinct` is NOT an enforced newtype today.** I probed it: `distinct Path = str`
  compiles, and then `base_of(s)` with a bare `str` is **accepted**; so is passing an
  `AccountId` where a `UserId` is wanted (`distinct UserId = i32`,
  `distinct AccountId = i32`). Building `Path` on `distinct` would give you a *name*
  with **no check** — worse than nothing, because it would read as safety.
  See `[[jestyr-typeck-assignability]]`, which records the int→int conversion decision
  as explicitly OPEN; this is the same permissiveness.
* A struct *field* of type `str` **is** declarable (`struct PathView { s: str }`
  compiles). The refusal is at the **store site**, not the declaration: storing a
  second-class borrow (a `read str`/`mut []u8` **parameter**) into a struct that
  outlives the call is what the escape checker rejects. So `Path` built from a literal
  is fine and `Path` built from a function's `read str` parameter is not.

**Therefore the honest sequencing is:**

1. Decide what `Path` is *for*. If it is "don't confuse a path with arbitrary text at
   an OS boundary", the enforcement has to come from assignability, so **fix `distinct`
   first** — and that means resolving the open int→int conversion question, because the
   two are the same rule.
2. `PathBuf` (owned) is easier and independent: `struct PathBuf { s: String }`
   compiles today, `String` is owned so there is no borrow problem, and RAII already
   recurses into owned struct fields (B1 field auto-drop), so it would free itself.
   That is a genuinely cheap, useful increment — an owned, growable path — and it does
   not wait on `distinct`.
3. Keep `std/path`'s lexical layer exactly as it is. It is `@no_alloc`-proven,
   view+buffer based, and the typed layer should sit *above* it rather than replace it.

**Do not** convert `std/path` to typed `Path` before step 1, or you will ship an API
whose central claim is unenforced.

### §3.3 — Reader / Writer (area 3): NOT language-blocked. I was wrong about this

I told the user earlier that this was gated on the traits/generics design. **I probed
it and that is false.** A `Writer`-shaped trait works today in all three forms:

```jestyr
trait Sink {
    fn put(mut self, b: u8) -> bool     // a MUTATING trait method
    fn written(read self) -> i64
}
// two impls with observably different behavior (a counter and a capped sink)
fn fill[T: Sink](mut s: T, count: i64) -> i64 { … }   // generic, static dispatch ✓
var d: dyn Sink = c2                                   // erased receiver ✓
d.put(66)                                              // mutation through dyn ✓
```

All three printed the right answers (`5`, `3`, `1` — the capped sink refusing past its
limit is what shows the impls are really distinct). Traits carry error sets in their
signatures too (`ledger.jtr`), `dyn` is a fat pointer with a compiler-built vtable
byte-compatible with a hand-written one (`dyn_dispatch.jtr`), and operator traits
exist (`operators.jtr`).

**So this area is blocked on DESIGN DECISIONS, not capability.** The four to settle
before writing code:

1. **Trait or vtable struct?** `mem.jtr`'s `Allocator` is a hand-written struct of
   `fn(…)` pointers, and its header explains why — it had to exist *before* traits
   did. Traits exist now. Pick one and be consistent, because a `Writer` trait next to
   an `Allocator` vtable is two idioms for one job.
2. **Where does buffering live?** A `BufWriter` wrapping a `Writer` needs to own a
   buffer, and a handle **cannot own borrowed storage** (§5) — so either it owns heap
   memory through an `Allocator` (making it `mem`-tier, not `core`) or the caller
   passes the buffer at every call, as `std/test`'s sink does.
3. **How do errors propagate?** `write` returning `bool` is what `fs.write` does today;
   error sets (`-> usize !{ IoError }`) are the richer option and traits support them.
   Choose once — this is the decision that is expensive to change later.
4. **Is `core` allowed a `Writer`?** `std/test`'s sink is `core` and `@no_alloc`
   because it writes into a caller `[]u8`. That is the shape that keeps a `Writer`
   usable on a freestanding target, and it is worth preserving.

**First increment:** promote the sink already inside `test.jtr` (`put`/`puts`/`line`,
which its own header flags as "the smallest thing that could be called a writer… when
a real `Writer` lands these three functions are what it replaces") into a `core`
`std/io_write.jtr` over a caller buffer, then make `test_report` and `io.jtr` consume
it. That is additive, keeps `core` heap-free, and gives you a real consumer
immediately — which is how every successful slice on this branch went.

### §3.4 — Collections v2 (area 4): the blocker is narrower than the roadmap says

**State.** `List(T)` (generic, allocator-parameterized, RAII-freed), `StrMap`
(string keys → `i64`), `intern`. No generic `HashMap(K,V)`, `Set(T)`, `Deque(T)`,
`SmallVec`.

**The roadmap says** generic containers "keep colliding with the escape checker's
treatment of opaque `T` as non-`Copy`; each new one is a fight, not a fill-in." That
is directionally right but reads as broader than it is: **generic containers demonstrably
work** — `genlist.jtr`, `vec_generic.jtr`, `list.jtr` are all in the corpus and in the
byte-identity allowlist. `container.jtr` is a **deliberate rejection demo**: what is
refused is storing a *borrow* in a collection, which is correct and should stay
refused.

**So the real questions are these, and they are design not capability:**

1. **Deterministic hashing.** The prompt's Tier 2 spec says deterministic by default,
   randomized only explicitly. `sha256.jtr` exists in-language and is already used as
   the cross-OS determinism canary, so a deterministic hasher has a proven,
   byte-exact primitive to build on. Decide the hash *function* before the map, and
   pin it in the canary so a change to it is a visible break.
2. **`K` needs equality and hashing.** That is a trait bound (`[K: Hash + Eq]`), and
   trait bounds work (`ledger.jtr`'s `[T: Account]`). Verify the bound composes for a
   *generic struct's* type parameter before committing — `gen_vtable.jtr` is the
   nearest precedent.
3. **Allocator-explicit, like `List(T)`.** `List` stores its `Allocator` and frees via
   a blanket `impl[T] Drop for List(T)`. Copy that shape exactly; it is proven.
4. **Pick ONE.** The roadmap's "no generic collections zoo" objection is about
   breadth, not existence. A single deterministic `HashMap(K,V)` with a real consumer
   is a slice; four containers with none is the zoo it warns against.

**First increment:** a deterministic `HashMap(K,V)` modelled on `strmap.jtr`'s open
addressing (it already caches hashes so a grow can re-place without re-hashing) but
generic in the key, with the hash function fixed and canaried. Its first consumer
should be a real one — `strmap.jtr`'s users, or the compiler's symbol table.

---

## §4. Two standing compiler follow-ups

Both recorded in `docs/stdlib-roadmap.md`'s follow-up list; neither blocks the above.

1. **Normalize `run_command`'s exit status.** The runtime helper is
   `return (int32_t)system(cp)` — raw. Windows gives the exit code; POSIX specifies a
   *wait status* with the code in the high byte, so `exit 3` is 3 on one platform and
   768 on the other. `std/process` works around it by making `run_ok` (`== 0`, which
   coincides on both) the portable API and documenting `run`'s value as
   platform-specific. The fix is `WEXITSTATUS` in the helper: a runtime change, so it
   owes the `cgen.jtr` mirror **and** a reseed. It is also the clearest concrete
   argument for the `sys` tier.
2. **Stop emitting `@test`/`@bench` items in non-test mode.** This is the proper fix
   for the leak in §5 and would let library tests be colocated again. Bigger than one
   predicate: `uses_*` helper gating, forward declarations, and generic-instance
   collection all scan `@test` bodies, and it moves the non-test golden for the three
   corpus files that have `@test` items — so mirror + reseed.

---

## §5. Traps, consolidated

Things that cost time this session and will cost it again.

* **A colocated `@test` is emitted into every consumer.** A `@test` fn is an ordinary
  function with an attribute and there is no dead-code elimination in the C backend.
  Measured on `path_demo.c`: **1,087** lines with `std/path`'s original colocated
  tests → **2,789** once those used `std/test_report` (pulling in `printf`) → **744**
  with the suite in a sibling `path_test.jtr`. A `core` module's own test scaffolding
  silently breaks its tier claim. **Convention: a module with non-test consumers puts
  tests in a sibling `*_test.jtr`.** A module only ever imported *by* tests
  (`std/test`) may colocate.
* **A capability handle cannot own borrowed storage.** A Jestyr borrow is
  second-class, so storing a `mut []u8`/`read str` **parameter** into a struct is
  refused ("a stored value must outlive the call"). Counters go in the handle, storage
  stays with the caller. The field *declaration* is fine — the refusal is at the store.
  `strmap.jtr` pays `unsafe` + raw pointers instead.
* **`out` is a reserved keyword.** A *local* named `out` gives `E0007: expected an
  expression` at every use; a parameter cascades worse. Cost me a rename mid-file.
* **`env.argc()`/`argv()`/`program()` read 0 and empty inside a `@test`** — the harness
  emits `int main(void)`, so the runtime never records arguments. Environment
  *variables* are unaffected (`getenv` bypasses `main`). Pinned by
  `argv_is_invisible_to_the_test_harness`.
* **A closure module's NAME is reserved across the whole flattened compiler.** Grep
  `\bNAME\.` in every closure module before adding an import — the `cgen.jtr` →
  `std/path` migration was tried and reverted for exactly this.
* **Don't assert a global absence to prove a local property.** I asserted the emitted C
  contained no `memcpy` to prove a slice sub-view is copy-free; it failed, because the
  runtime prelude has `memcpy` for unrelated reasons. Assert the *presence* of
  `.ptr + _lo`.
* **A differential test cannot catch a bug both sides share.** `normalize` compared the
  output's last two bytes instead of the whole segment; the Rust oracle written from
  the same spec had the identical flaw. Keep worked examples and adversarial reading
  alongside differential agreement.
* **Pair every refusal test with a positive control.** "The write was refused" means
  nothing unless the same write through a `host()` handle lands. Flipping
  `process.denied()` to permit kills 4 of 7 tests — that is the mutation check worth
  running on any new capability.
* **When a documented limitation is load-bearing, pin it with a test** that must be
  changed deliberately (`diff_count_is_aligned_not_an_edit_script`,
  `array_range_slicing_is_still_refused`).
* .jtr subset traps for closure modules: a `for` condition cannot start with `(`; a
  bare `{` after a call-init parses as the ctor form; never chain `string_view(x).len`.
  Author `.jtr` with Write, not shell heredocs — heredocs mangle backslashes.

---

## §6. Suggested order

1. **Merge or rebase (§0).** Nothing else matters until the nine commits are safe.
2. **`std/str`** — the last *free* slice on the roadmap's priority list, and it makes
   every later slice easier. Not a Tier 2 area itself, but cheap and load-bearing.
3. **`Reader`/`Writer` (§3.3)** — highest value of the four, and now known not to be
   language-blocked. Settle the four design decisions in writing *first*.
4. **`@no_os` (§3.1)** — turns the no-std contract from convention into a check, and is
   small.
5. **`PathBuf` (§3.2 step 2)** — owned, independent of the `distinct` question.
6. **`HashMap(K,V)` (§3.4)** — after the hashing decision, with a real consumer.

Leave `distinct`-enforced `Path` and `sys` alone until their blockers (the int→int
assignability decision; `extern "c"`) are actually resolved. Both are the kind of thing
that looks like library work and is not.
