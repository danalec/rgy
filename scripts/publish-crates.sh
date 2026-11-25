#!/usr/bin/env bash
# publish-crates.sh -- publish this repository's crates to crates.io, in an
# order that keeps every published crate installable.
#
# The bash twin of scripts/publish-crates.ps1. The two are twins, not
# cousins: same output, same order, same exit code. A publish that behaves
# differently on the machine that runs it is a publish nobody can repeat.
#
# Why the order matters
# ---------------------
# crates.io resolves a dependency when a crate is INSTALLED, not when it is
# published. So a crate that depends on another one cannot be published
# before the dependency is on the registry: publishing it first ships a
# crate that nobody can install, and nothing in the tree says so.
#
# rgy-name-core depends on rgy-par, and rgy-par was not on the registry at
# all. The order is computed from the manifests themselves
# (scripts/publish-plan.py), so it cannot be forgotten.
#
# The index is built asynchronously, so a crate that has just been published
# is not a candidate for a dependency for a moment. The script waits for
# the registry to carry what it has published before it publishes the next
# crate.
#
# Usage:
#   bash scripts/publish-crates.sh --dry-run   # the plan, nothing published
#   bash scripts/publish-crates.sh             # publish what the plan says
#
# A real run needs CARGO_REGISTRY_TOKEN in the environment. The vendored
# rgx trees go out under this repository's own names, patches and all.
#
# Run from anywhere; the script re-roots itself on the repository.
set -uo pipefail

script_path=$0
case "$script_path" in
    /*|[A-Za-z]:[/\\]*) ;;
    *) script_path=$PWD/$script_path ;;
esac
script_path=$(printf '%s' "$script_path" | tr '\\' '/')
script_dir=${script_path%/*}
[ "$script_dir" = "$script_path" ] && script_dir=.
here=$(cd "$script_dir" && pwd)
repo_root=$(cd "$here/.." && pwd)
cd "$repo_root" || exit 1

# The line ending the .ps1 twin writes: PowerShell's Write-Host ends a line
# with the platform newline -- CRLF on Windows, LF on Unix -- and the twin
# ends a line the same way, so the two outputs are byte-identical and not
# merely line-identical.
if command -v cygpath >/dev/null 2>&1 || command -v wslpath >/dev/null 2>&1; then
    eol=$'\r\n'
else
    eol=$'\n'
fi

# say <text> -- one line, the way the .ps1 writes one.
say() {
    printf '%s%s' "$1" "$eol"
}

dry_run=0

# usage -- what the two scripts are for, in the same words in both.
usage() {
    printf '%s\n' "publish-crates [--dry-run]"
    printf '\n'
    printf '%s\n' \
        "Publishes this repository's crates to crates.io in an order that keeps" \
        "every published crate installable: a crate goes out after every crate" \
        "it depends on, because crates.io resolves a dependency when a crate is" \
        "INSTALLED and not when it is published. The order is computed from the" \
        "manifests themselves (scripts/publish-plan.py), so it cannot be" \
        "forgotten." \
        "" \
        "A real run needs CARGO_REGISTRY_TOKEN in the environment. The vendored" \
        "rgx trees go out under this repository's own names, patches and all." \
        "" \
        "Exits 0 when every crate the plan names is published, 1 when the run" \
        "stops before that."
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
# thing", which in this directory means publishing to crates.io.
rgy_read_options "dryrun=dry_run" "" "" "$@"

# The plan is computed by a helper both twins call, so the two print the
# same plan and not two plans that look alike.
py=${RGY_PYTHON:-}
if [ -z "$py" ]; then
    for candidate in python3 python; do
        if command -v "$candidate" >/dev/null 2>&1; then
            py=$candidate
            break
        fi
    done
fi
[ -n "$py" ] || { echo "publish-crates: no python found; set RGY_PYTHON" >&2; exit 1; }

plan=$("$py" scripts/publish-plan.py)
plan_code=$?
say "crates.io publish plan for rgy"
if [ "$plan_code" -ne 0 ]; then
    # The plan failed: the reason is on stderr, and the plan itself is the
    # evidence. Neither is published.
    printf '%s\n' "$plan" >&2
    echo "publish-crates: the plan failed; nothing was published" >&2
    exit "$plan_code"
fi
printf '%s\n' "$plan" | while IFS= read -r line; do say "$line"; done

if [ "$dry_run" -eq 1 ]; then
    say "dry run: nothing was published"
    exit 0
fi

if [ -z "${CARGO_REGISTRY_TOKEN:-}" ]; then
    echo "publish-crates: CARGO_REGISTRY_TOKEN is not set; nothing was published" >&2
    exit 1
fi

published=0
# The plan's own order: a crate is published after every crate it depends
# on, and the lines come out in that order.
#
# The loop reads from a process substitution and not from a pipe: a pipe puts
# the loop in a subshell, and an `exit 1` inside a subshell leaves the
# script running -- so a publish that failed would be followed by the next
# crate, and the script would end with a success it did not have.
while IFS= read -r line; do
    case "$line" in
        "  publish "*) ;;
        *) continue ;;
    esac
    name=$(printf '%s' "$line" | awk '{print $2}')
    version=$(printf '%s' "$line" | awk '{print $3}')
    say "publishing $name $version"
    if ! cargo publish -p "$name" --locked; then
        echo "publish-crates: $name $version was not published; nothing after it was" >&2
        exit 1
    fi
    # The registry's index lags the publish: the next crate depends on this
    # one, and a dependency the registry has not listed yet is refused.
    if ! "$py" scripts/publish-plan.py --wait "$name:$version"; then
        echo "publish-crates: $name $version did not reach the registry; stopped" >&2
        exit 1
    fi
    say "published $name $version"
    published=$((published + 1))
done < <(printf '%s\n' "$plan")

say "published $published crate(s)"
