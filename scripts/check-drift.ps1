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

      tree -- a vendored crate, copied whole from upstream. Two questions
              are asked of it, and they are different questions. The first
              is whether upstream has touched that tree since the pin. Two
              answers are needed for that, not one: which commits upstream
              made, and which files those commits touched. The second is
              the actionable one -- it names the files to look at. A tree
              that upstream has not moved is fine, and says so by saying
              nothing.

              The second question is whether the copy here is the tree
              upstream holds at the pin, byte for byte. That is the half
              the rule is about: these trees are upstream's, and a local
              edit in one of them is a fork nobody asked for. It is
              answered by comparing blob hashes, because two equal hashes
              are two equal files: the upstream side from the pinned
              commit, and our side from the index, which is what a clone
              holds. The files that have a patch row of their own are left
              out -- the patch row checks them -- and so are the excluded
              paths, which upstream has and this repository does not carry.
              A file of ours that upstream does not have is a fork, and is
              reported as one.

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

    For a patch row the question is exact: is the file this repository
    holds the upstream blob at the pinned commit with the local patch
    applied on top? The three sides of that question are read from three
    places, and each is the place that answers it:

      upstream  the blob AT THE COMMIT, because the sync copies the
                commit and not the working tree upstream happens to have
      patched   that blob with the patch applied, in a scratch directory
                outside the repository
      mine      the repository's own bytes, from the index, because the
                index is what a fresh clone gets

    Nothing is normalised. A checkout is not a change, but the repository
    is not a checkout: the index holds the bytes upstream's blob holds,
    and two files that differ only in their line endings are two files.
    The gate that folded that difference away is the gate that let four
    testdata READMEs hide from it.

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

# The help, before the options are read: the script's own header, the way
# the twin prints it.
. (Join-Path $PSScriptRoot "lib/options.ps1")
if (Test-RgyHelp -Arguments $args) {
    Write-Usage
    exit 0
}

# The options, read by the helper the two twins share: the long spelling
# and the PowerShell one are the same option, and an option neither of
# them knows is an error on both platforms -- not a silent "check
# something else", which in a drift gate is a gate that did not run.
$Upstream = ""
$Pin = $false
$For = ""
Read-RgyOptions -Switches @{ "pin" = "Pin" } `
                 -Values   @{ "upstream" = "Upstream"; "for" = "For" } `
                 -Positionals @{} `
                 -Arguments $args

if ($Pin -or $For) {
    Write-Complaint "--pin is gone: the pin column is written by scripts/sync-rgx-upstream.sh,"
    Write-Complaint "which pins every row to the commit the whole sync was taken from."
    Write-Complaint "Re-pin by running the sync."
    exit 1
}

$manifestPath = Join-Path $PSScriptRoot 'vendored-sources.tsv'
if (-not (Test-Path $manifestPath)) {
    Write-Complaint "no manifest at $manifestPath"
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
# The path is resolved by hand and not by Resolve-Path, because
# Resolve-Path throws when the path is not there, and a script that
# crashes with a PowerShell stack trace says nothing about what the
# caller got wrong. The bash twin says "no such directory" and exits 1,
# so this says the same words and exits the same code.
$upstreamDir = ""
try {
    $upstreamDir = (Resolve-Path -LiteralPath $Upstream -ErrorAction Stop).Path
} catch {
    Write-Complaint "no such directory: $Upstream"
    exit 1
}
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

# One argument, quoted for the command line.
#
# git is a native program and reads its arguments from the command line,
# so an argument with a space in it has to be quoted -- and a backslash
# before the closing quote has to be doubled, or the quote is escaped and
# the argument runs on.
function ConvertTo-GitArg([string]$arg) {
    if ($arg -notmatch '[\s"]') { return $arg }
    $escaped = $arg -replace '(\\*)"', '$1$1\"'
    $escaped = $escaped -replace '(\\+)$', '$1$1'
    return '"' + $escaped + '"'
}

# git, asked a question, with the child's output captured as bytes.
#
# The child's stdout is a byte stream and is copied as one: the text
# pipeline would decode it and invent an encoding, and a file with a byte
# that is not valid UTF-8 would come back changed.
#
# Both pipes are redirected and stderr is drained while stdout is
# copied: a pipe that nobody reads fills up and stops the child. The
# stderr is thrown away on purpose. A native command that writes to
# stderr is turned into an error record, and this script stops on the
# first error record -- so "the patch does not apply", which answers on
# stderr, would arrive as a crash instead of as a verdict.
function Invoke-GitProcess {
    param([string[]]$GitArgs)
    $psi = New-Object System.Diagnostics.ProcessStartInfo
    $psi.FileName = 'git'
    $psi.Arguments = (($GitArgs | ForEach-Object { ConvertTo-GitArg $_ }) -join ' ')
    $psi.UseShellExecute = $false
    $psi.RedirectStandardOutput = $true
    $psi.RedirectStandardError = $true
    $proc = New-Object System.Diagnostics.Process
    $proc.StartInfo = $psi
    [void]$proc.Start()
    $errTask = $proc.StandardError.ReadToEndAsync()
    $ms = New-Object System.IO.MemoryStream
    $proc.StandardOutput.BaseStream.CopyTo($ms)
    try { [void]$errTask.Result } catch { }
    $proc.WaitForExit()
    $script:GitExit = $proc.ExitCode
    return [pscustomobject]@{ ExitCode = $proc.ExitCode; Bytes = $ms.ToArray() }
}

# git, asked for bytes, into a file.
function Invoke-GitToFile {
    param([string[]]$GitArgs, [string]$OutFile)
    $r = Invoke-GitProcess $GitArgs
    if ($r.ExitCode -ne 0) { return $false }
    [System.IO.File]::WriteAllBytes($OutFile, $r.Bytes)
    return $true
}

# The answer to a question that has one line.
#
# The whole line and not its first character: a single line comes back
# from a pipeline as a string, and $out[0] on a string is its first
# character. A hash compared by its first character is a hash that is not
# compared.
function Get-GitLine([string[]]$GitArgs) {
    $r = Invoke-GitProcess $GitArgs
    if ($r.ExitCode -ne 0 -or $r.Bytes.Length -eq 0) { return '' }
    $text = [System.Text.Encoding]::UTF8.GetString($r.Bytes)
    $line = ($text -split "`r?`n")[0]
    return $line.Trim()
}

# The upstream file, as the pinned commit holds it.
#
# The sync copies the COMMIT and not the working tree: the manifest pins
# a commit, so the copy has to be that commit's bytes, and a dirty
# upstream working tree would otherwise decide what this repository
# holds. The gate has to ask the question the sync answers, so it reads
# the same bytes.
function Get-UpstreamBlob {
    param([string]$commit, [string]$path, [string]$outFile)
    $gitArgs = @('-C', $upstreamDir, 'cat-file', 'blob', "${commit}:$path")
    return (Invoke-GitToFile $gitArgs $outFile)
}

# The file this repository holds, as the repository holds it.
#
# The index and not the working tree: with core.autocrlf the working tree
# holds CRLF for a file whose blob is LF, and comparing a working tree
# against an upstream blob reports a difference that is not one. The index
# is what a fresh clone gets, so it is what the gate compares.
function Get-RepoBlob {
    param([string]$path, [string]$outFile)
    $gitArgs = @('-C', $repoRoot, 'cat-file', 'blob', ":$path")
    return (Invoke-GitToFile $gitArgs $outFile)
}

# The hash of an index entry, which is the hash of the bytes a fresh clone
# would check out.
function Get-RepoIndexHash([string]$path) {
    return (Get-GitLine @('-C', $repoRoot, 'rev-parse', '--verify', ":$path"))
}

# Does the patch still apply to upstream's content of the file at the pin?
#
# The test is run against a copy of that file in a scratch directory,
# because that is the question the next sync will ask: the sync copies
# upstream's file over the one here and then applies the patch on top. A
# patch that no longer applies means upstream changed the file and the
# patch was not re-derived -- the next sync would take upstream's version
# and drop the fix without saying anything.
#
# Does the patch create the file, or change a file that is already there?
#
# A patch that creates a file has no upstream behind it: the file is the
# patch's own content, and the tree has to hold exactly that. A patch
# that changes a file needs upstream's content to change, and a file
# upstream no longer has is a file the next sync has nothing to put the
# patch on top of.
#
# The argument is the patch file in the scratch directory: the patch, as
# the repository holds it.
function Test-PatchCreatesFile([string]$patchFile) {
    if (-not (Test-Path -LiteralPath $patchFile)) { return $false }
    foreach ($line in [System.IO.File]::ReadAllLines($patchFile)) {
        if ($line -match '^(--- /dev/null|new file mode)') { return $true }
    }
    return $false
}

# Returns 'ok', 'no-upstream-file', 'no-copy', 'not-in-index',
# 'does-not-apply' or 'mismatch'.
#
# The question is the one the sync asks: is the file this repository holds
# the upstream blob at the pin with the local patch applied on top?
#
# The comparison is by bytes and nothing is folded away: two files that
# differ only in their line endings are two files.
function Test-PatchRow([string]$patchRel, [string]$upstreamRel, [string]$pin) {
    $work = Join-Path ([System.IO.Path]::GetTempPath()) ("rgy-drift-" + [guid]::NewGuid().ToString("N"))
    [void](New-Item -ItemType Directory -Path $work)
    try {
        # The patch, as the repository holds it: the index, like the rest
        # of this repository's own bytes.
        #
        # A patch that has not been staged is not in the index, so the
        # gate cannot see it and says the patch does not apply. That is
        # not a broken patch: it is a patch that has not been added to
        # the repository yet. Stage it, and the gate sees it.
        #
        # The verdict says so, because a gate that fails with the wrong
        # reason is worse than no gate: "the patch no longer applies"
        # sends the reader to re-derive a patch that never stopped
        # applying, and the re-derivation is an edit to a file the reader
        # was not asked to touch.
        $patchCopy = Join-Path $work 'rgy.patch'
        if (-not (Get-RepoBlob $patchRel $patchCopy)) { return 'not-in-index' }
        $creates = Test-PatchCreatesFile $patchCopy
        $target = Join-Path $work $upstreamRel
        [void](New-Item -ItemType Directory -Path (Split-Path -Parent $target) -Force)
        # Upstream has the file at the pin: the copy is the file and the
        # patch goes on top of it. Upstream never had it: the patch
        # creates it, and there is nothing to copy.
        if (-not (Get-UpstreamBlob $pin $upstreamRel $target) -and -not $creates) { return 'no-upstream-file' }
        # The copy in this repository, from the index.
        $mineHash = Get-RepoIndexHash $upstreamRel
        if ($mineHash -eq '') { return 'no-copy' }
        # git apply, in the scratch directory: the patch's paths are the
        # repository's paths, and the file it names is the copy of
        # upstream's file sitting next to it.
        #
        # core.autocrlf=false on purpose: the apply has to write exactly
        # the bytes of the patch, and a checkout that rewrote the line
        # endings would write a file the repository does not have. The
        # scratch directory is outside the repository for the same
        # reason: the repository's own rules must not answer for it.
        $gitArgs = @('-C', $work, '-c', 'core.autocrlf=false', 'apply', '--recount', '--whitespace=nowarn', 'rgy.patch')
        $applied = Invoke-GitProcess $gitArgs
        if ($applied.ExitCode -ne 0) { return 'does-not-apply' }

        # The copy and the patch have to agree about what the divergence
        # is. The comparison is by bytes: two files that differ only in
        # their line endings are two files.
        $patchedHash = Get-GitLine @('hash-object', '--no-filters', '--', $target)
        if ($patchedHash -eq '' -or $patchedHash -ne $mineHash) { return 'mismatch' }
        return 'ok'
    }
    finally {
        [System.IO.Directory]::Delete($work, $true)
    }
}

# git, asked a question whose answer is a list of lines.
#
# The child's stderr is thrown away by the helper below, so a command
# that fails answers with nothing instead of stopping the script.
function Get-GitLines([string[]]$GitArgs) {
    $r = Invoke-GitProcess $GitArgs
    if ($r.ExitCode -ne 0) { return @() }
    $text = [System.Text.Encoding]::UTF8.GetString($r.Bytes)
    $out = @()
    foreach ($l in ($text -split "`r?`n")) {
        if ($l -ne '') { $out += $l }
    }
    return $out
}

# Is the path under one of the excluded prefixes?
function Test-Excluded([string]$path, [string[]]$excluded) {
    foreach ($e in $excluded) {
        if ($e -and $path.StartsWith($e)) { return $true }
    }
    return $false
}

# The tree upstream holds at the pin, and the tree this repository holds.
#
# Both sides come out of one call each: `ls-tree` gives the files of the
# commit, `ls-files` gives the entries of the index. Two equal blob
# hashes are two equal files, so the comparison never looks at the bytes
# themselves -- and it does not look at the working tree either, because
# the working tree is not what a clone holds.
#
# Three kinds of path are left out of the comparison, and each of them is
# left out for a reason:
#
#   * the files that have a patch row of their own. The patch row is
#     what checks them, byte for byte, against upstream plus the patch,
#     and a file a patch creates is not upstream's at all.
#
#   * the excluded paths. They are upstream's files that this repository
#     does not carry, so their absence is the rule and not a difference.
#     The ones of them that the index holds are reported on their own:
#     the sync removes them, so one that is here is a divergence.
#
# Returns the lines:
#   missing: <path>   upstream has it, the copy does not
#   differs: <path>   both have it, and the bytes are not the same
#   fork: <path>      the copy has it, upstream does not
#   excluded: <path>  an excluded path is in the index
function Get-TreeProblems {
    param([string]$upstreamPath, [string]$repoPath, [string]$pin, [string[]]$excluded, [string[]]$patchPaths)
    $up = [ordered]@{}
    $upOrder = @()
    $ours = [ordered]@{}
    $ourOrder = @()
    $excludedFound = @()
    # "<blob hash>\t<path>" for each side. `ls-tree` prints
    # "<mode> SP <type> SP <hash> TAB <path>" and `ls-files` prints
    # "<mode> SP <hash> SP <stage> TAB <path>", so the hash is the third
    # field of the first one and the second of the second.
    foreach ($line in (Get-GitLines @('-C', $upstreamDir, 'ls-tree', '-r', $pin, '--', $upstreamPath))) {
        $tab = $line.IndexOf("`t")
        if ($tab -lt 0) { continue }
        $meta = $line.Substring(0, $tab).Split(' ')
        $path = $line.Substring($tab + 1)
        if (Test-Excluded $path $excluded) { continue }
        if ($patchPaths -contains $path) { continue }
        $up[$path] = $meta[2]
        $upOrder += $path
    }
    foreach ($line in (Get-GitLines @('-C', $repoRoot, 'ls-files', '-s', '--', $repoPath))) {
        $tab = $line.IndexOf("`t")
        if ($tab -lt 0) { continue }
        $meta = $line.Substring(0, $tab).Split(' ')
        $path = $line.Substring($tab + 1)
        if (Test-Excluded $path $excluded) { $excludedFound += $path; continue }
        if ($patchPaths -contains $path) { continue }
        $ours[$path] = $meta[1]
        $ourOrder += $path
    }
    $problems = @()
    foreach ($p in $upOrder) {
        if (-not $ours.Contains($p)) { $problems += "missing: $p" }
    }
    foreach ($p in $upOrder) {
        if ($ours.Contains($p) -and $ours[$p] -ne $up[$p]) { $problems += "differs: $p" }
    }
    foreach ($p in $ourOrder) {
        if (-not $up.Contains($p)) { $problems += "fork: $p" }
    }
    foreach ($p in $excludedFound) { $problems += "excluded: $p" }
    return $problems
}

# --- the manifest ------------------------------------------------------------
#
# Five tab-separated columns: rgx_path, upstream path, pin, kind, note.
# The rows are the manifest minus its comments and blank lines. A row that
# does not have four columns cannot say what kind of row it is, and the
# manifest is generated, so a malformed one is a bug in the generator
# rather than something to report here.
#
# The manifest carries the excluded paths as comment lines, one per line,
# each of them "# exclude: <path>". A comment line is not a row, so the
# rows stay the rows and the count of them does not move; the exclusions
# are read out of the same file the rows come from, because the sync is
# what writes both and there is one answer to what is excluded.
$rows = @()
$excluded = @()
foreach ($line in Get-Content -LiteralPath $manifestPath -Encoding UTF8) {
    if ($line -match '^# exclude: (.+)$') { $excluded += $Matches[1].Trim(); continue }
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

# The paths that have a patch row of their own. The tree comparison
# leaves them out of both sides: the patch row is what checks them, byte
# for byte, against upstream plus the patch. A file a patch creates is in
# here too, and that is what keeps it from being a fork.
$patchPaths = @($rows | Where-Object { $_.Kind -eq 'patch' } | ForEach-Object { $_.Upstream })

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
            # One row is one drift, whatever it complains about: the two
            # questions below are two questions about the same row.
            $rowDrifted = $false
            $commits = Get-UpstreamLog $r.Pin $r.Upstream
            if ($commits.Count -gt 0) {
                $rowDrifted = $true
                Write-Report "  FAIL: $($r.Rgx): upstream has moved since $($r.Pin) ($($commits.Count) commit(s)):" 'Red'
                foreach ($c in $commits) { Write-Report "    $c" 'Red' }
                $files = Get-UpstreamChangedFiles $r.Pin $r.Upstream
                if ($files.Count -gt 0) {
                    Write-Report "    changed files:" 'Red'
                    foreach ($f in $files) { Write-Report "      $f" 'Red' }
                }
            }
            # And the other half: the copy has to be upstream's tree at the
            # pin, byte for byte. A tree that was edited here is a fork,
            # and a fork is what the vendoring rule forbids.
            $problems = Get-TreeProblems $r.Upstream $r.Rgx $r.Pin $excluded $patchPaths
            if ($problems) {
                $rowDrifted = $true
                Write-Report "  FAIL: $($r.Rgx): the copy does not match upstream's tree at $($r.Pin):" 'Red'
                foreach ($p in $problems) { Write-Report "    $p" 'Red' }
            }
            if ($rowDrifted) { $drifted++ }
        }
        'patch' {
            $verdict = Test-PatchRow $r.Rgx $r.Upstream $r.Pin
            if ($verdict -ne 'ok') {
                $drifted++
                switch ($verdict) {
                    'no-upstream-file' { Write-Report "  FAIL: $($r.Rgx): upstream has no $($r.Upstream)" 'Red' }
                    'no-copy'         { Write-Report "  FAIL: $($r.Rgx): the patched file $($r.Upstream) is not in the index (the gate reads the index, not the working tree)" 'Red' }
                    'not-in-index'    { Write-Report "  FAIL: $($r.Rgx): the patch is not in the index (the gate reads the index, not the working tree)" 'Red' }
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
