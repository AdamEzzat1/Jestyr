# The Bruchion fork (2026-09-09) — a pointer

This repository was copied, with its full history, to a sibling directory
`C:\Users\adame\Bruchion` and renamed there: the language is Bruchion, the compiler is
`bruchionc` (with `jestyrc` kept as a forwarding binary), and the edge agent `jagent` is
Pylon (with `JAGENT_CONFIG`, `./jagent.ini` and a `tools/jagent` build plan kept as
aliases). The copy was taken from this branch at `d5d85ed`.

Nothing in this repository was changed by that work except this note. The copy's own
record of what was renamed, what was deliberately kept (the `.jtr` extension, the
`jestyr_` C prefix, the bootstrap file names, the compiler-closure modules' comments, the
history notes), and the compatibility that remains is `docs/migration-from-jestyr.md`
there; its cold-start note is `docs/session-notes/bruchion-handoff.md`.

Two fixes were made in the copy that this repository does not have: `runtime.poll_for`
resumes a wait the poller cut short (the `jstatus` timer flake, measured at about one run
in twenty here, went to 0 of 60 there), and `kv.swap_in` keeps the platform's error code
through the rewrite path instead of answering `KvFailed(-1)`. Both are small and
self-contained (`examples/std/runtime.jtr`, `examples/std/kv.jtr`, `kv_test.jtr`) and would
port back verbatim if wanted; see `docs/session-notes/jagent-v3-flakes-and-open-items.md`
for the measurements that motivated them.
