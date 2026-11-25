#!/usr/bin/env bash
# release-linux.sh -- build the Linux artifact of an rgy release and land it
# in .dist/ on this machine.
#
#   ./scripts/release-linux.sh [ref] [--host user@host]
#
# Flow: push the ref to the forgejo -> on the Linux host, clone it and run
# scripts/build-release.sh there -> copy the artifact and the digests back
# to .dist/.
#
# The Linux build has to happen on Linux: the binaries are linked against
# that platform's libc, and the install.sh pin and the release's Linux
# asset are both that build.
#
# The macOS artifact has no script here, because it has no host: it needs
# a Mac, and the release carries the brew formula for it when one is
# published.
#
# Requires only git, ssh and scp on this side. Built to run from WSL, Git
# Bash or a Linux box.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

REF="main"
# Where the Linux build runs, and the forgejo that hosts the release. Neither
# address is in this repository: a public repository that names somebody's
# private network is a map of that network, and the address is one line away
# from being read by everybody. Set both.
HOST="${RGY_RELEASE_HOST:-}"
FORGEJO_URL="${RGY_FORGEJO_URL:-}"

# The options, read by the helper the twins share: the long spelling and
# the PowerShell one are the same option, and an option neither of them
# knows is an error -- the same words, the same exit code. The ref is
# positional: it is the thing being released, not an option, and an unknown
# option is not a ref.
. "$ROOT/scripts/lib/options.sh"
rgy_read_options "" "host=HOST" "ref=REF" "$@"

if [ -z "$HOST" ]; then
    echo "RGY_RELEASE_HOST is not set: the Linux artifact is built on a Linux host" >&2
    echo "set it, or pass --host user@host" >&2
    exit 1
fi
if [ -z "$FORGEJO_URL" ]; then
    echo "RGY_FORGEJO_URL is not set: the forgejo that hosts the release is not in this repository" >&2
    echo "set it to the forgejo's base URL, e.g. RGY_FORGEJO_URL=http://host:3000" >&2
    exit 1
fi
FORGEJO="${FORGEJO_URL%/}/danalec/rgy.git"
REMOTE="/tmp/rgy-release"

VERSION="$(sed -n 's/^version = "\(.*\)"/\1/p' bins/searchctl/Cargo.toml | head -1)"
[ -n "$VERSION" ] || { echo "no version in bins/searchctl/Cargo.toml" >&2; exit 1; }
TAG="rgy-v$VERSION"

echo ">> rgy $VERSION ($TAG) on $HOST"

echo ">> pushing $REF to forgejo"
# The push is checked: a rejected push is invisible otherwise, and the host
# then clones the ref the forgejo still holds -- an archive of code that is
# not the code being released.
git push forgejo "$REF" 2>&1 | tail -1

echo ">> syncing the source on $HOST"
# The clone is the transport when it works, and the clone needs a credential
# the host does not always have: the forgejo repository is private, and a
# machine without a credential for it cannot read it at all -- the clone dies
# with "could not read Username", and the build then runs on whatever the
# forgejo last held, which is not the code being released.
#
# So the clone is tried, and when it fails the bytes of the commit travel
# instead: `git archive <ref>` is a tar of the tree the commit holds -- the
# same bytes a clone would bring -- and it needs no credential on the far
# side, because the tree is already here. The build does not use git, so the
# tree it gets is the tree either way.
#
# Which transport was used is said out loud: a release that says "synced" and
# built yesterday's code is the worst kind.
if ssh "$HOST" "rm -rf '$REMOTE' && mkdir -p '$REMOTE' && git clone --depth 1 --branch '$REF' '$FORGEJO' '$REMOTE/src'" 2>/dev/null; then
    echo "   the clone worked: the host read the forgejo itself"
else
    echo "   the clone did not work (a private repository and no credential on the host)"
    echo "   falling back to the bytes of the commit: git archive + scp"
    # The tar carries the prefix src/, so it unpacks into the remote and the
    # tree lands where the clone would have left it: $REMOTE/src.
    local_tar="$(mktemp)"
    git archive --prefix=src/ --format=tar "$REF" > "$local_tar"
    scp -q "$local_tar" "$HOST:/tmp/rgy-release-src.tar"
    rm -f "$local_tar"
    # The clone that failed may have left half a tree behind, and a file that
    # is in that half and not in the commit would be built and published as
    # if it were ours.
    ssh "$HOST" "rm -rf '$REMOTE/src' && mkdir -p '$REMOTE' && tar -xf /tmp/rgy-release-src.tar -C '$REMOTE' && rm -f /tmp/rgy-release-src.tar"
fi

echo ">> building on $HOST (this takes a few minutes)"
# The gate runs here too: the Windows release path runs it, and an asset
# that never passed the workspace's own tests is an asset nobody can say
# was tested.
ssh "$HOST" "bash '$REMOTE/src/scripts/build-release.sh' --version '$VERSION'"

echo ">> fetching the artifacts to .dist/"
mkdir -p .dist
# The digests are merged, not overwritten: the local build already wrote
# the Windows line, and the host's file has the Linux one.
if [ -f .dist/SHA256SUMS ]; then
    cp .dist/SHA256SUMS .dist/SHA256SUMS.local
fi
scp -q "$HOST:$REMOTE/src/.dist/rgy-v$VERSION-*.tar.gz" .dist/
scp -q "$HOST:$REMOTE/src/.dist/SHA256SUMS" .dist/SHA256SUMS.host
if [ -f .dist/SHA256SUMS.local ]; then
    cat .dist/SHA256SUMS.local .dist/SHA256SUMS.host | LC_ALL=C sort -k2 -u > .dist/SHA256SUMS
    rm -f .dist/SHA256SUMS.local .dist/SHA256SUMS.host
else
    mv .dist/SHA256SUMS.host .dist/SHA256SUMS
fi
# The digests of the documents the archive carries. The publish step refuses
# an archive without them -- "it was built by a build that did not write them"
# -- and the build that wrote them is the one that ran on the host, so they
# come from there too. Without this line the release stops at the publish and
# the digests have to be fetched by hand.
mkdir -p .dist/documents
scp -q "$HOST:$REMOTE/src/.dist/documents/rgy-v$VERSION-*.sha256" .dist/documents/
ssh "$HOST" "rm -rf '$REMOTE'"

echo ">> .dist/ now holds:"
ls -l .dist/
echo ">> next: publish-release.ps1 -Version $VERSION (bumps the winget digest and the installer pins)"
