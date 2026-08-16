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

**AREA 4 IS NOW COMPLETE as scoped**: `HashMap` (+ `remove`, enumeration), `set`,
`Deque(T)` and `SmallVec(T)` all exist, each with tests and a consumer.

My prediction that `SmallVec` would hit the "type-expressions" refusal was **wrong** —
an inline `[8]T` inside a generic type is fine, because a fixed-size array of a type
PARAMETER is not another generic instantiation. What it hit instead was three other
things, all worth knowing:

* **A comptime VALUE parameter is not accepted in type position.** `SmallVec(i64, 4)`
  → "expected a type, found `int`", so the inline capacity is a fixed constant, not a
  caller-chosen `N`.
* **An opaque `T` cannot be returned from an array field by value.** `return v.buf[i]`
  reads as letting a borrow of the receiver escape. The fix is the one the diagnostic
  suggests: declare the return `-> read T`. Two other routes — coercing the array to a
  slice for its `.ptr`, and `&v.buf` — **passed `jestyrc check` and then failed in
  gcc**, which is §1.3.2's hole hit twice in ten minutes. **Probe with `run`, never
  `check`.**
* **A real cgen bug** (fixed here): a generic struct's array FIELD never got its
  `JestyrArr_<T>_<N>` typedef emitted, so the struct referenced a type nothing defined.
  Single-file programs hid it — an inline `[0; 8]` literal in a non-generic caller is
  concrete and the per-expression scan caught it; move the constructor into an imported
  module and the literal moves into the generic body, where its type is `[8]T` and not
  concrete. **The module boundary was the trigger.**

**~~The one debt this leaves~~ — PAID, and it was worse than recorded.** The note said the
port "cannot compile" `smallvec` and framed the mirror as adding one missing scan. Both
halves were wrong, and the way they were wrong is the transferable part.

**It compiled fine and produced a silently wrong program.** The port did not omit
`JestyrArr_i64_8`; it emitted `JestyrArr_T_8`, defined as `int a[8]` — the opaque `T`
falling back to `int` — and used it for the field, both index forms and the repeat
literal. gcc accepted all of it, because `int` is a real type. A `Holder(i64)` storing
`9000000000` read back **`410065408`**: the field was 32 bits wide and every element
truncated. A missing typedef is a link error you cannot miss; a *wrong* typedef that
names something valid is a miscompile. **The `int` fallback for an unresolved opaque type
is what converts the first into the second** — worth remembering anywhere the port renders
a type it might not have substituted.

**And it was five call sites, not one scan.** The collection pass was the smallest part:

| site | was | now |
|---|---|---|
| the array typedef itself | (never emitted concretely) | new generic-struct pass over `g.gsi` |
| `emit_one_array_def` | emitted `JestyrArr_T_8` | `tyid_concrete` screen — mirrors `collect_arrays`' `is_concrete` |
| the struct instance's field | `emit_gs_ty` had no Array arm | Array arm via `push_gs_mangle` |
| both array-index forms + the repeat literal (5 calls) | `emit_ty_c` (unsubstituted) | `emit_su_tyid` (+ its new Array arm and `push_su_tymangle`) |

The root cause is one shape repeated: the port keeps a substituted renderer and an
unsubstituted one side by side, and **the array paths reached for the wrong one**.
`emit_su_tyid` handled checker kinds 4/6/3 (Opaque, GenStruct, Ptr) and simply had no
Array arm, so every array fell through to `emit_ty_c`. If you add a type form to the port,
check `emit_su_ty`/`emit_su_tyid`/`emit_gs_ty` as a *set* — a form handled by one and not
the others is exactly this bug.

`smallvec.jtr`/`smallvec_test.jtr` are now in `CGEN_GOLDEN_ALLOWLIST`, the mirror and the
reseed are paid, and `jc` compiles them.

**The pin is new, because no existing gate could hold this.** `jestyr_cgen_matches_reference`
compiles every corpus file **with no import resolution**, and the single-file form is
exactly the one where an inline `[zero; 8]` literal is concrete and the bug disappears — so
the corpus golden is *structurally* blind here, and adding files to the allowlist does not
help. `jestyr_driver_generic_struct_array_field_matches_reference` goes through `jc`'s own
loader and gcc driver instead, and asserts three things: byte-equality with the reference,
the presence of `JestyrArr_i64_8` with the absence of `JestyrArr_T_8`, and — because
§5 warns a differential cannot catch a bug both sides share — that the built binary
**prints `9000000000` back untruncated**.

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

### §1.2 — ~~A partial-read intrinsic~~ — DONE, and NO INTRINSIC WAS NEEDED

**`std/file` streams, and the compiler gained no file handle, no `read_into`, and no
eleven-site intrinsic.** `fread` already existed; what was missing was only a way to spell
its arguments. Two small language facts closed it — `cptr` and header-declared externs,
both in §1.5 — and the resource-with-a-lifetime question landed where it belongs, in the
library, as a `Drop` impl over a handle.

`Reader` is a concrete type with `open`/`is_open`/`read_into`/`at_eof`/`failed`/`close`,
RAII teardown, five tests and a demo whose whole point is that its buffer is 32 bytes: the
demo's peak memory does not depend on the file's size, which is the one thing a streaming
reader offers and the one thing a slurping demo could not show. Its byte/line/longest-line
counts are checked against `wc`/`awk`.

There is deliberately **no `Reader` trait** — that is still `docs/io-design.md`'s open
four-decision question, and shipping a trait to have one would be the "iterators in a
library costume" mistake. And deliberately **no error set**: `fread` reports a short count
and `ferror` reports why, which is two calls rather than fifty `?`s.

**The one real limitation, recorded rather than papered over:** `open` takes a `cstr`, and
the only way to build one today is `.cstr` on a string LITERAL — so a path computed at
runtime cannot be opened. That belongs to the string tier (a `String -> cstr` bridge with a
NUL guarantee), not to this module.

The original text follows.

### §1.2 (original) — A partial-read intrinsic. The thing that unblocks a streaming `Reader`

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

**1. ~~Normalize `run_command`'s exit status~~ — DONE.** The helper was
`return (int32_t)system(cp)`, raw: Windows gives the exit code, POSIX a wait status
with the code in the high byte, so `exit 3` was 3 on one platform and 768 on the other.
It now applies `WEXITSTATUS` on POSIX, so **`run` returns the exit code everywhere** and
`-1` is reserved for "did not exit normally" (signalled, or no shell) — distinguishable
because real codes are 0..255.

Normalised in the INTRINSIC, not the library, because the library cannot see which
platform it is on: until a `sys` tier exists, the intrinsic list *is* the platform
boundary. `std/process` no longer documents `run` as unusable, and `run_ok` is now a
readability choice rather than the only portable spelling. Runtime change → port mirror
(paid) + reseed (paid). Pinned by `run_reports_the_exit_code_not_a_wait_status`, whose
assertions would have read 768 and 10752 on Linux before the fix — the cross-OS canary
is what makes that test more than a Windows tautology.

**2. ~~Close the pointer-to-slice assignability hole~~ — DONE, together with the
int→int decision it was entangled with.** Both are settled; kept here because the
*method* generalises.

`assignable` judged only primitive-vs-primitive and returned `true` for everything
else, so `f(raw)` against `fn f(read s: []u8)` passed `check` and failed in gcc. Now a
slice-vs-pointer and slice-vs-array mismatch is refused in either direction —
deliberately narrow, covering only pairings with no conversion at all.

**The int→int question was settled by MEASUREMENT — and the measurement itself needed
correcting twice, which is the more useful lesson.**

The old comment said the self-hosted sources "spell it both ways", implying a large
migration. Three successive numbers came out of trying to check that:

* **~5300** — raw diagnostics. Meaningless: a site inside `core` is re-reported for
  every file that imports it, and most of it was `var n: usize = 0`, which the
  literal-defaulting guard already absorbs.
* **6** — distinct sites, per-file, with the literal guard respected. This is the number
  I acted on, and it was WRONG.
* **55** — the true count. Per-file checking **misses module-qualified calls**:
  `list.get(i32, p.roots, r)` is not argument-checked, and only the flatten — where it
  becomes a bare `get__list` — exposes it. My measurement had *the same blind spot as the
  checker it was measuring*, so it under-reported by 9×.

All 55 are `i32 → usize` index arguments, all mechanical, so the decision stands. But the
methodology lesson is the transferable part: **measure on the flattened program, not
per-file**, whenever the question is about calls.

**A second hole fell out of it, unfixed:** module-qualified calls skip argument
assignability entirely. That is why the per-file sweep was clean while the concat was
not, and it is a strictly larger version of the hole this item set out to close. It is
the obvious next increment.

Rule adopted: **lossless widening within one signedness stays implicit; narrowing and
any change of signedness need an explicit `as`.** Widening is permitted rather than
refused because it cannot lose information and the corpus has no such site, so a rule
against it would be pure noise. Reference-only — the port has no assignability check at
all — so no mirror; the six `as usize` edits are in closure modules, hence the reseed.

The old pin `integer_width_changes_are_deliberately_not_reported` said "if Jestyr later
decides these need a cast, this test is the one to invert", and inverting it is exactly
what happened. **That is the pattern worth copying: when you defer a decision, pin the
deferral with a test that names its own successor.**

**3. ~~Stop emitting `@test`/`@bench` items in non-test mode~~ — DONE, but it does NOT
deliver what it was expected to.** `@test`/`@bench` items are now skipped in non-test
mode: at both emission sites, and in the five `uses_*` gates (which scan the FLAT
expression arena, so a `print_str` in a test body would otherwise still switch on the
print runtime for every consumer). Mirror + reseed paid.

**Measured on `path_demo.c`, with `std/path`'s eleven tests colocated:**

| | lines |
|---|---|
| tests in a sibling file (the current convention) | **820** |
| colocated, before this change | **3162** |
| colocated, after this change | **1845** |

So it removes 42% of the penalty — real, and worth having — but colocation is still
2.25× the sibling-file cost, so **the convention stays.** The remaining 1,025 lines are
not the tests: they are the ordinary `pub fn` items of the modules those tests IMPORT
(`std/test`, `std/test_report`). Skipping `@test` items cannot touch those, because
`test_report.finish` is a perfectly ordinary function.

**What would actually free colocation is reachability-based dead-code elimination**, and
that is a different feature entirely — the backend currently emits every item it is
handed. Worth recording plainly, because the follow-up was written as though this change
were the whole fix, and it is not.

### §1.5 — `extern "c"` ALREADY WORKS, and `sys`'s real blocker is something else

**The roadmap says "needs `extern "c"`" in four places and it is wrong.**
`examples/extern_c.jtr` has been in the corpus and the byte-identity allowlist the whole
time; it calls libc's `puts` and `abs` directly. Probed further, and these all work
end-to-end:

* scalar arguments and returns, `cstr` arguments (`.cstr` bridges a literal);
* **raw pointer arguments**, and **out-parameters the OS writes through** —
  `extern "c" fn time(t: *mut i64) -> i64` both reads back and fills a slot.

So the platform boundary is already crossable, and `sys` was never gated on the feature
it is recorded as waiting for.

**The real blocker is that C's own file API cannot be spelled.** Binding
`fopen`/`fread`/`fclose` needs `FILE*`. Jestyr has no opaque pointer type, so the nearest
spelling is `*mut u8`, which emits `uint8_t* fopen(...)` — and that **clashes with the
prelude's `<stdio.h>`**:

```
error: conflicting types for 'fclose'; have 'int32_t(uint8_t *)'
note:  previous declaration ... 'int __cdecl fclose(FILE *_File)'
```

The same clash hits `memset`, `strlen` and anything else already declared by the
prelude's unconditional includes whose signature Jestyr cannot reproduce exactly.
(`abs` works precisely because `i32 -> i32` matches `<stdlib.h>` exactly.)

**So the next increment for `sys` is a `void*`-shaped FFI type**, not `extern "c"`.
`void*` converts implicitly to and from `FILE*` in C, which makes the clash disappear and
the whole stdio family bindable.

> **THAT LAST SENTENCE IS WRONG, and finding out why is the useful part of this
> section.** `cptr` was built, the probe was run, and the clash **did not disappear**:
>
> ```text
> error: conflicting types for 'fclose'
>  int32_t fclose(void* j_f);
>  note: previous declaration ... int __cdecl fclose(FILE *_File)
> ```
>
> C's implicit `void*` conversion applies to **values**, not to prototype compatibility.
> `int(void*)` and `int(FILE*)` are different function types, so a redeclaration in those
> terms conflicts exactly as `uint8_t*` did. **No spelling of the parameter can fix this**
> short of Jestyr having a nominal `FILE` — the type name is not the problem.
>
> **The actual missing piece is not emitting the prototype at all.** Deleting those three
> lines from the generated C by hand made it compile and run first try, which is what
> turned a guess into a finding.

### §1.5 (resolved) — `cptr` + header-declared externs; `sys`'s blocker is GONE

Two changes, both small, and together they make `std/file` (§1.2) pure library code:

**1. `cptr` — C's `void*` as an opaque handle.** A primitive, six sites plus the port pair
(`prim_code`/`prim_name`, the C-type map, the size class). It lowers to `void*`, holds a
`FILE*` without Jestyr knowing what a `FILE` is, and — the load-bearing decision — gets its
**own `PrimFamily::Opaque`** rather than falling into `Text`. `Text` is the family whose
members convert freely into one another (`prim_family(w) == Text` returns `true`
unconditionally in `assignable`), so a `cptr` landing there would have been silently
interchangeable with `str`, `String` and `cstr`.

**2. `extern "<header>.h"` suppresses the prototype.** The string after `extern` says where
the declaration comes from: `"c"` means "nowhere — emit one", a `.h` name means "that header
already did". Keyed on the `.h` suffix rather than on `abi != "c"` so a typo (`extern "cc"`)
falls back to emitting a prototype instead of silently dropping the declaration. The abi
span was already parsed and recorded on **both** sides (`it.w`/`it.u` in the port), so this
cost no parser change anywhere.

**The opacity claim was written before it was true — three holes, all found by probing.**
`std/file`'s header asserts a `cptr` "cannot be dereferenced and cannot have arithmetic done
to it"; when first written that was **false**, which is §5's "a header comment claiming a
property is evidence the property is false", caught this time because the claim was probed
rather than trusted. All three reached gcc rather than the checker:

| hole | why it slipped through |
|---|---|
| `f.*` | fell to `Deref`'s `_ => Ty::Unknown` arm; `*(void*)` is not valid C |
| `f + 1` | binary `+` took the OTHER operand's numeric type, so `(f + 1).*` type-checked as an **`i32` deref** |
| `let p: *mut u8 = f` | the "not modelled yet" default in `assignable` accepted it |

All three are refused now, and the **widening** direction is deliberately left open —
`*mut u8` → `cptr` is how a buffer reaches `fread`, and C performs exactly that conversion.
The asymmetry is the design: widening to opaque is safe, recovering a typed pointer from an
opaque handle is not.

**This also re-plans §1.2.** A streaming `Reader` was blocked on a partial-read
*intrinsic* — the eleven-site recipe plus a reseed. With an opaque pointer type,
`fopen`/`fread`/`fclose` bind directly and the `Reader` needs **no compiler change at
all**. That is a much smaller and more honest route than adding a file handle to the
intrinsic list, and it puts the resource-with-a-lifetime question where it belongs: in
the library, as a `Drop` impl over a handle.

### §1.4 — Typed `Path` (Tier 2 area 2, the blocked half)

**Do not build this on `distinct` yet.** Probed: `distinct Path = str` compiles, and then
passing a bare `str` where a `Path` is wanted is **accepted** — as is passing an `AccountId`
where a `UserId` is wanted. `distinct` today gives a *name* with **no check**, which is worse
than nothing because it reads as safety.

Enforcement has to come from assignability, and **the int→int question that gated it is now
settled** (§1.3.2), so this is no longer blocked — it is merely unbuilt.

**The scope is now MEASURED rather than guessed, and the guess above was right.** One probe
file, three `distinct` types, `jestyrc check`:

| position | verdict |
|---|---|
| `let a: AccountId = 7` | ✅ **refused** — "`distinct` types need an explicit `as`" |
| `takes_path(s)` where `s: str`, param is `Path` | ❌ accepted |
| `takes_uid(n)` where `n: i64`, param is `UserId` | ❌ accepted |
| `takes_uid(a)` where `a: AccountId`, param is `UserId` | ❌ accepted — **two unrelated distinct types interchange** |
| `fn ret_uid(x: i64) -> UserId { return x }` | ❌ accepted |

So `distinct` was enforced at **initializers only**. The notion was sound and the diagnostic
already had the right wording; argument and return positions simply never consulted it.

**BOTH ARE NOW FIXED, and they were indeed one fix** — the commit titled *"`distinct` is a
type everywhere, not just in an initializer"*. Two changes, done
together because neither is much use alone:

1. **`distinct_mismatch` moved into `assignable`**, so every position that goes through
   `check_assignable` — argument, return, initializer — judges it, and
   `check_assignable`'s hint gained a `distinct` arm so the `as` suggestion no longer
   depends on where you are standing.
2. **`resolve_qualified_call` now argument-checks**, closing §1.3.2's own leftover. It had
   checked ARITY ONLY, which is the whole reason the int→int sweep read 6 sites per-file
   and 55 on the flatten.

**The corpus cost was ONE site**, found by the sweep rather than predicted:
`io.print_i32(list.len(i32, xs))` in `examples/std/demo.jtr` — a `usize` narrowing into an
`i32` parameter, invisible until qualified calls were checked. Fixed with `as i32`, the same
way the 55 were. The flattened compiler passes clean, which is the stronger signal: after
the flatten there are no qualified calls left, so a regression there would have been the
*existing* rules breaking.

**Typed `Path` is now buildable and is deliberately NOT built.** Enforcement was the
blocker and it is gone; what remains is a library-design question the handoff never priced —
`std/path`'s queries return `read str` views, so a `distinct Path = str` makes every literal
and every returned view need an `as`, and `pathbuf` delegates to all of them. That is a real
ergonomic bill, and the "no zoo" discipline says price it before spending it. The compiler
half is done; the library half wants a measured cast count first.

The language is *ahead* of the library here: `os_str` is already a real distinct primitive
(`os_from_bytes`, `to_str_lossy`, participating in the text-family conversion rules;
`examples/os_str.jtr`).

---

## §2. DEFERRED — with the reason, so it is not re-litigated

Each was considered and rejected *for now*. The reason matters more than the verdict: when
the reason stops holding, the item becomes live.

| Deferred | Why, precisely | Becomes live when |
|---|---|---|
| ~~**Streaming `Reader`**~~ | ✅ **BUILT** as `std/file` — and with no intrinsic at all: `cptr` + header-declared externs made it library code. The `Reader` **trait** is still deferred (the four-decision question stands); this is a concrete type | done |
| **`BufWriter`** | A handle cannot own borrowed storage, so it needs an `Allocator` → `mem` tier, not `core`. And the one destination that would benefit is stdout, which **already buffers in C stdio** — wrapping it is double buffering with a second copy | A real case appears (socket, compressor) |
| **Error sets on writes** | Fifty `?`s down a formatter is how errors get swallowed, not handled. The one genuinely fallible operation is a final `flush`, and that is where an error set belongs — one fallible call, not N | A fallible write intrinsic exists |
| **`failed()` on `Writer`** | Removed, not shipped: `print_str`/`eprint_str` return nothing so a stream write has no detectable failure, and sink overflow is deliberately the *sink's* business. It could only ever answer `false`, and a query that always says "fine" invites a caller to believe it checked something | Same as above |
| ~~**`sys` tier**~~ | ✅ **UNBLOCKED.** Blocked on neither `extern "c"` (already worked) nor, as it turned out, on an opaque pointer type alone — `void*` does not fix a prototype clash. **Prototype SUPPRESSION did** (`extern "stdio.h"`), and `std/file` is the proof that the whole stdio family now binds. What remains is scope, not capability: which syscalls `sys` should expose | now — pick the surface |
| **Typed `Path` on `distinct`** | `distinct` is not enforced at argument positions (§1.4). **Its stated trigger — the int→int decision — has now FIRED**, so this is live: the next step is to check whether `distinct` follows the same `assignable` path the int rule now judges | NOW — the blocker is cleared |
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
| 3. Reader / Writer | ✅ **Writer complete** (`sink` core + `writer` std); **Reader complete for files** — `cursor` over memory, `std/file` streaming from disk. Only the `Reader` *trait* is open, and deliberately |
| 5. Testing / golden | ✅ `std/test` (core) + `test_report` (prints) + `test_fixture` (fetches) |
| 7. Package / build | ✅ *as scoped* — `build.jestyr` (CTFE, effect-free by construction) + module-manifest hash DAG |
| 6. No-std contract | ✅ **both axes checked** — `@no_alloc` *and* `@no_os` (below) |
| 2. Typed path / OsStr | 🟡 `os_str` is a real primitive; **`PathBuf` is BUILT** (below); typed `Path` is §1.4 |
| 4. Collections v2 | ✅ *as scoped* — `HashMap(K,V)` (+ `remove`, enumeration), `set`, `Deque(T)`, `SmallVec(T)`, each with tests and a consumer; the port mirror is paid and all four are allowlisted — §1.1 |

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

**And know what the list does NOT buy you.** Every file in it is compiled *single-file, with
no import resolution*, because that is what `rust_cgen_dump` does. So the allowlist verifies
emission for definitions the file owns, and says nothing about what happens once a definition
is reached across a module edge — which is where the generic-struct array-field miscompile
lived, invisible to a full-green corpus. The module-boundary gates are the separate
`jestyr_driver_*` tests that run `jc <file> build` through the real loader; **a change to
substitution or monomorphization owes one of those**, and it is the only place a
loader-triggered divergence can fail.

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
* **The corpus golden is BLIND to anything the module boundary triggers.**
  `jestyr_cgen_matches_reference` compiles each file with **no import resolution**, so a
  divergence that only appears once a definition crosses a module edge cannot fail it — and
  adding the file to `CGEN_GOLDEN_ALLOWLIST` does not help, because the single-file form is
  the form that works. That is how the generic-struct array-field miscompile survived: an
  inline `[zero; 8]` in a non-generic caller is concrete, so the expr scan catches it; the
  same literal inside the generic body is not. **A port change that touches substitution
  owes a `jc <file> build` test through the real loader**, not just an allowlist entry.
* **In the port, a wrong type NAME is a miscompile, not a build failure.** An unresolved
  opaque type renders as `int`, so `JestyrArr_T_8` was *defined* — as `int a[8]` — and gcc
  accepted a struct whose `i64` field was 32 bits wide. Values above 2³² truncated silently.
  Whenever the port can render a type it might not have substituted, assert on the C **type
  name** and on a runtime value that does not fit the fallback.
* **A C-level "implicit conversion" does not make two PROTOTYPES compatible.** `void*`
  converts to and from `FILE*` freely — as a *value*. `int(void*)` is still an incompatible
  redeclaration of `int(FILE*)`, so the whole "bind stdio with an opaque pointer type" plan
  failed on its first probe. When the blocker is a redeclaration conflict, the answer is
  usually to stop redeclaring, not to find a better type name.
* **A new type's safety properties are claims until probed, and the module header is where
  they get written down too early.** `cptr`'s "cannot be dereferenced, cannot do arithmetic"
  was false in three separate ways when first written — a bare `f.*` fell to
  `Ty::Unknown`, `f + 1` took the *other* operand's numeric type so `(f + 1).*` type-checked
  as an `i32` deref, and `let p: *mut u8 = f` rode the not-modelled-yet default. Probe each
  claim in the header as a rejection test **and** pair it with the positive control that
  keeps the type usable (here: widening `*mut u8` → `cptr`, which `fread` needs).
* **Substituting renderers come in sets; a form handled by one and not the others is a bug.**
  `emit_su_ty` (AST), `emit_su_tyid` (checker) and `emit_gs_ty` (struct-instance) must all
  know every type form. Arrays were in the first and missing from the other two, so five call
  sites quietly fell back to the unsubstituted `emit_ty_c`. Adding a type form means checking
  all three.
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
