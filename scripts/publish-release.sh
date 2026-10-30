#!/usr/bin/env bash
# publish-release.sh -- publish an rgy release end to end, from Linux or macOS.
#
# The Linux/macOS counterpart of scripts/publish-release.ps1.
#
#   bash scripts/publish-release.sh                 # full pipeline
#   bash scripts/publish-release.sh --skip-tests    # trust the build
#   bash scripts/publish-release.sh --dry-run       # show, don't touch
#   bash scripts/publish-release.sh --private       # keep the GitHub mirror private
#   bash scripts/publish-release.sh --commit        # commit the release data
#
# Pipeline: build this platform's archive -> tag rgy-v<version> + push ->
# create or update the forgejo release and (re)upload the assets -> create the
# GitHub release with the same assets -> refresh the brew formula with the
# macOS archive's digest, when that archive is in .dist/.
#
# The forgejo API uses the same stored git credentials (`git credential
# fill`); no tokens live in this script. GitHub is driven through `gh`.
#
# ONE PLATFORM DIFFERENCE FROM THE WINDOWS SCRIPT, because the thing it
# touches does not exist here:
#
#   the winget manifests are a Windows package manager's, and there is no
#   `winget` here to validate them. The step is skipped and says so; the
#   manifests are the Windows script's business, and they are updated by
#   the release flow on the Windows side.
#
# Everything else the two scripts do is the same thing: the same archives,
# the same release, the same messages, the same exit code. The commit of
# the release data is in both — that data is the release's own, and it
# does not belong to one platform.
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
commit=0

while [ $# -gt 0 ]; do
    case "$1" in
        --version) version=${2:-}; shift 2 ;;
        --skip-tests) skip_tests=1; shift ;;
        --dry-run) dry_run=1; shift ;;
        --private) private=1; shift ;;
        --manifests-only) manifests_only=1; shift ;;
        --commit) commit=1; shift ;;
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
# The order is the same on every machine: the digests file first, then the
# archives by name, byte by byte. A shell glob sorts by the locale, and a
# locale is a machine's habit, not the release's order — and the release
# body lists these names, so the body has to be the same wherever it is
# built.
assets=""
[ -f .dist/SHA256SUMS ] && assets="$assets .dist/SHA256SUMS"
for f in $(ls -1 .dist/rgy-v"$version"-* 2>/dev/null | LC_ALL=C sort); do
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
        # The carriage return: the digests file is shared with the Windows
        # twin, and a name that ends with one is not the name the digest is
        # filed under.
        want=$(awk -v n="$name" '{ sub(/\r$/, "", $2) } $2 == n { print $1; exit }' "$sums")
        count=$(awk -v n="$name" '{ sub(/\r$/, "", $2) } $2 == n { c++ } END { print c + 0 }' "$sums")
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

# The brew formula, checked before anything is published. The formula names
# the macOS archives, and a release that goes out while the formula cannot
# name what the release publishes is a release nobody can install from.
#
# The check is here, before the tag and before the release, so a formula
# that cannot be mapped stops the publish instead of stopping it half way.
formula="Formula/rgy-search.rb"
mac_arches="aarch64-apple-darwin x86_64-apple-darwin"
if [ -f "$formula" ]; then
    url_lines=$(grep -c '^[[:space:]]*url "' "$formula" || true)
    sha_lines=$(grep -c '^[[:space:]]*sha256 "' "$formula" || true)
    if [ "$url_lines" != "$sha_lines" ]; then
        echo "the formula carries $url_lines url lines and $sha_lines sha256 lines: they do not pair up" >&2
        exit 1
    fi
    for arch in $mac_arches; do
        [ -f ".dist/rgy-v$version-$arch.tar.gz" ] || continue
        named=$(grep -c -- "-$arch\.tar\.gz" "$formula" || true)
        if [ "$named" -eq 0 ]; then
            echo "the formula does not name $arch: this release publishes it and the formula would not install it" >&2
            exit 1
        fi
        if [ "$named" -gt 1 ]; then
            echo "the formula names $arch $named times: a formula that names the same archive twice is a formula nobody can read" >&2
            exit 1
        fi
    done
fi

# The release's main. The README, the installers and the winget manifests
# point at github.com/danalec/rgy/main, and the mirror carries main
# because of it -- so main has to carry the release, or the links resolve
# to an older tree that answers with the wrong project.
#
# The check is here, before the tag and before the release, so a main that
# is behind stops the publish instead of stopping it half way -- and it
# runs in the dry run too, because a dry run that says "the release would
# be rgy-v0.1.0" when the release would stop at the first step is a dry
# run that lies.
if [ "$manifests_only" -eq 0 ] && git rev-parse --verify --quiet refs/heads/main > /dev/null; then
    if ! git merge-base --is-ancestor HEAD main; then
        behind=$(git rev-list --count main..HEAD)
        echo "main does not carry the release commit: main is $behind commits behind HEAD" >&2
        echo "  the README, the installers and the winget manifests point at main, and they would answer with the older tree" >&2
        echo "  merge the branch into main (git merge --ff-only <branch>) and run this again" >&2
        exit 1
    fi
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

# The id of a release, out of the JSON the forgejo answered with.
#
# The release's own id is the FIRST "id" in the document. The assets array
# comes last and every asset carries an id of its own, and a greedy match
# picks that one: the release is then addressed by an asset id, every
# upload answers 404, and the publish dies on "the upload of ... failed".
release_id_of() {
    id=$(printf '%s' "$1" | sed -n 's/^[[:space:]]*{[[:space:]]*"id"[[:space:]]*:[[:space:]]*\([0-9]\+\).*/\1/p' | head -1)
    if [ -z "$id" ]; then
        id=$(printf '%s' "$1" | grep -o '"id"[[:space:]]*:[[:space:]]*[0-9]\+' | head -1 | grep -o '[0-9]\+')
    fi
    printf '%s' "$id"
}

# The body of a release, as the forgejo stored it: the JSON string, still
# escaped. The comparison below is between two escaped strings, so the
# escaping has to be the same on both sides — and it is: json_escape
# writes what the forgejo writes.
#
# The string ends at the first quote that is not escaped, and that is not
# something a sed pattern can say on its own: the body carries escaped
# quotes of its own.
release_body_of() {
    printf '%s' "$1" | awk '
        {
            i = index($0, "\"body\":\"")
            if (i == 0) next
            rest = substr($0, i + 8)
            j = 1
            while (j <= length(rest)) {
                c = substr(rest, j, 1)
                if (c == "\\") { j += 2; continue }
                if (c == "\"") break
                j++
            }
            print substr(rest, 1, j - 1)
            exit
        }'
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
    echo "[dry-run] create/patch the forgejo release $tag and upload:"
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
        release_id=$(release_id_of "$release")
        current_body=$(release_body_of "$release")
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
        release_id=$(release_id_of "$created")
        echo "release created: $forgejo/releases/tag/$tag"
    fi
    if [ -z "$release_id" ]; then
        echo "the release id could not be read: the forgejo answered with something this script does not understand" >&2
        exit 1
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
        # The size the way the Windows twin prints it: two decimals, in
        # megabytes, and not the count of blocks a `du -m` rounds up to.
        # LC_ALL=C: a locale that writes "12,35" is not the number the
        # Windows twin prints.
        bytes=$(wc -c < "$asset" | tr -d ' ')
        size=$(LC_ALL=C awk -v b="$bytes" 'BEGIN { printf "%.2f", b / 1048576 }')
        echo "uploaded: $name ($size MB)"
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
    echo "[dry-run] push main and the checked-out branch to $gh_repo, and make main the default branch"
    echo "[dry-run] create the GitHub release $tag in $gh_repo"
else
    if ! gh auth status > /dev/null 2>&1; then
        echo "gh is not authenticated" >&2
        exit 1
    fi
    if ! gh repo view "$gh_repo" > /dev/null 2>&1; then
        if [ "$private" -eq 1 ]; then
            gh repo create "$gh_repo" --private --source . --remote github \
                || { echo "the GitHub mirror could not be created" >&2; exit 1; }
        else
            gh repo create "$gh_repo" --public --source . --remote github \
                || { echo "the GitHub mirror could not be created" >&2; exit 1; }
        fi
    fi
    if ! git remote get-url github > /dev/null 2>&1; then
        git remote add github "$gh.git" || { echo "the github remote could not be added" >&2; exit 1; }
    fi
    # The mirror carries the branch that is checked out AND main: the
    # README, the installers and the winget manifests all point at
    # github.com/danalec/rgy/main, so a mirror without main is a mirror
    # whose links are all 404.
    #
    # main goes first: a mirror with no commit yet takes its default
    # branch from the first push, and a mirror whose default is
    # `feature/rgxy-launcher` opens on a branch nobody asked for. The
    # default is then set by name, so the outcome does not depend on
    # which push landed first.
    git push -u github main || { echo "main could not be pushed to the GitHub mirror" >&2; exit 1; }
    git push -u github HEAD || { echo "the branch could not be pushed to the GitHub mirror" >&2; exit 1; }
    git push -f github "$tag" || { echo "the tag could not be pushed to the GitHub mirror" >&2; exit 1; }
    gh repo edit "$gh_repo" --default-branch main || { echo "the default branch of the mirror could not be set to main" >&2; exit 1; }

    if gh release view "$tag" --repo "$gh_repo" > /dev/null 2>&1; then
        gh release delete "$tag" --repo "$gh_repo" --yes \
            || { echo "the previous GitHub release could not be deleted" >&2; exit 1; }
    fi
    files=""
    for a in $assets; do files="$files $a"; done
    # shellcheck disable=SC2086
    gh release create "$tag" $files --repo "$gh_repo" --title "rgy v$version" --notes "$body" \
        || { echo "the GitHub release could not be created" >&2; exit 1; }
    echo "github release: $gh/releases/tag/$tag"
fi

# --- 6. the brew formula ----------------------------------------------------
# The formula names the macOS archives, so its digests are the digests of the
# files that are about to be published. A Linux box has no macOS archive to
# digest, and the formula says so instead of carrying a digest of nothing.
#
# The rewrite is per architecture: the formula's url line says which archive
# it installs, and the archive that is here gets the URL and the digest it
# deserves. The architecture whose archive is not here keeps its placeholder
# -- the same rule the installer pins follow, and the same truth: there is
# no release for that platform yet.
#
# The n-th url line does NOT get the n-th archive: a formula with two
# blocks and a release with one archive would write a URL that names no
# file and a checksum that is empty, and `brew` would be asked to install a
# directory.
mac_assets=""
for arch in $mac_arches; do
    f=".dist/rgy-v$version-$arch.tar.gz"
    [ -f "$f" ] && mac_assets="$mac_assets $f"
done

if [ -z "$mac_assets" ]; then
    echo "brew formula: not touched (no macOS archive in .dist/)"
elif [ "$dry_run" -eq 1 ]; then
    echo "[dry-run] brew formula -> version $version"
    for f in $mac_assets; do echo "    $(basename "$f")"; done
else
    # The digests of the archives this release carries, by architecture.
    digests=""
    for f in $mac_assets; do
        arch=$(basename "$f")
        arch=${arch#"rgy-v$version-"}
        arch=${arch%.tar.gz}
        digests="$digests $arch=$(sha256sum "$f" | cut -d' ' -f1)"
    done
    awk -v digests="$digests" -v base="$gh/releases/download/$tag" -v ver="$version" '
        BEGIN {
            n = split(digests, D, " ")
            for (i = 1; i <= n; i++) {
                split(D[i], kv, "=")
                digest[kv[1]] = kv[2]
            }
            pending = ""
            vi = 0
        }
        $0 ~ /^[[:space:]]*url "/ {
            line = $0
            sub(/^[^"]*"/, "", line)
            sub(/".*$/, "", line)
            count = split(line, parts, "/")
            file = parts[count]
            arch = ""
            for (a in digest) {
                if (file ~ ("-" a "[.]tar[.]gz$")) arch = a
            }
            if (arch != "") {
                match($0, /^[[:space:]]*/)
                printf "%surl \"%s/rgy-v%s-%s.tar.gz\"\n", substr($0, 1, RLENGTH), base, ver, arch
                pending = arch
                next
            }
            pending = ""
            print
            next
        }
        $0 ~ /^[[:space:]]*sha256 "/ {
            if (pending != "") {
                match($0, /^[[:space:]]*/)
                printf "%ssha256 \"%s\"\n", substr($0, 1, RLENGTH), digest[pending]
                pending = ""
                next
            }
            print
            next
        }
        $0 ~ /^[[:space:]]*version "/ && vi++ == 0 {
            match($0, /^[[:space:]]*/)
            printf "%sversion \"%s\"\n", substr($0, 1, RLENGTH), ver; next
        }
        { print }
    ' "$formula" > "$formula.tmp" && mv "$formula.tmp" "$formula"
    for arch in $mac_arches; do
        [ -f ".dist/rgy-v$version-$arch.tar.gz" ] && continue
        echo "brew formula: $arch keeps its placeholder: this release carries no $arch archive"
    done
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
    # The carriage return: the digests file is shared with the Windows
    # twin, and a name that ends with one is not the name the digest is
    # filed under.
    awk -v n="$1" '{ sub(/\r$/, "", $2) } $2 == n { print toupper($1); exit }' .dist/SHA256SUMS
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

# --- 9. the release data ----------------------------------------------------
# The release's own data -- the installer pins, the brew formula and the
# winget manifests -- is release data: this script writes it, and it is
# committed with it. A clone that still carries the placeholders is a clone
# whose one-line installer says "there is no published rgy release for
# x86_64 yet" on the day of the release.
#
# The commit is asked for, not done on the side: publishing and committing
# are two acts, and the owner decides when the release data lands.
if [ "$commit" -eq 1 ] && [ "$dry_run" -eq 0 ] && [ "$manifests_only" -eq 0 ]; then
    staged=""
    for p in .winget install.sh install.ps1 Formula; do
        [ -e "$p" ] && staged="$staged $p"
    done
    # shellcheck disable=SC2086
    git add $staged || { echo "the release data could not be staged" >&2; exit 1; }
    if git diff --cached --quiet; then
        echo "release data: nothing to commit"
    else
        git commit -m "release: $tag release data" || { echo "the release data could not be committed" >&2; exit 1; }
        echo "release data committed"
    fi
    branch=$(git rev-parse --abbrev-ref HEAD)
    git push forgejo "$branch" || { echo "the release data could not be pushed to the forgejo" >&2; exit 1; }
    echo "release data pushed to the forgejo"
fi

# --- 10. done ---------------------------------------------------------------
if [ "$manifests_only" -eq 1 ]; then
    echo ""
    echo "manifests rewritten; nothing was published"
    exit 0
fi

echo ""
if [ "$dry_run" -eq 1 ]; then
    # The dry run says what it did not do. A line that says "published" at
    # the end of a run that published nothing is a line nobody can trust
    # -- and this is the last line of the script, the one a reader's eye
    # lands on.
    echo "[dry-run] the release would be $tag :"
    echo "[dry-run]   $forgejo/releases/tag/$tag"
    echo "[dry-run]   $gh/releases/tag/$tag"
    exit 0
fi
echo "published $tag : $forgejo/releases/tag/$tag"
echo "published $tag : $gh/releases/tag/$tag"
