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
# Both spellings of every option work: --skip-tests is the twin's and
# -SkipTests is PowerShell's, and an option neither of them knows stops
# the script instead of being ignored.
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
#   x86_64-unknown-freebsd          x86_64-freebsd.tar.gz        portable
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
$ErrorActionPreference = "Stop"
# A native command that writes to stderr -- and searchctl writes there
# whenever it rebuilds an index -- must not turn into a terminating error
# here: the exit code is what says whether the command worked.
if (Get-Variable -Name PSNativeCommandUseErrorActionPreference -ErrorAction SilentlyContinue) {
    $PSNativeCommandUseErrorActionPreference = $false
}
# The options, read by the helper the two twins share: the long spelling
# and the PowerShell one are the same option, and an option neither of
# them knows is an error on both platforms -- not a silent "do the real
# thing", which in this directory means building an archive that a release
# then publishes.
. (Join-Path $PSScriptRoot "lib/options.ps1")
. (Join-Path $PSScriptRoot "lib/digest.ps1")
. (Join-Path $PSScriptRoot "lib/version.ps1")
$Version = ""
$SkipTests = $false
$Target = ""
$Variant = "portable"
Read-RgyOptions -Switches @{ "skiptests" = "SkipTests" } `
                 -Values   @{ "version" = "Version"; "target" = "Target"; "variant" = "Variant" } `
                 -Positionals @{} `
                 -Arguments $args

$root = Split-Path $PSScriptRoot -Parent
Set-Location $root

function Get-ManifestVersion([string]$path) {
    $m = Select-String -Path $path -Pattern '^version[ \t]*=[ \t]*"([^"]*)"' -CaseSensitive | Select-Object -First 1
    if ($m) { return $m.Matches.Groups[1].Value }
    return ""
}

# The line the version stands on, so a drift is found by its place and not
# only by its number.
function Get-ManifestVersionLine([string]$path) {
    $m = Select-String -Path $path -Pattern '^version[ \t]*=[ \t]*"[^"]*"' -CaseSensitive | Select-Object -First 1
    if ($m) { return $m.LineNumber }
    return 0
}

# The requirement of a dependency line, the way the unix twin reads it: the
# first `version = "..."` on the line, and the value ends at the first quote
# after it.
function Get-Requirement([string]$line) {
    if ($line -cmatch 'version[ \t]*=[ \t]*"([^"]*)"') { return $matches[1] }
    return ""
}

# The package a renamed dependency names, read the way the unix twin reads
# it: the first `package = "..."` on the line, and the value ends at the
# first quote after it.
#
# The key of a renamed dependency is the upstream's -- `rgx-core` -- and the
# package is this repository's -- `rgy-vendored-core` -- so the key alone
# does not say whose requirement the line carries. The line that says it is
# the one that names the package.
function Get-PackageName([string]$line) {
    if ($line -cmatch 'package[ \t]*=[ \t]*"([^"]*)"') { return $matches[1] }
    return ""
}

# --- 1. the version ---------------------------------------------------------
#
# The workspace root is virtual: it has no version of its own. The crates
# that carry a version have to agree -- and every one of them has to, not
# only the four the release ships: the archive carries searchctl, searchd,
# rgy-autostart and rgxy, so a version drift in one of those crates is a
# release carrying binaries that do not agree about which release they are.
#
# Every manifest this repository owns is checked, and not only the shipped
# ones. A crate left at the old number is a crate the resolver can take
# from the registry instead of from this tree, and `cargo build --locked`
# -- which is the release build itself -- then refuses the lockfile. The
# check names the manifest AND the line, because a bump that missed one
# line of twenty manifests is found by the reader, not by the script.
#
# The manifests are the workspace's own: the crates this repository
# publishes and the binaries it ships -- and the vendored trees
# (crates/rgx-*, crates/rar-codec) too, because this repository publishes
# them, under this repository's names, at this repository's version. The
# e2e harness is not published either, and keeps its 0.0.0.
#
# The version and the line it stands on are read the same way the unix
# twin reads them: the [package] table is the first table in every
# manifest of ours, so the first `version = "..."` line is the one. The
# quoted value ends at the first quote after it -- a greedy read would
# swallow the rest of the line, and the two twins would then read two
# different versions out of the same manifest.
$manifests = @(
    Get-ChildItem -Path "crates/*/Cargo.toml", "bins/*/Cargo.toml" |
        ForEach-Object { $_.FullName.Substring((Get-Location).Path.Length + 1).Replace('\', '/') } |
        Sort-Object
)
if (-not $Version) { $Version = Get-ManifestVersion "bins/searchctl/Cargo.toml" }
if (-not $Version) {
    # The unix twin's sentence and the unix twin's exit code.
    [Console]::Error.Write("no version in bins/searchctl/Cargo.toml`n")
    exit 1
}

# The page the tree carries says which release of the line this is.
#
# The manifests hold three numbers; the release may carry a fourth, the fix
# count of the line, and the fourth is in the page's name. Reading the manifest
# alone picked the three-number page and the archive step stopped on a page the
# tree does not carry -- on every row of the CI -- while the release's own page
# was there under the name the tag has.
$Version = Get-RgyReleaseVersion $root $Version
if (-not $Version) { exit 1 }

# The release's number and the crates' number. The registry takes a version
# of three numbers and nothing else, and a release line that will carry
# fixes for a while wants a fourth -- the fix count. With four numbers the
# release carries all of them and every manifest carries the first three,
# the same rule scripts/bump-version.py bumps by, so the agreement checks
# below read the three the manifests hold and the archive carries the four
# the tag is made of.
$CrateVersion = (($Version -split '\.')[0..2]) -join '.'

foreach ($m in $manifests) {
    $v = Get-ManifestVersion $m
    if ($v -ne $CrateVersion) {
        # Plain lines on stderr, not `Write-Error`: the POSIX twin writes
        # the same two lines with `echo >&2`, and an error record would
        # print the script's path and line number on top of them.
        $line = Get-ManifestVersionLine $m
        [Console]::Error.Write("$($m):$line says version $v, but the release is $Version`n")
        [Console]::Error.Write("the crates must agree about their version`n")
        exit 1
    }
}

# Every requirement on one of our crates has to ask for the version the
# release carries. A dependency that still asks for the old one breaks
# `cargo build --locked`, and that is the release build itself.
#
# A requirement is ours when the entry names one of our packages: the key
# is one of ours (`rgy-...`), or the entry names the package with
# `package = "rgy-vendored-..."` -- the same rule scripts/bump-version.py
# bumps by, so the check reads the lines the bump writes, and a requirement
# the bump would not have touched is a requirement the check does not read
# either. The key of a renamed dependency is the upstream's, so the key
# alone does not say whose requirement the line carries; the package does.
# The requirement of a crate that is not ours is not ours to write -- it
# names somebody else's version, and it stays where it was put.
#
# A dependency entry can span lines, and the requirement can be on one of
# them, so the entry is read to its closing brace: a requirement the check
# cannot see is a requirement it cannot vouch for.
foreach ($m in $manifests) {
    $dep = ""
    $at = 0
    foreach ($line in Get-Content $m) {
        $at++
        if (-not $dep) {
            if ($line -cmatch '^[ \t]*([A-Za-z0-9_-]+)[ \t]*=') {
                $key = $matches[1]
                $pkg = Get-PackageName $line
                if (-not ($key.StartsWith('rgy-') -or $pkg.StartsWith('rgy-vendored-'))) {
                    continue
                }
                $dep = $key
                if ($pkg) { $dep = $pkg }
                $req = Get-Requirement $line
                if ($req -ne "") {
                    if ($req -ne $CrateVersion -and $req -ne "^$CrateVersion") {
                        [Console]::Error.Write("$($m):$at asks for $dep $req where the release is $Version`n")
                        [Console]::Error.Write("the dependency lines have to be bumped with the manifests`n")
                        exit 1
                    }
                    $dep = ""
                } elseif ($line.Contains("{") -and -not $line.Contains("}")) {
                    # The entry spans lines: the requirement is on one of them.
                } else {
                    $dep = ""
                }
            }
            continue
        }
        $req = Get-Requirement $line
        if ($req -ne "") {
            if ($req -ne $CrateVersion -and $req -ne "^$CrateVersion") {
                [Console]::Error.Write("$($m):$at asks for $dep $req where the release is $Version`n")
                [Console]::Error.Write("the dependency lines have to be bumped with the manifests`n")
                exit 1
            }
            $dep = ""
        } elseif ($line.Contains("}")) {
            $dep = ""
        }
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
        [Console]::Error.Write("unknown variant: $Variant (portable, avx2, znver4)`n")
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
        "x86_64-unknown-freebsd"    { return "x86_64-freebsd" }
        default {
            [Console]::Error.Write("unknown target: $target`n")
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
        [Console]::Error.Write("the $Variant build needs an x86_64 target, and $Target is not one`n")
        exit 2
    }
    $asset = "$Variant-" + $asset.Substring("x86_64-".Length)
}

[Console]::Out.Write("rgy release build: $Version ($asset)`n")
[Console]::Out.Write("target: $Target`n")
[Console]::Out.Write("variant: $Variant`n")

# --- 3. the build -----------------------------------------------------------
# The build comes before the tests, not after them. The e2e harness resolves
# the bins by path (target/release) and refuses to run against a binary
# older than its own sources -- which is the guard working, not a bug.
# `cargo test` builds the bins into deps/ but does not uplift them to
# target/release, so a test before a build would either exercise the
# previous release's binary or stop the release for a reason that looks
# like a test failure.
# The host's PLAIN build keeps the --target off, so its binaries land in
# target/release the way they always have: the harness, the benchmark
# write-up and the mem-peak recipe all name that path. A build for
# another target lands in target\<triple>\release, and the archive is
# packed from there.
#
# The TUNED builds are the exception, and the reason is the build
# scripts. A build script is a program that runs on THIS machine, and
# cargo compiles it for the target it is building: a script compiled for
# Zen 4 dies with an illegal instruction on a runner that has no
# AVX-512, and the release stops with a message about mimalloc, which
# says nothing about the tree. Naming the target is what tells cargo
# that the build scripts are the host's and not the target's, and the
# flags then reach the target's own artifacts and not the scripts. A
# tuned build therefore lands in target\<triple>\release like any other
# build for a named target, and the plain build keeps target\release.
if (($Target -eq $hostTriple) -and (-not $rustflags)) {
    $buildArgs = @("build", "--release", "--locked", "--workspace")
    $exeDir = Join-Path $root "target\release"
} else {
    $buildArgs = @("build", "--release", "--locked", "--workspace", "--target", $Target)
    $exeDir = Join-Path $root "target\$Target\release"
}
[Console]::Out.Write("build: cargo $($buildArgs -join ' ')`n")
if ($rustflags) {
    # The per-target flags, and not RUSTFLAGS: the per-target ones are
    # the flags of the target's own artifacts, and RUSTFLAGS reaches
    # every compilation cargo does. The name is the triple, uppercased,
    # with the dashes turned into underscores: x86_64-pc-windows-msvc
    # is CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_RUSTFLAGS.
    $rustflagsVar = "CARGO_TARGET_" + ($Target.ToUpperInvariant() -replace '-', '_') + "_RUSTFLAGS"
    [Console]::Out.Write("flags: $rustflagsVar=$rustflags`n")
    [Environment]::SetEnvironmentVariable($rustflagsVar, $rustflags, "Process")
}
cargo @buildArgs
$buildExit = $LASTEXITCODE
if ($rustflags) { [Environment]::SetEnvironmentVariable($rustflagsVar, $null, "Process") }
if ($buildExit -ne 0) { [Console]::Error.Write("the build failed`n"); exit 1 }

# --- 4. the gate ------------------------------------------------------------
if (-not $SkipTests) {
    [Console]::Out.Write("gate: clippy, then the test suite`n")
    cargo clippy --release --locked --workspace --all-targets -- -D warnings
    if ($LASTEXITCODE -ne 0) {
        [Console]::Error.Write("clippy warnings found; the workspace must stay warning-free`n")
        exit 1
    }
    cargo test --release --locked --workspace
    if ($LASTEXITCODE -ne 0) { [Console]::Error.Write("tests failed`n"); exit 1 }
} else {
    [Console]::Out.Write("gate: skipped (--skip-tests)`n")
}

# --- 5. the smoke -----------------------------------------------------------
#
# A binary that starts is not a binary that works. The smoke runs the two
# halves of the product against the repository's own sources: the name half
# (a fuzzy name query) and the content half (a literal search inside the
# files). Both must answer with a non-empty count.
#
# A binary built for another target cannot run on the machine that built
# it, so the smoke is for the host's build only -- with two exceptions the
# CI covers: the musl build is static, so it starts on the runner that
# built it, and the Intel Mac's build is checked for the architecture it
# was built for, because there is no Intel Mac in the matrix.
#
# The index goes to a file under target\, not to the user's cache: a release
# build has no business writing into somebody's home directory.
$smokeIndexDir = Join-Path $root "target\release-smoke"
New-Item -ItemType Directory -Force -Path $smokeIndexDir | Out-Null
$smokeIndex = Join-Path $smokeIndexDir "rgy.idx"

function Invoke-Smoke([string]$label, [string]$query) {
    # The index line ("rebuilding", "reusing index") goes to stderr, and it
    # is not an error: the exit code is.
    #
    # The stderr goes to a file next to the index, and the file is printed
    # when the smoke fails: a smoke that says only "failed (exit 1)" says
    # nothing about why, and the reason is the whole of the report.
    $errFile = Join-Path $smokeIndexDir "stderr.txt"
    $saved = $ErrorActionPreference
    $ErrorActionPreference = "Continue"
    $out = & (Join-Path $exeDir "searchctl.exe") --root (Join-Path $root "crates") `
        --index $smokeIndex --count $query 2> $errFile
    $code = $LASTEXITCODE
    $ErrorActionPreference = $saved
    if ($code -ne 0) {
        [Console]::Error.Write("smoke: $label failed (exit $code)`n")
        if (Test-Path $errFile) {
            $err = [IO.File]::ReadAllText($errFile)
            if ($err) { [Console]::Error.Write($err) }
        }
        exit 1
    }
    $text = ($out -join "`n")
    # The count is the first number in the answer, and the unix twin reads
    # it the same way. `[0-9]` and not `\d`: the unicode class counts
    # digits the unix twin does not, and the two twins have to count the
    # same answer the same way.
    $m = [regex]::Match($text, '[0-9]+')
    if (-not $m.Success -or [int]$m.Value -eq 0) {
        [Console]::Error.Write("smoke: $label answered with no hits`n")
        exit 1
    }
    [Console]::Out.Write("smoke: $label -> $($m.Value) hits`n")
}

# The instructions the machine has.
#
# The twin of the unix script's check: the smoke runs the binary this
# machine just built, so it can only run where the CPU has the
# instructions the build asked for. The AVX2 build asks for AVX2 and the
# Zen 4 build asks for AVX-512, and a machine without them answers with
# SIGILL -- a red run that says nothing about the tree is worse than no
# run.
#
# .NET answers the question itself.
function Test-CpuHasAvx2 {
    try {
        return [System.Runtime.Intrinsics.X86.Avx2]::IsSupported
    } catch {
        return $false
    }
}

function Test-CpuHasAvx512 {
    try {
        return [System.Runtime.Intrinsics.X86.Avx512F]::IsSupported
    } catch {
        return $false
    }
}

if ($Target -eq $hostTriple) {
    if ($Variant -eq "znver4" -and -not (Test-CpuHasAvx512)) {
        [Console]::Out.Write("smoke: skipped (this machine has no AVX-512, and the Zen 4 build asks for it)`n")
    } elseif ($Variant -eq "avx2" -and -not (Test-CpuHasAvx2)) {
        [Console]::Out.Write("smoke: skipped (this machine has no AVX2, and the AVX2 build asks for it)`n")
    } else {
        Invoke-Smoke "name query over crates/" "walk ext:rs"
        Invoke-Smoke "content query over crates/" "grep:Listing"
    }
} else {
    [Console]::Out.Write("smoke: skipped (a binary for $Target does not run here)`n")
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
# The documents the release carries, in ONE list: the build copies them into
# the archive, and the build writes down their digests beside it. A list that
# lives in two places is a list that drifts.
$documents = @("README.md", "MANUAL.md", "FORMATS.md", "COPYING", "NOTICE",
    "LICENSE-APACHE", "LICENSE-MIT")
foreach ($doc in $documents) {
    if (Test-Path (Join-Path $root $doc)) { Copy-Item (Join-Path $root $doc) $stage }
}

# Every document the release promises is in the stage, or the build stops.
#
# The copy above is conditional -- a document the tree does not have is not
# packed -- and the archive's member list names the documents either way, so
# a tree without one produced an archive without it. A release archive without
# a licence file is a release whose licence story is broken, and it was
# announced as if it were whole.
foreach ($doc in $documents) {
    if (-not (Test-Path (Join-Path $stage $doc))) {
        [Console]::Error.WriteLine("the release carries $doc and this tree does not have it: the archive would not")
        [Console]::Error.WriteLine("  copy the document into the tree, or take it out of the release")
        exit 1
    }
}

# The picture the README shows travels with it.
#
# The archive carries the README, and the README points at the picture by
# a relative path, so an archive without the picture has a README with a
# hole in it: the link is broken exactly where the reader is offline.
# The path inside the archive is the one the README uses, so the link
# works where the README is.
# The pictures and the clips the README points at.
#
# The archive carries the README, and the README points at these by a
# relative path, so an archive without them has a README with holes in it:
# the links are broken exactly where the reader is offline. The path inside
# the archive is the one the README uses, so the links work where the README
# is.
#
# ONE list, and three parts of the build read it: the copy below, the
# archive's member list, and the digests the publish step compares with the
# tree. A list that lives in three places is a list that drifts.
#
# The paths are spelled the way the unix twin spells them, with the forward
# slash: a message that names a media file has to name it the same way on
# both platforms, or the two twins answer the same question two ways.
$media = @(
    "docs/screenshots/rgxy.png",
    "docs/screenshots/gui-filters.png",
    "docs/videos/gui-filters.mp4",
    "docs/videos/term-tui.mp4"
)
foreach ($mediaFile in $media) {
    $mediaPath = Join-Path $root $mediaFile
    if (-not (Test-Path $mediaPath)) {
        [Console]::Error.WriteLine("the release carries $mediaFile and this tree does not have it: the archive would not")
        [Console]::Error.WriteLine("  the README points at it, and an archive without it is a README with a hole in it")
        exit 1
    }
    $inStage = Join-Path $stage $mediaFile
    New-Item -ItemType Directory -Force -Path (Split-Path $inStage -Parent) | Out-Null
    Copy-Item $mediaPath $inStage
}

# The page of the release the README calls the changelog.
#
# The README points at it by a relative path, and the name of that page
# carries the version -- so the page that travels with the archive is the
# page of the release being built, and not the page of an older one. A tree
# without it has a README with a hole in it.
$releasePage = "docs/release-$Version.md"
$releasePagePath = Join-Path $root $releasePage
if (-not (Test-Path $releasePagePath -PathType Leaf)) {
    [Console]::Error.WriteLine("the release carries $releasePage and this tree does not have it: the archive would not")
    [Console]::Error.WriteLine("  the README points at it, and an archive without it is a README with a hole in it")
    exit 1
}
$releasePageInStage = Join-Path $stage $releasePage
New-Item -ItemType Directory -Force -Path (Split-Path $releasePageInStage -Parent) | Out-Null
Copy-Item $releasePagePath $releasePageInStage

# The benchmark folder the README points at.
#
# The README links the folder itself and not one file of it, so the folder
# has to be inside the archive -- and the rows the README quotes live in it.
$benchmarkDir = "benchmark"
$benchmarkPath = Join-Path $root $benchmarkDir
if (-not (Test-Path $benchmarkPath -PathType Container)) {
    [Console]::Error.WriteLine("the release carries $benchmarkDir/ and this tree does not have it: the archive would not")
    [Console]::Error.WriteLine("  the README points at it, and an archive without it is a README with a hole in it")
    exit 1
}
$benchmarkInStage = Join-Path $stage $benchmarkDir
New-Item -ItemType Directory -Force -Path $benchmarkInStage | Out-Null
Copy-Item -Path (Join-Path $benchmarkPath "*") -Destination $benchmarkInStage -Recurse -Force

# Every relative link in every document that travels resolves inside the stage.
#
# The README and the manual are what the reader has, and they point at their
# neighbours by relative paths: the picture, the manual, the benchmark
# folder, the page of the release. A link that does not resolve inside the
# archive is a link that works on the forge and nowhere else -- and the
# reader of the archive is the one who cannot follow it.
#
# Every document in the stage is checked, and not only the README and the
# manual: the benchmark folder travels too, and its own documents point at
# each other. The order is the ordinal one, so the two twins read the same
# document first and say the same thing about it.
#
# The links are read the way the reader reads them: the text between the
# parentheses. The absolute ones (http:, https:, mailto:) are the reader's
# own business, and the pure anchors (#...) stay inside the document.
$mdFiles = [System.Collections.Generic.List[string]]::new()
foreach ($mdFile in (Get-ChildItem -Path $stage -Recurse -File -Filter '*.md')) {
    $mdRelative = $mdFile.FullName.Substring($stage.Length + 1) -replace '\\', '/'
    $at = 0
    while ($at -lt $mdFiles.Count -and
           [System.StringComparer]::Ordinal.Compare($mdFiles[$at], $mdRelative) -le 0) {
        $at++
    }
    $mdFiles.Insert($at, $mdRelative)
}
foreach ($docPath in $mdFiles) {
    # A relative link is resolved against the folder of the document that
    # carries it, and not against the root of the archive: a link in
    # benchmark/README.md to baseline-names.md is the file beside it.
    $docDir = $docPath.Substring(0, [Math]::Max(0, $docPath.LastIndexOf('/')))
    $docFull = Join-Path $stage ($docPath -replace '/', '\')
    foreach ($line in [System.IO.File]::ReadAllLines($docFull)) {
        foreach ($hit in [regex]::Matches($line, '\]\(([^)]*)\)')) {
            $link = $hit.Groups[1].Value
            if (-not $link) { continue }
            if ($link.StartsWith("#")) { continue }
            if ($link.StartsWith("//")) { continue }
            # A scheme means a URL and not a path: http:, https:, mailto:.
            if ($link -cmatch '^[A-Za-z][A-Za-z0-9+.-]*:') { continue }
            # The path: what is before the anchor, and what is before the title.
            $path = $link
            $at = $path.IndexOf("#")
            if ($at -ge 0) { $path = $path.Substring(0, $at) }
            if ($path.StartsWith("<")) {
                $path = $path.Substring(1)
                $at = $path.IndexOf(">")
                if ($at -ge 0) { $path = $path.Substring(0, $at) }
            } else {
                $at = $path.IndexOf(" ")
                if ($at -ge 0) { $path = $path.Substring(0, $at) }
            }
            # The trailing slash is how the README spells the folder, and not
            # part of the folder's name: the folder is looked up by its name.
            $check = $path -replace '/+$', ''
            if (-not $check) { continue }
            if ($docDir) { $target = "$docDir/$check" } else { $target = $check }
            $inStage = Join-Path $stage ($target -replace '/', '\')
            if (-not (Test-Path $inStage)) {
                [Console]::Error.WriteLine("the $docPath points at $link and the archive does not carry it: the link is broken exactly where the reader is offline")
                exit 1
            }
        }
    }
}

$archiveName = "rgy-v$Version-$asset.zip"
$archive = Join-Path $out $archiveName

# The archive's member list: the paths the archive carries.
#
# The unix twin hands this list to tar, which packs exactly these paths.
# The Windows twin packs the whole stage -- so the list is checked here
# instead: a member the stage does not have is a member the archive does
# not have, and the archive would say it carries it.
#
# The names are the unix twin's, with the two the platform spells
# differently: the .exe suffix, and the rgy link that Windows cannot make
# without an administrator, so the archive carries the binary itself.
$files = @("searchctl.exe", "searchd.exe", "rgy-autostart.exe", "rgxy.exe", "rgy.exe")
$files += $documents
$files += $media
$files += @("benchmark", $releasePage)
foreach ($member in $files) {
    $inStage = Join-Path $stage ($member -replace '/', '\')
    if (-not (Test-Path $inStage)) {
        [Console]::Error.WriteLine("the archive carries $member and the stage does not have it: the archive would not")
        [Console]::Error.WriteLine("  carry what this build promised")
        exit 1
    }
}

try {
    Compress-Archive -Path (Join-Path $stage "*") -DestinationPath $archive -Force -ErrorAction Stop
} catch {
    [Console]::Error.WriteLine("the archive was not packed: $($_.Exception.Message)")
    [Console]::Error.WriteLine("  a release without its archive is no release")
    exit 1
}
[Console]::Out.Write("packed: .dist/$archiveName`n")

# The digests of the documents that went into the archive.
#
# The archive's own digest proves the archive is the one this build made. It
# says nothing about what is INSIDE it: a release built before the README's
# last edit carries the older README, and its digest matches perfectly.
#
# So the build writes down what it packed -- the digest of every document it
# copied from the tree -- beside the archive, and the publish step reads it
# back and compares each digest with the tree's. The digests are of the
# tree's files, and the tree's files are the same on every machine: the text
# files are LF (the .gitattributes say so) and the picture is binary. So the
# file this build wrote here is the file the other platform's build wrote
# there.
$docsDir = Join-Path $out "documents"
New-Item -ItemType Directory -Force -Path $docsDir | Out-Null
$docsFile = Join-Path $docsDir "$archiveName.sha256"
$docsRows = @()
foreach ($doc in $documents) {
    $docPath = Join-Path $root $doc
    if (Test-Path $docPath) { $docsRows += "$(Get-RgySha256 $docPath)  $doc" }
}
foreach ($mediaFile in $media) {
    $mediaPath = Join-Path $root $mediaFile
    $docsRows += "$(Get-RgySha256 $mediaPath)  $mediaFile"
}
# The page of the release and the benchmark folder travel with the archive,
# so the record names them too: a record that lists only the documents is a
# record of a part of the archive, and the publish step compares the record
# with the tree -- so a stale page, or a benchmark row of an older run,
# would pass a check that is about the whole.
$docsRows += "$(Get-RgySha256 $releasePagePath)  $releasePage"
foreach ($benchFile in (Get-ChildItem -Path $benchmarkPath -Recurse -File)) {
    $benchRelative = $benchFile.FullName.Substring($root.Length + 1) -replace '\\', '/'
    $docsRows += "$(Get-RgySha256 $benchFile.FullName)  $benchRelative"
}
# The ordinal order, so the two machines write the same bytes.
$docsOrdered = [System.Collections.Generic.List[string]]::new()
foreach ($row in $docsRows) {
    $rowName = ($row -split '\s+', 2)[1]
    $at = 0
    while ($at -lt $docsOrdered.Count -and
           [System.StringComparer]::Ordinal.Compare((($docsOrdered[$at] -split '\s+', 2)[1]), $rowName) -le 0) {
        $at++
    }
    $docsOrdered.Insert($at, $row)
}
$docsText = (($docsOrdered) -join "`n") + "`n"
[System.IO.File]::WriteAllText($docsFile, $docsText, [System.Text.UTF8Encoding]::new($false))
[Console]::Out.Write("documents: .dist/documents/$archiveName.sha256 ($($docsOrdered.Count) files)`n")

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
# A file written by an older build carries a carriage return at the end of
# each line, and a name with one is not the name: the line is trimmed on the
# way in, so a file from the other platform reads the same as one from here.
if (Test-Path $sums) { $lines = @(Get-Content $sums | ForEach-Object { $_.TrimEnd("`r") }) }
# The digests file belongs to ONE release: the rows of the archives this
# release carries, and nothing else. A row left over from an older build in
# the same .dist/ names an archive the release does not publish, and
# `sha256sum -c` then fails on a file the user never got.
$lines = @($lines | Where-Object { $_ -match "  rgy-v$([regex]::Escape($Version))-" })
$lines = @($lines | Where-Object { $_ -notmatch "  $([regex]::Escape($archiveName))$" })
$sha = (Get-RgySha256 $archive)
$lines += "$sha  $archiveName"
# One line per asset, and the first one wins: a digests file that names the
# same archive twice is a file nobody can read -- the reader takes the
# digest and finds two of them.
$seen = @{}
$deduped = $lines | Where-Object {
    $key = ($_ -split '\s+', 2)[1]
    if ($seen.ContainsKey($key)) { $false } else { $seen[$key] = $true; $true }
}
# LF and no BOM: the file is read by awk on the other platform, and by
# `sha256sum -c` everywhere. A carriage return at the end of a line is part
# of the name awk reads, and the digest never matches.
# The order is the ordinal one, and not the locale's: the file is read by
# `sha256sum -c` everywhere and compared byte for byte between the
# platforms, so the two scripts have to write the same lines in the same
# order whatever the machine's language says.
$ordered = [System.Collections.Generic.List[string]]::new()
foreach ($row in $deduped) {
    $rowName = ($row -split '\s+', 2)[1]
    $at = 0
    while ($at -lt $ordered.Count -and
           [System.StringComparer]::Ordinal.Compare((($ordered[$at] -split '\s+', 2)[1]), $rowName) -le 0) {
        $at++
    }
    $ordered.Insert($at, $row)
}
$text = (($ordered) -join "`n") + "`n"
[System.IO.File]::WriteAllText($sums, $text, [System.Text.UTF8Encoding]::new($false))
[Console]::Out.Write("checksum: .dist/SHA256SUMS ($sha  $archiveName)`n")

[Console]::Out.Write("`n")
[Console]::Out.Write("built $archiveName`n")

# The exit code, said out loud. A caller that invoked this script with the
# call operator reads `$LASTEXITCODE`, and a script that ends without
# saying anything leaves the last native command's value there -- which
# can be non-zero even though the build succeeded. The build script says
# its own, so the caller reads the truth.
exit 0
