# msi-product-code.ps1 -- a STABLE ProductCode for one rgy release.
#
# WHY: ProductCode="*" makes WiX generate a new GUID on every build. Two
# builds of the same version then install as two different products that
# share the same component GUIDs, and Windows Installer refuses to remove
# either one ("another client exists"). A release must have exactly one
# ProductCode, derived deterministically from the release version, so a
# rebuild produces the same package and a reinstall upgrades cleanly.
#
# This is a version-5-style UUID over the name "rgy/<version>", using the
# product's UpgradeCode GUID as the namespace. SHA-1 is used only as a
# stable name-to-bytes function, not for security.
param([Parameter(Mandatory = $true)][string]$Version)

$namespace = [guid]'B6F1A2C4-7D3E-4A19-9C58-2E0B6D4F8A31'
$name = "rgy/$Version"

$ns = $namespace.ToByteArray()
# The namespace bytes must be in network (big-endian) order for RFC 4122.
$network = [byte[]]@(
    $ns[3],$ns[2],$ns[1],$ns[0],
    $ns[5],$ns[4],$ns[7],$ns[6],
    $ns[8],$ns[9],$ns[10],$ns[11],$ns[12],$ns[13],$ns[14],$ns[15]
)
$bytes = [byte[]]($network + [Text.Encoding]::UTF8.GetBytes($name))
$sha = [System.Security.Cryptography.SHA1]::Create().ComputeHash($bytes)
$h = [byte[]]@($sha[0..15])
$h[6] = ($h[6] -band 0x0f) -bor 0x50   # version 5
$h[8] = ($h[8] -band 0x3f) -bor 0x80   # RFC 4122 variant
([guid]$h).ToString().ToUpper()
