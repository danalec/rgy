#!/usr/bin/env bash
# publish-homebrew.sh -- publish the formula to the personal Homebrew tap.
#
#   ./scripts/publish-homebrew.sh [--version 0.1.0] [--dry-run]
#
# The tap is danalec/homebrew-rgy, NOT Homebrew/homebrew-core. The latter is
# Homebrew's own source repository; a formula submitted there is a pull
# request to somebody else's project, with their review, their CI and their
# template. The rgy formula is the rgy project's own, so it lives in a tap
# the project owns and is pushed straight to its main branch.
#
#   1. check that the release the formula names actually exists, and that
#      its digest is the one the formula carries
#   2. clone the tap (shallow: it is one formula and a README)
#   3. put the formula -- the one this repository carries, with the url,
#      the sha256 and the version the release flow just rewrote -- into it
#   4. commit and push to main
#
# No fork, no pull request, no macOS CI of somebody else's repository. The
# formula is verified against the published release before it is pushed.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

VERSION=""
FORMULA="rgy-search"
DRY_RUN=0
TAP_REPO="danalec/homebrew-rgy"

# The options, read by the helper the two twins share: the long spelling
# and the PowerShell one are the same option, and an option neither of
# them knows is an error on both platforms -- not a silent "do the real
# thing", which in this directory means pushing a release to a tap.
. "$ROOT/scripts/lib/options.sh"
rgy_read_options "dryrun=DRY_RUN" "version=VERSION formula=FORMULA" "" "$@"

formula_file="Formula/$FORMULA.rb"
if [ ! -f "$formula_file" ]; then
    echo "no formula at $formula_file" >&2
    exit 1
fi

# The version comes from the formula, and the formula's url and sha256
# come from the release flow: they are the release's own data, rewritten
# when the release was published.
if [ -z "$VERSION" ]; then
    VERSION=$(sed -n 's/^ *version "\(.*\)"/\1/p' "$formula_file" | head -1)
fi
[ -n "$VERSION" ] || { echo "no version in $formula_file" >&2; exit 1; }

url=$(sed -n 's/^ *url "\(.*\)"/\1/p' "$formula_file" | head -1)
sha=$(sed -n 's/^ *sha256 "\(.*\)"/\1/p' "$formula_file" | head -1)
[ -n "$url" ] || { echo "no url in $formula_file" >&2; exit 1; }
[ -n "$sha" ] || { echo "no sha256 in $formula_file" >&2; exit 1; }

echo ">> rgy-search $VERSION ($FORMULA)"
echo ">> url   $url"
echo ">> sha256 $sha"

# --- 1. the release has to exist --------------------------------------------
# A formula that names a URL that answers with something other than the
# file is a formula that installs nothing. The check is here, before the
# tap is touched.
echo ">> checking the release"
if ! curl -fsSLI "$url" > /dev/null 2>&1; then
    echo "the release artifact is not there yet: $url" >&2
    echo "publish the release first (scripts/publish-release.ps1), then run this" >&2
    exit 1
fi

# --- 2. the tap -------------------------------------------------------------
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
echo ">> cloning $TAP_REPO"
git clone --quiet --depth 1 "https://github.com/$TAP_REPO.git" "$work/tap"
cp "$formula_file" "$work/tap/$formula_file"

# --- 3. the commit ----------------------------------------------------------
git -C "$work/tap" add "$formula_file"
# A formula whose bytes did not change is a release that was already
# published to the tap; do not make an empty commit for it.
if ! git -C "$work/tap" -c user.name="danalec" -c user.email="danalec@gmail.com" \
    commit --quiet -m "$FORMULA $VERSION"; then
    echo ">> the tap already carries this formula; nothing to publish"
    exit 0
fi

if [ "$DRY_RUN" -eq 1 ]; then
    echo "[dry-run] would push $FORMULA $VERSION to $TAP_REPO main"
    git -C "$work/tap" show --stat --oneline HEAD
    exit 0
fi

echo ">> pushing $FORMULA $VERSION to $TAP_REPO main"
git -C "$work/tap" push --quiet "https://github.com/$TAP_REPO.git" HEAD:main

echo ""
echo ">> published to https://github.com/$TAP_REPO"
echo ">> install: brew tap danalec/rgy && brew install $FORMULA"
