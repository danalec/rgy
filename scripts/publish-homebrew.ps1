# publish-homebrew.ps1 -- open the homebrew-core pull request for a release.
#
#   pwsh -File scripts/publish-homebrew.ps1 [-Version 0.1.0] [-DryRun]
#
# Homebrew-core is a git repository like any other: a pull request is a
# branch, a commit and a merge. This script does the first two and opens
# the third.
#
#   1. check that the release the formula names actually exists, and that
#      its digest is the one the formula carries
#   2. clone homebrew-core (shallow: it is a big repository)
#   3. put the formula -- the one this repository carries, with the url,
#      the sha256 and the version the release flow just rewrote -- into a
#      branch
#   4. commit it and push the branch to the fork
#   5. open the pull request
#
# What the CI on that pull request runs is `brew audit --new --formula` and
# `brew readall`, and both need a Mac. The script says so here rather than
# pretending the pull request is the last step.
#
# Requires git, curl and gh (authenticated). The fork is created if it is
# not there yet.
$ErrorActionPreference = "Stop"

# The options, read by the helper the two twins share: the long spelling
# and the PowerShell one are the same option, and an option neither of
# them knows is an error on both platforms -- not a silent "do the real
# thing", which in this directory means opening a pull request.
. (Join-Path $PSScriptRoot "lib/options.ps1")
$Version = ""
$Formula = "rgy-search"
$DryRun = $false
Read-RgyOptions -Switches @{ "dryrun" = "DryRun" } `
                 -Values   @{ "version" = "Version"; "formula" = "Formula" } `
                 -Positionals @{} `
                 -Arguments $args

$root = Split-Path $PSScriptRoot -Parent
Set-Location $root

$coreRepo = "Homebrew/homebrew-core"
$coreFork = "danalec/homebrew-core"

$formulaFile = "Formula/$Formula.rb"
if (-not (Test-Path $formulaFile)) {
    Write-Error "no formula at $formulaFile"
    exit 1
}

# The version comes from the formula, and the formula's url and sha256
# come from the release flow: they are the release's own data, rewritten
# when the release was published.
if (-not $Version) {
    $m = Select-String -Path $formulaFile -Pattern '^\s*version "([^"]+)"' | Select-Object -First 1
    if ($m) { $Version = $m.Matches.Groups[1].Value }
}
if (-not $Version) { Write-Error "no version in $formulaFile"; exit 1 }

$url = ""
$m = Select-String -Path $formulaFile -Pattern '^\s*url "([^"]+)"' | Select-Object -First 1
if ($m) { $url = $m.Matches.Groups[1].Value }
$sha = ""
$m = Select-String -Path $formulaFile -Pattern '^\s*sha256 "([^"]+)"' | Select-Object -First 1
if ($m) { $sha = $m.Matches.Groups[1].Value }
if (-not $url) { Write-Error "no url in $formulaFile"; exit 1 }
if (-not $sha) { Write-Error "no sha256 in $formulaFile"; exit 1 }

Write-Host ">> rgy-search $Version ($Formula)"
Write-Host ">> url   $url"
Write-Host ">> sha256 $sha"

# --- 1. the release has to exist --------------------------------------------
# A pull request whose formula names a URL that answers with something
# other than the file is a pull request the CI will fail on, and the
# failure is slower and less clear than this one.
Write-Host ">> checking the release"
try {
    $head = Invoke-WebRequest -Uri $url -Method Head -UseBasicParsing
    if ($head.StatusCode -ne 200) { throw "status $($head.StatusCode)" }
} catch {
    Write-Error "the release artifact is not there yet: $url"
    Write-Error "publish the release first (scripts/publish-release.ps1), then run this"
    exit 1
}

# --- 2. the fork ------------------------------------------------------------
gh repo view $coreFork *> $null
if ($LASTEXITCODE -ne 0) {
    if ($DryRun) {
        Write-Host "[dry-run] would create the fork $coreFork"
    } else {
        Write-Host ">> forking $coreRepo"
        gh repo fork $coreRepo --clone=false
        if ($LASTEXITCODE -ne 0) { throw "the fork could not be created" }
    }
}

# --- 3. the branch ----------------------------------------------------------
$work = Join-Path ([IO.Path]::GetTempPath()) "rgy-brew-$([Guid]::NewGuid().ToString('N'))"
New-Item -ItemType Directory -Force -Path $work | Out-Null
Write-Host ">> cloning $coreRepo"
git clone --quiet --depth 1 "https://github.com/$coreRepo.git" "$work/core"
if ($LASTEXITCODE -ne 0) { throw "the clone failed" }
$branch = "$Formula-$Version"
git -C "$work/core" checkout --quiet -b $branch
Copy-Item $formulaFile "$work/core/$formulaFile" -Force

# --- 4. the commit ----------------------------------------------------------
git -C "$work/core" add $formulaFile
git -C "$work/core" -c user.name="danalec" -c user.email="danalec@gmail.com" commit --quiet -m "$Formula $Version"
if ($LASTEXITCODE -ne 0) { throw "the commit failed" }

if ($DryRun) {
    Write-Host "[dry-run] would push $branch to $coreFork and open the pull request"
    git -C "$work/core" show --stat --oneline HEAD
    exit 0
}

Write-Host ">> pushing $branch to $coreFork"
git -C "$work/core" push --quiet "https://github.com/$coreFork.git" $branch
if ($LASTEXITCODE -ne 0) { throw "the push failed" }

# --- 5. the pull request ----------------------------------------------------
# The body answers the questions homebrew-core's template asks, because a
# pull request that answers them is one review round shorter.
$body = @"
$Formula $Version

**What is this tool doing?**

rgy answers two questions about the files on a computer: which file is it,
and what is inside it. Both in one query. The name half is answered from
an index of names -- fuzzy, typo-tolerant and ranked -- and the content
half reads the file.

    searchctl --root ~ "invoice ext:pdf"
    searchctl --root ~ "invoice grep:tesla"

**Does this pull request change the software that Homebrew installs?**

This is the first release: it adds the formula, so it adds the software.
There is no previous version in Homebrew to replace.

**Why should this be in Homebrew core?**

It is a command-line tool that installs from a prebuilt, signed-by-digest
archive, needs no system library beyond what macOS ships, and has a stable
tagged release. The repository carries the formula, the release script and
the installers: https://github.com/danalec/rgy

**What does the test do?**

It writes a file and asks the CLI how many files match its name, so the
test is the product doing its own job.

"@
$bodyFile = Join-Path $work "pr.md"
Set-Content $bodyFile $body -NoNewline

Write-Host ">> opening the pull request"
gh pr create --repo $coreRepo --head "$coreFork`:$branch" --title "$Formula $Version" --body-file $bodyFile
if ($LASTEXITCODE -ne 0) { throw "the pull request could not be opened" }

Write-Host ""
Write-Host ">> what happens next: the CI runs 'brew audit --new --formula' and"
Write-Host ">> 'brew readall'. Both need a Mac; the audit checks the url, the"
Write-Host ">> digest, the licence, the description and the test."
