# jagent v3 — the two flakes, measured, and the open items

Notes from the jagent v3 pass (2026-09-09), written to be carried into a copy of the
language under another name. Nothing here depends on the branch name or the commit hashes;
where a hash appears it is only so the original can be found.

## The verification that closed the pass

| Ladder | Result | Notes |
|---|---|---|
| second (pre-commit tree) | 1375 passed / 2 failed / 3 ignored, 698 s | `jstate_survives_a_torn_batch`, `jstatus_serves_a_connection_without_starving_its_timers` |
| isolated rerun of both | 2 / 0, 3 s | modules untouched by the branch |
| third (committed tree `a3f8241`) | 1377 / 0 / 3, 781 s | fully green |

## The two flakes, measured by repetition

The handoff already said isolation is the wrong discriminator for the status-server flake,
so both were measured by repetition on the quiet machine after the third ladder.

**The status server's timer step** (`sysnet_demo`, "the timer fired, not the socket") fails
about one run in twenty on this branch and at the same rate on the base commit's sources
with the same compiler, with a timer count of zero. Measured: 4 of 70 runs on the branch,
2 of 40 runs of the base commit's `examples/std` extracted with `git archive`. That is
pre-existing, and it reads as the poller's wait not being clamped to the next deadline on
some iterations, not as load and not as a timer that fires late — the count is 0, not 1
arriving slowly.

**The compaction step** (`kv_demo`, `compact` printing `-1`) reproduces two runs in seventy
in the worktree and zero in forty at the base — but the base control ran from a temp
directory while the branch runs sat in the worktree, so that control separates directories
rather than trees. A rename over a just-closed file is exactly what a directory watcher
(Windows Search, an editor's index) disturbs. The sources involved — `kv.jtr`,
`kv_demo.jtr`, `sysfs.jtr`, `alog.jtr`, `fs.jtr` — are byte-identical between the two.
Run the branch's demo from a temp directory before reading anything into the zero.

Both are written up as intermittent, not as load flakes, under *Known flake* in
`jestyr-tier5-next-handoff.md`, with the base-commit control described as above.

## Open items for a fresh session

* The self-hosted compiler's cross-module collision rename reaches a LOCAL: two modules
  exporting `served` plus a parameter named `served` in one of them made the port rewrite
  the parameter's uses to `j_served__m3` while its declaration stayed `j_served` —
  `undeclared` from gcc, `FAIL jagent_cli` / `FAIL jagent_bench` in the build matrix. The
  reference compiler is unaffected, so it is an acceptance divergence. It stays open with a
  minimal repro in the handoff: two modules each exporting `pub fn served() -> i64`, a third
  importing both, and any function in one of them with a parameter or local named `served`.
  The rename must be scoped to item names, or the local must shadow. Worked around by
  renaming the parameter `answered`; the compiler's loader (`cgen.jtr`, `ml_*`) was not
  edited.
* The status-server flake deserves a counter on the runtime wait clamp rather than a wider
  deadline. Do not widen the 500 ms budget; instrument which wait returned without the timer.
* `kv`'s rename path should carry the errno through so a sharing violation can be told from
  anything else. Today `swap_in`'s `rename_replace` failure is caught into `swapped = false`
  and surfaces as `KvFailed(-1)` — the errno is dropped, so every sighting is undiagnosable.
* The Linux CI ladder has not run on any of the jagent work (TLS via `httpds`, the v3
  runner, the operator layer). Every socket suite is a loopback transcript; `tls_test` links
  `-lssl -lcrypto`.

## One tooling trap from the same day

On PowerShell 5.1, `Get-Content -Raw` reads a BOM-less UTF-8 source as code page 1252, and
writing it back as UTF-8 turns every em-dash and box-drawing character into mojibake (2619
non-ASCII bytes from 981 in one file). It is reversible exactly — read as UTF-8, encode with
`GetEncoding(1252)`, write the bytes — but never round-trip a source file through PowerShell
strings to apply a mutant. Use `[IO.File]::ReadAllBytes` / `WriteAllBytes`, or an editor.
