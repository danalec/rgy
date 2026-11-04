#!/usr/bin/env bash
# publish-release.sh -- publish an rgy release end to end, from Linux or macOS.
#
# The Linux/macOS counterpart of scripts/publish-release.ps1.
#
#   bash scripts/publish-release.sh                 # full pipeline
#   bash scripts/publish-release.sh --skip-tests    # trust the build
#   bash scripts/publish-release.sh --skip-build    # the release is built already
#   bash scripts/publish-release.sh --dry-run       # show, don't touch
#   bash scripts/publish-release.sh --private       # keep the GitHub mirror private
#   bash scripts/publish-release.sh --commit        # commit the release data
#   bash scripts/publish-release.sh --with-github   # publish to the GitHub mirror too
#   bash scripts/publish-release.sh --variant avx2  # a tuned asset
#
# Pipeline: build this platform's archive -> tag rgy-v<version> + push ->
# create or update the forgejo release and (re)upload the assets -> refresh
# the brew formula with the macOS archive's digest, when that archive is in
# .dist/. The GitHub release is not part of it: the mirror is a flat one and
# the mirror tool's business, and the switch asks for the old behaviour when
# a caller wants it.
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
with_github=0
# The release is built already. The caller built it, proved it, and the
# proof names the digests the archives have -- so this script publishes those
# bytes and does not build again. A second build here is a build of the tree
# under the chair, and the tree under the chair is shared: it may carry
# somebody's work in progress, and a release that ships it is a release whose
# bytes are in no commit.
skip_build=0
# The target the release is built for. It is the host's own when it is
# empty, and the reason it is an option at all: the build the publish
# step runs must be the build the release carries, and a matrix that
# builds twelve targets must not have the publish step build a twelfth
# one for the host and drop it into .dist/ beside the others.
target=""
# The flavour the release is built for. The vocabulary is the build
# script's -- portable, avx2, znver4 -- and the publish script does not
# invent a second one: a release that names a flavour the build does
# not know is a release nobody can build. The default is the portable
# one, so a caller that says nothing gets what it got before this
# option existed.
variant="portable"

# The options, read by the helper the two twins share: the long spelling
# and the PowerShell one are the same option, and an option neither of
# them knows is an error on both platforms -- not a silent "do the real
# thing", which in this directory means publishing a release.
. "$here/lib/options.sh"
. "$here/lib/digest.sh"
rgy_read_options "skiptests=skip_tests dryrun=dry_run private=private manifestsonly=manifests_only commit=commit withgithub=with_github skipbuild=skip_build" \
                 "version=version target=target variant=variant" "" "$@"

# The flavour, checked here and not by the build: the build's refusal
# would come out of a second script, and the two twins have to answer
# the same way. The sentence and the exit code are the build's own.
case "$variant" in
    portable|avx2|znver4) ;;
    *) echo "unknown variant: $variant (portable, avx2, znver4)" >&2; exit 2 ;;
esac

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
# The build the release carries, for the target and the flavour the
# release carries. The target is passed on so a matrix that builds
# twelve targets does not have the publish step build a thirteenth one
# for the host and drop it into .dist/ beside the others; the flavour is
# passed on for the same reason, so a row that asked for a tuned asset
# does not publish the portable one beside it.
build_args="--version $version --variant $variant"
[ -n "$target" ] && build_args="$build_args --target $target"
[ "$skip_tests" -eq 1 ] && build_args="$build_args --skip-tests"

# THE DRY RUN DOES NOT BUILD.
#
# A dry run that rebuilds the release rewrites the very bytes it is
# describing: the archives move, and the digests the installer pins name move
# with them. A release that was proved against one set of digests and then
# published with another is a release nobody can install, and the proof that
# let it through was the thing that broke it.
#
# So the dry run reads the .dist as it is and says what it would publish with
# it, and it says that the build was not run -- the reader is told what was
# NOT done, the way the last line of this script already does.
#
# The publish itself still builds: the release that goes out is built from
# the tree at the moment it goes out -- unless the caller says the release is
# built already, which is what the release chain does: it built the release,
# proved it, and the proof names the digests the archives have.
if [ "$dry_run" -eq 1 ]; then
    echo "[dry-run] the build was not run: the .dist is what it is"
elif [ "$skip_build" -eq 1 ]; then
    echo "the build was not run: the .dist is what it is"
else
    # shellcheck disable=SC2086
    bash "$here/build-release.sh" $build_args || { echo "the build failed" >&2; exit 1; }
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
        #
        # The case, too: the digest is compared as lowercase on both sides,
        # because the Windows twin lowercases the row it reads and the
        # answer is the lowercase hex everywhere else in the tree. A row
        # written in uppercase is the same digest, and a twin that refuses
        # what its own twin accepts is a twin that answers the same question
        # two ways.
        want=$(awk -v n="$name" '{ sub(/\r$/, "", $2) } $2 == n { print tolower($1); exit }' "$sums")
        count=$(awk -v n="$name" '{ sub(/\r$/, "", $2) } $2 == n { c++ } END { print c + 0 }' "$sums")
        if [ "$count" -gt 1 ]; then
            echo "$name is named $count times in $sums: a digests file that names the same archive twice is a file nobody can read" >&2
            exit 1
        fi
        if [ -z "$want" ]; then
            echo "$name has no digest: the build did not digest it" >&2
            exit 1
        fi
        got=$(rgy_sha256 "$asset")
        if [ "$want" != "$got" ]; then
            echo "$name does not match the digest the build wrote" >&2
            echo "  expected $want" >&2
            echo "  got      $got" >&2
            exit 1
        fi
    done
fi

# --- 2b. the archives carry the documents the tree has ----------------------
# The digest check above proves the archives are the ones the build made. It
# does not say what is INSIDE them: a release built before the README's last
# edit carries the older README, and its digest matches perfectly.
#
# The build wrote down the digest of every document it packed, beside the
# archive. This compares each one with the tree's, and a document that is not
# the tree's stops the publish: the README is what tells the reader what the
# release is, and a release whose README is older than the tree is a release
# that says something the tree does not say.
docs_root="$repo/.dist/documents"
for asset in $assets; do
    name=$(basename "$asset")
    [ "$name" = "SHA256SUMS" ] && continue
    sidecar="$docs_root/$name.sha256"
    if [ ! -f "$sidecar" ]; then
        echo "$name has no document digests: it was built by a build that did not write them" >&2
        echo "  rebuild it with scripts/build-release.sh, and copy .dist/documents/ with it" >&2
        exit 1
    fi
    docs_count=0
    while IFS= read -r row; do
        [ -n "$row" ] || continue
        want=$(printf '%s' "$row" | awk '{ print $1 }')
        doc=$(printf '%s' "$row" | sed 's/^[0-9a-fA-F]*  //')
        docs_count=$((docs_count + 1))
        if [ ! -f "$repo/$doc" ]; then
            echo "$name carries a $doc that the tree does not have" >&2
            exit 1
        fi
        got=$(rgy_sha256 "$repo/$doc")
        if [ "$want" != "$got" ]; then
            echo "$name carries a $doc that is not the tree's: the archive has $want, the tree has $got" >&2
            exit 1
        fi
    done < "$sidecar"
    echo "documents: $name carries the tree's documents ($docs_count files)"
done

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

# --- 2c. the release body ----------------------------------------------------
# The body of the release is a file in the tree: docs/release-<version>.md. It
# is the page the README and the manual promise, and both publishers read it,
# so the release says what the tree says and the page that is published is the
# page the repository carries.
#
# The name is the version's, not a constant: a page named for one release and
# read for another is a page that says one thing and is published as another,
# and the drift is silent -- the publish reads what is there and does not
# complain. The version is the one the tree carries (the manifests), so the
# page that names the version is the page of the release being cut.
#
# Two markers in the page are for the scripts, and they are the only part of
# it that is not the page:
#
#   <!-- assets: ... -->  the files the release carries. The list is compared
#                         with the files that are about to be uploaded, and a
#                         page that names an archive the release does not
#                         carry stops the publish -- and so does a release
#                         that carries an archive the page does not name.
#                         A run that rewrites the manifests and publishes
#                         nothing compares the list one way only: the page
#                         may name assets the run does not have -- the
#                         other rows' assets -- and everything the run
#                         carries has to be named by the page.
#   <!-- platform -->     where each publisher puts its own sentence about
#                         where the history lives.
#
# The page is read as UTF-8 and normalized: the carriage returns go and the
# trailing newline goes with them. A checkout that rewrote the line endings
# must not change what the release says, and a carriage return is not JSON.
page="docs/release-$version.md"
if [ ! -f "$page" ]; then
    echo "the release page is not there: $page" >&2
    exit 1
fi
# The command substitution drops the trailing newlines, and that is the
# point: the body is the same bytes whatever the checkout did to the file.
body=$(tr -d '\r' < "$page")

# The marker line, and how many of them there are: exactly one, or the page
# is not a page this script knows how to publish.
platform_markers=$(printf '%s\n' "$body" | grep -c '^<!-- platform -->$')
if [ "$platform_markers" -ne 1 ]; then
    echo "the release page does not carry exactly one platform marker: $page" >&2
    exit 1
fi
# The sentence that says where the history lives. It is the same sentence in
# the two twins, byte for byte: the page is published from both, and two
# sentences are two pages.
platform_sentence="The history of this project lives here, on the Forgejo; the GitHub repository is a flat mirror of this release — one commit, no parents."
body=$(printf '%s\n' "$body" | sed -e "s|^<!-- platform -->\$|$platform_sentence|")

# The assets the page names, and the assets the release carries. The two are
# compared as sets, and the order is the byte order: a locale's order is a
# machine's habit, and the two lists have to be compared the same way on
# every machine.
assets_markers=$(printf '%s\n' "$body" | grep -c '^<!-- assets: ')
if [ "$assets_markers" -ne 1 ]; then
    echo "the release page does not carry exactly one assets marker: $page" >&2
    exit 1
fi
page_assets=$(printf '%s\n' "$body" | sed -n 's|^<!-- assets: \(.*\) -->$|\1|p')
page_asset_list=$(printf '%s\n' "$page_assets" | tr -s ' \t' '\n' | grep -v '^$' | LC_ALL=C sort -u)
release_asset_list=$(for a in $assets; do basename "$a"; done | LC_ALL=C sort -u)

# The names of the first list that the second one does not carry.
list_difference() {
    local first="$1" second="$2" name out=""
    for name in $first; do
        if ! printf '%s\n' "$second" | grep -qxF -- "$name"; then
            out="$out $name"
        fi
    done
    printf '%s' "$out"
}

# The names of a list in a sentence, and "none" when the list is empty: the
# refusal reads the same either way.
names_in_a_line() {
    local out="" first=1 name
    for name in $1; do
        if [ $first -eq 1 ]; then out="$name"; first=0; else out="$out, $name"; fi
    done
    if [ -z "$out" ]; then out="none"; fi
    printf '%s' "$out"
}

# The refusal for a release page that does not match the assets of this
# run -- and the empty string when it does.
#
# The two modes ask two different questions, and they are answered in two
# different sentences.
#
# A PUBLISH carries the whole release: the archives in .dist/ are the
# release's assets, and the page has to name exactly them. A page that
# names an archive the release does not carry is a page that promises an
# archive nobody can download, and a release that carries an archive the
# page does not name is a release the page does not tell the reader about.
# Both stop the publish, and both are said in one sentence.
#
# A MANIFESTS-ONLY run carries ONE archive of the release: the release is
# built one row at a time, and each row rewrites the manifests from the
# archive it built. The page names the whole release, so the page may name
# assets this row does not have -- the other rows' assets -- and that is
# not a mistake. What IS a mistake is an archive this row carries and the
# page does not name: the release does not carry it either, and the
# manifests this row rewrites would pin an archive the release does not
# publish.
#
# The arguments are the mode, the page that carries the marker, the names
# the page has and the release does not, and the names the release has and
# the page does not.
release_assets_refusal() {
    local mode="$1" page="$2" only_page="$3" only_release="$4"
    if [ "$mode" = "manifests-only" ]; then
        [ -n "$only_release" ] || return 0
        printf '%s\n' "the release page does not name the assets of this run: this run carries [$(names_in_a_line "$only_release")] the page does not name"
        printf '%s\n' "  the assets marker in $page names the whole release, and every archive this run carries has to be in it"
        printf '%s\n' "  add the name to the marker and run this again"
        return 0
    fi
    if [ -n "$only_page" ] || [ -n "$only_release" ]; then
        printf '%s\n' "the release page does not match the assets of this release: the page names [$(names_in_a_line "$only_page")] the release does not carry, and the release carries [$(names_in_a_line "$only_release")] the page does not name"
        return 0
    fi
    return 0
}

only_page=$(list_difference "$page_asset_list" "$release_asset_list")
only_release=$(list_difference "$release_asset_list" "$page_asset_list")
# The mode decides which way the two lists are compared. A publish
# compares them both ways, and the sets have to be equal; a run that
# rewrites the manifests and publishes nothing compares them one way --
# everything this run carries has to be named by the page, and the page
# may name the other rows' assets on top of that.
#
# The dry run does not change this: a dry run that skips a check the real
# run does is a dry run that lies.
assets_mode="publish"
if [ "$manifests_only" -eq 1 ]; then
    assets_mode="manifests-only"
fi
refusal=$(release_assets_refusal "$assets_mode" "$page" "$only_page" "$only_release")
if [ -n "$refusal" ]; then
    printf '%s\n' "$refusal" >&2
    exit 1
fi

# A JSON string: the backslash, the quote and the newline are the three
# that a release body can carry.
#
# EVERY line is escaped, and that is the whole point of the two stages:
# the sed below escapes each line as it reads it -- the backslash first
# and the quote after it -- and the awk that follows joins the escaped
# lines with the two characters \ and n.
#
# The escaping used to run on the first line only, and the slurp loop
# appended the other lines to it afterwards: every line but the first
# kept its raw quotes, and a body that quotes a command -- "invoice
# ext:pdf" -- is not JSON. The forgejo answers 400 to a body that is
# not JSON, and the release is not made.
#
# The newline is joined in, and not escaped, because the escape of a
# newline is the two characters \ and n: a body that carried a real
# backslash would have it doubled by the sed, and the two characters
# that come out of the awk are the escape itself.
json_escape() {
    printf '%s\n' "$1" | sed -e 's/\\/\\\\/g' -e 's/"/\\"/g' |
        awk 'NR > 1 { printf "%s", "\\n" } { printf "%s", $0 }'
}

# The escaped body has to be a JSON string a parser accepts: every
# quote in it escaped, no raw newline in it, and no raw control
# character in it. This is the check that catches an escaping that is
# wrong, and it runs before the request goes out: a body that is not
# JSON is a 400 from the forgejo and a release that is not made, and
# the twin that refuses it refuses it the same way.
json_escape_is_valid() {
    printf '%s' "$1" | awk '
        # A second record is a raw newline: the escaped body is one
        # line, and a newline in it is a newline the forgejo reads.
        NR > 1 { bad = 1 }
        {
            n = length($0)
            for (i = 1; i <= n; i++) {
                c = substr($0, i, 1)
                # A backslash takes the next character with it: what
                # follows an escaped quote is a quote that does not
                # end the string.
                if (c == "\\") { i++; continue }
                if (c == "\"" || c == "\r" || c == "\t") { bad = 1 }
            }
        }
        END { exit (bad ? 1 : 0) }'
}

# The body as the forgejo receives it: escaped once, checked once, and
# the same bytes in every request. The check is here and not inside the
# request, because it has to run in the modes that do not talk to the
# forgejo too -- a dry run that says "the release would be rgy-v0.1.0"
# when the release would stop at the first step is a dry run that lies.
#
# And it is here and not after the tag: a body that cannot be published
# has to be found before the tag is pushed, because a tag that is out is
# a tag nobody can take back.
escaped_body=$(json_escape "$body")
if ! json_escape_is_valid "$escaped_body"; then
    echo "the release body is not valid JSON: the escaping is broken" >&2
    exit 1
fi

# The dry run says what the bytes would be: the digest of the body, so
# the two twins can be compared with a diff. It is the body's own bytes
# -- the text the forgejo stores -- and not the escaped ones, which are
# the way the bytes travel and not the page.
if [ "$dry_run" -eq 1 ]; then
    body_file=$(mktemp) || { echo "there is no temporary file to write the body into" >&2; exit 1; }
    printf '%s' "$body" > "$body_file"
    body_digest=$(rgy_sha256 "$body_file")
    rm -f "$body_file"
    echo "the release body: $body_digest"
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
        if [ "$current_body" != "$escaped_body" ]; then
            curl -fsSL -X PATCH -H "Authorization: Basic $auth" \
                -H "Content-Type: application/json" \
                -d "{\"name\":\"rgy v$version\",\"body\":\"$escaped_body\"}" \
                "$repo_api/releases/$release_id" > /dev/null \
                || { echo "the release could not be updated" >&2; exit 1; }
            echo "release updated"
        fi
    else
        created=$(curl -fsSL -X POST -H "Authorization: Basic $auth" \
            -H "Content-Type: application/json" \
            -d "{\"tag_name\":\"$tag\",\"name\":\"rgy v$version\",\"body\":\"$escaped_body\"}" \
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
#
# The repository on GitHub is a flat mirror -- the whole tree in one commit,
# with no parents -- and the development history lives on the forgejo. The
# tool that makes that mirror lives on the maintainer's machine and is not in
# this tree, and it deletes and recreates the repository to do it: a push
# of the branches and the tag would replace the mirror with the full
# history, and a repository carrying stars would lose them — the stars
# live with the repository, not with the commits.
#
# So the mirror is the mirror tool's business and this script leaves it
# alone. The switch asks for the old behaviour, for the caller who really
# wants the branches and the tag pushed there.
if [ "$with_github" -eq 0 ]; then
    echo "the GitHub mirror is left alone: it is a flat mirror, and a push of the branches and the tag would replace it with the full history"
elif [ "$manifests_only" -eq 1 ]; then
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
        digests="$digests $arch=$(rgy_sha256 "$f")"
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
        echo "    $(rgy_sha256 "$f")  $(basename "$f")"
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
    #
    # The backup suffix is part of the call on purpose. GNU sed reads
    # `sed -i` as "in place" and BSD sed reads it as "in place, with the
    # next argument as the suffix" -- so the bare `-i` turns into a
    # second filename on a Mac, and the rewrite lands in a file nobody
    # reads. `-i.bak` is the one spelling both of them take, and the
    # backup goes away with the file it belongs to.
    sed -i.bak -E "s#$pattern#\1$sha\2#" "$file" || { echo "the pin in $file could not be rewritten" >&2; exit 1; }
    # The tag is three or four numbers deep -- the release carries the fourth,
    # the fix count of the line, and the registry takes three -- so the
    # pattern reads every one of them. A pattern that stops at the third
    # leaves the fourth on the end of the replacement: a second run of this
    # step over `rgy-v0.1.9.0` writes `rgy-v0.1.9.0.0`, and the installer
    # asks the forge for a tag nobody published.
    sed -i.bak -E "s#rgy-v[0-9]+(\.[0-9]+){2,3}#$tag#g" "$file" || exit 1
    rm -f "$file.bak"
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
        rgy_sha256 ".dist/$1" | tr 'a-f' 'A-F'
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
    # The pins, and the line each one lives on: the file, the asset, the
    # pattern that keeps the name and the quotes and replaces only the digest
    # between them, and the pattern that finds the line the pin is on. The
    # name, not a suffix: the release carries a portable archive and two
    # tuned ones, and the installer is allowed to install the portable one
    # only.
    pins=(
        "install.ps1|x86_64-windows.zip|^([[:space:]]*'x86_64-windows\.zip'[[:space:]]*=[[:space:]]*')[0-9A-Fa-f]{64}(')$|^[[:space:]]*'x86_64-windows\.zip'[[:space:]]*="
        "install.ps1|aarch64-windows.zip|^([[:space:]]*'aarch64-windows\.zip'[[:space:]]*=[[:space:]]*')[0-9A-Fa-f]{64}(')$|^[[:space:]]*'aarch64-windows\.zip'[[:space:]]*="
        "install.sh|x86_64-linux.tar.gz|^(expected_linux_x86_64=\")[0-9A-Fa-f]{64}(\")$|^expected_linux_x86_64="
        "install.sh|aarch64-linux.tar.gz|^(expected_linux_aarch64=\")[0-9A-Fa-f]{64}(\")$|^expected_linux_aarch64="
        "install.sh|x86_64-apple-darwin.tar.gz|^(expected_macos_x86_64=\")[0-9A-Fa-f]{64}(\")$|^expected_macos_x86_64="
        "install.sh|aarch64-apple-darwin.tar.gz|^(expected_macos_aarch64=\")[0-9A-Fa-f]{64}(\")$|^expected_macos_aarch64="
    )
    # Is the pin on that line still the placeholder? The placeholder is a
    # pin that says "there is no release for this platform yet"; a real
    # digest is a pin that names a release that exists.
    pin_is_placeholder() {
        line=$(grep -E "$2" "$1" 2>/dev/null | head -n 1)
        case "$line" in
            *"$placeholder"*) return 0 ;;
            *) return 1 ;;
        esac
    }
    # A file is rewritten only as a whole. The version lives in the file,
    # not in the pin, so a file where one pin moves and another does not
    # would carry a version and a digest that name different releases --
    # and the installer would refuse the archive it downloaded, which is
    # safe and useless. So when a pin names a release this run does not
    # carry, the file is left exactly as it is: the installer keeps
    # installing the release it can vouch for.
    for file in install.ps1 install.sh; do
        blocked=0
        for pin in "${pins[@]}"; do
            [ "${pin%%|*}" = "$file" ] || continue
            rest="${pin#*|}"
            asset="${rest%%|*}"
            rest="${rest#*|}"
            line_pattern="${rest#*|}"
            if [ -z "$(digest_of "rgy-v$version-$asset")" ] && ! pin_is_placeholder "$file" "$line_pattern"; then
                blocked=1
                break
            fi
        done
        if [ "$blocked" -eq 1 ]; then
            echo "installer pins: $file is left alone: it pins a release this run does not carry"
            continue
        fi
        for pin in "${pins[@]}"; do
            [ "${pin%%|*}" = "$file" ] || continue
            rest="${pin#*|}"
            asset="${rest%%|*}"
            rest="${rest#*|}"
            pattern="${rest%%|*}"
            digest=$(digest_of "rgy-v$version-$asset")
            if [ -z "$digest" ]; then
                echo "installer pin: rgy-v$version-$asset is not in this release; the pin stays a placeholder"
                digest="$placeholder"
            fi
            set_pin "$file" "$digest" "$pattern"
        done
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
    if [ "$with_github" -eq 1 ]; then
        echo "[dry-run]   $gh/releases/tag/$tag"
    else
        echo "[dry-run] the GitHub mirror would be left alone: the flat mirror is the mirror tool's business"
    fi
    exit 0
fi
echo "published $tag : $forgejo/releases/tag/$tag"
if [ "$with_github" -eq 1 ]; then
    echo "published $tag : $gh/releases/tag/$tag"
else
    echo "the GitHub mirror was left alone: the flat mirror is the mirror tool's business"
fi
