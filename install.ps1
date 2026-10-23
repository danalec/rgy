#Requires -Version 5.1
<#
.SYNOPSIS
    Install rgy on Windows.

.DESCRIPTION
    Downloads the rgy release zip, verifies its SHA256, unpacks the three
    binaries into the install directory and prepends it to the user PATH.

.EXAMPLE
    irm https://raw.githubusercontent.com/danalec/rgy/main/install.ps1 | iex

.EXAMPLE
    .\install.ps1 -InstallDir C:\tools\rgy
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
    [switch]$SkipChecksum
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

# The asset of this platform, and the digest of that asset.
#
# The digest is uppercase hex, and it is rewritten in place by
# scripts/publish-release.ps1 -- so the shape of these lines is part of the
# contract, not a formatting choice.
$asset = "$Version-x86_64-windows.zip"
$expected = '0000000000000000000000000000000000000000000000000000000000000000'

$url = "$BaseUrl/$Version/$asset"
$tmp = Join-Path $env:TEMP "rgy-install-$asset"

Write-Host "Downloading $url ..." -ForegroundColor Cyan
Invoke-WebRequest -Uri $url -OutFile $tmp -UseBasicParsing

if (-not $SkipChecksum) {
    $actual = (Get-FileHash -Path $tmp -Algorithm SHA256).Hash
    if ($actual -ne $expected) {
        Remove-Item $tmp -Force
        throw "SHA256 mismatch for $asset`n expected $expected`n got      $actual"
    }
    Write-Host "SHA256 verified." -ForegroundColor Green
}

$staging = Join-Path $env:TEMP "rgy-staging-$(Get-Random)"
Expand-Archive -Path $tmp -DestinationPath $staging -Force

New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null
foreach ($exe in @('searchctl.exe', 'searchd.exe', 'rgy-autostart.exe')) {
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
}

Write-Host "Installed: searchctl, searchd and rgy-autostart in $InstallDir" -ForegroundColor Green
