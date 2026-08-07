# docs/ — supplementary documentation

Start at the repo root [README.md](../README.md) for current status, the claims
and how to verify them, and a map of the documentation. This folder holds the
deeper design docs and the project's development history.

## Design & reference docs (this folder)

Topic docs written as features landed — `attributes.md`, `error-handling.md`,
`error-payloads.md`, `unsafe-contract.md`, `ctfe-tiers.md`, `loops-spec.md`,
`obligations.md`, `diagnostics-json.md`, and friends. Each states what is
implemented and how to exercise it.

## `handoffs/`

Workstream handoff documents (concurrency, numerics, parallelism, modules,
tooling, self-hosting phases). These are internal development logs, kept for
provenance — status lines and counts inside them reflect the moment they were
written, not the current state. Trust the root README for current numbers.

## `session-notes/`

Per-session summaries written as each workstream landed (debug info,
self-hosting, numerics, traits, concurrency, etc.). Each is self-contained:
what shipped, design decisions, file/line anchors, and the "what's next" for
that workstream at the time. Same caveat as `handoffs/`: historical record,
not current status.
