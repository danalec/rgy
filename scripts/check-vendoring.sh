#!/usr/bin/env bash
# check-vendoring.sh -- guards the vendoring rules: rgy links nothing from
# upstream, and every vendored file declares its source.
#
# The bash twin of scripts/check-vendoring.ps1. The two are twins, not
# cousins: same six checks, same messages, same order, same exit code. A
# check that says "ok" on one platform and "FAIL" on another is worse than
# no check at all, so the text is identical, not merely equivalent.
#
# rgy copies code out of upstream; it never depends on it. This script fails
# if that ever stops being true, so the rule is checked rather than
# remembered. Six checks:
#
#   1. No upstream reference in any Cargo manifest, Cargo.lock, or
#      .cargo/config.toml. Provenance comments in *other* files are fine
#      and are not flagged; only manifests matter, because only manifests
#      can create a dependency edge.
#   2. No [patch] section in .cargo/config.toml, which could redirect a
#      dependency to a local upstream checkout without a path entry.
#   3. Every vendored source file carries a provenance header naming
#      upstream, so copied code stays traceable to its origin.
#   4. No external crates.io dependencies. Known-remaining ones are
#      listed with what replaces them, so the count is a progress metric
#      and a newly added crate fails the check. The count is split in
#      two: a crate the product links, and a crate kept only as a
#      dev-dependency -- an oracle, the thing an in-house implementation
#      is held to.
#   5. Every dependency on a vendored crate names it by path, and no lock
#      file resolves one from a registry. The vendored crates carry the
#      names upstream publishes on crates.io, at the same version, so a
#      version-only dependency would resolve to the registry copy instead
#      of the tree -- silently, and with cargo having nothing to say about
#      it.
#   6. The vendored manifest (scripts/vendored-sources.tsv) still describes
#      the tree: every path it names is there, every vendored crate and
#      every local patch has a row of its own, the header is the one the
#      sync writes, and every row is pinned to the commit that header
#      names. A manifest that falls behind the tree it describes reads as
#      "nothing to do here", which is the one thing a manifest must never
#      do.
#
# The list of known-remaining dependencies is the same list, with the same
# notes, as the Windows script holds: it is the ledger of what is left to
# remove, and a ledger that exists in two places with two wordings is a
# ledger that will say two different things.
#
# Usage:
#   bash scripts/check-vendoring.sh
#
# Run from anywhere; the script re-roots itself on the repository.
# Exits 1 on any violation, 0 when the rules hold.
set -uo pipefail

script_path=$0
case "$script_path" in
    /*|[A-Za-z]:[/\\]*) ;;
    *) script_path=$PWD/$script_path ;;
esac
# A Windows path handed to this script has backslashes, and the cut below
# is on "/". A path with no "/" in it has no directory to cut, the guard
# falls back to the current directory, and this script ends up scanning
# the directory ABOVE the repository root -- which is not a wrong answer
# but a scan of somebody else's tree, and a slow one. The .ps1 twin
# resolves the same path correctly, so the two would not even be looking
# at the same tree.
script_path=$(printf '%s' "$script_path" | tr '\\' '/')
script_dir=${script_path%/*}
[ "$script_dir" = "$script_path" ] && script_dir=.
here=$(cd "$script_dir" && pwd)
repo_root=$(cd "$here/.." && pwd)

# This script takes no options, and an argument it does not know is an
# error: the twin refuses it with the same words and the same exit code. A
# check that quietly ignores half its command line is a check that ran
# something else.
. "$here/lib/options.sh"
rgy_read_options "" "" "" "$@"

cd "$repo_root" || exit 1

# The .ps1 prints the repository root as a native absolute path, and
# relative paths behind a ".\" on Windows ("./" on Unix). Git Bash and WSL
# are Windows shells in a POSIX costume, so they print the Windows form;
# cygpath and wslpath do the translation.
#
# The prefix is unconditional — a path whose first segment starts with a
# dot keeps it (".\.cargo\config.toml") — because the .ps1 twin spells the
# path the same way, and the two have to agree on the bytes.
if command -v cygpath >/dev/null 2>&1; then
    native_root() { cygpath -w "$repo_root"; }
    rel_prefix='.\'
    rel_sep='\'
elif command -v wslpath >/dev/null 2>&1; then
    case "$repo_root" in
        /mnt/[a-z]/*)
            native_root() { wslpath -w "$repo_root"; }
            rel_prefix='.\'
            rel_sep='\'
            ;;
        *)
            native_root() { printf '%s\n' "$repo_root"; }
            rel_prefix='./'
            rel_sep='/'
            ;;
    esac
else
    native_root() { printf '%s\n' "$repo_root"; }
    rel_prefix='./'
    rel_sep='/'
fi

# rel_path <repo-relative path> — the .ps1's Get-RgyRelativePath form.
rel_path() {
    local p=${1//\//$rel_sep}
    printf '%s%s' "$rel_prefix" "$p"
}

# PowerShell's Write-Host ends a line with the platform newline: CRLF on
# Windows, LF on Unix. The twin ends a line the same way, so the two
# outputs are byte-identical and not merely line-identical.
if [ "$rel_sep" = '\' ]; then
    eol=$'\r\n'
else
    eol=$'\n'
fi

# say <text> -- one line, the way the .ps1 writes one.
say() {
    printf '%s%s' "$1" "$eol"
}

# say_head <text> -- a section header: the .ps1 writes these as
# "`n[1/4] ...", so the newline before the header is a bare LF even on
# Windows, where the line after it ends in CRLF.
say_head() {
    printf '\n%s%s' "$1" "$eol"
}

# The scope the .ps1 keeps: the crates that came from upstream, and nothing
# else. rgy's own crates are not vendored-from-upstream and would
# false-positive. The pattern is the .ps1's, in POSIX form.
scope_re='(^|/)crates/(rar-codec|rgx-core|rgx-adapters-[^/]+)(/|$)'
# A .cargo/config.toml anywhere in the tree.
cargo_re='(^|/)\.cargo/'

# The .ps1 lists the manifests in the order its directory walk visits them:
# one level at a time, alphabetically, the files of a directory before its
# subdirectories. A plain byte sort is not that order ("a/b" and "a-x" sort
# the wrong way round), so the separator is mapped below every printable
# character before the sort.
walk_sort() {
    tr '/' '\001' | LC_ALL=C sort | tr '\001' '/'
}

# find_files <name> -- every file with that name, outside target/ and .git/.
#
# The rule is relative to the root, and that is the whole of it: a
# DIRECTORY named target below the root is build output, and a checkout that
# lives under a directory called target is still the tree. The walk starts
# at the root, so the root's own ancestors never enter the question -- which
# is what the .ps1 twin does too, and what it did NOT do when its filter
# read the absolute path (a checkout under a directory named target hid the
# whole tree from it, and the two twins answered differently about the same
# files).
#
# The directory named .git is left alone for the same reason and for a
# sharper one: it holds the repository's own metadata -- the objects, the
# refs, the index -- and none of that is a file of the project. A checkout's
# .git holds objects, and a developer's holds whatever they left in it: a
# scratch copy of the tree, a probe, a published crate. A gate that reads a
# developer's scratch is a gate that gets turned off, and a gate that reads
# a copy of the tree cannot tell the copy from the tree. So the walk does
# not descend into it, and the rule is still relative to the root: a
# directory called .git BELOW the root is skipped, and the root's own
# ancestors never enter the question.
find_files() {
    find . -type d \( -name target -o -name .git \) -prune -o -type f -name "$1" -print | sed 's|^\./||'
}

manifests=()
while IFS= read -r f; do
    manifests+=("$f")
done < <(find_files Cargo.toml | grep -E "$scope_re" | walk_sort)
while IFS= read -r f; do
    manifests+=("$f")
done < <(find_files Cargo.lock | grep -E "$scope_re" | walk_sort)
while IFS= read -r f; do
    manifests+=("$f")
done < <(find_files config.toml | grep -E "$cargo_re" | walk_sort)

failures=0
fail() {
    say "  FAIL: $1"
    failures=$((failures + 1))
}

say "checking vendoring rules in $(native_root)"

# The check reads the tree with these. Without one of them the scan finds
# nothing, reports "ok" and exits 0: a check that passes without looking is
# worse than no check at all, so a missing tool is an error, not an empty
# result.
for tool in find awk grep sed sort tr wc; do
    if ! command -v "$tool" >/dev/null 2>&1; then
        echo "check-vendoring: required tool not found: $tool" >&2
        exit 1
    fi
done

# --- 1. no upstream reference in any manifest ---------------------------------
# A match here is a dependency edge, a [patch] target, or a workspace
# inherit. All three are forbidden. Comments that merely mention the
# provenance are acceptable in Cargo.toml, so only the non-comment part
# of a line is inspected.
say_head "[1/6] no upstream in manifests"
for f in ${manifests[@]+"${manifests[@]}"}; do
    rel=$(rel_path "$f")
    while IFS=$'\t' read -r line_no text; do
        [ -n "$line_no" ] || continue
        fail "$rel:$line_no references upstream source in manifest code: $text"
    done < <(awk '
        {
            line = $0
            sub(/\r$/, "", line)
            # drop comments: a full-line hash and any trailing hash
            code = line
            sub(/#.*/, "", code)
            if (tolower(code) !~ /vendored from/) next
            text = line
            gsub(/^[[:space:]]+/, "", text)
            gsub(/[[:space:]]+$/, "", text)
            printf "%d\t%s\n", NR, text
        }
    ' "$f")
done
if [ "$failures" -eq 0 ]; then
    say "  ok: ${#manifests[@]} manifests clean"
fi

# --- 2. no [patch] in .cargo/config.toml ----------------------------------
say_head "[2/6] no [patch] redirects"
cfg="$repo_root/.cargo/config.toml"
if [ -f "$cfg" ]; then
    if grep -Eqi '^[[:space:]]*\[patch' "$cfg"; then
        fail ".cargo/config.toml has a [patch] section; it could redirect a dependency to a local checkout"
    else
        say "  ok: no [patch] section"
    fi
else
    say "  ok: no .cargo/config.toml"
fi

# --- 3. vendored trees declare provenance ---------------------------------
# A vendored tree is covered by ONE provenance statement in its root module
# (mod.rs), not by per-file headers: the copies are kept byte-identical to
# upstream so a re-sync is a plain file overwrite, and a header on every file
# would have to be re-applied on each sync. Files that were *modified*
# after copying do carry their own header (see sevenz/graph_mode.rs) and
# are listed as exceptions below.
#
# A crate vendored whole (crates/rar-codec) has no mod.rs -- its root module
# is lib.rs, so that is accepted as the root when mod.rs is absent.
#
# So the invariant checked here is: the tree root names its upstream source.
#
# The list is the one upstream's own provenance manifest names -- the trees
# whose root module carries a tree-level statement. Upstream records them in
# `scripts/vendored-sources.tsv`, one section header per tree ("header:
# <path>"), and the five of them are the 7z adapter tree, the LZ4 adapter
# tree, the rgx-core codecs, the RAR codec and the memmap2 tree. Two of
# them were missing here until 2026-10-30, so the gate was looking at three
# of the five trees the record names.
say_head "[3/6] vendored trees declare provenance"
# The roots are spelled with backslashes, as the .ps1 spells them: the
# failure messages quote them the same way, and a message that says
# crates\rar-codec\src on one platform and crates/rar-codec/src on the
# other is two different messages.
vendored_roots=(
    'crates\rgx-adapters-archive\src\sevenz'
    'crates\rgx-adapters-archive\src\lz4'
    'crates\rgx-core\src\compress'
    'crates\rar-codec\src'
    'crates\rgx-core\src\memmap2'
)
checked=0
for vroot in "${vendored_roots[@]}"; do
    dir="$repo_root/${vroot//\\//}"
    if [ ! -d "$dir" ]; then continue; fi
    root_name=""
    for candidate in mod.rs lib.rs; do
        if [ -f "$dir/$candidate" ]; then
            root_name=$candidate
            break
        fi
    done
    if [ -z "$root_name" ]; then
        fail "$vroot has no mod.rs or lib.rs to carry tree-level provenance"
        continue
    fi
    checked=$((checked + 1))
    # A vendored tree must name its upstream source. The accepted markers
    # are `vendored from` (legacy trees) or `independent reimplementation`
    # (current trees); if neither is present the tree has no provenance to
    # audit.
    #
    # The search is one awk over the first 25 lines, not `head | grep`:
    # in a pipe the exit status depends on who closes first, and a grep
    # that finds its match early leaves head with a SIGPIPE, which under
    # pipefail reads as "no marker found" -- on the machine where the
    # marker is right there.
    if awk 'NR <= 25 { if (tolower($0) ~ /(vendored from|independent reimplementation)/) found = 1 } END { exit(found ? 0 : 1) }' "$dir/$root_name"; then
        n=$(find "$dir" -maxdepth 1 -type f -name '*.rs' | wc -l | tr -d ' ')
        say "  ok: $vroot\\$root_name declares provenance for $n files"
    else
        fail "$vroot\\$root_name does not name its upstream source; add a tree-level provenance header (vendored from / independent reimplementation)"
    fi
done
if [ "$checked" -eq 0 ]; then
    say "  ok: no vendored trees configured yet"
fi

# --- 4. no external crates ------------------------------------------------
# The zero-external-crates rule is the project's headline invariant, and
# checks 1-3 could not see it: they only look for upstream references and
# provenance headers, so any number of crates.io dependencies passed
# unnoticed. This check enumerates them.
#
# Known remaining dependencies are listed rather than simply forbidden, so
# the count is a progress metric and nobody can add a new one by forgetting.
# Each entry names what replaces it.
#
# The list is tab-separated: name, then what replaces it. A tab is the
# separator because the notes are sentences, and splitting a list of
# sentences on whitespace loses the sentences.
known_external=$(printf '%s\t%s\n' \
    'crossterm' 'the terminal UI of bins/search-ui speaks the terminal itself now: the escape sequences, the keys and the mode are declared by hand in bins/search-ui/src/term.rs, the way the clock and the filesystem are. The crate is a dev-dependency only -- the oracle the escape sequences and the key corpus are measured against' \
    'hashbrown' 'the maps of crates/name-core and crates/discovery-windows are the tree'"'"'s own now (crates/name-core/src/hmap.rs, crates/discovery-windows/src/hmap.rs): open addressing over the crate'"'"'s Fx hasher, and the maps that were never maps -- a merge, a count, a grouping -- are the shape their data has. The crate is a dev-dependency only -- the oracle the in-house maps are measured against' \
    'ignore' 'the pack-store builder of rgx-core walks the tree with a walker of its own (crates/rgx-core/src/tv_runtime/walk.rs), behind the tv-runtime feature -- and the default build turns that feature on, through rgy-engine'"'"'s own std feature, so the code that uses it is compiled and linted by every gate. In rgx-core the crate is a dev-dependency only -- the oracle the walker, the globs and the ignore files are measured against; the portable walk of the seam (crates/discovery) has no gitignore semantics yet' \
    'mimalloc' 'the allocator of the three binaries; it measured 5-12% on the parallel walk on Windows' \
    'regex' 'rgx ships its own engine (syntax + Thompson NFA + Pike VM) and that engine is the confirm in crates/name-core/src/content.rs. The product does not link the crate: a pattern the byte engine refuses is a refusal the search says. In rgx-core it is a dev-dependency only -- the oracle the capture groups are measured against (54/54 on the permanent corpus)' \
    'rusqlite' 'the SQLite adapter of rgx-adapters-tabular reads the file format itself; the crate is a dev-dependency only -- the oracle the in-house reader is measured against, over a corpus of real databases')

say_head "[4/6] no unexpected external crates"

# known_desc <name> -- what replaces it, or nothing if the crate is unknown.
# The lookup is case-insensitive, because a PowerShell hashtable is.
known_desc() {
    local want=$1 name desc
    want=$(printf '%s' "$want" | tr '[:upper:]' '[:lower:]')
    while IFS=$'\t' read -r name desc; do
        [ -n "$name" ] || continue
        if [ "$(printf '%s' "$name" | tr '[:upper:]' '[:lower:]')" = "$want" ]; then
            printf '%s' "$desc"
            return 0
        fi
    done <<< "$known_external"
    return 1
}

# A dependency section is any [..dependencies] table, including
# target-specific ones. Everything else is not a dependency list.
#
# The scan is the .ps1's, line for line: a section header is a bracketed
# name that ends the line, and only a dependency line with neither
# `path =` nor `workspace =` is an external crate.
#
# The path is added here rather than handed to awk, because awk reads escape
# sequences in a -v assignment, and a Windows path is full of them: a
# ".\crates\..." handed over that way arrives as ".crates..." with a warning
# for every backslash.
external=""
# The whole workspace, for this check only: the question is whether the
# tree links a crate from outside it, and that is a question about every
# crate in the tree. The scope above is what check 1 needs -- rgy's own
# crates are not vendored from upstream -- and it is the wrong scope here:
# the externals of rgy's own crates would not be seen at all.
all_manifests=()
while IFS= read -r f; do
    all_manifests+=("$f")
done < <(find_files Cargo.toml | walk_sort)

for f in ${all_manifests[@]+"${all_manifests[@]}"}; do
    [ "${f##*/}" = "Cargo.toml" ] || continue
    rel=$(rel_path "$f")
    while IFS=$'\t' read -r line_no name kind; do
        [ -n "$name" ] || continue
        external="$external$rel:$line_no	$name	$kind
"
    done < <(awk '
        BEGIN { kind = "linked" }
        {
            line = $0
            sub(/\r$/, "", line)
            trimmed = line
            gsub(/^[[:space:]]+/, "", trimmed)
            gsub(/[[:space:]]+$/, "", trimmed)
            if (match(trimmed, /^\[+[[:space:]]*[^]]+[[:space:]]*\]+$/)) {
                s = trimmed
                sub(/^\[+/, "", s)
                sub(/\]+[[:space:]]*$/, "", s)
                gsub(/^[[:space:]]+/, "", s)
                gsub(/[[:space:]]+$/, "", s)
                section = tolower(s)
                dep = (section ~ /(^|\.)(dev-|build-)?dependencies$/)
                # A dev-dependency is an oracle: it is not linked into a
                # shipped artifact. Everything else that names a crate is.
                kind = (section ~ /(^|\.)dev-dependencies$/) ? "oracle" : "linked"
                next
            }
            if (!dep) next
            code = trimmed
            sub(/#.*/, "", code)
            gsub(/^[[:space:]]+/, "", code)
            gsub(/[[:space:]]+$/, "", code)
            if (code !~ /^[A-Za-z0-9_-]+[[:space:]]*=/) next
            name = code
            sub(/[[:space:]]*=.*$/, "", name)
            # path = ... is an in-tree crate; workspace = true inherits one.
            if (code ~ /(^|[^A-Za-z0-9_])path[[:space:]]*=/) next
            if (code ~ /(^|[^A-Za-z0-9_])workspace[[:space:]]*=/) next
            printf "%d\t%s\t%s\n", NR, name, kind
        }
    ' "$f")
done

# The count is per name, not per occurrence: a crate listed in ten
# manifests is one crate to remove, and reporting it ten times makes the
# number a worse metric.
seen_names=""
unexpected=""
while IFS=$'\t' read -r where name kind; do
    [ -n "$name" ] || continue
    case "
$seen_names
" in
        *"
$name
"*) continue ;;
    esac
    seen_names="$seen_names$name
"
    if ! known_desc "$name" >/dev/null; then
        unexpected="$unexpected$where	$name
"
    fi
done <<< "$external"

if [ -n "$unexpected" ]; then
    while IFS=$'\t' read -r where name; do
        [ -n "$name" ] || continue
        fail "$where: external dependency '$name' is not in the known-remaining list; either it is in-tree (use path =) or add it with what replaces it"
    done <<< "$unexpected"
else
    say "  ok: no unlisted external crates"
fi

# A crate that is a dev-dependency in every manifest that names it is an
# oracle: it is not linked into a shipped artifact, it is the thing an
# in-house implementation is held to. A crate that is a real dependency
# anywhere is linked, and it is the one the invariant is about. The two
# are counted apart, because the number to drive to zero is the number of
# crates that go into a product -- and a ledger that counts an oracle the
# same as a dependency cannot say that.
linked_names=""
while IFS=$'\t' read -r where name kind; do
    [ -n "$name" ] || continue
    [ "$kind" = "linked" ] || continue
    case "
$linked_names
" in
    *"
$name
"*) continue ;;
    esac
    linked_names="$linked_names$name
"
done <<< "$external"

# Report what is still outstanding, and what is only held back.
remaining_linked=""
remaining_oracle=""
while IFS= read -r name; do
    [ -n "$name" ] || continue
    known_desc "$name" >/dev/null || continue
    case "
$linked_names
" in
    *"
$name
"*) remaining_linked="$remaining_linked$name
" ;;
    *) remaining_oracle="$remaining_oracle$name
" ;;
    esac
done <<< "$seen_names"
if [ -n "$remaining_linked" ]; then
    remaining=$(printf '%s' "$remaining_linked" | grep -c .)
    say "  $remaining linked external dependenc(ies) still to remove:"
    while IFS= read -r name; do
        [ -n "$name" ] || continue
        say "    - $name ($(known_desc "$name"))"
    done <<< "$(printf '%s' "$remaining_linked" | LC_ALL=C sort)"
else
    say "  zero linked external dependencies: the invariant holds"
fi
if [ -n "$remaining_oracle" ]; then
    oracles=$(printf '%s' "$remaining_oracle" | grep -c .)
    say "  $oracles oracle(s): a dev-dependency only -- the crate an in-house implementation is held to:"
    while IFS= read -r name; do
        [ -n "$name" ] || continue
        say "    - $name ($(known_desc "$name"))"
    done <<< "$(printf '%s' "$remaining_oracle" | LC_ALL=C sort)"
fi

# --- 5. the vendored crates come from the tree --------------------------------
# The vendored crates are the upstream's crates and they go out under the
# rgy's own names now -- rgy-vendored-core, the rgy-vendored-adapters-*
# family and rgy-vendored-rar-codec -- carrying this tree's patches inside
# them, at this tree's version (0.1.10). They used to go out under the
# upstream's names, and that was a collision: the upstream publishes the
# same names, and a name and a number is one thing on the registry, so two
# publishers cannot both take it. The upstream's names are the upstream's.
#
# The dependency that names one of them carries a path, and no lock file
# resolves one of them from a registry. A dependency that names one without
# a `path` resolves to the registry's copy instead of the tree's --
# silently, and that copy is the upstream's, without one of the patches that
# live here. The tree would stop being upstream's bytes and the drift check
# would have nothing to compare.
#
# The rule is: every dependency on a vendored name carries a path, and no
# lock file resolves one of them from a registry. The scan covers every
# manifest in the tree, not only the vendored ones: the offending
# declaration would sit in rgy's own crate, not in the vendored tree.
#
# Both names are matched: the key the tree writes (`rgx-core`, the name the
# code says) and the package the registry knows (`rgy-vendored-core`).
say_head "[5/6] the vendored crates come from the tree"

# Every manifest in the tree, and every lock file.
all_manifests=()
while IFS= read -r f; do
    all_manifests+=("$f")
done < <(find_files Cargo.toml | walk_sort)
all_locks=()
while IFS= read -r f; do
    all_locks+=("$f")
done < <(find_files Cargo.lock | walk_sort)

vendored_count=0
vendored_failures=""

# The scan is the .ps1's, rule for rule: a table header closes the table
# before it; a named dependency table ([dependencies.rgx-core]) is followed
# by its own keys, and it is the `path =` among them that counts; a
# dependency written inline inside a dependency table is on one line, and
# the `path =` has to be on that line.
for f in ${all_manifests[@]+"${all_manifests[@]}"}; do
    rel=$(rel_path "$f")
    while IFS=$'\t' read -r line_no name has_path; do
        [ -n "$name" ] || continue
        vendored_count=$((vendored_count + 1))
        if [ "$has_path" != "yes" ]; then
            vendored_failures="$vendored_failures$rel:$line_no '$name' is declared without a path; a version-only dependency resolves to crates.io, not to the tree
"
        fi
    done < <(awk '
        function flush_named() {
            # A named dependency is reported whether it carries a path or
            # not: the count is the number of declarations, not the number
            # of violations, and the two twins must agree on it.
            if (named != "") {
                printf "%d\t%s\t%s\n", named_line, named, (named_has_path ? "yes" : "no")
            }
            named = ""
            named_has_path = 0
        }
        {
            line = $0
            sub(/\r$/, "", line)
            trimmed = line
            gsub(/^[[:space:]]+/, "", trimmed)
            gsub(/[[:space:]]+$/, "", trimmed)
            if (match(trimmed, /^\[+[[:space:]]*[^]]+[[:space:]]*\]+$/)) {
                flush_named()
                s = trimmed
                sub(/^\[+/, "", s)
                sub(/\]+[[:space:]]*$/, "", s)
                gsub(/^[[:space:]]+/, "", s)
                gsub(/[[:space:]]+$/, "", s)
                section = s
                dep = (section ~ /(^|\.)(dev-|build-)?dependencies$/)
                # A named dependency table: [dependencies.rgx-core].
                if (section ~ /(^|\.)(dev-|build-)?dependencies\./) {
                    n = section
                    sub(/^.*dependencies\./, "", n)
                    if (n ~ /^(rgx-core|rgx-adapters-[a-z0-9-]+|rar-codec|rgy-vendored-[a-z0-9-]+)$/) {
                        named = n
                        named_line = NR
                        named_has_path = 0
                    }
                }
                next
            }
            if (named != "") {
                if (trimmed ~ /(^|[^A-Za-z0-9_])path[[:space:]]*=/) named_has_path = 1
                next
            }
            if (!dep) next
            code = trimmed
            sub(/#.*/, "", code)
            gsub(/^[[:space:]]+/, "", code)
            gsub(/[[:space:]]+$/, "", code)
            if (code !~ /^[A-Za-z0-9_-]+[[:space:]]*=/) next
            name = code
            sub(/[[:space:]]*=.*$/, "", name)
            if (name !~ /^(rgx-core|rgx-adapters-[a-z0-9-]+|rar-codec|rgy-vendored-[a-z0-9-]+)$/) next
            has_path = (code ~ /(^|[^A-Za-z0-9_])path[[:space:]]*=/) ? "yes" : "no"
            printf "%d\t%s\t%s\n", NR, name, has_path
        }
        END { flush_named() }
    ' "$f")
done

# A lock file resolves a package from a registry by carrying a `source =`
# line; a path dependency carries none. A vendored crate that has a source
# came from somewhere else.
for f in ${all_locks[@]+"${all_locks[@]}"}; do
    rel=$(rel_path "$f")
    while IFS=$'\t' read -r name src; do
        [ -n "$name" ] || continue
        vendored_failures="$vendored_failures$rel '$name' is resolved from $src, not from the tree
"
    done < <(awk '
        function flush_package() {
            if (inpkg && pkg_name ~ /^(rgx-core|rgx-adapters-[a-z0-9-]+|rar-codec|rgy-vendored-[a-z0-9-]+)$/ && pkg_src != "") {
                printf "%s\t%s\n", pkg_name, pkg_src
            }
            inpkg = 0
            pkg_name = ""
            pkg_src = ""
        }
        /^\[\[package\]\]$/ { flush_package(); inpkg = 1; next }
        inpkg && /^[[:space:]]*name[[:space:]]*=/ {
            t = $0
            sub(/^[^"]*"/, "", t)
            sub(/"[[:space:]]*$/, "", t)
            pkg_name = t
            next
        }
        inpkg && /^[[:space:]]*source[[:space:]]*=/ {
            t = $0
            sub(/^[^"]*"/, "", t)
            sub(/"[[:space:]]*$/, "", t)
            pkg_src = t
            next
        }
        END { flush_package() }
    ' "$f")
done

# The failures are sorted, not printed as they are found: the two twins walk
# the tree in the same order but a reader comparing the two outputs should
# not have to know that.
while IFS= read -r msg; do
    [ -n "$msg" ] || continue
    fail "$msg"
done <<< "$(printf '%s' "$vendored_failures" | LC_ALL=C sort)"

if [ -z "$vendored_failures" ]; then
    say "  ok: $vendored_count vendored dependency declarations, all by path"
    say "  ok: no lock file resolves a vendored crate from a registry"
fi

# --- 6. the vendored manifest matches the tree --------------------------------
# scripts/vendored-sources.tsv is the record of the vendored trees: which
# upstream crate each copy came from, which commit it was taken from, and
# which local patch sits on top of it. The sync writes it, the drift check
# reads it, and nothing on the GitHub side checked that it still describes
# the tree it is about -- so a manifest that fell behind the tree reads as
# "nothing to do here", which is the one thing a manifest must never do.
#
# The questions the tree can answer:
#
#   * does every path the manifest names exist -- the trees and the patches
#     alike?
#   * does every vendored crate in crates/ have a row of its own, and every
#     patch file under scripts/rgx-local-patches/ one?
#   * is the header the one the sync writes, and does the commit it names
#     pin every row?
#
# The commit is checked for its shape -- forty hex characters, the shape of
# a git object id -- and for the rows agreeing with it. It is NOT checked
# against the upstream: this check has no upstream checkout and no way to
# ask, so "the rows agree with the header" is the whole of what it can say.
# The question "is that commit still there?" is the drift check's, and it
# runs where the upstream is.
#   * is a path the manifest excludes really absent? The sync removes those
#     on every run, so one that is here is a divergence.
#
# The paths in the messages are the manifest's own, spelled the way the
# manifest spells them, so the two twins print the same bytes.
say_head "[6/6] the vendored manifest matches the tree"

manifest_failures=""
manifest_fault() {
    manifest_failures="$manifest_failures$1
"
}

manifest="scripts/vendored-sources.tsv"
manifest_trees=""
manifest_patches=""
manifest_tree_count=0
manifest_patch_count=0
header_pin=""

if [ ! -f "$manifest" ]; then
    manifest_fault "scripts/vendored-sources.tsv is missing; the sync writes it"
else
    # The header the sync writes. The first line says what the file is, and
    # the commit line says what the copy was taken from; every row carries
    # that same commit, because the sync takes the whole vendoring from one
    # commit.
    #
    # The header is read BEFORE the rows, and the rows are compared with it
    # where they are read. The first version of this check read the rows
    # into one string, joined with \001, and split them again in a second
    # loop -- and the macOS runner's bash (3.2, the last one Apple shipped)
    # does not split a `read` on that separator at all: every row arrived
    # whole in the first variable and the second one was empty, so all
    # forty rows were reported as pinned to nothing. The gate was green on
    # Ubuntu and on Windows and red on macOS, and the reason was the shell,
    # not the tree.
    #
    # So the comparison happens where the row is read, and nothing is
    # carried from one loop to another through a separator the shell has to
    # be asked to split. The PowerShell twin reads the rows into two arrays
    # and compares the same two values, and this is the same comparison in
    # the same order.
    header_first=$(awk 'NR == 1 { sub(/\r$/, ""); print; exit }' "$manifest")
    header_pin=$(awk '/^# The upstream commit the copy was taken from: / { sub(/\r$/, ""); print $NF; exit }' "$manifest")
    if [ "$header_first" != '# Vendored-source manifest. Read by scripts/check-drift.*, which asks' ]; then
        manifest_fault "scripts/vendored-sources.tsv does not start with the header the sync writes"
    fi
    if [ -z "$header_pin" ]; then
        manifest_fault "scripts/vendored-sources.tsv does not name the commit the copy was taken from"
    elif ! printf '%s' "$header_pin" | grep -Eq '^[0-9a-f]{40}$'; then
        manifest_fault "scripts/vendored-sources.tsv names a commit that is not a commit id: $header_pin"
    fi

    # The rows: tab separated, five columns. A comment line is not a row,
    # and a blank line is not a row either.
    while IFS=$'\t' read -r rgx up pin kind note; do
        [ -n "$rgx" ] || continue
        # A line with fewer than four columns is not a row. The shell's own
        # split leaves the kind empty when the columns are short, and the
        # twin reads the same line the same way -- a malformed row is
        # reported, not silently read as something else.
        if [ -z "$kind" ]; then
            manifest_fault "scripts/vendored-sources.tsv has a row with fewer than four columns"
            continue
        fi
        case "$kind" in
            tree)
                manifest_trees="$manifest_trees$rgx
"
                manifest_tree_count=$((manifest_tree_count + 1))
                ;;
            patch)
                manifest_patches="$manifest_patches$rgx
"
                manifest_patch_count=$((manifest_patch_count + 1))
                ;;
            *)
                manifest_fault "scripts/vendored-sources.tsv has a row of an unknown kind: $kind"
                ;;
        esac
        # Every row is pinned to the commit the header names. The pin is the
        # third column of the row, and the header named the commit before
        # the first row was read -- so the two are compared here, with the
        # two values in hand.
        if [ -n "$header_pin" ] && [ "$pin" != "$header_pin" ]; then
            manifest_fault "scripts/vendored-sources.tsv pins $rgx to $pin, and the header names $header_pin"
        fi
    done < <(awk -F'\t' '
        {
            sub(/\r$/, "")
            if ($0 ~ /^#/ || $0 ~ /^[[:space:]]*$/) next
            print
        }
    ' "$manifest")

    # Every tree the manifest names is in the tree.
    while IFS= read -r rgx; do
        [ -n "$rgx" ] || continue
        if [ ! -d "$repo_root/$rgx" ]; then
            manifest_fault "scripts/vendored-sources.tsv names a tree that is not in the tree: $rgx"
        fi
    done <<< "$manifest_trees"

    # Every vendored crate in the tree has a row of its own. The names are
    # upstream's crates: rgx-core, rar-codec and the rgx-adapters-* family.
    # rgy's own crates are not vendored and are not in here.
    for d in "$repo_root"/crates/*/; do
        [ -d "$d" ] || continue
        base=${d%/}
        base=${base##*/}
        case "$base" in
            rgx-*|rar-codec) ;;
            *) continue ;;
        esac
        rel="crates/$base"
        case "
$manifest_trees
" in
            *"
$rel
"*) ;;
            *) manifest_fault "$rel is a vendored tree and the manifest does not name it" ;;
        esac
    done

    # Every patch the manifest names is in the tree.
    while IFS= read -r rgx; do
        [ -n "$rgx" ] || continue
        if [ ! -f "$repo_root/$rgx" ]; then
            manifest_fault "scripts/vendored-sources.tsv names a patch that is not in the tree: $rgx"
        fi
    done <<< "$manifest_patches"

    # Every patch file in the tree has a row of its own. A patch the
    # manifest does not name is a divergence nobody wrote down, and the
    # sync would apply it without the manifest saying so.
    while IFS= read -r p; do
        [ -n "$p" ] || continue
        rel=${p#"$repo_root"/}
        case "
$manifest_patches
" in
            *"
$rel
"*) ;;
            *) manifest_fault "$rel is a patch and the manifest does not name it" ;;
        esac
    done < <(find "$repo_root/scripts/rgx-local-patches" -type f -name '*.patch' 2>/dev/null | LC_ALL=C sort)

    # The paths the manifest excludes are upstream's files this repository
    # does not carry. The sync removes them on every run, so one that is
    # here is a divergence from the rule the manifest states.
    while IFS= read -r ex; do
        [ -n "$ex" ] || continue
        if [ -e "$repo_root/$ex" ]; then
            manifest_fault "scripts/vendored-sources.tsv excludes $ex, and the tree has it"
        fi
    done < <(awk '/^# exclude: / { sub(/\r$/, ""); print substr($0, 12) }' "$manifest")
fi

# The failures are sorted, not printed as they are found: the two twins walk
# the tree in the same order but a reader comparing the two outputs should
# not have to know that.
while IFS= read -r msg; do
    [ -n "$msg" ] || continue
    fail "$msg"
done <<< "$(printf '%s' "$manifest_failures" | LC_ALL=C sort)"

if [ -z "$manifest_failures" ]; then
    say "  ok: $manifest_tree_count vendored trees and $manifest_patch_count patches, all pinned to $header_pin"
fi

# --- result ----------------------------------------------------------------
say ""
if [ "$failures" -gt 0 ]; then
    say "vendoring check FAILED: $failures violation(s)"
    exit 1
fi
say "vendoring check passed"
exit 0
