# The release's version, out of the tree.
#
# The manifests carry three numbers -- the registry takes three and nothing
# else -- and the release may carry a fourth, the fix count of the line. The
# fourth lives in the name of the release page and nowhere else in the tree:
# `docs/release-0.1.9.0.md` is the 0.1.9.0 release and `docs/release-0.1.9.md`
# is the 0.1.9 one. So the page the tree carries is what says which release of
# the line this tree is, and a caller that reads the manifest alone asks for a
# page the tree does not have.
#
# That is the failure this exists for: the archive job of the CI read the
# manifest, asked for `docs/release-0.1.9.md`, and every one of the eleven
# rows stopped with "the release carries docs/release-0.1.9.md and this tree
# does not have it" -- while the page the release carries was
# `docs/release-0.1.9.0.md`, the name the tag and the archives have.
#
#     version=$(rgy_release_version "$repo" "$base")
#
# The three-number page when the tree has one, and the highest page of the
# line when it does not. The PowerShell twin is version.ps1, and the two
# answer the same way -- run as a command, either one prints the version of
# the tree it sits in, which is how the CI asks the tree instead of reading
# the manifest again.

rgy_release_version() {
    root=$1
    base=$2
    if [ -f "$root/docs/release-$base.md" ]; then
        printf '%s\n' "$base"
        return 0
    fi
    best=""
    for page in "$root/docs/release-$base".*.md; do
        [ -f "$page" ] || continue
        fix=${page##*/release-$base.}
        fix=${fix%.md}
        [ -n "$fix" ] || continue
        case "$fix" in
            *[!0-9]*) continue ;;
        esac
        if [ -z "$best" ] || [ "$fix" -gt "$best" ]; then
            best=$fix
        fi
    done
    if [ -n "$best" ]; then
        printf '%s.%s\n' "$base" "$best"
        return 0
    fi
    echo "the tree carries no page for $base: neither docs/release-$base.md nor docs/release-$base.<n>.md" >&2
    return 1
}

# Run as a command, it prints the version of the tree it is in -- `bash
# scripts/lib/version.sh` -- so a caller that has no version in hand asks the
# same file the scripts source.
if [ "${BASH_SOURCE[0]}" = "$0" ]; then
    here=$(cd "$(dirname "$0")" && pwd)
    root=$(cd "$here/../.." && pwd)
    base=$(sed -n 's/^version = "\(.*\)"/\1/p' "$root/bins/searchctl/Cargo.toml" | head -1)
    [ -n "$base" ] || { echo "no version in bins/searchctl/Cargo.toml" >&2; exit 1; }
    rgy_release_version "$root" "$base" || exit 1
fi
