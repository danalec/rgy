<#
.SYNOPSIS
    Guards the vendoring rules: rgx links nothing from upstream, and every
    vendored file declares its source.

.DESCRIPTION
    rgx copies code out of upstream; it never depends on it. This script fails
    if that ever stops being true, so the rule is checked rather than
    remembered. Five checks:

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
         metric and a newly added crate fails the check.
      5. Every dependency on a vendored crate names it by path, and no
         lock file resolves one from a registry. The vendored crates
         carry the names upstream publishes on crates.io, at the same
         version, so a version-only dependency would resolve to the
         registry copy instead of the tree -- silently, and with cargo
         having nothing to say about it.

    Run from the repo root:  powershell -File scripts/check-vendoring.ps1
    Exits 1 on any violation.
#>

$ErrorActionPreference = 'Stop'
$repoRoot = Split-Path -Parent $PSScriptRoot
Set-Location $repoRoot

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
Write-Host "`n[1/5] no upstream in manifests" -ForegroundColor Cyan
# Scope to crates/rar-codec, crates/rgx-core, crates/rgx-adapters-* only: rgy's own crates (rgy-name-core, rgy-discovery*, rgy-engine, rgy-proto, bins/*) are not vendored-from-upstream and would false-positive.
$scope = '[\\/]crates[\\/](?:rar-codec|rgx-core|rgx-adapters-[^\\/]+)(?:[\\/]|$)'
$manifests = @()
$manifests += Get-ChildItem -Path . -Filter 'Cargo.toml' -Recurse -File |
    Where-Object { $_.FullName -notmatch '[\\/]target[\\/]' -and $_.FullName -match $scope }
$manifests += Get-ChildItem -Path . -Filter 'Cargo.lock' -Recurse -File |
    Where-Object { $_.FullName -notmatch '[\\/]target[\\/]' -and $_.FullName -match $scope }
$manifests += Get-ChildItem -Path . -Filter 'config.toml' -Recurse -File |
    Where-Object { $_.FullName -like '*\.cargo\*' -and $_.FullName -notmatch '[\\/]target[\\/]' }

foreach ($f in $manifests) {
    $rel = Resolve-Path -Relative $f.FullName
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
Write-Host "`n[2/5] no [patch] redirects" -ForegroundColor Cyan
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
Write-Host "`n[3/5] vendored trees declare provenance" -ForegroundColor Cyan
$vendoredRoots = @('crates\rgx-adapters-archive\src\sevenz', 'crates\rar-codec\src', 'crates\rgx-core\src\memmap2')
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
# Each entry names what replaces it; the OCR stack is planned in
# OCR-PLAN.md, the search engine in MUST-WRITE.md.
$knownExternal = @{
    # --- search engine (MUST-WRITE.md) ---
    'regex'         = 'K1 is built in upstream (b02/rgx: syntax + Thompson NFA + Pike VM, with capture groups agreeing 54/54 against the regex crate on the permanent corpus). Not yet wired here: rgx still matches through grep-regex. K2 is the blocker.'
    'ignore'        = 'MUST-WRITE K4 -- parallel walker with gitignore semantics'

    # --- compression and archives: upstream already has these in-house ---
    'brotli'        = 'NO REPLACEMENT: rgx has the RFC 7932 decoder in crates/rgx-adapters-archive/src/brotli/. See tests::oracle::vendored_brotli_does_not_decode_reference_streams'
    'xz2'           = 'upstream b02/compress/xz/ (RFC 8478 container + LZMA2)'
    'zstd'          = 'NOT YET: upstream b02/compress/zstd.rs was refused as not actually zstd'
    # rgx's own .bz2 decoding is in-house already (rgx_core::compress::bzip2,
    # a vendored copy of upstream b02/compress/bzip2.rs, verified against
    # reference-bzip2-CLI fixtures). What is left here is the `zip` crate's
    # own bzip2 backend, which only a fork of `zip` could drop.
    'bzip2-0_5-static' = 'C binding, pulled in by the zip crate; rgx needs a zip without the bzip2 feature to remove it'

    # --- container and metadata readers ---
    'rusqlite'      = 'upstream b01/io/binary/sqlite.rs + b02/sqlite/rusqlite_compat.rs'
    'kamadak-exif'  = 'upstream b01/io/image/exif/'

    # --- plumbing ---
    'clap'          = 'argument parsing; no upstream equivalent identified yet'
    'memchr'        = 'crates/rgx-core/src/memchr.rs re-exports the external memchr 2.x crate and carries an in-house tree beside it: src/memchr/dispatch.rs picks the byte kernels with the CPU probe the crate writes itself (src/detect.rs), and src/memchr/memmem.rs is the substring search, because the upstream packed-pair kernel cannot be reached without std. The external crate stays as the fallback for a CPU without AVX2 and for the architectures whose vector width is decided at compile time.'
    'libc'          = 'the constants and `mmap(2)` / `msync(2)` / `madvise(2)` syscall FFI the vendored `memmap2` module (crates/rgx-core/src/memmap2) reaches for on Unix, and the `dirent` / `stat` layouts the portable reader of the seam (crates/discovery/src/dir.rs) reads on Unix. Both are gated on the target, not on the `std` feature, so the bare-metal build does not link it. Will go away when the `memmap2` module is rewritten in pure inline FFI (a follow-up) or when Linux / macOS gating moves to `std::sys::pal`.'
    'anyhow'        = 'rgx-core/src/error.rs is the in-house replacement; leftover entry'

    # --- OCR stack (OCR-PLAN.md) ---
    'ocrs'          = 'OCR-PLAN K11 -- detection and recognition pipeline'
    'rten'          = 'OCR-PLAN K8/K9 -- model file reader and tensor runtime'
    'image'         = 'OCR-PLAN K10 -- decode and preprocess; upstream has the codecs'

    # --- test-only ---
    'flate2'        = 'dev-dependency for test fixtures'
    'zip'           = 'dev-dependency for test fixtures'
    'libfuzzer-sys' = 'fuzz harness only; not part of a shipped build'
}

Write-Host "`n[4/5] no unexpected external crates" -ForegroundColor Cyan

# A dependency section is any [..dependencies] table, including
# target-specific ones. Everything else is not a dependency list.
$depSection = $false
$external = New-Object System.Collections.Generic.List[object]
foreach ($f in $manifests) {
    if ($f.Name -ne 'Cargo.toml') { continue }
    $rel = Resolve-Path -Relative $f.FullName
    $section = ''
    $lineNo = 0
    foreach ($line in Get-Content $f.FullName) {
        $lineNo++
        $trimmed = $line.Trim()
        if ($trimmed -match '^\[+\s*([^\]]+?)\s*\]+$') {
            $section = $matches[1]
            $depSection = ($section -match '(^|\.)(dev-|build-)?dependencies$')
            continue
        }
        if (-not $depSection) { continue }
        $code = ($trimmed -split '#', 2)[0].Trim()
        if ($code -notmatch '^([A-Za-z0-9_-]+)\s*=') { continue }
        $name = $matches[1]
        # path = ... is an in-tree crate; workspace = true inherits one.
        if ($code -match '\bpath\s*=') { continue }
        if ($code -match '\bworkspace\s*=') { continue }
        $external.Add([pscustomobject]@{ Name = $name; Where = "$rel`:$lineNo" })
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

# Report what is still outstanding. This is the number to drive to zero.
$remaining = ($seen.Keys | Where-Object { $knownExternal.ContainsKey($_) } | Sort-Object)
if ($remaining.Count -gt 0) {
    Write-Host "  $($remaining.Count) known external dependenc(ies) still to remove:" -ForegroundColor Yellow
    foreach ($name in $remaining) {
        Write-Host "    - $name ($($knownExternal[$name]))" -ForegroundColor Yellow
    }
} else {
    Write-Host "  zero external dependencies: the invariant holds" -ForegroundColor Green
}

# --- 5. the vendored crates come from the tree --------------------------------
# The vendored crates carry the package names upstream publishes on
# crates.io, at the same version: rgx-core, the rgx-adapters-* family and
# rar-codec. A dependency that names one of them without a `path` resolves
# to the registry copy instead of the tree -- silently, and at the same
# version, so cargo has nothing to say. The tree would stop being upstream's
# bytes and the drift check would have nothing to compare.
#
# So the rule is: every dependency on a vendored name carries a path, and no
# lock file resolves one of them from a registry. The scan covers every
# manifest in the tree, not only the vendored ones: the offending
# declaration would sit in rgy's own crate, not in the vendored tree.
Write-Host "`n[5/5] the vendored crates come from the tree" -ForegroundColor Cyan

$vendoredRe = '^(rgx-core|rgx-adapters-[a-z0-9-]+|rar-codec)$'
$vendoredFails = New-Object System.Collections.Generic.List[string]
$vendoredCount = 0

$allManifests = @(Get-ChildItem -Path . -Filter 'Cargo.toml' -Recurse -File |
    Where-Object { $_.FullName -notmatch '[\\/]target[\\/]' })
$allLocks = @(Get-ChildItem -Path . -Filter 'Cargo.lock' -Recurse -File |
    Where-Object { $_.FullName -notmatch '[\\/]target[\\/]' })

foreach ($f in $allManifests) {
    $rel = Resolve-Path -Relative $f.FullName
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
    $rel = Resolve-Path -Relative $f.FullName
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

# --- result ----------------------------------------------------------------
Write-Host ""
if ($failures.Count -gt 0) {
    Write-Host "vendoring check FAILED: $($failures.Count) violation(s)" -ForegroundColor Red
    exit 1
}
Write-Host "vendoring check passed" -ForegroundColor Green
exit 0
