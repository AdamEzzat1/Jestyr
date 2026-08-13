# check_rejections.ps1 -- every rejection probe must FAIL to compile.
# A probe that compiles is a broken benchmark and exits this script 1.

$ErrorActionPreference = "Continue"
$root = Split-Path -Parent $PSScriptRoot
$repo = (Resolve-Path (Join-Path $root "..\..")).Path
$jestyrc = Join-Path $repo "target\release\jestyrc.exe"
$bad = 0

Write-Host "== rust rejection probes (rustc must refuse)"
foreach ($f in Get-ChildItem (Join-Path $root "rust") -Recurse -Filter "rejected.rs") {
    rustc --edition 2024 --emit=metadata --crate-name rejected_probe -o NUL $f.FullName 2>$null | Out-Null
    if ($LASTEXITCODE -eq 0) { Write-Host "  FAIL (compiled!): $($f.FullName)"; $bad++ }
    else { Write-Host "  ok (refused): $($f.Directory.Parent.Name)\$($f.Directory.Name)" }
}

Write-Host "== jestyr rejection probes (jestyrc must refuse)"
foreach ($f in Get-ChildItem (Join-Path $root "jestyr") -Filter "*_rejected.jtr") {
    & $jestyrc check $f.FullName 2>$null | Out-Null
    if ($LASTEXITCODE -eq 0) { Write-Host "  FAIL (accepted!): $($f.Name)"; $bad++ }
    else { Write-Host "  ok (refused): $($f.Name)" }
}

if ($bad -gt 0) { Write-Host "$bad probe(s) unexpectedly compiled"; exit 1 }
Write-Host "all rejection probes correctly refused"
