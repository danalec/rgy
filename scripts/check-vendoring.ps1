<#
.SYNOPSIS
    Guards the vendoring rules: rgx links nothing from upstream, and every
    vendored file declares its source.

.DESCRIPTION
    rgx copies code out of upstream; it never depends on it. This script fails
    if that ever stops being true, so the rule is checked rather than
    remembered. Six checks:

      1. No upstream reference in any Cargo manifest, Cargo.lock, or
         .cargo/config.toml. Provenance comments in *other* files are
         fine and are not flagged; only manifests matter, because only
         manifests can create a dependency edge.
      2. No [patch] section in .cargo/config.toml, which could redirect a
         dependency to a local upstream checkout without a path entry.
      3. Every vendored source file carries a provenance header naming
         upstream, so copied code stays traceable to its origin.
      4. No external crates.io dependencies. Known-remaining ones are
         listed with what replaces them, so the count is a progress
         metric and a newly added crate fails the check. The count is
         split in two: a crate the product links, and a crate kept only
         as a dev-dependency -- an oracle, the thing an in-house
         implementation is held to.
      5. Every dependency on a vendored crate names it by path, and no
         lock file resolves one from a registry. The vendored crates
         carry the names upstream publishes on crates.io, at the same
         version, so a version-only dependency would resolve to the
         registry copy instead of the tree -- silently, and with cargo
         having nothing to say about it.
      6. The vendored manifest (scripts/vendored-sources.tsv) still
         describes the tree: every path it names is there, every vendored
         crate and every local patch has a row of its own, the header is
         the one the sync writes, and every row is pinned to the commit
         that header names. A manifest that falls behind the tree it
         describes reads as "nothing to do here", which is the one thing
         a manifest must never do.

    Run from the repo root:  powershell -File scripts/check-vendoring.ps1
    Exits 1 on any violation.
#>

$ErrorActionPreference = 'Stop'

# This script takes no options, and an argument it does not know is an
# error: the twin refuses it with the same words and the same exit code. A
# check that quietly ignores half its command line is a check that ran
# something else.
. (Join-Path $PSScriptRoot "lib/options.ps1")
Read-RgyOptions -Positionals @{} -Arguments $args

$repoRoot = Split-Path -Parent $PSScriptRoot
Set-Location $repoRoot

# Get-RgyRelativePath <full path> — the relative path of a file, in the
# spelling the shell twin gives it.
#
# The shell twin builds this with its rel_path helper: a ".\" prefix and a
# "\" separator — the Windows form, because Git Bash and WSL are Windows
# shells in a POSIX costume, and they translate their paths with cygpath
# and wslpath. The two twins are compared byte for byte on the developer's
# machine, so this one has to spell the path the way the shell twin spells
# it, and not the way PowerShell would.
#
# Resolve-Path -Relative is not that spelling: it drops the leading ".\"
# when the first segment of the path starts with a dot, so a file under
# .cargo came back as ".cargo\config.toml" where the shell twin writes
# ".\.cargo\config.toml". The same violation then printed two different
# bytes on the two platforms — which is the one thing a pair of twins may
# not do.
function Get-RgyRelativePath([string]$fullName) {
    $rel = $fullName
    if ($fullName.StartsWith($repoRoot, [System.StringComparison]::OrdinalIgnoreCase)) {
        $rel = $fullName.Substring($repoRoot.Length).TrimStart([char[]]@('\', '/'))
    }
    return '.\' + $rel.Replace('/', '\')
}

# The .sh twin prunes a DIRECTORY named target below the root, and the rule
# is relative to the root on purpose: a directory named target IN the tree is
# build output, and a checkout that LIVES under a directory called target is
# still the tree.
#
# The filter here used to look at the absolute path, so the checkout's own
# ancestors counted -- and a checkout under a directory named target hid the
# whole tree from this script, while the shell twin walked it in full and
# answered for it. The two twins then said different things about the same
# files, which is the one thing a pair of twins may not do.
#
# So the rule is the shell twin's, spelled for PowerShell: the directories
# above the file are read one segment at a time, and a segment that is
# exactly "target" is build output. A file whose own name holds "target" is
# not build output, and the shell twin sees it.
#
# The segment named ".git" is left out for the same reason and for a sharper
# one: it holds the repository's own metadata -- the objects, the refs, the
# index -- and none of that is a file of the project. A checkout's .git
# holds objects, and a developer's holds whatever they left in it: a
# scratch copy of the tree, a probe, a published crate. A gate that reads a
# developer's scratch is a gate that gets turned off, and a gate that reads
# a copy of the tree cannot tell the copy from the tree. So the walk does
# not descend into it, and the rule is still relative to the root: a
# directory called .git BELOW the root is skipped, and the root's own
# ancestors never enter the question.
function Test-RgyInTree([string]$fullName) {
    $rel = $fullName
    if ($fullName.StartsWith($repoRoot, [System.StringComparison]::OrdinalIgnoreCase)) {
        $rel = $fullName.Substring($repoRoot.Length).TrimStart([char[]]@('\', '/'))
    }
    $dir = Split-Path -Parent $rel
    if ([string]::IsNullOrEmpty($dir)) { return $true }
    foreach ($seg in ($dir -split '[\\/]')) {
        if ($seg -eq 'target' -or $seg -eq '.git') { return $false }
    }
    return $true
}

# The walk the shell twin performs with find's -prune. Get-ChildItem
# -Recurse has no prune, so the tree is walked a top-level directory at a
# time and the repository's own metadata is left out of it; a .git nested
# deeper in the tree is caught by the filter above.
function Get-RgyTreeFiles([string]$name) {
    $files = New-Object System.Collections.Generic.List[System.IO.FileInfo]
    foreach ($f in Get-ChildItem -Path . -Filter $name -File) { $files.Add($f) }
    foreach ($d in Get-ChildItem -Path . -Directory) {
        if ($d.Name -eq 'target' -or $d.Name -eq '.git') { continue }
        foreach ($f in Get-ChildItem -Path $d.FullName -Filter $name -Recurse -File) { $files.Add($f) }
    }
    return $files
}

$failures = New-Object System.Collections.Generic.List[string]

function Add-Failure([string]$msg) {
    $script:failures.Add($msg) | Out-Null
    Write-Host "  FAIL: $msg" -ForegroundColor Red
}

Write-Host "checking vendoring rules in $repoRoot" -ForegroundColor Cyan

# --- 1. no upstream reference in any manifest ---------------------------------
# A match here is a dependency edge, a [patch] target, or a workspace
# inherit. All three are forbidden. Comments that merely mention the
# provenance are acceptable in Cargo.toml, so only the non-comment part
# of a line is inspected.
Write-Host "`n[1/6] no upstream in manifests" -ForegroundColor Cyan
# Scope to crates/rar-codec, crates/rgx-core, crates/rgx-adapters-* only: rgy's own crates (rgy-name-core, rgy-discovery*, rgy-engine, rgy-proto, bins/*) are not vendored-from-upstream and would false-positive.
$scope = '[\\/]crates[\\/](?:rar-codec|rgx-core|rgx-adapters-[^\\/]+)(?:[\\/]|$)'
$manifests = @()
$manifests += Get-RgyTreeFiles 'Cargo.toml' |
    Where-Object { (Test-RgyInTree $_.FullName) -and $_.FullName -match $scope }
$manifests += Get-RgyTreeFiles 'Cargo.lock' |
    Where-Object { (Test-RgyInTree $_.FullName) -and $_.FullName -match $scope }
$manifests += Get-RgyTreeFiles 'config.toml' |
    Where-Object { $_.FullName -like '*\.cargo\*' -and (Test-RgyInTree $_.FullName) }

foreach ($f in $manifests) {
    $rel = Get-RgyRelativePath $f.FullName
    $lineNo = 0
    foreach ($line in Get-Content $f.FullName) {
        $lineNo++
        # drop comments: a full-line '#' and any trailing ' #'
        $code = ($line -split '#', 2)[0]
        if ($code -match '(?i)vendored from') {
            Add-Failure "$rel`:$lineNo references upstream source in manifest code: $($line.Trim())"
        }
    }
}
if ($failures.Count -eq 0) {
    Write-Host "  ok: $($manifests.Count) manifests clean" -ForegroundColor Green
}

# --- 2. no [patch] in .cargo/config.toml ----------------------------------
Write-Host "`n[2/6] no [patch] redirects" -ForegroundColor Cyan
$cfg = Join-Path $repoRoot '.cargo\config.toml'
if (Test-Path $cfg) {
    if ((Get-Content $cfg -Raw) -match '(?m)^\s*\[patch') {
        Add-Failure ".cargo/config.toml has a [patch] section; it could redirect a dependency to a local checkout"
    } else {
        Write-Host "  ok: no [patch] section" -ForegroundColor Green
    }
} else {
    Write-Host "  ok: no .cargo/config.toml" -ForegroundColor Green
}

# --- 3. vendored trees declare provenance ---------------------------------
# A vendored tree is covered by ONE provenance statement in its root module
# (mod.rs), not by per-file headers: the copies are kept byte-identical to
# upstream so a re-sync is a plain file overwrite, and a header on every file
# would have to be re-applied on each sync. Files that were *modified*
# after copying do carry their own header (see sevenz/graph_mode.rs) and
# are listed as exceptions below.
#
# A crate vendored whole (crates/rar-codec) has no mod.rs -- its root module
# is lib.rs, so that is accepted as the root when mod.rs is absent.
#
# So the invariant checked here is: the tree root names its upstream source.
#
# The list is the one upstream's own provenance manifest names -- the trees
# whose root module carries a tree-level statement. Upstream records them in
# `scripts/vendored-sources.tsv`, one section header per tree ("header:
# <path>"), and the five of them are the 7z adapter tree, the LZ4 adapter
# tree, the rgx-core codecs, the RAR codec and the memmap2 tree. Two of
# them were missing here until 2026-10-30, so the gate was looking at three
# of the five trees the record names.
Write-Host "`n[3/6] vendored trees declare provenance" -ForegroundColor Cyan
$vendoredRoots = @('crates\rgx-adapters-archive\src\sevenz', 'crates\rgx-adapters-archive\src\lz4', 'crates\rgx-core\src\compress', 'crates\rar-codec\src', 'crates\rgx-core\src\memmap2')
$checked = 0
foreach ($vroot in $vendoredRoots) {
    $dir = Join-Path $repoRoot $vroot
    if (-not (Test-Path $dir)) { continue }
    $rootName = $null
    foreach ($candidate in @('mod.rs', 'lib.rs')) {
        if (Test-Path (Join-Path $dir $candidate)) { $rootName = $candidate; break }
    }
    if ($null -eq $rootName) {
        Add-Failure "$vroot has no mod.rs or lib.rs to carry tree-level provenance"
        continue
    }
    $checked++
    $root = Join-Path $dir $rootName
    $head = (Get-Content $root -TotalCount 25) -join "`n"
    # A vendored tree must name its upstream source. The accepted
    # markers are `vendored from` (legacy trees) or `independent
    # reimplementation` (current trees); if neither is present the tree
    # has no provenance to audit.
    if ($head -notmatch '(?i)(vendored from|independent reimplementation)') {
        Add-Failure "$vroot\$rootName does not name its upstream source; add a tree-level provenance header (vendored from / independent reimplementation)"
    } else {
        $n = (Get-ChildItem -Path $dir -Filter '*.rs' -File).Count
        Write-Host "  ok: $vroot\$rootName declares provenance for $n files" -ForegroundColor Green
    }
}
if ($checked -eq 0) {
    Write-Host "  ok: no vendored trees configured yet" -ForegroundColor Green
}

# --- 4. no external crates ------------------------------------------------
# The zero-external-crates rule is the project's headline invariant, and
# checks 1-3 could not see it: they only look for upstream references and
# provenance headers, so any number of crates.io dependencies passed
# unnoticed. This check enumerates them.
#
# Known remaining dependencies are listed rather than simply forbidden, so
# the count is a progress metric and nobody can add a new one by forgetting.
# Each entry names what replaces it.
$knownExternal = @{
    'crossterm'         = 'the terminal UI of bins/search-ui speaks the terminal itself now: the escape sequences, the keys and the mode are declared by hand in bins/search-ui/src/term.rs, the way the clock and the filesystem are. The crate is a dev-dependency only -- the oracle the escape sequences and the key corpus are measured against'
    'hashbrown'         = 'the maps of crates/name-core and crates/discovery-windows are the tree''s own now (crates/name-core/src/hmap.rs, crates/discovery-windows/src/hmap.rs): open addressing over the crate''s Fx hasher, and the maps that were never maps -- a merge, a count, a grouping -- are the shape their data has. The crate is a dev-dependency only -- the oracle the in-house maps are measured against'
    'ignore'            = 'the pack-store builder of rgx-core walks the tree with a walker of its own (crates/rgx-core/src/tv_runtime/walk.rs), behind the tv-runtime feature -- and the default build turns that feature on, through rgy-engine''s own std feature, so the code that uses it is compiled and linted by every gate. In rgx-core the crate is a dev-dependency only -- the oracle the walker, the globs and the ignore files are measured against; the portable walk of the seam (crates/discovery) has no gitignore semantics yet'
    'mimalloc'          = 'the allocator of the three binaries; it measured 5-12% on the parallel walk on Windows'
    'regex'             = 'rgx ships its own engine (syntax + Thompson NFA + Pike VM) and that engine is the confirm in crates/name-core/src/content.rs. The product does not link the crate: a pattern the byte engine refuses is a refusal the search says. In rgx-core it is a dev-dependency only -- the oracle the capture groups are measured against (54/54 on the permanent corpus)'
    'rusqlite'          = 'the SQLite adapter of rgx-adapters-tabular reads the file format itself; the crate is a dev-dependency only -- the oracle the in-house reader is measured against, over a corpus of real databases'
}

Write-Host "`n[4/6] no unexpected external crates" -ForegroundColor Cyan

# A dependency section is any [..dependencies] table, including
# target-specific ones. Everything else is not a dependency list.
$depSection = $false
$depKind = 'linked'
$external = New-Object System.Collections.Generic.List[object]
# The whole workspace, for this check only: the question is whether the
# tree links a crate from outside it, and that is a question about every
# crate in the tree. The scope above is what check 1 needs -- rgy's own
# crates are not vendored from upstream -- and it is the wrong scope here:
# the externals of rgy's own crates would not be seen at all.
$allManifests = @()
$allManifests += Get-RgyTreeFiles 'Cargo.toml' |
    Where-Object { Test-RgyInTree $_.FullName }

foreach ($f in $allManifests) {
    if ($f.Name -ne 'Cargo.toml') { continue }
    $rel = Get-RgyRelativePath $f.FullName
    $section = ''
    $lineNo = 0
    foreach ($line in Get-Content $f.FullName) {
        $lineNo++
        $trimmed = $line.Trim()
        if ($trimmed -match '^\[+\s*([^\]]+?)\s*\]+$') {
            $section = $matches[1]
            $depSection = ($section -match '(^|\.)(dev-|build-)?dependencies$')
            # A dev-dependency is an oracle: it is not linked into a
            # shipped artifact. Everything else that names a crate is.
            $depKind = if ($section -match '(^|\.)dev-dependencies$') { 'oracle' } else { 'linked' }
            continue
        }
        if (-not $depSection) { continue }
        $code = ($trimmed -split '#', 2)[0].Trim()
        if ($code -notmatch '^([A-Za-z0-9_-]+)\s*=') { continue }
        $name = $matches[1]
        # path = ... is an in-tree crate; workspace = true inherits one.
        if ($code -match '\bpath\s*=') { continue }
        if ($code -match '\bworkspace\s*=') { continue }
        $external.Add([pscustomobject]@{ Name = $name; Where = "$rel`:$lineNo"; Kind = $depKind })
    }
}

$unexpected = New-Object System.Collections.Generic.List[object]
$seen = @{}
foreach ($d in $external) {
    if ($seen.ContainsKey($d.Name)) { continue }
    $seen[$d.Name] = $true
    if (-not $knownExternal.ContainsKey($d.Name)) {
        $unexpected.Add($d)
    }
}

if ($unexpected.Count -gt 0) {
    foreach ($d in $unexpected) {
        Add-Failure "$($d.Where): external dependency '$($d.Name)' is not in the known-remaining list; either it is in-tree (use path =) or add it with what replaces it"
    }
} else {
    Write-Host "  ok: no unlisted external crates" -ForegroundColor Green
}

# A crate that is a dev-dependency in every manifest that names it is an
# oracle: it is not linked into a shipped artifact, it is the thing an
# in-house implementation is held to. A crate that is a real dependency
# anywhere is linked, and it is the one the invariant is about. The two
# are counted apart, because the number to drive to zero is the number of
# crates that go into a product -- and a ledger that counts an oracle the
# same as a dependency cannot say that.
$linked = @{}
foreach ($d in $external) {
    if ($d.Kind -ne 'linked') { continue }
    $linked[$d.Name] = $true
}

# Report what is still outstanding, and what is only held back.
$remainingLinked = @($seen.Keys | Where-Object { $knownExternal.ContainsKey($_) -and $linked.ContainsKey($_) } | Sort-Object)
$remainingOracle = @($seen.Keys | Where-Object { $knownExternal.ContainsKey($_) -and -not $linked.ContainsKey($_) } | Sort-Object)
if ($remainingLinked.Count -gt 0) {
    Write-Host "  $($remainingLinked.Count) linked external dependenc(ies) still to remove:" -ForegroundColor Yellow
    foreach ($name in $remainingLinked) {
        Write-Host "    - $name ($($knownExternal[$name]))" -ForegroundColor Yellow
    }
} else {
    Write-Host "  zero linked external dependencies: the invariant holds" -ForegroundColor Green
}
if ($remainingOracle.Count -gt 0) {
    Write-Host "  $($remainingOracle.Count) oracle(s): a dev-dependency only -- the crate an in-house implementation is held to:" -ForegroundColor Yellow
    foreach ($name in $remainingOracle) {
        Write-Host "    - $name ($($knownExternal[$name]))" -ForegroundColor Yellow
    }
}

# --- 5. the vendored crates come from the tree --------------------------------
# The vendored crates are the upstream's crates and they go out under the
# rgy's own names now -- rgy-vendored-core, the rgy-vendored-adapters-*
# family and rgy-vendored-rar-codec -- carrying this tree's patches inside
# them, at this tree's version (0.1.9). They used to go out under the
# upstream's names, and that was a collision: the upstream publishes the
# same names, and a name and a number is one thing on the registry, so two
# publishers cannot both take it. The upstream's names are the upstream's.
#
# The dependency that names one of them carries a path, and no lock file
# resolves one of them from a registry. A dependency that names one without
# a `path` resolves to the registry's copy instead of the tree's --
# silently, and that copy is the upstream's, without one of the patches that
# live here. The tree would stop being upstream's bytes and the drift check
# would have nothing to compare.
#
# The rule is: every dependency on a vendored name carries a path, and no
# lock file resolves one of them from a registry. The scan covers every
# manifest in the tree, not only the vendored ones: the offending
# declaration would sit in rgy's own crate, not in the vendored tree.
#
# Both names are matched: the key the tree writes (`rgx-core`, the name the
# code says) and the package the registry knows (`rgy-vendored-core`).
Write-Host "`n[5/6] the vendored crates come from the tree" -ForegroundColor Cyan

$vendoredRe = '^(rgx-core|rgx-adapters-[a-z0-9-]+|rar-codec|rgy-vendored-[a-z0-9-]+)$'
$vendoredFails = New-Object System.Collections.Generic.List[string]
$vendoredCount = 0

$allManifests = @(Get-RgyTreeFiles 'Cargo.toml' |
    Where-Object { Test-RgyInTree $_.FullName })
$allLocks = @(Get-RgyTreeFiles 'Cargo.lock' |
    Where-Object { Test-RgyInTree $_.FullName })

foreach ($f in $allManifests) {
    $rel = Get-RgyRelativePath $f.FullName
    $lines = @(Get-Content $f.FullName)
    $depSection = $false
    $namedDep = $null
    $namedHasPath = $false
    $namedLine = 0
    for ($i = 0; $i -lt $lines.Count; $i++) {
        $lineNo = $i + 1
        $trimmed = $lines[$i].Trim()
        if ($trimmed -match '^\[+\s*([^\]]+?)\s*\]+$') {
            # A table header closes the table before it.
            if ($null -ne $namedDep -and -not $namedHasPath) {
                $vendoredFails.Add("$rel`:$namedLine '$namedDep' is declared without a path; a version-only dependency resolves to crates.io, not to the tree")
            }
            $namedDep = $null
            $namedHasPath = $false
            $section = $matches[1]
            $depSection = ($section -match '(^|\.)(dev-|build-)?dependencies$')
            # A named dependency table: [dependencies.rgx-core].
            if ($section -match '(^|\.)(dev-|build-)?dependencies\.(.+)$') {
                $candidate = $matches[3]
                if ($candidate -match $vendoredRe) {
                    $namedDep = $candidate
                    $namedLine = $lineNo
                    $vendoredCount++
                }
            }
            continue
        }
        if ($null -ne $namedDep) {
            if ($trimmed -match '(^|[^A-Za-z0-9_])path\s*=') { $namedHasPath = $true }
            continue
        }
        if (-not $depSection) { continue }
        $code = ($trimmed -split '#', 2)[0].Trim()
        if ($code -notmatch '^([A-Za-z0-9_-]+)\s*=') { continue }
        $name = $matches[1]
        if ($name -notmatch $vendoredRe) { continue }
        $vendoredCount++
        if ($code -notmatch '(^|[^A-Za-z0-9_])path\s*=') {
            $vendoredFails.Add("$rel`:$lineNo '$name' is declared without a path; a version-only dependency resolves to crates.io, not to the tree")
        }
    }
    # The last table in the file is closed by the end of the file.
    if ($null -ne $namedDep -and -not $namedHasPath) {
        $vendoredFails.Add("$rel`:$namedLine '$namedDep' is declared without a path; a version-only dependency resolves to crates.io, not to the tree")
    }
}

foreach ($f in $allLocks) {
    $rel = Get-RgyRelativePath $f.FullName
    $lines = @(Get-Content $f.FullName)
    $inPackage = $false
    $pkgName = ''
    $pkgSource = ''
    foreach ($line in $lines) {
        if ($line -match '^\s*\[\[package\]\]') {
            if ($inPackage -and $pkgName -match $vendoredRe -and $pkgSource -ne '') {
                $vendoredFails.Add("$rel '$pkgName' is resolved from $($pkgSource), not from the tree")
            }
            $inPackage = $true
            $pkgName = ''
            $pkgSource = ''
            continue
        }
        if (-not $inPackage) { continue }
        if ($line -match '^\s*name\s*=\s*"([^"]*)"') { $pkgName = $matches[1]; continue }
        if ($line -match '^\s*source\s*=\s*"([^"]*)"') { $pkgSource = $matches[1]; continue }
    }
    if ($inPackage -and $pkgName -match $vendoredRe -and $pkgSource -ne '') {
        $vendoredFails.Add("$rel '$pkgName' is resolved from $($pkgSource), not from the tree")
    }
}

# The failures are sorted, not printed as they are found: the two twins walk
# the tree in the same order but a reader comparing the two outputs should
# not have to know that.
$sortedFails = [string[]]$vendoredFails.ToArray()
[Array]::Sort($sortedFails, [StringComparer]::Ordinal)
foreach ($m in $sortedFails) { Add-Failure $m }
if ($vendoredFails.Count -eq 0) {
    Write-Host "  ok: $vendoredCount vendored dependency declarations, all by path" -ForegroundColor Green
    Write-Host "  ok: no lock file resolves a vendored crate from a registry" -ForegroundColor Green
}

# --- 6. the vendored manifest matches the tree --------------------------------
# scripts/vendored-sources.tsv is the record of the vendored trees: which
# upstream crate each copy came from, which commit it was taken from, and
# which local patch sits on top of it. The sync writes it, the drift check
# reads it, and nothing on the GitHub side checked that it still describes
# the tree it is about -- so a manifest that fell behind the tree reads as
# "nothing to do here", which is the one thing a manifest must never do.
#
# The questions the tree can answer:
#
#   * does every path the manifest names exist -- the trees and the patches
#     alike?
#   * does every vendored crate in crates/ have a row of its own, and every
#     patch file under scripts/rgx-local-patches/ one?
#   * is the header the one the sync writes, and does the commit it names
#     pin every row?
#
# The commit is checked for its shape -- forty hex characters, the shape of
# a git object id -- and for the rows agreeing with it. It is NOT checked
# against the upstream: this check has no upstream checkout and no way to
# ask, so "the rows agree with the header" is the whole of what it can say.
# The question "is that commit still there?" is the drift check's, and it
# runs where the upstream is.
#   * is a path the manifest excludes really absent? The sync removes those
#     on every run, so one that is here is a divergence.
#
# The paths in the messages are the manifest's own, spelled the way the
# manifest spells them, so the two twins print the same bytes.
Write-Host "`n[6/6] the vendored manifest matches the tree" -ForegroundColor Cyan

$manifestFails = New-Object System.Collections.Generic.List[string]
$manifestTrees = New-Object System.Collections.Generic.List[string]
$manifestPatches = New-Object System.Collections.Generic.List[string]
$pinPaths = New-Object System.Collections.Generic.List[string]
$pinValues = New-Object System.Collections.Generic.List[string]
$headerPin = ''

$manifestFile = Join-Path $repoRoot 'scripts/vendored-sources.tsv'
if (-not (Test-Path $manifestFile)) {
    $manifestFails.Add('scripts/vendored-sources.tsv is missing; the sync writes it')
} else {
    $manifestLines = @(Get-Content $manifestFile)
    # The rows: tab separated, five columns. A comment line is not a row,
    # and a blank line is not a row either.
    foreach ($line in $manifestLines) {
        if ($line.StartsWith('#')) { continue }
        if ([string]::IsNullOrWhiteSpace($line)) { continue }
        # The shell twin's split, spelled out: a run of tabs is one
        # separator and the ends do not count. PowerShell's own -split
        # keeps the empty fields, and the two twins then read a malformed
        # row differently -- one as a row of an unknown kind, the other as
        # no row at all.
        $cols = @($line -split '\t+' | Where-Object { $_ -ne '' })
        # A line with fewer than four columns is not a row, and the shell
        # twin says so with the same words.
        if ($cols.Count -lt 4) {
            $manifestFails.Add('scripts/vendored-sources.tsv has a row with fewer than four columns')
            continue
        }
        $rgx = $cols[0]
        $pin = $cols[2]
        $kind = $cols[3]
        switch ($kind) {
            'tree' { $manifestTrees.Add($rgx) }
            'patch' { $manifestPatches.Add($rgx) }
            default { $manifestFails.Add("scripts/vendored-sources.tsv has a row of an unknown kind: $kind") }
        }
        $pinPaths.Add($rgx)
        $pinValues.Add($pin)
    }

    # The header the sync writes. The first line says what the file is, and
    # the commit line says what the copy was taken from; every row carries
    # that same commit, because the sync takes the whole vendoring from one
    # commit.
    $headerFirst = ''
    if ($manifestLines.Count -gt 0) { $headerFirst = $manifestLines[0] }
    foreach ($line in $manifestLines) {
        if ($line -match '^# The upstream commit the copy was taken from: (\S+)') {
            $headerPin = $matches[1]
            break
        }
    }
    if ($headerFirst -ne '# Vendored-source manifest. Read by scripts/check-drift.*, which asks') {
        $manifestFails.Add('scripts/vendored-sources.tsv does not start with the header the sync writes')
    }
    if ([string]::IsNullOrEmpty($headerPin)) {
        $manifestFails.Add('scripts/vendored-sources.tsv does not name the commit the copy was taken from')
    } elseif ($headerPin -notmatch '^[0-9a-f]{40}$') {
        $manifestFails.Add("scripts/vendored-sources.tsv names a commit that is not a commit id: $headerPin")
    }

    # Every tree the manifest names is in the tree.
    foreach ($rgx in $manifestTrees) {
        if (-not (Test-Path (Join-Path $repoRoot $rgx) -PathType Container)) {
            $manifestFails.Add("scripts/vendored-sources.tsv names a tree that is not in the tree: $rgx")
        }
    }

    # Every vendored crate in the tree has a row of its own. The names are
    # upstream's crates: rgx-core, rar-codec and the rgx-adapters-* family.
    # rgy's own crates are not vendored and are not in here.
    $crateDirs = @(Get-ChildItem -Path (Join-Path $repoRoot 'crates') -Directory |
        Where-Object { $_.Name -like 'rgx-*' -or $_.Name -eq 'rar-codec' })
    foreach ($d in $crateDirs) {
        $rel = "crates/$($d.Name)"
        if (-not $manifestTrees.Contains($rel)) {
            $manifestFails.Add("$rel is a vendored tree and the manifest does not name it")
        }
    }

    # Every patch the manifest names is in the tree.
    foreach ($rgx in $manifestPatches) {
        if (-not (Test-Path (Join-Path $repoRoot $rgx) -PathType Leaf)) {
            $manifestFails.Add("scripts/vendored-sources.tsv names a patch that is not in the tree: $rgx")
        }
    }

    # Every patch file in the tree has a row of its own. A patch the
    # manifest does not name is a divergence nobody wrote down, and the
    # sync would apply it without the manifest saying so.
    $patchFiles = @(Get-ChildItem -Path (Join-Path $repoRoot 'scripts/rgx-local-patches') -Recurse -File -Filter '*.patch' -ErrorAction SilentlyContinue)
    foreach ($p in $patchFiles) {
        $rel = $p.FullName.Substring($repoRoot.Length + 1).Replace('\', '/')
        if (-not $manifestPatches.Contains($rel)) {
            $manifestFails.Add("$rel is a patch and the manifest does not name it")
        }
    }

    # Every row is pinned to the commit the header names.
    for ($i = 0; $i -lt $pinPaths.Count; $i++) {
        if ($headerPin -ne '' -and $pinValues[$i] -ne $headerPin) {
            $manifestFails.Add("scripts/vendored-sources.tsv pins $($pinPaths[$i]) to $($pinValues[$i]), and the header names $headerPin")
        }
    }

    # The paths the manifest excludes are upstream's files this repository
    # does not carry. The sync removes them on every run, so one that is
    # here is a divergence from the rule the manifest states.
    foreach ($line in $manifestLines) {
        if ($line -match '^# exclude: (.+)$') {
            $ex = $matches[1].Trim()
            if (Test-Path (Join-Path $repoRoot $ex)) {
                $manifestFails.Add("scripts/vendored-sources.tsv excludes $ex, and the tree has it")
            }
        }
    }
}

# The failures are sorted, not printed as they are found: the two twins walk
# the tree in the same order but a reader comparing the two outputs should
# not have to know that.
$sortedManifestFails = [string[]]$manifestFails.ToArray()
[Array]::Sort($sortedManifestFails, [StringComparer]::Ordinal)
foreach ($m in $sortedManifestFails) { Add-Failure $m }
if ($manifestFails.Count -eq 0) {
    Write-Host "  ok: $($manifestTrees.Count) vendored trees and $($manifestPatches.Count) patches, all pinned to $headerPin" -ForegroundColor Green
}

# --- result ----------------------------------------------------------------
Write-Host ""
if ($failures.Count -gt 0) {
    Write-Host "vendoring check FAILED: $($failures.Count) violation(s)" -ForegroundColor Red
    exit 1
}
Write-Host "vendoring check passed" -ForegroundColor Green
exit 0
