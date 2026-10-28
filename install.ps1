#Requires -Version 5.1
<#
.SYNOPSIS
    Install rgy on Windows.

.DESCRIPTION
    Downloads the rgy release zip, verifies its SHA256, unpacks the three
    binaries into the install directory, prepends it to the user PATH and
    registers the daemon to start at login.

.EXAMPLE
    irm https://raw.githubusercontent.com/danalec/rgy/main/install.ps1 | iex

.EXAMPLE
    .\install.ps1 -InstallDir C:\tools\rgy

.EXAMPLE
    .\install.ps1 -NoAutostart
#>
[CmdletBinding()]
param(
    # install destination; default %LOCALAPPDATA%\Programs\rgy
    [string]$InstallDir = "$env:LOCALAPPDATA\Programs\rgy",

    # override the download base (mirror/self-hosted Forgejo)
    [string]$BaseUrl = 'https://github.com/danalec/rgy/releases/download',

    # pin the version instead of asking the server for the latest rgy-*
    [string]$Version = 'rgy-v0.1.0',

    # skip SHA256 verification (not recommended)
    [switch]$SkipChecksum,

    # do not register the daemon to start at login
    [switch]$NoAutostart
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

# The digests of the archives this installer is allowed to install.
#
# The digest is uppercase hex, and it is rewritten in place by
# scripts/publish-release.ps1 -- so the shape of these lines is part of the
# contract, not a formatting choice.
#
# There is one digest per architecture, because the architecture decides
# the archive: an aarch64 machine does not run the x86_64 binary without
# an emulator, and an installer that hands it one installs something that
# will not start.
$expected = @{
    'x86_64-windows.zip' = '0000000000000000000000000000000000000000000000000000000000000000'
    'aarch64-windows.zip' = '0000000000000000000000000000000000000000000000000000000000000000'
}

# The digest of an archive nobody has published yet.
$placeholder = '0000000000000000000000000000000000000000000000000000000000000000'

# The architecture decides the archive. The release carries a portable
# build for each one it targets, and nothing else: the AVX2 and the Zen 4
# archives are for a caller that knows its own CPU.
#
# PROCESSOR_ARCHITEW6432 is the 64-bit architecture seen from a 32-bit
# PowerShell, and it is the one that answers when the host is 64-bit.
#
# The name the machine gives is not the name the release gives: Windows
# says AMD64 and the release says x86_64. The message says the release's
# name, because install.sh -- the unix twin of this script -- prints that
# one, and a pair of scripts that answer the same question in two
# different words is a pair nobody can compare.
$arch = [Environment]::GetEnvironmentVariable('PROCESSOR_ARCHITEW6432')
if (-not $arch) { $arch = [Environment]::GetEnvironmentVariable('PROCESSOR_ARCHITECTURE') }
switch ($arch) {
    'AMD64' { $assetName = 'x86_64-windows.zip'; $archName = 'x86_64' }
    'ARM64' { $assetName = 'aarch64-windows.zip'; $archName = 'aarch64' }
    default {
        [Console]::Error.WriteLine("there is no rgy release for this architecture: $arch")
        exit 1
    }
}
$asset = "$Version-$assetName"
$digest = $expected[$assetName]

# The pin is still a placeholder: no release has been published for this
# architecture yet. The installer says so in words, instead of downloading
# something it cannot vouch for. A caller that asked for no digest check
# is asking for exactly that, and the flag is theirs to use.
if (-not $SkipChecksum -and $digest -eq $placeholder) {
    [Console]::Error.WriteLine("there is no published rgy release for $archName yet")
    [Console]::Error.WriteLine(" expected $asset")
    exit 1
}

$url = "$BaseUrl/$Version/$asset"
$tmp = Join-Path $env:TEMP "rgy-install-$asset"

Write-Host "Downloading $url ..." -ForegroundColor Cyan
Invoke-WebRequest -Uri $url -OutFile $tmp -UseBasicParsing

if (-not $SkipChecksum) {
    $actual = (Get-FileHash -Path $tmp -Algorithm SHA256).Hash
    if ($actual -ne $digest) {
        Remove-Item $tmp -Force
        throw "SHA256 mismatch for $asset`n expected $digest`n got      $actual"
    }
    Write-Host "SHA256 verified." -ForegroundColor Green
} else {
    Write-Host "SHA256 verification skipped."
}

$staging = Join-Path $env:TEMP "rgy-staging-$(Get-Random)"
Expand-Archive -Path $tmp -DestinationPath $staging -Force

New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null
foreach ($exe in @('searchctl.exe', 'searchd.exe', 'rgy-autostart.exe', 'rgy.exe')) {
    $src = Join-Path $staging $exe
    if (-not (Test-Path $src)) {
        $found = Get-ChildItem $staging -Recurse -Filter $exe -File | Select-Object -First 1
        if (-not $found) { throw "no $exe in $asset" }
        $src = $found.FullName
    }
    Copy-Item -Path $src -Destination $InstallDir -Force
}
foreach ($doc in @('README.md', 'MANUAL.md', 'FORMATS.md', 'COPYING', 'NOTICE')) {
    $src = Join-Path $staging $doc
    if (Test-Path $src) { Copy-Item -Path $src -Destination $InstallDir -Force }
}

Remove-Item $tmp -Force
Remove-Item $staging -Recurse -Force

# user PATH (idempotent prepend)
$userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
if (($userPath -split ';') -notcontains $InstallDir) {
    [Environment]::SetEnvironmentVariable('Path', "$InstallDir;$userPath", 'User')
    Write-Host "Added $InstallDir to the PATH (open a new terminal to pick it up)." -ForegroundColor Yellow
} else {
    Write-Host "$InstallDir is already on the PATH."
}

# The command the project is named after.
#
# The alias travels with the release: it is the same binary under another
# name, and it was copied above like the rest.
if (Test-Path (Join-Path $InstallDir 'rgy.exe')) {
    Write-Host "Added the rgy command: it runs searchctl."
}

# The daemon at login.
#
# The autostart tool finds the daemon next to itself -- the two were just
# copied into the same directory -- and registers it with the tree it
# indexes. The entry goes into the user's own session: no administrator,
# no prompt, no dialog.
if (-not $NoAutostart) {
    & (Join-Path $InstallDir 'rgy-autostart.exe') install
    $code = $LASTEXITCODE
    if ($code -eq 0) {
        Write-Host "The daemon starts at login. To take it out: rgy-autostart uninstall"
    } elseif ($code -eq 2) {
        [Console]::Error.WriteLine("The daemon was written but the platform could not arm it (see above).")
    } else {
        [Console]::Error.WriteLine("The daemon was not registered. To register it: rgy-autostart install")
    }
}

Write-Host "Installed: searchctl, searchd and rgy-autostart in $InstallDir" -ForegroundColor Green
