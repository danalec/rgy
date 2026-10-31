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
#   tree -- a vendored crate, copied whole from upstream. Two questions
#           are asked of it, and they are different questions. The first
#           is whether upstream has touched that tree since the pin. Two
#           answers are needed for that, not one: which commits upstream
#           made, and which files those commits touched. The second is
#           the actionable one -- it names the files to look at. A tree
#           that upstream has not moved is fine, and says so by saying
#           nothing.
#
#           The second question is whether the copy here is the tree
#           upstream holds at the pin, byte for byte. That is the half
#           the rule is about: these trees are upstream's, and a local
#           edit in one of them is a fork nobody asked for. It is
#           answered by comparing blob hashes, because two equal hashes
#           are two equal files: the upstream side from the pinned
#           commit, and our side from the index, which is what a clone
#           holds. The files that have a patch row of their own are left
#           out -- the patch row checks them -- and so are the excluded
#           paths, which upstream has and this repository does not carry.
#           A file of ours that upstream does not have is a fork, and is
#           reported as one.
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
# For a patch row the question is exact: is the file this repository holds
# the upstream blob at the pinned commit with the local patch applied on
# top? The three sides of that question are read from three places, and
# each is the place that answers it:
#
#   upstream  the blob AT THE COMMIT, because the sync copies the commit
#             and not the working tree upstream happens to have
#   patched   that blob with the patch applied, in a scratch directory
#             outside the repository
#   mine      the repository's own bytes, from the index, because the
#             index is what a fresh clone gets
#
# Nothing is normalised. A checkout is not a change, but the repository is
# not a checkout: the index holds the bytes upstream's blob holds, and two
# files that differ only in their line endings are two files. The gate
# that folded that difference away is the gate that let four testdata
# READMEs hide from it.
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

# The help, before the options are read: the script's own header, the way
# the twin prints it.
. "$here/lib/options.sh"
if rgy_help_requested "$@"; then
    usage
    exit 0
fi

# The options, read by the helper the two twins share: the long spelling
# and the PowerShell one are the same option, and an option neither of
# them knows is an error on both platforms -- not a silent "check
# something else", which in a drift gate is a gate that did not run.
upstream=""
pin=0
for_value=""
rgy_read_options "pin=pin" "upstream=upstream for=for_value" "" "$@"

if [ "$pin" -eq 1 ] || [ -n "$for_value" ]; then
    complaint "--pin is gone: the pin column is written by scripts/sync-rgx-upstream.sh,"
    complaint "which pins every row to the commit the whole sync was taken from."
    complaint "Re-pin by running the sync."
    exit 1
fi

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

# The upstream file, as the pinned commit holds it.
#
# The sync copies the COMMIT and not the working tree: the manifest pins a
# commit, so the copy has to be that commit's bytes, and a dirty upstream
# working tree would otherwise decide what this repository holds. The gate
# has to ask the question the sync answers, so it reads the same bytes.
#
# The exit code is the answer: a path the commit does not have is an
# answer ("no such file"), not a crash.
upstream_blob() {
    git -C "$upstream_dir" cat-file blob "$1:$2"
}

# The file this repository holds, as the repository holds it.
#
# The index and not the working tree: with core.autocrlf the working tree
# holds CRLF for a file whose blob is LF, and comparing a working tree
# against an upstream blob reports a difference that is not one. The index
# is what a fresh clone gets, so it is what the gate compares.
repo_blob() {
    git -C "$repo_root" cat-file blob ":$1"
}

# The hash of an index entry, which is the hash of the bytes a fresh clone
# would check out.
repo_index_hash() {
    git -C "$repo_root" rev-parse --verify ":$1"
}

# Does the patch still apply to upstream's content of the file at the pin?
#
# The test is run against a copy of that file in a scratch directory,
# because that is the question the next sync will ask: the sync copies
# upstream's file over the one here and then applies the patch on top. A
# patch that no longer applies means upstream changed the file and the patch
# was not re-derived -- the next sync would take upstream's version and drop
# the fix without saying anything.
#
# Does the patch create the file, or change a file that is already there?
#
# A patch that creates a file has no upstream behind it: the file is the
# patch's own content, and the tree has to hold exactly that. A patch
# that changes a file needs upstream's content to change, and a file
# upstream no longer has is a file the next sync has nothing to put the
# patch on top of.
#
# The argument is the patch file in the scratch directory: the patch, as
# the repository holds it.
patch_creates_file() {
    grep -q -E '^(--- /dev/null|new file mode)' "$1"
}

# Prints: ok | no-upstream-file | no-copy | not-in-index | does-not-apply | mismatch
#
# The question is the one the sync asks: is the file this repository holds
# the upstream blob at the pin with the local patch applied on top?
#
# The comparison is by bytes and nothing is folded away: two files that
# differ only in their line endings are two files.
patch_verdict() {
    local patch_rel="$1" rel="$2" pin="$3"
    local work target creates=0 mine_hash patched_hash
    work=$(mktemp -d) || { printf 'mismatch'; return 0; }
    # The patch, as the repository holds it: the index, like the rest of
    # this repository's own bytes.
    #
    # A patch that has not been staged is not in the index, so the gate
    # cannot see it and says the patch does not apply. That is
    # not a broken patch: it is a patch that has not been added to the
    # repository yet. Stage it, and the gate sees it.
    #
    # The verdict says so, because a gate that fails with the wrong reason
    # is worse than no gate: "the patch no longer applies" sends the reader
    # to re-derive a patch that never stopped applying, and the re-derivation
    # is an edit to a file the reader was not asked to touch.
    if ! repo_blob "$patch_rel" > "$work/rgy.patch" 2>/dev/null; then
        rm -rf "$work"
        printf 'not-in-index'
        return 0
    fi
    if patch_creates_file "$work/rgy.patch"; then
        creates=1
    fi
    target="$work/$rel"
    mkdir -p "$(dirname "$target")" || { rm -rf "$work"; printf 'mismatch'; return 0; }
    # Upstream has the file at the pin: the copy is the file and the patch
    # goes on top of it. Upstream never had it: the patch creates it, and
    # there is nothing to copy.
    if ! upstream_blob "$pin" "$rel" > "$target" 2>/dev/null; then
        rm -f "$target"
        if [ "$creates" -eq 0 ]; then
            rm -rf "$work"
            printf 'no-upstream-file'
            return 0
        fi
    fi
    # The copy in this repository, from the index.
    mine_hash=$(repo_index_hash "$rel" 2>/dev/null) || mine_hash=""
    if [ -z "$mine_hash" ]; then
        rm -rf "$work"
        printf 'no-copy'
        return 0
    fi
    # git apply, in the scratch directory: the patch's paths are the
    # repository's paths, and the file it names is the copy of upstream's
    # file sitting next to it.
    #
    # core.autocrlf=false on purpose: the apply has to write exactly the
    # bytes of the patch, and a checkout that rewrote the line endings
    # would write a file the repository does not have. The scratch
    # directory is outside the repository for the same reason: the
    # repository's own rules must not answer for it.
    if ! ( cd "$work" && git -c core.autocrlf=false apply --recount --whitespace=nowarn rgy.patch ) >/dev/null 2>&1; then
        rm -rf "$work"
        printf 'does-not-apply'
        return 0
    fi
    # The copy and the patch have to agree about what the divergence is.
    patched_hash=$(git hash-object --no-filters -- "$target" 2>/dev/null) || patched_hash=""
    rm -rf "$work"
    # An empty hash means git refused the file, and two refusals are not
    # an agreement.
    if [ -n "$patched_hash" ] && [ -n "$mine_hash" ] && [ "$patched_hash" = "$mine_hash" ]; then
        printf 'ok'
    else
        printf 'mismatch'
    fi
}

# The tree upstream holds at the pin, and the tree this repository holds.
#
# Both sides come out of one call each: `ls-tree` gives the files of the
# commit, `ls-files` gives the entries of the index. Two equal blob
# hashes are two equal files, so the comparison never looks at the bytes
# themselves -- and it does not look at the working tree either, because
# the working tree is not what a clone holds.
#
# Three kinds of path are left out of the comparison, and each of them is
# left out for a reason:
#
#   * the files that have a patch row of their own. The patch row is
#     what checks them, byte for byte, against upstream plus the patch,
#     and a file a patch creates is not upstream's at all.
#
#   * the excluded paths. They are upstream's files that this repository
#     does not carry, so their absence is the rule and not a difference.
#     The ones of them that the index holds are reported on their own:
#     the sync removes them, so one that is here is a divergence.
#
# Prints one line per difference:
#   missing: <path>   upstream has it, the copy does not
#   differs: <path>   both have it, and the bytes are not the same
#   fork: <path>      the copy has it, upstream does not
#   excluded: <path>  an excluded path is in the index
tree_problems() {
    local up="$1" our="$2" pin="$3"
    local up_pairs our_pairs problems
    up_pairs=$(mktemp) || { printf 'error: the trees could not be compared\n'; return 1; }
    our_pairs=$(mktemp) || { rm -f "$up_pairs"; printf 'error: the trees could not be compared\n'; return 1; }
    # "<blob hash>\t<path>" for each side. `ls-tree` prints
    # "<mode> SP <type> SP <hash> TAB <path>" and `ls-files` prints
    # "<mode> SP <hash> SP <stage> TAB <path>", so the hash is the third
    # field of the first one and the second of the second.
    git -C "$upstream_dir" ls-tree -r "$pin" -- "$up" \
        | awk -F'\t' '{ split($1, f, " "); print f[3] "\t" $2 }' > "$up_pairs"
    git -C "$repo_root" ls-files -s -- "$our" \
        | awk -F'\t' '{ split($1, f, " "); print f[2] "\t" $2 }' > "$our_pairs"
    # The comparison, with the paths of the other two lists read first:
    # the exclusions, and the paths the patch rows own.
    problems="$(awk -F'\t' \
        -v exclfile="$excludes" -v patfile="$patch_paths" \
        -v upfile="$up_pairs" -v ourfile="$our_pairs" '
        function is_excluded(path,   i) {
            for (i = 1; i <= ne; i++) {
                if (index(path, excl[i]) == 1) { return 1 }
            }
            return 0
        }
        FILENAME == exclfile { excl[++ne] = $0; next }
        FILENAME == patfile { patched[$0] = 1; next }
        FILENAME == upfile {
            path = $2
            if (is_excluded(path) || (path in patched)) { next }
            up[path] = $1; uporder[++nup] = path; next
        }
        FILENAME == ourfile {
            path = $2
            if (is_excluded(path)) { exorder[++nex] = path; next }
            if (path in patched) { next }
            ours[path] = $1; ourorder[++nout] = path; next
        }
        END {
            for (i = 1; i <= nup; i++) {
                p = uporder[i]
                if (!(p in ours)) { print "missing: " p }
            }
            for (i = 1; i <= nup; i++) {
                p = uporder[i]
                if ((p in ours) && up[p] != ours[p]) { print "differs: " p }
            }
            for (i = 1; i <= nout; i++) {
                p = ourorder[i]
                if (!(p in up)) { print "fork: " p }
            }
            for (i = 1; i <= nex; i++) { print "excluded: " exorder[i] }
        }
    ' "$excludes" "$patch_paths" "$up_pairs" "$our_pairs")" \
        || { rm -f "$up_pairs" "$our_pairs"; printf 'error: the trees could not be compared\n'; return 1; }
    rm -f "$up_pairs" "$our_pairs"
    printf '%s\n' "$problems"
}

# --- the manifest ------------------------------------------------------------
#
# Five tab-separated columns: rgx_path, upstream path, pin, kind, note. The
# rows are the manifest minus its comments and blank lines. A row that does
# not have four columns cannot say what kind of row it is, and the manifest
# is generated, so a malformed one is a bug in the generator rather than
# something to report here.
#
# The manifest carries the excluded paths as comment lines, one per line,
# each of them "# exclude: <path>". A comment line is not a row, so the
# rows stay the rows and the count of them does not move; the exclusions
# are read out of the same file the rows come from, because the sync is
# what writes both and there is one answer to what is excluded.
#
# awk splits on the tab and keeps the empty fields; the shell's IFS does
# not, because a tab is IFS whitespace and a run of tabs is one separator.
# The fields are handed over separated by \001, which is not whitespace, so
# an empty pin stays an empty pin.
rows=$(mktemp) || exit 1
excludes=$(mktemp) || exit 1
patch_paths=$(mktemp) || exit 1
trap 'rm -f "$rows" "$excludes" "$patch_paths"' EXIT
awk -v exclfile="$excludes" '
    BEGIN { FS = "\t" }
    /^# exclude: / { print substr($0, 12) > exclfile; next }
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

# The paths that have a patch row of their own. The tree comparison
# leaves them out of both sides: the patch row is what checks them, byte
# for byte, against upstream plus the patch. A file a patch creates is in
# here too, and that is what keeps it from being a fork.
while IFS=$'\001' read -r rgx up pin kind note; do
    if [ "$kind" = "patch" ]; then
        printf '%s\n' "$up"
    fi
done < "$rows" > "$patch_paths"

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
        # One row is one drift, whatever it complains about: the two
        # questions below are two questions about the same row.
        row_drifted=0
        commits="$(upstream_log "$pin" "$up")"
        n=$(printf '%s' "$commits" | grep -c . || true)
        if [ "$n" -gt 0 ]; then
            row_drifted=1
            report red "  FAIL: $rgx: upstream has moved since $pin ($n commit(s)):"
            printf '%s\n' "$commits" | while IFS= read -r line; do report red "    $line"; done
            files="$(upstream_changed_files "$pin" "$up")"
            if [ -n "$files" ]; then
                report red "    changed files:"
                printf '%s\n' "$files" | while IFS= read -r line; do report red "      $line"; done
            fi
        fi
        # And the other half: the copy has to be upstream's tree at the
        # pin, byte for byte. A tree that was edited here is a fork, and
        # a fork is what the vendoring rule forbids.
        problems="$(tree_problems "$up" "$rgx" "$pin")"
        if [ -n "$problems" ]; then
            row_drifted=1
            report red "  FAIL: $rgx: the copy does not match upstream's tree at $pin:"
            printf '%s\n' "$problems" | while IFS= read -r line; do report red "    $line"; done
        fi
        drifted=$((drifted + row_drifted))
    elif [ "$kind" = "patch" ]; then
        verdict="$(patch_verdict "$rgx" "$up" "$pin")"
        case "$verdict" in
            ok) ;;
            no-upstream-file)
                report red "  FAIL: $rgx: upstream has no $up"
                drifted=$((drifted + 1))
                ;;
            no-copy)
                report red "  FAIL: $rgx: the patched file $up is not in the index (the gate reads the index, not the working tree)"
                drifted=$((drifted + 1))
                ;;
            not-in-index)
                report red "  FAIL: $rgx: the patch is not in the index (the gate reads the index, not the working tree)"
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
