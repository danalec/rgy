<#
.SYNOPSIS
    Publish this repository's crates to crates.io, in an order that keeps every
    published crate installable.

.DESCRIPTION
    The PowerShell twin of scripts/publish-crates.sh. The two are twins, not
    cousins: same output, same order, same exit code. A publish that behaves
    differently on the machine that runs it is a publish nobody can repeat.

    Why the order matters
    ---------------------
    crates.io resolves a dependency when a crate is INSTALLED, not when it
    is published. So a crate that depends on another one cannot be published
    before the dependency is on the registry: publishing it first ships a
    crate that nobody can install, and nothing in the tree says so.

    rgy-name-core depends on rgy-par, and rgy-par was not on the registry at
    all. The order is computed from the manifests themselves
    (scripts/publish-plan.py), so it cannot be forgotten.

    The index is built asynchronously, so a crate that has just been published
    is not a candidate for a dependency for a moment. The script waits for
    the registry to carry what it has published before it publishes the next
    crate.

    Usage:
      powershell -File scripts/publish-crates.ps1 -DryRun
      powershell -File scripts/publish-crates.ps1

    A real run needs CARGO_REGISTRY_TOKEN in the environment. The vendored
    rgx crates are upstream's and are never published here.
#>

$ErrorActionPreference = 'Stop'
$repoRoot = Split-Path -Parent $PSScriptRoot
Set-Location $repoRoot

$dryRun = $false
foreach ($arg in $args) {
    switch ($arg) {
        '-DryRun' { $dryRun = $true }
        default {
            Write-Error "unknown option: $arg"
            exit 2
        }
    }
}

# The plan is computed by a helper both twins call, so the two print the
# same plan and not two plans that look alike.
$py = $env:RGY_PYTHON
if ([string]::IsNullOrWhiteSpace($py)) {
    foreach ($candidate in @('python3', 'python')) {
        if (Get-Command $candidate -ErrorAction SilentlyContinue) {
            $py = $candidate
            break
        }
    }
}
if ([string]::IsNullOrWhiteSpace($py)) {
    Write-Error 'publish-crates: no python found; set RGY_PYTHON'
    exit 1
}

$plan = & $py 'scripts/publish-plan.py' 2>&1
$planCode = $LASTEXITCODE
Write-Host 'crates.io publish plan for rgy'
if ($planCode -ne 0) {
    # The plan failed: the reason is on stderr, and the plan itself is the
    # evidence. Neither is published.
    foreach ($line in $plan) { Write-Error $line }
    Write-Error 'publish-crates: the plan failed; nothing was published'
    exit $planCode
}
foreach ($line in $plan) { Write-Host $line }

if ($dryRun) {
    Write-Host 'dry run: nothing was published'
    exit 0
}

if ([string]::IsNullOrWhiteSpace($env:CARGO_REGISTRY_TOKEN)) {
    Write-Error 'publish-crates: CARGO_REGISTRY_TOKEN is not set; nothing was published'
    exit 1
}

$published = 0
foreach ($line in $plan) {
    # The plan's own order: a crate is published after every crate it
    # depends on, and the lines come out in that order.
    if ($line -notmatch '^\s+publish\s+(\S+)\s+(\S+)\s*$') { continue }
    $name = $matches[1]
    $version = $matches[2]
    Write-Host "publishing $name $version"
    & cargo publish -p $name --locked
    if ($LASTEXITCODE -ne 0) {
        Write-Error "publish-crates: $name $version was not published; nothing after it was"
        exit 1
    }
    # The registry's index lags the publish: the next crate depends on this
    # one, and a dependency the registry has not listed yet is refused.
    & $py 'scripts/publish-plan.py' '--wait' "$name`:$version"
    if ($LASTEXITCODE -ne 0) {
        Write-Error "publish-crates: $name $version did not reach the registry; stopped"
        exit 1
    }
    Write-Host "published $name $version"
    $published++
}

Write-Host "published $published crate(s)"
exit 0
