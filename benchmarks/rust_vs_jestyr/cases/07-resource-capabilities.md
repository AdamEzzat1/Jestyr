# Case 7 — Resource Capabilities

**Status: implemented** (rust/std, jestyr) **+ documented gaps** — the
linear-capability comparison is design-only (safety-mosaic item 7 is
unimplemented) and is replaced by two MEASURED gap probes.

## What it tests

Affine resource handling: a device handle threaded through charge
(owned mutation), transfer (handoff), audit (borrow) — 2M pipeline
iterations — plus the rejection story: what happens when code uses a
capability after giving it away.

## What each side actually expressed

- **rust-std**: by-value moves (`mut d: Device` params), `&Device`
  audit. Use-after-move is refused (E0382, `rejected.rs`), and `Drop`
  would release exactly once.
- **jestyr**: `take` params — directly mutable and returnable, so the
  shapes match Rust one-for-one. Zero annotations beyond the modes.

## The measured gaps (this case's real finding)

Two probes run during this suite (see results/ANALYSIS.md):

1. **Use-after-take compiles.** The giver's binding is not poisoned;
   the read returns the stale copy (deterministic C value semantics —
   not UB, but not a move either). `resource_capabilities_gap.jtr`
   keeps this pinned as a KNOWN-GAP file that compiles today.
2. **A take parameter is never dropped by the callee.** With a `Drop`
   impl, move-in-and-discard runs zero drops anywhere — a resource
   leak. (Caller correctly skips its drop as moved-out; the callee
   never inserts one.)

Rust's affine story is complete; Jestyr's `take` currently conveys
ownership for the caller's drop-elision but enforces nothing on reuse
and completes nothing on the callee side. This is exactly the data
safety-mosaic item 7 (linear capabilities) needs.
