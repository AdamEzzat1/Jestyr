# measure_compiler_memory.ps1 -- peak working set of the COMPILER PROCESS.
#
# What is measured, and the caveats that make the numbers comparable only
# with their footnotes (also in METHODOLOGY.md):
#   - rust: rustc invoked directly on the case's main.rs at -O
#     (single process = the whole compiler incl. LLVM). Idiomatic-track
#     crates are skipped: they need extern crate plumbing.
#   - jestyr: jestyrc emit-c (parse/check/lower/emit, NO gcc). gcc's own
#     peak is not measured -- it forks cc1, whose memory the parent
#     process object does not expose.
# Peak is polled via PeakWorkingSet64 every 5 ms while the process runs.

$root = Split-Path -Parent $PSScriptRoot
$repo = (Resolve-Path (Join-Path $root "..\..")).Path
$jestyrc = Join-Path $repo "target\release\jestyrc.exe"
$scratch = Join-Path $env:TEMP "rvj_mem"
New-Item -ItemType Directory -Force $scratch | Out-Null

# The `rustc` on PATH is the rustup SHIM, which spawns the real compiler
# as a child -- measuring the shim reads a flat ~11 MB. Resolve the real
# toolchain binary through the sysroot instead.
$sysroot = (& rustc --print sysroot).Trim()
$rustcReal = Join-Path $sysroot "bin\rustc.exe"
if (-not (Test-Path $rustcReal)) { throw "real rustc not found at $rustcReal" }

function Peak-MB([string]$exe, [string]$argLine) {
    $out = Join-Path $scratch "out.tmp"
    $err = Join-Path $scratch "err.tmp"
    $p = Start-Process -FilePath $exe -ArgumentList $argLine -PassThru -WindowStyle Hidden `
        -RedirectStandardOutput $out -RedirectStandardError $err
    $peak = 0
    while (-not $p.HasExited) {
        try {
            $p.Refresh()
            if ($p.PeakWorkingSet64 -gt $peak) { $peak = $p.PeakWorkingSet64 }
        } catch {}
        Start-Sleep -Milliseconds 5
    }
    return [math]::Round($peak / 1MB, 1)
}

$cases = @("transient_borrow", "borrowed_projection", "disjoint_mutation", "observer_registry",
           "arena_ast", "dlist", "resource_capabilities", "structured_concurrency", "unsafe_boundary")

$rows = @()
foreach ($c in $cases) {
    $rs = Join-Path $root "rust\std\$c\src\main.rs"
    $jt = Join-Path $root "jestyr\$c.jtr"
    $rustPeak = Peak-MB $rustcReal "--edition 2024 -O --crate-name $c -o `"$scratch\$c.exe`" `"$rs`""
    $jesPeak = Peak-MB $jestyrc "emit-c `"$jt`""
    $rows += [pscustomobject]@{ case = $c; rustc_peak_mb = $rustPeak; jestyrc_peak_mb = $jesPeak }
    Write-Host ("  {0,-24} rustc {1,7} MB   jestyrc {2,6} MB" -f $c, $rustPeak, $jesPeak)
}

$md = @()
$md += "# Peak compiler memory"
$md += ""
$md += "Peak working set of the compiler process, polled every 5 ms."
$md += "rustc: direct invocation at -O (whole compiler incl. LLVM, single process)."
$md += "jestyrc: emit-c only -- gcc (forked cc1) is NOT included. Footnotes matter;"
$md += "see METHODOLOGY.md. Idiomatic-track crates skipped (extern plumbing)."
$md += ""
$md += "| case | rustc peak (MB) | jestyrc peak (MB) |"
$md += "|---|---:|---:|"
foreach ($r in $rows) { $md += "| $($r.case) | $($r.rustc_peak_mb) | $($r.jestyrc_peak_mb) |" }
$md -join "`n" | Out-File -Encoding utf8 (Join-Path $root "results\compiler_memory.md")
Write-Host "wrote results\compiler_memory.md"
