# `jagent` — the project

`jagent` is the Jestyr edge agent: named jobs in an INI file, a durable history of every
run, a local HTTP API, and an operator's layer that finds the configuration, writes a starter,
checks the machine and answers the first questions. This directory is the project's shape;
the reference is [`docs/jagent.md`](../../docs/jagent.md).

## Where the parts are

| part | file | why it is there |
|---|---|---|
| the runner (library) | `examples/std/jagent.jtr` | it is standard-library code; the suites and the seed gate live there |
| the operator layer (library) | `examples/std/jagent_ops.jtr` | discovery, `init`, `doctor`, `ask` — testable without a process |
| the command | `examples/std/jagent_cli.jtr` | thin: argv, real clocks, signals, exit codes |
| the build plan | `tools/jagent/build.jestyr` | names the executable and where it lands |
| the starter configuration | `jagent init` writes it; `examples/std/fixtures/jagent.ini` is the demo's |
| the suites | `examples/std/jagent_test.jtr` (13), `examples/std/jagent_ops_test.jtr` (11) | registered in `io_suites_pass` |
| the command's own tests | `src/proptests.rs`, `jagent_command` | a real `serve`, a real `doctor`, the discovery order |

**Why the sources are not under `tools/`.** A Jestyr module's imports resolve relative to
the file that names them (`src/module.rs`), so a command that says `import "jagent"` has to
sit beside `jagent.jtr`; moving it would mean `../../examples/std/…` imports, a spelling
nothing else in the tree uses and the self-hosted loader has never been asked for. The plan
file is what gives the executable a project name and a fixed output path, which is the part
of "first-class" that a path move would have bought.

## Build

From the repository root, with `jestyrc` built (`cargo build --release`) and a C compiler on
`PATH`:

```bash
jestyrc plan tools/jagent/build.jestyr --build     # -> ./jagent  (jagent.exe on Windows)
```

or the single-file form, which puts the binary in the temp directory:

```bash
jestyrc build examples/std/jagent_cli.jtr           # -> <temp>/jestyr_jagent_cli(.exe)
```

There is no installer and no `jc add jagent`: `jc add` adds a *dependency* to a package
manifest, it does not install a tool. Copy the binary where it is wanted; it has no runtime
files of its own. The smoke test of a built binary:

```bash
./jagent init          # writes ./jagent.ini
./jagent doctor        # result: ok warnings=0 errors=0
./jagent run health-check
./jagent ask "check my machine"
```

## Commands

```
jagent [-c <config>] serve                  the API, the reload, the schedule; SIGINT/SIGTERM stops it
jagent [-c <config>] run <job>              run one job now, record it, print the summary
jagent [-c <config>] schedule <job>         run one job on its interval until a signal
jagent [-c <config>] status                 the configuration, the store, every job with its last run
jagent [-c <config>] logs [<job>]           the last 20 runs, of one job or of all
jagent [-c <config>] jobs                   the job table, one line each
jagent [-c <config>] check                  load the configuration and say so (= config validate)
jagent [-c <config>] config show            the configuration text in force, `token = ****`
jagent [-c <config>] config validate        as `check`
jagent [-c <config>] doctor [--no-probes]   the self-check, one line per subsystem
jagent [-c <config>] ask <question...>      the operator's questions, answered from the record
jagent [-c <config>] init [--force]         write the starter configuration
```

The configuration is found in this order: `-c <path>`; `$JAGENT_CONFIG`; `./jagent.ini`.
Results go to stdout, the agent's log records to stderr. Exit codes: 0 succeeded; 1 the job
failed or does not exist, the configuration did not load, a check failed, `init` would have
replaced a file, or the question was not one `ask` knows; 2 usage or a refused `serve`.

## Running it as a service

`docs/jagent-systemd.service.example` is the unit a Linux install would start from and
`docs/jagent-windows-service.md` says what works on Windows today (a scheduled task) and what
a real service would need. Neither is a tested artifact; both say so.
