# The SHA256 of a file, in the tool the platform has.
#
# Linux has `sha256sum`. macOS has `shasum -a 256` and no `sha256sum` at
# all -- the runner images do not carry coreutils -- and a script that
# calls a tool the platform does not have gets an empty answer and not an
# error: the digest of the release comes out empty, the row in
# SHA256SUMS carries nothing, and the pin is a row that verifies nothing.
#
# FreeBSD has neither of the two: the base system has `sha256`, whose `-q`
# is the hex and nothing else. It is the third tool, and it gives the same
# answer.
#
# The answer is the same on all three platforms: the lowercase hex of the
# digest, and the same as `sha256sum -c` reads it back.

rgy_sha256() {
    if command -v sha256sum > /dev/null 2>&1; then
        sha256sum "$1" | cut -d' ' -f1
    elif command -v sha256 > /dev/null 2>&1; then
        sha256 -q "$1" | cut -d' ' -f1
    elif command -v shasum > /dev/null 2>&1; then
        shasum -a 256 "$1" | cut -d' ' -f1
    else
        echo "no sha256 tool on this machine: neither sha256sum, sha256 nor shasum" >&2
        return 1
    fi
}
