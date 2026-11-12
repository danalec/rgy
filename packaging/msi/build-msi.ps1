# build-msi.ps1 -- build the per-user rgy MSI from a release archive.
#
#   pwsh -File packaging/msi/build-msi.ps1 -Version 0.1.10.3 -Archive .dist/rgy-v0.1.10.3-x86_64-windows.zip
#
# The archive is the published Windows release. Its contents are extracted
# into a stage directory; the MSI takes the five binaries and seven
# documents from there, so the MSI and the zip install the same bytes.
#
# WiX 7 is a dotnet tool, installed here (idempotently) so the CI runner
# needs no preinstalled SDK. WiX 7 requires the OSMF EULA to be accepted
# once per machine -- wix eula accept wix7 -- which the owner explicitly
# authorised. The UI extension is added to the tool's cache the same way.
[cmdletbinding()]
param(
    [Parameter(Mandatory = $true)][string]$Version,
    [Parameter(Mandatory = $true)][string]$Archive,
    [string]$Out = "",
    [switch]$SkipToolInstall
)

$ErrorActionPreference = 'Stop'
$here = $PSScriptRoot
$root = (Resolve-Path (Join-Path $here '..\..')).Path
if (-not $Out) { $Out = Join-Path $root ".dist\rgy-$Version-x86_64.msi" }

# --- 1. the version --------------------------------------------------------
$msiVersion = (& (Join-Path $here 'msi-version.ps1') -Version $Version | Select-Object -Last 1)
if (-not $msiVersion -or $msiVersion -notmatch '^\d+\.\d+\.\d+$') { throw "the version mapping failed for $Version" }
$productCode = (& (Join-Path $here 'msi-product-code.ps1') -Version $Version | Select-Object -Last 1)
if (-not $productCode -or $productCode -notmatch '^\{?[0-9A-F-]{36}\}?$') { throw "the ProductCode could not be derived for $Version" }
Write-Host ">> release $Version -> ProductVersion $msiVersion, ProductCode $productCode"

# --- 2. WiX ----------------------------------------------------------------
if (-not $SkipToolInstall) {
    $wix = Get-Command wix -ErrorAction SilentlyContinue
    if (-not $wix) {
        Write-Host '>> installing the WiX 7 dotnet tool'
        dotnet tool install --global wix --version 7.0.1
        if ($LASTEXITCODE -ne 0) { throw 'the WiX tool could not be installed' }
        $env:PATH = "$env:USERPROFILE\.dotnet\tools;$env:PATH"
    }
}
# Accept the OSMF EULA (a no-op when already accepted) and make sure the
# WixUI extension is in the cache. Both are machine-local and idempotent.
wix eula accept wix7 2>$null | Out-Null
wix extension add WixToolset.UI.wixext/7.0.1 2>$null | Out-Null

# --- 3. the stage ----------------------------------------------------------
if (-not (Test-Path -LiteralPath $Archive -PathType Leaf)) {
    throw "the release archive does not exist: $Archive"
}
$stage = Join-Path $root ".dist\stage-msi-$Version"
if (Test-Path -LiteralPath $stage) { Remove-Item -LiteralPath $stage -Recurse -Force }
New-Item -ItemType Directory -Force -Path $stage | Out-Null
Write-Host ">> extracting $Archive"
Expand-Archive -LiteralPath $Archive -DestinationPath $stage -Force

$needed = @(
    'searchctl.exe','searchd.exe','rgy-autostart.exe','rgxy.exe','rgy.exe',
    'README.md','MANUAL.md','FORMATS.md','COPYING','NOTICE','LICENSE-APACHE','LICENSE-MIT'
)
foreach ($name in $needed) {
    if (-not (Test-Path -LiteralPath (Join-Path $stage $name) -PathType Leaf)) {
        throw "the archive is missing $name; the MSI would install a broken product"
    }
}

# --- 4. wix build ----------------------------------------------------------
New-Item -ItemType Directory -Force -Path (Split-Path $Out -Parent) | Out-Null
$wxs = Join-Path $here 'Product.wxs'
Write-Host '>> wix build'
wix build $wxs `
    -d "MsiVersion=$msiVersion" `
    -d "ProductCode=$productCode" `
    -d "Stage=$stage" `
    -d "MsiDir=$here" `
    -ext WixToolset.UI.wixext `
    -out $Out
if ($LASTEXITCODE -ne 0) { throw "wix build failed ($LASTEXITCODE)" }

if (-not (Test-Path -LiteralPath $Out -PathType Leaf)) { throw "wix build produced no MSI at $Out" }
Write-Host ">> built $Out ($((Get-Item $Out).Length) bytes)"

# --- 5. validate -----------------------------------------------------------
Write-Host '>> wix msi validate'
wix msi validate $Out
if ($LASTEXITCODE -ne 0) { throw "the MSI failed validation ($LASTEXITCODE)" }

Write-Host ">> rgy $Version MSI is ready: $Out"
