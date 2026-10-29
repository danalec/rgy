# publish-release.ps1 -- publish an rgy release end to end, from Windows.
#
# The Windows counterpart of scripts/publish-release.sh.
#
#   pwsh -File scripts/publish-release.ps1              # full pipeline
#   pwsh -File scripts/publish-release.ps1 -SkipTests   # trust the build
#   pwsh -File scripts/publish-release.ps1 -DryRun      # show, don't touch
#   pwsh -File scripts/publish-release.ps1 -Private     # keep the GitHub mirror private
#   pwsh -File scripts/publish-release.ps1 -Commit      # commit the release data
#
# PowerShell 7 (pwsh) is required: under Windows PowerShell 5.1 the
# credential request git needs does not survive the trip.
#
# Pipeline: build the Windows archive -> tag rgy-v<version> + push -> create
# or update the forgejo release and (re)upload the assets -> create the
# GitHub release with the same assets -> refresh the winget manifests
# (version, sha256, date, URLs) -> winget validate -> optional -Commit of the
# manifest changes.
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
param(
    [string]$Version = "",
    [switch]$SkipTests,
    [switch]$Commit,
    [switch]$Private,
    [switch]$ManifestsOnly,
    [switch]$DryRun
)

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
$root = Split-Path $PSScriptRoot -Parent
Set-Location $root

function Get-ManifestVersion([string]$path) {
    $m = Select-String -Path $path -Pattern '^version = "([^"]+)"' | Select-Object -First 1
    if ($m) { return $m.Matches.Groups[1].Value }
    return ""
}

if (-not $Version) { $Version = Get-ManifestVersion "bins/searchctl/Cargo.toml" }
if (-not $Version) { throw "no version in bins/searchctl/Cargo.toml" }
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
if ($SkipTests) {
    & (Join-Path $PSScriptRoot "build-release.ps1") -Version $Version -SkipTests
} else {
    & (Join-Path $PSScriptRoot "build-release.ps1") -Version $Version
}
if ($LASTEXITCODE -ne 0) { throw "build failed" }

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
$assets = @()
$sums = Get-Item ".dist\SHA256SUMS" -ErrorAction SilentlyContinue
if ($sums) { $assets += $sums }
$assets += @(Sort-ByNameOrdinal (Get-ChildItem ".dist" -File -ErrorAction SilentlyContinue |
    Where-Object { $_.Name -like "rgy-v$Version-*" }))
if (-not $assets) {
    throw "no assets to publish: .dist/ holds nothing for $tag. Run scripts/build-release.ps1 first."
}

# --- 2. the assets are what the build digested ------------------------------
# An asset whose digest does not match the digest the build wrote is an
# asset nobody can check, and a release that serves it is worse than no
# release. The file the build left behind is the authority. This runs
# before anything is published, because after the first upload the damage
# is done.
$sumsPath = ".dist\SHA256SUMS"
if (Test-Path $sumsPath) {
    $digests = @{}
    foreach ($line in (Get-Content $sumsPath)) {
        if ($line -match '^([0-9a-fA-F]{64})\s+(.+)$') {
            if ($digests.ContainsKey($Matches[2])) {
                # A digests file that names the same archive twice is a file
                # nobody can read: the reader takes the digest and finds two
                # of them.
                throw "$($Matches[2]) is named twice in $sumsPath"
            }
            $digests[$Matches[2]] = $Matches[1].ToLower()
        }
    }
    foreach ($asset in $assets) {
        if ($asset.Name -eq "SHA256SUMS") { continue }
        if (-not $digests.ContainsKey($asset.Name)) {
            throw "$($asset.Name) has no digest: the build did not digest it"
        }
        $got = (Get-FileHash $asset.FullName -Algorithm SHA256).Hash.ToLower()
        if ($digests[$asset.Name] -ne $got) {
            throw "$($asset.Name) does not match the digest the build wrote: expected $($digests[$asset.Name]), got $got"
        }
    }
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

$body = @"
Whole-filesystem name search and content search in one stack.

**Which file?** -- asked over the whole filesystem, then **what is inside it?** in the same query.

    searchctl --root ~ "invoice ext:pdf"
    searchctl --root ~ "invoice grep:tesla"

The release carries four binaries: ``searchctl`` (the command-line tool),
``searchd`` (the daemon: JSON lines over stdio, a pipe or a unix socket),
``rgy-autostart`` (put the daemon in place at login) and ``rgxy`` (the
floating window over the daemon).

**Assets**
$(($assets | ForEach-Object { "- ``$($_.Name)``" }) -join "`n")

Apache-2.0.
"@

if (-not $ManifestsOnly -and -not $DryRun) {
    if (-not $release) {
        $release = Invoke-RestMethod -Headers $auth -Method Post "$repoApi/releases" `
            -ContentType "application/json" -Body ([Text.Encoding]::UTF8.GetBytes(
                (@{ tag_name = $tag; name = "rgy v$Version"; body = $body } |
                    ConvertTo-Json -Compress)))
        Write-Host "release created: $forgejo/releases/tag/$tag"
    } elseif ($release.body -ne $body) {
        $release = Invoke-RestMethod -Headers $auth -Method Patch "$repoApi/releases/$($release.id)" `
            -ContentType "application/json" -Body ([Text.Encoding]::UTF8.GetBytes(
                (@{ name = "rgy v$Version"; body = $body } | ConvertTo-Json -Compress)))
        Write-Host "release updated"
    }

    foreach ($asset in $assets) {
        $stale = $release.assets | Where-Object { $_.name -eq $asset.Name }
        if ($stale) {
            Invoke-RestMethod -Headers $auth -Method Delete "$repoApi/releases/$($release.id)/assets/$($stale.id)" | Out-Null
            Write-Host "replaced asset: $($asset.Name)"
        }
        Invoke-RestMethod -Headers $auth -Method Post "$($release.upload_url)?name=$($asset.Name)" `
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
if ($ManifestsOnly) {
    Write-Host "[manifests-only] the GitHub release is not touched"
} elseif ($DryRun) {
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
    git push -u github HEAD
    if ($LASTEXITCODE -ne 0) {
        [Console]::Error.WriteLine("the branch could not be pushed to the GitHub mirror")
        exit 1
    }
    git push -u github main
    if ($LASTEXITCODE -ne 0) {
        [Console]::Error.WriteLine("main could not be pushed to the GitHub mirror")
        exit 1
    }
    git push -f github $tag
    if ($LASTEXITCODE -ne 0) {
        [Console]::Error.WriteLine("the tag could not be pushed to the GitHub mirror")
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
$zip = $assets | Where-Object { $_.Name -eq "rgy-v$Version-x86_64-windows.zip" } | Select-Object -First 1
$wingetDir = Join-Path $root ".winget"

function Set-Yaml([string]$file, [hashtable]$repl) {
    $text = Get-Content $file -Raw
    foreach ($k in $repl.Keys) {
        $text = $text -replace $k, $repl[$k]
    }
    Set-Content $file $text -NoNewline
}

if (-not $zip) {
    Write-Host "winget manifests: not touched (no Windows archive in .dist/)"
} elseif ($DryRun) {
    Write-Host "[dry-run] winget manifests -> version $Version, date $(Get-Date -Format 'yyyy-MM-dd')"
} else {
    $sha = (Get-FileHash $zip -Algorithm SHA256).Hash.ToUpper()
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
        $digests[$arch] = (Get-FileHash $a.FullName -Algorithm SHA256).Hash.ToLower()
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
        $digest = (Get-FileHash $a.FullName -Algorithm SHA256).Hash.ToLower()
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
    $text = [regex]::Replace($text, 'rgy-v[0-9]+\.[0-9]+\.[0-9]+', $version)
    Set-Content $file $text -NoNewline
    $check = Get-Content $file -Raw
    if ($check -notmatch [regex]::Escape($sha)) {
        throw "the pin in $name does not carry the digest of the release ($sha)"
    }
    if ($check -notmatch 'expected') {
        throw "the pin in $name lost its name: the rewrite ate more than the digest"
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
        return (Get-FileHash $path -Algorithm SHA256).Hash.ToUpper()
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
# the quotes and replace only the digest between them.
$pins = @(
    @{ File = "install.ps1"; Asset = "rgy-v$Version-x86_64-windows.zip"; Pattern = "('x86_64-windows\.zip'\s*=\s*')[0-9A-Fa-f]{64}(')" }
    @{ File = "install.ps1"; Asset = "rgy-v$Version-aarch64-windows.zip"; Pattern = "('aarch64-windows\.zip'\s*=\s*')[0-9A-Fa-f]{64}(')" }
    @{ File = "install.sh"; Asset = "rgy-v$Version-x86_64-linux.tar.gz"; Pattern = '(?m)^(expected_linux_x86_64=")[0-9A-Fa-f]{64}(")$' }
    @{ File = "install.sh"; Asset = "rgy-v$Version-aarch64-linux.tar.gz"; Pattern = '(?m)^(expected_linux_aarch64=")[0-9A-Fa-f]{64}(")$' }
    @{ File = "install.sh"; Asset = "rgy-v$Version-x86_64-apple-darwin.tar.gz"; Pattern = '(?m)^(expected_macos_x86_64=")[0-9A-Fa-f]{64}(")$' }
    @{ File = "install.sh"; Asset = "rgy-v$Version-aarch64-apple-darwin.tar.gz"; Pattern = '(?m)^(expected_macos_aarch64=")[0-9A-Fa-f]{64}(")$' }
)

if ($DryRun) {
    Write-Host "[dry-run] installer pins -> version $tag"
} else {
    foreach ($pin in $pins) {
        $digest = Get-AssetDigest $pin.Asset
        if (-not $digest) {
            $digest = $placeholder
            Write-Host "installer pin: $($pin.Asset) is not in this release; the pin stays a placeholder"
        }
        Set-InstallerPin (Join-Path $root $pin.File) $tag $digest $pin.Pattern
    }
}

# --- 9. winget validate -----------------------------------------------------
$winget = Join-Path $env:LOCALAPPDATA "Microsoft\WindowsApps\winget.exe"
if (Test-Path $winget) {
    if ($DryRun) { Write-Host "[dry-run] winget validate --manifest $wingetDir" }
    else {
        & $winget validate --manifest $wingetDir
        if ($LASTEXITCODE -ne 0) { throw "winget validate failed" }
        Write-Host "winget validate: ok"
    }
} else {
    Write-Host "winget.exe not found, skipping validation"
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
Write-Host "published $tag : $forgejo/releases/tag/$tag"
Write-Host "published $tag : $gh/releases/tag/$tag"
