# Reader / Writer — the four decisions, and why

Tier 2 area 3. This document exists because the area is **not** language-blocked — a
mutating trait method works today, generically and through `dyn` — so the only thing
standing between the roadmap and code was four decisions that are expensive to change
once callers exist. They are settled here, with the measurements that settled them.

## The finding that decided half of it

`@no_alloc` **does not see through a trait method, and passes vacuously**. Probed:

```jestyr
trait Sink { fn put(mut self, b: u8) -> bool }
impl Sink for Greedy {
    fn put(mut self, b: u8) -> bool { var p: *mut u8 = alloc(u8, 64)  … }  // allocates
}
@no_alloc fn fill[T: Sink](mut s: T) -> bool { return s.put(65) }   // ACCEPTED
```

while the direct-call control is correctly rejected:

```
error: `allocates` allocates — forbidden in a `@no_alloc` function
```

`docs/attributes.md` already documents that the call graph resolves by free-function
name; what matters here is the *consequence*. A `@no_alloc` marker on trait-polymorphic
code is not a weak proof, it is a **false** one — it reports success on code that
allocates on every call. So the trait boundary is precisely where the `core` tier's
central guarantee stops existing, and that is not a place to put `core`'s writing
primitive.

## Decision 1 — trait *and* concrete struct, split by tier

Not "one or the other". The two tiers want different things and the `@no_alloc` finding
says they cannot be the same type.

* **`core` gets a concrete `Sink`** (`examples/std/sink.jtr`): a plain struct, all
  operations free functions, no polymorphism. Every call is a direct call, so
  `@no_alloc` on it and on its callers is a **real** proof. This is what formatting code
  targets.
* **`std` gets the `Writer` trait** (`examples/std/writer.jtr`): polymorphism over
  stdout, stderr and an in-memory sink, for hosted code where `@no_alloc` is not the
  claim being made.

`mem.jtr`'s hand-written `Allocator` vtable is *not* the model to copy — its header says
it predates traits. Traits are the language's own abstraction and the hosted layer uses
them. But they are a `std`-tier tool, for the reason above.

## Decision 2 — buffering lives in the caller's buffer; no `BufWriter`

A handle **cannot own borrowed storage** (a Jestyr borrow is second-class, so a `[]u8`
parameter may not be stored in a struct that outlives the call). A buffered writer that
owns its buffer must therefore allocate through an `Allocator`, which makes it `mem`-tier,
not `core`.

So `Sink` *is* the buffer: the caller supplies `mut buf: []u8` at each call and the handle
carries only offsets. Counters in the handle, storage in the caller's hand — the same
shape `std/test`'s `Check` arrived at for the same reason.

**No `BufWriter` in this slice, and not merely for scope:** the one destination that would
benefit is stdout, and stdout already buffers in C stdio, so wrapping it would be double
buffering with a second copy. When a genuine case appears (a socket, a compressor), it
belongs in `mem` with a visible allocator.

## Decision 3 — many infallible writes, one fallible flush; errors latch

The expensive decision, and the one most likely to be judged later, so the reasoning is
explicit.

Rejected: `-> usize !{ IoError }` on every write. Error sets work and are used elsewhere
(`ledger.jtr` has them in *trait* signatures), but a formatter emitting fifty bytes would
carry fifty `?`s or a `catch` per byte. That does not make the code safer, it makes it
unreadable, and unreadable error handling is how errors get swallowed.

Adopted instead:

1. **Writes do not fail at the call site.** `put` returns nothing useful to check; a byte
   that does not fit is *counted*, not returned. Formatting code stays straight-line, which
   is exactly why `std/test`'s report rendering is legible.
2. **Failure latches on the handle** and is checked once — `overflowed(s)`, `failed(w)`.
   A truncated result is a reported fact rather than silent truncation, and the count says
   *how much* was lost.
3. **The one genuinely fallible operation is the last one.** A future `flush` against a
   real file is where an error set belongs: one fallible call at the end, not N along the
   way.

The trade, stated so nobody has to reverse-engineer it: this gives up per-write precision.
A caller who needs to know *which* write failed cannot ask. That is the right trade for
formatting, which is the dominant use, and the wrong one for a protocol that must stop at
the first short write — such a caller should check `written()` before and after.

## Decision 4 — yes, `core` gets the writing primitive; no, it does not get the trait

Answered by decision 1. `Sink` is `core` and `@no_alloc`-proven. `Writer` is `std`. The
line between them is the line where `@no_alloc` stops being a proof, which makes the tier
boundary mean something rather than being a filing convention.

## Reading is not symmetric with writing, and this is the honest part

`Writer` has a hosted implementation because `print_str` writes incrementally. **Reading
has no such primitive.** The intrinsic list offers `read_file` / `try_read_file`, which
slurp an entire file; there is no partial read, no `read_line`, no file handle. So:

* **`core` gets `Cursor`** (`examples/std/cursor.jtr`): a reader over bytes already in
  memory — `byte`, `line`, `until`, `remaining`, `at_end`, every result a view. This is
  the half that parsing actually needs, and it is fully buildable today.
* **A streaming hosted `Reader` is blocked on an intrinsic** (`read_partial` or a file
  handle), not on a design decision. Pretending otherwise by wrapping `read_file` in a
  `Reader` trait would produce an API whose central promise — that it streams — is false,
  and which would be rebuilt the moment the intrinsic lands.

`Cursor` over `fs.read_text`'s result covers every case a slurping reader could, without
claiming to stream.

## What implementation changed about the above

Two of these decisions survived contact with code only after being corrected. Both
corrections are in the shipped modules; this section exists so the reasoning above is not
read as having been right the first time.

**Decision 3 lost its latch, because nothing can latch.** The plan was: writes infallible
at the call site, failures latched on the handle, `failed(w)` checked once. Implementing it
revealed there is no failure to record. `print_str` and `eprint_str` return nothing, so a
stream write has no detectable error; and sink overflow was *already* assigned to the sink
(`sink.overflowed`) on the grounds that a caller can fix a small buffer and cannot fix a
broken stdout. So `failed()` could only ever return `false`.

It was removed rather than shipped. **A query that always answers "fine" is worse than no
query** — it invites a caller to believe something was checked, which is the same objection
this document raises against wrapping `read_file` in a streaming `Reader`. The latch
remains the right shape; it becomes real when a fallible write intrinsic does. The
infallible-write half of the decision stands and is what `Sink` implements.

**The `Writer` API needed one entry point, not two.** The first version had `write_str` for
streams and `write_str_into` for buffers, which read fine in isolation and was wrong in
use: every formatter would have had to open with `if is_buffered(w) { … } else { … }`, and a
formatter that must know its destination is not polymorphic — which was the entire purpose.
The sink and its buffer now travel on *every* write and are simply unused for a stream
target. A caller printing to stdout passes a scratch sink it never reads.

That cost is real and is pinned by a test (`a_stream_writer_leaves_the_scratch_sink_alone`)
rather than left as a claim. It buys the thing the trait exists for: `render(w, s, buf)` is
written once and runs against stdout or against a buffer, so **output becomes testable
without capturing a subprocess.**

## What this slice therefore contains

| module | tier | what |
|---|---|---|
| `sink.jtr` | `core` | `Sink` — bytes out into a caller buffer, `@no_alloc` proven |
| `cursor.jtr` | `core` | `Cursor` — bytes in from a caller buffer, views out |
| `writer.jtr` | `std` | the `Writer` trait, over stdout / stderr / a `Sink` |

Deferred with reasons above, not for lack of time: `BufWriter` (wants an allocator), a
streaming `Reader` (wants an intrinsic), and error sets on writes (wants a flush to hang
them on).
