# apply-vendored-edits.ps1 -- apply rgy's own divergences to the vendored rgx trees.
#
# Usage:
#   pwsh -File scripts/apply-vendored-edits.ps1 [-Upstream <path>] [-DryRun]
#
# What this is for
# ----------------
# The vendored rgx crates are upstream rgx's crates, transported verbatim:
# the copy in crates/rgx-core/ is upstream's crates/rgx-core/, byte for byte,
# and scripts/sync-rgx-upstream.sh puts it there. A verbatim copy has no
# room for a local edit -- so where rgy must differ from upstream, the
# difference is a patch, kept under scripts/rgx-local-patches/ and applied
# on top of the copy.
#
# That is the whole content of this directory today, and it is deliberately
# small: each patch is a bug upstream has not fixed, or a fix upstream has
# not taken, and each one is a line of divergence that has to be looked at
# on every sync.
#
# What this script does NOT do
# ----------------------------
# It does not rewrite the copy. The transforms it used to apply are gone,
# and the reason they were false is worth keeping in the record:
#
#   * `alloc::` -> `std::` -- both projects are `#![no_std]` + `alloc` now,
#     so there is nothing to rewrite.
#   * module path flattening -- upstream's crates/rgx-core/src/ has the
#     same layout as this workspace's crates/rgx-core/src/, so there is no
#     second path to reach a sibling module by.
#   * lifting visibility -- upstream already declares `pub mod nfa;` and
#     the rest.
#   * turning compile-time SIMD gates into runtime dispatch -- upstream
#     dispatches at runtime now, for the same reason rgy did: a binary
#     that ships to arbitrary machines cannot assume AVX2.
#
# A script that applies transforms nobody needs is a script that will
# eventually apply one nobody wants. The copy is a copy.
#
# The script is idempotent: it derives each patched file from upstream's
# current content plus the patch, so running it twice changes nothing, and
# running it after a sync restores a file the sync overwrote.
#
# The Linux counterpart is scripts/apply-vendored-edits.sh, and the two are
# twins: same text, same order, same exit code, on the same input.
#
# Exit codes: 0 all patches applied, 1 at least one failed.

[CmdletBinding()]
$ErrorActionPreference = 'Stop'

# usage -- what the two scripts are for, in the same words in both.
function Write-Usage {
    $lines = @(
        "apply-vendored-edits [--upstream <path>] [--dry-run]"
        ""
        "Applies rgy's own divergences to the vendored rgx trees: every patch"
        "under scripts/rgx-local-patches/ goes on top of the copy of upstream's"
        "file, so the vendored tree carries rgy's fixes and nothing else."
        "The copy is a copy — the script does not rewrite it."
        ""
        "Exits 0 when every patch applies, 1 when one does not."
    )
    foreach ($line in $lines) { [Console]::Out.Write($line + "`n") }
}

# The help, before the options are read: the script's own usage, the way
# the twin prints it.
. (Join-Path $PSScriptRoot "lib/options.ps1")
if (Test-RgyHelp -Arguments $args) {
    Write-Usage
    exit 0
}
# The options, read by the helper the two twins share: the long spelling
# and the PowerShell one are the same option, and an option neither of
# them knows is an error on both platforms -- not a silent "do the real
# thing", which in this directory means patching the vendored trees.
$Upstream = ""
$DryRun = $false
Read-RgyOptions -Switches @{ "dryrun" = "DryRun" } `
                 -Values   @{ "upstream" = "Upstream" } `
                 -Positionals @{} `
                 -Arguments $args

$here = Split-Path -Parent $MyInvocation.MyCommand.Path
$repo = Split-Path -Parent $here
Push-Location $repo
try {
    if ([string]::IsNullOrWhiteSpace($Upstream)) {
        # Relative to the repository root, and printed as given, so that the
        # bash twin prints the same line.
        $Upstream = "../rgx"
    }
    if (-not (Test-Path -LiteralPath (Join-Path $Upstream ".git"))) {
        Write-Host "no upstream checkout at $Upstream"
        Write-Host "  pass -Upstream <path to an rgx checkout>"
        exit 1
    }
    $upstreamAbs = (Resolve-Path -LiteralPath $Upstream).Path

    $patchDir = Join-Path $here "rgx-local-patches"
    if (-not (Test-Path -LiteralPath $patchDir)) {
        Write-Host "no patch directory at $patchDir"
        exit 1
    }

    Write-Host "applying rgy's local divergences to the vendored rgx crates"
    Write-Host "  upstream: $Upstream"

    $applied = 0
    $unchanged = 0
    $failed = 0

    $work = Join-Path ([IO.Path]::GetTempPath()) ("rgy-apply-" + [guid]::NewGuid().ToString("N"))
    New-Item -ItemType Directory -Path $work -Force | Out-Null
    try {
        $patches = @(Get-ChildItem -LiteralPath $patchDir -Filter "*.patch" -File -Recurse |
            Sort-Object FullName)

        foreach ($patch in $patches) {
            $rel = $patch.FullName.Substring($patchDir.Length).TrimStart('\', '/')
            $rel = $rel.Substring(0, $rel.Length - ".patch".Length)
            $rel = $rel -replace '\\', '/'
            $upstreamFile = Join-Path $upstreamAbs ($rel -replace '/', '\')
            $vendoredFile = Join-Path $repo ($rel -replace '/', '\')

            if (-not (Test-Path -LiteralPath $upstreamFile)) {
                Write-Host ("  {0,-58} FAILED: upstream no longer has this file" -f $rel)
                $failed++
                continue
            }

            # Build the patched file in a scratch tree: upstream's current
            # content with the patch on top. The result is what the vendored
            # file must be, so the script does not care whether the vendored
            # file is currently patched, stale, or missing.
            #
            # The base is the upstream COMMIT's blob, not the file in its
            # working tree: a checkout with core.autocrlf hands out CRLF for
            # content whose blob is LF, and a patch whose context is LF then
            # matches nothing. The commit is also what the sync copies from,
            # so the two agree.
            $tree = Join-Path $work "tree"
            if (Test-Path -LiteralPath $tree) { Remove-Item -LiteralPath $tree -Recurse -Force }
            $target = Join-Path $tree ($rel -replace '/', '\')
            New-Item -ItemType Directory -Path (Split-Path -Parent $target) -Force | Out-Null
            # The blob goes out as raw bytes: a text pipeline would rewrite
            # the line endings, and the patch's context is LF.
            $start = New-Object System.Diagnostics.ProcessStartInfo
            $start.FileName = "git"
            $start.Arguments = "-C `"$upstreamAbs`" cat-file blob `"HEAD:$rel`""
            $start.RedirectStandardOutput = $true
            $start.UseShellExecute = $false
            $proc = [System.Diagnostics.Process]::Start($start)
            $out = [System.IO.File]::Create($target)
            $proc.StandardOutput.BaseStream.CopyTo($out)
            $out.Close()
            $proc.WaitForExit()
            if ($proc.ExitCode -ne 0) {
                Write-Host ("  {0,-58} FAILED: upstream no longer has this file" -f $rel)
                $failed++
                continue
            }

            Push-Location $tree
            try {
                # `core.autocrlf=false` on purpose: the scratch tree is
                # outside this repository, so git apply would fall back to
                # the machine's global setting and rewrite the line endings
                # of the file it writes. A checkout is not a change, and the
                # copy has to stay what the commit says it is.
                & git -c core.autocrlf=false apply --check --recount --whitespace=nowarn $patch.FullName 2>&1 | Out-Null
                if ($LASTEXITCODE -ne 0) {
                    Write-Host ("  {0,-58} FAILED: the patch does not apply to upstream's current file" -f $rel)
                    $failed++
                    continue
                }
                & git -c core.autocrlf=false apply --recount --whitespace=nowarn $patch.FullName 2>&1 | Out-Null
                if ($LASTEXITCODE -ne 0) {
                    Write-Host ("  {0,-58} FAILED: the patch applied to the check but not to the file" -f $rel)
                    $failed++
                    continue
                }
            }
            finally {
                Pop-Location
            }

            if ((Test-Path -LiteralPath $vendoredFile) -and
                ((Get-FileHash -LiteralPath $vendoredFile -Algorithm SHA256).Hash -eq
                 (Get-FileHash -LiteralPath $target -Algorithm SHA256).Hash)) {
                Write-Host ("  {0,-58} unchanged" -f $rel)
                $unchanged++
                continue
            }

            if ($DryRun) {
                Write-Host ("  {0,-58} would update" -f $rel)
            }
            else {
                New-Item -ItemType Directory -Path (Split-Path -Parent $vendoredFile) -Force | Out-Null
                Copy-Item -LiteralPath $target -Destination $vendoredFile -Force
                Write-Host ("  {0,-58} updated" -f $rel)
            }
            $applied++
        }
    }
    finally {
        Remove-Item -LiteralPath $work -Recurse -Force -ErrorAction SilentlyContinue
    }

    Write-Host ""
    if ($failed -gt 0) {
        Write-Host "$applied applied, $unchanged unchanged, $failed failed"
        exit 1
    }
    Write-Host "$applied applied, $unchanged unchanged, 0 failed"
    exit 0
}
finally {
    Pop-Location
}
