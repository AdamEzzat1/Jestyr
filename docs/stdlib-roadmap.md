# Standard library roadmap

Jestyr's language work is well ahead of its library work. The compiler
self-hosts, but a newcomer cloning the repo cannot yet join two paths, list a
directory, ask the time, or assert anything without hand-rolling it. This
document is the plan for closing that gap, and — as importantly — the list of
things that should stay out of `std` for now.

Status: **`path`, `env`, `time`, `test` and `process` landed** (2026-08-13). Everything else
here is a plan, not a promise. Nothing in `std` is production-ready; this is a
research-preview standard library being pushed toward a capability-first Std v2.

## The shape we are copying, and from whom

| Source | What we take |
|---|---|
| Rust | the `core` / `alloc` / `std` layering, and the discipline that `core` links on a freestanding target |
| Zig | the allocator as an ordinary value, passed and stored explicitly |
| Go | boring, practical coverage — the common path paved, not a framework |
| Odin | a small core with pragmatic packages, data-oriented |
| C / POSIX | an honest system boundary; no hidden runtime |

The sentence to hold onto: **the machine is visible, allocation is explicit,
deterministic behavior is preferred, and the common path is paved.**

## The tiers

### `core` — no heap, no OS

Links on a freestanding target. Nothing here allocates and nothing here
syscalls. `examples/std/core.jtr` already carries Option/Result combinators,
slice algorithms, integer parse/format, the float tier (bits, Kahan/Neumaier,
pairwise, binned accumulator) and correctly-rounded parse/format.

Present: `core.jtr`, `path.jtr`, `test.jtr`, `sha256.jtr`, `float_bits.jtr`,
`slice_algos.jtr`, `combinators.jtr`.

The tier's contract is now *checked*, not merely documented: `path.jtr` marks
every function `@no_alloc`, so the escape checker rejects the file if any of it
ever reaches for the allocator. New `core`-tier modules should do the same.

> One honest limit on that proof, worth knowing before you lean on it:
> `@no_alloc` resolves the call graph **by free-function name**, so it does not
> see through a method, a closure, or a `fn(…)` pointer
> (`docs/attributes.md:180-184`). A module that allocates through an
> `Allocator` value's vtable will still pass. It is a strong check on
> direct-call code, not a total proof.

### `mem` / `alloc` — explicit allocation

The allocator is a value with an opaque context and a small vtable
(`examples/std/mem.jtr`), in the Zig shape. Anything that allocates either takes
an `Allocator` or writes into a caller-provided buffer.

Present: `mem.jtr` (system + arena allocators, `Layout`, `Drop`), `list.jtr`
(`List(T)`), `strmap.jtr`, `intern.jtr`.

**The rule for new code:** if a function allocates it must take an allocator or
make the allocation visible in its signature. `path.join` and `path.normalize`
take the second option — they write into `mut buf: []u8` and return the byte
count, the same idiom as `core.format_u64` — which is what lets the whole module
stay `@no_alloc`.

### `std` — hosted, practical

Thin *named* wrappers over intrinsics, so that when `extern "c"` eventually
retires an intrinsic, exactly one module changes. `io.jtr` and `env.jtr` state
this intent in their own headers and are the pattern to copy.

Present: `fs.jtr` (read/write/exists/remove), `env.jtr` (argc/argv/env_var),
`io.jtr` (four print wrappers), `time.jtr` (monotonic elapsed),
`test_report.jtr` (printing a `Check` report), `process.jtr` (running a command
behind a capability handle).

Thin is an understatement: `fs` is 35 lines and `env` is 45. This tier is where
most of the remaining work lives.

`test_report.jtr` is the tier boundary made visible: it exists *only* because
`std/test` must not print. Three functions, one import of `io`, and the whole
rest of the slice stays `core`. When a module's job splits cleanly into "decide"
and "emit", that is the split to make — it is what lets the deciding half be
`@no_alloc`-proven and reusable on a freestanding target.

### `sys` — the platform boundary

Does not exist yet, and deliberately so. Today the platform boundary *is* the
intrinsic list, which is closed: `arg`, `arg_count`, `read_file`,
`try_read_file`, `write_file`, `file_exists`, `remove_file`, `run_command`,
`eprint_str`, plus the print family. A `sys` tier becomes real when `extern "c"`
lands (design §14, currently 📐); until then a `sys` module would be a wrapper
around a wrapper.

When it does arrive it owns: the libc/POSIX/Windows split, errno-shaped errors,
and clock/process/env primitives — with `unsafe` visible and each block carrying
its safety argument, per `docs/unsafe-contract.md`.

### `parallel` / `sync` — deterministic concurrency

The most complete tier, because the language did the hard part. `par for …
reduce` accepts only declared-deterministic reductions, and `@span` makes
accidental serialization a compile error.

Present: `parallel.jtr` (`split_mut`, `par_split_mut`, `par_map`, `par_scan`),
`sync.jtr` (spinlock, `Mutex(T)`, `Channel(T)`), `binned.jtr`, `reductions.jtr`.

Known shape constraint: `spawn` targets cannot be generic, which is why
`split_mut` is `i64`-only and why several helpers are non-generic. Widening that
is a compiler change, not a library one.

## Priority order

Ranked by *how much each unlocks per unit of risk*, which is not the same as how
interesting it is.

| # | Slice | Tier | Cost | Unlocks |
|---|---|---|---|---|
| 1 | ~~`path`~~ ✅ | core | none | every CLI (but *not* the compiler's loader — see above) |
| 2 | ~~`test` — assert helpers, golden compare~~ ✅ | core + std | none | makes `@test` pleasant to write; the harness had two users in the whole corpus |
| 3 | ~~`process` — a named wrapper over `run_command` + `eprint_str`~~ ✅ | std | none | build scripts; the first Tier 2 capability handle |
| 4 | `str` — a named module over the string intrinsics | core | none | `substr`/`find`/`trim`/`starts_with` are compiler builtins with no module in front of them, exactly the gap `fs.jtr` describes itself as filling |
| 5 | ~~`env` expansion (`env_var` intrinsic)~~ ✅ | std | one intrinsic + reseed | configuration, temp dirs |
| 6 | ~~`time` (`mono_nanos` intrinsic)~~ ✅ | std | one intrinsic + reseed | in-language elapsed measurement |
| 7 | `fs` expansion — bytes, directory listing, temp files | std | **new intrinsics** + reseed; `fs` is a closure module; `readdir` needs real cross-platform C | build tools, anything that walks a tree |
| 8 | `fmt` — consolidated deterministic formatting | core | **high** | workstream E; touches types/typeck/cgen |
| 9 | `sys` | sys | blocked | needs `extern "c"` |

Slice 4 (`str`) is the last remaining *free* one and should be taken next.
Everything from 7 down pays a new intrinsic. That cost is now measured rather than
estimated, because slice 5 paid it: **eleven edits and one reseed**, and it went
byte-identical on the first attempt.

The eleven sites, as a checklist for the next intrinsic:

| Side | File | What |
|---|---|---|
| reference | `src/typeck.rs` | the return type in the intrinsic table |
| reference | `src/cgen.rs` | the lowering (`name` → `jestyr_rt_name(...)`) |
| reference | `src/cgen.rs` | the `is_intrinsic` name list |
| reference | `src/cgen.rs` | a `uses_X` field, its detection, and the gated helper emission |
| port | `examples/std/typeck.jtr` | the return type mirror |
| port | `examples/std/cgen.jtr` | a `uses_X` scan mirroring the reference's |
| port | `examples/std/cgen.jtr` | the gated helper emission, byte-for-byte |
| port | `examples/std/cgen.jtr` | the intrinsic→C name map |
| port | `examples/std/cgen.jtr` | the pipe-delimited intrinsic name list |
| both | — | `REFRESH_SEED=1 …` then rung 3 |

Two things that make it go smoothly: emit the helper **gated on use** so every
program not calling it stays byte-identical, and keep the emitted C text
identical between the two sides down to the comment — the goldens compare
strings, not behavior.

Note that `fs` and `env` being closure modules means expanding *them* forces a
reseed even when the intrinsic already exists.

### A closure module's NAME is a reserved identifier — measured the hard way

Migrating `cgen.jtr`'s hand-rolled loader onto `std/path` looked like the
obvious next step, and it was tried. It does not work, for a reason worth
knowing before anyone tries again.

The self-host build flattens its twelve modules by **concatenating them at the
token level** and stripping module qualifiers, so `mod.item` becomes `item`. The
flatten cannot tell a module-qualified reference from a field access on a local
variable that happens to share the module's name. `cgen.jtr` has thirteen
`path.` sites — `path.start`, `path.end`, `path.len` on locals of type
`ExprData` and `str`. Importing a module named `path` rewrote all of them into
bare `start` / `end` / `len`, and gcc rejected the flattened compiler:

```
error: 'j_start' undeclared (first use in this function)
    let svenum: i32 = find_variant_enum(p, src, start, end)
```

The fix would be renaming every local named `path` across 15,000 lines and
permanently reserving the identifier, to delete ten lines of duplicated
`dir_len`. That is not a trade worth making, so `cgen.jtr` keeps its own
`path_dir_len` and the duplication is accepted and documented at both sites.

**The general rule:** a module joining `SELFHOST_MODULES` makes its name
reserved across the whole flattened compiler. The existing twelve
(`mem`, `list`, `fs`, `env`, …) already are — which is why no local in
`cgen.jtr` is called `fs` or `list`. Check for `\bNAME\.` collisions in every
closure module *before* adding an import, not after; the failure surfaces as
undeclared-identifier errors in generated C, a long way from the cause.

(The seed regenerated cleanly and the byte-identity goldens passed — this failed
only at `selfhost_fixpoint_full` and `jestyr_driver_builds_itself`, the two
gates that actually compile the flattened compiler. Rung 3 is not optional for a
closure change.)

### Why `path` went first

It scored best on every axis that matters for a first slice: no compiler change,
no new intrinsic, no reseed, a real in-repo consumer, and a specification crisp
enough to property-test. It is also the shape of module we want more of —
lexical, allocation-free, and testable without a filesystem.

### Why `test` went second — and what it actually is

Same reasoning as `path`, plus one thing `path` did not have: every future slice
pays for its absence. The `@test` harness has existed since workstream O and had
**two users in the entire corpus** (`tests_demo.jtr` and `path.jtr`), because
writing a test meant hand-rolling `if str_eq(a, b) == false { return false }` and
getting a bare `false` back when it failed. A test that cannot say *why* it failed
is a test people do not write.

| | |
|---|---|
| **Files** | `examples/std/test.jtr` (core), `examples/std/test_report.jtr` (std), `examples/std/test_demo.jtr` (demo + differential oracle driver) |
| **Tier** | `core` for the whole decision half; `std` for the three hosted functions (`finish`/`dump` print, `exit_code` does not) |
| **Allocates?** | **No.** Every function in `test.jtr` is `@no_alloc`, so the escape checker rejects the file if any of it reaches for the allocator. The caller's report buffer is the only storage, and the caller allocates it. |
| **OS / runtime?** | `test.jtr`: none — no imports at all. `test_report.jtr`: stdout, nothing else. |
| **Guarantees** | Never aborts (a failed expectation returns `false`); never allocates; the report is always printable ASCII plus `\n`; the rendering is unambiguous (decodable, so two different values can never render alike); golden comparison is insensitive to CRLF and to a missing final newline and to nothing else. |
| **Capability model** | The recorder is an explicit `Check` value, not an ambient global. The report *sink* is a caller-supplied `[]u8`. The report *destination* is a separate module the caller chooses to import. |
| **Limits** | No float expectations; no expected-diagnostic helpers; no temp-file helpers; first-difference only, not a full diff. All four are argued below. |

The API in one screen:

```jestyr
import "test"
import "test_report"
import "path"

@test fn my_test() -> bool {
    var raw: *mut u8 = alloc(u8, 1024)
    var rep: []u8 = slice(u8, raw, 1024)
    var c: test.Check = test.new()

    test.eq_str(c, rep, "base", path.base("a/b.jtr"), "b.jtr")
    test.eq_usize(c, rep, "dir_len", path.dir_len("a/b/c"), 4)
    test.eq_golden(c, rep, "output", produced, expected)

    let ok: bool = test_report.finish(c, raw)   // prints only on failure
    free_ptr(raw)
    return ok
}
```

Expectations: `is_true`, `is_false`, `eq_bool`, `eq_i64`, `ne_i64`, `eq_usize`,
`eq_str`, `eq_golden`. Queries: `passed`, `checks`, `failures`, `report_len`,
`lost`. Composition: `note`, `tally`. Primitives, usable on their own:
`escaped`, `escaped_len`, `line_count`, `first_diff_line`, `lines_eq`.

**Three design decisions worth the words.**

*The handle carries counters; the caller carries storage.* It would read better
as `Check{ buf: rep }` and the escape checker refuses: a Jestyr borrow is
second-class, so a `[]u8` may not be stored in a struct that outlives the call
(`cannot store borrow in struct: a second-class borrow may not outlive its
call`). The alternative is a raw `*mut u8` field, which drags `unsafe` into a
`core` module to save one argument. This is safety-mosaic item 2 showing up
again, from a different direction than `path` hit it.

*The module cannot print.* `assert!` in most languages hides two effects —
counting, somewhere global, and reporting, to somebody's stdout. Making both
explicit is what lets one `Check` end up on stdout, another end up compared
against a golden file, and a third run on a target with no stdout at all. Only
the third is impossible if the assertion library prints for you.

*`\n` separates report lines rather than terminating them.* The harness emits
`printf("test %s ... ")` with no newline and `print_str` appends one, so a
terminating newline puts a blank line in the middle of the harness's output for
every failing test. A one-line rule in the library beats a wart in every
consumer's output.

**What was deliberately left out, and why.**

- **Float expectations.** A `near_f64` whose failure message cannot show the two
  values is worse than no helper at all, and honestly formatting an `f64` is the
  `fmt` slice (#8, high cost, touches types/typeck/cgen). Bit-exact comparison is
  available today as `eq_i64` over `float_bits`, which is also the comparison
  `FP-DETERMINISM-CONTRACT.md` actually cares about.
- **Expected-diagnostic helpers.** Asserting "this source produces error E0007
  at 4:12" requires the compiler as a library from inside Jestyr. Real, wanted,
  and gated on the driver growing a diagnostics API — not on this slice.
- **Temp files and temp dirs.** No `mkdtemp` intrinsic, and inventing one from
  `env.get("TEMP")` plus a counter would be a non-deterministic API wearing a
  deterministic costume. It belongs to the `fs` expansion (#7), which pays new
  intrinsics anyway.
- **A full diff.** `first_diff_line` answers "where", which is the whole value of
  a golden over `str_eq`. An edit-distance diff needs somewhere to put its output
  and a policy about how much of it to keep; that is a slice, not a function.
- **`unwrap`-style helpers.** Same reason as everywhere else in this document.

**The one bug this module's existence exposed, now fixed.** The `@test` harness
collected tests from the whole import closure, so `jestyrc test my_module.jtr` on
a file importing `test` also ran `test`'s own 22. Pre-existing behavior rather
than something the module introduced — `jestyrc test examples/std/path_demo.jtr`
ran `std/path`'s eleven even though `path_demo.jtr` has none of its own — and
nobody had noticed, because until now no *imported* module shipped a suite. It is
now scoped to the named module (the root file is always module 0), pinned by
`the_harness_is_scoped_to_the_named_module`, which asserts both directions and
that `--list` agrees with what the harness bakes.

Unusually for an emission-adjacent change, **no port mirror was owed**, and the
reason is worth knowing because "zero C change" normally is *not* the same as
"zero mirror owed": `examples/std/cgen.jtr` reaches test mode only through its
single-file dump path, while its module loader is wired to `build`/`run`, which
never emit a harness. So every item in the port is module 0 and the new condition
is vacuously true there. `jestyr_cgen_test_mode_matches_reference` and
`bootstrap_seed_is_current` both stayed green untouched, which is the evidence —
not the argument.

**How it was verified.** Six layers, all green:

| Layer | What | Command |
|---|---|---|
| Jestyr unit | 22 colocated `@test` functions | `jestyrc test examples/std/test.jtr` |
| Rust toolchain-free | 3 compile-clean (module, hosted half, 5-import demo) + 1 oracle-pinning case set | `cargo test --release test_props` |
| Property (proptest) | 11 properties over the Rust oracle | same |
| Bolero fuzz | 4 totality/consistency targets on arbitrary bytes | `cargo test --release fuzz_test_` |
| Differential (c-oracle) | the **compiled Jestyr module** vs the Rust oracle, five ops × 48 cases | `cargo test --release --features c-oracle c_oracle::test_` |
| Byte-identity golden | reference backend ≡ self-hosted `cgen.jtr`, incl. test-mode harness | `cargo test --release --features c-oracle jestyr_cgen` |

The differential test is the load-bearing one, and it does something
`path_matches_the_reference` could not: `test_demo.jtr` takes byte *stand-ins* in
its arguments (`;` newline, `~` CR, `^` backslash, `#` quote, `@` tab, `!` 0x01),
so every byte the escaping treats specially reaches the compiled module. `path`'s
differential test had to exclude backslash entirely because it passed paths
through the command line literally, and says so in its own doc comment.

The strongest property is that the escaping **round-trips**: an independent Rust
decoder recovers the original bytes from every rendering, at arbitrary bytes,
under the fuzzer. That is the claim a failure message actually needs — not that
the rendering is pretty, but that two different values can never render alike.

Heeding the lesson `path` learned the hard way (a differential test cannot catch
a bug both implementations share), the oracle is pinned to the module's own
documented cases *before* it is used to judge anything, and the awkward cases
that a generator will not name — `i64::MIN`, an undersized report buffer, the
96-byte value cap, the newline discipline — are pinned by named in-language tests
instead.

## Std v2: capabilities, not conveniences

The direction the stdlib is being built in, stated so it can be argued with.

1. **Ownership is visible.** Views out (`-> read str`), buffers in (`mut buf:
   []u8`). No function returns storage the caller did not ask for.
2. **Allocation is explicit.** If a function allocates, an `Allocator` is in its
   signature. If it cannot allocate, `@no_alloc` proves it.
3. **The safety tier is visible.** `core` / `mem` / `std` / `sys` is a real
   boundary, not a naming convention — and when a module straddles it, the module
   splits (see `test` / `test_report`).
4. **Effects go through handles, not globals.** An explicit `Fs`, `Clock`, `Env`,
   `Process`, `Reader`, `Writer` or recorder beats an ambient free function,
   because a handle can be substituted, sandboxed, or absent.
5. **Deterministic by default.** Where a platform difference exists, the API
   picks one answer and writes it down (`path` writes only `/`; `test` treats
   CRLF as invisible).
6. **Borrowed adapters over allocating results.** A collector allocates only when
   an allocator is in the signature.

### Tier 2 roadmap impact

Where this slice leaves the seven planned Tier 2 areas.

| Area | Status after this slice |
|---|---|
| **5. Testing / golden utilities** | **Advanced, mostly.** Assertions, expect-eq, golden comparison and the report sink all landed. Expected diagnostics and temp dirs did not (both argued above). |
| **6. No-std contract** | **Advanced.** The `core` / `std` line is now *demonstrated* rather than described: `test.jtr` has zero imports and is `@no_alloc` throughout; the only effect in the slice is three functions in a separate `std` file. This is the pattern the remaining slices should copy. |
| **3. Reader / Writer** | **Nudged, not built.** `put`/`puts`/`put_i64` in `test.jtr` are a minimal byte sink over a caller buffer, and `tally` is formatting-into-a-sink. That is the shape a real `Writer` generalizes; the sink is deliberately private so nothing depends on it before the real abstraction exists. |
| **1. Capability handles** | **Pattern established, handles not built.** `Check` is a capability-shaped recorder and `test_demo.jtr` shows the `fs` → `test` → `test_report` handoff, but `Fs`/`Clock`/`Env`/`Process` are still ambient free functions over intrinsics. |
| **2. Typed `Path`/`PathBuf`/`OsStr`** | **Untouched, on purpose.** No hosted filesystem API changed here. |
| **4. Collections v2** | **Untouched.** |
| **7. Package / build integration** | **Untouched.** The slice is three files consumed by ordinary `import`, which is as far as it should reach. |

**The next smallest follow-up**, in order:

1. ~~**Adopt it.**~~ ✅ `std/path`'s eleven tests are now written with `std/test`,
   in `examples/std/path_test.jtr`. The verdict on the API: the expectations
   themselves read well and the failure messages are the whole point, but **four
   lines of boilerplate per test** (`alloc`, `slice`, `new`, `finish` + `free_ptr`)
   is a real cost that the buffers-in convention forces and that a
   `[]u8`-range-slice would only partly relieve. The conversion also turned up the
   test-emission leak that moved convention 4 above.
2. ~~**`std/process`**~~ ✅ The first of the four Tier 2 handles is built. Two
   findings worth carrying forward:

   * **A capability handle is worth having even without enforcement.** Nothing
     stops a function from calling `run_command` directly and nothing in the
     library can, so `Process` does not sandbox — it makes the authority to spawn
     something a caller must *pass*, which puts it in the signature where a
     reviewer sees it. Same shape of limitation as `@no_alloc`'s blind spot, and
     worth having for the same reason. Real enforcement needs effects in the type
     system, which is a language question.
   * **`denied()` earns its keep by counting.** A handle that refuses but still
     records attempts lets a test assert *both* that nothing ran and that the
     right number of attempts were made. `process_test.jtr` proves the refusal is
     real rather than cosmetic by running one file-creating command through each
     handle kind and letting the filesystem be the witness — with the `host` half
     present as a control, so the negative result cannot pass vacuously. Flipping
     `denied()` to permit kills four of the seven tests.

3. **Normalize `run_command`'s exit status.** The runtime helper is
   `return (int32_t)system(cp)` — raw. Windows gives the exit code; POSIX
   specifies a *wait status* with the code in the high byte, so `exit 3` is 3 on
   one and 768 on the other. `std/process` works around it by making `run_ok`
   (== 0, which coincides on both) the portable API and documenting `run`'s value
   as platform-specific, but the honest fix is `WEXITSTATUS` in the helper. That
   is a runtime/emission change: it owes the `cgen.jtr` mirror and a reseed. It is
   also the clearest argument yet for the `sys` tier — this is exactly the
   platform difference `sys` should own once `extern "c"` lands.
4. **Range-slice `[]u8` in the C backend.** It would delete the `raw: *mut u8`
   parameter from `test_report.finish`, and it is the single most-felt gap in the
   buffers-in convention across the whole stdlib.
5. **Mangle module `const`s by module** — see the gap recorded below.
6. **Stop emitting `@test`/`@bench` items in non-test mode** — see convention 4
   above for why, and for why it is not as small as it looks.

### Cheap vs expensive, precisely

This is the single most useful operational fact for anyone extending the stdlib.

**Cheap** — a new `examples/std/*.jtr` that no closure module imports. It costs a
header comment, a row in `examples/README.md`, and a test or two. **No bootstrap
reseed, no port mirror, no allowlist edit.** The corpus sweeps pick it up
automatically (they glob `examples/` and `examples/std/`), so keep it free of
uncovered raw-pointer sites, unresolvable error sets, and borrows whose type
never resolves.

**Expensive** — anything that (a) needs a new intrinsic, (b) changes emission,
or (c) is imported by one of the twelve self-host closure modules
(`mem, intern, fs, env, list, tokens, parser, ctfe, typeck, escape, sha256,
cgen` — the list is `SELFHOST_MODULES` in `src/proptests.rs`). Those pay the
two-sided tax: the port mirror in `examples/std/cgen.jtr` plus a refreshed
bootstrap seed **in the same commit**, or rung 3 fails.

Note that `fs` and `env` are *in* the closure. Expanding them is therefore not
the cheap operation their size suggests.

## What should deliberately NOT enter `std` yet

Saying no is most of what keeps a standard library good.

- **Networking, HTTP, TLS.** No async story (design 📐), no `extern "c"`, and
  the moment a socket lands the platform boundary stops being optional. Wait for
  `sys`.
- **JSON / serialization frameworks.** Needs the string tier (workstream E,
  ~25%) to settle first. A serializer built on today's string primitives would
  be rewritten.
- **A generic collections zoo.** `List(T)` and `StrMap` cover the real cases.
  Generic containers keep colliding with the escape checker's treatment of
  opaque `T` as non-`Copy`; each new one is a fight, not a fill-in.
- **Iterators / a lazy-sequence protocol.** This is a language design question
  (traits + closures + lifetimes) wearing a library costume. Answer it in the
  design, not by shipping a shape we'd have to break.
- **A logging framework.** Wants formatting, time, and a global — all three are
  either missing or deliberately absent.
- **A package registry or vendored dependencies.** `ROADMAP.md` already calls
  this "ecosystem-premature", and the module manifest covers the real need.
- **`unwrap`-style panicking convenience wrappers.** They would undercut the
  error-set design that error payloads and `catch |e|` exist to serve.

## Conventions for new stdlib modules

1. **Header comment first.** State the tier, whether it allocates, and — for
   runnable demos — the expected output. `examples/README.md:11-14` makes this
   the house rule; the c-oracle test then verifies the documented output.
2. **`@no_alloc` on anything claiming to be allocation-free**, so the claim is
   checked. Know its blind spot (above).
3. **Views out, buffers in.** Return `-> read str` for a borrow into an
   argument; take `mut buf: []u8` and return a length for anything composed.
4. **Ship `@test` functions with the code, but in a sibling `*_test.jtr`** —
   `examples/std/path_test.jtr` beside `examples/std/path.jtr` — and write them
   with `std/test`: `eq_str(c, rep, "name", got, want)` tells you what broke,
   where a bare `return false` does not.

   This convention used to say *colocated*, and that was measured to be wrong for
   any module with non-test consumers. **A `@test` function is an ordinary
   function with an attribute, so the C backend emits it, and everything it
   transitively calls, into every program that imports the module.** There is no
   dead-code elimination at that layer. Converting `std/path`'s eleven tests
   in-place and importing `std/test_report` therefore put 2,045 extra lines of C
   *and a `printf`* into `path_demo.jtr` — which is to say into every consumer of
   `std/path`, breaking precisely the freestanding-linkable property the `core`
   tier exists to guarantee.

   The numbers, for the same consumer (`path_demo.jtr`):

   | arrangement | emitted C |
   |---|---|
   | tests colocated, converted to `std/test` | 2,789 lines, pulls in `printf` |
   | tests colocated, old plain comparisons | 1,087 lines, 11 `malloc` |
   | **tests in a sibling `path_test.jtr`** | **744 lines, no test code at all** |

   Note the middle row: the *original* colocated tests already leaked `alloc`/
   `free_ptr` into consumers. Nobody had noticed because nothing measured it, and
   `std/path` is now cleaner than it was before this convention changed.

   Two exceptions, both principled. A module only ever imported *by* tests may
   colocate — `std/test` keeps its own 22, because everything importing it
   allocates and prints anyway. And a leaf demo with no importers has nobody to
   leak into. Pinned by `path_stays_a_leaf_module`, which asserts `std/path`
   imports nothing and declares no `@test`.

   The real fix is a compiler change: **stop emitting `@test`/`@bench` items in
   non-test mode**, where by construction nothing can reach them. That would make
   colocation safe everywhere and shrink every binary carrying a suite. It is a
   genuine emission change — it moves the non-test golden for the three corpus
   files that have `@test` items, so it owes the `cgen.jtr` mirror and a reseed —
   and it is bigger than one predicate, because the `uses_*` helper gating,
   forward declarations, and generic-instance collection all scan `@test` bodies
   too. Worth doing; not worth smuggling into a library slice.
5. **Add the two Rust-side tests**: a toolchain-free "compiles clean" via
   `module::load` + typeck + escape + cgen, and a c-oracle `toks(...)` assertion
   on the demo's documented output.

   Then decide on `CGEN_GOLDEN_ALLOWLIST` (`src/proptests.rs`). Adding the file
   opts it into byte-identity between the Rust reference backend and the
   self-hosted `cgen.jtr` — a real guarantee, and free if the module sticks to
   constructs the port already handles. Measure rather than assume: add it, run
   `cargo test --release --features c-oracle jestyr_cgen`, and use
   `DUMP_DIVERGE=1` if it fails. `path.jtr` and `path_demo.jtr` were added this
   way and passed first try; a module that diverges should be left off the list
   with a note rather than dragging a port change into a library slice.
6. **Property-test the spec, differentially where you can.** `path` ships a Rust
   oracle in `src/proptests.rs`; the c-oracle test drives the *compiled Jestyr
   module* and requires the two to agree, so the properties are statements about
   the shipped code rather than about a Rust re-description of it.

   With one caveat worth internalizing, because it bit during this very slice:
   **a differential test cannot catch a bug both implementations share.** The
   first version of `normalize` decided whether the preceding segment was `..`
   by looking at the output's last two bytes, so a directory legitimately named
   `a..` was mistaken for a `..` segment and refused to pop. Oracle and module
   agreed perfectly — and were both wrong. It was found by reading the code, and
   is now pinned by named cases on both sides
   (`normalize_pops_dirs_that_merely_end_in_dots`). Keep worked examples and
   adversarial reading in the mix; differential agreement is evidence, not
   proof.

## Open language gaps the stdlib keeps running into

Recorded here because library work is where they actually bite.

- **Borrowed projection has no source.** `-> read str` says "the return is a
  borrow" but not *of what*. `path.base(p)` is fine because there is only one
  candidate; `path.join(a, b, buf)` could not return a view even in principle,
  which is why it writes into a caller buffer. This is safety-mosaic item 2, and
  `path` is now its first concrete stdlib consumer.
- **A capability handle cannot own borrowed storage.** A borrow is second-class,
  so `[]u8` may not be a struct field (`cannot store borrow in struct: a
  second-class borrow may not outlive its call`). Any handle that wants to hold a
  caller's buffer must either take it as a parameter on every call — what
  `std/test` does, and what keeps it `unsafe`-free — or store a raw `*mut u8` and
  pay `unsafe`, as `strmap.jtr` does. Worth knowing *before* designing a handle,
  because it changes every signature in the module.
- **`[]u8` cannot be range-sliced.** `rep[0 .. n]` is `error: the C backend does
  not support ranges yet`, though `str` slices fine. So viewing the filled prefix
  of a caller buffer still goes through `slice(u8, raw, n)`, which means the raw
  pointer has to be kept alive and passed around next to the slice. This is the
  single most-felt gap in the "buffers in" convention — it is why
  `std/test_report.finish` takes a `*mut u8` at all.
- **Module `const`s are emitted unqualified, so two modules cannot share a const
  name.** `const BACKSLASH` in both `std/path` and `std/test` produced `error:
  redefinition of 'j_BACKSLASH'` from gcc when one program imported both. Note the
  asymmetry with modules-v2, which *does* let two modules share a non-generic
  struct name: types are collidable, consts are not. Until the mangling is fixed
  (an emission change, so it owes the port mirror plus a reseed), a stdlib module
  meant to be imported next to anything must prefix its constants —
  `std/test` uses `B_` and says so at the declaration site.
- **`@no_alloc` cannot see through the allocator vtable** (above), so the tier
  boundary between `core` and `mem` is enforced by convention at exactly the
  point where it matters most.
- ~~**No clock intrinsic.**~~ **Closed** by `mono_nanos` + `std/time`. What
  remains unexposed is the *wall* clock: there is still no calendar date or
  time-of-day, deliberately — `CLOCK_MONOTONIC` is the right primitive for
  measuring durations and immune to the wall clock being adjusted mid-measure.
  A calendar tier is a separate question needing its own intrinsic.
- **Keyword collisions cost API names.** `env.get` is spelled that way because
  `var` is a Jestyr keyword; `out` is reserved too, and a parameter named `out`
  produces a parse cascade rather than a clear message. Worth checking a
  proposed API name against the keyword list before writing the module.
- **`spawn` targets cannot be generic**, which shapes `parallel` more than any
  library decision did.
- **A stack array has no `.ptr`**, so `slice(T, arr.ptr, n)` does not typecheck
  and fixed-size scratch buffers must come from the heap. Minor, but it costs a
  line in every buffer-writing function.
