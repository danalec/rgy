#!/usr/bin/env bash
# The twins, side by side, and diffed.
#
# WHY THIS EXISTS. The tree's rule is that every .ps1 has a .sh beside it and
# the two are twins: the same text, on the same input, with the same exit
# code. The test for that is to RUN BOTH and diff the result -- not to compare
# exit codes, not to read the code and believe it.
#
# The CI runs each twin on its own runner, which exercises both of them and
# compares neither. One pair is compared: the publish scripts, whose escaped
# release body is asserted to be the same bytes on the two runners (that is
# `scripts/test-publish-release.sh`). The other ten pairs are never compared,
# so a drift between them is invisible until somebody publishes a release with
# it -- and the release is published by one of the twins.
#
# So this script is the comparison, and it runs where both interpreters live:
# bash natively, and pwsh beside it. It is unix-only by design, like
# `release-linux.sh` and `sync-rgx-upstream.sh`: a twin of it would have to
# run the twins, and the two would run each other.
#
# THE INPUT. An option no script knows, so every script must refuse it with
# exit 2 before it touches anything -- and the refusal must be the SAME
# refusal, byte for byte. The scripts that publish are exercised on this path
# too, and that is the point: the refusal is the first thing they do, and a
# script that cannot refuse an option it does not know is a script that ran
# something else.
#
# WHAT IT DOES NOT COVER. The libraries in scripts/lib/ are not standalone
# scripts -- they are sourced by the scripts above -- so they are not in this
# list. And a .ps1 with no .sh beside it is reported, because the rule says
# every .ps1 has a .sh beside it.

set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

# The option neither script knows.
PROBE="--rgy-nonsense"

# Where pwsh lives. The GitHub runners ship it, and the box has it.
PWSH="${PWSH:-pwsh}"

if ! command -v "$PWSH" >/dev/null 2>&1; then
    echo "pwsh is not on the PATH: the twins cannot be compared without it" >&2
    exit 1
fi

echo "the repo:   $ROOT"
echo "the probe:  $PROBE (an option no script knows)"
echo "the twins:  bash and $($PWSH -NoProfile -Command '$PSVersionTable.PSVersion.ToString()')"
echo

# The text a refusal prints, with the ANSI escapes the tools add taken off,
# so a colour cannot be the only difference between the two.
normalize() {
    sed -e 's/\x1b\[[0-9;]*[A-Za-z]//g' \
        -e "s#$ROOT#<repo>#g" \
        -e 's/[[:space:]]*$//'
}

pairs=()
lone_ps1=()

for sh in scripts/*.sh install.sh; do
    [ -f "$sh" ] || continue
    base="${sh%.sh}"
    if [ -f "${base}.ps1" ]; then
        pairs+=("$base")
    fi
done

for ps1 in scripts/*.ps1 install.ps1; do
    [ -f "$ps1" ] || continue
    base="${ps1%.ps1}"
    if [ ! -f "${base}.sh" ]; then
        lone_ps1+=("$base")
    fi
done

fail=0
rows=""

for base in "${pairs[@]}"; do
    sh_out="$(bash "${base}.sh" "$PROBE" 2>&1)"
    sh_rc=$?
    ps_out="$("$PWSH" -NoProfile -File "${base}.ps1" "$PROBE" 2>&1)"
    ps_rc=$?

    a="$(printf '%s' "$sh_out" | normalize)"
    b="$(printf '%s' "$ps_out" | normalize)"

    if [ "$a" = "$b" ] && [ "$sh_rc" = "$ps_rc" ]; then
        verdict="ok"
    else
        verdict="DIVERGE"
        fail=1
    fi

    printf -v row '  %-34s rc %s/%s  %s\n' "${base#scripts/}" "$sh_rc" "$ps_rc" "$verdict"
    rows="${rows}${row}"

    if [ "$verdict" = "DIVERGE" ]; then
        echo "=== ${base}: the twins do not agree"
        echo "--- the .sh says (exit $sh_rc):"
        printf '%s\n' "$a"
        echo "--- the .ps1 says (exit $ps_rc):"
        printf '%s\n' "$b"
        echo
    fi
done

echo "the pairs:"
printf '%s' "$rows"

if [ "${#lone_ps1[@]}" -gt 0 ]; then
    echo
    echo "a .ps1 with no .sh beside it:"
    for base in "${lone_ps1[@]}"; do
        echo "  ${base#scripts/}"
    done
    fail=1
fi

echo
if [ "$fail" -eq 0 ]; then
    echo "the twins agree: ${#pairs[@]} pairs, the same text and the same exit code"
else
    echo "the twins do not agree" >&2
fi

exit "$fail"
