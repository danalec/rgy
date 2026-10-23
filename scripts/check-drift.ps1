<#
.SYNOPSIS
    Report vendored code that has fallen behind its upstream source.

.DESCRIPTION
    scripts/check-vendoring.ps1 enforces the *shape* of the vendoring rule:
    no [patch] redirect, a provenance header on each vendored tree. None
    of that can notice that a file was copied on Monday and that the
    upstream changed it on Tuesday.

    This asks the other half: for each row in scripts/vendored-sources.tsv,
    has the upstream moved past the commit that row was taken from?

    The manifest has two kinds of row, and they drift in different ways.

      tree -- a vendored crate, copied whole from upstream. The question
              is whether upstream has touched that tree since the pin. Two
              answers are needed, not one: which commits upstream made, and
              which files those commits touched. The second is the
              actionable one -- it names the files to look at. A tree that
              upstream has not moved is fine, and says so by saying
              nothing.

      patch -- a divergence from upstream, kept as a patch under
              scripts/rgx-local-patches/. Two things can be wrong with it,
              and both are drift:

                * the patch no longer applies to upstream's current content
                  of the file. Upstream moved and the patch did not, so the
                  next sync would copy upstream's file and silently drop
                  the fix. That is reported loudly, because a patch that
                  stops applying is a fix that stops existing.

                * the file in this tree is not what upstream's file plus
                  the patch produces. The copy and the patch disagree about
                  what the divergence is, so one of the two is wrong.

    A row whose pin is not a commit in the upstream checkout is UNPINNED
    and counts as a failure. A manifest that quietly leaves a row out, or
    pins it to a guess, is worse than one that admits it has not looked:
    the first one reads as "nothing to do here".

    The upstream checkout is a read-only input. Nothing in this repository
    is written, and nothing in the upstream is written either.

    The old -Pin mode is gone. The manifest used to be written by hand, one
    hash per file, and a hand-written hash is a check that passes over
    drift; -Pin picked the hash mechanically instead. The manifest is
    generated now (scripts/sync-rgx-upstream.sh writes the pin for every
    row when it copies the trees), so the pin column is the generator's to
    write: the sync takes the whole vendoring from one commit, and a
    per-row pin would say something the generator does not. Re-pin by
    running the sync.

    This script and scripts/check-drift.sh are twins: the same questions,
    the same words, the same bytes. Write-Host is not used for the report
    because it ends a line with CRLF and lets the host decide the colour,
    and the bash twin has no host to ask -- so the escapes and the newline
    are written here, and both scripts write the same ones.

.PARAMETER Upstream
    Path to a git checkout of the upstream source. Required. It is a
    local, read-only input -- passing it does not make rgy depend on
    the upstream, and nothing in any manifest changes.

.EXAMPLE
    powershell -File scripts/check-drift.ps1 -Upstream ../rgx
    Lists, per vendored row, what upstream has moved that the copy has
    not seen.

    Run from anywhere; the script re-roots itself from $PSScriptRoot.
    Exits 1 on any drift or any unpinned row, 0 otherwise.
#>

param(
    [string]$Upstream = "",
    [switch]$Pin,
    [string]$For = "",
    [switch]$Help
)

$ErrorActionPreference = 'Stop'
# git is asked questions whose answer is sometimes "no" -- does this pin
# resolve? -- and a native command that exits non-zero must not throw just
# because it did. PowerShell 7.4 made that the default; undo it here.
if (Get-Variable -Name PSNativeCommandUseErrorActionPreference -ErrorAction SilentlyContinue) {
    $PSNativeCommandUseErrorActionPreference = $false
}

$repoRoot = Split-Path -Parent $PSScriptRoot
Set-Location $repoRoot

# --- the report --------------------------------------------------------------
#
# The bash twin writes the same escapes and the same LF endings, so the two
# can be compared byte for byte and not only word for word. The report
# goes to stdout; the complaints go to stderr, in both scripts.
$script:Esc = [string][char]27
$script:Colors = @{
    Red    = "$($script:Esc)[31m"
    Green  = "$($script:Esc)[32m"
    Yellow = "$($script:Esc)[33m"
    Cyan   = "$($script:Esc)[36m"
}
try { [Console]::OutputEncoding = New-Object System.Text.UTF8Encoding($false) } catch { }

function Write-Report([string]$text = '', [string]$color = '') {
    if ($color -and $script:Colors.ContainsKey($color)) {
        [Console]::Out.Write($script:Colors[$color] + $text + "$($script:Esc)[0m`n")
    } else {
        [Console]::Out.Write($text + "`n")
    }
}

function Write-Complaint([string]$text = '') {
    [Console]::Error.Write($script:Colors['Red'] + $text + "$($script:Esc)[0m`n")
}

# usage -- what the two scripts are for, in the same words in both.
function Write-Usage {
    Write-Report "check-drift --upstream <path to an upstream checkout>"
    Write-Report ""
    Write-Report "Reports vendored code that has fallen behind its upstream source:"
    Write-Report "for each row in scripts/vendored-sources.tsv, has the upstream moved"
    Write-Report "past the commit that row was taken from? Exits 1 on any drift or any"
    Write-Report "unpinned row, 0 otherwise."
}

if ($Help) {
    Write-Usage
    exit 0
}

# An argument this script does not know is an error, not something to
# ignore: the bash twin says so too, and a check that quietly ignores half
# its command line is a check that ran something else.
if ($args.Count -gt 0) {
    Write-Complaint "unknown argument: $($args[0])"
    exit 1
}

$manifestPath = Join-Path $PSScriptRoot 'vendored-sources.tsv'
if (-not (Test-Path $manifestPath)) {
    Write-Complaint "no manifest at $manifestPath"
    exit 1
}

if ($Pin -or $For) {
    Write-Complaint "--pin is gone: the pin column is written by scripts/sync-rgx-upstream.sh,"
    Write-Complaint "which pins every row to the commit the whole sync was taken from."
    Write-Complaint "Re-pin by running the sync."
    exit 1
}

if (-not $Upstream) {
    Write-Complaint "no -Upstream path given; this check needs a local upstream checkout to ask"
    Write-Complaint "  example: scripts/check-drift.ps1 --upstream ../rgx"
    Write-Complaint "          scripts/check-drift.sh --upstream ../rgx"
    exit 1
}
if (-not (Get-Command git -ErrorAction SilentlyContinue)) {
    Write-Complaint "git not found; this check asks git about the upstream"
    exit 1
}
$upstreamDir = (Resolve-Path -LiteralPath $Upstream).Path
if (-not (Test-Path (Join-Path $upstreamDir '.git'))) {
    Write-Complaint "$upstreamDir is not a git checkout"
    exit 1
}

# --- git, asked a question ---------------------------------------------------
#
# The answer is the output lines; $script:GitExit is the exit code, because
# "no such commit" is an answer and not a crash. A helper keeps the calls
# below one line each, which matters here: the check is a list of
# questions about other people's commits, and each one has to read like
# the question it asks.
function Invoke-Git {
    param([string[]]$GitArgs)
    $out = & git @GitArgs 2>$null
    $script:GitExit = $LASTEXITCODE
    $lines = @()
    foreach ($l in @($out)) {
        if ($null -eq $l) { continue }
        $t = [string]$l
        if ($t -eq '') { continue }
        $lines += $t
    }
    return $lines
}

# A pin that does not resolve reads as "zero commits behind" -- `git log`
# fails, the error is swallowed, and the row goes green having been
# checked against nothing. Resolve the commit first and say so when it is
# not real.
function Test-UpstreamCommit([string]$commit) {
    $out = Invoke-Git @('-C', $upstreamDir, 'rev-parse', '--verify', '-q', "$commit^{commit}")
    return ($script:GitExit -eq 0 -and $out.Count -gt 0)
}

# The commits upstream made under a path since the pin.
function Get-UpstreamLog([string]$commit, [string]$path) {
    return Invoke-Git @('-C', $upstreamDir, 'log', '--oneline', '--no-color', "$commit..HEAD", '--', $path)
}

# The files those commits touched. This is the actionable half of the
# answer: the commits say what upstream did, the files say where to look.
function Get-UpstreamChangedFiles([string]$commit, [string]$path) {
    return Invoke-Git @('-C', $upstreamDir, 'diff', '--name-only', '--no-color', "$commit..HEAD", '--', $path)
}

# A copy with the carriage returns taken out.
#
# A checkout is not a change: the same source is CRLF on one machine and LF
# on another, and a patch that applies to one has to apply to the other.
# The bytes are moved through Latin-1 so that what comes out is what went
# in, minus the CRs -- no encoding is decoded and another one invented.
function Write-Normalized([string]$src, [string]$dst) {
    $latin = [System.Text.Encoding]::GetEncoding(28591)
    $text = $latin.GetString([System.IO.File]::ReadAllBytes($src))
    $text = $text.Replace("`r", "")
    [System.IO.File]::WriteAllBytes($dst, $latin.GetBytes($text))
}

# Does the patch still apply to upstream's current content of the file?
#
# The test is run against a copy of upstream's file in a scratch
# directory, because that is the question the next sync will ask: the sync
# copies upstream's file over the one here and then applies the patch on
# top. A patch that no longer applies means upstream changed the file and
# the patch was not re-derived -- the next sync would take upstream's
# version and drop the fix without saying anything.
#
# Returns 'ok', 'no-upstream-file', 'no-copy', 'does-not-apply' or
# 'mismatch'.
function Test-PatchRow([string]$patchRel, [string]$upstreamRel) {
    $upFile = Join-Path $upstreamDir $upstreamRel
    if (-not (Test-Path -LiteralPath $upFile)) { return 'no-upstream-file' }
    $myFile = Join-Path $repoRoot $upstreamRel
    if (-not (Test-Path -LiteralPath $myFile)) { return 'no-copy' }

    $work = Join-Path ([System.IO.Path]::GetTempPath()) ("rgy-drift-" + [guid]::NewGuid().ToString("N"))
    [void](New-Item -ItemType Directory -Path $work)
    try {
        $target = Join-Path $work $upstreamRel
        [void](New-Item -ItemType Directory -Path (Split-Path -Parent $target) -Force)
        Write-Normalized $upFile $target
        $patchCopy = Join-Path $work 'rgy.patch'
        Write-Normalized (Join-Path $repoRoot $patchRel) $patchCopy

        # git apply, in the scratch directory: the patch's paths are the
        # repository's paths, and the file it names is the copy of
        # upstream's file sitting next to it.
        & git -C $work apply --recount --whitespace=nowarn 'rgy.patch' 2>$null
        if ($LASTEXITCODE -ne 0) { return 'does-not-apply' }

        # The copy and the patch have to agree about what the divergence
        # is. Compare the patched upstream file with the file here.
        #
        # Both sides are normalised, including the patched one: git apply
        # writes the file back with the line endings the checkout's own
        # rules say, and a checkout is not a change.
        $patched = Join-Path $work 'rgy-patched'
        Write-Normalized $target $patched
        $mine = Join-Path $work 'rgy-mine'
        Write-Normalized $myFile $mine
        $a = Invoke-Git @('hash-object', '--no-filters', '--', $patched)
        $b = Invoke-Git @('hash-object', '--no-filters', '--', $mine)
        if ($a.Count -eq 0 -or $b.Count -eq 0 -or $a[0] -ne $b[0]) { return 'mismatch' }
        return 'ok'
    }
    finally {
        [System.IO.Directory]::Delete($work, $true)
    }
}

# --- the manifest ------------------------------------------------------------
#
# Five tab-separated columns: rgx_path, upstream path, pin, kind, note.
# The rows are the manifest minus its comments and blank lines. A row that
# does not have four columns cannot say what kind of row it is, and the
# manifest is generated, so a malformed one is a bug in the generator
# rather than something to report here.
$rows = @()
foreach ($line in Get-Content -LiteralPath $manifestPath -Encoding UTF8) {
    if ($line.Trim() -eq '' -or $line.StartsWith('#')) { continue }
    $f = $line -split "`t"
    if ($f.Count -lt 4) { continue }
    $rows += [pscustomobject]@{
        Rgx      = $f[0].Trim()
        Upstream = $f[1].Trim()
        Pin      = $f[2].Trim()
        Kind     = $f[3].Trim()
        Note     = $(if ($f.Count -ge 5) { $f[4].Trim() } else { '' })
    }
}

Write-Report "checking vendored drift against upstream at $upstreamDir" 'Cyan'

# --- the check ---------------------------------------------------------------
$total = $rows.Count
$checked = 0
$drifted = 0
$unpinned = 0
$unpinnedRows = @()

Write-Report "" ''
Write-Report "[1/1] vendored trees and patches against their upstream source" 'Cyan'

foreach ($r in $rows) {
    if (-not (Test-Path -LiteralPath (Join-Path $repoRoot $r.Rgx))) {
        Write-Report "  FAIL: $($r.Rgx): listed in the manifest but not in the tree" 'Red'
        $drifted++
        continue
    }
    if ($r.Pin -eq '' -or $r.Pin -eq '-') {
        Write-Report "  UNPINNED: $($r.Rgx): no sync point recorded (the pin is `"$($r.Pin)`")" 'Yellow'
        $unpinned++
        $unpinnedRows += $r.Rgx
        continue
    }
    if (-not (Test-UpstreamCommit $r.Pin)) {
        Write-Report "  UNPINNED: $($r.Rgx): pin $($r.Pin) is not a commit in the upstream checkout" 'Yellow'
        $unpinned++
        $unpinnedRows += $r.Rgx
        continue
    }

    $checked++
    switch ($r.Kind) {
        'tree' {
            $commits = Get-UpstreamLog $r.Pin $r.Upstream
            if ($commits.Count -gt 0) {
                $drifted++
                Write-Report "  FAIL: $($r.Rgx): upstream has moved since $($r.Pin) ($($commits.Count) commit(s)):" 'Red'
                foreach ($c in $commits) { Write-Report "    $c" 'Red' }
                $files = Get-UpstreamChangedFiles $r.Pin $r.Upstream
                if ($files.Count -gt 0) {
                    Write-Report "    changed files:" 'Red'
                    foreach ($f in $files) { Write-Report "      $f" 'Red' }
                }
            }
        }
        'patch' {
            $verdict = Test-PatchRow $r.Rgx $r.Upstream
            if ($verdict -ne 'ok') {
                $drifted++
                switch ($verdict) {
                    'no-upstream-file' { Write-Report "  FAIL: $($r.Rgx): upstream has no $($r.Upstream)" 'Red' }
                    'no-copy'         { Write-Report "  FAIL: $($r.Rgx): the patched file $($r.Upstream) is not in the tree" 'Red' }
                    'does-not-apply'  { Write-Report "  FAIL: $($r.Rgx): the patch no longer applies to upstream's current content of $($r.Upstream)" 'Red' }
                    'mismatch'        { Write-Report "  FAIL: $($r.Rgx): the copy does not match upstream's current content plus the patch" 'Red' }
                }
            }
        }
        default {
            $drifted++
            Write-Report "  FAIL: $($r.Rgx): unknown kind `"$($r.Kind)`"" 'Red'
        }
    }
}

if ($unpinnedRows.Count -gt 0) {
    Write-Report "  $($unpinnedRows.Count) row(s) have no recorded sync point:" 'Yellow'
    foreach ($u in $unpinnedRows) { Write-Report "    - $u" 'Yellow' }
    Write-Report "    pin one with: scripts/sync-rgx-upstream.sh --upstream <path>" 'Yellow'
}

Write-Report "" ''
if ($unpinned -gt 0 -or $drifted -gt 0) {
    $n = $drifted + $unpinned
    Write-Report "vendored drift: $n outstanding ($drifted drifted, $unpinned unpinned); $checked of $total row(s) checked" 'Red'
    exit 1
}
Write-Report "ok: $total row(s) checked, no drift" 'Green'
exit 0
