# Std v3 — Tier 3 complete, `@cfg`, and the ownership rules it forced

Cold-start note. **§0 is what to do next — read it first.** Then: what was built (§1), what
the build turned up in the compiler (§2), what is still open (§3), traps (§4), order (§5).

**Everything is on `master` (`577f3bc`).** `git pull` and go; there is no branch to chase.

```bash
cargo build --release && cargo test --release --features "c-oracle,selfhost-fixpoint"
```

**1228 passed at the start of this work; 1262 passed, 0 failed, 3 ignored now.** The 3
ignored are the deliberate slow numeric sweeps, not breakage.

---

## §0. START HERE — what is left, measured rather than inherited

**Tier 3 is 8 of 8. Done.** `diag`, `cli`, `buildgraph`, `sysdir`+`walk`, `bitset`,
`json`, `memprof`, `runtime`. Every one has a real consumer and a suite.

> A note if you are working from an older summary: **§1.4's `walk` IS built** —
> `examples/std/walk.jtr` + `walk_test.jtr`, 7 tests, with the sort `sysdir` refuses to do,
> a fn-pointer + `ctx` visitor, and `Fs` as the capability (a denied walk reports
> `refused=1`, with the `host()` positive control beside it). Any text saying "walk is
> unbuilt" predates commit `98f4bac`.

### The one blocking debt — **CLOSED**

**The `@cfg` port mirror is DONE.** `cgen.jtr` understands `@cfg`; `cfg_platform.jtr` and
`sysdir.jtr` are in `CGEN_GOLDEN_ALLOWLIST` and byte-identity verified in both dump and
test mode; the seed is refreshed. **`sys` now builds under `jc`** — end to end, verified on
a real multi-module driver: the self-hosted compiler loaded `sysdir`'s import closure
itself, emitted both platform branches under `#if` guards, drove gcc, and the program
listed a real directory. Its C is **byte-identical to the reference's over the whole
closure** (modulo the pre-existing `#line` divergence, which is unrelated and unchanged).

What it took, beyond the five emission sites:

* **The extern needed somewhere to keep its attributes.** `mk_item_extern` discarded them.
  `(g,h)` looks free on an extern and is NOT usable: it is the bracket-generic slice into
  `gar`, and `typeck.fn_call_ret` reads it off *whatever item a call resolves to* — extern
  included, so parking attrs there segfaults on the first call to an extern. `ItemData`
  grew a dedicated `(xat,xac)` pair instead. **Before overloading any `ItemData` slot,
  grep for who reads it off an item of a different `kind`.**
* **A latent slice bug in `emit_extern_protos`**, found by `@cfg` only because
  `cfg_platform.jtr` has multibyte characters in its header comment. The header test read
  `(w,u)` as "abi span" *before* checking `kind == 8`; on a Fn those are the body ExprId and
  the attribute offset, so it sliced two arbitrary source bytes on every root item. Harmless
  while they never spelled `.h` — an assertion failure the moment an offset landed inside a
  UTF-8 continuation byte. Fixed by testing `kind` first.

**`walk.jtr` is still out of the allowlist, and the old reason was wrong.** It was recorded
as blocked "transitively — it imports `sysdir`"; measured, that has nothing to do with it
(`walk.jtr` contains no `@cfg`). The golden feeds the RAW file to both backends with
imports UNRESOLVED, and `walk.jtr` is the first corpus file to put a scope-local droppable
(`var names: Names`, dropped through an `impl Drop` whose `Drop` trait lives in the
unresolved `mem`) into that degraded mode: **the reference emits no auto-drop for it, the
port emits one.** Every other allowlisted `impl Drop` file emits ZERO auto-drop calls in
raw-dump mode, so none of them exercises it. Rebuilt self-contained with `trait Drop`
declared locally, the two agree byte-for-byte — drop call included — and with the module
path resolved (`jestyrc emit-c examples/std/walk.jtr`) the reference emits the drop too. So
it is a disagreement about how far to degrade an ERRONEOUS program, not an emission bug,
and it is the one thing standing between `walk.jtr` and the allowlist. The reference's
likely reason (unprobed): `names_new`'s signature mentions `Allocator`, which is unresolved,
so the call's result type degrades and `needs_drop` never sees `Names`.

`cfg_is_not_yet_in_the_byte_identity_allowlist` is gone, replaced by its inverse —
`every_cfg_bearing_corpus_file_is_byte_identity_verified` scans the corpus for a real
leading `@cfg(` attribute and asserts each such file is allowlisted. A dropped entry does
not error, it silently stops verifying a file; for the one feature whose whole point is that
both platforms are always emitted, that silence is worth its own test.

**`examples/cfg_headers.jtr` is new, and it is the anti-vacuity half.** The header-agreement
rule (guard the synthesized `#include` only when EVERY declaration naming that header
agrees; mixed platforms or any unguarded declaration → unconditional) was corpus-blind:
`cfg_platform.jtr` and `sysdir.jtr` only ever exercise the agreeing case, one header per
platform, so the other two rows were dead code that still type-checked. The new file pins
all three and is allowlisted, which is what puts the **port's** agreement scan under
byte-identity rather than under a probe I ran once. The reference side gained
`a_header_named_by_two_opposite_platforms_is_unconditional` for the same reason: the
pre-existing mixed test pairs a guarded declaration with an *unguarded* one, where
"unconditional" also falls out of "one of them is live everywhere" — so `all agree` vs
`any is guarded` was not actually distinguished by anything.

### Known bugs, open

| Bug | State |
|---|---|
| **Intrinsic shadowing** — a fn named for a cgen intrinsic is silently replaced at every UNQUALIFIED call, arguments discarded | typeck WARNS; the real fix (cgen prefers the user's fn) is an emission change in a closure module → mirror + reseed + golden churn. `lexer.str_eq` and `set.contains` are grandfathered and pinned by `intrinsic_shadowing_is_confined_to_two_names` |
| **`Self` in a trait method parameter** | Parses ✓, type-checks ✓ (as `Opaque("Self")`), and **cgen refuses**: *"the C backend cannot lower the external type `Self` yet"*. A diagnostic, not a miscompile — but `check` passes and `run` fails, so it is still the degrades-to-gcc class |
| **Move-only port mirror** | Not owed today (the corpus trips the rule zero times, so both sides agree). Owed the moment a corpus file trips it |
| **`.jtr` trap:** a `\u00XX` escape in a string literal passes through to the emitted C verbatim; C rejects a universal character name below 0x20 | Accepted by the front end, fails in gcc. Use a byte-append helper |

### §2.4 — trait / generic expressiveness. THREE separate gaps, each measured

Probed this session; the state is more precise than "untouched".

1. **Multi-bounds do not parse.** `fn f[T: Hash + Eq]` → `expected ], found +` at the `+`.
   A parser change plus the port's parser mirror — and **the P2 golden has no allowlist**,
   so the mirror is not optional. This is the one Collections v2 bent around, and it is why
   `hashmap` stores fn-pointer hash/eq instead of a bound.
2. **`Self` in a trait parameter is a CGEN gap, not a parse or typeck gap** — see the bug
   table. Fixing it means teaching cgen to substitute the impl target for `Self` in a
   method signature, which the impl-body work already does for `self`.
3. **Generic aliases are refused by design**: `fn Stack(comptime T: type) -> type { return
   list.List(T) }` → *"generic-struct constructor must `return struct { … }`"*. Combined
   with "a generic type cannot hold a pointer to another generic type", there is still no
   way to newtype a container — which is why `std/set` is free functions over
   `HashMap(T, bool)` rather than a `Set(T)`.

Do (1) first if Collections v3 is coming; do (2) first if a trait needs `Self`-typed
comparison, which is what a `MapKey` trait would want.

### §2.6 — memory init / unsafe contracts. Genuinely untouched

**There is no uninitialized-memory facility at all** — no `uninit`, no `MaybeUninit`, no
`alloc_uninit`, nothing in typeck or cgen. Containers therefore need a real value for every
slot, which is why `hashmap` carries fake defaults and why `get(…, take default: V)` exists
at all.

What it needs, in order: an `uninit(T, n)`-shaped allocation, an initialize-element
operation, a drop-the-initialized-prefix rule, and the partial-initialization destructor
question. `unsafe` as a permission marker already exists and is fully enforced (the unsafe
ladder is complete), so the contract half is there — the *primitive* is not.

### §2.7 — concurrency with ownership. Measured, and smaller than it looks

`std/sync` already does two of the things this row asks for:

* **`channel_send(…, take v: T)`** — moving a value into a channel already transfers
  ownership, and with this session's `droppable_ty` fix that now covers generic containers
  too.
* **`mutex_with(m, op: fn(*mut T))`** is a SCOPED-CALLBACK lock: there is no `Guard` value,
  so "non-copyable mutex guards" is moot in the current shape — the lock cannot outlive the
  callback because it is never a value a caller holds. That is a sound design, not a gap.

What is actually missing: a `Send`-like marker (nothing stops a non-thread-safe handle
crossing `spawn`), join handles with typed results, and channel close semantics. The
`Send` question is the real one and it is a type-system feature, not a library one.

---

## §1. `std/diag` — Tier 3 §1.1, built

`examples/std/diag.jtr` + `diag_test.jtr` (15 cases) + `diag_demo.jtr`. Allowlisted,
byte-identical against the self-hosted backend first try, and green through `jc`'s own
loader and gcc driver.

### What it is

`SourceMap` / `FileId` / `Span` / `Pos` / line-column mapping / `Diagnostic` / `Label` /
`Severity` / `LabelKind` / `Theme`, a caret renderer, and a one-line `render_brief`.

```
error[E0042]: no field `z` on struct `Point`
  --> area.jtr:5:11
   |
 4 |     let p: Point = Point{ x: 1, y: 2 }
 5 |     return p.z
   |              ^ unknown field
   |              - did you mean `y`?
  --> shapes.jtr:1:12
   |
 1 | pub struct Point {
   |            ----- `Point` is declared here
   |
   = note: `Point` has fields `x` and `y`
```

### The consumer is real, and it is the one the driver already needs

`diag_demo.jtr` runs the **ported parser** over a real `.jtr` file, scans `p.ex` for Error
expressions (kind 9) and `p.it` for Error items (kind 99) — the identical two scans
`cgen.jtr`'s `driver_build` performs — and renders them with source, caret and note. That
is exactly the upgrade `cgen.jtr`'s own comment records as a follow-up. The driver today
prints `path:line:col: error: syntax error (unexpected or malformed input)` and stops at
the first; the demo renders the first in full and the cascade as brief lines, which is
strictly more information for the same screenful.

**Wiring `std/diag` INTO `cgen.jtr`'s driver is the obvious next increment and was
deliberately not done here.** `cgen.jtr` is a closure module, so it costs a reseed, and the
driver's stderr text is what several goldens compare. Do it as its own increment with the
gate re-run, not as a rider on the library.

### Four design decisions, with the reasoning that survives review

**1. No `Writer` parameter, and no error set — the spec asked for both and both are wrong.**
The brief specified `render_diagnostic(read writer: file.Writer, …) -> usize !{ DiagnosticWriteFailed }`.
`sink` writes are infallible at the call site *by design* (overflow is counted, not
returned) and `file.Writer`'s one fallible call is `finish`, so a renderer between them has
nothing to fail at: the error set would be `writer.jtr`'s deleted `failed()` in a new
costume — a query that can only ever answer "fine". And `writer.Writer`'s stream targets
are line-oriented and already carry a `Sink`, so a writer-taking renderer needs *two*
sinks. `render` therefore writes into a caller `Sink` and returns a byte count; the
destination stays the caller's and composes with stderr, a file, and the trait.

**2. Two arenas, no per-label ownership.** A `SourceMap` is every file's text concatenated
into one `String`, every name into another, and four `i32`s per file in a `List(i32)` — the
same shape `cgen.jtr`'s `Ml.mods` already uses. A `Diagnostic` is one `String` arena with
each `Label` slicing into it. Not cleverness: a borrow is second-class so a `read str`
cannot be stored, and a `List(String)` would **leak**, because B1's field auto-drop recurses
into droppable fields and `String` is a primitive with a manual `string_free` — the trap
`std/pathbuf` paid for.

**3. `FileId` is a `distinct usize`, and the bill was measured.** A file index and a byte
offset are both `usize` and never interchangeable. **11 casts inside the module, 0 in the
demo.** That distribution is the opposite of the one that shelved typed `Path` (111 of 132
on callers), which is what makes it worth having — and it is the first real consumer of the
`distinct` operation-inheritance work: a `FileId` still compares with `==` and still
arithmetics with another `FileId`.

**4. One `Label` record, four concepts.** Primary / secondary / note / suggestion differ in
exactly two bits — does it point at source, and which character underlines it. Three structs
agreeing on all their fields would be three things to walk in the right interleaving to keep
the caller's order; one record with a `LabelKind` keeps the order for free and keeps
`render` a single loop.

### What the tests pin, and why each earns its place

* **The overflow contract.** `an_unguarded_render_into_a_small_buffer_truncates_mid_diagnostic`
  performs the unguarded render *on purpose*. Every number it asserts is TRUE and the output
  is still ruined: `render` reports 20 bytes, it wrote 20 bytes, and those 20 bytes are a
  header cut in half. Only `sink.overflowed` distinguishes it — so the check is in the
  module's documented recipe, not a footnote. Paired with the positive control that a
  big-enough buffer does not overflow.
* **`a_span_naming_an_unknown_file_is_skipped_not_fatal`**, with its positive control. Losing
  the whole report because one label is unresolvable is the worst failure mode a diagnostic
  renderer has.
* **`caret_alignment_is_byte_based_and_tabs_expand`** — the pin to invert when a display-width
  function exists. Columns are BYTE columns, matching what `--json` already reports; a
  renderer that disagreed with the JSON report about where a diagnostic points would be worse
  than one that is uniformly byte-based. Tabs are the exception and expand in *both* the
  source row and the caret row, computed from the one string so they cannot disagree.
* **`labels_are_grouped_by_file_not_by_call_order`** — a regression pin for a bug this build
  had. Rendering strictly in label order printed `a.jtr`'s source line twice under two `-->`
  headers, which reads as two different places in the file.
* **`jestyr_driver_diag_across_the_module_boundary`** — the gate the corpus golden cannot be.
  `jestyr_cgen_matches_reference` compiles every file with **no import resolution**, so a
  single-file `diag.jtr` never instantiates `List(Label)` at all. This one goes through `jc`'s
  real loader, asserts `Jestyr_List__Label` is present and no `_T` instance survived, asserts
  `typedef size_t Jestyr_FileId` (a `distinct` degrading to `int` would index a file table
  with 32 bits and still compile), and — the claim byte-equality cannot make — that the
  port-built binary renders the same diagnostic character for character.

---

## §2. Two compiler rules, both from §2.2/§2.3, both reference-only

### §2.1 — A discarded fallible result is refused (§2.2 of the brief, DONE)

`file.finish(w)` as a bare statement used to compile and run with **no diagnostic at all** —
`std/file`'s header said so, and named this as "the actual fix" rather than polish.

**Measured before choosing the severity, over all 208 corpus files: FOUR sites, every one of
them `file.finish(…)` in `file_test.jtr`, zero false positives elsewhere.** That is what
justifies an error rather than a warning. The rule is also *structurally* incapable of firing
on handled code: `e?` and `e catch v` both unwrap to the ok type before statement position
sees them, so the two spellings that handle the error are unreachable by it. A block's
trailing expression is skipped — in a fallible body that is the implicit return and discards
nothing.

The four sites now **assert the verdict** (`file.finish(big) catch 0` compared against the
expected byte count) rather than dropping it, which is strictly more coverage: a setup whose
write silently failed used to make the real assertion fail for the wrong reason.

**Half of `std/file`'s language ask is now closed and the halves are worth keeping apart:**
discarding the verdict is a compile error; **not calling `finish` at all is still silent**,
because that is a linear obligation on the handle at scope exit, not a rule about one
expression. The module header now says exactly that instead of the stale claim.

### §2.2 — A `return` in a fallible function must be Result-typed (§2.3 class, DONE)

Found while probing the must-use escape hatches. `cgen` emits `return <value>` verbatim, so a
bare ok value out of a `-> T !E` produced C assigning an `int64_t` to a `JestyrResult_i64`:
`jestyrc check` passed and **gcc** refused. typeck deliberately compared against the *ok*
type; cgen never implemented the sugar that comparison implies.

**The boundary was probed, not reasoned about, and it is not where it looks:**

| form | verdict |
|---|---|
| `return ok(v)` | ✅ legal |
| `return err(E)` | ✅ legal |
| `return other_fallible(x)` | ✅ **legal** — forwarding a whole result |
| `return f(x)?` | ❌ unwraps to `T`, emitted as a bare value |
| `return f(x) catch v` | ❌ same |

So one condition covers all five: the returned expression is a `Result`. A rule demanding a
literal `ok(…)`/`err(…)` would have refused working code. **Zero corpus hits** — no edits
needed anywhere.

### Why neither owes a port mirror or a reseed

Both add diagnostics and change **no emitted byte**. The port has no assignability check at
all either — the int→int rule set that precedent — and neither rule creates an Error *type*,
so `jc` stays permissive where `jestyrc` refuses. That is the checker being ahead of the
bootstrap, not a divergence in what the two backends emit. Verified: `jestyr_cgen_matches_reference`
green, `selfhost_fixpoint_full` green, seed unchanged.

---

## §3. OPEN — with what is known

### §3.1 — `string_view(x).len` emitted invalid C. **FIXED** — see §3.-4

Recorded in the previous handoff as a `.jtr` **subset trap for closure modules**. It was
not: it was a missing row in `string_intrinsic_ret`, so the call typed as `Unknown` and
cgen emitted `.j_len` against `JestyrStr`. Fixed, mirrored in the port, reseeded, and pinned
by `string_intrinsic_types`. **The text below is the ORIGINAL report, kept for its shape.**

```jtr
fn main() -> i32 {
    var s: String = string_new()
    string_push(s, "abc")
    print_int(string_view(s).len as i64)     // check: ok.  gcc: no member named 'j_len'
    return 0
}
```

A `.len` on a `str` whose base is a **call** rather than a name emits `j_len` — the field is
being resolved as a user struct field instead of the builtin. `std/diag` works around it by
binding `let v: str = string_view(x)` first, four times.

Fixing it is an **emission** change, so it owes a port mirror in `cgen.jtr` plus a reseed —
but **zero golden churn**, because no corpus file uses the shape today (they all avoid the
documented trap). That makes it a cheap, well-bounded increment, and it removes the workaround
from `diag.jtr` and from the closure modules' subset rules.

### §3.2 — A module-qualified struct LITERAL does not parse

`diag.Theme{ gutter: false, … }` from another file is a parse error, though `diag.Theme` in
**type** position is fine. So a `pub struct` a caller is expected to build needs an exported
constructor or it is effectively read-only outside its module — `diag.theme(…)` exists for
exactly this reason. Worth knowing before designing any library type in this tree; found by
writing the test that varies the theme, not by reading the grammar.

### §3.-4 — Tier 3 §1.6 (`std/json`) and brief §2.1/§2.2/§2.3 — **Tier 3 is 8 of 8**

`std/json` (10 tests), the `string_view` miscompile, and move-only droppables.

**`std/json`.** The deferral said "after `fmt`", which was right about a serialization
FRAMEWORK and wrong about a codec: a framework renders arbitrary values and needs
formatting; JSON renders exactly six things and `std/sink` already renders four. What was
really blocking was reflection, and the answer is to not need it — no derive, no trait, no
value-to-JSON mapping. The writer is `@no_alloc` into a caller `Sink`; the reader builds a
flat node arena.

Four decisions worth keeping: key order is the CALLER's (never sorted, never deduplicated —
`docs/diagnostics-json.md` commits to byte-identical-across-runs); numbers are integers only
and a float is marked non-integral rather than truncated, because a silent `3` for `3.5` is
a plausible wrong answer; trailing content is an error, since accepting a prefix makes a
truncated file look valid; and nesting is capped at 128 so untrusted `[[[[…` is a diagnostic
rather than a stack overflow.

Nested containers collect through a scratch stack — an inner container appends while the
outer is still open, so one shared child list would interleave two containers' slices.
`nested_containers_do_not_interleave_their_children` walks `[1,[2,3],4,[5,[6,7]],8]`.

**New .jtr trap:** a `\u00XX` escape in a Jestyr string literal passes through to the
emitted C verbatim, and C rejects a universal character name below 0x20. Accepted by the
front end, fails in gcc.

**`string_view(x).len` (§2.3) was never a subset trap — it was a missing table entry.**
`string_intrinsic_ret` had no row for the owned-String family, so the call typed as
`Unknown` and cgen emitted `.j_len` against `JestyrStr`, whose C field is `len`. It survived
because the workaround is invisible: an annotated `let v: str = string_view(s)` supplies the
type the intrinsic did not, so every call site was written that way and the repo recorded
the shape as a **.jtr subset trap** rather than as the compiler bug it is. Three table rows,
mirrored in `examples/std/typeck.jtr` (the P3 golden has no allowlist), reseed paid.

**Move-only droppables (§2.1, and §2.2's handle half).** A `let`/`var` initialized from a
bare droppable NAME now moves it, reusing `take`'s machinery — one notion of "moved", one
diagnostic, with a `MoveCause` so a rebinding is not reported as a `take` argument.

**The bigger half was a pre-existing hole.** `droppable_ty` looked up `("Drop", ty_key)`, and
a blanket `impl[T] Drop for List(T)` registers under `List(T)` — so a concrete `List(i64)`
never matched and **every ownership rule silently skipped the most-used droppable in the
tree**. Use-after-`take` of a `List` was not diagnosed at all. Matching on the constructor
fixed the old rule and the new one together.

Measured over 210 corpus files before choosing the severity: **two sites, both in one
test** — and that test was documenting a latent double free. `smallvec_test` copied a
`SmallVec`, which frees a heap buffer once spilled, and was safe only because it held two
elements and never spilled: a property of the test, not of the code it checked. Rewritten
to pin the real invariant (no self-pointer into the inline buffer) through a `read`
parameter, which is physically a copy and creates no second owner.

**No port mirror is owed today** — the corpus trips the rule zero times, so both sides
agree and `jestyr_escape_dump_matches_reference` is green. One is owed the moment a corpus
file trips it.

### §3.-3 — §2.5 error-model ergonomics: THREE of five rows were already done

Probed rather than inherited, because the table this session started from was carrying
claims from an older note. What is actually open is one row, and it is a documented
deferral rather than a gap.

| Row | Real state |
|---|---|
| Discarded-error diagnostics | ✅ done earlier this session |
| Good propagation | ✅ a superset propagates; a NARROWER enclosing set is refused, naming the members it does not declare |
| Trait-signature error sets | ✅ **already enforced** — an impl declaring an error its trait never promised is refused, with both sets in the message |
| Payload extraction | ✅ `catch \|e\| match e { NotFound(w) => …, Busy(n) => n }` discriminates and binds |
| Named / namespaced sets | ⬜ **measured and NOT built** |
| Owning payloads | ⬜ deliberately deferred, with a diagnostic that says so |

**Named sets were measured and declined.** The ergonomic complaint is that every signature
respells `!{ A, B, C }`. Over the whole corpus: **40 error-set sites, 18 distinct sets, and
the largest multi-member repeat is 3** (`!{ Empty, TooBig(i64), BadKey(str) }`). The most
frequent set of all is `!{ TooBig }` at five sites — a single member, which a named set
saves nothing on. A new top-level item form costs a lexer keyword, a parser item, an AST
node, visibility, typeck resolution, **and the port's parser and typeck mirrors — and the
P2/P3 goldens have no allowlist.** That is a large two-sided tax against a repetition
count of three. Same verdict and same method as typed `Path` at 132 casts: measure the
conversion you would actually ship, and this one does not pay.

It becomes live when a real consumer repeats a multi-member set across modules — `sys`
growing a shared platform-error set is the likeliest trigger.

**Owning payloads are refused by name, with a reason.** `!{ NotFound(String) }` gives *"a
v1 error payload must be a scalar or `str` — owning and aggregate payloads are deliberately
deferred (docs/error-payloads.md §3)"*. And a `str` payload cannot carry the thing a caller
actually wants: `err(NotFound(p))` on a `read p: str` is refused by the escape checker,
correctly, since the error outlives the call — so only literals survive. That is the felt
gap, and closing it means the error union carries heap data, which widens the result struct
of every program that links it and needs a drop obligation on the error path. A whole-program
ABI change, argued in its own doc, and not something to start as a rider.

### §3.-2 — Tier 3 §1.4 (finished), §1.5, §1.7, §1.8

`std/walk` (7), `std/bitset` (6), `std/memprof` (6), `std/runtime` (5). **Tier 3 is now 7
of 8**; only §1.6 (serialization) is unbuilt, still gated on the `fmt` tier.

**§1.4 is complete.** `walk(a, fs, root, opts, visitor)` does the sorting `sysdir` refuses
to — byte-wise, because a locale-aware collation is not a determinism story — with a
fn-pointer + `ctx` visitor (the `mem.Allocator` shape; Jestyr has no closures that cross a
module edge). `false` from the visitor PRUNES a directory and STOPS at a file; two
meanings, one return value, and the caller always knows which was asked because `is_dir`
is a parameter. The capability is real: `fs.denied()` walks nothing and reports
`refused=1`, with the `host()` positive control beside it, because "a denied walk returned
nothing" is also what a broken walk returns.

Two costs named rather than hidden: `is_dir` is answered by OPENING the entry, since
neither platform exposes a portable type flag (POSIX `d_type` is not in the standard), and
sorting buffers a whole directory's names before visiting the first.

**§1.5 — `bitset`, and the rule was "pick one with a consumer".** `buildgraph`'s
topological sort was colouring nodes white/grey/black in a `List(i32)` — four bytes for two
bits. It is now two bitsets, and all 10 buildgraph tests still pass unchanged, which is the
only evidence that mattered. The other six candidates stay unbuilt. **The intrinsic-shadow
warning earned its keep immediately**: `bitset.contains` tripped it while the module was
being written, and it is `has` now — `set.contains` is grandfathered, this one never had to
be.

**§1.7 — `memprof`, a counting allocator that wraps rather than replaces.** Peak vs total
is the whole point and is pinned by a test that allocates 10×1000 bytes serially (total
10,000, peak 1,000) and again concurrently (same total, peak 10,000) — same totals,
different answer to "does it fit". `free_fn` gets no length, so the size lives in an
8-byte header before each block; that is why a wrapped pointer must not be freed through
the base allocator, which is stated and pinned.

Two things it forced. `mem.Layout`'s fields are now `pub` — a Layout whose size is
unreadable outside `mem` is an opaque token, not a layout — which cost a **reseed** (paid).
And `Counting` had to become a handle over a heap block, because the allocator vtable needs
a real ADDRESS for its context and Jestyr cannot take one of a local struct.

**§1.8 — `runtime`, the boundary and not the IO.** An explicit `Runtime` over a
`time.Clock`: timers, cancellation, `poll`, `run_until_idle`, `next_deadline`. No hidden
executor, no `spawn` free function, no `block_on`, and **nothing sleeps** — under
`time.manual()` the whole suite spans simulated seconds and takes none.

**There is deliberately no `Pollable`.** Non-blocking IO needs epoll/kqueue/IOCP — three
models with three readiness semantics — which is `sys` work behind the same `@cfg` port
mirror `sysdir` already owes. Shipping a `Pollable` that could not poll would be worse than
having none. What is settled here is the part every async design must agree on first, and
settling it against a manual clock is far cheaper than against a socket.

A real design bug surfaced in its own tests: the runtime takes its clock by `take`, so
advancing the CALLER's copy did nothing and every timer silently failed to fire. Fixed by
`runtime.advance` — time is driven through its owner. That is the honest consequence of
ownership being real, and it is now the first thing the module's header explains.

`walk`, like `sysdir`, is **not** in `CGEN_GOLDEN_ALLOWLIST`: it imports `sysdir`, so the
`@cfg` exclusion is transitive. `bitset`, `memprof` and `runtime` are allowlisted and were
byte-identical first try.

### §3.-1 — Tier 3 §1.2/§1.3/§1.4 — three modules (§1.4 was FINISHED later, in §3.-2)

`std/cli` (11 tests), `std/buildgraph` (10), `std/sysdir` (5), each with a real consumer.
`cli_demo` is `jlint` — parses a file with the ported parser and reports through
`std/diag`; `buildgraph_demo` is `jplan` — orders the manifest `Modules::render_manifest`
actually emits, verified against a real one by
`jplan_orders_a_manifest_the_compiler_rendered`.

**`std/sysdir` is the first `sys`-tier module, and it lists a real directory.** The thing
recorded as "BLOCKED, and not on what anyone expected" now works: `examples/modules` reads
back as exactly `main.jtr` and `mathx.jtr`, `.`/`..` filtered, a missing directory giving a
closed handle, an interior NUL refused with the prefix-opens positive control beside it.

Two things that made it possible and were not obvious:

* **`cptr` narrowing.** POSIX `readdir` returns a `struct dirent*` whose name must be read
  out of it — impossible through an opaque handle. An explicit `e as *mut u8` IS accepted
  (only the implicit direction is refused), and the explicitness is right: the cast is a
  claim about a foreign struct's layout and should look like one. Windows needs no such
  cast, because the OS writes into a buffer Jestyr owns.
* **`d_name`'s offset is the one number that is not portable within POSIX** — 19 on
  glibc/musl LP64, 21 on macOS. It is a plain `const` (a constant needs no guard; it is
  merely unused on Windows) and it is *asserted*, not assumed:
  `every_name_is_nonempty_and_terminated` fails on a platform where 19 is wrong, because a
  wrong offset yields plausible garbage rather than an error. **This is the trigger the
  `@cfg` vocabulary was left closed for** — `linux`/`macos` become worth adding here — and
  they are deliberately NOT added, because neither machine in this session can run the
  branch and an untested branch is the failure this module's header argues against.

**§1.4 IS NOT FINISHED.** `sysdir` is the platform half. The brief's actual ask —
`walk(fs, root, opts, visitor)` with deterministic order, ignore/glob, and a capability —
is unbuilt, and it is `std`, not `sys`. What it needs, precisely:

* **Sorting, because `sysdir` refuses to.** Neither `readdir` nor `FindNextFileA` promises
  an order and NTFS and ext4 genuinely differ, so the module returns OS order and says so.
  Determinism is `walk`'s job, which is where it is actually required.
* **A visitor that is a fn pointer**, not a closure — the `mem.Allocator` shape.
* **`Fs` as the capability**, so a walk can be denied and tested denied, with the positive
  control through `host()` that a refusal test needs to mean anything.

`sysdir.jtr`/`sysdir_test.jtr` are in `io_suites_pass` and deliberately **not** in
`CGEN_GOLDEN_ALLOWLIST`: they use `@cfg`, so byte-identity against the self-hosted backend
is owed together with §3.0's port mirror.

### §3.0 — `@cfg(<platform>)` — BUILT, reference-side. The port mirror is owed

The `sys` blocker recorded as "Jestyr has NO conditional-compilation mechanism at any
level" is closed on the reference toolchain. `examples/cfg_platform.jtr` binds the actual
divergent family — POSIX `opendir`/`readdir`/`closedir` against Windows
`FindFirstFileA`/`FindClose` — and runs.

**The design was forced, not chosen.** `attest` hashes the emitted C and "same source →
byte-identical C" is the invariant that hash commits to. A `cfg` that dropped items before
codegen would make emission a function of the HOST, so the same source would attest
differently on Linux and Windows and the cross-OS canary would go with it. So **`@cfg`
selects at the C preprocessor, not in codegen**: every guarded item is emitted, wrapped in
`#if defined(_WIN32)` / `#if !defined(_WIN32)`, and `cc` keeps the half that applies.

Two consequences, both improvements on a dropping `cfg`:

* **Both platforms are always checked** — a type error or an escape violation in the
  inactive branch is caught on either host. Pinned by `the_inactive_branch_is_still_checked`.
  (The first probe for this used an unknown bare NAME and passed, proving nothing —
  unknown bare names are not an error for any function here. Fix the probe, not the claim.)
* **Two items may share a name when their platforms are disjoint.** Same-platform
  duplicates and unguarded-vs-guarded still collide, each with its own control test.

Five emission sites carry the guard: the `#include` for a header-declared extern (which
**must** be guarded — `<dirent.h>` does not exist on Windows, so an unguarded include fails
before the guarded prototype is reached), `extern "c"` prototypes, non-generic fn
prototypes and definitions, and monomorphized instances. A header named by mixed platforms
falls back to unconditional.

`ExternFn` gained an `attrs` field: extern attributes were parsed, validated against
`Target::Extern`, and then **discarded**, which was invisible while no attribute meant
anything on an extern.

**What was owed — now paid.** The port did not understand `@cfg`, so `cgen.jtr` needed the
mirror and the seed a refresh. The feature landed green ahead of that because **no corpus
file used `@cfg`**, so no existing emitted byte changed. The mirror has since landed (§0):
`cfg_platform.jtr` and `sysdir.jtr` are in `CGEN_GOLDEN_ALLOWLIST`,
`cfg_is_not_yet_in_the_byte_identity_allowlist` is replaced by its inverse, and **`sys`
builds under `jc`**. The port's own extern items had to grow an attribute slice to get
there — `ExternFn.attrs` on the reference side has a `(xat,xac)` counterpart in
`parser.ItemData` now.

The vocabulary is `posix` and `windows`, closed on purpose, with `cfg_guard` total over it
and an anti-vacuity test tying the two together. `linux`/`macos` stay out until something
needs to tell them apart.

### §3.3 — Not attempted, and why

| item | why not now |
|---|---|
| Wiring `std/diag` into `cgen.jtr`'s driver | Closure module → reseed, and the driver's stderr text is golden-compared. Own increment, own gate run. See §1 |
| A machine (JSON) renderer | The brief defers it "after serialization exists", and `docs/diagnostics-json.md` already specifies the shape the reference emits. Build the codec first, then make `diag` render into it — not a second hand-written escaper |
| Multi-line span underlining | v1 underlines the first line only and says so. The full form needs rustc's `/ | \` gutter, a second rendering mode; a v1 that ran a caret across a line break would put it in the *wrong* place rather than a simplified one |
| Colour | `Theme` has the seam (`unicode`); colour needs a terminal-capability question that belongs with the CLI kit (§1.2 of the brief), not here |
| Un-`finish`ed `Writer` at scope exit | A linear obligation on a handle, not a rule about an expression — the `@must_use`/move-only work in §2.1 of the brief. The expression half is done; this is the other half |

---

## §4. TRAPS found this session

* **An explicit `impl Drop` does NOT replace the field auto-drop — it runs BEFORE it.**
  Measured in the emitted C: `Drop__SourceMap__drop` (the two `String`s) then
  `Drop__List_i32___drop` on the `ix` field. So an impl covering only the manual-free
  primitives is right, and adding `list.free` to it would be a **double free**. The `pathbuf`
  note recorded the leak half of this and left the composition half unstated.
* **The default parameter convention is a second-class `read` borrow, even for plain data.**
  A 24-byte `Span` could not be stored in a struct. Two fixes: `take` at every call site
  (viral) or `@copy` on the aggregate. `@copy` is right for handles and value types with no
  droppable payload — `Allocator` already carries it. `Span`, `Pos`, `Label`, `Theme`,
  `Severity` and `LabelKind` are all `@copy` here.
* **Enums have no `Eq`.** Every kind question goes through a `match`. That is a feature: an
  exhaustive `match` makes adding a fifth variant a compile error at each site that has to
  decide, where a chain of `==` would silently take the else branch.
* **`error` is a keyword.** `pub fn error(…)` is a parse error with six cascading
  diagnostics; the constructors are `new_error`/`new_warning`.
* **Array range-slicing is still refused** (`buf[0 .. 64]` on a `[64]u8`), which is pinned
  deliberately. Use `alloc` + `slice`, as `sink_test.jtr` does.
* **A theme field named for what it shows must be able to show nothing.** `gutter: false`
  first zeroed the column width and kept printing the number, giving `2 | two` — a gutter
  with no column rather than no gutter. Only the test that varied the theme caught it.
* **Compute spans in a demo, do not hand-count them.** The showcase's first version used a
  literal offset that pointed into the wrong line; `str.index_of` makes it self-correcting.
  A caret pointing at the wrong token is precisely the failure this module exists to prevent.

---

## §5. Suggested order from here

1. ~~**The `@cfg` port mirror**~~ — **DONE** (§0). `sys` builds under `jc`;
   `cfg_platform.jtr` and `sysdir.jtr` are allowlisted and byte-identity verified.
   `walk.jtr` remains out, for a reason that turned out to be about auto-drop under
   unresolved imports and not about `@cfg` at all — see §0 if you want to close it.
2. **Wire `std/diag` into `cgen.jtr`'s driver** — the consumer that makes the module
   load-bearing rather than merely available. The driver today prints
   `path:line:col: error: …` and stops at the first; `diag_demo.jtr` already shows the
   upgrade. Closure module, so reseed + golden run; budget it as its own increment.
3. **Intrinsic shadowing, properly** — cgen prefers the user's function over the intrinsic.
   Emission change in a closure module: mirror, reseed, golden churn. The warning makes
   this a known debt rather than a latent one, but `lexer.str_eq` is a live trap until then.
4. **Pick ONE of §2.4's three gaps** — multi-bounds if Collections v3 is next, `Self`-in-cgen
   if a trait needs typed comparison. Do not do both at once: the first is a parser change
   owing a P2 mirror, the second is a cgen change owing a P5 one.
5. **§2.6's uninit primitive**, which is what would let containers stop carrying fake
   defaults. Larger than it looks — the partial-initialization destructor rule is the hard
   part, not the allocation.

Leave named error sets alone until a consumer repeats a multi-member set across modules
(measured: 40 sites, 18 distinct, largest multi-member repeat is 3 — §3.-3). Leave owning
error payloads alone until someone is ready for a whole-program ABI change.

The JSON machine renderer for diagnostics is now unblocked — `std/json` exists — but wiring
it is a second hand-written escaper unless the compiler uses the module, which is a
cross-language dependency this tree has never taken. Decide that deliberately.
