# msi-version.ps1 -- map a four-number rgy release to a three-field
# Windows Installer ProductVersion.
#
# WHY: MSI ignores the fourth field of ProductVersion. 0.1.10.0 and
# 0.1.10.1 are the SAME version to Windows Installer, so an upgrade from
# one fix release to the next would silently do nothing. The release's fix
# count is folded into the third field instead:
#
#   0.1.10.0 -> 0.1.100
#   0.1.10.1 -> 0.1.101
#   0.1.10.3 -> 0.1.103
#   0.1.11.0 -> 0.1.110
#
# The third field is patch*10 + fix, which keeps the ordering of releases
# as long as the fix count stays below ten. A fix count of ten or more is
# refused rather than producing a ProductVersion that would sort below a
# later patch's. The fourth field is also capped at 65535 by MSI's
# ProductVersion rules (major and minor are capped at 255, build at 65535).

param(
    [Parameter(Mandatory = $true)]
    [string]$Version
)

$parts = $Version -split '\.'
if ($parts.Count -lt 3 -or $parts.Count -gt 4) {
    [Console]::Error.WriteLine("the release version must have three or four numbers: $Version")
    exit 1
}
$numbers = @()
foreach ($part in $parts) {
    if ($part -notmatch '^\d+$') {
        [Console]::Error.WriteLine("the release version has a non-numeric field: $Version")
        exit 1
    }
    $numbers += [int]$part
}
if ($numbers[0] -gt 255 -or $numbers[1] -gt 255) {
    [Console]::Error.WriteLine("major and minor must fit in 255: $Version")
    exit 1
}
$fix = if ($numbers.Count -eq 4) { $numbers[3] } else { 0 }
if ($fix -ge 10) {
    [Console]::Error.WriteLine("the fix count must be below ten for the third-field mapping: $Version")
    exit 1
}
$third = ($numbers[2] * 10) + $fix
if ($third -gt 65535) {
    [Console]::Error.WriteLine("the mapped ProductVersion would exceed 65535 in its third field: $Version")
    exit 1
}
"$($numbers[0]).$($numbers[1]).$third"
