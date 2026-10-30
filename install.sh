#!/usr/bin/env bash
# install.sh -- install rgy on Linux and macOS.
#
# The unix counterpart of install.ps1.
#
#   curl -fsSL https://raw.githubusercontent.com/danalec/rgy/main/install.sh | bash
#   bash install.sh --install-dir /usr/local/bin
#   bash install.sh --version rgy-v0.1.1
#
# Downloads the rgy release tarball, verifies its SHA256, unpacks the four
# binaries into the install directory, prepends it to the user's PATH and
# registers the daemon -- and the launcher -- to start at login.
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
# There is one pin per platform and architecture, because they decide the
# archive: an aarch64 machine does not run the x86_64 binary without an
# emulator, and an installer that hands it one installs something that will
# not start.
#
# Until the first release is published the pins below are placeholders, and
# the installer says so instead of downloading something it cannot vouch
# for. That is the honest answer, not a broken script.
set -u

version="rgy-v0.1.1"
install_dir="$HOME/.local/bin"
base_url="https://github.com/danalec/rgy/releases/download"
skip_checksum=0
autostart=1
launcher=1

# The digests of the archives this installer is allowed to install.
#
# The digest is uppercase hex, and it is rewritten in place by
# scripts/publish-release.sh -- so the shape of these lines is part of the
# contract, not a formatting choice.
expected_linux_x86_64="0000000000000000000000000000000000000000000000000000000000000000"
expected_linux_aarch64="0000000000000000000000000000000000000000000000000000000000000000"
expected_macos_x86_64="0000000000000000000000000000000000000000000000000000000000000000"
expected_macos_aarch64="0000000000000000000000000000000000000000000000000000000000000000"

# The pin of an archive nobody has published yet.
placeholder="0000000000000000000000000000000000000000000000000000000000000000"

while [ $# -gt 0 ]; do
    case "$1" in
        --install-dir) install_dir=${2:-}; shift 2 ;;
        --base-url) base_url=${2:-}; shift 2 ;;
        --version) version=${2:-}; shift 2 ;;
        --skip-checksum) skip_checksum=1; shift ;;
        --no-autostart) autostart=0; shift ;;
        --no-launcher) launcher=0; shift ;;
        *) echo "unknown option: $1" >&2; exit 2 ;;
    esac
done

# The platform and the architecture decide the archive. The release carries
# a portable build for each one it targets, and nothing else: the AVX2 and
# the Zen 4 archives are for a caller that knows its own CPU.
os="$(uname -s)"
arch="$(uname -m)"
case "$os" in
    Linux)
        case "$arch" in
            x86_64|amd64)
                asset="$version-x86_64-linux.tar.gz"
                expected="$expected_linux_x86_64"
                ;;
            aarch64|arm64)
                asset="$version-aarch64-linux.tar.gz"
                expected="$expected_linux_aarch64"
                ;;
            *)
                echo "there is no rgy release for this architecture: $arch" >&2
                exit 1
                ;;
        esac
        ;;
    Darwin)
        case "$arch" in
            x86_64)
                asset="$version-x86_64-apple-darwin.tar.gz"
                expected="$expected_macos_x86_64"
                ;;
            arm64|aarch64)
                asset="$version-aarch64-apple-darwin.tar.gz"
                expected="$expected_macos_aarch64"
                ;;
            *)
                echo "there is no rgy release for this architecture: $arch" >&2
                exit 1
                ;;
        esac
        ;;
    *)
        echo "there is no rgy release for this operating system: $os" >&2
        exit 1
        ;;
esac
url="$base_url/$version/$asset"

# The pin is still a placeholder: no release has been published for this
# platform yet. The installer says so in words, instead of downloading
# something it cannot vouch for. A caller that asked for no digest check
# is asking for exactly that, and the flag is theirs to use.
if [ "$skip_checksum" -eq 0 ] && [ "$expected" = "$placeholder" ]; then
    echo "there is no published rgy release for $arch yet" >&2
    echo " expected $asset" >&2
    exit 1
fi

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
for exe in searchctl searchd rgy-autostart rgxy; do
    if [ ! -f "$stage/$exe" ]; then
        found=$(find "$stage" -name "$exe" -type f | head -1)
        [ -n "$found" ] || { echo "no $exe in $asset" >&2; exit 1; }
        stage=$(dirname "$found")
        break
    fi
done

mkdir -p "$install_dir"
for exe in searchctl searchd rgy-autostart rgxy; do
    [ -f "$stage/$exe" ] || { echo "no $exe in $asset" >&2; exit 1; }
    cp "$stage/$exe" "$install_dir/$exe"
    chmod +x "$install_dir/$exe"
done
for doc in README.md MANUAL.md FORMATS.md COPYING NOTICE; do
    [ -f "$stage/$doc" ] && cp "$stage/$doc" "$install_dir"
done

# user PATH (idempotent prepend)
#
# The PATH of a shell is set by a file it reads at startup, and which file
# that is depends on the shell: zsh reads .zshrc (and .zprofile when it is
# a login shell, which is what a terminal opens), bash reads .bashrc, and
# a shell with none of them falls back to .profile. The candidates are
# tried in the order of what the user's terminal is most likely to read.
profile=""
for candidate in "$HOME/.zshrc" "$HOME/.zprofile" "$HOME/.bashrc" "$HOME/.profile"; do
    if [ -f "$candidate" ]; then
        profile="$candidate"
        break
    fi
done
if [ -z "$profile" ]; then
    profile="$HOME/.profile"
fi
# The directory is looked for in the spellings a user might have written
# it: the expanded path, and the $HOME form of the same directory. A
# check that knows only the first one appends a second line on every
# run, and a PATH with the same directory in it twice is a PATH nobody
# reads.
already_on_path() {
    if grep -qF "$install_dir" "$profile" 2>/dev/null; then
        return 0
    fi
    case "$install_dir" in
        "$HOME"/*)
            grep -qF "\$HOME/${install_dir#"$HOME"/}" "$profile" 2>/dev/null
            return $?
            ;;
    esac
    return 1
}

if ! already_on_path; then
    printf '\n# rgy\nexport PATH="%s:$PATH"\n' "$install_dir" >> "$profile"
    echo "Added $install_dir to the PATH (open a new terminal to pick it up)."
else
    echo "$install_dir is already on the PATH."
fi

# The command the project is named after.
#
# The alias travels with the release: it is a link in the archive, and it
# is copied as a link. A copy of the binary would be a second binary to
# keep in step with the first.
#
# The alias is optional. The four binaries above are the product, and an
# archive that does not carry the alias still installs them: an installer
# that aborts over a second name leaves a half-installed tree behind, and
# the .ps1 twin does not abort either. It says the same thing this one
# says, and the install goes on.
#
# The alias is looked for where the archive keeps it, the same way the
# .ps1 twin looks: at the root first, and in the tree when the release
# packed it in a subdirectory.
alias_src=""
if [ -e "$stage/rgy" ] || [ -L "$stage/rgy" ]; then
    alias_src="$stage/rgy"
else
    alias_src=$(find "$stage" -name rgy \( -type f -o -type l \) | head -1)
fi
if [ -n "$alias_src" ]; then
    cp -P "$alias_src" "$install_dir/rgy"
    echo "Added the rgy command: it runs searchctl."
else
    echo "the archive carries no rgy command: the install continues without it." >&2
fi

# The daemon at login.
#
# The autostart tool finds the daemon next to itself -- the two were just
# copied into the same directory -- and registers it with the tree it
# indexes. The entry goes into the user's own session: no root, no prompt,
# no dialog.
#
# The launcher is registered with it, under its own name: the window is
# there in a few milliseconds because the *daemon* has the index warm, not
# because the launcher built one.
if [ "$autostart" -eq 1 ]; then
    # The launcher is registered with the daemon, under its own name, and
    # only when it is actually on the machine: an entry that points at a
    # binary that is not there is an entry that fails at every login.
    args="install"
    if [ "$launcher" -eq 1 ] && [ -x "$install_dir/rgxy" ]; then
        args="$args --launcher"
    fi
    "$install_dir/rgy-autostart" $args
    code=$?
    if [ "$code" -eq 0 ]; then
        echo "The daemon starts at login. To take it out: rgy-autostart uninstall"
        case "$args" in
            *--launcher*) echo "The launcher starts at login too: Control twice opens the window." ;;
        esac
    elif [ "$code" -eq 2 ]; then
        echo "The entries were written but the platform could not arm them (see above)." >&2
    else
        echo "The daemon was not registered. To register it: rgy-autostart install" >&2
    fi
fi

echo "Installed: searchctl, searchd, rgy-autostart and rgxy in $install_dir"
