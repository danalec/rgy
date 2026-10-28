#!/usr/bin/env bash
# publish-release.sh -- publish an rgy release end to end, from Linux or macOS.
#
# The Linux/macOS counterpart of scripts/publish-release.ps1.
#
#   bash scripts/publish-release.sh                 # full pipeline
#   bash scripts/publish-release.sh --skip-tests    # trust the build
#   bash scripts/publish-release.sh --dry-run       # show, don't touch
#   bash scripts/publish-release.sh --private       # keep the GitHub mirror private
#
# Pipeline: build this platform's archive -> tag rgy-v<version> + push ->
# create or update the forgejo release and (re)upload the assets -> create the
# GitHub release with the same assets -> refresh the brew formula with the
# macOS archive's digest, when that archive is in .dist/.
#
# The forgejo API uses the same stored git credentials (`git credential
# fill`); no tokens live in this script. GitHub is driven through `gh`.
#
# TWO PLATFORM DIFFERENCES FROM THE WINDOWS SCRIPT, both because the thing
# they touch does not exist here:
#
#   1. the winget manifests are a Windows package manager's, and there is
#      no `winget` here to validate them. The step is skipped and says
#      so; the manifests are the Windows script's business, and they are
#      updated by the release flow on the Windows side.
#   2. the Windows script commits the manifest changes at the end. There
#      is nothing to commit here, so there is nothing to push.
#
# The assets that go on the release page are the archives in .dist/ -- the
# one this machine just built, plus the ones the other platforms built into
# the same directory -- and the SHA256SUMS file beside them. A release is a
# joint act: whoever publishes last publishes all three platforms.
set -u

here=$(cd "$(dirname "$0")" && pwd)
repo=$(cd "$here/.." && pwd)
cd "$repo"

version=""
skip_tests=0
dry_run=0
private=0
manifests_only=0

while [ $# -gt 0 ]; do
    case "$1" in
        --version) version=${2:-}; shift 2 ;;
        --skip-tests) skip_tests=1; shift ;;
        --dry-run) dry_run=1; shift ;;
        --private) private=1; shift ;;
        --manifests-only) manifests_only=1; shift ;;
        *) echo "unknown option: $1" >&2; exit 2 ;;
    esac
done

[ -n "$version" ] || version=$(sed -n 's/^version = "\(.*\)"/\1/p' bins/searchctl/Cargo.toml | head -1)
[ -n "$version" ] || { echo "no version in bins/searchctl/Cargo.toml" >&2; exit 1; }

tag="rgy-v$version"
# The forgejo that hosts the release. Its address is not in this repository:
# a public repository that names somebody's private network is a map of that
# network, and the address is one line away from being read by everybody.
# Set RGY_FORGEJO_URL to the forgejo's base URL.
forgejo_url="${RGY_FORGEJO_URL:-}"
if [ -z "$forgejo_url" ]; then
    echo "RGY_FORGEJO_URL is not set: the forgejo that hosts the release is not in this repository" >&2
    echo "set it to the forgejo's base URL, e.g. RGY_FORGEJO_URL=http://host:3000" >&2
    exit 1
fi
forgejo_url="${forgejo_url%/}"
repo_api="$forgejo_url/api/v1/repos/danalec/rgy"
forgejo="$forgejo_url/danalec/rgy"
# A credential request names a host, not a URL: `host:port`, with neither
# the scheme nor the path.
forgejo_proto=$(printf '%s' "$forgejo_url" | sed -n 's|^\([a-zA-Z][a-zA-Z0-9+.-]*\)://.*|\1|p')
[ -n "$forgejo_proto" ] || forgejo_proto="http"
forgejo_host=$(printf '%s' "$forgejo_url" | sed -e 's|^[a-zA-Z][a-zA-Z0-9+.-]*://||' -e 's|/.*$||')
gh_repo="danalec/rgy"
gh="https://github.com/$gh_repo"

# --- 1. build ---------------------------------------------------------------
if [ "$skip_tests" -eq 1 ]; then
    bash "$here/build-release.sh" --skip-tests --version "$version" || { echo "the build failed" >&2; exit 1; }
else
    bash "$here/build-release.sh" --version "$version" || { echo "the build failed" >&2; exit 1; }
fi

# The assets: the archives this release carries. The list is explicit
# because .dist/ also holds the working files of whoever built the
# release, and a glob over it would publish all of that to the world.
assets=""
for f in .dist/SHA256SUMS .dist/rgy-v"$version"-*; do
    [ -f "$f" ] && assets="$assets $f"
done
if [ -z "$assets" ]; then
    echo "no assets to publish: .dist/ holds nothing for $tag" >&2
    echo "run scripts/build-release.sh first" >&2
    exit 1
fi

# --- 2. the assets are what the build digested ------------------------------
# An asset whose digest does not match the digest the build wrote is an
# asset nobody can check, and a release that serves it is worse than no
# release. The file the build left behind is the authority. This runs
# before anything is published, because after the first upload the damage
# is done.
sums=".dist/SHA256SUMS"
if [ -f "$sums" ]; then
    for asset in $assets; do
        name=$(basename "$asset")
        [ "$name" = "SHA256SUMS" ] && continue
        want=$(awk -v n="$name" '$2 == n { print $1; exit }' "$sums")
        count=$(awk -v n="$name" '$2 == n { c++ } END { print c + 0 }' "$sums")
        if [ "$count" -gt 1 ]; then
            echo "$name is named $count times in $sums: a digests file that names the same archive twice is a file nobody can read" >&2
            exit 1
        fi
        if [ -z "$want" ]; then
            echo "$name has no digest: the build did not digest it" >&2
            exit 1
        fi
        got=$(sha256sum "$asset" | cut -d' ' -f1)
        if [ "$want" != "$got" ]; then
            echo "$name does not match the digest the build wrote" >&2
            echo "  expected $want" >&2
            echo "  got      $got" >&2
            exit 1
        fi
    done
fi

# --- 3. tag + push ---------------------------------------------------------
if [ "$manifests_only" -eq 1 ]; then
    echo "[manifests-only] the tag is not created"
elif git tag -l "$tag" | grep -q .; then
    echo "tag $tag already exists"
elif [ "$dry_run" -eq 1 ]; then
    echo "[dry-run] git tag $tag; git push forgejo $tag"
else
    git tag "$tag"
    git push forgejo "$tag"
fi

# --- 4. forgejo release + assets -------------------------------------------
# The credentials are the ones git already has for this host. `git
# credential fill` is a program, not a shell builtin, and the pipe into it
# is what makes it answer; the empty line ends the request.
#
# They are fetched only when the release is actually touched: the dry run
# and the manifests-only mode never call the API, and a host that has no
# credential for the forgejo can still rewrite the manifests.
fetch_credentials() {
    cred=$(printf 'protocol=%s\nhost=%s\n\n' "$forgejo_proto" "$forgejo_host" | git credential fill 2>/dev/null)
    user=$(printf '%s\n' "$cred" | sed -n 's/^username=//p')
    pass=$(printf '%s\n' "$cred" | sed -n 's/^password=//p')
    if [ -z "$user" ] || [ -z "$pass" ]; then
        echo "no git credentials for the forgejo" >&2
        exit 1
    fi
    auth=$(printf '%s:%s' "$user" "$pass" | base64 | tr -d '\n')
}

# A JSON string: the backslash, the quote and the newline are the three
# that a release body can carry.
json_escape() {
    printf '%s' "$1" | sed -e 's/\\/\\\\/g' -e 's/"/\\"/g' -e :a -e 'N' -e '$!ba' -e 's/\n/\\n/g'
}

body="Whole-filesystem name search and content search in one stack.

**Which file?** -- asked over the whole filesystem, then **what is inside it?** in the same query.

    searchctl --root ~ \"invoice ext:pdf\"
    searchctl --root ~ \"invoice grep:tesla\"

The release carries four binaries: \`searchctl\` (the command-line tool),
\`searchd\` (the daemon: JSON lines over stdio, a pipe or a unix socket),
\`rgy-autostart\` (put the daemon in place at login) and \`rgxy\` (the
floating window over the daemon).

**Assets**
$(for a in $assets; do printf -- "- \`%s\`\n" "$(basename "$a")"; done)
Apache-2.0."

release=""
if [ "$manifests_only" -eq 1 ]; then
    echo "[manifests-only] the forgejo release is not touched"
elif [ "$dry_run" -eq 1 ]; then
    echo "[dry-run] create/patch the release $tag and upload:"
    for a in $assets; do printf '    %s\n' "$(basename "$a")"; done
else
    fetch_credentials
    release=$(curl -fsSL -H "Authorization: Basic $auth" "$repo_api/releases/tags/$tag" 2>/dev/null || true)
    if [ -n "$release" ]; then
        echo "release exists: $forgejo/releases/tag/$tag"
    else
        echo "release $tag does not exist yet"
    fi
    release_id=""
    if [ -n "$release" ]; then
        release_id=$(printf '%s' "$release" | sed -n 's/.*"id":\([0-9]\+\).*/\1/p' | head -1)
        current_body=$(printf '%s' "$release" | sed -n 's/.*"body":"\(.*\)","created_at".*/\1/p' | head -1)
        if [ "$current_body" != "$(json_escape "$body")" ]; then
            curl -fsSL -X PATCH -H "Authorization: Basic $auth" \
                -H "Content-Type: application/json" \
                -d "{\"name\":\"rgy v$version\",\"body\":\"$(json_escape "$body")\"}" \
                "$repo_api/releases/$release_id" > /dev/null \
                || { echo "the release could not be updated" >&2; exit 1; }
            echo "release updated"
        fi
    else
        created=$(curl -fsSL -X POST -H "Authorization: Basic $auth" \
            -H "Content-Type: application/json" \
            -d "{\"tag_name\":\"$tag\",\"name\":\"rgy v$version\",\"body\":\"$(json_escape "$body")\"}" \
            "$repo_api/releases") || { echo "the release could not be created" >&2; exit 1; }
        release_id=$(printf '%s' "$created" | sed -n 's/.*"id":\([0-9]\+\).*/\1/p' | head -1)
        echo "release created: $forgejo/releases/tag/$tag"
    fi

    for asset in $assets; do
        name=$(basename "$asset")
        # A stale asset of the same name has to go first: the forgejo
        # refuses a second upload under a name that is already taken, and
        # a release that still serves the previous build is worse than no
        # release.
        stale=$(printf '%s' "$release" | grep -o "\"id\":[0-9]*,\"name\":\"$name\"" | head -1 | grep -o '[0-9]\+' | head -1)
        if [ -n "$stale" ]; then
            curl -fsSL -X DELETE -H "Authorization: Basic $auth" \
                "$repo_api/releases/$release_id/assets/$stale" > /dev/null
            echo "replaced asset: $name"
        fi
        curl -fsSL -X POST -H "Authorization: Basic $auth" \
            -H "Content-Type: application/octet-stream" \
            --data-binary "@$asset" \
            "$repo_api/releases/$release_id/assets?name=$name" > /dev/null \
            || { echo "the upload of $name failed" >&2; exit 1; }
        size=$(du -m "$asset" | cut -f1)
        echo "uploaded: $name (${size} MB)"
    done
fi

# --- 5. GitHub release -----------------------------------------------------
# GitHub is the public mirror: the same tag, the same assets, the same
# description. The forgejo goes first because it is the development
# upstream, but a mirror that is down must not hold the public one
# hostage -- the report says what did not happen and the port goes ahead.
if [ "$manifests_only" -eq 1 ]; then
    echo "[manifests-only] the GitHub release is not touched"
elif [ "$dry_run" -eq 1 ]; then
    echo "[dry-run] create the GitHub release $tag in $gh_repo"
else
    if ! gh auth status > /dev/null 2>&1; then
        echo "gh is not authenticated" >&2
        exit 1
    fi
    if ! gh repo view "$gh_repo" > /dev/null 2>&1; then
        if [ "$private" -eq 1 ]; then
            gh repo create "$gh_repo" --private --source . --remote github
        else
            gh repo create "$gh_repo" --public --source . --remote github
        fi
    fi
    git remote get-url github > /dev/null 2>&1 || git remote add github "$gh.git"
    git push -u github HEAD
    git push -f github "$tag"

    if gh release view "$tag" --repo "$gh_repo" > /dev/null 2>&1; then
        gh release delete "$tag" --repo "$gh_repo" --yes
    fi
    files=""
    for a in $assets; do files="$files $a"; done
    # shellcheck disable=SC2086
    gh release create "$tag" $files --repo "$gh_repo" --title "rgy v$version" --notes "$body"
    echo "github release: $gh/releases/tag/$tag"
fi

# --- 6. the brew formula ----------------------------------------------------
# The formula names the macOS archives, so its digests are the digests of the
# files that are about to be published. A Linux box has no macOS archive to
# digest, and the formula says so instead of carrying a digest of nothing.
#
# The formula lists the archives in the order they appear in it -- arm
# first, then intel -- and the rewrite follows that order: the n-th url
# line gets the n-th archive's URL, the n-th sha256 line its digest.
mac_assets=""
for f in .dist/rgy-v"$version"-aarch64-apple-darwin.tar.gz .dist/rgy-v"$version"-x86_64-apple-darwin.tar.gz; do
    [ -f "$f" ] && mac_assets="$mac_assets $f"
done
formula="Formula/rgy-search.rb"

if [ -z "$mac_assets" ]; then
    echo "brew formula: not touched (no macOS archive in .dist/)"
elif [ "$dry_run" -eq 1 ]; then
    echo "[dry-run] brew formula -> version $version"
    for f in $mac_assets; do echo "    $(basename "$f")"; done
else
    names=""
    shas=""
    for f in $mac_assets; do
        names="$names${names:+,}$(basename "$f")"
        shas="$shas${shas:+,}$(sha256sum "$f" | cut -d' ' -f1)"
    done
    awk -v names="$names" -v shas="$shas" -v base="$gh/releases/download/$tag" -v ver="$version" '
        BEGIN { split(names, N, ","); split(shas, S, ","); ui = 1; si = 1; vi = 0 }
        $0 ~ /^[[:space:]]*url "/ {
            match($0, /^[[:space:]]*/)
            printf "%surl \"%s/%s\"\n", substr($0, 1, RLENGTH), base, N[ui++]; next
        }
        $0 ~ /^[[:space:]]*sha256 "/ {
            match($0, /^[[:space:]]*/)
            printf "%ssha256 \"%s\"\n", substr($0, 1, RLENGTH), S[si++]; next
        }
        $0 ~ /^[[:space:]]*version "/ && vi++ == 0 {
            match($0, /^[[:space:]]*/)
            printf "%sversion \"%s\"\n", substr($0, 1, RLENGTH), ver; next
        }
        { print }
    ' "$formula" > "$formula.tmp" && mv "$formula.tmp" "$formula"
    echo "brew formula updated: version $version"
    for f in $mac_assets; do
        echo "    $(sha256sum "$f" | cut -d' ' -f1)  $(basename "$f")"
    done
fi

# --- 7. the installer pins ----------------------------------------------------
# Both installers carry one digest per architecture they may install, and
# the digests are release data: they are rewritten here, from the assets
# this release carries, and a pin that does not end up naming the release's
# own artifact is a failure -- a hand-maintained pin is a pin that is wrong
# on the day after the release.
#
# An asset this release does not carry leaves its pin at the placeholder.
# The installer then says that there is no release for that platform yet,
# which is true -- and better than a digest of an archive that is not there.
set_pin() {
    file="$1"; sha="$2"; pattern="$3"
    if [ ! -f "$file" ]; then
        echo "installer pin: $file not found, skipped"
        return 0
    fi
    # The pattern keeps the name and the quotes in groups 1 and 2, and only
    # the digest between them is replaced.
    sed -i -E "s#$pattern#\1$sha\2#" "$file" || { echo "the pin in $file could not be rewritten" >&2; exit 1; }
    sed -i -E "s#rgy-v[0-9]+\.[0-9]+\.[0-9]+#$tag#g" "$file" || exit 1
    if ! grep -q "$sha" "$file"; then
        echo "the pin in $file does not carry the digest of the release ($sha)" >&2
        exit 1
    fi
    if ! grep -q "expected" "$file"; then
        echo "the pin in $file lost its name: the rewrite ate more than the digest" >&2
        exit 1
    fi
    echo "pin updated: $file -> $tag $sha"
}

digest_of() {
    # The archive itself, when it is here.
    if [ -f ".dist/$1" ]; then
        sha256sum ".dist/$1" | cut -d' ' -f1 | tr 'a-f' 'A-F'
        return 0
    fi
    # The digest file, which is what the other platforms wrote.
    [ -f .dist/SHA256SUMS ] || return 1
    awk -v n="$1" '$2 == n { print toupper($1); exit }' .dist/SHA256SUMS
}

if [ "$dry_run" -eq 1 ]; then
    echo "[dry-run] installer pins -> version $tag"
else
    placeholder="0000000000000000000000000000000000000000000000000000000000000000"
    # The pins, and the line each one lives on: the file, the asset, and
    # the pattern that keeps the name and the quotes and replaces only the
    # digest between them. The name, not a suffix: the release carries a
    # portable archive and two tuned ones, and the installer is allowed to
    # install the portable one only.
    pins=(
        "install.ps1|x86_64-windows.zip|^([[:space:]]*'x86_64-windows\.zip'[[:space:]]*=[[:space:]]*')[0-9A-Fa-f]{64}(')$"
        "install.ps1|aarch64-windows.zip|^([[:space:]]*'aarch64-windows\.zip'[[:space:]]*=[[:space:]]*')[0-9A-Fa-f]{64}(')$"
        "install.sh|x86_64-linux.tar.gz|^(expected_linux_x86_64=\")[0-9A-Fa-f]{64}(\")$"
        "install.sh|aarch64-linux.tar.gz|^(expected_linux_aarch64=\")[0-9A-Fa-f]{64}(\")$"
        "install.sh|x86_64-apple-darwin.tar.gz|^(expected_macos_x86_64=\")[0-9A-Fa-f]{64}(\")$"
        "install.sh|aarch64-apple-darwin.tar.gz|^(expected_macos_aarch64=\")[0-9A-Fa-f]{64}(\")$"
    )
    for pin in "${pins[@]}"; do
        file="${pin%%|*}"
        rest="${pin#*|}"
        asset="${rest%%|*}"
        pattern="${rest#*|}"
        digest=$(digest_of "rgy-v$version-$asset")
        if [ -z "$digest" ]; then
            echo "installer pin: rgy-v$version-$asset is not in this release; the pin stays a placeholder"
            digest="$placeholder"
        fi
        set_pin "$file" "$digest" "$pattern"
    done
fi

# --- 8. winget -------------------------------------------------------------
# The winget manifests are a Windows package manager's, and there is no
# winget here. The Windows script owns that step; this one says so
# instead of pretending to have done it.
echo "winget manifests: not touched (winget is a Windows package manager; scripts/publish-release.ps1 owns that step)"

# --- 9. done ---------------------------------------------------------------
if [ "$manifests_only" -eq 1 ]; then
    echo ""
    echo "manifests rewritten; nothing was published"
    exit 0
fi

echo ""
echo "published $tag : $forgejo/releases/tag/$tag"
echo "published $tag : $gh/releases/tag/$tag"
