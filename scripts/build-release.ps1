# build-release.ps1 -- build the shippable rgy binaries and pack them.
#
# The Windows counterpart of scripts/build-release.sh.
#
#   pwsh -File scripts/build-release.ps1                # clippy gate + tests + the archive
#   pwsh -File scripts/build-release.ps1 -SkipTests     # iterate faster
#   pwsh -File scripts/build-release.ps1 -Version 0.1.0
#
# Unless -SkipTests, the build fails on any clippy warning
# (`cargo clippy --release --locked --workspace --all-targets -- -D warnings`)
# and on any failing test. The workspace stays warning-free by policy, and a
# release is not the moment to relax that.
#
# Every cargo call carries `--locked`: the lockfile is what this repository
# says it builds with, and a release that silently updates it is a release
# of something the repository does not describe.
#
# What it produces (.dist/):
#
#   rgy-v<version>-<platform>.zip   the binaries and the documents
#   SHA256SUMS                      the digests, in sha256sum(1) format
#
# The platform tag comes from the rustc host triple, so the same script on
# three machines produces three archives and never overwrites another.
#
# ONE DELIBERATE DIFFERENCE FROM THE LINUX TWIN: the archive format.
# Windows gets a .zip, because winget installs a zip and nothing else;
# Linux and macOS get a .tar.gz. The file list inside is the same on every
# platform, the log lines are the same, and the exit code is the same --
# see AGENTS.md on the twins.
#
# The version comes from the binary crates' manifests, and all four have to
# agree: a release whose binaries disagree about their version is a release
# nobody can describe.
param(
    [string]$Version = "",
    [switch]$SkipTests
)

$ErrorActionPreference = "Stop"
# A native command that writes to stderr -- and searchctl writes there
# whenever it rebuilds an index -- must not turn into a terminating error
# here: the exit code is what says whether the command worked.
if (Get-Variable -Name PSNativeCommandUseErrorActionPreference -ErrorAction SilentlyContinue) {
    $PSNativeCommandUseErrorActionPreference = $false
}
$root = Split-Path $PSScriptRoot -Parent
Set-Location $root

function Get-ManifestVersion([string]$path) {
    $m = Select-String -Path $path -Pattern '^version = "([^"]+)"' | Select-Object -First 1
    if ($m) { return $m.Matches.Groups[1].Value }
    return ""
}

# --- 1. the version ---------------------------------------------------------
#
# The workspace root is virtual: it has no version of its own. The crates
# that carry a version have to agree -- and `rgy-autostart` is one of them,
# because the release ships it: the archive carries searchctl, searchd and
# rgy-autostart, so a version drift in the autostart crate is a release
# carrying three binaries that do not agree about which release they are.
$manifests = @(
    "bins/searchctl/Cargo.toml", "bins/searchd/Cargo.toml",
    "bins/search-ui/Cargo.toml", "bins/zen-rs/Cargo.toml",
    "crates/autostart/Cargo.toml"
)
if (-not $Version) { $Version = Get-ManifestVersion "bins/searchctl/Cargo.toml" }
if (-not $Version) { throw "no version in bins/searchctl/Cargo.toml" }

foreach ($m in $manifests) {
    $v = Get-ManifestVersion $m
    if ($v -ne $Version) {
        # Plain lines on stderr, not `Write-Error`: the POSIX twin writes
        # the same two lines with `echo >&2`, and an error record would
        # print the script's path and line number on top of them.
        [Console]::Error.WriteLine("$m says version $v, but the release is $Version")
        [Console]::Error.WriteLine("the crates must agree about their version")
        exit 1
    }
}

# --- 2. the platform tag ----------------------------------------------------
#
# From the rustc host triple, because that is the thing that says what the
# binaries will run on: x86_64-pc-windows-msvc -> x86_64-windows,
# x86_64-unknown-linux-gnu -> x86_64-linux, aarch64-apple-darwin ->
# aarch64-apple-darwin.
$hostLine = (rustc -vV | Select-String '^host: ')
if (-not $hostLine) { throw "rustc did not report a host triple" }
$hostTriple = ($hostLine.ToString() -replace '^host: ', '').Trim()
if ($hostTriple -like '*-windows*') { $platform = ($hostTriple -split '-')[0] + "-windows" }
elseif ($hostTriple -like '*-linux*') { $platform = ($hostTriple -split '-')[0] + "-linux" }
elseif ($hostTriple -like '*-apple-darwin') { $platform = $hostTriple }
else { $platform = $hostTriple }

Write-Host "rgy release build: $Version ($platform)"

# --- 3. the build -----------------------------------------------------------
# The build comes before the tests, not after them. The e2e harness resolves
# the bins by path (target/release) and refuses to run against a binary
# older than its own sources -- which is the guard working, not a bug.
# `cargo test` builds the bins into deps/ but does not uplift them to
# target/release, so a test before a build would either exercise the
# previous release's binary or stop the release for a reason that looks
# like a test failure.
Write-Host "build: cargo build --release --locked --workspace"
cargo build --release --locked --workspace
if ($LASTEXITCODE -ne 0) { [Console]::Error.WriteLine("the build failed"); exit 1 }

# --- 4. the gate ------------------------------------------------------------
if (-not $SkipTests) {
    Write-Host "gate: clippy, then the test suite"
    cargo clippy --release --locked --workspace --all-targets -- -D warnings
    if ($LASTEXITCODE -ne 0) {
        [Console]::Error.WriteLine("clippy warnings found; the workspace must stay warning-free")
        exit 1
    }
    cargo test --release --locked --workspace
    if ($LASTEXITCODE -ne 0) { [Console]::Error.WriteLine("tests failed"); exit 1 }
} else {
    Write-Host "gate: skipped (--skip-tests)"
}

$exeDir = Join-Path $root "target\release"

# --- 5. the smoke -----------------------------------------------------------
#
# A binary that starts is not a binary that works. The smoke runs the two
# halves of the product against the repository's own sources: the name half
# (a fuzzy name query) and the content half (a literal search inside the
# files). Both must answer with a non-empty count.
#
# The index goes to a file under target\, not to the user's cache: a release
# build has no business writing into somebody's home directory.
$smokeIndexDir = Join-Path $root "target\release-smoke"
New-Item -ItemType Directory -Force -Path $smokeIndexDir | Out-Null
$smokeIndex = Join-Path $smokeIndexDir "rgy.idx"

function Invoke-Smoke([string]$label, [string]$query) {
    # The index line ("rebuilding", "reusing index") goes to stderr, and it
    # is not an error: the exit code is.
    $saved = $ErrorActionPreference
    $ErrorActionPreference = "Continue"
    $out = & (Join-Path $exeDir "searchctl.exe") --root (Join-Path $root "crates") `
        --index $smokeIndex --count $query 2> $null
    $code = $LASTEXITCODE
    $ErrorActionPreference = $saved
    if ($code -ne 0) {
        [Console]::Error.WriteLine("smoke: $label failed (exit $code)")
        exit 1
    }
    $text = ($out -join "`n")
    $m = [regex]::Match($text, '\d+')
    if (-not $m.Success -or [int]$m.Value -eq 0) {
        [Console]::Error.WriteLine("smoke: $label answered with no hits")
        exit 1
    }
    Write-Host "smoke: $label -> $($m.Value) hits"
}

Invoke-Smoke "name query over crates/" "walk ext:rs"
Invoke-Smoke "content query over crates/" "grep:Listing"

# --- 6. the archive ---------------------------------------------------------
#
# Flat layout: the binaries and the documents at the root of the archive, so
# that unzipping it next to a PATH directory is the whole install.
$out = Join-Path $root ".dist"
New-Item -ItemType Directory -Force -Path $out | Out-Null
$stage = Join-Path $root "target\release-stage"
if (Test-Path $stage) { Remove-Item $stage -Recurse -Force }
New-Item -ItemType Directory -Force -Path $stage | Out-Null

Copy-Item (Join-Path $exeDir "searchctl.exe") $stage
Copy-Item (Join-Path $exeDir "searchd.exe") $stage
Copy-Item (Join-Path $exeDir "rgy-autostart.exe") $stage
foreach ($doc in @("README.md", "MANUAL.md", "FORMATS.md", "COPYING", "NOTICE",
        "LICENSE-APACHE", "LICENSE-MIT", "LICENSE-MIT-FSEARCH")) {
    if (Test-Path (Join-Path $root $doc)) { Copy-Item (Join-Path $root $doc) $stage }
}

$archiveName = "rgy-v$Version-$platform.zip"
$archive = Join-Path $out $archiveName
Compress-Archive -Path (Join-Path $stage "*") -DestinationPath $archive -Force
Write-Host "packed: .dist/$archiveName"

# --- 7. the digest ----------------------------------------------------------
#
# SHA256SUMS holds every platform's digest, not just this one's: three
# machines build three archives into the same directory, and the release
# publishes them together.
$sums = Join-Path $out "SHA256SUMS"
$lines = @()
if (Test-Path $sums) { $lines = Get-Content $sums }
$lines = $lines | Where-Object { $_ -notmatch "  $([regex]::Escape($archiveName))$" }
$sha = (Get-FileHash $archive -Algorithm SHA256).Hash.ToLower()
$lines += "$sha  $archiveName"
# One line per asset, and the first one wins: a digests file that names the
# same archive twice is a file nobody can read -- the reader takes the
# digest and finds two of them.
$seen = @{}
$deduped = $lines | Where-Object {
    $key = ($_ -split '\s+', 2)[1]
    if ($seen.ContainsKey($key)) { $false } else { $seen[$key] = $true; $true }
}
$deduped | Sort-Object { ($_ -split '\s+', 2)[1] } | Set-Content $sums -Encoding utf8
Write-Host "checksum: .dist/SHA256SUMS ($sha  $archiveName)"

Write-Host ""
Write-Host "built $archiveName"
