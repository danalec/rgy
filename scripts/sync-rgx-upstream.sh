#!/usr/bin/env bash
# Re-sync the vendored rgx crates with an upstream rgx checkout.
#
# Usage:
#   scripts/sync-rgx-upstream.sh --upstream <path> [--dry-run] [--prune]
#
# What it does:
#   The `rgx-*` and `rar-codec` crates in crates/ are upstream rgx's crates,
#   transported verbatim (README.md: "its engine and adapters, transported
#   verbatim"). This script copies them from an upstream checkout, so the
#   transport is a command with a log rather than a memory.
#
#   * Every file upstream tracks under a vendored crate is copied to the same
#     relative path under crates/.
#   * The third-party licence texts upstream keeps in LICENSES/ are copied
#     to LICENSES/ as well, so a notice cannot go stale because the copy
#     that needed it was not part of the crate. The copy never prunes: a
#     text rgy carries and upstream does not stays. Upstream keeps no
#     LICENSES/ directory today, so this is a rule and not a copy that
#     ran; rgy's own notices live in NOTICE and in the licence files at
#     the root, and those are not this directory's business.
#   * Files rgy has and upstream does not are reported as STALE. `--prune`
#     removes them; without it they are only listed, because a file rgy
#     wrote itself (see KEEP below) is not stale but ours.
#   * The paths in EXCLUDE are not copied at all, and are removed from the
#     tree on every sync. They are upstream's own measurement and fuzzing
#     harnesses, which rgy does not run and does not carry.
#   * The copy is byte-for-byte: no SPDX stripping, no path rewriting, no
#     `alloc::` -> `std::`. Upstream and rgy are both `no_std` + `alloc`
#     now, and the copy-time transforms that used to live in
#     scripts/apply-vendored-edits.ps1 are all obsolete.
#
# What it does not do:
#   It does not merge. A file upstream changed is taken whole; rgy's
#   local edits to a vendored file are not re-applied on top of it. The
#   vendored trees are upstream's trees, and the only file in them that is
#   rgy's own is on the KEEP list.
#
#   It does not touch the root documents. COPYING, NOTICE and the
#   workspace Cargo.toml are rgy's own and are maintained here: they
#   describe the whole workspace, not the vendored part of it.
#
# The upstream path is a read-only input. Nothing outside crates/ is
# touched.

set -uo pipefail

UPSTREAM=""
DRY_RUN=0
PRUNE=0

here="$(cd "$(dirname "$0")" && pwd)"

# The help, before the options are read: the script's own header, the way
# the twin prints it.
. "$here/lib/options.sh"
if rgy_help_requested "$@"; then
    sed -n '2,26p' "$0"
    exit 0
fi

# The options, read by the helper the twins share: the long spelling and
# the PowerShell one are the same option, and an option neither of them
# knows is an error -- the same words, the same exit code. A sync that
# ignores the option the caller asked for copies the trees anyway.
rgy_read_options "dryrun=DRY_RUN prune=PRUNE" "upstream=UPSTREAM" "" "$@"

if [ -z "$UPSTREAM" ]; then
    echo "no --upstream path given" >&2
    echo "  example: scripts/sync-rgx-upstream.sh --upstream ../rgx" >&2
    exit 1
fi

repo_root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$repo_root" || exit 1

UPSTREAM="$(cd "$UPSTREAM" 2>/dev/null && pwd)" || { echo "no such directory: $UPSTREAM" >&2; exit 1; }
if [ ! -d "$UPSTREAM/.git" ]; then
    echo "$UPSTREAM is not a git checkout" >&2
    exit 1
fi

# The vendored crates. Each one is upstream rgx's crate of the same name,
# and the path layout is identical on both sides (crates/<name>/...), so the
# copy is a path-preserving one.
CRATES="rgx-core rar-codec rgx-adapters-archive rgx-adapters-docs \
rgx-adapters-inspect rgx-adapters-llm rgx-adapters-mail rgx-adapters-media \
rgx-adapters-office rgx-adapters-pdf rgx-adapters-tabular rgx-adapters-threed"

# Files inside a vendored tree that are rgy's own and must survive the
# copy. One per line, repo-relative.
#
# tesla_valve.rs is rgy's empirical harness for the Tesla Valve indexing
# idea: upstream rgx has no such example, and it is rgy's argument for the
# pack-store design, not a copy of anything. It lives in the rgy-engine crate,
# where rgy's own code belongs -- a vendored tree carries upstream's
# licence and nothing else.
KEEP=""

# Files upstream has that this tree does not carry. Each entry is a
# repo-relative path prefix, and the copy skips it.
#
# These are rgx's own measurement and fuzzing harnesses: the example
# programs that produce its benchmark tables, and the cargo-fuzz crate
# that fuzzes its parsers. rgy has its own benches --
# crates/name-core/examples and crates/engine/examples -- and its own gate,
# and nothing in rgy runs any of these. They are upstream's tools, in
# upstream's repository, and carrying them here would be carrying a tool
# nobody in this repository can reach.
#
# The tree is therefore upstream's, minus what rgy does not carry. The
# manifest says so on the rows of the crates this touches, and a sync
# removes the paths, so they cannot come back by accident.
EXCLUDE="
crates/rgx-core/examples/
crates/rgx-core/fuzz/
crates/rgx-adapters-archive/examples/
crates/rar-codec/examples/"

# The excluded paths as one pattern, for the filter that takes them out
# of the file list. The prefixes hold no regular-expression characters,
# so they are the pattern as they are.
exclude_pattern=""
for prefix in $EXCLUDE; do
    [ -n "$exclude_pattern" ] && exclude_pattern="$exclude_pattern|"
    exclude_pattern="$exclude_pattern^$prefix"
done

# Is this repo-relative path under one of the excluded prefixes?
is_excluded() {
    local rel="$1" prefix
    for prefix in $EXCLUDE; do
        case "$rel" in
            "$prefix"*) return 0 ;;
        esac
    done
    return 1
}

# Local patches, applied after the copy, one file per patch under
# scripts/rgx-local-patches/<repo-relative-path>.patch.
#
# A patch is a divergence from upstream that rgy has to carry: a bug
# upstream has not fixed yet, or a fix upstream has not taken. The patch
# directory is the place where that divergence is recorded, so the next
# sync re-applies it instead of silently reverting it -- and so a patch
# that no longer applies says so out loud, instead of leaving the file
# half-patched.
PATCH_DIR="scripts/rgx-local-patches"

is_kept() {
    local rel="$1" k
    for k in $KEEP; do
        [ "$rel" = "$k" ] && return 0
    done
    return 1
}

# Apply the local patch for a repo-relative path, if there is one.
# Returns 0 when there is no patch, 1 when the patch does not apply.
apply_local_patch() {
    local rel="$1"
    local patch="$PATCH_DIR/$rel.patch"
    [ -f "$patch" ] || return 0
    # `core.autocrlf=false` on purpose: the copy has to stay what the
    # commit says it is. A checkout that rewrites the line endings is not
    # a change, and a patch whose context is LF then matches nothing.
    if git -c core.autocrlf=false apply --recount --whitespace=nowarn "$patch" 2>/dev/null; then
        echo "  patch  $rel"
        patched=$((patched + 1))
        return 0
    fi
    echo "  FAILED local patch does not apply: $rel" >&2
    echo "         $patch" >&2
    echo "         upstream changed the file; re-derive the patch." >&2
    return 1
}

# The bytes come from the upstream COMMIT, not from the working tree: the
# manifest pins a commit, so the copy has to be that commit's bytes, and a
# dirty upstream working tree would otherwise decide what this repository
# holds. It also means the copy has the canonical line endings -- a
# checkout with core.autocrlf hands out CRLF for content whose blob is LF.

copied=0; unchanged=0; added=0; patched=0; ahead=0
stale_list=""

# The commit the copy is taken from. Every row of the manifest records it,
# and it is the answer to "has upstream moved under this tree since we
# looked?".
upstream_commit="$(git -C "$UPSTREAM" rev-parse HEAD 2>/dev/null)"
if [ -z "$upstream_commit" ]; then
    echo "cannot read the upstream HEAD commit" >&2
    exit 1
fi

echo "syncing vendored rgx crates from $UPSTREAM" >&2

# Every tracked upstream file under the vendored crates, once. The staleness
# check below asks "does upstream have this file", and asking git per file
# would spawn a process per file -- a thousand of them, for a question one
# `ls-files` answers.
upstream_list="$(mktemp)"
patched_paths="$(mktemp)"
upstream_tree_paths="$(mktemp)"
trap 'rm -f "$upstream_list" "$patched_paths" "$upstream_tree_paths"' EXIT
for crate in $CRATES; do
    git -C "$UPSTREAM" ls-files "crates/$crate" >> "$upstream_list"
done
sort -o "$upstream_list" "$upstream_list"

# Every path the pinned commit holds, and the question the manifest asks
# about a patch: does upstream have this path at all? A patch that changes
# a file upstream has is applied on top of the copy; a patch that creates a
# file upstream has no path for is the file's whole content. The two are
# both "a divergence from upstream" and the manifest has to tell them
# apart, or the row says the patch goes on top of a copy that does not
# exist.
#
# The list is the commit's and not the checkout's: the copy is taken from
# the commit, so the question is asked of the commit.
git -C "$UPSTREAM" ls-tree -r --name-only "$upstream_commit" > "$upstream_tree_paths"

# The paths that have a local patch, repository-relative. One list for the
# whole run, because the copy list asks it for every crate: a file with a
# patch is rewritten whatever the hashes say, and this is what says so.
#
# The paths come from the patches and not from the tree: a patch is a
# divergence from upstream, and the file it names is the one that has it.
if [ -d "$PATCH_DIR" ]; then
    find "$PATCH_DIR" -name '*.patch' -type f | while IFS= read -r patch_path; do
        rel="${patch_path#"$PATCH_DIR"/}"
        rel="${rel%.patch}"
        printf '%s\n' "$rel"
    done | sort > "$patched_paths"
else
    : > "$patched_paths"
fi

for crate in $CRATES; do
    src_dir="$UPSTREAM/crates/$crate"
    dst_dir="$repo_root/crates/$crate"
    if [ ! -d "$src_dir" ]; then
        echo "  MISSING upstream crate: crates/$crate" >&2
        exit 1
    fi

    # Upstream's tracked files only. The working tree also holds build
    # junk under crates/rgx-core/fuzz/target, which upstream's .gitignore
    # excludes and which must not travel.
    #
    # The bytes come from the commit, not from the working tree. The
    # manifest pins a commit, so the copy has to be that commit's bytes:
    # a dirty upstream working tree would otherwise decide what this
    # repository holds, and the two would disagree the moment somebody
    # edited a file upstream. `git cat-file` gives the committed content,
    # which is also the canonical line endings -- a checkout with
    # core.autocrlf would hand out CRLF for content whose blob is LF.
    # Which files have to be copied: the ones whose local content is not
    # the upstream blob's.
    #
    # The comparison is by hash, and it is done in bulk: the upstream tree
    # at the pin is one `ls-tree` and the local files are one
    # `hash-object --stdin-paths`. Doing it per file meant three process
    # spawns per file -- three thousand of them for a thousand files, which
    # is where the minutes went.
    upstream_paths="$(mktemp)"
    upstream_pairs="$(mktemp)"
    local_paths="$(mktemp)"
    local_pairs="$(mktemp)"
    to_copy="$(mktemp)"

    # "<blob hash>\t<path>" for every file the pinned upstream tree holds.
    # `ls-tree` prints "<mode> SP <type> SP <hash> TAB <path>", and the path
    # may hold spaces, so the split is on the tab and the hash is the third
    # space-separated field of what comes before it.
    git -C "$UPSTREAM" ls-tree -r "$upstream_commit" -- "crates/$crate" \
        | awk -F'\t' '{ split($1, f, " "); print f[3] "\t" $2 }' \
        > "$upstream_pairs"
    cut -f2 "$upstream_pairs" > "$upstream_paths"

    # The paths this tree does not carry are not paths of this tree: they
    # are not copied, and they are not counted as unchanged.
    #
    # The filter has to reach the pairs as well, and not only the paths:
    # the copy list is built from the pairs, so a pair that survives the
    # filter is a file the copy carries. The excluded paths were then
    # reported as "add" by a dry run, and a real sync wrote them out
    # before removing them at the end -- the tree ended in the right
    # state by way of a wrong report and a file written and deleted.
    if [ -n "$exclude_pattern" ]; then
        grep -v -E "$exclude_pattern" "$upstream_paths" > "$upstream_paths.kept" \
            || true
        mv "$upstream_paths.kept" "$upstream_paths"
        awk -F'\t' '
            FILENAME == ARGV[1] { keep[$0] = 1; next }
            ($2 in keep)
        ' "$upstream_paths" "$upstream_pairs" > "$upstream_pairs.kept" \
            || true
        mv "$upstream_pairs.kept" "$upstream_pairs"
    fi

    # The local hashes, for the paths that exist here. A path that does not
    # exist is an addition, and `hash-object` would fail on it.
    while IFS= read -r rel; do
        [ -f "$repo_root/$rel" ] && printf '%s\n' "$rel"
    done < "$upstream_paths" > "$local_paths"
    git hash-object --no-filters --stdin-paths < "$local_paths" \
        | paste -d'\t' "$local_paths" - > "$local_pairs"

    # The paths to copy: the ones the upstream has and this tree does not,
    # the ones whose content is not the blob's, and the ones that have a
    # local patch.
    #
    # A file with a local patch is in the second group by construction --
    # the patch changed its content -- so it is rewritten and the patch
    # goes on top again. That is true only while the patch is applied to
    # the tree. The moment the tree's copy IS the upstream blob (a patch
    # that was never applied, or one whose effect was reverted), the file
    # leaves the copy list, the copy skips it, and the patch never goes on
    # top: the sync says nothing and the tree keeps upstream's bytes
    # without the fix. A file with a patch is therefore always rewritten.
    #
    # Both directions matter. A path only upstream has is a new file; a
    # path both have with a different hash is a changed one. Asking only
    # "which local files differ" misses every new file the upstream brings,
    # which is how a rename arrives as a deletion and nothing else.
    #
    # The patched paths upstream never had are not in here: the upstream
    # list holds only the paths upstream has, and the block below is what
    # creates those. And a path under an excluded prefix is not here
    # either: the upstream list was filtered before this.
    #
    # The three files are read by name and not by order: the local pairs
    # are "path TAB hash" and the upstream pairs are "hash TAB path", so
    # the awk has to know which is which.
    awk -F'\t' \
        -v patfile="$patched_paths" -v locfile="$local_pairs" -v upfile="$upstream_pairs" '
        FILENAME == patfile { patched[$0] = 1; next }
        FILENAME == locfile { local[$1] = $2; next }
        FILENAME == upfile {
            path = $2; hash = $1
            if (!(path in local)) { print path; next }
            if (local[path] != hash) { print path; next }
            if (path in patched) { print path }
        }
    ' "$patched_paths" "$local_pairs" "$upstream_pairs" > "$to_copy"

    while IFS= read -r rel; do
        [ -n "$rel" ] || continue
        dst="$repo_root/$rel"
        if [ "$DRY_RUN" = 1 ]; then
            # A dry run writes nothing: not the copy, not the patch, and not
            # the file a patch creates. A dry run that moves a file out of
            # the tree and puts it back is a dry run that can be caught out.
            if [ -f "$dst" ]; then
                echo "  update $rel"
                copied=$((copied + 1))
            else
                echo "  add    $rel"
                added=$((added + 1))
            fi
            # The patch is not applied -- nothing is -- but the report has to
            # say what a real run would do with it, or the two reports
            # disagree about the same tree: the dry run said "patched 4"
            # where the real run said "patched 26".
            if [ -f "$PATCH_DIR/$rel.patch" ]; then
                echo "  patch  $rel"
                patched=$((patched + 1))
            fi
            continue
        fi
        # The tree's file is moved aside before the copy. The copy is
        # upstream's bytes and the patch goes on top of it; if the patch
        # does not apply, the file that was here goes back -- a failed patch
        # must not leave the tree holding upstream's bytes with the local
        # fix silently gone.
        existed=0
        saved=""
        if [ -f "$dst" ]; then
            existed=1
            saved="$(mktemp)" || exit 1
            cp "$dst" "$saved" || exit 1
        fi
        if [ "$existed" = 1 ]; then
            git -C "$UPSTREAM" cat-file blob "$upstream_commit:$rel" > "$dst" || exit 1
            echo "  update $rel"
            copied=$((copied + 1))
        else
            mkdir -p "$(dirname "$dst")" || exit 1
            git -C "$UPSTREAM" cat-file blob "$upstream_commit:$rel" > "$dst" || exit 1
            echo "  add    $rel"
            added=$((added + 1))
        fi
        if ! apply_local_patch "$rel"; then
            if [ "$existed" = 1 ]; then
                cp "$saved" "$dst"
            else
                # The copy created it, and the patch that would have made it
                # right did not apply: the file goes away again.
                rm -f "$dst"
            fi
            [ -n "$saved" ] && rm -f "$saved"
            exit 1
        fi
        [ -n "$saved" ] && rm -f "$saved"
    done < "$to_copy"

    # The files rgy has and the pinned upstream does not, that a patch
    # carries.
    #
    # A patch is the source of truth for a diverged file: the sync takes
    # upstream's content and puts the patch on top of it. For a file
    # upstream never had, the patch alone is the content -- the patch
    # creates the file, and the file here is what the patch says.
    #
    # The list comes from the patches and not from the tree, so a file that
    # is missing here is created by the sync and not reported as stale.
    while IFS= read -r patch_path; do
        [ -n "$patch_path" ] || continue
        rel="${patch_path#"$PATCH_DIR"/}"
        rel="${rel%.patch}"
        # Upstream has this file: the copy above already put the patch on
        # top of it.
        grep -Fxq "$rel" "$upstream_paths" && continue
        dst="$repo_root/$rel"
        if [ "$DRY_RUN" = 1 ]; then
            # A dry run writes nothing -- and this block is the one that
            # takes the tree's file away, so it is the one that has to be
            # careful.
            echo "  add    $rel (the patch creates it)"
            patched=$((patched + 1))
            continue
        fi
        # The patch creates the file, so the file has to be gone when the
        # patch runs. It is moved aside first: a patch that does not apply
        # must not take the tree's file with it.
        saved=""
        if [ -f "$dst" ]; then
            saved="$(mktemp)"
            cp "$dst" "$saved" || exit 1
            rm -f "$dst"
        fi
        if ! git -c core.autocrlf=false apply --recount --whitespace=nowarn "$patch_path" 2>/dev/null; then
            echo "  FAILED local patch does not apply: $rel" >&2
            echo "         $patch_path" >&2
            echo "         upstream has no $rel; the patch has to create it." >&2
            [ -n "$saved" ] && cp "$saved" "$dst"
            [ -n "$saved" ] && rm -f "$saved"
            exit 1
        fi
        [ -n "$saved" ] && rm -f "$saved"
        echo "  add    $rel (the patch creates it)"
        patched=$((patched + 1))
    done < <(find "$PATCH_DIR" -path "*/crates/$crate/*" -name '*.patch' -type f | sort)

    # What is left is what already matches the blob.
    unchanged=$((unchanged + $(wc -l < "$upstream_paths") - $(grep -c . "$to_copy")))

    # What rgy has that the pinned upstream does not.
    while IFS= read -r rel; do
        [ -n "$rel" ] || continue
        is_kept "$rel" && continue
        is_excluded "$rel" && continue
        grep -Fxq "$rel" "$upstream_paths" && continue
        # A patch carries it: the file is here because the patch says so,
        # and the sync puts it back. It is not a file rgy has and upstream
        # does not -- it is a divergence the patch records.
        [ -f "$PATCH_DIR/$rel.patch" ] && continue
        stale_list="$stale_list$rel"$'\n'
    done < <(find "$dst_dir" -type f \
        -not -path '*/fuzz/target/*' \
        | sed "s|^$repo_root/||" | sort)

    # What the upstream has that the commit does not: the files it has
    # edited or staged but not committed. The copy is taken from the
    # commit, so those are not here -- and saying so is better than a
    # silent difference between what is here and what is upstream.
    #
    # The wording says "uncommitted", because that is what this list is:
    # it is the upstream working tree against the commit being copied, so
    # every name in it is a change upstream has not committed yet. A
    # committed change upstream would have been copied, not reported.
    #
    # The comparison ignores the carriage return at the end of a line,
    # because the checkout's line endings are not a change. A checkout
    # made by a git configured to write CRLF holds CRLF on disk where the
    # blob has LF, and without this the report said 325 files had
    # uncommitted changes on a checkout that had none -- the answer came
    # from the git that ran the question and not from the checkout. The
    # copy takes the commit's bytes, so the endings on the upstream disk
    # are not what this repository ends up with either.
    while IFS= read -r rel; do
        [ -n "$rel" ] || continue
        echo "  ahead  $rel: uncommitted upstream change; the copy takes the commit, so this is not in the tree yet" >&2
        ahead=$((ahead + 1))
    done < <(git -C "$UPSTREAM" diff --name-only --ignore-cr-at-eol "$upstream_commit" -- "crates/$crate" 2>/dev/null)

    rm -f "$upstream_paths" "$upstream_pairs" "$local_paths" "$local_pairs" "$to_copy"
done

# The excluded paths are not part of this tree. A sync removes them, so
# a file that is not carried cannot come back by accident: what the tree
# holds is what the copy says it holds.
excluded_count=0
for prefix in $EXCLUDE; do
    target="$repo_root/$prefix"
    [ -e "$target" ] || continue
    excluded_count=$((excluded_count + 1))
    if [ "$DRY_RUN" = 1 ]; then
        echo "  excluded $prefix (not carried)"
    else
        rm -rf "$target"
        echo "  excluded $prefix (removed)"
    fi
done

stale_count=0
if [ -n "$stale_list" ]; then
    stale_count="$(printf '%s' "$stale_list" | grep -c . || true)"
fi

# The third-party licence texts. Upstream's NOTICE points at these by name,
# so a text that is missing here is a notice that points nowhere. The copy
# is one way: a text rgy carries and upstream does not stays, because
# rgy's NOTICE names it too.
if [ -d "$UPSTREAM/LICENSES" ]; then
    mkdir -p "$repo_root/LICENSES" || exit 1
    for text in "$UPSTREAM"/LICENSES/*.txt; do
        [ -f "$text" ] || continue
        name="$(basename "$text")"
        if [ -f "$repo_root/LICENSES/$name" ] && cmp -s "$text" "$repo_root/LICENSES/$name"; then
            continue
        fi
        if [ "$DRY_RUN" = 1 ]; then
            echo "  notice $name"
        else
            cp "$text" "$repo_root/LICENSES/$name" || exit 1
            echo "  notice $name"
        fi
    done
fi

if [ "$stale_count" -gt 0 ]; then
    echo ""
    echo "stale: $stale_count file(s) rgy has and upstream does not" >&2
    printf '%s' "$stale_list" | while IFS= read -r rel; do
        [ -n "$rel" ] || continue
        if [ "$PRUNE" = 1 ] && [ "$DRY_RUN" = 0 ]; then
            rm -f "$repo_root/$rel"
            echo "  removed $rel"
        else
            echo "  stale  $rel"
        fi
    done
    if [ "$PRUNE" = 0 ]; then
        echo "  (re-run with --prune to remove them)" >&2
    fi
fi

echo ""
echo "added $added, updated $copied, unchanged $unchanged, stale $stale_count, patched $patched, ahead $ahead, excluded $excluded_count"

# --- the manifest ---------------------------------------------------------
#
# scripts/vendored-sources.tsv is written here, not maintained by hand, so
# it cannot fall behind the tree it describes. The old manifest listed, per
# file, the specification the file was written from -- rows like "RFC 1950"
# -- and the drift check could never verify those: a specification version
# is not a git commit, so every one of them read as drift and the check
# said 81 of 115 rows were broken, which is the same as saying nothing.
#
# The manifest now records the relationship that actually exists: these
# trees are upstream rgx's crates, and here is the commit they were taken
# from. Every pin is a commit, so the check can ask its question and get
# an answer. The per-file specification provenance is upstream's own
# manifest's business -- scripts/vendored-sources.tsv in the rgx checkout
# -- and the third-party notices travel with this repository's NOTICE.
manifest="$repo_root/scripts/vendored-sources.tsv"
if [ "$DRY_RUN" = 0 ]; then
    tmp_manifest="$(mktemp)"
    {
        echo "# Vendored-source manifest. Read by scripts/check-drift.*, which asks"
        echo "# one question: has the upstream changed what we copied since we copied"
        echo "# it?"
        echo "#"
        echo "# Written by scripts/sync-rgx-upstream.sh, not by hand: a manifest that"
        echo "# falls behind the tree it describes is worse than none, because it"
        echo "# reads as \"nothing to do here\"."
        echo "#"
        echo "# The file holds two things: the rows, which are tab separated and"
        echo "# five columns wide, and the excluded paths, which are comment"
        echo "# lines of their own -- \"# exclude: <path>\", one per line. A"
        echo "# comment line is not a row: the parsers skip the comments, and"
        echo "# the rows are the rows."
        echo "#"
        echo "# Columns, tab separated:"
        echo "#"
        echo "#   1  rgx_path    the path in this repository. A tree row names a"
        echo "#                   directory; a patch row names the patch file."
        echo "#   2  upstream    the path in the upstream rgx checkout."
        echo "#   3  pin         the upstream commit the copy was taken from."
        echo "#   4  kind        tree | patch."
        echo "#   5  note        why the row is here."
        echo "#"
        echo "# The trees below are upstream rgx's crates: the copy of them, with"
        echo "# this repository's patches on top. The licence follows upstream's."
        echo "# The per-file"
        echo "# specification provenance of those files is upstream's own manifest"
        echo "# (scripts/vendored-sources.tsv in the rgx checkout), and the"
        echo "# third-party notices are in NOTICE and the licence files at the root."
        echo "#"
        echo "# A patch row is a file of this repository that upstream does not"
        echo "# have, or has differently: a divergence from upstream is a patch"
        echo "# applied on top of the copy -- or, when upstream has no such path"
        echo "# at all, a patch that creates the file -- and the patch is what"
        echo "# says what the divergence is. The note on the row says which of"
        echo "# the two it is. A tree that carries patches is not upstream's"
        echo "# verbatim, and the patch rows below it are the ones that say so."
        echo "#"
        echo "# A row whose note says the copy excludes paths is a tree that is"
        echo "# upstream's minus what rgy does not carry: upstream's benchmark"
        echo "# harnesses and its fuzzing crate, which rgy does not run. The"
        echo "# exclusion is in the sync script, so the paths cannot come back."
        echo ""
        echo "# The upstream commit the copy was taken from: $upstream_commit"
        echo ""
        echo "# The paths the copy does not carry, one per line. The sync removes"
        echo "# them from this repository on every run, so a path that is here is"
        echo "# a divergence from the rule that these trees are upstream's, minus"
        echo "# what rgy does not carry."
        echo "#"
        echo "# The drift check reads this list: a file upstream holds under one of"
        echo "# these paths is not a file the copy is missing, and one of them that"
        echo "# is here is a file the copy should not have. The list is written"
        echo "# here, by the sync, so there is one answer to what is excluded."
        for prefix in $EXCLUDE; do
            echo "# exclude: $prefix"
        done
        echo ""
    } > "$tmp_manifest"

    for crate in $CRATES; do
        note="upstream rgx's crate: the copy of it, with the patches below on top"
        for prefix in $EXCLUDE; do
            case "$prefix" in
                "crates/$crate/"*)
                    note="upstream rgx's crate: the copy of it less the paths the copy excludes, with the patches below on top"
                    ;;
            esac
        done
        printf 'crates/%s\tcrates/%s\t%s\ttree\t%s\n' \
            "$crate" "$crate" "$upstream_commit" "$note" >> "$tmp_manifest"
    done

    if [ -d "$PATCH_DIR" ]; then
        while IFS= read -r patch; do
            rel="${patch#"$PATCH_DIR"/}"
            rel="${rel%.patch}"
            # Two kinds of divergence, and the row says which one this is.
            # A patch that changes a file upstream has is applied on top of
            # the copy. A patch for a path upstream does not have creates
            # the file, and there is no copy for it to go on top of: the
            # patch is the file's whole content. The note said "on top of
            # the copy" for both, and the reader had no way to tell the
            # four creation patches from the twenty-six edits.
            if grep -Fxq "$rel" "$upstream_tree_paths"; then
                note="a divergence from upstream: the patch is applied on top of the copy"
            else
                note="a divergence from upstream: the patch creates the file, upstream has no such path"
            fi
            printf '%s\t%s\t%s\tpatch\t%s\n' \
                "$PATCH_DIR/$rel.patch" "$rel" "$upstream_commit" "$note" \
                >> "$tmp_manifest"
        # The rows are sorted ordinally, and not by the locale's habit: a
        # locale-aware sort puts `sqlite_oracle.rs.patch` before
        # `sqlite.rs.patch`, and the manifest is a file two machines
        # generate and one reader compares. The order has to be the one
        # every machine agrees on.
        done < <(find "$PATCH_DIR" -name '*.patch' -type f | sed "s|^$repo_root/||" | LC_ALL=C sort)
    fi

    mv "$tmp_manifest" "$manifest"
    echo "manifest: scripts/vendored-sources.tsv rewritten (pin $upstream_commit)"
fi

[ "$DRY_RUN" = 1 ] && echo "(dry run: nothing was written)"
exit 0
