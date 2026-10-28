# build-release.ps1 -- build the shippable rgy binaries and pack them.
#
# The Windows counterpart of scripts/build-release.sh.
#
#   pwsh -File scripts/build-release.ps1                # clippy gate + tests + the archive
#   pwsh -File scripts/build-release.ps1 -SkipTests     # iterate faster
#   pwsh -File scripts/build-release.ps1 -Version 0.1.0
#   pwsh -File scripts/build-release.ps1 -Target aarch64-pc-windows-msvc
#   pwsh -File scripts/build-release.ps1 -Variant znver4
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
# --- the platforms ---------------------------------------------------------
#
# The release covers the same ground the upstream rgx covers, and it is
# built the same way: one portable binary for every CPU, plus the tuned
# ones for the CPUs that can run them. The upstream's release is the
# reference; what differs here is the name of the portable archive, which
# the installers already point at.
#
#   target                          asset                    flavour
#   x86_64-pc-windows-msvc          x86_64-windows.zip       portable
#   x86_64-pc-windows-msvc          avx2-windows.zip         AVX2
#   x86_64-pc-windows-msvc          znver4-windows.zip       AVX-512, Zen 4
#   aarch64-pc-windows-msvc          aarch64-windows.zip      portable
#   x86_64-unknown-linux-gnu        x86_64-linux.tar.gz      portable
#   x86_64-unknown-linux-gnu        avx2-linux.tar.gz        AVX2
#   x86_64-unknown-linux-gnu        znver4-linux.tar.gz      AVX-512, Zen 4
#   x86_64-unknown-linux-musl       x86_64-linux-musl.tar.gz portable
#   aarch64-unknown-linux-gnu       aarch64-linux.tar.gz     portable
#   aarch64-apple-darwin            aarch64-apple-darwin.tar.gz  portable
#   x86_64-apple-darwin             x86_64-apple-darwin.tar.gz   portable
#
# The three x86_64 portable names are the ones the installers ask for
# (install.sh wants x86_64-linux.tar.gz, install.ps1 wants
# x86_64-windows.zip) and the ones the published 0.1.0 carries, so they do
# not move: a rename here would point the installers at an archive that
# does not exist.
#
# The AVX2 and the Zen 4 builds are for the CPUs that can run them and
# nobody else: the Zen 4 one starts with an illegal instruction on a
# baseline x86_64 CPU, and the AVX2 one on a CPU older than Haswell.
#
# What it produces (.dist/):
#
#   rgy-v<version>-<asset>.zip   the binaries and the documents
#   SHA256SUMS                   the digests, in sha256sum(1) format
#
# ONE DELIBERATE DIFFERENCE FROM THE LINUX TWIN: the archive format.
# Windows gets a .zip, because winget installs a zip and nothing else;
# Linux and macOS get a .tar.gz. The file list inside is the same on every
# platform, the log lines are the same, and the exit code is the same.
#
# The version comes from the binary crates' manifests, and all four have to
# agree: a release whose binaries disagree about their version is a release
# nobody can describe.
param(
    [string]$Version = "",
    [switch]$SkipTests,
    [string]$Target = "",
    [string]$Variant = "portable"
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
# because the release ships it: the archive carries searchctl, searchd,
# rgy-autostart and rgxy, so a version drift in one of those crates is a
# release carrying binaries that do not agree about which release they are.
$manifests = @(
    "bins/searchctl/Cargo.toml", "bins/searchd/Cargo.toml",
    "bins/search-ui/Cargo.toml", "bins/zen-rs/Cargo.toml",
    "bins/rgxy/Cargo.toml",
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

# --- 2. the target and the flavour -----------------------------------------
#
# The host triple is the default, so the script keeps doing what it did
# before this option existed. The asset name is a property of the target
# and the flavour, never of the machine that happens to run the build: two
# machines building the same target produce the same archive name.
$hostLine = (rustc -vV | Select-String '^host: ')
if (-not $hostLine) { throw "rustc did not report a host triple" }
$hostTriple = ($hostLine.ToString() -replace '^host: ', '').Trim()
if (-not $Target) { $Target = $hostTriple }

$rustflags = ""
switch ($Variant) {
    "portable" { $rustflags = "" }
    "avx2"     { $rustflags = "-C target-cpu=x86-64-v3" }
    "znver4"   { $rustflags = "-C target-cpu=znver4" }
    default    {
        [Console]::Error.WriteLine("unknown variant: $Variant (portable, avx2, znver4)")
        exit 2
    }
}

# The asset name of a portable build, per target.
function Get-AssetName([string]$target) {
    switch ($target) {
        "x86_64-pc-windows-msvc"   { return "x86_64-windows" }
        "aarch64-pc-windows-msvc"   { return "aarch64-windows" }
        "x86_64-unknown-linux-gnu"  { return "x86_64-linux" }
        "x86_64-unknown-linux-musl" { return "x86_64-linux-musl" }
        "aarch64-unknown-linux-gnu" { return "aarch64-linux" }
        "aarch64-apple-darwin"      { return "aarch64-apple-darwin" }
        "x86_64-apple-darwin"       { return "x86_64-apple-darwin" }
        default {
            [Console]::Error.WriteLine("unknown target: $target")
            exit 2
        }
    }
}

$asset = Get-AssetName $Target

# The tuned builds replace the architecture in the name with the flavour:
# znver4-windows is a Windows x86_64 build for Zen 4, and avx2-linux is
# the Linux x86_64 one for the CPUs that have AVX2 and not AVX-512. Only
# the x86_64 targets have a tuned build; the others are tuned already.
if ($Variant -ne "portable") {
    if (-not $asset.StartsWith("x86_64-")) {
        [Console]::Error.WriteLine("the $Variant build needs an x86_64 target, and $Target is not one")
        exit 2
    }
    $asset = "$Variant-" + $asset.Substring("x86_64-".Length)
}

Write-Host "rgy release build: $Version ($asset)"
Write-Host "target: $Target"
Write-Host "variant: $Variant"

# --- 3. the build -----------------------------------------------------------
# The build comes before the tests, not after them. The e2e harness resolves
# the bins by path (target/release) and refuses to run against a binary
# older than its own sources -- which is the guard working, not a bug.
# `cargo test` builds the bins into deps/ but does not uplift them to
# target/release, so a test before a build would either exercise the
# previous release's binary or stop the release for a reason that looks
# like a test failure.
# The host's build keeps the --target off, so its binaries land in
# target/release the way they always have: the harness, the benchmark
# write-up and the mem-peak recipe all name that path. A build for
# another target lands in target\<triple>\release, and the archive is
# packed from there.
if ($Target -eq $hostTriple) {
    $buildArgs = @("build", "--release", "--locked", "--workspace")
    $exeDir = Join-Path $root "target\release"
} else {
    $buildArgs = @("build", "--release", "--locked", "--workspace", "--target", $Target)
    $exeDir = Join-Path $root "target\$Target\release"
}
Write-Host "build: cargo $($buildArgs -join ' ')"
if ($rustflags) { $env:RUSTFLAGS = $rustflags }
cargo @buildArgs
$buildExit = $LASTEXITCODE
if ($rustflags) { Remove-Item Env:RUSTFLAGS -ErrorAction SilentlyContinue }
if ($buildExit -ne 0) { [Console]::Error.WriteLine("the build failed"); exit 1 }

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

# --- 5. the smoke -----------------------------------------------------------
#
# A binary that starts is not a binary that works. The smoke runs the two
# halves of the product against the repository's own sources: the name half
# (a fuzzy name query) and the content half (a literal search inside the
# files). Both must answer with a non-empty count.
#
# A binary built for another target cannot run here, so the smoke is for
# the host's build only: a cross build is checked by the suite on the
# machine that owns that CPU.
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

if ($Target -eq $hostTriple) {
    Invoke-Smoke "name query over crates/" "walk ext:rs"
    Invoke-Smoke "content query over crates/" "grep:Listing"
} else {
    Write-Host "smoke: skipped (a binary for $Target does not run here)"
}

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
# The launcher: the window the user opens with two Control presses. It
# travels with the release because a launcher that is not installed is a
# feature nobody can use.
Copy-Item (Join-Path $exeDir "rgxy.exe") $stage
# The command the project is named after: the same binary under another
# name.
#
# It travels with the release, so every path that unpacks the archive --
# the one-line installer, winget, a zip by hand -- gives the user the name
# the project has. On unix the archive carries a link; on Windows a link is
# something only an administrator can make, and the archive carries the
# binary itself. It is a copy, and the copy is the price of the name:
# winget only aliases an executable.
Copy-Item (Join-Path $exeDir "searchctl.exe") (Join-Path $stage "rgy.exe")
foreach ($doc in @("README.md", "MANUAL.md", "FORMATS.md", "COPYING", "NOTICE",
        "LICENSE-APACHE", "LICENSE-MIT", "LICENSE-MIT-FSEARCH")) {
    if (Test-Path (Join-Path $root $doc)) { Copy-Item (Join-Path $root $doc) $stage }
}

$archiveName = "rgy-v$Version-$asset.zip"
$archive = Join-Path $out $archiveName
Compress-Archive -Path (Join-Path $stage "*") -DestinationPath $archive -Force
Write-Host "packed: .dist/$archiveName"

# --- 7. the digest ----------------------------------------------------------
#
# SHA256SUMS holds every platform's digest, not just this one's: three
# machines build three archives into the same directory, and the release
# publishes them together.
$sums = Join-Path $out "SHA256SUMS"
# The array is kept an array on purpose: PowerShell unrolls an array
# of one element into a scalar, and `$lines += "..."` on a scalar
# concatenates the entry onto the string instead of appending a line.
# The digests file is read by awk on the other platform, and one line
# with every digest in it is a file nobody can read.
$lines = @()
if (Test-Path $sums) { $lines = @(Get-Content $sums) }
$lines = @($lines | Where-Object { $_ -notmatch "  $([regex]::Escape($archiveName))$" })
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
