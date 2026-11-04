# test-publish-release.ps1 -- the two checks the publish script makes on
# the release body and on the release page.
#
# The Windows counterpart of scripts/test-publish-release.sh.
#
#   pwsh -File scripts/test-publish-release.ps1
#   bash scripts/test-publish-release.sh
#
# Two checks live here, and both of them are checks the publish script
# makes before it publishes anything.
#
# The first one is the escaping of the release body. The body is a JSON
# string, and the escaping of it used to run on the first line only: the
# body that was POSTed kept the raw quotes of every line but the first,
# so it was not JSON, and the forgejo answered 400. The publish scripts
# carry a check for that now -- the escaped body is parsed before
# anything is sent -- but that check only runs when somebody publishes,
# and the break it was written for survived a long time with nothing
# testing it.
#
# The second one is the comparison between the release page and the
# assets of the release. The page names the whole release, and a publish
# compares the two both ways: the sets have to be equal. A run that
# rewrites the manifests and publishes nothing compares them one way
# only -- the release is built one row at a time, and a row carries one
# archive of it -- and that difference is the one the CI found: the two
# modes answer different questions, and one sentence cannot answer both.
#
# So the checks are here, on their own, and they ask the same questions
# on every push. The helpers are taken OUT of
# scripts/publish-release.ps1 -- the code that publishes, and not a copy
# of it -- and the body is the tree's own: a quote, a backslash and a
# newline, the three a release body can carry.
#
#   1. is the escaped body a JSON string a parser accepts?
#   2. is the parsed body the body that went in?
#   3. are the bytes the ones the twin produces?
#   4. would the check have refused the old escaping?
#   5. does a publish refuse a page that does not match the release, and
#      accept one that does?
#   6. does a manifests-only run refuse an archive the page does not
#      name, and accept a page that names the other rows' assets?
#
# The exit code is 0 when the six answers are yes and 1 when one of them
# is not, and the line that says which one is the line a human reads.
#
# The scripts find what they need next to themselves -- the publish
# script and the digest helper -- and they write nothing into the
# repository: the work happens in a directory of their own, and it goes
# away with them.
$ErrorActionPreference = "Stop"
if (Get-Variable -Name PSNativeCommandUseErrorActionPreference -ErrorAction SilentlyContinue) {
    $PSNativeCommandUseErrorActionPreference = $false
}

$here = $PSScriptRoot
$publish = Join-Path $here "publish-release.ps1"

# The options, read by the helper the two twins share: this script takes
# none, so every argument is an error — the same words and the same exit
# code on both platforms. A test that ignores what it does not understand
# runs the whole test when the caller asked for something else.
. (Join-Path $here "lib/options.ps1")
Read-RgyOptions -Switches @{} -Values @{} -Positionals @{} -Arguments $args

function Stop-WithFailure([string]$message) {
    # The line the shell twin writes, on the stream the shell twin writes
    # it on: the two are compared byte for byte.
    [Console]::Error.Write("test-publish-release: FAILED: $message`n")
    exit 1
}

$work = Join-Path ([IO.Path]::GetTempPath()) ("rgy-test-publish-release-" + [guid]::NewGuid().ToString("N"))

try {
    New-Item -ItemType Directory -Force -Path $work | Out-Null

    [Console]::Out.Write("test-publish-release: the escaping of the release body`n")

    # The body: the tree's own release body, in the shape it has on the
    # release page. The quote and the backslash are the two the escaping
    # has to carry, and the lines are the newlines it has to join.
    $body = @'
Whole-filesystem name search and content search in one stack.

**Which file?** -- asked over the whole filesystem, then **what is inside it?** in the same query.

    searchctl --root ~ "invoice ext:pdf"
    searchctl --root ~ "invoice grep:tesla"

The release carries four binaries: ``searchctl`` (the command-line tool),
``searchd`` (the daemon: JSON lines over stdio, a pipe or a unix socket),
``rgy-autostart`` (put the daemon in place at login) and ``rgxy`` (the
launcher over the daemon: a floating window where there is a desktop, the
terminal where there is not).

A Windows path is a backslash too: C:\Users\rgy\searchctl.exe

**Assets**
- ``rgy-v0.1.2-x86_64-linux.tar.gz``
- ``SHA256SUMS``

Apache-2.0.
'@
    # A checkout that rewrote the line endings would put a carriage
    # return in the body, and a carriage return is not JSON: the body is
    # normalised to the line ending the shell twin writes.
    $body = $body -replace "`r`n", "`n"

    # The escaping helper, taken out of the publish script: the functions
    # are used as they are there, and a copy of them is what would drift.
    # The parser of this platform finds them -- the twin of the shell
    # script's sed -- and a helper that is not there is a failure, not a
    # helper that answers nothing.
    $tokens = $null
    $parseErrors = $null
    $ast = [System.Management.Automation.Language.Parser]::ParseFile($publish, [ref]$tokens, [ref]$parseErrors)
    if ($parseErrors.Count -gt 0) {
        Stop-WithFailure "scripts/publish-release.ps1 does not parse"
    }
    $escapeHelper = $ast.Find({
        param($node)
        $node -is [System.Management.Automation.Language.FunctionDefinitionAst] -and
            $node.Name -eq "ConvertTo-JsonString"
    }, $true)
    if (-not $escapeHelper) {
        Stop-WithFailure "the escaping helper was not found in scripts/publish-release.ps1"
    }
    $checkHelper = $ast.Find({
        param($node)
        $node -is [System.Management.Automation.Language.FunctionDefinitionAst] -and
            $node.Name -eq "Test-JsonText"
    }, $true)
    if (-not $checkHelper) {
        Stop-WithFailure "the check the publish script uses was not found in scripts/publish-release.ps1"
    }
    Invoke-Expression $escapeHelper.Extent.Text
    Invoke-Expression $checkHelper.Extent.Text
    . (Join-Path $here "lib/digest.ps1")

    $escaped = ConvertTo-JsonString $body

    # 1. the escaped body is a JSON string a parser accepts: the parser
    #    this platform has, and the check the publish script itself uses.
    $json = '"' + $escaped + '"'
    try {
        $null = $json | ConvertFrom-Json
    } catch {
        Stop-WithFailure "the escaped body is not valid JSON"
    }
    if (-not (Test-JsonText $json)) {
        Stop-WithFailure "the escaped body is not valid JSON"
    }
    [Console]::Out.Write("ok: the escaped body is valid JSON`n")

    # 2. the parsed body is the body that went in.
    $parsed = $json | ConvertFrom-Json
    if ($parsed -cne $body) {
        Stop-WithFailure "the parsed body is not the body that went in"
    }
    [Console]::Out.Write("ok: the parsed body is the body that went in`n")

    # 3. the bytes are the ones the twin produces. The digest below is
    #    the same constant in the two twins: it is what a reference JSON
    #    serializer writes for this body, and each twin has to arrive at
    #    it. So the two twins produce the same bytes, and a twin that
    #    escapes differently is caught here.
    $escapedPath = Join-Path $work "escaped.json"
    [IO.File]::WriteAllText($escapedPath, $escaped, (New-Object Text.UTF8Encoding $false))
    $digest = Get-RgySha256 $escapedPath
    if ($digest -cne "ca28e40ba5921cc6f7d59454651c69f88bf50ea55ae5894c3a05e7c1165e948f") {
        Stop-WithFailure "the escaped body is not the bytes the twin produces"
    }
    [Console]::Out.Write("ok: the escaped body is the bytes the twin produces`n")

    # 4. the old escaping -- the one that escaped the first line only --
    #    is refused by the check. This is the regression: a check that
    #    accepts the old output catches nothing, and the break this test
    #    is written for is the one that check has to catch.
    $lines = $body -split "`n"
    $old = (ConvertTo-JsonString $lines[0]) + "`n" + ($lines[1..($lines.Count - 1)] -join "`n")
    if (Test-JsonText ('"' + $old + '"')) {
        Stop-WithFailure "the old escaping was not refused by the check"
    }
    $accepted = $false
    try {
        $null = ('"' + $old + '"') | ConvertFrom-Json
        $accepted = $true
    } catch {
        $accepted = $false
    }
    if ($accepted) {
        Stop-WithFailure "the old escaping was not refused by the check"
    }
    [Console]::Out.Write("ok: the old escaping is refused by the check`n")

    # 5. the assets check of a publish. The page names the whole release,
    #    and a publish compares the page with the release both ways: the
    #    two sets have to be equal, and a page that names an archive the
    #    release does not carry -- and a release that carries an archive
    #    the page does not name -- stop the publish.
    #
    # The helper is taken out of the publish script, the way the escaping
    # helper is: a copy of it is what would drift.
    $namesHelper = $ast.Find({
        param($node)
        $node -is [System.Management.Automation.Language.FunctionDefinitionAst] -and
            $node.Name -eq "Join-Names"
    }, $true)
    if (-not $namesHelper) {
        Stop-WithFailure "the helper the assets check uses was not found in scripts/publish-release.ps1"
    }
    $assetsHelper = $ast.Find({
        param($node)
        $node -is [System.Management.Automation.Language.FunctionDefinitionAst] -and
            $node.Name -eq "Get-ReleaseAssetsRefusal"
    }, $true)
    if (-not $assetsHelper) {
        Stop-WithFailure "the assets check was not found in scripts/publish-release.ps1"
    }
    Invoke-Expression $namesHelper.Extent.Text
    Invoke-Expression $assetsHelper.Extent.Text

    $publishRefusal = Get-ReleaseAssetsRefusal "publish" "docs/release-0.1.2.md" "rgy-v0.1.2-x86_64-linux.tar.gz" "rgy-v0.1.2-avx2-windows.zip"
    $publishExpected = "the release page does not match the assets of this release: the page names [rgy-v0.1.2-x86_64-linux.tar.gz] the release does not carry, and the release carries [rgy-v0.1.2-avx2-windows.zip] the page does not name"
    if ($publishRefusal -cne $publishExpected) {
        Stop-WithFailure "the publish mode did not refuse a page that does not match the release"
    }
    if (Get-ReleaseAssetsRefusal "publish" "docs/release-0.1.2.md" "" "") {
        Stop-WithFailure "the publish mode refused a page that matches the release"
    }
    [Console]::Out.Write("ok: the publish mode refuses a page that does not match the release`n")

    # 6. the assets check of a run that rewrites the manifests and
    #    publishes nothing. The page names the whole release and the run
    #    carries one archive of it, so the page may name assets the run
    #    does not have -- the other rows' assets -- and that is not a
    #    refusal. An archive the run carries and the page does not name
    #    is one: the release does not carry it either, and the sentence
    #    says so in its own words.
    $rowRefusal = Get-ReleaseAssetsRefusal "manifests-only" "docs/release-0.1.2.md" "" "rgy-v0.1.2-avx2-windows.zip"
    $rowExpected = "the release page does not name the assets of this run: this run carries [rgy-v0.1.2-avx2-windows.zip] the page does not name" + "`n" +
                   "  the assets marker in docs/release-0.1.2.md names the whole release, and every archive this run carries has to be in it" + "`n" +
                   "  add the name to the marker and run this again"
    if ($rowRefusal -cne $rowExpected) {
        Stop-WithFailure "the manifests-only mode did not refuse an asset the page does not name"
    }
    if (Get-ReleaseAssetsRefusal "manifests-only" "docs/release-0.1.2.md" "rgy-v0.1.2-x86_64-windows.zip" "") {
        Stop-WithFailure "the manifests-only mode refused a page that names the other rows' assets"
    }
    [Console]::Out.Write("ok: the manifests-only mode refuses an asset the page does not name`n")

    [Console]::Out.Write("escaped body sha256: $digest`n")
    [Console]::Out.Write("test-publish-release: ok`n")
    exit 0
} finally {
    # The work goes away with the script: the tree is not a scratch
    # folder, and the next run must not find what this one left.
    if (Test-Path $work) { Remove-Item -LiteralPath $work -Recurse -Force }
}
