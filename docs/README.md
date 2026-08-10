# docs/ — supplementary documentation

Start at the repo root [README.md](../README.md) for current status, the claims
and how to verify them, and a map of the documentation. This folder holds the
deeper design docs and the project's development history.

## Design & reference docs (this folder)

Topic docs written as features landed — `attributes.md`, `error-handling.md`,
`error-payloads.md`, `unsafe-contract.md`, `ctfe-tiers.md`, `loops-spec.md`,
`obligations.md`, `diagnostics-json.md`, and friends. Each states what is
implemented and how to exercise it.

**Frontend**: [`frontend-grammar.md`](frontend-grammar.md) is the EBNF for the
syntax the parser accepts *today* (with an explicit list of where it is
approximate); [`frontend-roadmap.md`](frontend-roadmap.md) is the architectural
assessment and the staged plan for CST/HIR/diagnostics work. The grammar is kept
honest by the `grammar_conformance` tables in `src/proptests.rs`.

## `handoffs/`

**Start here for "what's next":**
[`SAFETY-MOSAIC-AND-FRONTEND-HANDOFF.md`](handoffs/SAFETY-MOSAIC-AND-FRONTEND-HANDOFF.md)
— what the frontend/lowering work left unfinished, the safety-mosaic roadmap
(borrowed projections, genref scopes, disjoint borrowing, region tokens, linear
capabilities), and a measured account of what mirroring a change in the
self-hosted compiler actually costs.

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
