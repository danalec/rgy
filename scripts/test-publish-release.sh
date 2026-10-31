#!/usr/bin/env bash
# test-publish-release.sh -- the two checks the publish script makes on
# the release body and on the release page.
#
# The Linux/macOS counterpart of scripts/test-publish-release.ps1.
#
#   bash scripts/test-publish-release.sh
#   pwsh -File scripts/test-publish-release.ps1
#
# Two checks live here, and both of them are checks the publish script
# makes before it publishes anything.
#
# The first one is the escaping of the release body. The body is a JSON
# string, and the escaping of it used to run on the first line only: the
# body that was POSTed kept the raw quotes of every line but the first,
# so it was not JSON, and the forgejo answered 400. The publish scripts
# carry a check for that now -- the escaped body is parsed before
# anything is sent -- but that check only runs when somebody publishes,
# and the break it was written for survived a long time with nothing
# testing it.
#
# The second one is the comparison between the release page and the
# assets of the release. The page names the whole release, and a publish
# compares the two both ways: the sets have to be equal. A run that
# rewrites the manifests and publishes nothing compares them one way
# only -- the release is built one row at a time, and a row carries one
# archive of it -- and that difference is the one the CI found: the two
# modes answer different questions, and one sentence cannot answer both.
#
# So the checks are here, on their own, and they ask the same questions
# on every push. The helpers are taken OUT of
# scripts/publish-release.sh -- the code that publishes, and not a copy
# of it -- and the body is the tree's own: a quote, a backslash and a
# newline, the three a release body can carry.
#
#   1. is the escaped body a JSON string a parser accepts?
#   2. is the parsed body the body that went in?
#   3. are the bytes the ones the twin produces?
#   4. would the check have refused the old escaping?
#   5. does a publish refuse a page that does not match the release, and
#      accept one that does?
#   6. does a manifests-only run refuse an archive the page does not
#      name, and accept a page that names the other rows' assets?
#
# The exit code is 0 when the six answers are yes and 1 when one of them
# is not, and the line that says which one is the line a human reads.
#
# The scripts find what they need next to themselves -- the publish
# script and the digest helper -- and they write nothing into the
# repository: the work happens in a directory of their own, and it goes
# away with them.
set -u

here=$(cd "$(dirname "$0")" && pwd)
publish="$here/publish-release.sh"

# The options, read by the helper the two twins share: this script takes
# none, so every argument is an error — the same words and the same exit
# code on both platforms. A test that ignores what it does not understand
# runs the whole test when the caller asked for something else.
. "$here/lib/options.sh"
rgy_read_options "" "" "" "$@"

fail() {
    echo "test-publish-release: FAILED: $1" >&2
    exit 1
}

work=$(mktemp -d) || fail "there is no temporary directory to work in"
trap 'rm -rf "$work"' EXIT

echo "test-publish-release: the escaping of the release body"

command -v python3 > /dev/null 2>&1 ||
    fail "the JSON parser the check needs is not on this machine"

# The body: the tree's own release body, in the shape it has on the
# release page. The quote and the backslash are the two the escaping
# has to carry, and the lines are the newlines it has to join.
body=$(cat <<'RGY_BODY'
Whole-filesystem name search and content search in one stack.

**Which file?** -- asked over the whole filesystem, then **what is inside it?** in the same query.

    searchctl --root ~ "invoice ext:pdf"
    searchctl --root ~ "invoice grep:tesla"

The release carries four binaries: ``searchctl`` (the command-line tool),
``searchd`` (the daemon: JSON lines over stdio, a pipe or a unix socket),
``rgy-autostart`` (put the daemon in place at login) and ``rgxy`` (the
launcher over the daemon: a floating window where there is a desktop, the
terminal where there is not).

A Windows path is a backslash too: C:\Users\rgy\searchctl.exe

**Assets**
- ``rgy-v0.1.2-x86_64-linux.tar.gz``
- ``SHA256SUMS``

Apache-2.0.
RGY_BODY
)

# The escaping helper, taken out of the publish script: the functions are
# used as they are there, and a copy of them is what would drift.
sed -n '/^json_escape() {$/,/^}$/p' "$publish" > "$work/json_escape.sh"
sed -n '/^json_escape_is_valid() {$/,/^}$/p' "$publish" > "$work/json_escape_is_valid.sh"
[ -s "$work/json_escape.sh" ] ||
    fail "the escaping helper was not found in scripts/publish-release.sh"
[ -s "$work/json_escape_is_valid.sh" ] ||
    fail "the check the publish script uses was not found in scripts/publish-release.sh"
# shellcheck source=/dev/null
. "$work/json_escape.sh"
# shellcheck source=/dev/null
. "$work/json_escape_is_valid.sh"
# shellcheck source=/dev/null
. "$here/lib/digest.sh"

escaped=$(json_escape "$body")

# 1. the escaped body is a JSON string a parser accepts: the parser the
#    machine has, and the check the publish script itself uses.
printf '"%s"' "$escaped" |
    python3 -c "import json,sys; json.loads(sys.stdin.read())" > /dev/null 2>&1 ||
    fail "the escaped body is not valid JSON"
json_escape_is_valid "$escaped" ||
    fail "the escaped body is not valid JSON"
echo "ok: the escaped body is valid JSON"

# 2. the parsed body is the body that went in.
#
# The bytes come out through `sys.stdout.buffer`, not through
# `sys.stdout`: the text stream translates a newline into the platform's
# line ending on the way out, so on Windows a body of LF lines came back
# as CRLF lines and the comparison below failed on a body that had
# nothing wrong with it. The twin on the other side of this test parses
# the same string with a parser that does not translate anything, so the
# two twins disagreed about the same body on the same machine -- and the
# one that disagreed was the one running on Windows.
parsed=$(printf '"%s"' "$escaped" |
    python3 -c "import json,sys; sys.stdout.buffer.write(json.loads(sys.stdin.read()).encode())") ||
    fail "the parsed body is not the body that went in"
[ "$parsed" = "$body" ] ||
    fail "the parsed body is not the body that went in"
echo "ok: the parsed body is the body that went in"

# 3. the bytes are the ones the twin produces. The digest below is the
#    same constant in the two twins: it is what a reference JSON
#    serializer writes for this body, and each twin has to arrive at it.
#    So the two twins produce the same bytes, and a twin that escapes
#    differently is caught here.
printf '%s' "$escaped" > "$work/escaped.json"
digest=$(rgy_sha256 "$work/escaped.json")
[ "$digest" = "ca28e40ba5921cc6f7d59454651c69f88bf50ea55ae5894c3a05e7c1165e948f" ] ||
    fail "the escaped body is not the bytes the twin produces"
echo "ok: the escaped body is the bytes the twin produces"

# 4. the old escaping -- the one that escaped the first line only -- is
#    refused by the check. This is the regression: a check that accepts
#    the old output catches nothing, and the break this test is written
#    for is the one that check has to catch.
old_first=$(printf '%s\n' "$body" | head -1 | sed -e 's/\\/\\\\/g' -e 's/"/\\"/g')
old_rest=$(printf '%s\n' "$body" | tail -n +2)
old="$old_first
$old_rest"
if json_escape_is_valid "$old"; then
    fail "the old escaping was not refused by the check"
fi
if printf '"%s"' "$old" |
    python3 -c "import json,sys; json.loads(sys.stdin.read())" > /dev/null 2>&1; then
    fail "the old escaping was not refused by the check"
fi
echo "ok: the old escaping is refused by the check"

# 5. the assets check of a publish. The page names the whole release,
#    and a publish compares the page with the release both ways: the two
#    sets have to be equal, and a page that names an archive the release
#    does not carry -- and a release that carries an archive the page
#    does not name -- stop the publish.
#
# The helper is taken out of the publish script, the way the escaping
# helper is: a copy of it is what would drift.
sed -n '/^names_in_a_line() {$/,/^}$/p' "$publish" > "$work/names_in_a_line.sh"
sed -n '/^release_assets_refusal() {$/,/^}$/p' "$publish" > "$work/release_assets_refusal.sh"
[ -s "$work/names_in_a_line.sh" ] ||
    fail "the helper the assets check uses was not found in scripts/publish-release.sh"
[ -s "$work/release_assets_refusal.sh" ] ||
    fail "the assets check was not found in scripts/publish-release.sh"
# shellcheck source=/dev/null
. "$work/names_in_a_line.sh"
# shellcheck source=/dev/null
. "$work/release_assets_refusal.sh"

publish_refusal=$(release_assets_refusal publish docs/release-0.1.2.md "rgy-v0.1.2-x86_64-linux.tar.gz" "rgy-v0.1.2-avx2-windows.zip")
publish_expected="the release page does not match the assets of this release: the page names [rgy-v0.1.2-x86_64-linux.tar.gz] the release does not carry, and the release carries [rgy-v0.1.2-avx2-windows.zip] the page does not name"
[ "$publish_refusal" = "$publish_expected" ] ||
    fail "the publish mode did not refuse a page that does not match the release"
[ -z "$(release_assets_refusal publish docs/release-0.1.2.md "" "")" ] ||
    fail "the publish mode refused a page that matches the release"
echo "ok: the publish mode refuses a page that does not match the release"

# 6. the assets check of a run that rewrites the manifests and publishes
#    nothing. The page names the whole release and the run carries one
#    archive of it, so the page may name assets the run does not have --
#    the other rows' assets -- and that is not a refusal. An archive the
#    run carries and the page does not name is one: the release does not
#    carry it either, and the sentence says so in its own words.
row_refusal=$(release_assets_refusal manifests-only docs/release-0.1.2.md "" "rgy-v0.1.2-avx2-windows.zip")
row_expected=$(printf '%s\n%s\n%s' \
    "the release page does not name the assets of this run: this run carries [rgy-v0.1.2-avx2-windows.zip] the page does not name" \
    "  the assets marker in docs/release-0.1.2.md names the whole release, and every archive this run carries has to be in it" \
    "  add the name to the marker and run this again")
[ "$row_refusal" = "$row_expected" ] ||
    fail "the manifests-only mode did not refuse an asset the page does not name"
[ -z "$(release_assets_refusal manifests-only docs/release-0.1.2.md "rgy-v0.1.2-x86_64-windows.zip" "")" ] ||
    fail "the manifests-only mode refused a page that names the other rows' assets"
echo "ok: the manifests-only mode refuses an asset the page does not name"

echo "escaped body sha256: $digest"
echo "test-publish-release: ok"
exit 0
