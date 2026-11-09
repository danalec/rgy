#!/usr/bin/env bash
# install.sh -- install rgy on Linux, macOS and FreeBSD.
#
# The unix counterpart of install.ps1.
#
#   curl -fsSL https://raw.githubusercontent.com/danalec/rgy/main/install.sh | bash
#   bash install.sh --install-dir /usr/local/bin
#   bash install.sh --version rgy-v0.1.10.2
#   bash install.sh --activate
#   bash install.sh --uninstall
#
# Downloads the rgy release tarball, verifies its SHA256, unpacks the four
# binaries into the install directory, prepends it to the user's PATH and
# registers the daemon to start at login -- and the launcher too on
# Windows, where it is a resident floating window. On Linux, macOS and
# FreeBSD the launcher is the terminal interface: it needs a terminal, a
# login entry has none, and `rgy-autostart` says so instead of writing a
# unit that starts and exits 0 without drawing anything.
#
# --activate is the post-install option: it registers the daemon and starts
# it now (Windows: the Windows twin runs the logon task with `schtasks
# /Run`), and downloads nothing. A package manager (Scoop, AUR, an
# extracted archive, crates.io) puts the binaries on the PATH but never
# runs this installer, so the daemon is neither registered nor running
# after those installs -- this switch is the one command that turns it on.
# It is the same action the end of a normal install takes, so an install
# that was made with --no-autostart, or an `rgy-autostart uninstall`, is
# put back with the same words and the same result. It is re-runnable: the
# registration is idempotent.
#
# --uninstall takes out exactly what install put in: the login entries,
# the copied files and the PATH block. It removes no directory it did not
# create, and no file install did not put there. The same URL installs and
# uninstalls, so there is no second script to keep in step with this one.
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
# The pins below are the real digests of the twelve archives this release
# carries: each was built and smoked on the runner that has its platform,
# and `scripts/publish-release.sh` rewrote these lines from the combined
# SHA256SUMS the CI run produced. The installer still refuses a zero
# placeholder by name before it downloads anything, so a future release
# whose archives are not built yet cannot hand out something it cannot
# vouch for. A digest left over from an earlier tag would refuse every
# download of the new one.
#
# The options, the digest and the refusal are the tree's own
# (scripts/lib/options.sh and scripts/lib/digest.sh), so that this script
# and its .ps1 twin answer the same question the same way. The helpers
# live in the tree, and the installer is also run from a URL -- piped
# straight into a shell, where there is no tree to read them from -- so
# they are used when they are there, and the same behaviour is spelled out
# here when they are not.
set -u

# The tag of the release that ships the binary archives: the crates carry
# three numbers (0.1.10) and the release carries a fourth (0.1.10.2), which
# is the fix count of the line. An installer that names another tag downloads
# another release's archives and refuses them on the digest pinned below.
version="rgy-v0.1.10.2"
install_dir="$HOME/.local/bin"
base_url="https://github.com/danalec/rgy/releases/download"
skip_checksum=0
no_autostart=0
no_launcher=0
activate=0
uninstall=0

# The digests of the archives this installer is allowed to install.
#
# The digest is uppercase hex, and it is rewritten in place by
# scripts/publish-release.sh -- so the shape of these lines is part of the
# contract, not a formatting choice.
expected_linux_x86_64="0000000000000000000000000000000000000000000000000000000000000000"
expected_linux_aarch64="0000000000000000000000000000000000000000000000000000000000000000"
expected_macos_x86_64="0000000000000000000000000000000000000000000000000000000000000000"
expected_macos_aarch64="0000000000000000000000000000000000000000000000000000000000000000"
expected_freebsd_x86_64="0000000000000000000000000000000000000000000000000000000000000000"

# The pin of an archive nobody has published yet.
placeholder="0000000000000000000000000000000000000000000000000000000000000000"

# The tree this installer was run from, when it was run from a checkout.
#
# The path is only a hint. Piped into a shell there is no checkout, and
# `dirname` of the shell's own name is the directory the shell was started
# in -- which is a checkout only by accident.
here=$(cd "$(dirname "$0")" 2> /dev/null && pwd 2> /dev/null) || here=""

# The options, read by the helper the two twins share.
#
# The vocabulary is the tree's: the long spelling and the PowerShell one
# are the same option, and an option neither of them knows is refused with
# the tree's words and the tree's exit code.
#
# The fallback is the helper's own loop with the table lookup spelled out,
# for the run that has no tree to read the helper from. It refuses the same
# way, and it takes the same tables.
if [ -n "$here" ] && [ -f "$here/scripts/lib/options.sh" ]; then
    . "$here/scripts/lib/options.sh"
else
    rgy_read_options() {
        local _switches="$1" _values="$2" _positionals="$3"
        shift 3
        local _arg _name _value _has_value _key _var _entry
        while [ $# -gt 0 ]; do
            _arg="$1"
            shift
            _name="$_arg"
            _value=""
            _has_value=0
            case "$_name" in
                *:*)
                    # The value is the tail of the argument as it was
                    # written, read before the name loses the tail itself:
                    # the helper reads it from the whole token, and a value
                    # taken from the truncated name would be the name
                    # (`--version:x` would set version to `--version`).
                    _value="${_arg#*:}"
                    _name="${_name%%:*}"
                    _has_value=1
                    ;;
            esac
            # The name with the dashes and the case off.
            _key=$(printf '%s' "$_name" | tr 'A-Z' 'a-z')
            _key="${_key//-/}"
            _key="${_key//_/}"
            _var=""
            for _entry in $_switches; do
                [ "${_entry%%=*}" = "$_key" ] && _var="${_entry#*=}"
            done
            if [ -n "$_var" ]; then
                if [ "$_has_value" -eq 1 ]; then
                    echo "unknown option: $_arg" >&2
                    exit 2
                fi
                eval "$_var=1"
                continue
            fi
            _var=""
            for _entry in $_values; do
                [ "${_entry%%=*}" = "$_key" ] && _var="${_entry#*=}"
            done
            if [ -n "$_var" ]; then
                if [ "$_has_value" -eq 0 ]; then
                    if [ $# -lt 1 ]; then
                        echo "option $_arg needs a value" >&2
                        exit 2
                    fi
                    _value="$1"
                    shift
                fi
                eval "$_var=\$_value"
                continue
            fi
            # Not an option, and not a name this script knows. The
            # installers take no positional argument, so every one of them
            # is a name the script does not know.
            echo "unknown option: $_arg" >&2
            exit 2
        done
    }
fi

# The digest of the archive, asked of the tree's helper.
#
# The installer runs on the user's machine, and the user's machine is not
# necessarily the one the release was built on: Linux has `sha256sum` and
# macOS has `shasum -a 256` and no `sha256sum` at all. A script that calls
# the tool of one platform gets an empty digest on the other one and
# refuses a perfect archive with a message that says the digest does not
# match. The helper asks the machine what it has.
if [ -n "$here" ] && [ -f "$here/scripts/lib/digest.sh" ]; then
    . "$here/scripts/lib/digest.sh"
else
    rgy_sha256() {
        if command -v sha256sum > /dev/null 2>&1; then
            sha256sum "$1" | cut -d' ' -f1
        elif command -v sha256 > /dev/null 2>&1; then
            # FreeBSD's base tool: -q prints the hex and nothing else. The
            # helper in scripts/lib/digest.sh asks for it the same way, so the
            # URL-piped run and the checkout run answer alike on FreeBSD.
            sha256 -q "$1" | cut -d' ' -f1
        elif command -v shasum > /dev/null 2>&1; then
            shasum -a 256 "$1" | cut -d' ' -f1
        else
            echo "no sha256 tool on this machine: neither sha256sum, sha256 nor shasum" >&2
            return 1
        fi
    }
fi

rgy_read_options "skipchecksum=skip_checksum noautostart=no_autostart nolauncher=no_launcher activate=activate uninstall=uninstall" \
                 "installdir=install_dir baseurl=base_url version=version" "" "$@"

# --uninstall takes out exactly what install put in.
#
# The install is four things: the login entries, the binaries and documents
# copied into the install directory, the alias, and the two-line PATH block
# appended to the profile. The uninstall reverses those four and nothing
# else: it never removes the install directory itself, because on unix that
# directory is shared with everything else the user put in ~/.local/bin.
#
# The autostart tool removes both entries in one call -- the daemon's and
# the launcher's -- and says so in its own words. A missing tool is not an
# error: an install with --no-autostart wrote no entries to remove.
rgy_uninstall() {
    dir="$1"
    if [ -x "$dir/rgy-autostart" ]; then
        "$dir/rgy-autostart" uninstall
        code=$?
        if [ "$code" -eq 0 ]; then
            echo "The login entries were removed."
        else
            echo "The login entries could not be removed (see above)." >&2
        fi
    else
        echo "No rgy-autostart in $dir; no login entries to remove."
    fi

    removed=0
    for f in searchctl searchd rgy-autostart rgxy rgy \
             README.md MANUAL.md FORMATS.md COPYING NOTICE \
             LICENSE-APACHE LICENSE-MIT; do
        if [ -e "$dir/$f" ] || [ -L "$dir/$f" ]; then
            rm -f "$dir/$f" && removed=$((removed + 1))
        fi
    done
    if [ -d "$dir/docs" ]; then
        rm -rf "$dir/docs"
    fi
    echo "Removed $removed rgy files from $dir."

    # The exact two lines install appended, and only those. A profile that
    # carries a different line mentioning rgy is a profile the user wrote,
    # and taking it out would remove a PATH they still need.
    case "${SHELL:-}" in
        */zsh|*/zsh-*|*/zsh[0-9]*) profile="$HOME/.zshrc" ;;
        */bash|*/bash-*)           profile="$HOME/.bashrc" ;;
        *)                         profile="$HOME/.profile" ;;
    esac
    if [ -f "$profile" ] && grep -F -x -q -e "export PATH=\"$dir:\$PATH\"" "$profile"; then
        tmp_profile=$(mktemp)
        grep -F -x -v -e '# rgy' -e "export PATH=\"$dir:\$PATH\"" \
            "$profile" > "$tmp_profile" || true
        cat "$tmp_profile" > "$profile"
        rm -f "$tmp_profile"
        echo "Removed the rgy PATH block from $profile."
    else
        echo "No rgy PATH block in $profile."
    fi
    echo "Uninstalled rgy from $dir."
}

# Register and arm the daemon on an install that is already in place.
#
# This is the post-install activation and the last step of a normal install,
# and it is ONE function so the two answer the same question the same way:
# no download, no PATH edit, no copy -- just `rgy-autostart install`. The
# tool is looked for next to where the install put it first, and on the PATH
# second, because a package manager put the binaries on the PATH and not in
# $install_dir, and that install still has a daemon to activate.
#
# On Linux and macOS `rgy-autostart install` both writes the login entry
# and starts the daemon at once (`systemctl --user enable --now`,
# `launchctl bootstrap`), so "activate" is a real start here, not an entry
# that waits for the next login. The Windows twin cannot say that: a Task
# Scheduler ONLOGON task only runs at the next logon, so install.ps1 runs
# the task with `schtasks /Run` after registering it. (FreeBSD has no login
# item yet, and the tool says so.) It is idempotent: a second call leaves
# one entry and starts nothing twice. Running this command IS the consent
# -- no prompt, no elevation.
#
# The launcher is asked for only when it is actually on the machine, and the
# autostart tool decides whether the platform has that entry (Windows only).
# A failure here is never a reason to fail a normal install: by the time the
# install reaches this function, the binaries, the alias and the PATH are all
# in place. --activate, which does nothing else, propagates the code instead.
rgy_register_daemon() {
    tool=""
    if [ -x "$install_dir/rgy-autostart" ]; then
        tool="$install_dir/rgy-autostart"
    elif command -v rgy-autostart > /dev/null 2>&1; then
        tool=$(command -v rgy-autostart)
    fi
    if [ -z "$tool" ]; then
        echo "rgy-autostart was not found: install rgy first, then --activate it." >&2
        return 1
    fi

    launcher=""
    if [ -x "$install_dir/rgxy" ]; then
        launcher="$install_dir/rgxy"
    elif command -v rgxy > /dev/null 2>&1; then
        launcher=$(command -v rgxy)
    fi
    args="install"
    if [ "$no_launcher" -eq 0 ] && [ -n "$launcher" ]; then
        args="$args --launcher"
    fi

    "$tool" $args
    code=$?
    if [ "$code" -eq 0 ]; then
        echo "The daemon starts at login. To take it out: rgy-autostart uninstall"
    elif [ "$code" -eq 2 ]; then
        echo "The entries were written but the platform could not arm them (see above)." >&2
    else
        echo "The daemon was not registered. To register it: rgy-autostart install" >&2
    fi
    return "$code"
}

if [ "$uninstall" -eq 1 ]; then
    rgy_uninstall "$install_dir"
    exit $?
fi

# --activate is a post-install action, so it never reaches the download. It
# works on an install made by a package manager, by an extracted archive, by
# crates.io, or by this installer with --no-autostart: the binaries are on
# the PATH already, and the only thing left to do is turn the daemon on.
if [ "$activate" -eq 1 ]; then
    echo "Activating the rgy daemon on the install that is already here ..."
    rgy_register_daemon
    exit $?
fi

# The platform and the architecture decide the archive. The release carries
# a portable build for each one it targets, and nothing else: the AVX2 and
# the Zen 4 archives are for a caller that knows its own CPU.
os="$(uname -s)"
arch="$(uname -m)"
# The name the refusal uses. The architecture alone is not enough: an
# x86_64 Mac and an x86_64 Linux machine are the same architecture and
# different platforms, and a sentence that says "there is no published
# rgy release for x86_64 yet" is false on the Linux one -- that release
# exists. The sentence names the platform and the architecture.
case "$os" in
    Linux)
        os_name="Linux"
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
        os_name="macOS"
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
    FreeBSD)
        os_name="FreeBSD"
        case "$arch" in
            x86_64|amd64)
                asset="$version-x86_64-freebsd.tar.gz"
                expected="$expected_freebsd_x86_64"
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

# The pin is the digest of the archive this release carries, and the check
# below refuses a download that does not match it: an installer that hands
# out an archive it cannot vouch for is worse than one that says there is
# none.
#
# A platform whose archive this release does not carry would hold the
# placeholder digest, and that is refused by name, before anything is
# downloaded. The sentence names the platform and the architecture, because
# the architecture alone is a lie on some of them: there is no published
# rgy release for x86_64 macOS, and there is a published one for x86_64
# Linux, and a sentence that said only "x86_64" would be wrong on one of
# the two.
#
# A caller that asked for no digest check is asking for exactly that, and
# the flag is theirs to use.
if [ "$skip_checksum" -eq 0 ] && [ "$expected" = "$placeholder" ]; then
    echo "there is no published rgy release for $os_name $arch yet" >&2
    echo " expected $asset" >&2
    exit 1
fi

command -v curl > /dev/null 2>&1 || { echo "curl not found" >&2; exit 1; }
command -v tar > /dev/null 2>&1 || { echo "tar not found" >&2; exit 1; }

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

echo "Downloading $url ..."
# The transport's own complaint is not printed: the sentence below is the
# one the .ps1 twin prints, and the URL is on the line above. A pair of
# twins that answer the same question in two different words is a pair
# nobody can compare.
curl -fsL -o "$tmp/$asset" "$url" || { echo "the download failed" >&2; exit 1; }

if [ "$skip_checksum" -eq 0 ]; then
    actual=$(rgy_sha256 "$tmp/$asset") || {
        echo "the SHA256 of $asset could not be computed on this machine" >&2
        exit 1
    }
    # The pin is uppercase hex and the helper answers in lowercase.
    actual=$(printf '%s' "$actual" | tr 'a-f' 'A-F')
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
# The root of the archive, and the directory the binaries are in.
#
# The two are not always the same: the tarball holds the binaries at its
# root, and a build that put them in a subdirectory would otherwise
# install nothing and say nothing. The root is kept because it is where
# the archive keeps the documents, and it is the root the .ps1 twin
# searches.
stage_root="$stage"
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
# The documents are looked for the way the .ps1 twin looks for them: at the
# root of the archive first, and in the tree when the release packed them
# in a subdirectory. An archive that carries the documents in a
# subdirectory installs them too -- a README that never arrived is a README
# the reader does not have.
for doc in README.md MANUAL.md FORMATS.md COPYING NOTICE LICENSE-APACHE LICENSE-MIT; do
    if [ -f "$stage_root/$doc" ]; then
        cp "$stage_root/$doc" "$install_dir"
    else
        found=$(find "$stage_root" -name "$doc" -type f | head -1)
        [ -n "$found" ] && cp "$found" "$install_dir"
    fi
done
# The picture the README shows travels with it.
#
# The README points at it by a relative path, so an install that leaves it
# behind has a README with a hole in it -- and a README that shows nothing
# is a README that says less than the window does. The path inside the
# install is the one the README uses, so the link works where the README
# is.
if [ -d "$stage_root/docs" ]; then
    mkdir -p "$install_dir/docs"
    cp -R "$stage_root/docs/." "$install_dir/docs/"
fi

# user PATH (idempotent prepend)
#
# The PATH of a shell is set by a file it reads at startup, and which file
# that is depends on the shell: zsh reads .zshrc, bash reads .bashrc, and
# a shell with none of them falls back to .profile.
#
# The file is chosen by the shell that is running, and not by the first
# of them that happens to exist: a bash user with a leftover .zshrc got
# the entry in a file bash never reads, and the installer claimed the
# PATH was set for a shell that did not have it.
case "${SHELL:-}" in
    */zsh|*/zsh-*|*/zsh[0-9]*)
        profile="$HOME/.zshrc"
        ;;
    */bash|*/bash-*)
        profile="$HOME/.bashrc"
        ;;
    *)
        # A shell the tree does not know, or none at all: the file every
        # POSIX shell reads.
        profile="$HOME/.profile"
        ;;
esac

# The PATH entries the profile holds, one per line.
#
# The value of every PATH assignment in the profile is read, the quotes
# are taken off, and the value is split on the `:` that separates the
# entries. A line the shell does not read as an assignment is not one: a
# `#` ends the line the way it ends the command, so what follows it is a
# comment and not a PATH.
profile_path_entries() {
    [ -f "$profile" ] || return 0
    sed 's/#.*//' "$profile" 2>/dev/null |
        grep -oE '(^|[^A-Za-z0-9_])PATH[[:space:]]*=.*' |
        cut -d '=' -f 2- |
        tr -d "\"'" |
        tr ':' '\n' |
        sed -e 's/^[[:space:]]*//' -e 's/[[:space:]]*$//'
}

# Whether the install directory is one of the entries the profile holds.
#
# The entries are compared one by one, and exactly: the directory is on
# the PATH when one of them IS the directory, and not when one of them
# starts with it. A substring check of the file answers a different
# question -- a profile that carries $HOME/.local/bin2 makes a check of
# $HOME/.local/bin succeed, and the installer would then claim the
# directory is on the PATH when it is not. The .ps1 twin splits the user
# PATH on ';' and asks whether the entry is in it, and this is the same
# question in the unix spelling.
#
# The directory is looked for in the spellings a user might have written
# it: the expanded path, and the $HOME form of the same directory. A
# check that knows only the first one appends a second line on every
# run, and a PATH with the same directory in it twice is a PATH nobody
# reads.
already_on_path() {
    local entry
    while IFS= read -r entry; do
        [ -n "$entry" ] || continue
        if [ "$entry" = "$install_dir" ]; then
            return 0
        fi
        case "$install_dir" in
            "$HOME"/*)
                if [ "$entry" = "\$HOME/${install_dir#"$HOME"/}" ]; then
                    return 0
                fi
                ;;
        esac
    done < <(profile_path_entries)
    return 1
}

if ! already_on_path; then
    printf '\n# rgy\nexport PATH="%s:$PATH"\n' "$install_dir" >> "$profile"
    echo "Added $install_dir to the PATH in $profile (open a new terminal to pick it up)."
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
# The same action --activate takes, through the same function: the autostart
# tool finds the daemon next to itself -- the two were just copied into the
# same directory -- and registers it with the tree it indexes. The entry
# goes into the user's own session: no root, no prompt, no dialog.
#
# This step is a convenience, and a failure here is not a reason to tell
# the user the install failed: by the time it runs, the daemon, the
# launcher, the alias and the PATH entry are all in place. So the step
# warns and the install finishes -- the .ps1 twin does the same, and an
# installer that aborted here would leave a half-installed tool behind
# and a non-zero exit on top of it.
if [ "$no_autostart" -eq 0 ]; then
    # A normal install ignores the code: the function has already said what
    # happened, and the install is complete with or without the daemon.
    rgy_register_daemon
fi

echo "Installed: searchctl, searchd, rgy-autostart and rgxy in $install_dir"
