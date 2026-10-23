#!/usr/bin/env bash
# install.sh -- install rgy on Linux.
#
# The Linux counterpart of install.ps1.
#
#   curl -fsSL https://raw.githubusercontent.com/danalec/rgy/main/install.sh | bash
#   bash install.sh --install-dir /usr/local/bin
#   bash install.sh --version rgy-v0.1.0
#
# Downloads the rgy release tarball, verifies its SHA256, unpacks the three
# binaries into the install directory and prepends it to the user's PATH.
#
# The digest is PINNED HERE, exactly as install.ps1 pins the Windows zip.
# That is the whole point of a pin: the installer carries the digest of the
# artifact it is allowed to install, so a tampered or substituted download
# is refused before anything is unpacked.
#
# The pins are release data, and the release owns them: scripts/publish-release.sh
# rewrites them from the artifacts it has just built, and refuses to finish
# if the rewrite did not move. A hand-maintained table is a table that is
# wrong on the day after the release.
#
# Until the first release is published the pin below is a placeholder, and
# the installer refuses the download it cannot vouch for. That is the
# honest answer, not a broken script.
set -u

version="rgy-v0.1.0"
install_dir="$HOME/.local/bin"
base_url="https://github.com/danalec/rgy/releases/download"
skip_checksum=0

# The asset of this platform, and the digest of that asset.
#
# The digest is uppercase hex, and it is rewritten in place by
# scripts/publish-release.sh -- so the shape of this line is part of the
# contract, not a formatting choice.
asset="$version-x86_64-linux.tar.gz"
expected="0000000000000000000000000000000000000000000000000000000000000000"

while [ $# -gt 0 ]; do
    case "$1" in
        --install-dir) install_dir=${2:-}; shift 2 ;;
        --base-url) base_url=${2:-}; shift 2 ;;
        --version) version=${2:-}; shift 2 ;;
        --skip-checksum) skip_checksum=1; shift ;;
        *) echo "unknown option: $1" >&2; exit 2 ;;
    esac
done

asset="$version-x86_64-linux.tar.gz"
url="$base_url/$version/$asset"

command -v curl > /dev/null 2>&1 || { echo "curl not found" >&2; exit 1; }
command -v tar > /dev/null 2>&1 || { echo "tar not found" >&2; exit 1; }

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

echo "Downloading $url ..."
curl -fsSL -o "$tmp/$asset" "$url" || { echo "the download failed" >&2; exit 1; }

if [ "$skip_checksum" -eq 0 ]; then
    actual=$(sha256sum "$tmp/$asset" | awk '{print toupper($1)}')
    if [ "$actual" != "$expected" ]; then
        echo "SHA256 mismatch for $asset" >&2
        echo " expected $expected" >&2
        echo " got      $actual" >&2
        exit 1
    fi
    echo "SHA256 verified."
else
    echo "SHA256 verification skipped."
fi
stage="$tmp/stage"
mkdir -p "$stage"
tar -xzf "$tmp/$asset" -C "$stage"
# The tarball holds the binaries at its root; a build that put them in a
# subdirectory would otherwise install nothing and say nothing.
for exe in searchctl searchd rgy-autostart; do
    if [ ! -f "$stage/$exe" ]; then
        found=$(find "$stage" -name "$exe" -type f | head -1)
        [ -n "$found" ] || { echo "no $exe in $asset" >&2; exit 1; }
        stage=$(dirname "$found")
        break
    fi
done

mkdir -p "$install_dir"
for exe in searchctl searchd rgy-autostart; do
    [ -f "$stage/$exe" ] || { echo "no $exe in $asset" >&2; exit 1; }
    cp "$stage/$exe" "$install_dir/$exe"
    chmod +x "$install_dir/$exe"
done
for doc in README.md MANUAL.md FORMATS.md COPYING NOTICE; do
    [ -f "$stage/$doc" ] && cp "$stage/$doc" "$install_dir"
done

# user PATH (idempotent prepend)
profile="$HOME/.profile"
if [ -f "$HOME/.bashrc" ]; then
    profile="$HOME/.bashrc"
fi
if ! grep -qF "$install_dir" "$profile" 2>/dev/null; then
    printf '\n# rgy\nexport PATH="%s:$PATH"\n' "$install_dir" >> "$profile"
    echo "Added $install_dir to the PATH (open a new terminal to pick it up)."
else
    echo "$install_dir is already on the PATH."
fi

echo "Installed: searchctl, searchd and rgy-autostart in $install_dir"
