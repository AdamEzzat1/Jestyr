# Running `jagent serve` on Windows

`jagent` is a console program. It has no Service Control Manager integration — no
`StartServiceCtrlDispatcher`, no service status reporting — so **it is not a Windows
service** and `sc create` alone will not run it (the SCM waits for a status report that never
comes and kills the process). This page records what works today and what a real service
build would need. Nothing here has been installed on a production machine; the commands were
run by hand on the development box.

## What works today: a scheduled task

Task Scheduler runs an ordinary console program at logon or at boot, captures nothing (the
task has no console), and stops it with a hard terminate. That is enough for a box that
belongs to one operator.

```powershell
# From the directory that holds jagent.exe and jagent.ini (or set JAGENT_CONFIG).
$action  = New-ScheduledTaskAction -Execute "C:\jagent\jagent.exe" -Argument "serve" -WorkingDirectory "C:\jagent"
$trigger = New-ScheduledTaskTrigger -AtStartup
Register-ScheduledTask -TaskName "jagent" -Action $action -Trigger $trigger -RunLevel Limited
Start-ScheduledTask -TaskName "jagent"
```

What to know before relying on it:

- **Output is lost.** `serve` prints "listening on 127.0.0.1:PORT" to stdout and its log
  records to stderr; a task has neither. Put a fixed `port =` in the configuration so the
  address is known without reading it, and read the run history with `jagent logs` — every
  run is in the store, whatever happened to the console.
- **Stop is a kill, not a signal.** `Stop-ScheduledTask` terminates the process. `serve`
  never sees SIGINT/SIGTERM, so it does not drain; a run in flight at that moment loses its
  agent and its record is not written — and its process tree is **not** reaped: the job's
  group is a Windows Job object without kill-on-close (`std/sandbox` says why), so a
  command that was mid-run keeps running until it finishes on its own. Every run that *was*
  answered is on disk: `recorded=true` means the batch was synced before it was reported.
- **The working directory matters.** `store = jagent.db` is relative to it, and so is a
  `-c` path that is not absolute. Set it in the action, as above.
- **`jagent doctor` first.** It binds the address and releases it, opens and syncs the
  store, and runs a real child with a real timeout — the three things a service needs and a
  task will not tell you about when they fail.

## What a real service would need

1. **A service entry point in the executable**: `StartServiceCtrlDispatcher`, a
   `ServiceMain` that reports `SERVICE_RUNNING`, and a control handler that turns
   `SERVICE_CONTROL_STOP` into the same drain-then-halt path a signal takes today. The
   agent's loop already polls `sysignal.shutdown_requested`; a stop request would set the
   same flag. This is new extern surface for `std/sysignal` or a sibling module, not a
   change to `std/jagent`.
2. **A log destination that is not a console**: stderr goes nowhere under the SCM. A file
   writer for `std/log` (`writer.to_file`, which does not exist yet) or the Event Log.
3. **A working-directory and environment contract** — the SCM starts services in
   `%SystemRoot%\System32` with a minimal environment, so `JAGENT_CONFIG` and an absolute
   `store` path would be mandatory.
4. **A test that installs and stops it**, on a machine that allows it. The suite has none,
   which is why this page says "not a service" rather than "a service".

Until those exist, a wrapper that provides the SCM contract around a console program (NSSM,
WinSW, or `srvany`) is the honest option; the wrapper's stop is still a kill unless it is
configured to send Ctrl-C, which `serve` does handle.
