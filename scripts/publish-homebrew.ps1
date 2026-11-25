# publish-homebrew.ps1 -- publish the formula to the personal Homebrew tap.
#
#   pwsh -File scripts/publish-homebrew.ps1 [-Version 0.1.0] [-DryRun]
#
# The tap is danalec/homebrew-rgy, NOT Homebrew/homebrew-core. The latter is
# Homebrew's own source repository; a formula submitted there is a pull
# request to somebody else's project, with their review, their CI and their
# template. The rgy formula is the rgy project's own, so it lives in a tap
# the project owns and is pushed straight to its main branch.
#
#   1. check that the release the formula names actually exists, and that
#      its digest is the one the formula carries
#   2. clone the tap (shallow: it is one formula and a README)
#   3. put the formula -- the one this repository carries, with the url,
#      the sha256 and the version the release flow just rewrote -- into it
#   4. commit and push to main
#
# No fork, no pull request, no macOS CI of somebody else's repository. The
# formula is verified against the published release before it is pushed.
$ErrorActionPreference = "Stop"

# The options, read by the helper the two twins share: the long spelling
# and the PowerShell one are the same option, and an option neither of
# them knows is an error on both platforms -- not a silent "do the real
# thing", which in this directory means pushing a release to a tap.
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

$tapRepo = "danalec/homebrew-rgy"

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
# A formula that names a URL that answers with something other than the
# file is a formula that installs nothing. The check is here, before the
# tap is touched.
Write-Host ">> checking the release"
try {
    $head = Invoke-WebRequest -Uri $url -Method Head -UseBasicParsing
    if ($head.StatusCode -ne 200) { throw "status $($head.StatusCode)" }
} catch {
    Write-Error "the release artifact is not there yet: $url"
    Write-Error "publish the release first (scripts/publish-release.ps1), then run this"
    exit 1
}

# --- 2. the tap -------------------------------------------------------------
$work = Join-Path ([IO.Path]::GetTempPath()) "rgy-brew-$([Guid]::NewGuid().ToString('N'))"
New-Item -ItemType Directory -Force -Path $work | Out-Null
Write-Host ">> cloning $tapRepo"
git clone --quiet --depth 1 "https://github.com/$tapRepo.git" "$work/tap"
if ($LASTEXITCODE -ne 0) { throw "the clone failed" }
Copy-Item $formulaFile "$work/tap/$formulaFile" -Force

# --- 3. the commit ----------------------------------------------------------
git -C "$work/tap" add $formulaFile
# A formula whose bytes did not change is a release that was already
# published to the tap; do not make an empty commit for it.
git -C "$work/tap" -c user.name="danalec" -c user.email="danalec@gmail.com" commit --quiet -m "$Formula $Version"
if ($LASTEXITCODE -ne 0) {
    Write-Host ">> the tap already carries this formula; nothing to publish"
    exit 0
}

if ($DryRun) {
    Write-Host "[dry-run] would push $Formula $Version to $tapRepo main"
    git -C "$work/tap" show --stat --oneline HEAD
    exit 0
}

Write-Host ">> pushing $Formula $Version to $tapRepo main"
git -C "$work/tap" push --quiet "https://github.com/$tapRepo.git" HEAD:main
if ($LASTEXITCODE -ne 0) { throw "the push failed" }

Write-Host ""
Write-Host ">> published to https://github.com/$tapRepo"
Write-Host ">> install: brew tap danalec/rgy && brew install $Formula"
