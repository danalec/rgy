# publish-release.ps1 -- publish an rgy release end to end, from Windows.
#
# The Windows counterpart of scripts/publish-release.sh.
#
#   pwsh -File scripts/publish-release.ps1              # full pipeline
#   pwsh -File scripts/publish-release.ps1 -SkipTests   # trust the build
#   pwsh -File scripts/publish-release.ps1 -SkipBuild   # the release is built already
#   pwsh -File scripts/publish-release.ps1 -DryRun      # show, don't touch
#   pwsh -File scripts/publish-release.ps1 -Private     # keep the GitHub mirror private
#   pwsh -File scripts/publish-release.ps1 -Commit      # commit the release data
#   pwsh -File scripts/publish-release.ps1 -WithGithub   # publish to the GitHub mirror too
#   pwsh -File scripts/publish-release.ps1 -Variant avx2 # a tuned asset
#
# Both spellings of every option work: --dry-run is the twin's and
# -DryRun is PowerShell's, and an option neither of them knows stops the
# script instead of being ignored.
#
# PowerShell 7 (pwsh) is required: under Windows PowerShell 5.1 the
# credential request git needs does not survive the trip.
#
# Pipeline: build the Windows archive -> tag rgy-v<version> + push -> create
# or update the forgejo release and (re)upload the assets -> refresh the
# winget manifests (version, sha256, date, URLs) -> winget validate ->
# optional -Commit of the manifest changes. The GitHub release is not part
# of it: the mirror is a flat one and the mirror tool's business, and the
# switch asks for the old behaviour when a caller wants it.
#
# The forgejo API uses the same stored git credentials (`git credential
# fill`); no tokens live in this script. GitHub is driven through `gh`.
#
# ONE PLATFORM DIFFERENCE FROM THE LINUX SCRIPT, because the thing it
# touches does not exist on Linux:
#
#   the winget manifests are a Windows package manager's, and there is no
#   `winget` here to validate them. The step is skipped and says so; the
#   manifests are this script's business.
#
# Everything else the two scripts do is the same thing: the same archives,
# the same release, the same messages, the same exit code. The commit of
# the release data is in both — that data is the release's own, and it
# does not belong to one platform.
#
# The assets that go on the release page are the archives in .dist/ -- the
# ones this machine built, plus the ones the other platforms built into the
# same directory -- and the SHA256SUMS file beside them.
$ErrorActionPreference = "Stop"
# A native command that writes to stderr -- git and gh both do, when they
# have something to say -- must not turn into a terminating error here: the
# exit code is what says whether the command worked.
if (Get-Variable -Name PSNativeCommandUseErrorActionPreference -ErrorAction SilentlyContinue) {
    $PSNativeCommandUseErrorActionPreference = $false
}
# PowerShell 7 is the house shell for the scripts in this directory, and
# this one needs it: under Windows PowerShell 5.1 the credential request
# below reaches git mangled, and git answers "refusing to work with
# credential missing protocol field".
if ($PSVersionTable.PSVersion.Major -lt 7) {
    Write-Error "this script needs PowerShell 7: run it with 'pwsh -File $PSCommandPath'"
    exit 1
}

# The options, read by the helper the two twins share: the long spelling
# and the PowerShell one are the same option, and an option neither of
# them knows is an error on both platforms -- not a silent "do the real
# thing", which in this directory means publishing a release.
. (Join-Path $PSScriptRoot "lib/options.ps1")
. (Join-Path $PSScriptRoot "lib/digest.ps1")
$Version = ""
$SkipTests = $false
$Commit = $false
$Private = $false
$ManifestsOnly = $false
$DryRun = $false
$WithGithub = $false
# The release is built already. The caller built it, proved it, and the
# proof names the digests the archives have -- so this script publishes those
# bytes and does not build again. A second build here is a build of the tree
# under the chair, and the tree under the chair is shared: it may carry
# somebody's work in progress, and a release that ships it is a release whose
# bytes are in no commit.
$SkipBuild = $false
# The target the release is built for. It is the host's own when it is
# empty, and the reason it is an option at all: the build the publish
# step runs must be the build the release carries, and a matrix that
# builds twelve targets must not have the publish step build a twelfth
# one for the host and drop it into .dist/ beside the others.
$Target = ""
# The flavour the release is built for. The vocabulary is the build
# script's -- portable, avx2, znver4 -- and the publish script does not
# invent a second one: a release that names a flavour the build does
# not know is a release nobody can build. The default is the portable
# one, so a caller that says nothing gets what it got before this
# option existed.
$Variant = "portable"
Read-RgyOptions -Switches @{ "skiptests" = "SkipTests"; "dryrun" = "DryRun";
                             "private" = "Private"; "manifestsonly" = "ManifestsOnly";
                             "commit" = "Commit"; "withgithub" = "WithGithub";
                             "skipbuild" = "SkipBuild" } `
                 -Values   @{ "version" = "Version"; "target" = "Target";
                             "variant" = "Variant" } `
                 -Positionals @{} `
                 -Arguments $args

# The flavour, checked here and not by the build: the build's refusal
# would come out of a second script, and the two twins have to answer
# the same way. The sentence and the exit code are the build's own.
#
# The comparison is the case-sensitive one (-cne): the unix twin's case
# statement is case-sensitive too, and "AVX2" is a flavour the build
# does not know.
if ($Variant -cne "portable" -and $Variant -cne "avx2" -and $Variant -cne "znver4") {
    # The newline the shell writes, and not the one the host would: the
    # two scripts are compared byte for byte, and a CRLF here is a byte
    # the bash twin does not write.
    [Console]::Error.Write("unknown variant: $Variant (portable, avx2, znver4)`n")
    exit 2
}

$root = Split-Path $PSScriptRoot -Parent
Set-Location $root

function Get-ManifestVersion([string]$path) {
    $m = Select-String -Path $path -Pattern '^version = "([^"]+)"' | Select-Object -First 1
    if ($m) { return $m.Matches.Groups[1].Value }
    return ""
}

if (-not $Version) { $Version = Get-ManifestVersion "bins/searchctl/Cargo.toml" }
if (-not $Version) {
    # A refusal is a sentence on stderr and an exit code, and never a
    # `throw`: an unhandled one prints an error record with this script's
    # path, its line number and the offending source line on top of the
    # message -- three extra lines the unix twin does not write, and the
    # two are compared byte for byte.
    [Console]::Error.WriteLine("no version in bins/searchctl/Cargo.toml")
    exit 1
}
$tag = "rgy-v$Version"

# The forgejo that hosts the release. Its address is not in this repository:
# a public repository that names somebody's private network is a map of that
# network, and the address is one line away from being read by everybody.
# Set RGY_FORGEJO_URL to the forgejo's base URL.
$forgejoUrl = $env:RGY_FORGEJO_URL
if (-not $forgejoUrl) {
    [Console]::Error.WriteLine("RGY_FORGEJO_URL is not set: the forgejo that hosts the release is not in this repository")
    [Console]::Error.WriteLine("set it to the forgejo's base URL, e.g. RGY_FORGEJO_URL=http://host:3000")
    exit 1
}
$forgejoUrl = $forgejoUrl.TrimEnd("/")
$repoApi = "$forgejoUrl/api/v1/repos/danalec/rgy"
$forgejo = "$forgejoUrl/danalec/rgy"
# A credential request names a host, not a URL: `host:port`, with neither
# the scheme nor the path.
if ($forgejoUrl -match '^([a-zA-Z][a-zA-Z0-9+.-]*)://') { $forgejoProto = $Matches[1] } else { $forgejoProto = "http" }
$forgejoHost = ($forgejoUrl -replace '^[a-zA-Z][a-zA-Z0-9+.-]*://', '') -replace '/.*$', ''
$ghRepo = "danalec/rgy"
$gh = "https://github.com/$ghRepo"

# --- 1. build ---------------------------------------------------------------
# The build the release carries, for the target and the flavour the
# release carries. The target is passed on so a matrix that builds
# twelve targets does not have the publish step build a thirteenth one
# for the host and drop it into .dist/ beside the others; the flavour is
# passed on for the same reason, so a row that asked for a tuned asset
# does not publish the portable one beside it.
#
# The arguments go over as plain arguments, and not as a hashtable
# splat: build-release.ps1 has no param block and reads its options
# through the shared helper, and a splat reaches it as "-Version:" and
# a bare "0.1.1" -- the helper reads the colon form as an option with
# an empty value and then refuses the value as an option it does not
# know ("unknown option: 0.1.1"), and the publish stops at its first
# step. An array splat is the other thing: it passes each element as
# its own argument, which is what the helper reads.
$buildArgs = @("-Version", $Version, "-Variant", $Variant)
if ($Target) { $buildArgs += @("-Target", $Target) }
if ($SkipTests) { $buildArgs += "-SkipTests" }
# THE DRY RUN DOES NOT BUILD.
#
# A dry run that rebuilds the release rewrites the very bytes it is
# describing: the archives move, and the digests the installer pins name move
# with them. A release that was proved against one set of digests and then
# published with another is a release nobody can install, and the proof that
# let it through was the thing that broke it.
#
# So the dry run reads the .dist as it is and says what it would publish with
# it, and it says that the build was not run -- the reader is told what was
# NOT done, the way the last line of this script already does.
#
# The publish itself still builds: the release that goes out is built from
# the tree at the moment it goes out -- unless the caller says the release is
# built already, which is what the release chain does: it built the release,
# proved it, and the proof names the digests the archives have.
if ($DryRun) {
    Write-Host '[dry-run] the build was not run: the .dist is what it is'
}
elseif ($SkipBuild) {
    Write-Host 'the build was not run: the .dist is what it is'
}
else {
    # A PowerShell script call does not set `$LASTEXITCODE` -- only a native
    # command does -- and a script that ends normally leaves whatever the last
    # native command inside it left, which can be non-zero even when the build
    # succeeded. So the exit code is reset before the call, the success flag is
    # read right after it, and the build script sets its own when it is done.
    $global:LASTEXITCODE = 0
    & (Join-Path $PSScriptRoot "build-release.ps1") @buildArgs
    $buildSucceeded = $?
    if ($LASTEXITCODE -ne 0 -or -not $buildSucceeded) {
        # The unix twin's sentence and the unix twin's exit code: a `throw`
        # would print an error record with this script's path and line
        # number on top of it, and the two scripts have to say the same
        # thing about the same failure. The newline is the shell's.
        #
        # And this is the rule for every refusal in this script, not only for
        # this one: a sentence on stderr and an exit code, and never a
        # `throw`. The two scripts are compared byte for byte, and three
        # extra lines of error record are three lines the unix twin does not
        # write.
        [Console]::Error.Write("the build failed`n")
        exit 1
    }
}

# The assets: the archives this release carries. The list is explicit
# because .dist/ also holds the working files of whoever built the release,
# and a glob over it would publish all of that to the world.
# The order is the same on every machine: the digests file first, then the
# archives by name, byte by byte. A locale's order is a machine's habit, not
# the release's order — and the release body lists these names, so the body
# has to be the same wherever it is built.
function Sort-ByNameOrdinal($items) {
    $list = [System.Collections.Generic.List[object]]::new()
    foreach ($item in $items) {
        $at = 0
        while ($at -lt $list.Count -and
               [System.StringComparer]::Ordinal.Compare($list[$at].Name, $item.Name) -le 0) {
            $at++
        }
        $list.Insert($at, $item)
    }
    return $list
}

# The same order, for the names themselves: the byte order, and not a
# locale's. The unix twin sorts its lists with LC_ALL=C, and the two have
# to compare the page's assets and the release's assets the same way.
function Sort-NameOrdinal([string[]]$names) {
    $list = [System.Collections.Generic.List[string]]::new()
    foreach ($name in $names) {
        $at = 0
        while ($at -lt $list.Count -and
               [System.StringComparer]::Ordinal.Compare($list[$at], $name) -le 0) {
            $at++
        }
        $list.Insert($at, $name)
    }
    return $list
}

# The names of a list in a sentence, and "none" when the list is empty: the
# refusal reads the same either way, and the unix twin says the same word.
function Join-Names([string[]]$names) {
    if (-not $names -or $names.Count -eq 0) { return "none" }
    return ($names -join ", ")
}
$assets = @()
$sums = Get-Item ".dist\SHA256SUMS" -ErrorAction SilentlyContinue
if ($sums) { $assets += $sums }
$assets += @(Sort-ByNameOrdinal (Get-ChildItem ".dist" -File -ErrorAction SilentlyContinue |
    Where-Object { $_.Name -like "rgy-v$Version-*" }))
if (-not $assets) {
    # The unix twin's two lines, in the unix twin's words: the sentence
    # names the build script that fills .dist/, and the path is the unix
    # twin's -- the two are compared byte for byte, and a path spelled the
    # host's way is a line the other one does not have.
    [Console]::Error.WriteLine("no assets to publish: .dist/ holds nothing for $tag")
    [Console]::Error.WriteLine("run scripts/build-release.sh first")
    exit 1
}

# --- 2. the assets are what the build digested ------------------------------
# An asset whose digest does not match the digest the build wrote is an
# asset nobody can check, and a release that serves it is worse than no
# release. The file the build left behind is the authority. This runs
# before anything is published, because after the first upload the damage
# is done.
$sumsPath = ".dist\SHA256SUMS"
if (Test-Path $sumsPath) {
    # The count of the rows is kept with the digest: the unix twin names
    # the archive AND the number of times it is named, and the two have to
    # say the same thing about the same file.
    $digests = @{}
    $digestsTimes = @{}
    foreach ($line in (Get-Content $sumsPath)) {
        if ($line -match '^([0-9a-fA-F]{64})\s+(.+)$') {
            $name = $Matches[2]
            if ($digests.ContainsKey($name)) {
                $digestsTimes[$name] = $digestsTimes[$name] + 1
            } else {
                $digests[$name] = $Matches[1].ToLower()
                $digestsTimes[$name] = 1
            }
        }
    }
    foreach ($asset in $assets) {
        if ($asset.Name -eq "SHA256SUMS") { continue }
        if ($digests.ContainsKey($asset.Name) -and $digestsTimes[$asset.Name] -gt 1) {
            # A digests file that names the same archive twice is a file
            # nobody can read: the reader takes the digest and finds two
            # of them.
            #
            # The path in the sentence is the unix twin's, with the slash:
            # the two are compared byte for byte, and a path spelled the
            # host's way is a line the other one does not have.
            [Console]::Error.WriteLine("$($asset.Name) is named $($digestsTimes[$asset.Name]) times in .dist/SHA256SUMS: a digests file that names the same archive twice is a file nobody can read")
            exit 1
        }
        if (-not $digests.ContainsKey($asset.Name)) {
            [Console]::Error.WriteLine("$($asset.Name) has no digest: the build did not digest it")
            exit 1
        }
        $got = (Get-RgySha256 $asset.FullName)
        if ($digests[$asset.Name] -ne $got) {
            # The unix twin's three lines, in the unix twin's words: the
            # two digests are printed one under the other, so the reader
            # sees which of the two is the odd one out.
            [Console]::Error.WriteLine("$($asset.Name) does not match the digest the build wrote")
            [Console]::Error.WriteLine("  expected $($digests[$asset.Name])")
            [Console]::Error.WriteLine("  got      $got")
            exit 1
        }
    }
}

# --- 2b. the archives carry the documents the tree has ---------------------
# The digest check above proves the archives are the ones the build made. It
# does not say what is INSIDE them: a release built before the README's last
# edit carries the older README, and its digest matches perfectly.
#
# The build wrote down the digest of every document it packed, beside the
# archive. This compares each one with the tree's, and a document that is not
# the tree's stops the publish: the README is what tells the reader what the
# release is, and a release whose README is older than the tree is a release
# that says something the tree does not say.
$docsRoot = ".dist\documents"
foreach ($asset in $assets) {
    if ($asset.Name -eq "SHA256SUMS") { continue }
    $sidecar = Join-Path $docsRoot "$($asset.Name).sha256"
    if (-not (Test-Path $sidecar)) {
        [Console]::Error.WriteLine("$($asset.Name) has no document digests: it was built by a build that did not write them")
        [Console]::Error.WriteLine("  rebuild it with scripts/build-release.ps1, and copy .dist\documents\ with it")
        exit 1
    }
    $docsCount = 0
    foreach ($row in (Get-Content $sidecar)) {
        if (-not $row) { continue }
        $parts = $row -split '\s+', 2
        $want = $parts[0].ToLower()
        $doc = $parts[1]
        $docsCount++
        $docPath = Join-Path $root $doc
        if (-not (Test-Path $docPath)) {
            [Console]::Error.WriteLine("$($asset.Name) carries a $doc that the tree does not have")
            exit 1
        }
        $got = (Get-RgySha256 $docPath)
        if ($want -ne $got) {
            [Console]::Error.WriteLine("$($asset.Name) carries a $doc that is not the tree's: the archive has $want, the tree has $got")
            exit 1
        }
    }
    # The console's stdout, and not Write-Host: the twin's output is compared
    # byte for byte, and the information stream is not what a redirection
    # captures.
    [Console]::Out.Write("documents: $($asset.Name) carries the tree's documents ($docsCount files)`n")
}

# The brew formula, checked before anything is published. The formula names
# the macOS archives, and a release that goes out while the formula cannot
# name what the release publishes is a release nobody can install from.
#
# The check is here, before the tag and before the release, so a formula
# that cannot be mapped stops the publish instead of stopping it half way.
$formula = "Formula\rgy-search.rb"
$macArches = @("aarch64-apple-darwin", "x86_64-apple-darwin")
if (Test-Path $formula) {
    $formulaLines = Get-Content $formula
    $urlLines = @($formulaLines | Where-Object { $_ -match '^\s*url "' })
    $shaLines = @($formulaLines | Where-Object { $_ -match '^\s*sha256 "' })
    if ($urlLines.Count -ne $shaLines.Count) {
        [Console]::Error.WriteLine("the formula carries $($urlLines.Count) url lines and $($shaLines.Count) sha256 lines: they do not pair up")
        exit 1
    }
    foreach ($arch in $macArches) {
        if (-not (Test-Path ".dist\rgy-v$Version-$arch.tar.gz")) { continue }
        $named = @($urlLines | Where-Object { $_ -match "-$([regex]::Escape($arch))\.tar\.gz" })
        if ($named.Count -eq 0) {
            [Console]::Error.WriteLine("the formula does not name ${arch}: this release publishes it and the formula would not install it")
            exit 1
        }
        if ($named.Count -gt 1) {
            [Console]::Error.WriteLine("the formula names $arch $($named.Count) times: a formula that names the same archive twice is a formula nobody can read")
            exit 1
        }
    }
}

# The release's main. The README, the installers and the winget manifests
# point at github.com/danalec/rgy/main, and the mirror carries main
# because of it -- so main has to carry the release, or the links resolve
# to an older tree that answers with the wrong project.
#
# The check is here, before the tag and before the release, so a main that
# is behind stops the publish instead of stopping it half way -- and it
# runs in the dry run too, because a dry run that says "the release would
# be rgy-v0.1.0" when the release would stop at the first step is a dry
# run that lies.
$mainBranch = git rev-parse --verify --quiet refs/heads/main
if (-not $ManifestsOnly -and $LASTEXITCODE -eq 0 -and $mainBranch) {
    git merge-base --is-ancestor HEAD main
    if ($LASTEXITCODE -ne 0) {
        $behind = (git rev-list --count main..HEAD).Trim()
        [Console]::Error.WriteLine("main does not carry the release commit: main is $behind commits behind HEAD")
        [Console]::Error.WriteLine("  the README, the installers and the winget manifests point at main, and they would answer with the older tree")
        [Console]::Error.WriteLine("  merge the branch into main (git merge --ff-only <branch>) and run this again")
        exit 1
    }
}

# --- 2c. the release body ----------------------------------------------------
# The body of the release is a file in the tree: docs/release-<version>.md. It
# is the page the README and the manual promise, and both publishers read it,
# so the release says what the tree says and the page that is published is the
# page the repository carries.
#
# The name is the version's, not a constant: a page named for one release and
# read for another is a page that says one thing and is published as another,
# and the drift is silent -- the publish reads what is there and does not
# complain. The version is the one the tree carries (the manifests), so the
# page that names the version is the page of the release being cut.
#
# Two markers in the page are for the scripts, and they are the only part of
# it that is not the page:
#
#   <!-- assets: ... -->  the files the release carries. The list is compared
#                         with the files that are about to be uploaded, and a
#                         page that names an archive the release does not
#                         carry stops the publish -- and so does a release
#                         that carries an archive the page does not name.
#                         A run that rewrites the manifests and publishes
#                         nothing compares the list one way only: the page
#                         may name assets the run does not have -- the
#                         other rows' assets -- and everything the run
#                         carries has to be named by the page.
#   <!-- platform -->     where each publisher puts its own sentence about
#                         where the history lives.
#
# The page is read as UTF-8 and normalized: the carriage returns go and the
# trailing newline goes with them. A checkout that rewrote the line endings
# must not change what the release says, and a carriage return is not JSON.
$pageRelative = "docs/release-$Version.md"
$pagePath = Join-Path $root $pageRelative
if (-not (Test-Path $pagePath)) {
    [Console]::Error.WriteLine("the release page is not there: $pageRelative")
    exit 1
}
$body = [IO.File]::ReadAllText($pagePath, [Text.Encoding]::UTF8)
$body = $body -replace "`r", ""
$body = $body.TrimEnd("`n")

# The marker line, and how many of them there are: exactly one, or the page
# is not a page this script knows how to publish.
$platformMarkers = [regex]::Matches($body, '(?m)^<!-- platform -->$')
if ($platformMarkers.Count -ne 1) {
    [Console]::Error.WriteLine("the release page does not carry exactly one platform marker: $pageRelative")
    exit 1
}
# The sentence that says where the history lives. It is the same sentence in
# the two twins, byte for byte: the page is published from both, and two
# sentences are two pages.
$platformSentence = "The history of this project lives here, on the Forgejo; the GitHub repository is a flat mirror of this release — one commit, no parents."
$body = $body -replace '(?m)^<!-- platform -->$', $platformSentence

# The assets the page names, and the assets the release carries. The two are
# compared as sets, and the order is the byte order: a locale's order is a
# machine's habit, and the two lists have to be compared the same way on
# every machine.
$assetsMarkers = [regex]::Matches($body, '(?m)^<!-- assets: (.*) -->$')
if ($assetsMarkers.Count -ne 1) {
    [Console]::Error.WriteLine("the release page does not carry exactly one assets marker: $pageRelative")
    exit 1
}
# The refusal for a release page that does not match the assets of this
# run -- and the empty string when it does.
#
# The twin of the unix script's release_assets_refusal, and the same
# sentences: the page is published from both, and two sentences are two
# pages.
#
# The two modes ask two different questions, and they are answered in two
# different sentences.
#
# A PUBLISH carries the whole release: the archives in .dist/ are the
# release's assets, and the page has to name exactly them. A page that
# names an archive the release does not carry is a page that promises an
# archive nobody can download, and a release that carries an archive the
# page does not name is a release the page does not tell the reader about.
# Both stop the publish, and both are said in one sentence.
#
# A MANIFESTS-ONLY run carries ONE archive of the release: the release is
# built one row at a time, and each row rewrites the manifests from the
# archive it built. The page names the whole release, so the page may name
# assets this row does not have -- the other rows' assets -- and that is
# not a mistake. What IS a mistake is an archive this row carries and the
# page does not name: the release does not carry it either, and the
# manifests this row rewrites would pin an archive the release does not
# publish.
#
# The arguments are the mode, the page that carries the marker, and the two
# lists of names -- each one a space-separated string.
function Get-ReleaseAssetsRefusal([string]$mode, [string]$page, [string]$onlyPage, [string]$onlyRelease) {
    $pageNames = @()
    foreach ($name in ($onlyPage -split '\s+')) { if ($name) { $pageNames += $name } }
    $releaseNames = @()
    foreach ($name in ($onlyRelease -split '\s+')) { if ($name) { $releaseNames += $name } }
    if ($mode -eq "manifests-only") {
        if ($releaseNames.Count -eq 0) { return "" }
        return @(
            "the release page does not name the assets of this run: this run carries [$(Join-Names $releaseNames)] the page does not name",
            "  the assets marker in $page names the whole release, and every archive this run carries has to be in it",
            "  add the name to the marker and run this again"
        ) -join "`n"
    }
    if ($pageNames.Count -eq 0 -and $releaseNames.Count -eq 0) { return "" }
    return "the release page does not match the assets of this release: the page names [$(Join-Names $pageNames)] the release does not carry, and the release carries [$(Join-Names $releaseNames)] the page does not name"
}

$pageAssetText = $assetsMarkers[0].Groups[1].Value
$pageAssetNames = @(Sort-NameOrdinal @($pageAssetText -split '\s+' | Where-Object { $_ }))
$releaseAssetNames = @(Sort-NameOrdinal @($assets | ForEach-Object { $_.Name }))
$onlyPage = @($pageAssetNames | Where-Object { $releaseAssetNames -cnotcontains $_ })
$onlyRelease = @($releaseAssetNames | Where-Object { $pageAssetNames -cnotcontains $_ })
# The mode decides which way the two lists are compared. A publish
# compares them both ways, and the sets have to be equal; a run that
# rewrites the manifests and publishes nothing compares them one way --
# everything this run carries has to be named by the page, and the page
# may name the other rows' assets on top of that.
#
# The dry run does not change this: a dry run that skips a check the real
# run does is a dry run that lies.
$assetsMode = "publish"
if ($ManifestsOnly) { $assetsMode = "manifests-only" }
$assetsRefusal = Get-ReleaseAssetsRefusal $assetsMode $pageRelative ($onlyPage -join " ") ($onlyRelease -join " ")
if ($assetsRefusal) {
    [Console]::Error.Write("$assetsRefusal`n")
    exit 1
}

# A JSON string: the backslash, the quote and the newline are the three
# that a release body can carry.
#
# The twin of the unix script's json_escape, and the same bytes: the
# backslash first, then the quote, then the newline. The order matters
# -- a quote escaped before the backslash would take the backslash that
# came with it.
#
# The request is written by hand, and not by ConvertTo-Json, for the
# same reason the unix twin writes it by hand: the two have to send the
# same bytes, and a PowerShell hashtable keeps no order, so
# ConvertTo-Json writes the keys in an order of its own.
function ConvertTo-JsonString([string]$text) {
    $builder = New-Object Text.StringBuilder
    foreach ($ch in $text.ToCharArray()) {
        if ($ch -eq '\') { [void]$builder.Append('\\') }
        elseif ($ch -eq '"') { [void]$builder.Append('\"') }
        elseif ($ch -eq "`n") { [void]$builder.Append('\n') }
        else { [void]$builder.Append($ch) }
    }
    return $builder.ToString()
}

# A piece of JSON, checked by a parser: the escaped body is checked as a
# string (it is one) and the request is checked as a document.
#
# The twin of the unix script's json_escape_is_valid, and the same
# question: is this text something a JSON parser accepts? The unix twin
# answers it with a scanner of its own -- a shell has no JSON parser --
# and the two refuse the same way.
function Test-JsonText([string]$json) {
    # A raw control character is not JSON. The parser PowerShell has
    # accepts one inside a string -- a newline in the middle of a body
    # parses, and the forgejo answers 400 to it -- so the rule is
    # written out here. The text this is asked about is one line: an
    # escaped body, or a request this script wrote itself.
    #
    # The class is the regex engine's own (\x00 is not a PowerShell
    # escape): a single-quoted string hands it to the engine as it is.
    if ($json -match '[\x00-\x08\x0A-\x1F]') { return $false }
    try {
        $null = $json | ConvertFrom-Json
        return $true
    } catch {
        return $false
    }
}

# The body as the forgejo receives it: escaped once, checked once, and
# the same bytes in every request -- and the same bytes the unix twin
# sends, which is the point of a twin.
#
# The check parses the escaped body as a JSON string, before anything is
# sent: a body that is not JSON is a 400 from the forgejo and a release
# that is not made, and a dry run that says "the release would be
# rgy-v0.1.0" when the release would stop at the first step is a dry
# run that lies. The unix twin asks the same question of the same bytes
# with a scanner of its own, and refuses the same way.
#
# And it is here and not after the tag: a body that cannot be published
# has to be found before the tag is pushed, because a tag that is out is
# a tag nobody can take back.
#
# A refusal prints a sentence on stderr and an exit code, and never a
# `throw`: an unhandled one prints this script's path, its line number
# and the offending source line on top of the message -- three extra
# lines the unix twin does not write, and the two are compared byte for
# byte.
$escapedBody = ConvertTo-JsonString $body
if (-not (Test-JsonText ('"' + $escapedBody + '"'))) {
    [Console]::Error.Write("the release body is not valid JSON: the escaping is broken`n")
    exit 1
}

# The dry run says what the bytes would be: the digest of the body, so
# the two twins can be compared with a diff. It is the body's own bytes
# -- the text the forgejo stores -- and not the escaped ones, which are
# the way the bytes travel and not the page.
if ($DryRun) {
    $bodyFile = Join-Path ([IO.Path]::GetTempPath()) ("rgy-release-body-" + [guid]::NewGuid().ToString("N"))
    [IO.File]::WriteAllText($bodyFile, $body, (New-Object Text.UTF8Encoding $false))
    $bodyDigest = Get-RgySha256 $bodyFile
    Remove-Item -LiteralPath $bodyFile -Force
    [Console]::Out.Write("the release body: $bodyDigest`n")
}

# --- 2d. winget validate ----------------------------------------------------
# The winget manifests are a Windows package manager's, and there is no
# `winget` on Linux: the unix twin has no step here, and it says so
# instead of pretending to have done it.
#
# The validation runs before the tag and before the release, and not after
# it: a validator that refuses the manifest is a release that must not go
# out, and a refusal that lands after the release is public is a refusal
# that comes too late. The manifests the validator reads are the ones the
# tree carries; the rewrite below only fills in the version, the date, the
# URL and the digest, and it cannot turn a manifest the validator accepted
# into one it refuses.
$wingetDir = Join-Path $root ".winget"
$winget = Join-Path $env:LOCALAPPDATA "Microsoft\WindowsApps\winget.exe"
if (Test-Path $winget) {
    if ($DryRun) { Write-Host "[dry-run] winget validate --manifest $wingetDir" }
    else {
        & $winget validate --manifest $wingetDir
        if ($LASTEXITCODE -ne 0) {
            [Console]::Error.WriteLine("winget validate failed")
            exit 1
        }
        Write-Host "winget validate: ok"
    }
} else {
    Write-Host "winget.exe not found, skipping validation"
}

# --- 3. tag + push ---------------------------------------------------------
$existingTag = git tag -l $tag
if ($ManifestsOnly) {
    Write-Host "[manifests-only] the tag is not created"
} elseif (-not $existingTag) {
    if ($DryRun) { Write-Host "[dry-run] git tag $tag; git push forgejo $tag" }
    else {
        git tag $tag
        git push forgejo $tag
    }
} else {
    Write-Host "tag $tag already exists"
}

# --- 4. forgejo release + assets -------------------------------------------
# The credentials are the ones git already has for this host.
#
# The request goes in as exact bytes, not through the pipeline: PowerShell's
# pipe to a native command rewrites the newlines, and git reads the request
# line by line -- a CRLF where it expects an LF is a request with no
# protocol, and git answers "refusing to work with credential missing
# protocol field".
function Get-GitCredential([string]$protocol, [string]$gitHost) {
    $psi = New-Object Diagnostics.ProcessStartInfo
    $psi.FileName = "git"
    $psi.Arguments = "credential fill"
    # The working directory is the repository, and it is set on the child
    # and not on the shell: PowerShell's Set-Location does not change
    # [Environment]::CurrentDirectory, and that is the directory a child
    # process inherits -- so the request went out from wherever the shell
    # happened to be, and the repository's own credential configuration
    # was not read. The unix twin does a real `cd "$repo"` before its
    # `git credential fill`, and this is the same thing: the command runs
    # IN the repository.
    $psi.WorkingDirectory = $root
    $psi.RedirectStandardInput = $true
    $psi.RedirectStandardOutput = $true
    $psi.RedirectStandardError = $true
    $psi.UseShellExecute = $false
    $proc = [Diagnostics.Process]::Start($psi)
    $bytes = [Text.Encoding]::UTF8.GetBytes("protocol=$protocol`nhost=$gitHost`n`n")
    $proc.StandardInput.BaseStream.Write($bytes, 0, $bytes.Length)
    $proc.StandardInput.Close()
    $out = $proc.StandardOutput.ReadToEnd()
    $err = $proc.StandardError.ReadToEnd()
    $proc.WaitForExit()
    if ($proc.ExitCode -ne 0) {
        Write-Error "git credential fill failed: exit=$($proc.ExitCode) $err"
        exit 1
    }
    $creds = @{}
    foreach ($line in ($out -split "`n")) {
        if ($line -match '^([^=]+)=(.*)$') { $creds[$Matches[1]] = $Matches[2] }
    }
    return $creds
}

# The credentials are fetched only when the release is actually touched:
# the dry run and the manifests-only mode never call the API, and a host
# that has no credential for the forgejo can still rewrite the manifests.
$auth = $null
$release = $null
if ($ManifestsOnly) {
    Write-Host "[manifests-only] the forgejo release is not touched"
} elseif ($DryRun) {
    Write-Host "[dry-run] create/patch the forgejo release $tag and upload:"
    $assets | ForEach-Object { Write-Host "    $($_.Name)" }
} else {
    $cred = Get-GitCredential $forgejoProto $forgejoHost
    $user = $cred["username"]
    $pass = $cred["password"]
    if (-not $user -or -not $pass) {
        [Console]::Error.WriteLine("no git credentials for the forgejo")
        exit 1
    }
    $auth = @{ Authorization = "Basic " + [Convert]::ToBase64String(
        [Text.Encoding]::ASCII.GetBytes("${user}:${pass}")) }
    try {
        $release = Invoke-RestMethod -Headers $auth "$repoApi/releases/tags/$tag"
        Write-Host "release exists: $forgejo/releases/tag/$tag"
    } catch {
        Write-Host "release $tag does not exist yet"
    }
}

if (-not $ManifestsOnly -and -not $DryRun) {
    if (-not $release) {
        $json = '{"tag_name":"' + $tag + '","name":"rgy v' + $Version + '","body":"' + $escapedBody + '"}'
        if (-not (Test-JsonText $json)) {
            [Console]::Error.Write("the release request is not valid JSON: the escaping is broken`n")
            exit 1
        }
        $release = Invoke-RestMethod -Headers $auth -Method Post "$repoApi/releases" `
            -ContentType "application/json" -Body ([Text.Encoding]::UTF8.GetBytes($json))
        Write-Host "release created: $forgejo/releases/tag/$tag"
    } elseif ($release.body -ne $body) {
        $json = '{"name":"rgy v' + $Version + '","body":"' + $escapedBody + '"}'
        if (-not (Test-JsonText $json)) {
            [Console]::Error.Write("the release request is not valid JSON: the escaping is broken`n")
            exit 1
        }
        $release = Invoke-RestMethod -Headers $auth -Method Patch "$repoApi/releases/$($release.id)" `
            -ContentType "application/json" -Body ([Text.Encoding]::UTF8.GetBytes($json))
        Write-Host "release updated"
    }

    foreach ($asset in $assets) {
        $stale = $release.assets | Where-Object { $_.name -eq $asset.Name }
        if ($stale) {
            Invoke-RestMethod -Headers $auth -Method Delete "$repoApi/releases/$($release.id)/assets/$($stale.id)" | Out-Null
            Write-Host "replaced asset: $($asset.Name)"
        }
        # The URL is built here, and not taken out of the release's own
        # upload_url: that field is a TEMPLATE -- the forgejo answers with
        # "<base>/releases/assets{?name,attachment}" -- and a query
        # appended to it as a literal string asks for a path that carries
        # the braces. The upload then answers 404, and the release carries
        # no assets.
        #
        # The unix twin builds the same URL out of the release's own id,
        # and the two have to ask for the same bytes.
        Invoke-RestMethod -Headers $auth -Method Post "$repoApi/releases/$($release.id)/assets?name=$($asset.Name)" `
            -ContentType "application/octet-stream" -Body ([IO.File]::ReadAllBytes($asset.FullName)) | Out-Null
        # The size the way the .sh twin prints it: two decimals, in
        # megabytes, and not a locale's comma between them.
        $size = [math]::Round($asset.Length / 1MB, 2).ToString("0.00", [Globalization.CultureInfo]::InvariantCulture)
        Write-Host "uploaded: $($asset.Name) ($size MB)"
    }
}

# --- 5. GitHub release ------------------------------------------------------
# GitHub is the public mirror: the same tag, the same assets, the same
# description. The forgejo goes first because it is the development
# upstream, but a mirror that is down must not hold the public one
# hostage -- the report says what did not happen and the port goes ahead.
#
# The repository on GitHub is a flat mirror -- the whole tree in one commit,
# with no parents -- and the development history lives on the forgejo. The
# tool that makes that mirror lives on the maintainer's machine and is not in
# this tree, and it deletes and recreates the repository to do it: a push
# of the branches and the tag would replace the mirror with the full
# history, and a repository carrying stars would lose them — the stars
# live with the repository, not with the commits.
#
# So the mirror is the mirror tool's business and this script leaves it
# alone. The switch asks for the old behaviour, for the caller who really
# wants the branches and the tag pushed there.
if (-not $WithGithub) {
    Write-Host "the GitHub mirror is left alone: it is a flat mirror, and a push of the branches and the tag would replace it with the full history"
} elseif ($ManifestsOnly) {
    Write-Host "[manifests-only] the GitHub release is not touched"
} elseif ($DryRun) {
    Write-Host "[dry-run] push main and the checked-out branch to $ghRepo, and make main the default branch"
    Write-Host "[dry-run] create the GitHub release $tag in $ghRepo"
} else {
    gh auth status *> $null
    if ($LASTEXITCODE -ne 0) {
        [Console]::Error.WriteLine("gh is not authenticated")
        exit 1
    }

    gh repo view $ghRepo *> $null
    if ($LASTEXITCODE -ne 0) {
        if ($Private) { gh repo create $ghRepo --private --source . --remote github }
        else { gh repo create $ghRepo --public --source . --remote github }
        if ($LASTEXITCODE -ne 0) {
            [Console]::Error.WriteLine("the GitHub mirror could not be created")
            exit 1
        }
    }
    git remote get-url github *> $null
    if ($LASTEXITCODE -ne 0) {
        git remote add github "$gh.git"
        if ($LASTEXITCODE -ne 0) {
            [Console]::Error.WriteLine("the github remote could not be added")
            exit 1
        }
    }
    # The mirror carries the branch that is checked out AND main: the
    # README, the installers and the winget manifests all point at
    # github.com/danalec/rgy/main, so a mirror without main is a mirror
    # whose links are all 404.
    #
    # main goes first: a mirror with no commit yet takes its default
    # branch from the first push, and a mirror whose default is
    # `feature/rgxy-launcher` opens on a branch nobody asked for. The
    # default is then set by name, so the outcome does not depend on
    # which push landed first.
    git push -u github main
    if ($LASTEXITCODE -ne 0) {
        [Console]::Error.WriteLine("main could not be pushed to the GitHub mirror")
        exit 1
    }
    git push -u github HEAD
    if ($LASTEXITCODE -ne 0) {
        [Console]::Error.WriteLine("the branch could not be pushed to the GitHub mirror")
        exit 1
    }
    git push -f github $tag
    if ($LASTEXITCODE -ne 0) {
        [Console]::Error.WriteLine("the tag could not be pushed to the GitHub mirror")
        exit 1
    }
    gh repo edit $ghRepo --default-branch main
    if ($LASTEXITCODE -ne 0) {
        [Console]::Error.WriteLine("the default branch of the mirror could not be set to main")
        exit 1
    }

    gh release view $tag --repo $ghRepo *> $null
    if ($LASTEXITCODE -eq 0) {
        gh release delete $tag --repo $ghRepo --yes
        if ($LASTEXITCODE -ne 0) {
            [Console]::Error.WriteLine("the previous GitHub release could not be deleted")
            exit 1
        }
    }

    $files = $assets | ForEach-Object { $_.FullName }
    gh release create $tag @files --repo $ghRepo --title "rgy v$Version" --notes $body
    if ($LASTEXITCODE -ne 0) {
        [Console]::Error.WriteLine("the GitHub release could not be created")
        exit 1
    }
    Write-Host "github release: $gh/releases/tag/$tag"
}

# --- 6. the winget manifests ----------------------------------------------------
# The manifest names the zip the release carries, so its hash is the hash
# of the file that is about to be uploaded -- not the hash of the one the
# manifest was written for.
# The portable Windows archive, by its exact name: the release carries
# an AVX2 and a Zen 4 zip as well, and the manifest installs the one
# that runs on every x86_64 CPU.
$zipName = "rgy-v$Version-x86_64-windows.zip"
$zip = $assets | Where-Object { $_.Name -eq $zipName } | Select-Object -First 1

function Set-Yaml([string]$file, [hashtable]$repl) {
    $text = Get-Content $file -Raw
    foreach ($k in $repl.Keys) {
        $text = $text -replace $k, $repl[$k]
    }
    Set-Content $file $text -NoNewline
}

if (-not $zip) {
    # The manifest is left exactly as it is, and the sentence says why in
    # the words of the archive: a row that built the AVX2 zip, the Zen 4
    # one or the aarch64 one did not build the archive the manifest
    # installs, and a row's archive is not that archive. The old sentence
    # said there was no Windows archive in .dist/, which is false on
    # three of the four Windows rows -- they hold one.
    Write-Host "winget manifests: not touched (.dist/ does not hold $zipName, the archive the manifest installs)"
} elseif ($DryRun) {
    Write-Host "[dry-run] winget manifests -> version $Version, date $(Get-Date -Format 'yyyy-MM-dd')"
} else {
    $sha = (Get-RgySha256 $zip).ToUpper()
    $today = Get-Date -Format "yyyy-MM-dd"
    Set-Yaml (Join-Path $wingetDir "danalec.rgy-search.installer.yaml") @{
        "PackageVersion: .*"    = "PackageVersion: $Version"
        "ReleaseDate: .*"       = "ReleaseDate: $today"
        "InstallerUrl: .*"      = "InstallerUrl: $gh/releases/download/$tag/$($zip.Name)"
        "InstallerSha256: .*"   = "InstallerSha256: $sha"
    }
    Set-Yaml (Join-Path $wingetDir "danalec.rgy-search.yaml") @{
        "PackageVersion: .*"    = "PackageVersion: $Version"
    }
    Set-Yaml (Join-Path $wingetDir "danalec.rgy-search.locale.en-US.yaml") @{
        "PackageVersion: .*"    = "PackageVersion: $Version"
        "ReleaseNotesUrl: .*"   = "ReleaseNotesUrl: $gh/releases/tag/$tag"
    }
    Write-Host "manifests updated: version $Version, sha256 $sha, date $today"
}

# --- 7. the brew formula ----------------------------------------------------
# The formula names the macOS archives, so its digests are the digests of the
# files that are about to be published. A Windows box has no macOS archive to
# digest, and the formula says so instead of carrying a digest of nothing.
#
# The rewrite is per architecture: the formula's url line says which archive
# it installs, and the archive that is here gets the URL and the digest it
# deserves. The architecture whose archive is not here keeps its placeholder
# -- the same rule the installer pins follow, and the same truth: there is
# no release for that platform yet.
#
# The n-th url line does NOT get the n-th archive: a formula with two blocks
# and a release with one archive would write a URL that names no file and a
# checksum that is empty, and `brew` would be asked to install a directory.
$macAssets = @()
foreach ($arch in $macArches) {
    $f = Get-Item ".dist\rgy-v$Version-$arch.tar.gz" -ErrorAction SilentlyContinue
    if ($f) { $macAssets += $f }
}

if ($macAssets.Count -eq 0) {
    Write-Host "brew formula: not touched (no macOS archive in .dist/)"
} elseif ($DryRun) {
    Write-Host "[dry-run] brew formula -> version $Version"
    $macAssets | ForEach-Object { Write-Host "    $($_.Name)" }
} else {
    # The digests of the archives this release carries, by architecture.
    $digests = @{}
    foreach ($a in $macAssets) {
        $arch = $a.Name -replace "^rgy-v$([regex]::Escape($Version))-", ""
        $arch = $arch -replace "\.tar\.gz$", ""
        $digests[$arch] = (Get-RgySha256 $a.FullName)
    }
    $lines = Get-Content $formula
    $pending = ""
    $vi = 0
    $rewritten = foreach ($line in $lines) {
        if ($line -match '^(\s*)url "') {
            $indent = $Matches[1]
            $file = ($line -replace '^[^"]*"', "") -replace '".*$', ""
            $name = ($file -split "/")[-1]
            $arch = ""
            foreach ($key in $digests.Keys) {
                if ($name -match "-$([regex]::Escape($key))\.tar\.gz$") { $arch = $key }
            }
            if ($arch) {
                $pending = $arch
                $indent + 'url "' + "$gh/releases/download/$tag/rgy-v$Version-$arch.tar.gz" + '"'
            } else {
                $pending = ""
                $line
            }
        } elseif ($line -match '^(\s*)sha256 "') {
            $indent = $Matches[1]
            if ($pending) {
                $digest = $digests[$pending]
                $pending = ""
                $indent + 'sha256 "' + $digest + '"'
            } else {
                $line
            }
        } elseif ($line -match '^(\s*)version "' -and $vi -eq 0) {
            $vi++
            "$($Matches[1])version `"$Version`""
        } else {
            $line
        }
    }
    # The formula, written with LF endings and no BOM: the .sh twin writes
    # it that way, and the two scripts have to leave the same bytes behind.
    [IO.File]::WriteAllText($formula, (($rewritten -join "`n") + "`n"), (New-Object Text.UTF8Encoding $false))
    foreach ($arch in $macArches) {
        if ($digests.ContainsKey($arch)) { continue }
        Write-Host "brew formula: $arch keeps its placeholder: this release carries no $arch archive"
    }
    Write-Host "brew formula updated: version $Version"
    foreach ($a in $macAssets) {
        $digest = (Get-RgySha256 $a.FullName)
        Write-Host "    $digest  $($a.Name)"
    }
}

# --- 8. the installer pins ----------------------------------------------------
# Both installers carry one digest per architecture they may install, and
# the digests are release data: they are rewritten here, from the artifacts
# this release carries, and a pin that does not end up naming the release's
# own artifact is a failure -- a hand-maintained pin is a pin that is wrong
# on the day after the release.
#
# An asset this release does not carry leaves its pin at the placeholder.
# The installer then says that there is no release for that platform yet,
# which is true -- and better than a digest of an archive that is not there.
function Set-InstallerPin([string]$file, [string]$version, [string]$sha, [string]$pattern) {
    # The name, not the path: the .sh twin names the file the same way, and
    # the two scripts have to say the same thing about the same pin.
    $name = Split-Path $file -Leaf
    if (-not (Test-Path $file)) {
        Write-Host "installer pin: $name not found, skipped"
        return
    }
    $text = Get-Content $file -Raw
    # The pattern keeps the name and the quotes in groups 1 and 2, and only
    # the digest between them is replaced. `${1}` and not `$1`: a `$1`
    # followed by a digit is group 1 followed by that digit -- or, worse,
    # group 13.
    $text = [regex]::Replace($text, $pattern, '${1}' + $sha + '${2}')
    # The tag is three or four numbers deep -- the release carries the fourth,
    # the fix count of the line, and the registry takes three -- so the
    # pattern reads every one of them. A pattern that stops at the third
    # leaves the fourth on the end of the replacement: a second run of this
    # step over `rgy-v0.1.9.0` writes `rgy-v0.1.9.0.0`, and the installer
    # asks the forge for a tag nobody published.
    $text = [regex]::Replace($text, 'rgy-v[0-9]+(?:\.[0-9]+){2,3}', $version)
    Set-Content $file $text -NoNewline
    $check = Get-Content $file -Raw
    if ($check -notmatch [regex]::Escape($sha)) {
        [Console]::Error.WriteLine("the pin in $name does not carry the digest of the release ($sha)")
        exit 1
    }
    if ($check -notmatch 'expected') {
        [Console]::Error.WriteLine("the pin in $name lost its name: the rewrite ate more than the digest")
        exit 1
    }
    Write-Host "pin updated: $name -> $version $sha"
}

# The digest of an asset this release carries.
#
# The archive itself when it is here, and the digest file when it is not:
# the file is what the other platforms wrote into the same directory.
function Get-AssetDigest([string]$name) {
    $path = Join-Path ".dist" $name
    if (Test-Path $path) {
        return (Get-RgySha256 $path).ToUpper()
    }
    $sumsFile = ".dist\SHA256SUMS"
    if (Test-Path $sumsFile) {
        foreach ($line in (Get-Content $sumsFile)) {
            if ($line -match '^([0-9a-fA-F]{64})\s+(.+)$' -and $Matches[2] -eq $name) {
                return $Matches[1].ToUpper()
            }
        }
    }
    return $null
}

$placeholder = '0' * 64
# The pins, and the line each one lives on. The patterns keep the name and
# the quotes and replace only the digest between them; the second pattern
# finds the line the pin is on.
$pins = @(
    @{ File = "install.ps1"; Asset = "rgy-v$Version-x86_64-windows.zip"; Pattern = "('x86_64-windows\.zip'\s*=\s*')[0-9A-Fa-f]{64}(')"; Line = "^\s*'x86_64-windows\.zip'" }
    @{ File = "install.ps1"; Asset = "rgy-v$Version-aarch64-windows.zip"; Pattern = "('aarch64-windows\.zip'\s*=\s*')[0-9A-Fa-f]{64}(')"; Line = "^\s*'aarch64-windows\.zip'" }
    @{ File = "install.sh"; Asset = "rgy-v$Version-x86_64-linux.tar.gz"; Pattern = '(?m)^(expected_linux_x86_64=")[0-9A-Fa-f]{64}(")$'; Line = '^expected_linux_x86_64=' }
    @{ File = "install.sh"; Asset = "rgy-v$Version-aarch64-linux.tar.gz"; Pattern = '(?m)^(expected_linux_aarch64=")[0-9A-Fa-f]{64}(")$'; Line = '^expected_linux_aarch64=' }
    @{ File = "install.sh"; Asset = "rgy-v$Version-x86_64-apple-darwin.tar.gz"; Pattern = '(?m)^(expected_macos_x86_64=")[0-9A-Fa-f]{64}(")$'; Line = '^expected_macos_x86_64=' }
    @{ File = "install.sh"; Asset = "rgy-v$Version-aarch64-apple-darwin.tar.gz"; Pattern = '(?m)^(expected_macos_aarch64=")[0-9A-Fa-f]{64}(")$'; Line = '^expected_macos_aarch64=' }
)

# Is the pin on that line still the placeholder? The placeholder is a pin
# that says "there is no release for this platform yet"; a real digest is a
# pin that names a release that exists.
function Test-PinIsPlaceholder([string]$file, [string]$line) {
    $text = Get-Content $file -Raw
    foreach ($l in ($text -split "`r?`n")) {
        if ($l -match $line) {
            return $l.Contains($placeholder)
        }
    }
    return $false
}

if ($DryRun) {
    Write-Host "[dry-run] installer pins -> version $tag"
} else {
    # A file is rewritten only as a whole. The version lives in the file, not
    # in the pin, so a file where one pin moves and another does not would
    # carry a version and a digest that name different releases -- and the
    # installer would refuse the archive it downloaded, which is safe and
    # useless. So when a pin names a release this run does not carry, the
    # file is left exactly as it is: the installer keeps installing the
    # release it can vouch for.
    foreach ($group in @(@{ File = "install.ps1" }, @{ File = "install.sh" })) {
        $file = $group.File
        $path = Join-Path $root $file
        $blocked = $false
        foreach ($pin in $pins) {
            if ($pin.File -ne $file) { continue }
            if (-not (Get-AssetDigest $pin.Asset)) {
                if (-not (Test-PinIsPlaceholder $path $pin.Line)) {
                    $blocked = $true
                    break
                }
            }
        }
        if ($blocked) {
            Write-Host "installer pins: $file is left alone: it pins a release this run does not carry"
            continue
        }
        foreach ($pin in $pins) {
            if ($pin.File -ne $file) { continue }
            $digest = Get-AssetDigest $pin.Asset
            if (-not $digest) {
                Write-Host "installer pin: $($pin.Asset) is not in this release; the pin stays a placeholder"
                $digest = $placeholder
            }
            Set-InstallerPin $path $tag $digest $pin.Pattern
        }
    }
}

if ($ManifestsOnly) {
    Write-Host ""
    Write-Host "manifests rewritten; nothing was published"
    exit 0
}

# --- 10. the release data ----------------------------------------------------
# The release's own data -- the installer pins, the brew formula and the
# winget manifests -- is release data: this script writes it, and it is
# committed with it. A clone that still carries the placeholders is a clone
# whose one-line installer says "there is no published rgy release for
# x86_64 yet" on the day of the release.
#
# The commit is asked for, not done on the side: publishing and committing
# are two acts, and the owner decides when the release data lands.
if ($Commit -and -not $DryRun) {
    $staged = @()
    foreach ($p in @(".winget", "install.sh", "install.ps1", "Formula")) {
        if (Test-Path $p) { $staged += $p }
    }
    git add @staged
    if ($LASTEXITCODE -ne 0) {
        [Console]::Error.WriteLine("the release data could not be staged")
        exit 1
    }
    git diff --cached --quiet
    if ($LASTEXITCODE -eq 0) {
        Write-Host "release data: nothing to commit"
    } else {
        git commit -m "release: $tag release data"
        if ($LASTEXITCODE -ne 0) {
            [Console]::Error.WriteLine("the release data could not be committed")
            exit 1
        }
        Write-Host "release data committed"
    }
    $branch = git rev-parse --abbrev-ref HEAD
    git push forgejo $branch
    if ($LASTEXITCODE -ne 0) {
        [Console]::Error.WriteLine("the release data could not be pushed to the forgejo")
        exit 1
    }
    Write-Host "release data pushed to the forgejo"
}

Write-Host ""
if ($DryRun) {
    # The dry run says what it did not do. A line that says "published" at
    # the end of a run that published nothing is a line nobody can trust
    # -- and this is the last line of the script, the one a reader's eye
    # lands on.
    Write-Host "[dry-run] the release would be $tag :"
    Write-Host "[dry-run]   $forgejo/releases/tag/$tag"
    if ($WithGithub) {
        Write-Host "[dry-run]   $gh/releases/tag/$tag"
    } else {
        Write-Host "[dry-run] the GitHub mirror would be left alone: the flat mirror is the mirror tool's business"
    }
    exit 0
}
Write-Host "published $tag : $forgejo/releases/tag/$tag"
if ($WithGithub) {
    Write-Host "published $tag : $gh/releases/tag/$tag"
} else {
    Write-Host "the GitHub mirror was left alone: the flat mirror is the mirror tool's business"
}
