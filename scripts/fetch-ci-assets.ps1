#
# fetch-ci-assets.ps1 -- bring the archives a CI run built into .dist/.
#
#   ./scripts/fetch-ci-assets.ps1 <run-id>
#   ./scripts/fetch-ci-assets.ps1 --dir <directory-with-downloaded-artifacts>
#
# The release carries every archive the CI builds -- eleven platforms and the
# FreeBSD one, each on the runner that has it -- and the release is published
# from one machine. This is the step between the two: the CI keeps each row's
# archive (and the digests of the documents inside it) as an artifact, and
# this brings a run's artifacts into .dist/, where the publish step reads
# them. The archives of the version in .dist/ are replaced, not merged: a
# local build of the same version beside the run's would put two digests of
# "the" archive in one release, and the publish would pin whichever it read
# last.
#
# `--dir` is the same step without the network: a directory that holds what
# `gh run download` writes (one subdirectory per artifact), so a run whose
# artifacts were fetched by hand can be assembled the same way.
#
# The run has to have finished green: a release assembled from a run that
# failed somewhere is a release whose archives were built by the rows that
# worked, and nobody knows which rows those are.
#
# What lands in .dist/:
#
#   rgy-v<version>-<asset>.<zip|tar.gz>          the archive
#   documents/rgy-v<version>-<asset>.*.sha256    the digests of its documents
#   SHA256SUMS                                   over every archive there
#
# The twin is fetch-ci-assets.sh, and the two say the same words on the same
# input.
$ErrorActionPreference = "Stop"
# A native command that writes to stderr -- gh does -- must not turn into a
# terminating error here: the exit code is what says whether it worked.
if (Get-Variable -Name PSNativeCommandUseErrorActionPreference -ErrorAction SilentlyContinue) {
    $PSNativeCommandUseErrorActionPreference = $false
}

$root = Split-Path $PSScriptRoot -Parent
Set-Location $root

. (Join-Path $PSScriptRoot "lib/options.ps1")
. (Join-Path $PSScriptRoot "lib/digest.ps1")

$Run = ""
$Dir = ""
$Repo = "danalec/rgy"

# The options, read by the helper the twins share: an option neither of them
# knows is an error -- the same words and the same exit code on both.
Read-RgyOptions -Switches @{} -Values @{ "dir" = "Dir"; "repo" = "Repo" } -Positionals @{ "run" = "Run" } -Arguments $args

if (-not $Run -and -not $Dir) {
    [Console]::Error.Write("usage: fetch-ci-assets.sh <run-id> | --dir <directory>`n")
    exit 1
}
if ($Run -and $Dir) {
    [Console]::Error.Write("--dir and a run id are two different sources: name one of them`n")
    exit 1
}

# The release's version, out of the tree: the page's name, by the rule the
# build and publish scripts read the same way.
$Version = & (Join-Path $PSScriptRoot "lib/version.ps1")
if (-not $Version) { exit 1 }

$Work = ""
if ($Dir) {
    if (-not (Test-Path $Dir -PathType Container)) {
        [Console]::Error.Write("no such directory: $Dir`n")
        exit 1
    }
    $Work = (Resolve-Path $Dir).Path
    [Console]::Out.Write(">> rgy-v${Version}: the artifacts in ${Dir}`n")
} else {
    if (-not (Get-Command gh -ErrorAction SilentlyContinue)) {
        [Console]::Error.Write("gh not found: it is what reads a run's artifacts`n")
        exit 1
    }
    $status = & gh run view $Run --repo $Repo --json status,conclusion 2>$null
    if ($LASTEXITCODE -ne 0 -or -not $status) {
        [Console]::Error.Write("no run ${Run} in ${Repo}`n")
        exit 1
    }
    $parsed = $status | ConvertFrom-Json
    [Console]::Out.Write(">> rgy-v${Version}: the artifacts of run ${Run} ($($parsed.status)/$($parsed.conclusion))`n")
    if ($parsed.status -ne "completed" -or $parsed.conclusion -ne "success") {
        [Console]::Error.Write("the run did not finish green: a release is assembled from a run that did`n")
        exit 1
    }
    $Work = Join-Path ([IO.Path]::GetTempPath()) ("rgy-ci-assets-" + [guid]::NewGuid().ToString("N"))
    & gh run download $Run --repo $Repo --dir $Work
    if ($LASTEXITCODE -ne 0) {
        [Console]::Error.Write("the artifacts of run ${Run} could not be downloaded`n")
        exit 1
    }
}

$dist = Join-Path $root ".dist"
$docs = Join-Path $dist "documents"
New-Item -ItemType Directory -Force -Path $docs | Out-Null
# The archives of this version that .dist/ already holds are the previous
# source of this release, and a release has one source: they go.
[Console]::Out.Write(">> clearing .dist/ of the rgy-v${Version} archives`n")
Remove-Item (Join-Path $dist "rgy-v$Version-*") -Force -ErrorAction SilentlyContinue
Remove-Item (Join-Path $docs "rgy-v$Version-*") -Force -ErrorAction SilentlyContinue

# One subdirectory per artifact, and the tree inside it is the row's .dist/:
# the archive at the root and the document digests under documents/.
foreach ($art in (Get-ChildItem $Work -Directory)) {
    foreach ($file in (Get-ChildItem $art.FullName -File)) {
        if ($file.Name -like "rgy-v$Version-*" -and ($file.Name -like "*.zip" -or $file.Name -like "*.tar.gz")) {
            Move-Item -Force $file.FullName $dist
        }
    }
    $artDocs = Join-Path $art.FullName "documents"
    if (Test-Path $artDocs -PathType Container) {
        foreach ($sidecar in (Get-ChildItem $artDocs -File)) {
            if ($sidecar.Name -like "rgy-v$Version-*.sha256") {
                Move-Item -Force $sidecar.FullName $docs
            }
        }
    }
}

$names = New-Object System.Collections.Generic.List[string]
foreach ($archive in (Get-ChildItem $dist -File)) {
    if ($archive.Name -notlike "rgy-v$Version-*") { continue }
    $names.Add($archive.Name)
}
if ($names.Count -eq 0) {
    [Console]::Error.Write("no rgy-v${Version} archives were found: the run built another version, or none`n")
    exit 1
}
# The order is the ordinal one: the listing is read by a person and compared
# with the twin's, and a directory's enumeration order is the platform's
# habit and not the release's.
$sorted = $names.ToArray()
[System.Array]::Sort($sorted, [System.StringComparer]::Ordinal)
foreach ($name in $sorted) {
    $sidecar = Join-Path $docs "$name.sha256"
    if (-not (Test-Path $sidecar -PathType Leaf)) {
        [Console]::Error.Write("$name has no document digests: the run that built it did not keep them`n")
        exit 1
    }
    [Console]::Out.Write("   $name`n")
}

# The digests, over every archive of this version that is here now: the
# entries the separate builds wrote are one archive each, and a release's
# file names all of them.
#
# The order is the ordinal one, and not the culture's: the file is read by
# `sha256sum -c` everywhere and compared byte for byte between the platforms.
$lines = foreach ($name in $sorted) {
    "$(Get-RgySha256 (Join-Path $dist $name))  $name"
}
[IO.File]::WriteAllText((Join-Path $dist "SHA256SUMS"), (($lines -join "`n") + "`n"), (New-Object Text.UTF8Encoding $false))

[Console]::Out.Write(">> .dist/ holds $($names.Count) archives of rgy-v${Version} and the SHA256SUMS over them`n")
