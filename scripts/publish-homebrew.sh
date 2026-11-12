#!/usr/bin/env bash
# publish-homebrew.sh -- open the homebrew-core pull request for a release.
#
#   ./scripts/publish-homebrew.sh [--version 0.1.0] [--dry-run]
#
# Homebrew-core is a git repository like any other: a pull request is a
# branch, a commit and a merge. This script does the first two and opens
# the third.
#
#   1. check that the release the formula names actually exists, and that
#      its digest is the one the formula carries
#   2. clone homebrew-core (shallow: it is a big repository)
#   3. put the formula -- the one this repository carries, with the url,
#      the sha256 and the version the release flow just rewrote -- into a
#      branch
#   4. commit it and push the branch to the fork
#   5. open the pull request
#
# What the CI on that pull request runs is `brew audit --new --formula` and
# `brew readall`, and both need a Mac. The script says so here rather than
# pretending the pull request is the last step.
#
# Requires git, curl and gh (authenticated). The fork is created if it is
# not there yet.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

VERSION=""
FORMULA="rgy-search"
DRY_RUN=0
CORE_REPO="Homebrew/homebrew-core"
CORE_FORK="danalec/homebrew-core"

# The options, read by the helper the two twins share: the long spelling
# and the PowerShell one are the same option, and an option neither of
# them knows is an error on both platforms -- not a silent "do the real
# thing", which in this directory means opening a pull request.
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
# A pull request whose formula names a URL that answers with something
# other than the file is a pull request the CI will fail on, and the
# failure is slower and less clear than this one.
echo ">> checking the release"
if ! curl -fsSLI "$url" > /dev/null 2>&1; then
    echo "the release artifact is not there yet: $url" >&2
    echo "publish the release first (scripts/publish-release.ps1), then run this" >&2
    exit 1
fi

# --- 2. the fork ------------------------------------------------------------
if ! gh repo view "$CORE_FORK" > /dev/null 2>&1; then
    if [ "$DRY_RUN" -eq 1 ]; then
        echo "[dry-run] would create the fork $CORE_FORK"
    else
        echo ">> forking $CORE_REPO"
        gh repo fork "$CORE_REPO" --clone=false
    fi
fi

# --- 3. the branch ----------------------------------------------------------
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
echo ">> cloning $CORE_REPO"
git clone --quiet --depth 1 "https://github.com/$CORE_REPO.git" "$work/core"
branch="$FORMULA-$VERSION"
git -C "$work/core" checkout --quiet -b "$branch"
cp "$formula_file" "$work/core/$formula_file"

# --- 4. the commit ----------------------------------------------------------
git -C "$work/core" add "$formula_file"
git -C "$work/core" -c user.name="danalec" -c user.email="danalec@gmail.com" \
    commit --quiet -m "$FORMULA $VERSION"

if [ "$DRY_RUN" -eq 1 ]; then
    echo "[dry-run] would push $branch to $CORE_FORK and open the pull request"
    git -C "$work/core" show --stat --oneline HEAD
    exit 0
fi

echo ">> pushing $branch to $CORE_FORK"
git -C "$work/core" push --quiet "https://github.com/$CORE_FORK.git" "$branch"

# --- 5. the pull request ----------------------------------------------------
# The body answers the questions homebrew-core's template asks, because a
# pull request that answers them is one review round shorter.
body="$work/pr.md"
cat > "$body" <<EOF
$FORMULA $VERSION

**What is this tool doing?**

rgy answers two questions about the files on a computer: which file is it,
and what is inside it. Both in one query. The name half is answered from
an index of names -- fuzzy, typo-tolerant and ranked -- and the content
half reads the file.

    searchctl --root ~ "invoice ext:pdf"
    searchctl --root ~ "invoice grep:tesla"

**Does this pull request change the software that Homebrew installs?**

This is the first release: it adds the formula, so it adds the software.
There is no previous version in Homebrew to replace.

**Why should this be in Homebrew core?**

It is a command-line tool that installs from a prebuilt, signed-by-digest
archive, needs no system library beyond what macOS ships, and has a stable
tagged release. The repository carries the formula, the release script and
the installers: https://github.com/danalec/rgy

**What does the test do?**

It writes a file and asks the CLI how many files match its name, so the
test is the product doing its own job.

EOF

echo ">> opening the pull request"
gh pr create --repo "$CORE_REPO" \
    --head "$CORE_FORK:$branch" \
    --title "$FORMULA $VERSION" \
    --body-file "$body"

echo ""
echo ">> what happens next: the CI runs 'brew audit --new --formula' and"
echo ">> 'brew readall'. Both need a Mac; the audit checks the url, the"
echo ">> digest, the licence, the description and the test."
