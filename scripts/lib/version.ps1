# The release's version, out of the tree.
#
# The manifests carry three numbers -- the registry takes three and nothing
# else -- and the release may carry a fourth, the fix count of the line. The
# fourth lives in the name of the release page and nowhere else in the tree:
# `docs/release-0.1.9.0.md` is the 0.1.9.0 release and `docs/release-0.1.9.md`
# is the 0.1.9 one. So the page the tree carries is what says which release of
# the line this tree is, and a caller that reads the manifest alone asks for a
# page the tree does not have.
#
# That is the failure this exists for: the archive job of the CI read the
# manifest, asked for `docs/release-0.1.9.md`, and every one of the eleven
# rows stopped with "the release carries docs/release-0.1.9.md and this tree
# does not have it" -- while the page the release carries was
# `docs/release-0.1.9.0.md`, the name the tag and the archives have.
#
#     Get-RgyReleaseVersion $root $base
#
# The three-number page when the tree has one, and the highest page of the
# line when it does not. The shell twin is version.sh, and the two answer the
# same way -- run as a command, either one prints the version of the tree it
# sits in, which is how the CI asks the tree instead of reading the manifest
# again.

function Get-RgyReleaseVersion([string]$root, [string]$base) {
    $three = Join-Path (Join-Path $root "docs") "release-$base.md"
    if (Test-Path $three -PathType Leaf) {
        return $base
    }
    $best = $null
    $docs = Join-Path $root "docs"
    if (Test-Path $docs -PathType Container) {
        foreach ($page in (Get-ChildItem $docs -File -Filter "release-$base.*.md")) {
            $fix = $page.Name.Substring("release-$base.".Length)
            $fix = $fix.Substring(0, $fix.Length - 3)
            if ($fix -notmatch '^[0-9]+$') { continue }
            if ($null -eq $best -or [int]$fix -gt [int]$best) { $best = $fix }
        }
    }
    if ($null -ne $best) {
        return "$base.$best"
    }
    [Console]::Error.WriteLine("the tree carries no page for ${base}: neither docs/release-${base}.md nor docs/release-${base}.<n>.md")
    return $null
}

# Run as a command, it prints the version of the tree it is in -- `pwsh -File
# scripts/lib/version.ps1` -- so a caller that has no version in hand asks the
# same file the scripts source. Dot-sourcing it defines the function and
# prints nothing.
if ($MyInvocation.InvocationName -ne ".") {
    $here = $PSScriptRoot
    $root = Split-Path (Split-Path $here -Parent) -Parent
    $manifest = Join-Path $root "bins/searchctl/Cargo.toml"
    $base = ""
    if (Test-Path $manifest -PathType Leaf) {
        $m = Select-String -Path $manifest -Pattern '^version = "([^"]+)"' | Select-Object -First 1
        if ($m) { $base = $m.Matches.Groups[1].Value }
    }
    if (-not $base) {
        [Console]::Error.WriteLine("no version in bins/searchctl/Cargo.toml")
        exit 1
    }
    $version = Get-RgyReleaseVersion $root $base
    if (-not $version) { exit 1 }
    # The value goes out on the pipeline, and not through [Console]::Out: a
    # caller that runs this file with `&` captures the pipeline, and a line
    # written straight to the console is a line the caller never sees.
    Write-Output $version
}
