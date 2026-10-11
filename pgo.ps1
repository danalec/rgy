# Release build optimized with a profile of rgy's own searches (PGO) —
# Windows driver. Same two-pass shape as pgo.sh (fsearch's pattern): train
# on the name bench, merge profiles, rebuild with -Cprofile-use.
# Needs: rustup component add llvm-tools
# Run: powershell -ExecutionPolicy Bypass -File pgo.ps1
$ErrorActionPreference = "Stop"

# Build from the workspace root no matter where the script is invoked
# from — `cargo -p engine` is meaningless outside it.
Set-Location $PSScriptRoot

$sysroot = (rustc --print sysroot).Trim()
$profdata = Get-ChildItem -Path (Join-Path $sysroot "lib\rustlib") -Recurse -Filter "llvm-profdata.exe" |
    Select-Object -First 1
if (-not $profdata) {
    Write-Error "pgo.ps1: needs llvm-profdata: rustup component add llvm-tools"
    exit 1
}

$tmp = Join-Path ([System.IO.Path]::GetTempPath()) ("rgy-pgo-" + [guid]::NewGuid().ToString("N"))
New-Item -ItemType Directory -Path $tmp | Out-Null
try {
    # Pass 1: instrumented build of the trainer only (name_bench exercises
    # the same Searcher path the daemon does).
    $env:RUSTFLAGS = "-Cprofile-generate=$tmp\prof"
    cargo build --release -p engine --example name_bench --target-dir "$tmp\target"
    # Native commands don't throw on failure even with Stop; every step
    # needs its exit code checked or a broken build looks like a missing exe.
    if ($LASTEXITCODE -ne 0) { throw "pgo.ps1: instrumented build failed" }
    & "$tmp\target\release\examples\name_bench.exe" 50000 | Out-Null
    if ($LASTEXITCODE -ne 0) { throw "pgo.ps1: trainer failed" }

    & $profdata.FullName merge -o "$tmp\rgy.profdata" "$tmp\prof"
    if ($LASTEXITCODE -ne 0) { throw "pgo.ps1: llvm-profdata merge failed" }

    # Pass 2: the real release build, consuming the merged profile.
    $env:RUSTFLAGS = "-Cprofile-use=$tmp\rgy.profdata"
    cargo build --release
    if ($LASTEXITCODE -ne 0) { throw "pgo.ps1: profile-use build failed" }
    Write-Host "pgo.ps1: release build in target\release uses the trained profile"
}
finally {
    $env:RUSTFLAGS = $null
    Remove-Item -Recurse -Force $tmp
}
