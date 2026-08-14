# run_all.ps1 -- build, verify, and time every implemented case.
#
# Usage:  powershell -File scripts\run_all.ps1  [-Reps 7]
#
# Emits results\latest.json (machine-readable) and results\latest.md
# (human-readable). A case whose tracks disagree on output is reported
# as OUTPUT-MISMATCH and given no timing row.

param([int]$Reps = 7)

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot          # benchmarks/rust_vs_jestyr
$repo = (Resolve-Path (Join-Path $root "..\..")).Path
$jestyrc = Join-Path $repo "target\release\jestyrc.exe"
$rustTarget = Join-Path $root "rust\target\release"

# ---------- case table ----------------------------------------------------
$cases = @(
    @{ name = "transient_borrow";   tracks = @(
        @{ track = "rust-std";       exe = (Join-Path $rustTarget "transient_borrow.exe");   src = (Join-Path $root "rust\std\transient_borrow\src\main.rs") },
        @{ track = "jestyr";         jtr = (Join-Path $root "jestyr\transient_borrow.jtr") },
        @{ track = "jestyr-std";     jtr = (Join-Path $root "jestyr_std\transient_borrow_std.jtr") }
    )},
    @{ name = "borrowed_projection"; tracks = @(
        @{ track = "rust-std";       exe = (Join-Path $rustTarget "borrowed_projection.exe"); src = (Join-Path $root "rust\std\borrowed_projection\src\main.rs") },
        @{ track = "jestyr";         jtr = (Join-Path $root "jestyr\borrowed_projection.jtr") },
        @{ track = "jestyr-std";     jtr = (Join-Path $root "jestyr_std\borrowed_projection_std.jtr") }
    )},
    @{ name = "disjoint_mutation";  tracks = @(
        @{ track = "rust-std";       exe = (Join-Path $rustTarget "disjoint_mutation.exe");  src = (Join-Path $root "rust\std\disjoint_mutation\src\main.rs") },
        @{ track = "jestyr";         jtr = (Join-Path $root "jestyr\disjoint_mutation.jtr") }
    )},
    @{ name = "observer_registry";  tracks = @(
        @{ track = "rust-std";       exe = (Join-Path $rustTarget "observer_registry.exe");  src = (Join-Path $root "rust\std\observer_registry\src\main.rs") },
        @{ track = "rust-idiomatic"; exe = (Join-Path $rustTarget "observer_registry_idiomatic.exe"); src = (Join-Path $root "rust\idiomatic\observer_registry\src\main.rs") },
        @{ track = "jestyr";         jtr = (Join-Path $root "jestyr\observer_registry.jtr") },
        @{ track = "jestyr-std";     jtr = (Join-Path $root "jestyr_std\observer_registry_std.jtr") }
    )},
    @{ name = "arena_ast";          tracks = @(
        @{ track = "rust-std";       exe = (Join-Path $rustTarget "arena_ast.exe");          src = (Join-Path $root "rust\std\arena_ast\src\main.rs") },
        @{ track = "rust-idiomatic"; exe = (Join-Path $rustTarget "arena_ast_idiomatic.exe"); src = (Join-Path $root "rust\idiomatic\arena_ast\src\main.rs") },
        @{ track = "jestyr";         jtr = (Join-Path $root "jestyr\arena_ast.jtr") },
        @{ track = "jestyr-std";     jtr = (Join-Path $root "jestyr_std\arena_ast_std.jtr") }
    )},
    @{ name = "dlist";              tracks = @(
        @{ track = "rust-std";       exe = (Join-Path $rustTarget "dlist.exe");              src = (Join-Path $root "rust\std\dlist\src\main.rs") },
        @{ track = "rust-idiomatic"; exe = (Join-Path $rustTarget "dlist_idiomatic.exe");    src = (Join-Path $root "rust\idiomatic\dlist\src\main.rs") },
        @{ track = "jestyr";         jtr = (Join-Path $root "jestyr\dlist.jtr") },
        @{ track = "jestyr-std";     jtr = (Join-Path $root "jestyr_std\dlist_std.jtr") }
    )},
    @{ name = "resource_capabilities"; tracks = @(
        @{ track = "rust-std";       exe = (Join-Path $rustTarget "resource_capabilities.exe"); src = (Join-Path $root "rust\std\resource_capabilities\src\main.rs") },
        @{ track = "jestyr";         jtr = (Join-Path $root "jestyr\resource_capabilities.jtr") }
    )},
    @{ name = "structured_concurrency"; tracks = @(
        @{ track = "rust-std";       exe = (Join-Path $rustTarget "structured_concurrency.exe"); src = (Join-Path $root "rust\std\structured_concurrency\src\main.rs") },
        @{ track = "rust-idiomatic"; exe = (Join-Path $rustTarget "structured_concurrency_idiomatic.exe"); src = (Join-Path $root "rust\idiomatic\structured_concurrency\src\main.rs") },
        @{ track = "jestyr";         jtr = (Join-Path $root "jestyr\structured_concurrency.jtr") }
    )},
    @{ name = "unsafe_boundary";    tracks = @(
        @{ track = "rust-std";       exe = (Join-Path $rustTarget "unsafe_boundary.exe");    src = (Join-Path $root "rust\std\unsafe_boundary\src\main.rs") },
        @{ track = "jestyr";         jtr = (Join-Path $root "jestyr\unsafe_boundary.jtr") }
    )}
)

# ---------- helpers -------------------------------------------------------
function Count-Loc([string]$path) {
    $n = 0
    foreach ($line in Get-Content $path) {
        $t = $line.Trim()
        if ($t -ne "" -and -not $t.StartsWith("//")) { $n++ }
    }
    return $n
}

function Run-Capture([string]$exe) {
    # stdout captured; exit code preserved
    $out = & $exe 2>$null
    return @{ text = ($out -join "`n"); code = $LASTEXITCODE }
}

function Time-Once([string]$exe) {
    $sw = [System.Diagnostics.Stopwatch]::StartNew()
    & $exe *> $null
    $sw.Stop()
    return $sw.Elapsed.TotalMilliseconds
}

# ---------- environment ---------------------------------------------------
Write-Host "== environment"
$rustcV = (rustc --version)
$cargoV = (cargo --version)
$gccV = (gcc --version | Select-Object -First 1)
$commit = (git -C $repo rev-parse --short HEAD)
$branch = (git -C $repo branch --show-current)
$cpu = (Get-CimInstance Win32_Processor).Name
$os = (Get-CimInstance Win32_OperatingSystem).Caption
Write-Host "  $rustcV / $cargoV"
Write-Host "  $gccV"
Write-Host "  jestyr $branch@$commit"

# ---------- build ---------------------------------------------------------
Write-Host "== building rust workspace"
$manifest = Join-Path $root "rust\Cargo.toml"
$swb = [System.Diagnostics.Stopwatch]::StartNew()
cmd /c "cargo build --release --manifest-path `"$manifest`" >nul 2>&1"
if ($LASTEXITCODE -ne 0) { throw "rust workspace build failed" }
$swb.Stop()
Write-Host ("  ok ({0:n1}s)" -f $swb.Elapsed.TotalSeconds)

Write-Host "== building jestyr cases"
foreach ($case in $cases) {
    foreach ($t in $case.tracks) {
        if ($t.jtr) {
            if (-not (Test-Path $t.jtr)) { Write-Host "  MISSING $($t.jtr)"; $t.exe = $null; continue }
            $stem = [IO.Path]::GetFileNameWithoutExtension($t.jtr)
            $samples = @()
            $failed = $false
            for ($k = 0; $k -lt 3; $k++) {
                $sw = [System.Diagnostics.Stopwatch]::StartNew()
                cmd /c "`"$jestyrc`" build `"$($t.jtr)`" >nul 2>&1"
                $sw.Stop()
                if ($LASTEXITCODE -ne 0) { $failed = $true; break }
                $samples += $sw.Elapsed.TotalMilliseconds
            }
            if ($failed) { Write-Host "  BUILD-FAIL $stem"; $t.exe = $null; continue }
            $t.exe = Join-Path $env:TEMP ("jestyr_" + $stem + ".exe")
            $t.src = $t.jtr
            $t.compile_ms = [math]::Round(($samples | Sort-Object)[1])   # median of 3
            Write-Host ("  {0} ({1} ms)" -f $stem, $t.compile_ms)
        }
    }
}

# per-package rust compile time (cold, deps kept warm)
Write-Host "== rust per-package compile times"
$pkgs = @{ "transient_borrow" = "transient_borrow"; "borrowed_projection" = "borrowed_projection";
           "disjoint_mutation" = "disjoint_mutation"; "observer_registry" = "observer_registry";
           "observer_registry_idiomatic" = "observer_registry_idiomatic";
           "arena_ast" = "arena_ast"; "arena_ast_idiomatic" = "arena_ast_idiomatic";
           "dlist" = "dlist"; "dlist_idiomatic" = "dlist_idiomatic";
           "resource_capabilities" = "resource_capabilities";
           "structured_concurrency" = "structured_concurrency";
           "structured_concurrency_idiomatic" = "structured_concurrency_idiomatic";
           "unsafe_boundary" = "unsafe_boundary" }
$rustCompile = @{}
foreach ($p in $pkgs.Keys) {
    $samples = @()
    for ($k = 0; $k -lt 3; $k++) {
        cmd /c "cargo clean --release -p $p --manifest-path `"$manifest`" >nul 2>&1"
        $sw = [System.Diagnostics.Stopwatch]::StartNew()
        cmd /c "cargo build --release -p $p --manifest-path `"$manifest`" >nul 2>&1"
        $sw.Stop()
        $samples += $sw.Elapsed.TotalMilliseconds
    }
    $rustCompile[$p] = [math]::Round(($samples | Sort-Object)[1])   # median of 3
    Write-Host ("  {0}: {1} ms" -f $p, $rustCompile[$p])
}

# ---------- verify + time -------------------------------------------------
$results = @()
foreach ($case in $cases) {
    Write-Host "== case $($case.name)"
    $tracks = @($case.tracks | Where-Object { $_.exe -and (Test-Path $_.exe) })
    if ($tracks.Count -eq 0) { Write-Host "  no runnable tracks"; continue }

    # output verification
    $outs = @{}
    foreach ($t in $tracks) { $outs[$t.track] = (Run-Capture $t.exe).text }
    $ref = $outs[$tracks[0].track]
    $match = $true
    foreach ($t in $tracks) { if ($outs[$t.track] -ne $ref) { $match = $false } }
    if (-not $match) {
        Write-Host "  OUTPUT-MISMATCH -- no timing recorded"
        foreach ($t in $tracks) { Write-Host ("  [{0}]`n{1}" -f $t.track, $outs[$t.track]) }
    } else {
        Write-Host "  outputs identical across $($tracks.Count) tracks"
    }

    # interleaved timing: discard first round, min of the rest
    $times = @{}
    foreach ($t in $tracks) { $times[$t.track] = @() }
    for ($i = 0; $i -lt ($Reps + 1); $i++) {
        foreach ($t in $tracks) {
            $ms = Time-Once $t.exe
            if ($i -gt 0) { $times[$t.track] += $ms }   # round 0 = warm-up
        }
    }

    foreach ($t in $tracks) {
        $min = ($times[$t.track] | Measure-Object -Minimum).Minimum
        $compileMs = $t.compile_ms
        if (-not $t.jtr) {
            $pkg = Split-Path (Split-Path (Split-Path $t.src -Parent) -Parent) -Leaf
            if ($t.track -eq "rust-idiomatic") { $pkg = $pkg + "_idiomatic" }
            $compileMs = $rustCompile[$pkg]
        }
        $entry = [ordered]@{
            case = $case.name
            track = $t.track
            runtime_ms = [math]::Round($min, 1)
            compile_ms = $compileMs
            binary_bytes = (Get-Item $t.exe).Length
            loc = (Count-Loc $t.src)
            output_match = $match
        }
        $results += [pscustomobject]$entry
        Write-Host ("  {0,-16} {1,8} ms   loc {2,4}   bin {3,8} B   compile {4} ms" -f $t.track, $entry.runtime_ms, $entry.loc, $entry.binary_bytes, $entry.compile_ms)
    }
}

# ---------- emit ----------------------------------------------------------
$resultsDir = Join-Path $root "results"
New-Item -ItemType Directory -Force $resultsDir | Out-Null

$payload = [ordered]@{
    date = (Get-Date -Format "yyyy-MM-dd HH:mm")
    rustc = $rustcV
    cargo = $cargoV
    gcc = $gccV
    cpu = $cpu
    os = $os
    jestyr_branch = $branch
    jestyr_commit = $commit
    reps = $Reps
    timing = "interleaved min-of-$Reps, first round discarded"
    results = $results
}
$payload | ConvertTo-Json -Depth 5 | Out-File -Encoding utf8 (Join-Path $resultsDir "latest.json")

$md = @()
$md += "# Latest results"
$md += ""
$md += "- date: $($payload.date)"
$md += "- rustc: $rustcV / $cargoV"
$md += "- gcc: $gccV"
$md += "- jestyr: $branch@$commit"
$md += "- timing: interleaved min-of-$Reps runs, first round discarded"
$md += ""
$md += "| case | track | runtime (ms) | compile (ms) | binary (B) | LOC | outputs match |"
$md += "|---|---|---:|---:|---:|---:|---|"
foreach ($r in $results) {
    $md += "| $($r.case) | $($r.track) | $($r.runtime_ms) | $($r.compile_ms) | $($r.binary_bytes) | $($r.loc) | $($r.output_match) |"
}
$md -join "`n" | Out-File -Encoding utf8 (Join-Path $resultsDir "latest.md")

Write-Host "== wrote results\latest.json and results\latest.md"
