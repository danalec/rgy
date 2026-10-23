#!/usr/bin/env bash
# check-drift.sh -- report vendored code that has fallen behind its
# upstream source.
#
# The bash twin of scripts/check-drift.ps1. The two are twins, not
# cousins: same questions, same words, same bytes. A check that says "ok"
# on one platform and "FAIL" on the other is worse than no check at all,
# so the text is identical, not merely equivalent.
#
# scripts/check-vendoring.ps1 enforces the *shape* of the vendoring rule:
# no [patch] redirect, a provenance header on each vendored tree. None of
# that can notice that a file was copied on Monday and that the upstream
# changed it on Tuesday.
#
# This asks the other half: for each row in scripts/vendored-sources.tsv,
# has the upstream moved past the commit that row was taken from?
#
# The manifest has two kinds of row, and they drift in different ways.
#
#   tree -- a vendored crate, copied whole from upstream. The question is
#           whether upstream has touched that tree since the pin. Two
#           answers are needed, not one: which commits upstream made, and
#           which files those commits touched. The second is the actionable
#           one -- it names the files to look at. A tree that upstream has
#           not moved is fine, and says so by saying nothing.
#
#   patch -- a divergence from upstream, kept as a patch under
#           scripts/rgx-local-patches/. Two things can be wrong with it,
#           and both are drift:
#
#             * the patch no longer applies to upstream's current content
#               of the file. Upstream moved and the patch did not, so the
#               next sync would copy upstream's file and silently drop the
#               fix. That is reported loudly, because a patch that stops
#               applying is a fix that stops existing.
#
#             * the file in this tree is not what upstream's file plus the
#               patch produces. The copy and the patch disagree about what
#               the divergence is, so one of the two is wrong.
#
# A row whose pin is not a commit in the upstream checkout is UNPINNED and
# counts as a failure. A manifest that quietly leaves a row out, or pins it
# to a guess, is worse than one that admits it has not looked: the first
# one reads as "nothing to do here".
#
# The upstream checkout is a read-only input. Nothing in this repository is
# written, and nothing in the upstream is written either.
#
# The old --pin mode is gone. The manifest used to be written by hand, one
# hash per file, and a hand-written hash is a check that passes over drift;
# --pin picked the hash mechanically instead. The manifest is generated now
# (scripts/sync-rgx-upstream.sh writes the pin for every row when it copies
# the trees), so the pin column is the generator's to write: the sync takes
# the whole vendoring from one commit, and a per-row pin would say
# something the generator does not. Re-pin by running the sync.
#
# The report is written here, and not with the shell's plain echo alone,
# because the .ps1 writes the same escapes and the same LF endings: the
# two have to be comparable byte for byte and not only word for word. The
# report goes to stdout; the complaints go to stderr, in both scripts.
#
# Usage:
#   bash scripts/check-drift.sh --upstream <path>
#
# Run from anywhere; the script re-roots itself on the repository.
# Exits 1 on any drift or any unpinned row, 0 otherwise.
set -uo pipefail

script_path=$0
case "$script_path" in
    /*|[A-Za-z]:[/\\]*) ;;
    *) script_path=$PWD/$script_path ;;
esac
# A Windows path handed to this script has backslashes, and the cut below
# is on "/". A path with no "/" in it has no directory to cut, the guard
# falls back to the current directory, and this script ends up looking for
# the manifest one level above the repository root -- and reports a
# manifest that is not missing. The .ps1 twin resolves the same path
# correctly, so the two would answer differently on the same input, which
# is the one thing they may not do.
script_path=$(printf '%s' "$script_path" | tr '\\' '/')
script_dir=${script_path%/*}
[ "$script_dir" = "$script_path" ] && script_dir=.
here=$(cd "$script_dir" && pwd)
repo_root=$(cd "$here/.." && pwd)
cd "$repo_root" || exit 1

ESC=$'\033'
C_RED="$ESC[31m"
C_GREEN="$ESC[32m"
C_YELLOW="$ESC[33m"
C_CYAN="$ESC[36m"
C_NONE="$ESC[0m"

# report <colour> <text> -- the report, on stdout.
report() {
    local color="$1"
    shift
    case "$color" in
        red)    printf '%s%s%s\n' "$C_RED" "$*" "$C_NONE" ;;
        green)  printf '%s%s%s\n' "$C_GREEN" "$*" "$C_NONE" ;;
        yellow) printf '%s%s%s\n' "$C_YELLOW" "$*" "$C_NONE" ;;
        cyan)   printf '%s%s%s\n' "$C_CYAN" "$*" "$C_NONE" ;;
        *)      printf '%s\n' "$*" ;;
    esac
}

# complaint <text> -- the complaints, on stderr.
complaint() {
    printf '%s%s%s\n' "$C_RED" "$*" "$C_NONE" >&2
}

# usage -- what the two scripts are for, in the same words in both.
usage() {
    report "" "check-drift --upstream <path to an upstream checkout>"
    report "" ""
    report "" "Reports vendored code that has fallen behind its upstream source:"
    report "" "for each row in scripts/vendored-sources.tsv, has the upstream moved"
    report "" "past the commit that row was taken from? Exits 1 on any drift or any"
    report "" "unpinned row, 0 otherwise."
}

# The .ps1 prints paths the way PowerShell renders them: the repository
# root and the upstream as native absolute paths. Git Bash and WSL are
# Windows shells in a POSIX costume, so they print the Windows form;
# cygpath and wslpath do the translation.
native_path() {
    local p="$1"
    if command -v cygpath >/dev/null 2>&1; then
        cygpath -w "$p"
    elif command -v wslpath >/dev/null 2>&1; then
        case "$p" in
            /mnt/[a-z]/*) wslpath -w "$p" ;;
            *) printf '%s\n' "$p" ;;
        esac
    else
        printf '%s\n' "$p"
    fi
}

upstream=""
while [ $# -gt 0 ]; do
    case "$1" in
        --upstream) upstream="$2"; shift 2 ;;
        --pin|--for)
            complaint "--pin is gone: the pin column is written by scripts/sync-rgx-upstream.sh,"
            complaint "which pins every row to the commit the whole sync was taken from."
            complaint "Re-pin by running the sync."
            exit 1
            ;;
        -h|--help) usage; exit 0 ;;
        *) complaint "unknown argument: $1"; exit 1 ;;
    esac
done

manifest="$here/vendored-sources.tsv"
if [ ! -f "$manifest" ]; then
    complaint "no manifest at $(native_path "$manifest")"
    exit 1
fi

if [ -z "$upstream" ]; then
    complaint "no -Upstream path given; this check needs a local upstream checkout to ask"
    complaint "  example: scripts/check-drift.ps1 --upstream ../rgx"
    complaint "          scripts/check-drift.sh --upstream ../rgx"
    exit 1
fi

# The check reads the upstream with these. Without one of them the scan
# finds nothing, reports "ok" and exits 0: a check that passes without
# looking is worse than no check at all, so a missing tool is an error, not
# an empty result.
for tool in git awk sed tr mktemp; do
    if ! command -v "$tool" >/dev/null 2>&1; then
        complaint "check-drift: required tool not found: $tool"
        exit 1
    fi
done

upstream_dir=$(cd "$upstream" 2>/dev/null && pwd) || {
    complaint "no such directory: $upstream"
    exit 1
}
if [ ! -d "$upstream_dir/.git" ]; then
    complaint "$(native_path "$upstream_dir") is not a git checkout"
    exit 1
fi

# git, asked a question. The answer is the output lines; the exit code is
# checked where the answer can be "no" -- does this pin resolve? -- because
# "no such commit" is an answer and not a crash.
upstream_is_commit() {
    git -C "$upstream_dir" rev-parse --verify -q "$1^{commit}" >/dev/null 2>&1
}

# The commits upstream made under a path since the pin.
upstream_log() {
    git -C "$upstream_dir" log --oneline --no-color "$1..HEAD" -- "$2" 2>/dev/null
}

# The files those commits touched. This is the actionable half of the
# answer: the commits say what upstream did, the files say where to look.
upstream_changed_files() {
    git -C "$upstream_dir" diff --name-only --no-color "$1..HEAD" -- "$2" 2>/dev/null
}

# A copy with the carriage returns taken out.
#
# A checkout is not a change: the same source is CRLF on one machine and LF
# on another, and a patch that applies to one has to apply to the other.
# tr moves bytes and touches nothing else.
normalize() {
    tr -d '\r' < "$1" > "$2"
}

# Does the patch still apply to upstream's current content of the file?
#
# The test is run against a copy of upstream's file in a scratch directory,
# because that is the question the next sync will ask: the sync copies
# upstream's file over the one here and then applies the patch on top. A
# patch that no longer applies means upstream changed the file and the patch
# was not re-derived -- the next sync would take upstream's version and drop
# the fix without saying anything.
#
# Prints: ok | no-upstream-file | no-copy | does-not-apply | mismatch
patch_verdict() {
    local patch_rel="$1" rel="$2" work target patched mine a b
    [ -f "$upstream_dir/$rel" ] || { printf 'no-upstream-file'; return 0; }
    [ -f "$repo_root/$rel" ] || { printf 'no-copy'; return 0; }
    work=$(mktemp -d) || { printf 'mismatch'; return 0; }
    target="$work/$rel"
    mkdir -p "$(dirname "$target")" || { rm -rf "$work"; printf 'mismatch'; return 0; }
    normalize "$upstream_dir/$rel" "$target"
    normalize "$repo_root/$patch_rel" "$work/rgy.patch"
    # git apply, in the scratch directory: the patch's paths are the
    # repository's paths, and the file it names is the copy of upstream's
    # file sitting next to it.
    if ! ( cd "$work" && git apply --recount --whitespace=nowarn rgy.patch ) >/dev/null 2>&1; then
        rm -rf "$work"
        printf 'does-not-apply'
        return 0
    fi
    # The copy and the patch have to agree about what the divergence is.
    # Compare the patched upstream file with the file here.
    #
    # Both sides are normalised, including the patched one: git apply
    # writes the file back with the line endings the checkout's own rules
    # say, and a checkout is not a change.
    patched="$work/rgy-patched"
    normalize "$target" "$patched"
    mine="$work/rgy-mine"
    normalize "$repo_root/$rel" "$mine"
    a=$(git hash-object --no-filters -- "$patched")
    b=$(git hash-object --no-filters -- "$mine")
    rm -rf "$work"
    # An empty hash means git refused the file, and two refusals are not
    # an agreement.
    if [ -n "$a" ] && [ -n "$b" ] && [ "$a" = "$b" ]; then
        printf 'ok'
    else
        printf 'mismatch'
    fi
}

# --- the manifest ------------------------------------------------------------
#
# Five tab-separated columns: rgx_path, upstream path, pin, kind, note. The
# rows are the manifest minus its comments and blank lines. A row that does
# not have four columns cannot say what kind of row it is, and the manifest
# is generated, so a malformed one is a bug in the generator rather than
# something to report here.
#
# awk splits on the tab and keeps the empty fields; the shell's IFS does
# not, because a tab is IFS whitespace and a run of tabs is one separator.
# The fields are handed over separated by \001, which is not whitespace, so
# an empty pin stays an empty pin.
rows=$(mktemp) || exit 1
trap 'rm -f "$rows"' EXIT
awk '
    BEGIN { FS = "\t" }
    /^#/ { next }
    /^[[:space:]]*$/ { next }
    NF < 4 { next }
    {
        f1 = $1; f2 = $2; f3 = $3; f4 = $4; f5 = $5
        gsub(/^[[:space:]]+/, "", f1); gsub(/[[:space:]]+$/, "", f1)
        gsub(/^[[:space:]]+/, "", f2); gsub(/[[:space:]]+$/, "", f2)
        gsub(/^[[:space:]]+/, "", f3); gsub(/[[:space:]]+$/, "", f3)
        gsub(/^[[:space:]]+/, "", f4); gsub(/[[:space:]]+$/, "", f4)
        gsub(/^[[:space:]]+/, "", f5); gsub(/[[:space:]]+$/, "", f5)
        printf "%s\001%s\001%s\001%s\001%s\n", f1, f2, f3, f4, f5
    }
' "$manifest" > "$rows"

total=$(grep -c . "$rows" || true)

report cyan "checking vendored drift against upstream at $(native_path "$upstream_dir")"

# --- the check ---------------------------------------------------------------
checked=0
drifted=0
unpinned=0
unpinned_rows=""

report "" ""
report cyan "[1/1] vendored trees and patches against their upstream source"

while IFS=$'\001' read -r rgx up pin kind note; do
    [ -n "$rgx" ] || continue
    if [ ! -e "$repo_root/$rgx" ]; then
        report red "  FAIL: $rgx: listed in the manifest but not in the tree"
        drifted=$((drifted + 1))
        continue
    fi
    if [ -z "$pin" ] || [ "$pin" = "-" ]; then
        report yellow "  UNPINNED: $rgx: no sync point recorded (the pin is \"$pin\")"
        unpinned=$((unpinned + 1))
        unpinned_rows="$unpinned_rows$rgx
"
        continue
    fi
    if ! upstream_is_commit "$pin"; then
        report yellow "  UNPINNED: $rgx: pin $pin is not a commit in the upstream checkout"
        unpinned=$((unpinned + 1))
        unpinned_rows="$unpinned_rows$rgx
"
        continue
    fi

    checked=$((checked + 1))
    if [ "$kind" = "tree" ]; then
        commits="$(upstream_log "$pin" "$up")"
        n=$(printf '%s' "$commits" | grep -c . || true)
        if [ "$n" -gt 0 ]; then
            drifted=$((drifted + 1))
            report red "  FAIL: $rgx: upstream has moved since $pin ($n commit(s)):"
            printf '%s\n' "$commits" | while IFS= read -r line; do report red "    $line"; done
            files="$(upstream_changed_files "$pin" "$up")"
            if [ -n "$files" ]; then
                report red "    changed files:"
                printf '%s\n' "$files" | while IFS= read -r line; do report red "      $line"; done
            fi
        fi
    elif [ "$kind" = "patch" ]; then
        verdict="$(patch_verdict "$rgx" "$up")"
        case "$verdict" in
            ok) ;;
            no-upstream-file)
                report red "  FAIL: $rgx: upstream has no $up"
                drifted=$((drifted + 1))
                ;;
            no-copy)
                report red "  FAIL: $rgx: the patched file $up is not in the tree"
                drifted=$((drifted + 1))
                ;;
            does-not-apply)
                report red "  FAIL: $rgx: the patch no longer applies to upstream's current content of $up"
                drifted=$((drifted + 1))
                ;;
            mismatch)
                report red "  FAIL: $rgx: the copy does not match upstream's current content plus the patch"
                drifted=$((drifted + 1))
                ;;
        esac
    else
        report red "  FAIL: $rgx: unknown kind \"$kind\""
        drifted=$((drifted + 1))
    fi
done < "$rows"

if [ -n "$unpinned_rows" ]; then
    report yellow "  $unpinned row(s) have no recorded sync point:"
    printf '%s' "$unpinned_rows" | while IFS= read -r line; do report yellow "    - $line"; done
    report yellow "    pin one with: scripts/sync-rgx-upstream.sh --upstream <path>"
fi

report "" ""
if [ "$unpinned" -gt 0 ] || [ "$drifted" -gt 0 ]; then
    n=$((drifted + unpinned))
    report red "vendored drift: $n outstanding ($drifted drifted, $unpinned unpinned); $checked of $total row(s) checked"
    exit 1
fi
report green "ok: $total row(s) checked, no drift"
exit 0
