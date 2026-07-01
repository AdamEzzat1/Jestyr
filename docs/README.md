# docs/ — project notes & context backup

Supplementary Jestyr documentation, checked in so a fresh `git clone` on a new
machine carries the full working context (not just the code). The primary design
docs live at the repo root (`jestyr-design.md`, `ROADMAP.md`, `HANDOFF.md`, and the
`*-HANDOFF.md` set); this folder holds two additional backups.

## `session-notes/`

Per-session summaries and handoff notes written to `~/Downloads/` as each workstream
landed (debug info, self-hosting unblockers, numerics, traits, concurrency, etc.).
Each is self-contained: what shipped, design decisions, file/line anchors, and the
"what's next" for that workstream. Read alongside the root handoff docs.

## `claude-memory-snapshot/`

A snapshot of Claude Code's persistent project memory. On the machine it was written,
these live at:

```
~/.claude/projects/C--Users-adame-Jestyr/memory/
```

`MEMORY.md` is the index (one line per memory, loaded each session); the other files
are individual facts (project state, self-host status, numerics, parallelism, the
backup/remotes note, etc.).

**To restore on a new computer:** copy the contents of this folder back into
`~/.claude/projects/<project-slug>/memory/` so a new Claude Code session auto-loads
them. The `<project-slug>` is derived from the checkout path (e.g. a clone at
`C:\Users\you\Jestyr` → `C--Users-you-Jestyr`); create the folder if it doesn't exist.

> This is a point-in-time backup — the live memory files continue to evolve. Re-snapshot
> if you want this folder current.
