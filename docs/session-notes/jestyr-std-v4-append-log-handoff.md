# Std v4 §1.10 — an append-only log, and the tier rule that lost

Cold-start note for `std/alog`. **§0 is what to do next.** Then what was built (§1), what it
turned up in the compiler and in `std/file` (§2), what is open (§3), traps (§4).

```bash
cargo build --release && cargo test --release --features "c-oracle,selfhost-fixpoint"
```

**1276 passed / 0 failed / 3 ignored** at the end of this work. `jc_build_matrix` is **58 of
58**. Seed 42,523 lines.

---

## §0. START HERE

The brief's **§1.10 (append-only log) is done**, with a consumer that crashes itself on
purpose. What remains in Tier 4 is **§1.7 (HTTP/1.1)**, **§1.8 (tar)** and **§1.11 (plugin
process protocol)**; §1.11 is the smaller of the three and `std/process` + `std/sysnet` are
both already there.

Two things found on the way are worth reading before building anything else:

* **A degrades-to-gcc row in the reference** (§2.1). A unit-fallible function whose last
  statement is an ordinary one emitted `return <that expression>`. Fixed both sides.
* **Two modules cannot bind the same C symbol** (§2.2). This killed the first design of this
  module outright and is the third angle on §2.1b's rule.

---

## §1. WHAT WAS BUILT

### §1.1 — `std/alog`: the frame, the recovery rule, the cursor

`alog.jtr` + `alog_test.jtr` (6 cases) + `alog_demo.jtr`.

**The claim, which is what the tests check rather than describe:** after a crash, reopening
yields exactly the records that were completely written, in order, and says how many bytes it
threw away.

```
offset 0  (4)  len   payload length, LITTLE-ENDIAN
offset 4  (4)  crc   CRC-32 of len ++ seq ++ payload
offset 8  (8)  seq   this record's sequence number, LITTLE-ENDIAN
offset 16 (len) payload
```

**Little-endian is written byte by byte and is never the host's order.** A log is a FILE
FORMAT — written on one machine, read on another — so the one thing it must not do is inherit
whatever the compiler felt like. The same discipline `std/sysnet` applies to `sockaddr_in`.

**The CRC says a record is INTACT; the sequence says it is the record this POSITION owes.**
Two different questions, and a checksum answers only the first. A log that is reset or repaired
and written again leaves bytes beyond the new tail that can be a perfectly valid record with a
perfectly good CRC — it *was* one, before the rewind.

**The claim about the sequence was WEAKENED while writing the test, and the weaker version is
the true one.** An early draft of the header said the sequence "catches a stale record". It
does not, in general: a stale record lying at exactly its old offset with exactly its old
number is byte-identical to a legitimate one, and **no per-record check can separate them**.
What the sequence closes is the misalignment cases — a shorter or longer replacement, a reset
to fewer records. Closing the rest needs a per-generation nonce (an epoch stamped at open,
checked alongside the sequence) at 8 more bytes per append, and the header now names that as
the cost rather than implying the problem is solved.

**`ALOG_MAX_RECORD` is a refusal to trust, not a limit.** A corrupted `len` otherwise decides
how much to allocate before anything has checked it.

**CRC-32 is `pub` and pure**, so the part most likely to be silently wrong is testable on any
host — `std/syserr`'s argument for its errno tables, third instance. A wrong checksum is
self-consistent: it accepts everything it wrote itself and rejects nothing, which looks exactly
like a working log until another implementation reads it. `the_published_crc32_vector_matches`
pins it against `crc32("123456789") == 0xCBF43926`, which is the only assertion in the file
that can catch a home-grown polynomial. Computed bitwise rather than from a 256-entry table:
the table is four times faster and would be 256 lines of generated constants nobody can review,
and a log's cost is dominated by the write.

### §1.2 — The suite builds its damaged logs with its OWN encoder

`alog_test.jtr` re-implements the frame — `put_u32`, `put_u64`, `put_frame` — instead of poking
at a file the module wrote. Two reasons, and the second is the one that matters:

1. The test says exactly what it is feeding in, rather than describing a byte offset.
2. **A test that corrupts a file the module wrote can only ever agree with the module.** The
   layout is now cross-checked against an independent implementation.

`put_frame` takes a `crc_delta`, so writing a deliberately-bad checksum is one argument.

**Every corruption test carries a control that says WHICH field did the rejecting.** The
corrupted-payload test re-lays the same two records with a correct checksum and asserts both
read; the stale-record test re-lays them with the second numbered 1 instead of 9 and asserts
both read. Without those, either test would pass for a reader that refused every second record.

### §1.3 — The consumer: `jledger`, which crashes itself

`alog_demo.jtr`. Appends three entries, syncs, then writes **nine bytes of a record whose
header promised sixteen** — what a process killed mid-write leaves on disk. Reopening reports
3 recovered / 9 discarded, the next entry lands at sequence 3, and the replay reads four
entries in order with no gap and no phantom.

`jledger_survives_a_torn_write_and_says_what_it_lost` asserts the transcript to the line, plus
that the word `damaged` never appears — the torn bytes were truncated away, so a clean end is
the whole point.

---

## §2. WHAT IT TURNED UP

### §2.1 — A tail EXPRESSION is not a return value when the ok type is unit. FIXED

```jtr
fn bump(mut s: S) !{ Bad } {
    if s.n < 0 { return err(Bad) }
    s.n = s.n + 1                    // <- last statement
}
```

`jestyrc check` reported **ok**; gcc refused with *incompatible types when returning
`int64_t` but `JestyrResult_unit` was expected*. Another degrades-to-gcc row.

The cause: `ret` is true for a unit-fallible function (its C return type is
`JestyrResult_unit`), and both body emitters turn the last `Stmt::Expr` into a `return`. There
is no value to return — the success return is synthesized afterwards, which is the path a body
that simply ends already takes.

* Reference: `Stmt::Expr(e) if !unit_tail => …`, in **both** `emit_body` sites.
* Port: the same guard, spelled `depth == 0 and g.res_ok == 0 - 1` — the port's existing
  encoding of "this is the function's own body and it is unit-fallible". It is now computed
  once into a `unit_tail` local so it cannot drift from the unit-ok return below it.

Found by `std/alog.sync`, whose last statement is `l.synced = l.synced + 1`. **This is the
fourth thing the `JestyrResult_unit` lowering has turned up** (the rethrow form, the port's
`Unit` mangle, `assignable`'s missing Unit row, and now this) — the shape is still finding
places the checker and the emitter disagree, so a fifth is likely.

Zero golden churn: no corpus file had a unit-fallible function ending in a non-return
statement until this one.

### §2.2 — TWO MODULES CANNOT BIND THE SAME C SYMBOL, and it killed a design

`std/alog` was built first with its own `fopen`/`fread`/`fwrite`/`fseek`, so that `sync` could
reach the descriptor of the handle that wrote — `std/file` keeps its `FILE *` private, so a
sync built on top of it could only flush a *different* handle to the same path, which proves
nothing.

That version type-checked. It could not be imported alongside `std/file`:

```
error: duplicate definition of `fopen`
```

**An extern's name belongs to the linker.** §2.1b of the v4 notes recorded that from one side
(a Jestyr function may not shadow an extern's name); this is the same rule saying two *externs*
may not collide either. A log that cannot coexist with the file module is not a log anyone can
use.

So the platform pair moved into `std/file`:

|  | POSIX | Windows |
|---|---|---|
| descriptor | `fileno` | `_fileno` |
| durability | `fsync` | `_commit` |
| truncation | `ftruncate` | `_chsize` |

All three take an `int` descriptor on both platforms, so they unify cleanly — unlike
`std/sysnet`'s socket calls, whose handle widths differ. `_commit`/`_chsize` are in `<io.h>`.

**`std/file` gained `sync`, `truncate_path`, `no_reader` and `no_writer`**, and `file.jtr` now
carries `@cfg` (it was already in `CGEN_GOLDEN_ALLOWLIST`, so nothing new was owed there).

### §2.3 — `std/file`'s header had PREDICTED both halves, and both predictions are now corrected

Two claims in that header went stale in the same commit, and both were written as forward-looking
notes by whoever built the module. That is the system working, and the notes are updated in
place rather than left to mislead:

* *"`flush` becomes worth adding when a caller must learn about durability MID-stream without
  ending the file — a log tailed by another process. Nothing in the tree has one."* The caller
  now exists, and what it needed was **not** `flush`: `fflush` alone never answers the
  durability question. `sync` is `fflush` AND `fsync`, a different operation with a different
  promise.
* *"`fsync` is POSIX, and by this module's own scope rule that makes it `sys`'s."* **The tier
  rule lost**, and the header now says why: given a choice between a tier boundary and a
  library that can be imported, the boundary moved — and the operations landed where they
  belong anyway, since durability is a property of a writer.

**`fflush` is not `fsync`** is now stated at the one place it is called. `fflush` moves bytes
out of the C library's buffer into the kernel; `fsync` asks the kernel to put them on the
device. A program that flushed and called it durable returns success, its bytes are visible to
every other process, and a power cut loses them anyway.

What `sync` does **not** promise, and it is written down: that the DIRECTORY ENTRY is durable.
On POSIX a newly created file needs its parent directory synced too. That needs `open(2)` on a
directory, which `std/file` does not do; a caller who needs it should write to an existing name
or use `sysfs.rename_replace`.

---

## §3. OPEN

### Tier 4 remaining

| § | item | state |
|---|---|---|
| 1.7 | HTTP/1.1 | ⬜ — the largest remaining |
| 1.8 | tar / reproducible archive | ⬜ |
| 1.11 | plugin process protocol | ⬜ — **start here**; `std/process` + `std/sysnet` both exist |

Everything else in Tier 4 is done.

### What `std/alog` deliberately does not do

* **No rotation**, and `std/log`'s header pointed here for it. Rotation is a policy (by size,
  by age, by count) and every variant needs a decision about sequence numbering across a
  boundary. Doing it properly means segments, a manifest and a reader that spans them. The
  primitives are here: `sysfs.rename_replace` seals a segment atomically and `size_bytes()` is
  the number a size policy tests.
* **No concurrent writers.** One process, one `Log`. Appending from two processes is atomic
  only for writes the platform chooses to make atomic, and Windows has no equivalent guarantee.
* **No index or random access by sequence.** A reader scans.
* **The per-generation nonce** of §1.1, if a caller is ever hurt by the residual stale case.

### Language obligations

Unchanged. `§2.1` move-only resources is now owed by a **fifth** handle type (`alog.Log` holds
a `file.Writer`, itself freely copyable). `§2.2` `@must_use` on a non-union return is still
enforced only by gcc.

---

## §4. TRAPS

**`bytes` is a compiler intrinsic.** `pub fn bytes(read l: Log)` compiled with a warning that
an unqualified call would reach the intrinsic and a qualified one this function. Renamed to
`size_bytes`. The warning is good; read it.

**A second-class borrow may not be returned.** `fn payload(mut buf: []u8, n: usize) -> []u8 {
return buf[0 .. n] }` is refused. The shape that works is a `fill` that returns nothing and a
caller that slices at the call site.

**A test importing a module cannot re-declare that module's externs**, transitively. The suite
originally bound `fopen` to build corrupted files; once `alog` imported `std/file`, that became
a duplicate definition. Building the bytes with `file.create_path` + `write_from` is better
anyway — see §1.2.

**`file.Reader` is sequential only.** There is no seek-to-offset, which is why `alog`'s scan
and its cursor both read forward from the start and track their own offset.
