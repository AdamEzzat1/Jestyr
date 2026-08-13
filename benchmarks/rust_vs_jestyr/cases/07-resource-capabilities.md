# Case 7 — Resource Capabilities

**Status: planned** (second pass; may be part design-only)

## What it tests

Linear/affine resource handling: a handle that must be released exactly
once, cannot be used after release, and cannot be duplicated. Rust: RAII
+ move semantics + `Drop`, `MutexGuard`-style scoped capabilities.
Jestyr: `take` parameters, RAII `Drop` (field/payload auto-drop landed —
see B1 in the self-hosting notes), and `region` tokens. If a
linear-capability design exists but is unimplemented, that part of the
case is written as a clearly-marked design note, not code.

## Sketch

A simulated device pool: acquire, use, hand off (move to another
function), release; a rejected double-release and a rejected
use-after-move on both sides, recorded as diagnostics.
