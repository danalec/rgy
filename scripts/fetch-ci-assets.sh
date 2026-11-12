#!/usr/bin/env bash
# fetch-ci-assets.sh -- bring the archives a CI run built into .dist/.
#
#   ./scripts/fetch-ci-assets.sh <run-id>
#   ./scripts/fetch-ci-assets.sh --dir <directory-with-downloaded-artifacts>
#
# The release carries every archive the CI builds -- eleven platforms and the
# FreeBSD one, each on the runner that has it -- and the release is published
# from one machine. This is the step between the two: the CI keeps each row's
# archive (and the digests of the documents inside it) as an artifact, and
# this brings a run's artifacts into .dist/, where the publish step reads
# them. The archives of the version in .dist/ are replaced, not merged: a
# local build of the same version beside the run's would put two digests of
# "the" archive in one release, and the publish would pin whichever it read
# last.
#
# `--dir` is the same step without the network: a directory that holds what
# `gh run download` writes (one subdirectory per artifact), so a run whose
# artifacts were fetched by hand can be assembled the same way.
#
# The run has to have finished green: a release assembled from a run that
# failed somewhere is a release whose archives were built by the rows that
# worked, and nobody knows which rows those are.
#
# What lands in .dist/:
#
#   rgy-v<version>-<asset>.<zip|tar.gz>          the archive
#   documents/rgy-v<version>-<asset>.*.sha256    the digests of its documents
#   SHA256SUMS                                   over every archive there
set -u

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

RUN=""
DIR=""
REPO="danalec/rgy"

# The options, read by the helper the twins share: an option neither of them
# knows is an error -- the same words and the same exit code on both.
. "$ROOT/scripts/lib/options.sh"
. "$ROOT/scripts/lib/digest.sh"
rgy_read_options "" "dir=DIR repo=REPO" "run=RUN" "$@"

if [ -z "$RUN" ] && [ -z "$DIR" ]; then
    echo "usage: fetch-ci-assets.sh <run-id> | --dir <directory>" >&2
    exit 1
fi
if [ -n "$RUN" ] && [ -n "$DIR" ]; then
    echo "--dir and a run id are two different sources: name one of them" >&2
    exit 1
fi

# The release's version, out of the tree: the page's name, by the rule the
# build and publish scripts read the same way.
VERSION=$(bash "$ROOT/scripts/lib/version.sh") || exit 1
[ -n "$VERSION" ] || exit 1

WORK=""
if [ -n "$DIR" ]; then
    [ -d "$DIR" ] || { echo "no such directory: $DIR" >&2; exit 1; }
    WORK="$DIR"
    echo ">> rgy-v$VERSION: the artifacts in $DIR"
else
    command -v gh > /dev/null 2>&1 || { echo "gh not found: it is what reads a run's artifacts" >&2; exit 1; }
    status=$(gh run view "$RUN" --repo "$REPO" --json status,conclusion 2>/dev/null) || {
        echo "no run $RUN in $REPO" >&2
        exit 1
    }
    state=$(printf '%s' "$status" | sed -n 's/.*"status":"\([^"]*\)".*/\1/p')
    verdict=$(printf '%s' "$status" | sed -n 's/.*"conclusion":"\([^"]*\)".*/\1/p')
    echo ">> rgy-v$VERSION: the artifacts of run $RUN ($state/$verdict)"
    if [ "$state" != "completed" ] || [ "$verdict" != "success" ]; then
        echo "the run did not finish green: a release is assembled from a run that did" >&2
        exit 1
    fi
    WORK=$(mktemp -d)
    trap 'rm -rf "$WORK"' EXIT
    gh run download "$RUN" --repo "$REPO" --dir "$WORK" || {
        echo "the artifacts of run $RUN could not be downloaded" >&2
        exit 1
    }
fi

mkdir -p .dist/documents
# The archives of this version that .dist/ already holds are the previous
# source of this release, and a release has one source: they go.
echo ">> clearing .dist/ of the rgy-v$VERSION archives"
rm -f .dist/rgy-v"$VERSION"-*
rm -f .dist/documents/rgy-v"$VERSION"-*

# One subdirectory per artifact, and the tree inside it is the row's .dist/:
# the archive at the root and the document digests under documents/.
for art in "$WORK"/*/; do
    [ -d "$art" ] || continue
    for file in "$art"*.zip "$art"*.tar.gz; do
        [ -f "$file" ] || continue
        case "$(basename "$file")" in
            "rgy-v$VERSION-"*) mv -f "$file" .dist/ ;;
        esac
    done
    for sidecar in "$art"documents/*.sha256; do
        [ -f "$sidecar" ] || continue
        case "$(basename "$sidecar")" in
            "rgy-v$VERSION-"*) mv -f "$sidecar" .dist/documents/ ;;
        esac
    done
done

count=0
names=$(mktemp)
for archive in .dist/rgy-v"$VERSION"-*; do
    [ -f "$archive" ] || continue
    basename "$archive" >> "$names"
done
# The order is the ordinal one: the listing is read by a person and compared
# with the twin's, and a directory's enumeration order is the platform's
# habit and not the release's.
LC_ALL=C sort -o "$names" "$names"
while IFS= read -r name; do
    [ -n "$name" ] || continue
    if [ ! -f ".dist/documents/$name.sha256" ]; then
        echo "$name has no document digests: the run that built it did not keep them" >&2
        exit 1
    fi
    echo "   $name"
    count=$((count + 1))
done < "$names"
rm -f "$names"
if [ "$count" -eq 0 ]; then
    echo "no rgy-v$VERSION archives were found: the run built another version, or none" >&2
    exit 1
fi

# The digests, over every archive of this version that is here now: the
# entries the separate builds wrote are one archive each, and a release's
# file names all of them.
sums=$(mktemp)
for archive in .dist/rgy-v"$VERSION"-*; do
    [ -f "$archive" ] || continue
    printf '%s  %s\n' "$(rgy_sha256 "$archive")" "$(basename "$archive")" >> "$sums"
done
# The order is the ordinal one, and not the locale's: the file is read by
# `sha256sum -c` everywhere and compared byte for byte between the platforms.
LC_ALL=C sort -k2 "$sums" > .dist/SHA256SUMS
rm -f "$sums"

echo ">> .dist/ holds $count archives of rgy-v$VERSION and the SHA256SUMS over them"
