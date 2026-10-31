#!/usr/bin/env bash
# apply-vendored-edits.sh -- apply rgy's own divergences to the vendored rgx trees.
#
# Usage:
#   scripts/apply-vendored-edits.sh [--upstream <path>] [--dry-run]
#
# What this is for
# ----------------
# The vendored rgx crates are upstream rgx's crates, transported verbatim:
# the copy in crates/rgx-core/ is upstream's crates/rgx-core/, byte for byte,
# and scripts/sync-rgx-upstream.sh puts it there. A verbatim copy has no
# room for a local edit -- so where rgy must differ from upstream, the
# difference is a patch, kept under scripts/rgx-local-patches/ and applied
# on top of the copy.
#
# That is the whole content of this directory today, and it is deliberately
# small: each patch is a bug upstream has not fixed, or a fix upstream has
# not taken, and each one is a line of divergence that has to be looked at
# on every sync.
#
# What this script does NOT do
# ----------------------------
# It does not rewrite the copy. The transforms it used to apply are gone,
# and the reasons they were false are worth keeping in the record:
#
#   * `alloc::` -> `std::` -- both projects are `#![no_std]` + `alloc` now,
#     so there is nothing to rewrite.
#   * module path flattening -- upstream's crates/rgx-core/src/ has the
#     same layout as this workspace's crates/rgx-core/src/, so there is no
#     second path to reach a sibling module by.
#   * lifting visibility -- upstream already declares `pub mod nfa;` and
#     the rest.
#   * turning compile-time SIMD gates into runtime dispatch -- upstream
#     dispatches at runtime now, for the same reason rgy did: a binary
#     that ships to arbitrary machines cannot assume AVX2.
#
# A script that applies transforms nobody needs is a script that will
# eventually apply one nobody wants. The copy is a copy.
#
# The script is idempotent: it derives each patched file from upstream's
# current content plus the patch, so running it twice changes nothing, and
# running it after a sync restores a file the sync overwrote.
#
# The PowerShell counterpart is scripts/apply-vendored-edits.ps1, and the
# two are twins: same text, same order, same exit code, on the same input.
#
# Exit codes: 0 all patches applied, 1 at least one failed.

set -uo pipefail

here=$(cd "$(dirname "$0")" && pwd)
repo=$(cd "$here/.." && pwd)

upstream=""
dry_run=0

# usage -- what the two scripts are for, in the same words in both.
usage() {
    printf '%s\n' "apply-vendored-edits [--upstream <path>] [--dry-run]"
    printf '\n'
    printf '%s\n' \
        "Applies rgy's own divergences to the vendored rgx trees: every patch" \
        "under scripts/rgx-local-patches/ goes on top of the copy of upstream's" \
        "file, so the vendored tree carries rgy's fixes and nothing else." \
        "The copy is a copy — the script does not rewrite it." \
        "" \
        "Exits 0 when every patch applies, 1 when one does not."
}

# The help, before the options are read: the script's own usage, the way
# the twin prints it.
. "$here/lib/options.sh"
if rgy_help_requested "$@"; then
    usage
    exit 0
fi

# The options, read by the helper the two twins share: the long spelling
# and the PowerShell one are the same option, and an option neither of
# them knows is an error on both platforms -- not a silent "do the real
# thing", which in this directory means patching the vendored trees.
rgy_read_options "dryrun=dry_run" "upstream=upstream" "" "$@"

if [ -z "$upstream" ]; then
    # Relative to the repository root, and printed as given, so that the
    # PowerShell twin prints the same line.
    upstream="../rgx"
fi
if [ ! -d "$upstream/.git" ]; then
    echo "no upstream checkout at $upstream" >&2
    echo "  pass --upstream <path to an rgx checkout>" >&2
    exit 1
fi
upstream_abs="$(cd "$upstream" && pwd)"

patch_dir="$here/rgx-local-patches"
if [ ! -d "$patch_dir" ]; then
    echo "no patch directory at $patch_dir" >&2
    exit 1
fi

applied=0; failed=0; unchanged=0

echo "applying rgy's local divergences to the vendored rgx crates"
echo "  upstream: $upstream"

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

while IFS= read -r patch_file; do
    [ -n "$patch_file" ] || continue
    rel="${patch_file#"$patch_dir"/}"
    rel="${rel%.patch}"
    upstream_file="$upstream_abs/$rel"
    vendored_file="$repo/$rel"

    if ! git -C "$upstream_abs" cat-file -e "HEAD:$rel" 2>/dev/null; then
        printf '  %-58s FAILED: upstream no longer has this file\n' "$rel"
        failed=$((failed + 1))
        continue
    fi

    # Build the patched file in a scratch tree: upstream's current content
    # with the patch on top. The result is what the vendored file must be,
    # so the script does not care whether the vendored file is currently
    # patched, stale, or missing.
    #
    # The base is the upstream COMMIT's blob, not the file in its working
    # tree: a checkout with core.autocrlf hands out CRLF for content whose
    # blob is LF, and a patch whose context is LF then matches nothing.
    # The commit is also what the sync copies from, so the two agree.
    rm -rf "$work/tree"
    mkdir -p "$work/tree/$(dirname "$rel")"
    git -C "$upstream_abs" cat-file blob "HEAD:$rel" > "$work/tree/$rel"

    # `core.autocrlf=false` on purpose: the scratch tree is outside this
    # repository, so git apply would fall back to the machine's global
    # setting and rewrite the line endings of the file it writes. A
    # checkout is not a change, and the copy has to stay what the commit
    # says it is.
    if ! (cd "$work/tree" && git -c core.autocrlf=false apply --check --recount --whitespace=nowarn "$patch_file" >/dev/null 2>&1); then
        printf '  %-58s FAILED: the patch does not apply to upstream'"'"'s current file\n' "$rel"
        failed=$((failed + 1))
        continue
    fi
    if ! (cd "$work/tree" && git -c core.autocrlf=false apply --recount --whitespace=nowarn "$patch_file" >/dev/null 2>&1); then
        printf '  %-58s FAILED: the patch applied to the check but not to the file\n' "$rel"
        failed=$((failed + 1))
        continue
    fi

    if [ -f "$vendored_file" ] && cmp -s "$work/tree/$rel" "$vendored_file"; then
        printf '  %-58s unchanged\n' "$rel"
        unchanged=$((unchanged + 1))
        continue
    fi

    if [ "$dry_run" = 1 ]; then
        printf '  %-58s would update\n' "$rel"
    else
        mkdir -p "$(dirname "$vendored_file")"
        cp "$work/tree/$rel" "$vendored_file" || { failed=$((failed + 1)); continue; }
        printf '  %-58s updated\n' "$rel"
    fi
    applied=$((applied + 1))
done < <(find "$patch_dir" -name '*.patch' -type f | sort)

echo ""
if [ "$failed" -gt 0 ]; then
    echo "$applied applied, $unchanged unchanged, $failed failed"
    exit 1
fi
echo "$applied applied, $unchanged unchanged, 0 failed"
exit 0
