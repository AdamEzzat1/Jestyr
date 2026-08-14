# Examples, indexed by feature

Every file here is runnable with the Rust reference compiler:

```
cargo run -- run examples/<file>.jtr     # compile via gcc and execute
cargo run -- check examples/<file>.jtr   # type/ownership check only
```

or with the bootstrapped self-hosted compiler (`./jc examples/<file>.jtr run` —
see [bootstrap/README.md](../bootstrap/README.md)). Each file opens with a
header comment saying what it demonstrates and, for the runnable ones, its
expected output. A few are **rejection demos**: their point is the compile
error, so use `check` (marked ⛔ below).

> **`examples/std/` is not a demo folder — it is the self-hosted compiler's
> own source code** (plus the standard library, written in Jestyr). See the
> [section at the bottom](#examplesstd--the-compiler-itself).

## Start here

| file | shows |
|---|---|
| `hello.jtr` | hello world |
| `compute.jtr` | structs, functions, recursion, control flow |
| `methods.jtr` | methods and method-call sugar |
| `vec.jtr` | a growable vector, end to end |
| `tests_demo.jtr` | the built-in `@test` harness (`cargo run -- test …`) |

## Sample programs (whole programs, not feature demos)

Seven small complete programs, each built around one pillar of the language —
read them in this order for a guided tour:

| file | program | the pillar it shows |
|---|---|---|
| `life.jtr` | Conway's Game of Life (a glider, 5 generations) | arrays as stack *values*, invisible `read` borrows, zero heap in the simulation |
| `calc.jtr` | an expression evaluator | recursive ADTs, payload-carrying errors (`DivByZero(i64)` carries the numerator out), the enforced `unsafe` boundary |
| `primes.jtr` | a Sieve of Eratosthenes | `@no_alloc` proven allocation-freedom + `requires`/`ensures` contracts |
| `pi.jtr` | Monte Carlo π on four threads | structured concurrency, atomics, and a *deterministic* parallel answer |
| `histogram.jtr` | a word-length histogram | zero-copy `split` views, the owned/borrowed string split |
| `ledger.jtr` | two account types behind one trait | error sets in *trait* signatures, generic bounds (`[T: Account]`), payload recovery |
| `arena_points.jtr` | route lengths over arena points | `region` arenas: zero-cost refs, O(1) bulk free, escape = compile error |

## Ownership, moves, borrows (the language thesis)

| file | shows |
|---|---|
| `mvs.jtr` | move-by-default value semantics; `read` is the default parameter convention |
| `escapes.jtr` ⛔ | borrows that try to outlive their frame — the escape checker at work |
| `collection.jtr` ⛔ | the "store a borrow in a collection" escape, refused |
| `copy_optin.jtr` | `Copy` is opt-in, moves are the default |
| `records.jtr` | immutable `record` values |
| `drop.jtr`, `drop_nested.jtr` | RAII drops, including recursive field/payload auto-drop |
| `cow.jtr` | copy-on-write strings — you can see the allocation point |

## References beyond borrows: genref & regions

| file | shows |
|---|---|
| `genref.jtr` | generation-checked references — use-after-free is a deterministic fault |
| `region.jtr`, `region_string.jtr` | scope-bounded arena allocation |
| `region_escape.jtr` ⛔ | a region value escaping its scope, refused |
| `refine.jtr` | refinement-driven bounds-check elision (`i in 0..s.len`) |
| `nested_place.jtr` | assignment through bounds-checked indexing |

## Errors

| file | shows |
|---|---|
| `errors.jtr` | error sets (`!{ Io, Parse }`) and `?` propagation |
| `error_catch.jtr` | `catch` / `catch \|e\|` recovery |
| `error_payload.jtr` | payload-carrying errors + `catch \|e\| match` extraction |
| `method_errors.jtr` | fallible methods |
| `trait_errors.jtr` | error sets in trait signatures, through `dyn` dispatch |
| `option.jtr` | optionals |
| `try_utf8.jtr` | fallible UTF-8 validation |

## Generics & collections

| file | shows |
|---|---|
| `generic.jtr`, `genlist.jtr`, `genmethods.jtr` | generic functions, structs, and methods (monomorphized) |
| `vec_generic.jtr`, `vec_alloc.jtr` | generic vectors; allocator-as-value |
| `bracket_generic.jtr` | the `List(i32)` type-application syntax |
| `defaults.jtr` | default field values |
| `container.jtr` | a heap-backed growable container from raw parts |
| `builder.jtr` | a StringBuilder / iolist |
| `gen_vtable.jtr`, `alloc_vtable.jtr` | vtables under generics; allocator dispatch |

## Traits & dispatch

| file | shows |
|---|---|
| `traits_static.jtr` | traits with static dispatch |
| `dyn_dispatch.jtr`, `shapes.jtr` | `dyn` trait objects |
| `bound_method.jtr` | bound-method values |
| `fn_ptr.jtr` | function-pointer types |
| `operators.jtr` | operator traits |
| `eq_fold.jtr` | opt-in case-fold comparison |

## Structs, enums, pattern matching

| file | shows |
|---|---|
| `struct_variant.jtr`, `union.jtr` | enums with payloads |
| `match_check.jtr`, `exhaustive_check.jtr` | exhaustiveness checking |
| `nested_match.jtr`, `guards.jtr`, `orpat.jtr`, `rest_pat.jtr` | nested patterns, guards, or-patterns, rest-patterns |
| `discriminants.jtr`, `niche.jtr` | explicit discriminants; niche-optimized optional pointers |
| `spread.jtr` | struct update syntax (`Point { x: 9, ..p }`) |
| `distinct.jtr` | distinct (newtype) types |
| `def_order.jtr` | definition-order independence in emitted C |

## Strings & text

| file | shows |
|---|---|
| `strings.jtr`, `owned_string.jtr` | string views vs the owned `String` |
| `str_ops.jtr`, `str_iter.jtr`, `substr.jtr` | text operations and byte-level iteration |
| `fstring.jtr` | f-string interpolation |
| `codepoints.jtr`, `utf8_validate.jtr` | UTF-8 handling |
| `os_str.jtr` | platform bytes (WTF-8) as a distinct unvalidated type |

## Arrays, slices, ranges

`arrays.jtr`, `array_lit.jtr`, `slices.jtr` (bounds-checked fat pointers),
`ranges.jtr`, `slice_range.jtr` (`xs[lo .. hi]` on a `[]T` — a narrower view of
the same buffer, no copy; closed, open-ended, inclusive and empty forms, with
bounds asserted).

## Loops

`loops.jtr`, `loops_advanced.jtr`, `loops_else.jtr` (loop-`else`:
search-or-default), `recursion.jtr`.

## Concurrency & parallelism

| file | shows |
|---|---|
| `concurrent.jtr` | structured concurrency: `concurrent { spawn … }` → pthreads with scoped join |
| `atomics.jtr` | atomics |
| `proc_demo.jtr` | the driver's process-spawning intrinsics |

(The data-parallel surface — `par for … reduce`, `par_map`, `@simd`, the
cost model — lives in `examples/std/par_*.jtr` and `std/parallel.jtr`.)

## Compile-time evaluation & reflection

`comptime_block.jtr`, `comptime_table.jtr`, `comptime_reflect.jtr`,
`reflect.jtr`.

## Layout & bare metal

| file | shows |
|---|---|
| `layout.jtr`, `layout_auto.jtr` | `@packed` / `@align` / `@layout(auto)` and the layout report |
| `bitfields.jtr` | bitfields |
| `mmio.jtr` | `@volatile` fields + `@address` — memory-mapped I/O |
| `extern_c.jtr` | calling libc directly via `extern "c"` |

## Contracts, docs, diagnostics

| file | shows |
|---|---|
| `contracts.jtr` | `requires`/`ensures` design-by-contract |
| `docs.jtr` | doc comments + `jestyrc doc` |
| `typeerr.jtr` ⛔ | diagnostic quality on type errors |
| `bench_fib.jtr` | the `@bench` harness |

## Modules

`modules/main.jtr` (+ `modules/mathx.jtr`) — a multi-file program:
`import`, `pub` visibility, qualified access.

## Comparison suite

`cpp_compare/` — ten Jestyr programs paired with hand-written C and C++
equivalents, byte-compared on output and benchmarked; see its README.

## `examples/std/` — the compiler itself

The self-hosted compiler and the standard library, all in Jestyr:

* **The compiler:** `tokens.jtr`/`lexer.jtr` → `parser.jtr` → `typeck.jtr`
  → `escape.jtr` → `cgen.jtr` (which also contains the module loader and
  the gcc driver). The `*_cli.jtr` files are their stage-dump entry points.
* **The stdlib:** `core.jtr`, `mem.jtr`, `io.jtr`, `list.jtr`, `fs.jtr`,
  `env.jtr`, `path.jtr`, `test.jtr`, `test_report.jtr`, `strmap.jtr`,
  `intern.jtr`, `combinators.jtr`,
  `slice_algos.jtr`. See [docs/stdlib-roadmap.md](../docs/stdlib-roadmap.md)
  for the tiers (`core` / `mem` / `std` / `sys` / `parallel`), what is
  planned next, and what is deliberately staying out.
  * `time.jtr` measures elapsed time over the `mono_nanos` intrinsic —
    `now_nanos`, `since_nanos`/`since_micros`/`since_millis`. Monotonic only:
    no calendar, no time-of-day, because only DIFFERENCES are meaningful.
    Worked examples (and two hard-won lessons about timing demos) in
    `time_demo.jtr`.
  * `env.jtr` reads the process environment: `argc`/`argv`/`program` plus
    `get`/`has`/`get_or` over the `env_var` intrinsic. Values are `str` VIEWS
    into OS-owned storage — no allocation, nothing to free. Worked examples in
    `env_demo.jtr`. (`get`, not Rust's `var`: `var` is a Jestyr keyword.)
  * `path.jtr` is lexical path manipulation — `base`, `dir`, `ext`, `stem`,
    `is_abs`, `dir_len`, `join`, `normalize`. Every function is `@no_alloc`,
    so "path handling never allocates" is proven rather than promised:
    queries return `read str` views into the argument, and composition writes
    into a caller buffer. Its suite is the sibling `path_test.jtr` (written with
    `std/test`) — `jestyrc test examples/std/path_test.jtr`, 11 tests. It is a
    *separate file* because a `@test` function is emitted into every consumer of
    its module, so colocating a suite would put the test code, and
    `test_report`'s `printf`, into everything importing `std/path` — see that
    file's header. Worked examples in `path_demo.jtr`.
  * `test.jtr` + `test_report.jtr` are expectations and golden comparison for
    the `@test` harness, split across the tier boundary on purpose. `test.jtr`
    is `core` — zero imports, every function `@no_alloc`, and it cannot print:
    a `Check` handle counts, and failure text is rendered into a `[]u8` the
    caller supplies. `test_report.jtr` is the `std` half and the only file in
    the slice that performs an effect (`finish`/`dump` print; `exit_code`
    doesn't). `eq_golden` compares line-wise, insensitive to CRLF and to a
    missing final newline, and names the line that differs; `escaped` renders
    arbitrary bytes as printable ASCII so a failure message can show you the
    trailing `\r`. Worked examples in `test_demo.jtr`; own suite via
    `jestyrc test examples/std/test.jtr`.
  * `process.jtr` runs external commands behind an explicit capability. (For the
    `[]T` range-slicing that lets `test_report.finish` take a slice rather than a
    raw pointer, see `slice_range.jtr` in the top-level examples.) A
    `Process` value is the authority to spawn, so `fn build(mut p: Process, …)`
    announces in its type that it may execute commands; `denied()` is a handle
    that refuses everything while still *counting* the attempts, which is what
    makes it useful in a test. It is not a sandbox — nothing stops direct
    `run_command` — but it puts the authority where a reviewer sees it. Note
    `run_ok` (== 0) is the portable API: `run` returns the raw `system()` status,
    an exit code on Windows and a wait status on POSIX. `process_demo.jtr` proves
    the refusal is real by running one file-creating command through each handle
    kind and checking the filesystem; suite in `process_test.jtr`.
* **Numerics & determinism:** `numbers.jtr`, `parse_float.jtr`,
  `format_float.jtr`, `float_bits.jtr`, `binned.jtr`, `reductions.jtr`,
  `numerics_canary.jtr` (the locked determinism canary), `sha256.jtr`
  (zero-import FIPS 180-4, used by `attest`).
* **Concurrency/parallelism:** `sync.jtr`, `mutex.jtr`, `channel.jtr`,
  `select.jtr`, `await.jtr`, `dynamic_spawn.jtr`, `parallel.jtr`, the
  `par_*.jtr` set (`par_for`, `par_for_simd`, `par_cost`, …).
* **Demos of std itself:** `demo.jtr`, `alloc_demo.jtr`, `files.jtr`,
  `args.jtr`, `try_read.jtr`, `strmap_demo.jtr`, `intern_demo.jtr`,
  `ctfe.jtr`, `deterministic.jtr`.

Changing anything under `examples/std/` changes the compiler — the
byte-identity corpus, the self-hosting fixed point, and the committed
bootstrap seed all pin its behavior (see
[bootstrap/README.md](../bootstrap/README.md) on refreshing the seed).

To be precise about *which* changes force a reseed: the self-host closure is
the twelve modules listed in `SELFHOST_MODULES` (`src/proptests.rs`) — `mem`,
`intern`, `fs`, `env`, `list`, `tokens`, `parser`, `ctfe`, `typeck`, `escape`,
`sha256`, `cgen`. Editing one of those, or importing a new module *from* one,
pays the two-sided tax. A new stdlib module nothing in that closure imports
(as `path.jtr` is today) does not — though the corpus sweeps still glob it up,
so it must stay clean.
