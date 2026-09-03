# Tier 5 — what to do next

Cold-start note, ordered by what should be done FIRST. §1 is the serial work (compiler
defects — one session at a time). §2 is the parallel work (library breadth — fan out).
§3 is the coordination rules that make §2 safe.

```bash
cargo build --release && cargo test --release --features "c-oracle,selfhost-fixpoint"
```

**1350 passed / 0 failed / 3 ignored.** On `master`, pushed. **CI is fully green** — all
four jobs — for the first time in over two weeks.

The long-form history is `docs/session-notes/jestyr-tier5-handoff.md`. Read its §5 for the
area inventory (rewritten and current) and §6A for the defect register. Everything below
supersedes that note's §0 queue.

---

## §0. STATE

**Every known two-compiler divergence is closed.** A1 (select ExprIds), A5 (intrinsic
shadowing), A3 (`@copy` struct containment), A3b (`@copy` enum payloads), A2 (`environ`
inheritance, which turned out to need a test rather than a machine). The reference and the
self-hosted compiler now agree on acceptance, emission, and diagnostics wherever a rule
exists on both sides.

**The tier's own definition of done is MET** — *"a service can start, report health, run
background work, shut down gracefully, and be tested deterministically."* What remains is
breadth, not the headline claim.

**Four areas done** (service runtime, config, crypto-boundary, compatibility), **three
mostly** (observability, sandbox, and compatibility's tail), **three barely** (package,
HTTP, storage), **one undecided** (TLS).

---

## §1. DO THESE FIRST — the serial queue

Every item here touches the compiler's own closure, owes a port mirror, and forces a
reseed. **They cannot run in parallel with each other or with anything else that reseeds**
(see §3). Do them one at a time, in this order.

### ~~1.1 — A7: a range expression may not be a call ARGUMENT~~ — **DONE**

Closed both sides. `examples/slice_range_mut.jtr` is the corpus file, allowlisted, and the
port mirror was **watched failing** without it (the golden named exactly that file).

**Two recorded facts about A7 were wrong, and both cost time — re-measure before designing.**

* **It was never about argument position.** `total(b[0 .. 3])` into a `read []i64` has always
  worked, and `examples/slice_range.jtr` already shipped `from_utf8(b[0 .. 3])`. The Tier 4
  note's original `mut` diagnosis was the correct one; §6A's "correction" of it was the
  error. The boundary is **mutability**, i.e. by-address passing.
* **It was not a parser change**, so the P2-golden argument for the mirror did not apply.
  The parser builds the `Range` node fine and typeck types the sub-view correctly — `check`
  passes. It was one arm of **`cgen::emit_place`**.

**The actual mechanism.** `emit_place` is the lvalue-yielding twin of `emit_expr`, used for
`mut`/`out` arguments because those pass by address. Its `Index` arm assumed the index is a
*scalar element offset*; a range index is not — `xs[a .. b]` computes a whole new
`{ ptr, len }` view. The arm emitted the `Range` node as if it were an offset and hit the
backend's "the C backend does not support ranges yet", **pointing at the range**, which is
what made the symptom look like a missing range feature rather than a missing place case.

The fix parks the computed sub-view in a **compound literal of array type** — `(T[1]){ v }`
decays to `T*`, `(*…)` reads back as a place, and its lifetime is the enclosing block rather
than the statement expression that built it, so `&` of it outlives the call. This is the same
shape `abi_ref_arg` already uses, for the same reason. `str` takes the same route (a
`jestyr_rt_substr` call is a value too) and that half is reachable — a `mut str` parameter
was equally broken, as a raw gcc error rather than the range diagnostic.

The bounds assert survives the wrapping, so a bad sub-range still faults before the callee
sees it. The fixed-size-array refusal is deliberately untouched: typeck does not extend
re-slicing to arrays (that needs the borrowed-projection story, safety-mosaic item 2), so
the guard is restricted to the two bases `emit_expr` actually lowers a range over.

### 1.1b — A11: a **non-place** `mut` argument degrades to a raw gcc error — NEW, needs a decision

Found while fixing A7, and left open deliberately because it is a semantics question, not a
lowering bug. Same code path, so whoever touches `emit_place` next should see it first.

```
fn bump(mut xs: []i64) { … }
fn mk() -> []i64 { … }
bump(mk())          // error: lvalue required as unary '&' operand   ← raw gcc, no span
```

`emit_addr_arg` assumes every `mut`/`out` argument lowers to a C lvalue and emits `&(place)`.
For any argument that is a *value* — a call result, a cast, an arithmetic expression — that is
"lvalue required". `check` passes; only `run` fails; the message is gcc's, with no Jestyr span.
The A7 sub-view was one instance of this hole; **this is the rest of it.**

**Why it was NOT folded into the A7 fix.** The tempting one-liner is to route the fall-through
arms through `is_c_lvalue` and wrap the non-lvalues in the same compound literal. That is a
**silent-miscompile trap**: `is_c_lvalue` answers "never" for `Index`, but `emit_place`'s
checked-index arm deliberately renders `(*({ … &elem; }))`, which *is* an lvalue. Wrapping it
would copy the element and **silently discard the callee's writes**. `abi_ref_arg` can afford
that predicate because it serves a `read` parameter, where a spurious copy costs a copy; on
the `mut` path the same wrongness costs a wrong answer. Any fix here needs a predicate that
tracks `emit_place`'s *rendering*, not the source form — and that predicate must not be able
to drift from `emit_place`, which is the real design work.

**The decision owed first:** accept it (lower via the compound literal, as A7 now does) or
**refuse it**. Refusing looks right — a `mut` borrow of a temporary has nowhere to write back
to, so the mutation is unobservable and the code is almost certainly a bug — but a refusal is
a rule, so per the standing constraint it must live in **`escape` on both sides** (`typeck.jtr`
has no diagnostic channel). Note the sub-view case must stay ACCEPTED either way: it aliases
the caller's buffer, so its element writes are observable and useful.

### ~~1.2 — A6: `Self` in a trait parameter — `check` passes, `run` fails~~ — **DONE**

Closed both sides. `examples/trait_self.jtr` is the corpus file, allowlisted, and **each of the
two mirrors was watched failing on its own**: disabling the `cgen.jtr` half fails the cgen
golden, disabling the `typeck.jtr` half fails the P3 typeck golden (and only that one).

**The register's description was again narrower than the defect** — third time in two items.
It was not "a trait parameter", and it was not one failure:

* **A parameter, a return, a local, and nested (`[]Self`) all failed**, and only when the
  IMPL spells `Self`. A trait declaration's `Self` was always fine (traits are not emitted),
  and an impl spelling the concrete type was always fine — which is why every corpus file
  worked and the gap survived.
* **It was TWO defects wearing two faces.** `check` passes / `run` fails was only the cgen
  half. `mut o: Self` failed the other way — at `check`, with a message about *escape
  analysis* — and nothing connected the two.

**cgen.** `Self` reached neither of cgen's two type doors. `c_ty_ast` (a written-down source
type) refused it: "cannot lower the external type `Self`", fired twice because the impl
emitter runs once for the prototype and once for the definition. `c_type` (an inferred `Ty`)
was worse — `Ty::Opaque("Self")` missed the subst and fell through to **`int`, silently, with
no diagnostic**. Both doors already consult `self.subst`, so the fix is one entry —
`subst.insert("Self", target)` in `emit_impl_method_decl` — not two special cases. That is
also what makes nesting work: the emitters recurse through the map.

**typeck.** `check_fn` lowered a body's `Self` to `Opaque("Self")` and left it. The source
comment called this a deliberate deferral costing nothing, "because `assignable` is lenient on
`Opaque`". **That justification had expired.** Leniency is not the only consumer: the escape
checker's `Unknown`-finalization backstop refuses a *borrow* whose type never resolved, so a
`mut Self` parameter was rejected outright. `read` and `take` slipped through only because
that backstop is about borrows — which is exactly why the hole looked empty. `register_impls`
already built the `{Self → target}` map for recorded return types; `check_fn` now applies it
to parameters and the return too.

**Port shapes differ, and the mirror is not a transcription.** `cgen.jtr`'s `emit_impl_sig`
took no `Cg`, so it had no substitution to bind into, and its substitution map is keyed by
source SPAN with a Ty-triple payload rather than by name. The mirror threads `c`/`g` in, swaps
`emit_c_ty` → `emit_su_ty` (a strict superset — it falls through to `emit_c_ty`), and binds
`Self` as a kind-0 name-span entry, which renders `Jestyr_P` for a struct target and
`int32_t` for a primitive one. `su_slot` matches by TEXT, so any occurrence of `Self` serves
as the key. `typeck.jtr` needed no new machinery — `subst_self` already existed.

**A latent port defect this exposed, now closed with it:** `jc` never refused `Self` at all,
so where `jestyrc` errored, `jc` alone emitted `int` and `JestyrSlice_Self` silently. It was
unreachable only because the reference refused first — a divergence that would have become a
miscompile the moment the reference stopped refusing.

### ~~1.3 — A8: `attest` accepts `@deprecated` and does nothing with it~~ — **DONE**

Closed both sides; `examples/attributes.jtr` already carried a real `@deprecated`, so both
goldens covered it without a new corpus file. **Area 10 is now complete.**

One correction to the entry: `@deprecated` was never doing *nothing* — it reached cgen and
emitted `__attribute__((deprecated("…")))`. What it missed were the two places that
*describe* the API, `doc` and the manifest.

It now has its own manifest line and its own `> **Deprecated**` blockquote. The design
question is the whole item: a deprecation is **not a guarantee** (that block says "checked by
the compiler"; a deprecation is asserted, not proven) and **not part of the signature**
(`diff_item` calls any signature change `Breaking`, which would classify *deprecating* an API
as a break — backwards, and a gate that fires on good behaviour gets switched off). So every
deprecation verdict is `Compatible`, and all of them are still reported.

**Cost note for whoever sizes the next port mirror:** the reference side was ~40 lines. The
port was four times that, and none of it was the extractor — it was *widening two packed
tuples*. `jc`'s parsed-manifest item is a flat `List(i32)` of 7-wide records and its doc
target is 11-wide; adding one optional field to each meant 7→9 and 11→13 plus every stride
site. **The first sweep missed three of them** (`(r * 11)`, `(m2 * 11)`, `(m3 * 11)`, all in
one loop a too-specific regex skipped) and the port compiled fine and then died at runtime on
`Assertion failed!`. A stride change is not done until `grep -n '\* <old>\|/ <old>'` comes
back empty — check, don't sweep.

### 1.4 — B1: `select`'s `closed { … }` arm

Sugar over the termination that already works. **No longer blocked** — the old note said it
waited on A1, which turned out to be a shim gap rather than an AST change. Still its own
increment: `ExprKind::Select` across 22 reference sites, the parser, and the no-allowlist P2
golden.

### 1.5 — B2: `extern` binding a C global — MEASURED, then deferred

The principled answer for foreign globals, and no longer needed for anything urgent
(`environ` went through an intrinsic instead). **Measured before deferring:** a new item kind
means 252 `Item::` match sites across nine reference files plus 42 in the port, and it must
also reach `attest` (a global is ABI) and `doc`. Larger than everything above it combined.

### Not in this queue, deliberately

- **A4** (Windows: `capture` + print does not round-trip a child's bytes). Making stdout
  binary would change every `\n` the compiler emits on Windows and re-baseline many goldens.
  A recorded decision, not a bug to fix blind.
- **A10's sanitizer half** and **a second C compiler**. Both are CI-only: this machine has no
  `libasan`, and **no clang at all** — measured, not assumed. They inherit the caveat about
  shipping changes nobody in reach can watch. The WARNING half is done and green.

---

## §2. THEN FAN OUT — the parallel queue

None of these touch the compiler's closure, so none of them reseed. They are genuinely
independent: different modules, different tests, different areas of the brief.

**Recommended first wave — three sessions:**

### 2.1 — Package substrate (brief area 5) — THE LOAD-BEARING ONE

semver → resolver → lockfile → content-addressed cache. Content-hashing, `buildgraph.jtr`,
`tar.jtr` and `sha256.jtr` already exist underneath it. This is the area the tier's own
"distribution" theme names, so if the tier should mean what its title says, this is the item.

### 2.2 — HTTP V2 (area 6)

The parser is hardened and refuses request smuggling; **everything above the message is
absent**. Routing, middleware, streaming bodies, keep-alive, timeouts, static files, access
logs, a test client/server. `sysproc` timeouts and `syspoll` readiness are in place under it.

### 2.3 — Storage V2 (area 9)

KV, compaction, atomic batches, migrations, backup/export on top of `alog.jtr` (which is
CRC'd and crash-recoverable). Note `sysfs` has **no mtime**, deliberately — `struct stat`'s
layout differs per platform.

**Available for a second wave, same rules:**

| work | note |
|---|---|
| Crypto: HMAC, signing, a hash interface | New modules importing `sha256` read-only. These are BINDINGS, not algorithms to write — `csrand` deliberately invents nothing. |
| Trace spans | **Pick another word first.** `Span` is taken three times: `http.Header` spans, `diag` source spans, and the `@span` work-span attribute. Use the fn-pointer vtable shape, not a trait — `@no_alloc` passes vacuously through a trait method. |
| Service supervision / restart policy | The lifecycle is complete; a supervisor over `std/sysproc` is its own module. |
| Sandbox: cwd, process groups, fs capability projection | `sysproc.jtr:113` names all three. `fs.Fs` gates the parent; nothing projects it onto a child. |
| Config: live reload, nesting | `std/syswatch` exists; composing them is the caller's job today. |
| Rewrite `std/plugin` as a server on the pipe transport | Tier 4 leftover. One-process-per-call only because the transport did not exist; it does now (`start_piped`/`capture`). |

### TLS (area 8) — a DECISION, not effort

Binding OpenSSL or schannel is a link-flag change, and `CC_FLAGS` is attest-hashed, so
`-lssl` churns **every manifest in the corpus**. That makes it exclusive of all other work.
Decide whether Tier 5 claims TLS at all, or whether it is its own arc, before anyone starts.

---

## §3. COORDINATION — what makes §2 safe

**The seed is the global lock.** These sixteen modules are the compiler's own closure:

```
mem  intern  fs  env  list  tokens  parser  ctfe  typeck
intrinsics  escape  sha256  sink  width  diag  cgen
```

Editing ANY of them forces `REFRESH_SEED=1`, which rewrites ~74,000 lines across
`bootstrap/jestyr_flat.jtr` and `bootstrap/jestyr_seed.c`. Two sessions reseeding
concurrently produce a conflict nobody can hand-merge.

**Rules for a parallel session:**

1. **Do not edit any of the sixteen.** Import them freely; a new leaf module that imports
   `fs` or `sha256` is fine and needs no reseed.
2. **If you find you need a compiler change, STOP and report it.** Do not reseed. Add it to
   §1 instead. This is the most likely way a parallel session turns serial.
3. **Run the drift guard rather than the heuristic.** The standing rule is "reseed on any
   `examples/std` change"; the guard is the authority and has twice said no reseed was owed:
   ```bash
   cargo test --release --features "c-oracle,selfhost-fixpoint" bootstrap_seed_is_current
   ```
4. **A new corpus file appends one line to `docs/jc_build_matrix.txt`.** Regenerate with
   `JC_BUILD_MATRIX=1` and **read the diff** — it is hand-maintained on purpose. Conflicts
   there are line-local and trivial.
5. **Corpus-size assertions are lower bounds** (`> 100`), so new files break nothing.
6. **Everyone edits the handoff note and CHANGELOG.** Text conflicts, mergeable, but this is
   the real friction cost — which is why 3–4 concurrent sessions is the recommendation and
   8 is not.

**Cross-cutting defects do not parallelise.** The cc-flag duplication found this session
touched four places in two languages. A session that finds one like it must own the whole
fix or leave it fully recorded — a partial de-duplication leaves the hazard exactly where it
is hardest to see, which is what happened and cost an extra CI round trip.

---

## §4. THE RULES THIS TREE KEEPS RELEARNING

Read these before writing a test. Each cost a real failure.

**A rule that changes nothing owes a probe that it CAN fail — and the probe must be watched
failing.** A whole-corpus sweep that is byte-identical before and after means every golden
would pass with the port unmirrored.

**Verification on one platform is often vacuous.** This bit three times in one session: the
`-Werror` gate (1,662 warnings were a mingw false positive, proven only by RUNNING the
program), the platform defines (the missing half was POSIX, invisible from Windows), and a
line-terminator stripper (`cfg!(windows)` is true here, so the broken branch was
unreachable). **When a branch cannot run on your host, make it reachable** — take the
platform as a parameter, or assert on the emitted C where `@cfg` puts both arms.

**The obvious test for a portability fix is often the vacuous one.** A POSIX child used to
get `PATH` alone, so "the child sees `PATH`" passes against the broken and the fixed build. A
fix that WIDENS something needs a value outside the old width.

**"Owed to the Linux ladder" can mean "owed to a test nobody wrote."** A2 sat as
blocked-on-a-machine for an entire arc while the machine ran that code green. Before
recording an item as infrastructure-blocked, check whether the assertion exists at all.

**A recorded diagnosis is worth less than a recorded symptom, and can be worth less than
nothing.** Three times in one session the note's *conclusion* was right and its *mechanism*
wrong, sending the fix at something far larger than the real one. Re-measure before
designing around a recorded explanation.

**A7 then made it four, and added a sharper edge: a CORRECTION can be worse than the error it
replaced.** The entry's original `mut` diagnosis was right; the later "actually the boundary
is argument position, not mutability" was wrong, and it cost more than the original ever did
— it pointed at the parser, invoked the no-allowlist P2 golden, and made a two-line `cgen`
fix look like a mandatory-mirror parser change. **Thirty seconds of running the claim would
have killed it**: the corpus file the entry sat next to, `examples/slice_range.jtr`, had
shipped a range sub-view in argument position all along. Before rewriting a defect's
mechanism, run the sentence you are about to delete.

**A defect that wears two faces gets recorded as the smaller one.** A7 and A6 were each filed
by their most visible symptom, and each hid a second failure with a different message, a
different phase, and sometimes a worse consequence. A6's register line was "check passes, run
fails" — true of the cgen half, while the typeck half failed AT check, and while the port
lowered the same construct to a silent `int`. When closing an item, probe every position and
every convention the construct admits (`read`/`mut`/`take`, parameter/return/local/nested,
struct receiver and primitive receiver) before believing the recorded shape is the whole of it.

**"It costs nothing today" is a claim about CONSUMERS, and it goes stale when one is added.**
`check_fn` left `Self` opaque on the reasoning that assignability is lenient on `Opaque`. That
was true and stayed true — but the escape checker's `Unknown` backstop is a second consumer of
the same fact, and it refuses rather than shrugs. A deferral justified by "the only thing that
reads this is lenient" is only as good as the word *only*.

**A grandfathered exception is a claim with a timestamp.** "Those two have not been bitten
yet" was reasonable when written and stayed on the page while the hazard fired twice more.

**Read the code that WRITES a table, not the comment describing it.** The port's header calls
enum payloads "payload TyIds also in `tch`"; they are 3-tuples. Indexing them as a flat run
made a mirror that silently did nothing.

**A comment saying "validated by the reference" is a divergence with a note attached.** Grep
the port for that shape — each one marks a pass the self-hosted compiler is trusting but
does not have.

**Never round-trip a source file through PowerShell `Get-Content`/`Set-Content`.** It reads
UTF-8 as ANSI, so every em-dash becomes mojibake, and it adds a BOM. Use the editor. Note
`python3` is not on PATH here; `python` is.

**`.jtr` subset traps:** a for-condition cannot start with `(`; a bare `{` after a call-init
parses as the ctor form; never chain `string_view(x).len`; `out`, `read`, `take`, `error` and
`spawn` are keywords.

**`cmd.exe /c` strips the outer quotes off a command line that BEGINS with one**, so quoting
a program path — the spelling that looks obviously correct — mangles the rest of the line.
