# Jestyr vs C++ Comparison Examples

These examples are C++-shaped systems programs written twice: once in current
Jestyr and once in straightforward C++17. They are meant to make syntax,
performance, memory layout, and ownership tradeoffs visible without pretending
this is a formal benchmark suite.

## Programs

| Pair | Focus | Jestyr feature shown |
|---|---|---|
| `showcase.jtr` / `.cpp` | One bundled article demo | ADTs, contracts, layout, allocation, regions, parse/format, comptime, reductions |
| `numeric_kernel.jtr` / `.cpp` | Tight integer loops over contiguous buffers | `[]T` slices, bounds-checked slice indexing, C lowering |
| `allocator_list.jtr` / `.cpp` | Growable collection with selectable allocation policy | first-class `Allocator`, generic `List(T)`, RAII `Drop` |
| `layout_probe.jtr` / `.cpp` | Byte-level struct layout | `@packed`, `@align`, `size_of`, `align_of`, `offset_of` |
| `adt_match.jtr` / `.cpp` | Expression trees and optional pointers | payload enums, exhaustive `match`, niche optimization |
| `contracts_demo.jtr` / `.cpp` | Function boundary checks | `requires` / `ensures` contracts |
| `region_arena.jtr` / `.cpp` | Fast scoped allocation | lexical `region`, region references, bulk reclaim |
| `deterministic_numbers.jtr` / `.cpp` | Parse/format without hidden locale or allocation | `Result`, caller buffers, deterministic std routines |
| `comptime_table.jtr` / `.cpp` | Work moved to compile time | `comptime` aggregate generation |
| `float_reductions.jtr` / `.cpp` | Floating-point determinism | locked FP flags, Neumaier compensation, binned accumulator (Jestyr-only) |
| `parallel_reduce.jtr` / `.cpp` | Data-parallel reduction over a slice | `par for … reduce(r)`, declared-deterministic reductions, `@span` cost contract |
| `static_rejections.jtr` / `.cpp` | Programs Jestyr rejects before codegen | exhaustiveness, region-escape, and reduction-determinism checks |

## Run The Jestyr Versions

```powershell
cargo run --release -- run examples/cpp_compare/showcase.jtr
cargo run --release -- run examples/cpp_compare/numeric_kernel.jtr
cargo run --release -- run examples/cpp_compare/allocator_list.jtr
cargo run --release -- run examples/cpp_compare/layout_probe.jtr
cargo run --release -- run examples/cpp_compare/adt_match.jtr
cargo run --release -- run examples/cpp_compare/contracts_demo.jtr
cargo run --release -- run examples/cpp_compare/region_arena.jtr
cargo run --release -- run examples/cpp_compare/deterministic_numbers.jtr
cargo run --release -- run examples/cpp_compare/comptime_table.jtr
cargo run --release -- run examples/cpp_compare/float_reductions.jtr
cargo run --release -- run examples/cpp_compare/parallel_reduce.jtr
```

`jestyrc run` prints the temporary native executable path. To time only the
program, build once, then run that temp executable directly:

```powershell
cargo run --release -- build examples/cpp_compare/numeric_kernel.jtr
Measure-Command { & "$env:TEMP\jestyr_numeric_kernel.exe" }
```

## Build And Run The C++ Versions

```powershell
g++ -O2 -std=c++17 -ffp-contract=off -fno-fast-math -o "$env:TEMP\showcase_cpp.exe" examples/cpp_compare/showcase.cpp
g++ -O2 -std=c++17 -ffp-contract=off -fno-fast-math -o "$env:TEMP\numeric_kernel_cpp.exe" examples/cpp_compare/numeric_kernel.cpp
g++ -O2 -std=c++17 -o "$env:TEMP\allocator_list_cpp.exe" examples/cpp_compare/allocator_list.cpp
g++ -O2 -std=c++17 -o "$env:TEMP\layout_probe_cpp.exe" examples/cpp_compare/layout_probe.cpp
g++ -O2 -std=c++17 -o "$env:TEMP\adt_match_cpp.exe" examples/cpp_compare/adt_match.cpp
g++ -O2 -std=c++17 -o "$env:TEMP\contracts_demo_cpp.exe" examples/cpp_compare/contracts_demo.cpp
g++ -O2 -std=c++17 -o "$env:TEMP\region_arena_cpp.exe" examples/cpp_compare/region_arena.cpp
g++ -O2 -std=c++17 -o "$env:TEMP\deterministic_numbers_cpp.exe" examples/cpp_compare/deterministic_numbers.cpp
g++ -O2 -std=c++17 -o "$env:TEMP\comptime_table_cpp.exe" examples/cpp_compare/comptime_table.cpp
g++ -O2 -std=c++17 -ffp-contract=off -fno-fast-math -o "$env:TEMP\float_reductions_cpp.exe" examples/cpp_compare/float_reductions.cpp
g++ -O2 -std=c++17 -pthread -o "$env:TEMP\parallel_reduce_cpp.exe" examples/cpp_compare/parallel_reduce.cpp
g++ -O2 -std=c++17 -pthread -Wall -o "$env:TEMP\static_rejections_cpp.exe" examples/cpp_compare/static_rejections.cpp

& "$env:TEMP\showcase_cpp.exe"
& "$env:TEMP\numeric_kernel_cpp.exe"
& "$env:TEMP\allocator_list_cpp.exe"
& "$env:TEMP\layout_probe_cpp.exe"
& "$env:TEMP\adt_match_cpp.exe"
& "$env:TEMP\contracts_demo_cpp.exe"
& "$env:TEMP\region_arena_cpp.exe"
& "$env:TEMP\deterministic_numbers_cpp.exe"
& "$env:TEMP\comptime_table_cpp.exe"
& "$env:TEMP\float_reductions_cpp.exe"
& "$env:TEMP\parallel_reduce_cpp.exe"
```

`static_rejections.jtr` is intentionally a check-only failure:

```powershell
cargo run --release -- check examples/cpp_compare/static_rejections.jtr
```

It reports three errors and exits 1, so it works as a CI gate.

The C++ analogue compiles to show the contrast. With `-Wall`, GCC warns about
exactly one of the three: the missing enum case (`-Wswitch`). It says nothing
about `bad_region_escape` returning a pointer into a destroyed local, because
the return goes through `std::array::data()`, which is enough indirection to
defeat `-Wreturn-local-addr`. And it says nothing about `schedule_dependent_sum`,
because there is nothing there to warn about -- it is a legal program that simply
has no single answer. One opt-in warning, two silences, three hard errors.

A fourth rejection needs its own run, because it aborts compilation before the
other three can be reported. In `parallel_reduce.jtr`, replace the `par for` body
of `par_sum_sq` with the serial loop from `serial_sum_sq` and re-check: the
declared `@span(log)` no longer holds, and the compiler names the cause.

```powershell
cargo run --release -- check examples/cpp_compare/parallel_reduce.jtr
```

## What You Should See

Syntax:
Jestyr is closer to Rust/Zig than C++. Type names stay compact (`[]i64`,
`[N]T`, `*mut T`), generics use `comptime T: type`, and allocator choice is an
explicit value. C++ has a larger standard library surface and mature templates,
but the equivalent code pulls in more concepts (`std::vector`, RAII classes,
function-pointer allocator plumbing, pragmas/attributes).

Do not overclaim brevity from these files. Jestyr is dramatically shorter only
where the C++ pair hand-rolls infrastructure that Jestyr gets from `std/`
(`allocator_list`, `region_arena`). Elsewhere it is a wash or slightly longer:
`numeric_kernel` and `comptime_table` are both a few lines longer in Jestyr. And
Jestyr's generics are more verbose at the call site, not less -- the type
argument is explicit every time (`list.push(i32, xs, i + 1)` against C++'s
`xs.push(i + 1)`). The honest claim is fewer hidden concepts, not fewer
characters.

Performance:
The numeric kernel should produce the same checksum in both languages. Jestyr
lowers to optimized C with `-O2`; C++ compiles directly with `-O2`. The examples
are structured so the C/C++ compiler does most of the final optimization work;
measure locally before making speed claims.

One asymmetry is worth stating rather than hiding, because it favors Jestyr.
Jestyr's `xs[i]` lowers to a bounds-checked access -- the emitted C carries an
`assert(_ix < _s.len)` per indexing operation, and the locked flags do not
include `-DNDEBUG`, so those checks are live in the optimized build. C++'s
`std::vector::operator[]` is unchecked. The two programs are therefore not doing
identical work: the Jestyr side is doing strictly more. The elision is real but
not universal, and the emitted C shows exactly where it applies: `fill` has no
asserts at all (the loop bound proves `i < xs.len`), while `stencil` keeps them
(indices `i - 1` and `i + 1` against a `len - 1` bound defeat the proof).

Scaled to 20M elements -- 320 MB across two buffers, ~60M element operations
including 40M integer modulos, and 79M bounds checks on the Jestyr side -- the
measured result is roughly 210 ms for both, with Jestyr slightly ahead of the
C++ pair. Do not quote that as a win. `std::vector<std::int64_t> xs(n)`
value-initializes, so the C++ pair zeroes 320 MB that `fill` immediately
overwrites; Jestyr's `alloc` is a bare `malloc` and skips it. Replace the vector
with `new std::int64_t[n]` (default-init, no zeroing) and C++ moves about 15%
ahead. That 15% is the honest price of the bounds checks: not free, just cheap
enough that idiomatic C++ does not beat them.

Two measurement notes, because both bit during this comparison. Run each binary
several times and discard the first -- a cold first touch of 320 MB dominates
everything else. And alternate which binary runs first, or whichever goes first
absorbs that cost in every round and the ordering silently decides the winner.

Memory efficiency:
`numeric_kernel` uses two flat `i64` buffers in both versions. `layout_probe`
shows padding explicitly: natural layout is larger than packed layout, while
forced alignment changes the alignment contract. In `allocator_list` the
allocator is stored in the collection and the list buffer cleanup is automatic
through `Drop`. Be honest about the scope of that claim: the C++ pair does the
same thing, because `IntList` stores its allocator and frees in its destructor.
This is parity with C++, not an advantage over it -- the advantage is over C.
What is Jestyr-specific is that `Drop` is written once as a blanket
`impl[T] Drop for List(T)` and monomorphized per element type. In both
languages, allocator backing resources such as the arena are still explicitly
destroyed by user code.

Other benefits:
Jestyr's current examples emphasize deterministic lowering, explicit allocation,
bounds-checked slice indexing, RAII for owned buffers, runtime layout
intrinsics, and reductions whose answer does not depend on the thread schedule. C++ is far more
complete today, with richer libraries and toolchains, but it also gives you more
ways to accidentally hide allocation policy, indexing assumptions, and floating
point nondeterminism.

## Blog / Documentation Angles

Use `numeric_kernel` to show that Jestyr is not an interpreter demo: it lowers to
native C and can express ordinary systems kernels with compact slice syntax.

Use `allocator_list` to frame Jestyr's allocation story. The allocator is not a
global and not a hidden template parameter; it is a value that user code passes,
stores, and eventually uses for automatic cleanup.

Use `layout_probe` for the memory-efficiency section. The runtime output makes
padding visible and shows that packed/aligned representations are first-class in
the compiled program.

Use `adt_match` for expressiveness. C++ reaches for `std::variant`,
`std::visit`, smart pointers, and `if constexpr`; Jestyr uses an enum plus a
direct `match`. The optional-pointer size check is a nice concrete payoff:
Jestyr demonstrates that `MaybePtr` is pointer-sized. The C++ file prints bare
pointer size as a baseline rather than modeling the exact same optional layout.

That size is a real niche encoding, not a coincidence, and the emitted C is the
proof if you want it in the article: `MaybePtr` lowers to a bare `int32_t*` and
the `match` becomes `if (p != (int32_t*)0)`. An enum needing a genuine
discriminant does not get this -- give two variants a pointer payload each and
the same compiler emits a tag plus a union, at 16 bytes.

Note this pair is also the one place where C++ ownership is the tidier story:
`unique_ptr` frees the whole tree, while the Jestyr version must name and free
each box by hand. `Expr` here is a raw-pointer tree by design, to keep the
example focused on layout and matching.

Use `contracts_demo` to explain executable API boundaries. In Jestyr, the
contract is attached to the function declaration, so readers see the promise
before the implementation.

Use `region_arena` to explain scoped allocation: `region r { ... }` owns a bump
arena for the block and reclaims it at the closing brace. The C++ version can
model a scoped arena, but C++ pointers can still escape unless the programmer
maintains the discipline. Note that `region_arena.jtr` itself only demonstrates
the allocation; the compile-time escape *rejection* lives in
`static_rejections.jtr`, so pair the two if you are making the safety argument.

Use `deterministic_numbers` for determinism. The Jestyr version returns
`Result`, distinguishes overflow from success, and formats into caller memory.
The C++ equivalent relies on `errno`, `strtoll`, and `snprintf`, which works but
spreads the policy across ambient C library conventions.

Use `showcase` as the first code block in the article or docs site. It bundles
the language's pitch into one executable program, then the smaller examples let
readers zoom into each topic.

Use `comptime_table` when explaining that Jestyr can shift work out of runtime
without preprocessor tricks. C++ has `constexpr`, so this is a peer comparison:
both can do it, but Jestyr's `comptime { ... }` keeps the phase boundary visible
at the expression site.

Use `float_reductions` to make the determinism story more specific. A plain
floating-point sum loses small terms; compensated and binned reductions make the
tradeoff explicit. Two caveats keep this honest. First, the compensated sums are
the *same* algorithm on both sides: Jestyr's `core.f64_kahan_sum` is named for
Kahan but is really Kahan-Babuska (Neumaier), matching the C++ `neumaier_sum`.
That is worth a sentence in the article rather than a silent equivalence,
because classic Kahan returns 0.0 on this exact input -- the dataset is the
textbook case separating the two. Second, the only genuine Jestyr-only piece
here is the binned superaccumulator; the C++ final line prints the expected
result as a disclosed placeholder, over a different dataset
(`{1, 1e16, -1e16, 3}`) than the `xs` above it.

Use `parallel_reduce` for the claim that is hardest to make in any other
language: parallelism that cannot change your answer. The syntactic win is real
but small -- one `par for … reduce(r)` line against a page of chunking, thread
spawning, per-thread partials, and a join. The real argument is the two things
underneath it.

First, determinism is checked rather than assumed. The C++ version is
schedule-independent because integer `+` happens to be associative; swap the
element type to `double` and it silently is not, and no compiler, library, or
warning flag will mention it. Jestyr will not compile the equivalent: `par for`
accepts only reductions from a declared-deterministic set. The check is nominal
on purpose, which `static_rejections.jtr` shows by having it refuse a reduction
that *is* sum underneath -- "happens to be associative today" is not something
the compiler can keep relying on.

Second, `@span` puts parallel depth in the signature and verifies it. Declaring
`@span(log)` on a reduction and `@span(linear)` on its serial reference means a
later refactor that quietly serializes the parallel path is a compile error, not
a performance regression someone notices in a dashboard six weeks on. The
diagnostic even asks the right question: "did a parallel reduction get
serialized?" There is no C++ analogue to compare against, which is the point.

Use `static_rejections` for the strongest contrast. It is not about matching
runtime output; it is about moving mistakes earlier. Missing enum cases, region
escapes, and undeclared parallel reductions are errors in Jestyr, while the C++
analogue compiles with at most one opt-in warning.

## The heavy tier (`heavy_*`)

The original pairs above are demonstration-scale on purpose — small enough to
hold both versions in your head. The `heavy_*` pairs are the performance tier:
the same twin-program discipline (identical algorithm, identical traversal
order, identical checksums, byte-identical output required) on workloads big
enough for timing to mean something.

| pair | workload | shape it stresses |
|---|---|---|
| `heavy_sieve` | Sieve of Eratosthenes to 50,000,000 (~50 MB of flags) | memory-bound, branchy, strided writes |
| `heavy_matmul` | naive 768×768 f64 multiply (~453M mul-adds), exact integer checksum | floating-point compute |
| `heavy_wordcount` | 10,000,000 LCG-drawn words into a hash map | allocation + hashing (Jestyr's own `std/strmap` vs `std::unordered_map`) |
| `heavy_parsum` | parallel sum of squares over 20,000,000 i64 (~160 MB) | threaded reduction: one `par for … reduce` line vs hand-rolled `std::thread` chunking |

Measured on one machine (Windows 11, gcc/g++ 8.3, `-O2`, medians of 7 runs;
treat single-digit percentages as code-layout noise — they move between
sessions):

| pair | jestyr | c++ | jestyr speed |
|---|---|---|---|
| `heavy_sieve` | 0.505 s | 0.458 s | 0.91× (0.9–1.2× across sessions) |
| `heavy_matmul` | 0.210 s | 0.202 s | 0.96× |
| `heavy_wordcount` | 0.274 s | 0.376 s | **1.37×** — open addressing with no per-node allocation beats the node-based `unordered_map`, and the map itself is written in Jestyr |
| `heavy_parsum` | 0.145 s | 0.087 s | **0.60×** — the honest loss: the general `par for` reduction machinery costs real overhead against hand-tuned fixed-chunk threading at this size. The serial halves match at parity; this is a lowering-optimization target, not a semantics cost |

All four pairs print byte-identical output, so the numbers compare the same
computation, verified — including the parallel one, whose Jestyr answer is
schedule-independent by construction and cross-checked against its own serial
pass in-program.
