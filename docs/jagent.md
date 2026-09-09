# `jagent` — the Jestyr edge agent

`jagent` is a small operational agent for one machine: a few named jobs in an INI file, a
durable history of every run, a local HTTP API that reports health, metrics, the job table
and that history, and an operator's layer that finds the configuration, writes a starter,
checks the machine and answers the questions an operator asks first. It runs on a laptop, a
VPS, a build box or a homelab node, and it is built from parts the standard library already
had — `std/sysproc` and `std/sandbox` run the jobs, `std/kv` keeps the history, `std/httpd`
is the API, `std/config` + `std/ini` + `std/syswatch` are the configuration and its reload,
`std/service` and `std/metrics` are the health and the numbers.

It is the smallest credible version of a systemd-unit / cron / CI-runner hybrid, and it is
built to grow: one job kind today, a table with the column for the next one.

| what | where |
|---|---|
| the runner (library) | `examples/std/jagent.jtr` |
| the operator's layer (library) | `examples/std/jagent_ops.jtr` — discovery, `init`, `doctor`, `ask` |
| the command | `examples/std/jagent_cli.jtr` |
| the project: build plan and page | `tools/jagent/build.jestyr`, `tools/jagent/README.md` |
| the pinned demo | `examples/std/jagent_demo.jtr` (`jagent_runs_records_serves_and_reloads`) |
| the suites | `examples/std/jagent_test.jtr` (20), `examples/std/jagent_ops_test.jtr` (13) — registered in `io_suites_pass` |
| the TLS listener | `examples/std/httpds.jtr` — `std/httpd` over `std/tls`, one request per connection, every blocking call bounded |
| the command's own tests | `jagent_cli_runs_status_logs_and_serves` (a real `serve`, killed after), `jagent_cli_init_doctor_and_ask`, `jagent_build_plan_names_the_binary` |
| the benchmark script | `examples/std/jagent_bench.jtr` |
| the demo configuration | `examples/std/fixtures/jagent.ini` |
| service notes | `docs/jagent-systemd.service.example`, `docs/jagent-windows-service.md` |

## The problem it solves

A machine has a handful of things that must run — a health probe, a log rotation, a backup,
a build — and three questions about each of them: *did it run*, *did it succeed*, and *what
did it say*. Cron answers none of those after the fact; a full orchestrator answers all of
them at the cost of a cluster. `jagent` answers them for one box with one binary and one
file: every run is a record you can read back a week later, and the API is the same answer
over a socket.

## From an example to a project

The first version was an example: a library, a command built by path into the temp
directory, a fixture. What changed to make it a project, and what deliberately did not:

- **A build plan names the executable.** `tools/jagent/build.jestyr` is a `jestyrc plan`
  script with one target, `examples/std/jagent_cli.jtr → jagent`, so `jestyrc plan
  tools/jagent/build.jestyr --build` from the repository root produces `./jagent` (`.exe` on
  Windows) where the build was invoked. The plan is evaluated, never run, and is pinned by
  a test.
- **The sources did not move.** A Jestyr module's imports resolve relative to the file that
  names them (`src/module.rs`), so a command under `tools/` would need
  `../../examples/std/…` imports — a spelling nothing else in the tree uses and the
  self-hosted loader has never been asked for. The plan is the first-class part; the path is
  not. `tools/jagent/README.md` says so, and where every part is.
- **The runner and the operator's layer are two modules.** `std/jagent` is unchanged in
  semantics (it gained three readers: `sync_store`, `last_record`, `config_text`).
  `std/jagent_ops` is everything an operator does *around* a run — discovery, the starter,
  the self-check, the questions — testable without a process, and the command stays thin.
- **No installer.** `jc add` adds a *dependency* to a package manifest; it does not install
  a tool. The binary has no runtime files: copy it where it is wanted.

## Building and running

```bash
jestyrc plan tools/jagent/build.jestyr --build   # -> ./jagent (.exe on Windows)
jestyrc build examples/std/jagent_cli.jtr         # or: <temp>/jestyr_jagent_cli(.exe)
jestyrc run   examples/std/jagent_demo.jtr        # the transcript, end to end, in one process
jestyrc test  examples/std/jagent_test.jtr        # the runner's suite
jestyrc test  examples/std/jagent_ops_test.jtr    # the operator layer's suite
jestyrc run   examples/std/jagent_bench.jtr       # the numbers
```

`jestyrc run` forwards no arguments, so the command is built once and run by path. The
smoke test of a built binary, in an empty directory:

```bash
jagent init                 # jagent: created jagent.ini
jagent doctor               # ... result: ok warnings=0 errors=0
jagent run health-check     # job=health-check status=success code=0 seq=1 recorded=true ...
jagent ask "check my machine"
```

## Commands

| command | does | exit |
|---|---|---|
| `jagent [-c cfg] status` | the configuration, the store, every job with its last run | 0; 1 when the file did not load (the faults are printed) |
| `jagent [-c cfg] run <job>` | run it now, record it, print the summary | 0 when the job succeeded; 1 otherwise, or for a job that does not exist |
| `jagent [-c cfg] logs [<job>]` | the last 20 runs of one job, or of every job, oldest first | 0; 1 for a job that does not exist |
| `jagent [-c cfg] run group <group>` | every member in file order, each its own record; the summary and one line per member | 0 when every member succeeded; 1 otherwise, or for a group that does not exist |
| `jagent [-c cfg] jobs` | the job table, one line each | 0 |
| `jagent [-c cfg] groups` | every group and its members, in file order | 0 |
| `jagent [-c cfg] compact` | apply the retention policy, then rewrite the store with only its live records | 0; 1 when the store did not open or the rewrite failed (the store is then as it was) |
| `jagent [-c cfg] schedule <job>` | run it every `interval_ms` until SIGINT/SIGTERM | 0; 1 when any run failed, or the job has no interval |
| `jagent [-c cfg] serve` | the API (plain, or TLS when `tls = true`), the reload, the schedule, until SIGINT/SIGTERM | 0 for a clean halt; 1 for a failed/abandoned one; 2 when refused (`tls = true` without both files, or a certificate that does not load) |
| `jagent [-c cfg] check` | load the configuration and say so: `config: ok path=… jobs=N` | 0; 1 with the faults on stderr and nothing on stdout |
| `jagent [-c cfg] config validate` | as `check` | as `check` |
| `jagent [-c cfg] config show` | the configuration text in force, secrets redacted (`token = ****`) | 0; 1 when it did not load |
| `jagent [-c cfg] doctor [--no-probes]` | the self-check, one line per subsystem (below) | 0 when no check failed; 1 otherwise |
| `jagent [-c cfg] ask <question…>` | the operator's questions, answered from the record (below) | 0 answered (a refusal is an answer); 1 for a question it does not know, or no configuration |
| `jagent [-c cfg] init [--force]` | write the starter configuration | 0 created; 1 the file exists (untouched) or could not be written |

Results go to **stdout**; the agent's own log records (logfmt, one per run and per reload)
go to **stderr**. Exit 2 is usage. There is no `--format json`: the argument parser is
hand-rolled and the text lines are the contract; a JSON mode is future work, not a flag that
half exists.

### Where the configuration comes from

In this order, and `jagent_ops.discover` says which step chose it:

1. `-c <path>`, when given — a path that does not exist is still the answer, and the load
   reports `cannot read`;
2. `$JAGENT_CONFIG`, when set — read once at the top of `main` and handed to `discover` as a
   value, which is what makes the order a unit test rather than a process test;
3. `jagent.ini` in the working directory.

When none applies the default *name* is still the answer, so an error names the file that was
looked for. A user or system directory (`~/.config/jagent`, `/etc/jagent/agent.ini`) is not
searched; the service notes set `JAGENT_CONFIG` instead.

## The configuration

`jagent init` writes this, comments on their own lines because `std/ini` reads a value to
the end of its line:

```ini
[agent]
host = 127.0.0.1
port = 0
tls = false
store = jagent.db

[job.health-check]
kind = command
command = echo healthy
timeout_ms = 10000
```

| key | meaning | default |
|---|---|---|
| `agent.host` | `127.0.0.1`, `localhost`, or `0.0.0.0` (every interface — `doctor` warns) | `127.0.0.1` |
| `agent.port` | 0 asks the platform; `serve` prints the port it got | `0` |
| `agent.tls` | `true` serves the API over TLS through `std/httpds` — see "Security" | `false` |
| `agent.tls_cert`, `agent.tls_key` | the certificate and its private key, PEM files; both required when `tls = true`, else `serve` refuses (exit 2) | unset |
| `agent.store` | the history file, relative to the working directory, created by the first run | `jagent.db` |
| `agent.token` | a bearer token every API request but `GET /health` must carry; declared **secret**, so every rendering prints `****` | unset: no gate |
| `agent.history_max_runs` | keep at most this many run records (the oldest go, never a job's latest); 0 keeps everything | `0` |
| `agent.compact_after_dead_bytes` | rewrite the store once its dead bytes reach this; 0 never | `0` |
| `job.NAME.kind` | `command` or `http-check` | `command` |
| `job.NAME.command` | `command`: runs through the platform shell (`cmd.exe /c`, `sh -c`); required, non-empty | — |
| `job.NAME.cwd` | `command`: the working directory; unset inherits; one that does not exist fails the run | unset |
| `job.NAME.env` | `command`: ONE `NAME=value` appended to the inherited environment | unset |
| `job.NAME.url` | `http-check`: `http://<ipv4-or-localhost>[:port]/path`; required; https is refused, not half-checked | — |
| `job.NAME.expect_status` | `http-check`: the status that is success, 100..599 | `200` |
| `job.NAME.timeout_ms` | positive; a command and its whole tree are killed at it; an http-check's socket is bounded by it | `5000` |
| `job.NAME.interval_ms` | > 0: `serve` and `schedule` run it on that interval | `0` |
| `job.NAME.retries` | attempts beyond the first, 0..10; one record per run whatever the count | `0` |
| `job.NAME.backoff_ms` | the wait between attempts, on the caller's clock | `0` |
| `group.NAME.jobs` | `a, b, c`: configured job names, run in this order by `run group NAME`; a group may not share a name with a job | — |

A v3 file, showing each of them:

```ini
[agent]
store = jagent.db
history_max_runs = 1000
compact_after_dead_bytes = 65536

[job.site-health]
kind = http-check
url = http://127.0.0.1:8080/health
expect_status = 200
timeout_ms = 2000
interval_ms = 60000
retries = 2
backoff_ms = 250

[job.backup-home]
command = ./backup.sh
cwd = /srv/backup
env = JAGENT_MODE=prod
timeout_ms = 300000

[group.daily]
jobs = site-health, backup-home
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

## Job kinds, retries, groups, retention

**`http-check`.** A GET through `std/httpc` whose exit code is the response status. The
run is `success` when the status is `expect_status`, `failure` with `unexpected status N`
when it is not, `failure` with `could not connect to host:port` when nothing listens,
`timeout` when the target accepts and never answers within `timeout_ms` (the socket's
receive and send are bounded by it, so it cannot hang), and `config-error` when the URL is
not `http://<ipv4-or-localhost>[:port]/path` — there is no resolver in the tree, so a
hostname is refused at load rather than answered with a hang, and `https://` is refused
rather than checked in plaintext. The preview is `status=N expected=M`; the same record
shape, the same counters; nothing is spawned, so `tree_reaped` is vacuously true.

**`cwd` and `env`** on a command job are the process's at creation — the only time a
process can be given them (`sysproc.start_piped_at`'s argument). `cwd` unset inherits; one
that does not exist fails the run with the directory named. `env` is one `NAME=value`
appended to the inherited block, which is what the standard library offers; the suite
proves the variable reaches the child and the control shows it does not without.

**Retries.** A job with `retries = N` is attempted up to N+1 times; a failed attempt is
followed by `backoff_ms` on the caller's clock. **One record per run**: it carries
`attempts`, the last attempt's status and output, the first attempt's start and the whole
duration. The summary and the log line spell `attempts=N` only when more than one was
made, so a single attempt reads exactly as it always did, and `jagent_run_retries` counts
the retries. `ask "why did X fail"` says `after N attempts` when the record does.

**Groups.** `[group.NAME]` with `jobs = a, b, c` runs its members in file order, each
recorded as its own run — there is no group record, because a group is a way of asking,
not a thing that ran. `run group NAME` prints `group=NAME jobs=N success=S failure=F` and
one run line per member, and exits 1 if any failed. A member that is not a configured
job, an empty list, a group declared twice, or a group named like a job are faults that
name their line; `find_group` and `find_job` never answer each other's names.

**Retention.** `history_max_runs` prunes the OLDEST run records beyond the bound that are
not any job's latest — so `status` and `last:` never point at a missing record — in one
synced batch; `compact_after_dead_bytes` rewrites the store through `kv.compact` once the
dead bytes reach it. `seq` is a counter, never a count: numbers are not reused after a
prune. The policy runs after every `run`, `schedule` iteration and `serve` round
(`jagent.maintain`), and says on stderr what it did; `jagent compact` applies it and
rewrites the store whatever the threshold.

## `doctor`

One line per subsystem, `name: verdict key=value…`, then a result line. Every verdict is
**measured**, and the check **mutates nothing the operator owns**:

```
config: ok path=jagent.ini source=cwd jobs=4 groups=1
store: ok path=jagent.db present=true synced=true runs=12 dead_bytes=2048
history: ok runs=12 max=1000 dead_bytes=2048 compact_after=65536
sysproc: ok spawn=true timeout=true tree_reaped=true children=2 cwd=supported env=supported
http: ok host=0.0.0.0 port=8080 bound=8080
tls: supported requested=false
auth: present kind=bearer exempt=GET/health
reload: ok watched=true dir=.
schedule: ok scheduled=1 of=4
jobs: ok command=3 http-check=1 groups=1 retrying=1
limits: kinds=command,http-check concurrency=none tls=one-request-per-connection preview_bytes=240
risk: warn count=2 token-in-the-clear never-run:rotate-log
result: ok warnings=2 errors=0
```

`source=` is where the configuration was found (`flag`, `env`, `cwd`, `default`). The
`risk:` line is the one place exposure is counted: every tag is a fact from the
configuration or the history, and `warnings=` is their number plus the `reload:` warning
when the watcher could not open. The tags, in the order they are tested:

| tag | when |
|---|---|
| `remote-bind-without-token` | `agent.host = 0.0.0.0` and no `agent.token` |
| `token-in-the-clear` | `0.0.0.0` with a token but `tls = false` |
| `tls-incomplete` | `tls = true` without both `tls_cert` and `tls_key` |
| `never-run:<job>`, `scheduled-never-ran:<job>` | no record for the job (the scheduled form when it has an interval) |
| `last-run-failed:<job>`, `scheduled-last-failed:<job>` | the job's latest record is `failure`, `timeout` or `config-error` |
| `stale:<job>` | scheduled, last run succeeded, and it started more than two intervals ago by the wall clock |
| `history-over-max`, `compaction-due` | the history exceeds `history_max_runs`; the dead bytes have reached `compact_after_dead_bytes` |

| line | what was done | fails / warns when |
|---|---|---|
| `config` | the file is loaded through the real loader; `source=` says where it was found | it cannot be read (`reason=cannot-read`) or has faults (`faults=N`, printed on stderr) — an error; the store, history, bind, reload, schedule, jobs and risk lines are then `skipped reason=no-config` |
| `store` | a present store is opened, `kv.sync`ed, its run count and dead bytes read; an absent one is **not created** — its directory is checked instead | `reason=cannot-open`, `cannot-sync`, `directory-missing` — errors |
| `history` | the live run records against `history_max_runs`, the dead bytes against `compact_after_dead_bytes` | never by itself; the `risk:` line tags `history-over-max` and `compaction-due` |
| `sysproc` | on an agent of its own with no store: a job that must print `jagent-doctor`, then a job that must be killed at 200 ms with its tree confirmed reaped; `cwd`/`env` are the build's contract, stated | any half false — an error; `--no-probes` skips it (`skipped reason=no-probes`) |
| `http` | `agent.host:agent.port` is bound and released; `bound=` is the port the platform gave | `reason=cannot-bind` — an error |
| `jobs` | the job table by kind, the groups, the jobs with retries | never |
| `risk` | the rules above, tagged | `warn count=N …` — N warnings |
| `tls` | what the file asks and, when it asks, whether the certificate and key LOAD (a context is built and freed, nothing bound) | `loaded=false reason=tls_cert-or-tls_key-unset`, or `loaded=false reason=<OpenSSL's words>` — errors, because `serve` would exit 2 |
| `auth` | whether `agent.token` is set | `present kind=bearer exempt=GET/health` when it is; otherwise `missing` with `risk=remote-exposed` on `0.0.0.0` or `risk=local-only` on loopback (the `risk:` line counts the exposure) |
| `reload` | the watcher over the configuration's directory is opened and closed | `watched=false` — a warning |
| `schedule` | how many jobs declare an interval | never; the load already refused a bad one |
| `limits` | the fixed facts a reader should know | never |

The suite watches the controls: a **denied spawner** makes the process probe fail (so the
verdict is a measurement, not an assumption); a **read-only handle** makes the store fail;
a `0.0.0.0` file counts two warnings and a loopback one none; `tls = true` is an error; the
store is absent before and after a passing check.

## `ask` — the operator's layer

A rule-based reader, not a model: no network, no weights, no text it did not read from the
configuration, the job table, the history or the server settings. Ask it what an operator
asks first, and it answers in plain sentences that end in the exact command to run next:

```
$ jagent ask "check my machine"
Configuration jagent.ini is valid: 4 jobs, generation 1.
History jagent.db: 7 runs recorded.
3 of 4 jobs have run at least once.
health-check last succeeded (run #6).
backup-home last failed: failure, exit code 1 (run #7).
rotate-log has never run.
build last succeeded (run #5).
TLS is off; the API binds to loopback (127.0.0.1) without authentication.
Recommended next action: jagent logs backup-home
```

| question shape | answers from |
|---|---|
| *is the agent healthy*, *check my machine*, *status*, *summary* | the configuration, the store, every job's last run, the bind |
| *what failed* | every job whose last run was `failure`, `timeout` or `config-error`, with the status and the error text the record holds |
| *why did `<job>` fail* | that job's last record: `flaky's last run (#2) ended failure: exit code 3.` and that *the record holds no more than that* — a job that succeeded "did not fail", one that never ran "has never run", a name that is not configured is repeated back with the list |
| *what failed recently* | the last twenty records, the failed ones only — history, not just each job's latest |
| *what jobs have never run* | the jobs without a `last:` pointer |
| *what is stale* | the scheduled jobs that never ran, last failed, or whose last success is more than two intervals old |
| *what is risky* | the risk rules (the doctor's tags), as sentences |
| *what changed after reload* | the generation, the loads and rejections, and either what applied or the faults of the rejected load |
| *how did `<group>` go*, *what failed in `<group>`*, *what never ran in `<group>`* | the group's members: all, the failed, the never-run; the counts; the next action for the group |
| *what should I check next* | the first fact that needs attention: a failed job → `jagent logs <job>`; a job that never ran → `jagent run <job>`; a `0.0.0.0` bind with no token → set the host or set `agent.token`; else `jagent doctor` |
| anything with *run*, *execute*, *start*, *launch*, *kill*, *delete*, *edit*, *compact* | a refusal that names the command: `I do not run, change or delete anything. To run a configured job: jagent run <name>; a group: jagent run group <name>. …` |
| anything else | the list of what it answers, exit 1 |

A group named in the question is the subject before any other rule; the refusal comes
before the questions; `next` outranks `check`, so "what should I check next" is the next
question. Every answer comes from the configuration, the job table, the groups, each job's
last record, the last twenty records, the reload counters and the risk rules — nothing
else, and nothing invented.

The rules that make it safe are structural, and each has a test that watched a mutant fail:

- **It cannot run a job.** `jagent_ops.ask` takes no spawner. The refusal is the answer to a
  request, not the barrier — the barrier is the type signature. The suite asks it to run
  `rm -rf /`, `calc.exe` and a configured job by name, and reads the history counter and the
  job table back unchanged.
- **It opens the history only if the file is there.** Asking about a machine that has never
  run anything creates no store (`status` does open one, as it always has).
- **It does not invent a cause.** When the record says `exit code 1`, the answer says
  `exit code 1` and points at `jagent logs`, whose lines carry the output preview.
- **It edits nothing.** There is no path from a question to `init`, the configuration, or
  the store's contents.

There is no LLM behind it and none is planned into this layer: the tree has no local model
interface, and a remote one would move the machine's job table and history off the machine.
If one is wanted later, it belongs *behind* this layer — a summariser of `ask`'s text, never
a source of it.

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

**With `agent.token` set, every route but `GET /health` is behind a bearer token.** The gate
is `std/httpd` middleware, so it runs *before* routing: a request without
`Authorization: Bearer <token>` is answered 401 with `WWW-Authenticate: Bearer` whatever
its path, and learns nothing about the route table. The comparison is constant-time in the
token's length (`config.value_is_ct`), a refusal is counted in `jagent_http_unauthorized`
and not in `jagent_http_requests`, and `GET /health` stays open because a readiness probe
reveals only the service's phase and is the one thing a load balancer must be able to ask
without a secret. A token is picked up by the reload like any other setting: change the
file and the next request is judged by the new one.

A run requested over the API is executed on the server's thread: **the API is quiet for that
job's duration, bounded by its timeout**. That is the reason a timeout is required to be
positive, and the reason there is no concurrency yet (see below).

The demo's `/metrics` after four runs:

```
counter jagent_config_reloads 1
counter jagent_history_compactions 0
counter jagent_history_pruned 0
counter jagent_http_requests 7
counter jagent_http_unauthorized 0
gauge jagent_jobs 4
histogram jagent_run_ms le 10 4
…
counter jagent_run_retries 0
counter jagent_runs_config_error 0
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
`killed by signal N`, `timed out after N ms`, or empty. `ask` reads the same record through
`jagent.last_record`; there is no second copy of any fact.

The store is a `std/kv` file like any other: `kv.compact` reclaims superseded pointer and
counter records, `kv.snapshot` is the backup, and the bench measures the growth (below).
Reading is by write order: `logs` scans the live `run:` slots, parses each record and filters
by the `job` field — a job whose name is a prefix of another's (`db`, `db-backup`) filters
exactly. A job's *last* run is two lookups whatever the history's size.

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
`doctor` runs the same probe on every invocation.

The output is drained *while* the child runs, gated on `sysproc.output_ready`, so a command
that writes more than a pipe buffer cannot deadlock the agent. A run never blocks on the
child's stdin: it is closed at start.

The service's two questions come apart on shutdown: a draining agent is **live but not
ready**, refuses new runs as `config-error`, finishes the one in flight, and `halt` reports
`clean`, `abandoned` or `failed` — `failed` whenever any run failed, because that is the
reason an operator wants first. The demo's shutdown says `failed` for exactly that reason.

## Running it as a service

`serve` is a console program that stops on SIGINT/SIGTERM by draining and halting; exit 0 is
a clean halt, 1 a failed or abandoned one, 2 a refusal that a restart would only repeat
(`tls = true`). It needs a working directory (the store and a relative `-c` path are relative
to it) and somewhere for stdout and stderr to go. `docs/jagent-systemd.service.example` is
the unit a Linux install would start from — `Type=simple`, `KillSignal=SIGTERM`,
`RestartPreventExitStatus=2`, `JAGENT_CONFIG` in the environment — and
`docs/jagent-windows-service.md` says what works on Windows today (a scheduled task) and what
a real service would need (an SCM entry point, a log destination that is not a console).
**Neither is a tested artifact**, and both say so: `jagent` has run only on the development
box, and a Windows Job object without kill-on-close means a `serve` that is killed rather
than signalled leaves a mid-run command running.

## Security model, TLS and authentication

The API is **open unless `agent.token` is set**. Bind it to `127.0.0.1` (the default, and
what `init` writes) and it is reachable by whatever runs on the box; bind it to `0.0.0.0`
and it is reachable by whatever reaches the box. Nothing in the API can change the
configuration or run a command that is not already in the file, but `POST /jobs/:name/run`
runs one, so on an open interface it must carry a token. Without one, `doctor` counts a
`0.0.0.0` bind as two warnings (`http` and `auth`), `serve` prints a warning to stderr
before it binds, and `ask`'s summary carries a `WARNING:` line; with one, all three report
the token as a fact and the warnings go. Commands run with the agent's own user, environment
and working directory; there is no per-job sandbox yet (`std/sandbox` has the jail; nothing
here asks for it). `ask` cannot run anything, and `doctor` runs only its own two probe
commands.

**The token is a secret by declaration.** `agent.token` is declared secret in `std/config`,
which renders it as `****` on every path — so `config show` prints `token = ****`, the
faults, the status and the doctor's report never hold its bytes, and there is no accessor
that returns them: the agent can *compare* the token (`token_matches`, constant-time) and
*report that one is set* (`token_required`), and that is all. The token lives in the
configuration file, so the file's permissions are the token's; `init` writes the key
commented out with that said beside it.

**Plaintext HTTP carries the token in the clear**, so a token over the network wants TLS.
A bearer token over an unencrypted connection is readable by anyone on the path, which on
loopback is the machine's own processes and on `0.0.0.0` is the network. Without TLS a
remote bind with a token is protected against a *caller* who lacks it, not against a
*listener*.

**TLS is served, through `std/httpds`.** `tls = true` with `tls_cert` and `tls_key` (PEM
files; the certificate and key are checked against each other at load) makes `serve` an
HTTPS listener over the same routes and middleware — the token gate included — and
`jagent.tls_supported()` answers true. `serve` refuses (exit 2, with the reason) when either
file is unset or when OpenSSL will not load them; `doctor` reports the same as
`tls: supported requested=true cert=… key=… loaded=true|false`, building the context and
freeing it without binding. The Jestyr suite drives a real round trip: a client trusting the
fixture certificate gets the job table with the token and 401 without it, and a client
speaking plaintext at the TLS port gets nothing back.

**What TLS costs here, stated.** `std/tls` blocks (its header says why: no non-blocking
mode in `sysnet` yet), and `std/httpd` is a poll loop that must never block on one peer. So
`httpds` serves each TLS connection *whole*, on the serve thread: accept, handshake, one
request, close — with **every blocking call bounded by a socket timeout (5 s in the
command)** and **`Connection: close` forced on every answer**. A peer that connects and
never finishes the handshake, or never sends its request, costs the loop up to that bound
and no longer; an idle keep-alive peer cannot hold it at all. While one TLS connection is
served, no other is; the scheduler and the reload run between connections. That is the
right shape for an operator's API on an edge box with a handful of clients and the wrong
one for a busy service; the day `std/tls` grows a non-blocking mode, the session moves
into `httpd`'s connection table and the bound goes. The plaintext listener is unchanged.
No client certificates, no ALPN, no session resumption, no streamed request bodies over
TLS.

## Performance

Measured by `examples/std/jagent_bench.jtr` on 2026-09-09, Windows 11, an x86-64 laptop,
mingw gcc 8.3 at the tree's locked flags, one run, nothing else loaded. These are numbers
to compare against, not thresholds — the tree has no wall-clock gate, and one that flaked on
a loaded runner would be worse than none. The 2026-09-09 machine was noisier than the day
before with nothing else visibly running — the ten-job load measured 71 and 167 µs in two
runs against 28 µs on 2026-09-08, the trivial job 98–134 ms against 65–83 — so every new
number is given as the range of two runs, and the 2026-09-08 figures are kept where they
were the quieter measurement of the same code.

| what | measured | notes |
|---|---|---|
| configuration load, ten jobs (`load_text`) | 28 µs | the two-pass scan plus `std/ini`; alternating texts so no load is `same` |
| configuration validate, a hundred jobs (`load_text`) | 1.3–1.4 ms | the same path over ten times the schema; linear in the job count |
| a trivial command job (`exit 0`), start → drain → reap → sweep → record → sync | 65–83 ms | dominated by `cmd.exe`'s own start (`std/plugin` measured 75–120 ms); all 20 trees confirmed reaped |
| one history entry (one `kv` batch of the three keys, a 150-byte record, synced) | 909 µs | what `run_job` pays over the child; 9 µs of it is the write, the rest is the `fsync` |
| storage after 1,000 entries | 239,918 bytes; 239 bytes per entry | 158 KB live, 34 KB superseded pointers/counters; `compact` → 188,120 bytes |
| `GET /health`, a fresh connection each time | 15.5 ms | see below |
| `GET /health`, one kept-alive connection | 15.5 ms | |
| `jagent init` (the starter written, forced) | 1.4–2.7 ms | one `fs.put`: open, write, close |
| configuration discovery, nothing to find | 77 µs | three steps, the last a stat of a file that is not there — all of the cost |
| `jagent doctor --no-probes` | 7–13 ms | load, store open + sync, bind + release, watch open + close |
| `jagent doctor` with the probes | 330–430 ms | + a child that prints, + a child killed at 200 ms and its tree reaped: the 200 ms budget, two `cmd.exe` starts, the sweep |
| history open, 1,000 records over 50 jobs | 26–37 ms | the store's index replayed — the cost every command pays before it reads |
| `ask "check my machine"`, 50 jobs over 1,000 records | 2.2–2.5 ms | 50 `last:` lookups and 50 record parses (~45 µs a job); independent of the history's length |
| `status`, 50 jobs with their last runs | 1.9–2.5 ms | the same lookups, rendered |
| `logs <job>`, one job filtered from 1,000 records | 20–25 ms | a scan of every live record, each parsed (~22 µs a record); this one grows with the history |
| `jagent status` (the whole process, from the shell) | ~90–100 ms | measured by hand with the CLI; the process start and the store replay |
| **v3 (2026-09-09, one run, the machine quiet)** | | |
| configuration validate, a hundred jobs (`load_text`) | 6.1 ms | **a regression against v2's 1.3–1.4 ms**: ten declared keys per job instead of four, plus the group scan; 60 µs a job |
| `ask "what failed"`, 50 jobs over 1,000 records | 1.7 ms | the `last:` pointers |
| `ask "what is risky"`, 50 jobs over 1,000 records | 1.2 ms | the risk rules over the same pointers |
| `ask "what failed recently"` | 0.6 ms | the last 20 records rendered and filtered |
| http-check, connection refused (`127.0.0.1:1`) | **2.0 s** | Windows retries a refused loopback SYN for ~2 s before `connect` fails; the socket timeout bounds the request, not the connect — see the limitation below; on Linux a refusal is immediate |
| `GET /jobs` with the bearer token, kept-alive | 12.1 ms | the poll tick again; the constant-time compare is not visible against it |
| `GET /jobs` refused 401 (no token), kept-alive | 12.0 ms | the same tick |
| `run group` of five trivial command jobs | 62 ms a member | five `cmd.exe` starts, five records |
| `maintain` over 65 runs with `history_max_runs = 50` | 1.3 ms | 15 records deleted in one synced batch; the compaction did not trigger (2 KB dead against a 4 KB threshold) |
| `maintain` with nothing to do | 4 µs | the check every `serve` round pays |
| a trivial command job, this run | 59 ms | the day's quieter figure; the v2 range stands |

**The `/health` figure is the poll's granularity, not the server's work.** The bench drives
the server with `httpd.serve_for(sv, net, 1)` between requests and Windows rounds a 1 ms
wait to its ~15 ms timer tick; the same loop on Linux is expected in the tens of
microseconds. A serving process spends that wait idle in the poll, so it is latency a client
sees, not CPU the agent burns. Re-measure before quoting it as the server's cost.

**`logs` is the one reader that scales with the history.** `last_record`, `ask` and `status`
go through the `last:` pointers; `logs` scans and parses every live `run:` slot to filter by
job — 20–25 ms at a thousand records, so seconds at a hundred thousand. A per-job index (or
parsing only the `job` field before the whole record) is the fix when that day comes; the
open itself is 26–37 ms at that size, and every command pays it.

**Memory and resource cleanup** is asserted, not measured: every run releases its child
(`sysproc.release`) and its group (`sandbox.release_group`), the bench prints "children
started, trees confirmed reaped: 20, 20", the suite's timeout case checks the tree of a
killed sleeper, and `doctor` reports `children=2` with both trees reaped on every run.
Nothing in the language reads the process's own memory; by hand, the CLI's working set after
20 runs was flat within the noise of a Windows process, and the store's in-memory arena grows
by the record size per run until a `compact` — which is the shape `std/kv` documents.

## Known limitations

- **Two job kinds** (`command`, `http-check`). An http-check names its target by IPv4
  address or `localhost` — no resolver in the tree — and checks `http://` only: `https://`
  is refused at load rather than checked in plaintext, and a TLS client check is the next
  kind. The check reads the status and nothing of the body. **Its `connect` is not bounded
  by `timeout_ms`** — `sysnet` has no non-blocking connect — so a target that black-holes
  the SYN costs the platform's connect timeout, and a refused loopback port costs Windows'
  ~2 s of SYN retries (measured; immediate on Linux). The request and the response are
  bounded, and a target that accepts and never answers is a `timeout` within the budget.
- **One environment variable per command job**, and no per-job user or jail
  (`std/sandbox` has the jail; nothing here asks for it).
- **A group runs its members one after the other**, and a group record is not written; the
  members' records are the group's history.
- **Retention prunes by count and compacts by dead bytes**; there is no age-based window,
  and the pruning scan is linear in the history (it runs only when the bound is exceeded).
- **No concurrency.** A run blocks the API for its duration, bounded by its timeout — and
  by its retries: a job with `retries = 10` and a long backoff can hold the API for the sum.
  Two scheduled jobs due in the same tick run one after the other.
- **TLS is one request per connection, served whole on the loop, every blocking call
  bounded at 5 s** (above). **One token, one scope**:
  every gated route is all-or-nothing; no per-route scopes, no read-only token, no rotation
  without a reload of the file. **No `--format json`.** **No `bench` subcommand**:
  `jagent_bench.jtr` is a script, and folding it into the command would move measurement
  code into the binary for a number an operator reads once.
- **No user or system configuration directory**; `-c`, `$JAGENT_CONFIG`, `./jagent.ini`.
- **Not a service.** A console program with signal handling; the unit file and the Windows
  page are documentation.
- **`ask` knows six question shapes** and matches them by words, case-insensitively. A
  question phrased another way gets the list. It answers from the *last* run of each job; it
  does not read trends.
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

Two non-defects worth a sentence. The first link of the suite failed on a bare
`jestyr_impl_Drop__Writer__drop` — the shape of A13, recorded as closed — and it was the
prebuilt `jestyrc.exe` being older than the source: `cargo build --release` before believing
a compiler symptom. And a test helper named `contains` is refused by the driver because the
name is a compiler intrinsic; the convention is `has`.

## Next steps toward a production-grade edge agent

1. **Non-blocking TLS**: a `WANT_READ`/`WANT_WRITE` mode in `std/tls` over a non-blocking
   `sysnet`, so the session can live in `httpd`'s connection table, keep-alive returns over
   TLS, and one slow peer stops costing the loop its bound.
2. **Token scopes**: a read-only token beside the full one, so a dashboard can read `/logs`
   without being able to `POST …/run`; the middleware already has the request, so a scope is
   a second declared secret and a method check.
3. **A service entry point** on Windows (`docs/jagent-windows-service.md` lists the four
   parts) and a Job object with kill-on-close for it; a first Linux install from the unit.
4. **Concurrent runs**: a run per `sysproc.Child` in a table stepped from the serve loop
   (`std/supervise`'s struct-of-arrays shape), so the API answers while a job runs and two
   due jobs overlap.
5. **A per-job jail** through `sandbox.Jail`, and a per-job user; a full environment block
   rather than one variable.
6. **An https check** (a `tls.Session` client in `execute_http`) and a resolver.
7. **An age-based retention window** beside the count, and a per-job index for `logs`.
8. **The Linux ladder**: the suites, the demo and the command have only run on Windows.
