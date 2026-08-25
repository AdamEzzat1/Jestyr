# Std v4 §1.11 — a plugin protocol, and the spawn that does not exist

Cold-start note for `std/plugin`. **§0 is what to do next.**

```bash
cargo build --release && cargo test --release --features "c-oracle,selfhost-fixpoint"
```

**1277 passed / 0 failed / 3 ignored.** `jc_build_matrix` is **60 of 60**.

---

## §0. START HERE

**Tier 4 is now §1.7 (HTTP/1.1) and §1.8 (tar) only.** Everything else in the brief is done.

Before either, read §2.1: **the thing most worth building next is not in the brief at all.**
`std/process` runs commands through `system()`, which blocks until the child exits. That single
fact shaped this module, blocks a long-lived plugin server, blocks timeouts, and will block
HTTP the moment anything wants a subprocess. A `sys` spawn module (`posix_spawn` /
`CreateProcess` plus pipes) is the highest-leverage unblocker left in the tier.

---

## §1. WHAT WAS BUILT

`plugin.jtr` + `plugin_test.jtr` (4 cases) + **two** demo programs: `plugin_demo.jtr` (the host)
and `plugin_echo.jtr` (a real plugin).

### §1.1 — The value is the FAILURE TAXONOMY, not the calling

A plugin runs out-of-process so that it can fail without taking the host with it. So the module
answers with an outcome, not a `bool`:

| outcome | means | the fix is |
|---|---|---|
| `PLUGIN_OK` | it answered | — |
| `PLUGIN_FAILED` | it ran and reported an error itself | the plugin's logic |
| `PLUGIN_CRASHED` | did not exit normally, or never started | the plugin, or the install |
| `PLUGIN_BAD_RESPONSE` | **exited 0 and wrote something unusable** | the plugin's contract |
| `PLUGIN_REFUSED` | the capability said no | your own configuration |

**`PLUGIN_BAD_RESPONSE` is the one everybody forgets and the reason the module earns its
keep.** A plugin that exits 0 having written nothing, or half a message, or a message for a
version it invented, has failed — and its exit code says it succeeded. A host that trusted the
code would report success and then read whatever file was lying around, which (without the
response file being deleted on every call — `call` does this deliberately) is the PREVIOUS
call's answer. Silent, and wrong in the most confusing possible way.

### §1.2 — The frame, and the attribute that redesigned it

```
0        (4)  magic "JPL1"
4        (2)  version   LITTLE-ENDIAN
6        (2)  kind      request | response
8        (4)  len       LITTLE-ENDIAN
12       (len) payload
12+len   (4)  crc       CRC-32 of everything before it
```

**The CRC is a TRAILER, and `@no_alloc` is why.** The first version put it at offset 12, which
makes the checksummed region "everything except itself" — two disjoint ranges — so computing it
needed a scratch copy and the compiler **refused the `@no_alloc` attribute**. Moving it to the
end makes the region `0 .. 12+len`, one contiguous slice, and both sides verify without
allocating. That is the attribute doing its job as a design constraint rather than a label.

**The magic is checked first**, because everything after it is only meaningful if this is a
message at all: without it, a file of zeroes parses as a version-0, kind-0, length-0 message
whose checksum over nothing is zero — structurally valid, and nobody wrote it.
`something_that_is_not_a_message_is_refused_as_a_bad_frame` is that case.

**The checksum is verified BEFORE the version is trusted**, and the test pins the ordering. A
corrupt frame can carry any version at all, so reading the version first would report an
imaginary deployment mismatch for what is really bit rot. Two different bugs, two different
fixes.

**`alog.crc32` is reused rather than reimplemented.** Two CRCs in one tree is two places to be
wrong about one question, and that one is already pinned against the published vector.

**Version mismatch is a REFUSAL, not a negotiation.** A plugin that does its best with a version
it does not understand produces answers the host cannot distinguish from correct ones. A clean
refusal is a bug report; a best effort is a mystery.

### §1.3 — The consumer: two Jestyr programs against each other

`jhost_survives_every_way_a_plugin_can_fail` is the only test in the tree that **compiles two
Jestyr programs and runs one against the other**. Four real calls, three failing on purpose:

```
ordinary                     -> ok            HELLO FROM THE HOST
plugin reports its own error -> failed        exit code 5
plugin exits 0, writes none  -> bad-response  exit code 0
host has no permission       -> refused
```

`plugin_echo.jtr` carries two directives so the misbehaviour is real rather than mocked: `!fail`
exits non-zero without answering, and **`!silent` exits 0 having written nothing** — the
`PLUGIN_BAD_RESPONSE` case, which cannot be demonstrated with a well-behaved plugin.

---

## §2. WHAT IT TURNED UP

### §2.1 — `system()` BLOCKS, and that is the tier's real hole

`std/process` is `system()`. A host can never both run a plugin and talk to it, so:

* **No long-lived plugin server.** One process per call, and a process start is milliseconds —
  real cost for a plugin invoked per source file. The loopback-socket design `std/sysnet` would
  otherwise support needs a child running CONCURRENTLY with its parent.
* **No timeouts.** A plugin that hangs hangs the host. There is no way to interrupt `system()`
  from here.

Both need `posix_spawn`/`CreateProcess` plus pipes — a real `sys` module, not a wrapper. It is
named in the module header as the unblocker rather than worked around, and it is the single
highest-leverage thing left in Tier 4.

**Paths travel as ARGUMENTS, not shell redirection.** `plugin < req > resp` works in both
`cmd.exe` and `sh` and was still not chosen: the plugin would then have to read stdin, and
nothing in this tree can — `std/file` opens paths.

### §2.2 — Two cross-module name collisions, one of them in a module never touched

* **`pub fn ok` shadowed the Result intrinsic** and broke `examples/std/file.jtr`'s
  `return ok(w.put)` — *cannot find `ok` in this module; it is defined in module `plugin`*. A
  `pub fn` in a NEW module can break an OLD one it has nothing to do with. Renamed `succeeded`.
  The compiler also warns (`shadows a compiler intrinsic`), and the warning is worth reading.
* **`out` is a keyword**, hit while naming an encoder's destination parameter — a trap this
  arc's own earlier handoff already records. Renamed `dst`.

### §2.3 — `-1` is not a discriminator, and `std/process` says so

The denied-capability case first reported `crashed`. `process.run` answers `-1` for a refusal
AND for a real failure, which that module's header explicitly calls out, pointing at `can_run`.
So `call` asks `process.can_run` before running anything.

Refusal versus crash is the pair a caller most needs separated — one is its own configuration,
the other is the plugin's fault — and the demo's transcript asserts `crashed` never appears.

---

## §3. OPEN

| § | item | state |
|---|---|---|
| 1.7 | HTTP/1.1 | ⬜ |
| 1.8 | tar / reproducible archive | ⬜ |

Plus, ahead of both: **a `sys` process module** (§2.1).

### What `std/plugin` does not do

No streaming, no concurrent calls, no plugin-initiated messages, no timeouts, and no sandbox —
a plugin runs with the host's privileges, and `process.denied()` refusing to run anything at all
is the only enforcement this tier has. A path containing a space is **refused** rather than
quoted, because quoting differs between `cmd.exe` and `sh` and a half-right rule would hand the
plugin a truncated path and then blame the plugin.

---

## §4. TRAPS

**`@no_alloc` is proven, not documentation.** It rejected a wire format. If it refuses a
function, the function really does allocate — consider whether the DESIGN can avoid it before
dropping the attribute.

**A new module's `pub fn` can break an old module.** Check a proposed public name against the
intrinsic list and the tree before using it: `grep -rn "fn <name>" examples/std/`.

**`jestyrc run <file>.jtr <args>` does not forward arguments** to the program. Build the exe and
run it directly, which is what the Rust test does.
