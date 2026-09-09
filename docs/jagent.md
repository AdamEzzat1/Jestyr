# `jagent` — the Jestyr edge agent

`jagent` is a small operational agent for one machine: a few named jobs in an INI file, a
durable history of every run, and a local HTTP API that reports health, metrics, the job
table and that history. It runs on a laptop, a VPS, a build box or a homelab node, and it is
built from parts the standard library already had — `std/sysproc` and `std/sandbox` run the
jobs, `std/kv` keeps the history, `std/httpd` is the API, `std/config` + `std/ini` +
`std/syswatch` are the configuration and its reload, `std/service` and `std/metrics` are the
health and the numbers.

It is the smallest credible version of a systemd-unit / cron / CI-runner hybrid, and it is
built to grow: one job kind today, a table with the column for the next one.

| what | where |
|---|---|
| the library | `examples/std/jagent.jtr` |
| the command | `examples/std/jagent_cli.jtr` |
| the pinned demo | `examples/std/jagent_demo.jtr` (`jagent_runs_records_serves_and_reloads`) |
| the suite | `examples/std/jagent_test.jtr` (13 cases, registered in `io_suites_pass`) |
| the command's own test | `jagent_cli_runs_status_logs_and_serves` (a real `serve`, killed after) |
| the benchmark script | `examples/std/jagent_bench.jtr` |
| the demo configuration | `examples/std/fixtures/jagent.ini` |

## The problem it solves

A machine has a handful of things that must run — a health probe, a log rotation, a backup,
a build — and three questions about each of them: *did it run*, *did it succeed*, and *what
did it say*. Cron answers none of those after the fact; a full orchestrator answers all of
them at the cost of a cluster. `jagent` answers them for one box with one binary and one
file: every run is a record you can read back a week later, and the API is the same answer
over a socket.

## Building and running

```bash
jestyrc build examples/std/jagent_cli.jtr      # the binary: <temp>/jestyr_jagent_cli.exe
jestyrc run   examples/std/jagent_demo.jtr     # the transcript, end to end, in one process
jestyrc test  examples/std/jagent_test.jtr     # the suite
jestyrc run   examples/std/jagent_bench.jtr    # the numbers
```

`jestyrc run` forwards no arguments, so the command is built once and run by path. With the
fixture beside it:

```bash
jagent status
jagent run health-check
jagent logs health-check
jagent serve
```

## Commands

| command | does | exit |
|---|---|---|
| `jagent [-c cfg] status` | the configuration, the store, every job with its last run | 0; 1 when the file did not load (the faults are printed) |
| `jagent [-c cfg] run <job>` | run it now, record it, print the summary | 0 when the job succeeded; 1 otherwise, or for a job that does not exist |
| `jagent [-c cfg] logs <job>` | the last 20 runs of one job, oldest first | 0; 1 for a job that does not exist |
| `jagent [-c cfg] schedule <job>` | run it every `interval_ms` until SIGINT/SIGTERM | 0; 1 when any run failed, or the job has no interval |
| `jagent [-c cfg] serve` | the API, the reload, the schedule, until SIGINT/SIGTERM | 0 for a clean halt; 1 for a failed/abandoned one; 2 when refused (`tls = true`) |

`-c` defaults to `jagent.ini` in the working directory. Results go to **stdout**; the
agent's own log records (logfmt, one per run and per reload) go to **stderr**.

## The configuration

```ini
[agent]
host = 127.0.0.1        # 127.0.0.1, localhost, or 0.0.0.0 (every interface)
port = 0                # 0 asks the platform; `serve` prints the port it got
tls = false             # true is REFUSED — see "Security"
store = jagent.db       # the history file, created on first use

[job.health-check]
kind = command          # the only kind today; the default
command = echo healthy  # runs through the platform shell (cmd.exe /c, sh -c)
timeout_ms = 10000      # required to be positive; default 5000
interval_ms = 0         # > 0: `serve` and `schedule` run it on that interval; default 0

[job.rotate-log]
command = echo rotated
```

**The file declares its own schema.** A load is two passes: the `[job.NAME]` headers are
scanned first and each one declares its four keys, then `std/ini` applies the file against
that schema. So a key that is not one of the four, a section that is not a job, a name with
a character outside `[A-Za-z0-9_-]`, a job declared twice, a missing or empty `command`, a
`kind` that is not `command`, a non-positive `timeout_ms`, a negative `interval_ms`, a host
the server cannot bind or a port off the end are all **faults that name their line**:

```
error[E-INI]: refused by the schema
  --> agent.ini:3:1
   |
 3 | timeout_ms = soon
   | ^^^^^^^^^^ not an integer
```

**A load applies whole or not at all.** The new file builds a candidate configuration and
job table, and only a candidate with zero faults replaces the current one; a file with three
good jobs and one bad line changes nothing. `generation` counts the loads that changed
something; identical bytes are `same` and touch nothing. A reload resets every schedule
(the next run is one interval after the reload) — stated rather than hidden, because
carrying a due time across a reload that may have changed the interval is a rule with more
cases than callers.

## The API

`serve` binds `agent.host:agent.port` and answers, in plain text, one fact per line:

| route | answers |
|---|---|
| `GET /health` | `phase=ready ready=true live=true inflight=0` — `std/service`'s two questions; 503 once halted |
| `GET /metrics` | every counter, gauge and histogram, name-ordered (`std/metrics`'s text form) |
| `GET /jobs` | one line per job: `name kind=… timeout_ms=… interval_ms=…` |
| `GET /jobs/:name` | the job's definition and its last run; 404 for an unknown name |
| `POST /jobs/:name/run` | runs it now, answers the run's summary; 404 unknown; 405 for any other method |
| `GET /logs` | the last 20 runs of every job, oldest first |
| `GET /logs/:name` | the last 20 runs of one job; 404 for an unknown name |

Anything else is `std/httpd`'s own 404, a non-HTTP request its 400 (and the connection is
closed); neither reaches the agent, and `jagent_http_requests` counts only the routed ones.

A run requested over the API is executed on the server's thread: **the API is quiet for that
job's duration, bounded by its timeout**. That is the reason a timeout is required to be
positive, and the reason there is no concurrency yet (see below).

The demo's `/metrics` after four runs:

```
counter jagent_config_reloads 1
counter jagent_http_requests 7
gauge jagent_jobs 4
histogram jagent_run_ms le 10 4
…
counter jagent_runs_failure 1
counter jagent_runs_no_such_job 1
counter jagent_runs_success 3
counter jagent_runs_timeout 0
counter jagent_runs_total 4
counter jagent_runs_unrecorded 0
counter service_accepted 4
counter service_completed 3
counter service_failed 1
```

Metrics are process-lifetime: they start at zero with the process, where the history does not.

## Data and storage

The history is a `std/kv` store — an append-only log with a memory-resident index — at
`agent.store`. Three keys per run, written as **one batch**, so a crash can never leave the
counter ahead of the records or a job pointing at a run that was never written:

| key | value |
|---|---|
| `run:<seq, 12 digits>` | the record, as JSON |
| `last:<job>` | the sequence number of that job's latest run |
| `seq` | the last sequence number |

The record:

```json
{"seq":1,"job":"health-check","status":"success","code":0,"started_at":1700000000,
 "duration_ms":12,"out_bytes":9,"output":"healthy\r\n","error":""}
```

`started_at` is wall-clock seconds (`time(NULL)`; the CLI's clocks are real, the demo's are
pinned). `duration_ms` is on the agent's monotonic clock. `output` is the first 240 bytes the
command wrote (stdout and stderr merged), `out_bytes` is all of it. `error` is `exit code N`,
`killed by signal N`, `timed out after N ms`, or empty.

The store is a `std/kv` file like any other: `kv.compact` reclaims superseded pointer and
counter records, `kv.snapshot` is the backup, and the bench measures the growth (below).
Reading is by write order: `logs` scans the live `run:` slots, parses each record and filters
by the `job` field — a job whose name is a prefix of another's (`db`, `db-backup`) filters
exactly.

**A job that does not exist writes nothing.** There is no run to record, and a history entry
for a name nobody configured would be a lie the next `status` repeats. **A store that cannot
be opened does not stop a run** — the operator asked for it — but the run reports
`recorded=false` and `jagent_runs_unrecorded` counts it.

**`recorded=true` means the bytes reached the disk.** The batch is committed and then
`kv.sync`ed (`fflush` + `fsync`) before the run is reported, so a `jagent serve` that is
killed — the ordinary end of a service — keeps every run it answered. That was measured the
other way first: the CLI test's run over the API was answered `recorded=true` and was absent
from the file after the kill, because `std/kv` appends through the C library's buffer and
leaves the flush to the caller. A run is rare and the history is the point, so the flush is
paid per run (about a millisecond here); a sync that fails reports `recorded=false`.

## Failure semantics

| status | when | recorded |
|---|---|---|
| `success` | the command exited 0 | yes |
| `failure` | a non-zero exit, a signal, or a command that could not be started | yes |
| `timeout` | the command outlived `timeout_ms`; it and its whole process tree were killed | yes |
| `config-error` | the job exists but cannot be run (an unknown kind; the agent is draining) | yes |
| `no-such-job` | no job by that name | **no** |

**A timeout kills the tree.** A command runs through the platform shell, so the child the
agent holds is `cmd.exe` or `sh` and the work is under it. Killing the shell alone would
orphan the work (the defect `std/plugin` recorded and closed), so every job starts *apart*
inside a `sandbox.Group` — a Job object on Windows, a process group on POSIX — joins it
before it runs an instruction, and a timeout terminates the child, then the group, and waits
up to two seconds for the group to empty. `tree_reaped` in the run reports whether it did;
it is a measurement, never an assumption, and the suite asserts it against a real sleeper.

The output is drained *while* the child runs, gated on `sysproc.output_ready`, so a command
that writes more than a pipe buffer cannot deadlock the agent. A run never blocks on the
child's stdin: it is closed at start.

The service's two questions come apart on shutdown: a draining agent is **live but not
ready**, refuses new runs as `config-error`, finishes the one in flight, and `halt` reports
`clean`, `abandoned` or `failed` — `failed` whenever any run failed, because that is the
reason an operator wants first. The demo's shutdown says `failed` for exactly that reason.

## Security model, and TLS

The API is **unauthenticated**. Bind it to `127.0.0.1` (the default) and it is reachable by
whatever runs on the box; bind it to `0.0.0.0` and it is reachable by whatever reaches the
box. Nothing in the API can change the configuration or run a command that is not already
in the file, but `POST /jobs/:name/run` runs one, so on an open interface it must sit behind
something that authenticates. Commands run with the agent's own user, environment and
working directory; there is no per-job sandbox yet (`std/sandbox` has the jail; nothing here
asks for it).

**TLS is not served, and `tls = true` is refused rather than faked.** `std/tls` exists and
works (a real handshake over loopback, `tls_test`), but `std/httpd` reads its sockets through
`std/sysnet` directly and has no hook for a session in between; wiring one is a change to
the server, not to this agent. So `serve` under `agent.tls = true` exits 2 with the reason,
`jagent.tls_supported()` answers false so a caller can ask first, and the suite pins both.
The gap is stated here rather than served in plaintext under a setting that says otherwise.

## Performance

Measured by `examples/std/jagent_bench.jtr` on 2026-09-08, Windows 11, an x86-64 laptop,
mingw gcc 8.3 at the tree's locked flags, one run, nothing else loaded. These are numbers
to compare against, not thresholds — the tree has no wall-clock gate, and one that flaked on
a loaded runner would be worse than none.

| what | measured | notes |
|---|---|---|
| configuration load, ten jobs (`load_text`) | 28 µs | the two-pass scan plus `std/ini`; alternating texts so no load is `same` |
| a trivial command job (`exit 0`), start → drain → reap → sweep → record → sync | 65–83 ms | dominated by `cmd.exe`'s own start (`std/plugin` measured 75–120 ms); all 20 trees confirmed reaped; two runs gave 65 and 83 |
| one history entry (one `kv` batch of the three keys, a 150-byte record, synced) | 909 µs | what `run_job` pays over the child; 9 µs of it is the write, the rest is the `fsync` |
| storage after 1,000 entries | 239,918 bytes; 239 bytes per entry | 158 KB live, 34 KB superseded pointers/counters; `compact` → 188,120 bytes |
| `GET /health`, a fresh connection each time | 15.5 ms | see below |
| `GET /health`, one kept-alive connection | 15.5 ms | |
| `jagent status` (the whole process, from the shell) | ~90–100 ms | measured by hand with the CLI; the process start and the store replay |

**The `/health` figure is the poll's granularity, not the server's work.** The bench drives
the server with `httpd.serve_for(sv, net, 1)` between requests and Windows rounds a 1 ms
wait to its ~15 ms timer tick; the same loop on Linux is expected in the tens of
microseconds. A serving process spends that wait idle in the poll, so it is latency a client
sees, not CPU the agent burns. Re-measure before quoting it as the server's cost.

**Memory and resource cleanup** is asserted, not measured: every run releases its child
(`sysproc.release`) and its group (`sandbox.release_group`), the bench prints "children
started, trees confirmed reaped: 20, 20", and the suite's timeout case checks the tree of a
killed sleeper. Nothing in the language reads the process's own memory; by hand, the CLI's
working set after 20 runs was flat within the noise of a Windows process, and the store's
in-memory arena grows by the record size per run until a `compact` — which is the shape
`std/kv` documents.

## Known limitations

- **One job kind** (`command`). The table has the column; a second kind is a constant, an
  arm in `execute`, and a check at load.
- **No concurrency.** A run blocks the API for its duration, bounded by its timeout. Two
  scheduled jobs due in the same tick run one after the other.
- **No TLS** (above). **No authentication** on the API.
- **No per-job environment, working directory or user**; `sysproc.start_piped_at` offers the
  first two and `std/sandbox` a jail — nothing here asks yet.
- **No retries, no dependencies between jobs, no backoff**; `std/supervise` has the policy
  vocabulary if a restart-until-stable job is wanted.
- **Windows-verified only.** The POSIX branches (`sh -c`, process groups, inotify) are the
  standard library's and run on the Linux ladder; this agent has only been run here.
- **The output preview is 240 bytes** and the record holds only that; a job whose whole
  output matters should redirect it.
- **`schedule` and `serve` reset every schedule on a reload**, and the first run of a
  scheduled job is one interval after the load — not at once.
- **JSON numbers in a record are read back as `i32`** (`std/json`'s node width): fine for
  every field today; `started_at` overflows it in 2038.
- **The history is synced per run, not the directory entry.** `file.sync` documents the
  gap: a brand-new store file's name is not durable on POSIX until its directory is synced,
  which this tree does not do. A crash in the first seconds of a fresh store can lose the
  file; a store that already exists loses nothing.

## What this project exposed

Four standard-library collisions, none previously reachable because no program had combined
these modules: `service.accept` against `sysnet`'s `accept(2)`, `syswatch`'s `close` and
`WaitForSingleObject` against `sysnet`'s and `sysproc`'s, and `sysproc`'s `poll` against
`syspoll`'s. An extern's bare name is reserved in every module that links it, so the second
binder of each pair now uses the declared-alias form (`sys_accept`, `watch_close`,
`watch_wait`, `pipe_poll`); the C symbols and every consumer are unchanged. Recorded in the
Tier 5 handoff note.

And one non-defect worth a sentence: the first link of the suite failed on a bare
`jestyr_impl_Drop__Writer__drop` — the shape of A13, recorded as closed — and it was the
prebuilt `jestyrc.exe` being older than the source. `cargo build --release` before believing
a compiler symptom.

## Next steps toward a production-grade edge agent

1. **TLS in `std/httpd`**: a `tls.Session` per connection, read/write through it instead of
   `sysnet`, and the `tls_cert`/`tls_key` keys the schema already declares. `tls_supported()`
   becomes true and nothing in this agent changes.
2. **A token on the API**: `std/httpd` middleware answering 401 without a bearer the file
   names, redacted in `status` through `std/config`'s secret declaration.
3. **Concurrent runs**: a run per `sysproc.Child` in a table stepped from the serve loop
   (`std/supervise`'s struct-of-arrays shape), so the API answers while a job runs and two
   due jobs overlap.
4. **Per-job `cwd`, environment and jail** through `sysproc.start_piped_at` and
   `sandbox.Jail`.
5. **Retry and backoff policies** per job, from `std/supervise`'s `Policy`.
6. **Compaction on a schedule** (`kv.dead_bytes` is the query), a size cap, and a retention
   window for the history.
7. **A second job kind** — an HTTP probe through `std/httpc` is the obvious one: no shell,
   no process, a status code as the exit code.
8. **The Linux ladder**: the suite and the demo have only run on Windows.
