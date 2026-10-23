#!/usr/bin/env bash
# smoke-serve.sh -- ask the --serve mode the same question two ways.
#
# The Linux counterpart of scripts/smoke-serve.ps1.
#
# `--serve` reads one JSON argv-array per line on stdin and answers each
# one, framed by begin/end sentinels. The two queries here are the same
# search asked two ways: once plainly, once with `-c`. Both are pointed at
# the same directory, so the two answers have to agree -- a serve-mode
# query that answers a different count than the same search on the command
# line is the bug this catches.
#
# The Windows script names the binary by its absolute path, in the sibling
# rgx checkout. This one keeps that path as the default, because the twin
# has to ask the same binary the same question, and falls back to that
# checkout's own build where the absolute path is not there, and then to
# the repository's own. RGX_BIN overrides all of them.
#
# Usage:
#   bash scripts/smoke-serve.sh <dir>
#
# Exit codes:
#   the serve mode's own -- the .ps1 is the last command in its pipeline,
#   so its exit code is the binary's. 1 when there is no rgx binary to ask,
#   which is the .ps1's exit code for a command that is not there.

set -uo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd)"

rgx_win='C:\Users\danalec\Documents\src\rgx\target\release\rgx.exe'
case "$(uname -s)" in
    MINGW*|MSYS*|CYGWIN*)
        rgx_default="$(cygpath -u "$rgx_win" 2>/dev/null || printf '%s' "$rgx_win")" ;;
    *)
        rgx_default="$rgx_win" ;;
esac

rgx="${RGX_BIN:-}"
if [ -z "$rgx" ]; then
    if [ -x "$rgx_default" ]; then
        rgx="$rgx_default"
    elif [ -x "$(dirname "$repo_root")/rgx/target/release/rgx" ]; then
        # The same sibling checkout, in the layout a unix box has it.
        rgx="$(dirname "$repo_root")/rgx/target/release/rgx"
    elif [ -x "$repo_root/target/release/rgx.exe" ]; then
        rgx="$repo_root/target/release/rgx.exe"
    elif [ -x "$repo_root/target/release/rgx" ]; then
        rgx="$repo_root/target/release/rgx"
    else
        echo "no rgx binary at $rgx_default; build it first: cargo build --release" >&2
        exit 1
    fi
fi

# A JSON string needs its backslashes and its quotes escaped; a path has
# nothing else that JSON cares about.
json_str() {
    printf '%s' "$1" | sed -e 's/\\/\\\\/g' -e 's/"/\\"/g'
}

if [ $# -ge 1 ]; then
    dir_json="\"$(json_str "$1")\""
else
    # No argument at all: the .ps1 hands a null to ConvertTo-Json, and the
    # twin hands the same null to the serve mode.
    dir_json='null'
fi

{
    printf '["needle",%s]\n' "$dir_json"
    printf '["-c","needle",%s]\n' "$dir_json"
} | "$rgx" --serve
