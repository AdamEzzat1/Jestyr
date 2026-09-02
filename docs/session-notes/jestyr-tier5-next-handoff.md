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

### 1.1 — A7: a range expression may not be a call ARGUMENT

`bounds[0 .. 3]` passed to a `read []i64` parameter is refused. The workaround is `alloc` +
`slice(T, raw, N)` bound to a named local, which every caller currently does by hand.

**Why first:** it is the gap most likely to STOP a parallel session mid-flight. Library work
in §2 will reach for sub-slices constantly, and today it hits a wall with no diagnostic
explaining the shape. Closing it removes the most probable cause of a parallel session
turning serial.

Recorded too narrowly for a long time as a `mut` sub-slice problem — it is not; the boundary
is argument position, not mutability. Parser change → the P2 golden has **no allowlist**, so
the port mirror is mandatory.

### 1.2 — A6: `Self` in a trait parameter — `check` passes, `run` fails

Parses, type-checks as `Opaque("Self")`, and cgen refuses. The degrades-to-gcc class this
tier has otherwise been closing. A cgen change → mirror + reseed.

### 1.3 — A8: `attest` accepts `@deprecated` and does nothing with it

`src/attrs.rs` marks it Active; it reaches neither `doc::fn_guarantees` nor the manifest, so
a deprecation is invisible to the breaking-change gate. Small, and it is the last hole in
area 10, which is otherwise done. `cgen.jtr` implements `jc attest`, so this reseeds.

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
nothing.** Three times this session the note's *conclusion* was right and its *mechanism*
wrong, sending the fix at something far larger than the real one. Re-measure before
designing around a recorded explanation.

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
