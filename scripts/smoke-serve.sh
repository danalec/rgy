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
# The binary to ask is named by the caller: `--bin <path>`, or the RGX_BIN
# environment variable, or the sibling rgx checkout, or this repository's
# own build. A path written into the script is a path that is wrong on the
# machine that runs it, and a script that asks a binary that is not there
# proves nothing -- so the twin refuses the same way, and says the same
# thing, when there is no binary to ask.
#
# Usage:
#   bash scripts/smoke-serve.sh [--bin <path>] <dir>
#
# Exit codes:
#   the serve mode's own -- the .ps1 is the last command in its pipeline,
#   so its exit code is the binary's. 1 when there is no rgx binary to ask,
#   which is the .ps1's exit code for a command that is not there.

set -uo pipefail

here=$(cd "$(dirname "$0")" && pwd)
repo_root="$(cd "$here/.." && pwd)"

# The directory is the only thing this script takes, and it is positional.
# An option it does not know is an error, and so is a second directory: the
# twin refuses both with the same words and the same exit code, because a
# script that ignores what it does not understand does the real thing when
# the caller asked for something else.
. "$here/lib/options.sh"
bin=""
rgy_read_options "" "bin=bin" "dir=dir" "$@"

# The binary to ask, in the order the question is asked:
#
#   1. the one the caller named (--bin, or RGX_BIN),
#   2. the sibling rgx checkout, in the layout each platform has it,
#   3. this repository's own build.
#
# The order is the same on both platforms, and so is the refusal: a smoke
# that cannot ask the question says so and fails, instead of exiting 0
# without having smoked anything.
if [ -z "$bin" ] && [ -n "${RGX_BIN:-}" ]; then
    bin="$RGX_BIN"
fi
if [ -n "$bin" ]; then
    if [ ! -x "$bin" ]; then
        echo "no rgx binary at $bin; build it first: cargo build --release" >&2
        exit 1
    fi
else
    bin=""
    for candidate in \
        "$(dirname "$repo_root")/rgx/target/release/rgx" \
        "$(dirname "$repo_root")/rgx/target/release/rgx.exe" \
        "$repo_root/target/release/rgx" \
        "$repo_root/target/release/rgx.exe"; do
        if [ -x "$candidate" ]; then
            bin="$candidate"
            break
        fi
    done
    if [ -z "$bin" ]; then
        echo "no rgx binary: build it first: cargo build --release" >&2
        exit 1
    fi
fi

# A JSON string needs its backslashes and its quotes escaped; a path has
# nothing else that JSON cares about.
json_str() {
    printf '%s' "$1" | sed -e 's/\\/\\\\/g' -e 's/"/\\"/g'
}

# The directory is set only when the caller gave one, and an empty
# directory is a directory: the .ps1 hands a null to ConvertTo-Json when
# there was no argument at all, and the twin hands the same null to the
# serve mode.
if [ "${dir+set}" = "set" ]; then
    dir_json="\"$(json_str "$dir")\""
else
    dir_json='null'
fi

# The request is two JSON lines, and a line ends with a newline: the
# daemon reads JSON lines, so the request is one line and the line ends
# where the next one begins. The bytes here are the bytes the .ps1 twin
# writes -- a newline and not a CRLF, and one request per line with no
# empty line after it -- so the two platforms are asked the same question.
{
    printf '["needle",%s]\n' "$dir_json"
    printf '["-c","needle",%s]\n' "$dir_json"
} | "$bin" --serve
