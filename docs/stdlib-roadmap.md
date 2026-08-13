# Standard library roadmap

Jestyr's language work is well ahead of its library work. The compiler
self-hosts, but a newcomer cloning the repo cannot yet join two paths, list a
directory, ask the time, or assert anything without hand-rolling it. This
document is the plan for closing that gap, and — as importantly — the list of
things that should stay out of `std` for now.

Status: **`path` landed** (2026-08-13, the first slice). Everything else here is
a plan, not a promise.

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

Present: `core.jtr`, `path.jtr`, `sha256.jtr`, `float_bits.jtr`,
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

Present: `fs.jtr` (read/write/exists/remove), `env.jtr` (argc/argv), `io.jtr`
(four print wrappers).

Thin is an understatement: `fs` is 35 lines and `env` is 15. This tier is where
most of the remaining work lives.

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
| 2 | `test` — assert helpers, golden compare | core | none | makes `@test` pleasant to write; the harness exists and has almost no users |
| 3 | `process` — a named wrapper over `run_command` + `eprint_str` | std | none | build scripts; matches the `io.jtr` pattern exactly |
| 4 | `str` — a named module over the string intrinsics | core | none | `substr`/`find`/`trim`/`starts_with` are compiler builtins with no module in front of them, exactly the gap `fs.jtr` describes itself as filling |
| 5 | ~~`env` expansion (`env_var` intrinsic)~~ ✅ | std | one intrinsic + reseed | configuration, temp dirs |
| 6 | `time` | std | **new intrinsic** + reseed | benchmarks, timeouts; no clock is exposed to Jestyr code today |
| 7 | `fs` expansion — bytes, directory listing, temp files | std | **new intrinsics** + reseed; `fs` is a closure module; `readdir` needs real cross-platform C | build tools, anything that walks a tree |
| 8 | `fmt` — consolidated deterministic formatting | core | **high** | workstream E; touches types/typeck/cgen |
| 9 | `sys` | sys | blocked | needs `extern "c"` |

Slices 2–4 are the remaining *free* ones and should be taken first. Everything
from 6 down pays a new intrinsic. That cost is now measured rather than
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
4. **Ship `@test` functions beside the code.** `std/path.jtr` does; before it,
   `examples/tests_demo.jtr` was the harness's only user in the entire corpus.
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
- **`@no_alloc` cannot see through the allocator vtable** (above), so the tier
  boundary between `core` and `mem` is enforced by convention at exactly the
  point where it matters most.
- **No clock intrinsic.** `@bench` uses C's `clock()` inside generated code;
  Jestyr code cannot ask the time at all. This is now the cheapest remaining
  gap — the `env_var` slice established the exact recipe, and a monotonic clock
  is the same eleven edits.
- **Keyword collisions cost API names.** `env.get` is spelled that way because
  `var` is a Jestyr keyword; `out` is reserved too, and a parameter named `out`
  produces a parse cascade rather than a clear message. Worth checking a
  proposed API name against the keyword list before writing the module.
- **`spawn` targets cannot be generic**, which shapes `parallel` more than any
  library decision did.
- **A stack array has no `.ptr`**, so `slice(T, arr.ptr, n)` does not typecheck
  and fixed-size scratch buffers must come from the heap. Minor, but it costs a
  line in every buffer-writing function.
