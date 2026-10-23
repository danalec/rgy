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
# Where the Linux build runs. Overridable so the published script does not
# hardcode one person's box; the default is the host this was written on.
HOST="${RGY_RELEASE_HOST:-danalec@192.168.0.40}"
FORGEJO="http://192.168.0.10:3000/danalec/rgy.git"
REMOTE="/tmp/rgy-release"

while [ $# -gt 0 ]; do
    case "$1" in
        --host) HOST=${2:-}; shift 2 ;;
        *) REF="$1"; shift ;;
    esac
done

VERSION="$(sed -n 's/^version = "\(.*\)"/\1/p' bins/searchctl/Cargo.toml | head -1)"
[ -n "$VERSION" ] || { echo "no version in bins/searchctl/Cargo.toml" >&2; exit 1; }
TAG="rgy-v$VERSION"

echo ">> rgy $VERSION ($TAG) on $HOST"

echo ">> pushing $REF to forgejo"
git push forgejo "$REF" 2>&1 | tail -1 || true

echo ">> syncing the source on $HOST"
ssh "$HOST" "rm -rf '$REMOTE' && mkdir -p '$REMOTE' && git clone --depth 1 --branch '$REF' '$FORGEJO' '$REMOTE/src' 2>&1 | tail -1"

echo ">> building on $HOST (this takes a few minutes)"
ssh "$HOST" "bash '$REMOTE/src/scripts/build-release.sh' --skip-tests --version '$VERSION'"

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
    cat .dist/SHA256SUMS.local .dist/SHA256SUMS.host | sort -k2 -u > .dist/SHA256SUMS
    rm -f .dist/SHA256SUMS.local .dist/SHA256SUMS.host
else
    mv .dist/SHA256SUMS.host .dist/SHA256SUMS
fi
ssh "$HOST" "rm -rf '$REMOTE'"

echo ">> .dist/ now holds:"
ls -l .dist/
echo ">> next: publish-release.ps1 -Version $VERSION (bumps the winget digest and the installer pins)"
