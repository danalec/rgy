# The SHA256 of a file, in the tool the platform has.
#
# The twin of `scripts/lib/digest.sh`: the answer is the same on both
# platforms, and the answer is the lowercase hex of the digest -- the same
# as `sha256sum -c` reads it back, and the same as the unix twin's
# `shasum -a 256` on the platform that has no `sha256sum`.

function Get-RgySha256([string]$path) {
    (Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash.ToLower()
}
