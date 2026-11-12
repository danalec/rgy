#!/usr/bin/env bash
# rgy-name-protocol.sh -- the bash twin of scripts/rgy-name-protocol.ps1.
#
# The paper's protocol (sec:eval) applied to the name tier: paired samples,
# arm order rotated within each round, minimum of seven as estimator, a
# count gate in front of every timing line, and a verdict that can refuse
# (a +/-3% floor is a tie; a winner is named only when every sample of one
# arm lies below every sample of the other; overlapping distributions are
# reported as such).
#
# The timer is the shell's wall clock around the whole invocation -- setup,
# search and teardown -- with query output discarded, and the count gate
# runs in a separate untimed pass, so no stage timer adjudicates anything
# (sec:mmap is why). The process floor is measured here, in this session,
# never carried over.
#
# What it does not do:
#   It does not decide anything the .ps1 does not decide. Same corpus, same
#   count gate, same estimator, same verdicts, same markdown, same exit
#   codes. What differs is what has to differ: the paths are the shell's
#   own, the timings are this run's, and the host line names the host the
#   shell runs on.
#
# Usage:
#   bash scripts/rgy-name-protocol.sh --scales 200000,5000000 \
#        --rounds 7 --threads 16 --out benchmark/rgy-name-transcript.md
#
# The .ps1's parameter names are accepted as they stand (case-insensitively,
# PowerShell style: -Scales, -Rounds, -Threads, -Out, -Repo, -Queries), so
# the invocation it documents transfers. An option neither script knows is
# an error on both platforms, with the same words and the same exit code.
#
# --queries is repeatable and takes one query per occurrence, which is the
# -Queries array of the .ps1 in -Command mode. In -File mode pwsh hands
# that array over as one literal string ("report,file" is one query there),
# so the twin does not split a value on commas.
#
# RGY_NAME_SCALES overrides the scale list, as it overrides -Scales:
# environment variables survive the argument marshalling of nested shells,
# which has been observed to deliver a comma-joined list as one space-joined
# token.
#
# Exit codes:
#   0  the transcript was written
#   1  a terminating error, the .ps1's `throw` paths: an unparsable scale,
#      a missing fixture, a count gate that got no count

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

REPO=""
SCALES='200000,5000000'
ROUNDS=7
THREADS=16
OUT='benchmark/rgy-name-transcript.md'
query_args=()

# The options, read by the helper the two twins share: the long spelling
# and the PowerShell one are the same option, and an option neither of
# them knows is an error on both platforms -- the same words, the same
# exit code. The helper takes the value with the colon (-Scales:200000)
# or in the next word, and a name that may be given more than once
# collects every occurrence.
. "$here/lib/options.sh"
rgy_read_options "" "repo=REPO scales=SCALES rounds=ROUNDS threads=THREADS out=OUT queries=query_args[]" "" "$@"

# [int]'abc' throws in PowerShell; a scale that is not an integer aborts the
# run here too, rather than becoming a corpus of no files.
to_int() {
    local t="$1" sign='' digits
    case "$t" in
        -*) sign='-'; t="${t#-}" ;;
        +*) t="${t#+}" ;;
    esac
    case "$t" in
        ''|*[!0-9]*) return 1 ;;
    esac
    while [ "${#t}" -gt 1 ] && [ "${t:0:1}" = '0' ]; do t="${t:1}"; done
    [ "$sign" = '-' ] && [ "$t" = '0' ] && sign=''
    printf '%s%s' "$sign" "$t"
}

# [int] binds before the body runs: a round count or a thread count that is
# not an integer is an argument error, and it is one before the scale list
# is even looked at. The value is reported as it was written, and not as
# the empty string the failed substitution would leave behind.
if ! rounds_int="$(to_int "$ROUNDS")"; then
    echo "not an integer round count: '$ROUNDS'" >&2
    exit 1
fi
ROUNDS="$rounds_int"
if ! threads_int="$(to_int "$THREADS")"; then
    echo "not an integer thread count: '$THREADS'" >&2
    exit 1
fi
THREADS="$threads_int"

# RGY_NAME_SCALES overrides the parameter: environment variables survive
# the argument marshalling of nested shells, which has been observed to
# deliver a comma-joined list as one space-joined token.
if [ -n "${RGY_NAME_SCALES:-}" ]; then
    SCALES="$RGY_NAME_SCALES"
fi

# The parsed list goes in its own variable: the .ps1 keeps it apart from the
# [string]-typed parameter so the array is not coerced back into one
# space-joined string, and the loop would then see one corpus named
# "200000 5000000". Split on commas and whitespace alike: an outer shell
# marshals a `-Scales 200000,5000000` argument as an array and the native
# command line may deliver it joined by spaces instead of commas.
scale_list=()
scale_tokens="$(printf '%s' "$SCALES" | tr ',' '\n' | tr -s '[:space:]' '\n')"
while IFS= read -r tok; do
    [ -n "$tok" ] || continue
    scale_list+=("$(to_int "$tok")") || { echo "not an integer scale: '$tok'" >&2; exit 1; }
done <<EOF
$scale_tokens
EOF
if [ ${#scale_list[@]} -eq 0 ]; then
    echo "no scales parsed from '$SCALES'" >&2
    exit 1
fi

ROUNDS="$(to_int "$ROUNDS")" || { echo "not an integer round count: '$ROUNDS'" >&2; exit 1; }
THREADS="$(to_int "$THREADS")" || { echo "not an integer thread count: '$THREADS'" >&2; exit 1; }

# The queries the count gate runs. The .ps1 defaults to the same eight;
# an explicit list replaces them, as an explicit parameter does there.
query_list=('report' 'file' "'report_00000.pdf" '^src' 'dat$' 'mod' 'rep' 'f1')
if [ ${#query_args[@]} -gt 0 ]; then
    query_list=("${query_args[@]}")
fi

join_by() {
    local sep="$1" out='' first=1 item
    shift
    for item in "$@"; do
        if [ "$first" -eq 1 ]; then out="$item"; first=0; else out="$out$sep$item"; fi
    done
    printf '%s' "$out"
}

echo "name-tier protocol: scales=$(join_by ', ' "${scale_list[@]}") rounds=$ROUNDS threads=$THREADS"

repo_root="$(cd "$(dirname "$0")/.." && pwd)"
repo="$repo_root"
[ -n "$REPO" ] && repo="$REPO"

# The platform, for the paths and the host line.
uname_s="$(uname -s)"
case "$uname_s" in
    MINGW*|MSYS*|CYGWIN*) on_windows=1 ;;
    *) on_windows=0 ;;
esac

# The binary. The .ps1 names target\release\examples\name_proto.exe; the
# extension is the only difference between the two platforms.
examples="$repo/target/release/examples"
bin=''
if [ -x "$examples/name_proto.exe" ]; then
    bin="$examples/name_proto.exe"
elif [ -x "$examples/name_proto" ]; then
    bin="$examples/name_proto"
fi
if [ -z "$bin" ]; then
    cargo build --release --manifest-path "$repo/Cargo.toml" -p rgy-engine --example name_proto
    if [ -x "$examples/name_proto.exe" ]; then
        bin="$examples/name_proto.exe"
    elif [ -x "$examples/name_proto" ]; then
        bin="$examples/name_proto"
    fi
    [ -n "$bin" ] || { echo "no name_proto binary at $examples after cargo build" >&2; exit 1; }
fi

# The registry, on Windows: the same values Win32_CimInstance reports, and
# the .ps1 reads them from there.
reg_get() {
    reg.exe query "$1" //v "$2" 2>/dev/null | tr -d '\r' \
        | awk -v want="$2" '$1 == want { sub(/^.*REG_[A-Z_]+[ \t]+/, ""); sub(/[ \t]+$/, ""); print; exit }'
}

host_name=''
cpu_name=''
os_caption=''
os_build=''
if [ "$on_windows" -eq 1 ]; then
    # COMPUTERNAME is the NetBIOS name, which is upper case; the shell's
    # hostname keeps the case the machine registered.
    host_name="$(hostname | tr '[:lower:]' '[:upper:]')"
    cpu_name="$(reg_get 'HKLM\HARDWARE\DESCRIPTION\System\CentralProcessor\0' 'ProcessorNameString')"
    os_build="$(reg_get 'HKLM\SOFTWARE\Microsoft\Windows NT\CurrentVersion' 'CurrentBuildNumber')"
    product="$(reg_get 'HKLM\SOFTWARE\Microsoft\Windows NT\CurrentVersion' 'ProductName')"
    # The registry's ProductName is stale on some builds ("Windows 10 Pro N"
    # on a Windows 11 machine), so the version comes from the build number
    # and the edition from the product name.
    os_version=10
    [ "${os_build:-0}" -ge 22000 ] 2>/dev/null && os_version=11
    os_edition="$(printf '%s' "$product" | sed -e 's/^Windows [0-9][0-9]* //')"
    os_caption="Microsoft Windows $os_version $os_edition"
else
    host_name="$(hostname)"
    cpu_name="$(sed -n 's/^model name[[:space:]]*:[[:space:]]*//p' /proc/cpuinfo 2>/dev/null | head -n 1 | sed -e 's/[[:space:]]*$//')"
    os_caption="$( . /etc/os-release 2>/dev/null; printf '%s' "${PRETTY_NAME:-Linux}" )"
    os_build="$(uname -r)"
fi

# The tree and the date. Get-Date's 'yyyy-MM-dd HH:mm zzz' is the date, the
# time and the offset with a colon; BSD date has no %:z, so the offset is
# spelled out. Git's own complaint about a tree that is not there belongs on
# stderr, as it is in the .ps1.
tree="$(git -C "$repo" rev-parse HEAD | tr -d '\r\n')"
date_offset="$(date '+%z')"
date_stamp="$(date '+%Y-%m-%d %H:%M') ${date_offset:0:3}:${date_offset:3}"

# The fixture and the index live under the temporary directory. The .ps1
# uses $env:TEMP; the shell's equivalent is TMPDIR, and on Windows the
# temporary directory is TEMP.
case "$uname_s" in
    MINGW*|MSYS*|CYGWIN*) tmp_root="${TEMP:-${TMP:-/tmp}}" ;;
    *) tmp_root="${TMPDIR:-/tmp}" ;;
esac
if [ "$on_windows" -eq 1 ] && command -v cygpath >/dev/null 2>&1; then
    case "$tmp_root" in
        [A-Za-z]:*|*\\*) tmp_root="$(cygpath -u "$tmp_root")" ;;
    esac
    # The .ps1 prints Windows paths; the twin prints the same strings, so
    # the transcript and the messages are the same text.
    tmp_display="$(cygpath -w "$tmp_root")"
    repo_display="$(cygpath -w "$repo")"
else
    tmp_display="$tmp_root"
    repo_display="$repo"
fi

# Join-Path, as the .ps1 spells it: the child is taken as relative to the
# repository, so a rooted one loses its root and lands under it.
out_rel="${OUT#/}"
out_path="$repo/$out_rel"
if [ "$on_windows" -eq 1 ] && command -v cygpath >/dev/null 2>&1; then
    out_display="$repo_display\\$(printf '%s' "$out_rel" | tr '/' '\\')"
else
    out_display="$repo/$out_rel"
fi

# The clock. EPOCHREALTIME is bash's own microsecond clock, and reading it
# costs no process: a fork between the two readings would land inside the
# measurement and add the same millisecond to every sample.
NOW_US=0
now_us() {
    local t s f
    if [ -n "${EPOCHREALTIME:-}" ]; then
        t="$EPOCHREALTIME"
    else
        t="$(date +%s.%N)"
        case "$t" in
            *N*) t="$(perl -MTime::HiRes=time -e 'printf "%.6f", time' 2>/dev/null || printf '%s' "$t")" ;;
        esac
    fi
    s="${t%%.*}"
    f="${t#*.}"
    f="${f//[!0-9]/}"
    while [ ${#f} -lt 6 ]; do f="${f}0"; done
    f="${f:0:6}"
    NOW_US=$(( s * 1000000 + 10#$f ))
}

# The elapsed milliseconds of one invocation, in INVOKE_MS. The result is
# left in a variable rather than printed, so the caller does not fork
# either.
INVOKE_MS=0
invoke_ms() {
    # Wall clock around the whole process, query output discarded.
    local start end
    now_us
    start="$NOW_US"
    "$bin" "$@" >/dev/null 2>&1
    now_us
    end="$NOW_US"
    INVOKE_MS=$(( end - start ))
}

invoke_count() {
    # Fail loudly: a missing index or a panic must never read as zero hits,
    # or a 0 == 0 accident passes the gate.
    local out
    out="$("$bin" "$@" 2>/dev/null)"
    out="${out#"${out%%[![:space:]]*}"}"
    out="${out%"${out##*[![:space:]]}"}"
    case "$out" in
        ''|*[!0-9]*)
            echo "name_proto $* produced no count (got: '$out')" >&2
            exit 1 ;;
    esac
    printf '%s' "$out"
}

min_of() {
    local m="$1" v
    shift
    for v in "$@"; do [ "$v" -lt "$m" ] && m="$v"; done
    printf '%s' "$m"
}

max_of() {
    local m="$1" v
    shift
    for v in "$@"; do [ "$v" -gt "$m" ] && m="$v"; done
    printf '%s' "$m"
}

# [math]::Round rounds to the nearest, ties to even. The arithmetic is done
# on integers -- microseconds, tenths of a millisecond -- so the rounding
# is exact and not a floating point near-miss.
round_half_even() {
    local num="$1" den="$2" q r
    if [ "$num" -lt 0 ]; then num=$(( -num )); q=-1; else q=1; fi
    local i=$(( num / den )) r=$(( num % den ))
    if [ $(( r * 2 )) -gt "$den" ] || { [ $(( r * 2 )) -eq "$den" ] && [ $(( i % 2 )) -ne 0 ]; }; then
        i=$(( i + 1 ))
    fi
    printf '%s' "$(( q * i ))"
}

# The .NET rendering of a double that is a whole number of tenths or
# hundredths of a millisecond: no trailing zeros, no trailing point.
fmt_scaled() {
    local n="$1" d="$2" neg='' int frac
    if [ "$n" -lt 0 ]; then neg='-'; n=$(( -n )); fi
    case "$d" in
        0) printf '%s%d' "$neg" "$n" ;;
        1)
            int=$(( n / 10 )); frac=$(( n % 10 ))
            if [ "$frac" -eq 0 ]; then printf '%s%d' "$neg" "$int"
            else printf '%s%d.%d' "$neg" "$int" "$frac"; fi ;;
        2)
            int=$(( n / 100 )); frac=$(( n % 100 ))
            if [ "$frac" -eq 0 ]; then printf '%s%d' "$neg" "$int"
            elif [ $(( frac % 10 )) -eq 0 ]; then printf '%s%d.%d' "$neg" "$int" $(( frac / 10 ))
            else printf '%s%d.%02d' "$neg" "$int" "$frac"; fi ;;
        *) printf '%s%d' "$neg" "$n" ;;
    esac
}

# Round(walk / index, 1), in tenths.
ratio_tenths() {
    local w="$1" i="$2"
    if [ "$i" -eq 0 ]; then printf 'Infinity'; return; fi
    round_half_even $(( w * 10 )) "$i"
}

# Ceiling(T_build / (walk - index)), the break-even ratio. T_build is the
# rounded build minimum (tenths of a millisecond), the difference is raw.
qstar_of() {
    local build="$1" w="$2" i="$3" diff num
    diff=$(( w - i ))
    if [ "$diff" -eq 0 ]; then printf 'Infinity'; return; fi
    num=$(( build * 100 ))
    if [ "$diff" -gt 0 ]; then
        printf '%s' $(( (num + diff - 1) / diff ))
    else
        printf '%s' $(( -(( num ) / ( -diff )) ))
    fi
}

# The paper's gate: within +/-3% a tie; outside it a winner is named only
# if every sample of one arm lies below every sample of the other;
# overlapping distributions are reported as such rather than as a ratio.
get_verdict() {
    local amin amax bmin bmax
    amin="$(min_of "${WALK[@]}")"
    amax="$(max_of "${WALK[@]}")"
    bmin="$(min_of "${IDX[@]}")"
    bmax="$(max_of "${IDX[@]}")"
    if [ "$bmin" -gt 0 ] \
        && [ $(( amin * 100 )) -gt $(( 97 * bmin )) ] \
        && [ $(( amin * 100 )) -lt $(( 103 * bmin )) ]; then
        printf 'tie'
        return
    fi
    if [ "$amin" -gt "$bmax" ]; then printf 'index (separated)'; return; fi
    if [ "$bmin" -gt "$amax" ]; then printf 'walk (separated)'; return; fi
    printf 'overlap, not a result'
}

# The samples of one arm, rounded the way the transcript prints them.
rounded_samples() {
    local d="$1" out='' first=1 v
    shift
    for v in "$@"; do
        if [ "$first" -eq 1 ]; then first=0; else out="$out, "; fi
        if [ "$d" -eq 1 ]; then
            out="$out$(fmt_scaled "$(round_half_even "$v" 100)" 1)"
        else
            out="$out$(fmt_scaled "$(round_half_even "$v" 10)" 2)"
        fi
    done
    printf '%s' "$out"
}

# 1..$Rounds, the way PowerShell's range operator spells it: counting down
# when the end is below the start.
round_seq=()
if [ "$ROUNDS" -ge 1 ]; then
    for ((r = 1; r <= ROUNDS; r++)); do round_seq+=("$r"); done
else
    for ((r = 1; r >= ROUNDS; r--)); do round_seq+=("$r"); done
fi

md=()
md+=("# Name-tier protocol transcript (rgy name_proto)")
md+=("")
md+=("Host: $host_name --- $cpu_name --- $os_caption build $os_build --- 64 GB DDR5-6000.")
md+=("Tree: rgy $tree. Date: $date_stamp. Threads: walk arm scans with $THREADS;")
md+=("the index arm's scan parallelism is its own runtime default (the walk")
md+=("arm being wider is the conservative direction: it understates the")
md+=("saving). Timer: shell wall clock around the whole process, query output")
md+=("discarded. Estimator: minimum of $ROUNDS paired samples per arm, arm")
md+=("order rotated within each paired round. Count gate: a separate untimed")
md+=("pass compares both arms' match counts; a disagreeing line is")
md+=("suppressed, not timed. Warm cache: the untimed pass and the first")
md+=("round populate it. Floor: measured below, in this session.")
md+=("")

for n in "${scale_list[@]}"; do
    root="$tmp_root/rgy_name_proto_$n"
    index="$tmp_root/rgy_name_proto_$n.idx"
    if [ "$on_windows" -eq 1 ] && command -v cygpath >/dev/null 2>&1; then
        root_arg="$(cygpath -w "$root")"
        index_arg="$(cygpath -w "$index")"
    else
        root_arg="$root"
        index_arg="$index"
    fi

    if [ ! -e "$root" ]; then
        "$bin" materialize "$root_arg" "$n" "$THREADS" 2>/dev/null
    fi
    # Name-level check, no recursive walk: the corpus is deterministic (the
    # generator writes exactly $n files over its directories), so the count
    # is the parameter and the arms' agreeing counts are the real proof the
    # tree is what the index saw.
    if [ ! -d "$root/dir_0000" ]; then
        echo "fixture $root_arg missing or empty after materialize" >&2
        exit 1
    fi
    entries="$n"

    # Build arm first: it creates the index the count gate's q-index arm
    # loads. T_build of the break-even ratio.
    build_samples=()
    for r in "${round_seq[@]}"; do
        invoke_ms build "$root_arg" "$index_arg" "$THREADS"
        build_samples+=("$INVOKE_MS")
    done
    build_us="$(min_of "${build_samples[@]}")"
    build_tenths="$(round_half_even "$build_us" 100)"
    build_min="$(fmt_scaled "$build_tenths" 1)"

    # Floor, this session.
    floor_samples=()
    for i in 1 2 3 4 5 6 7; do
        invoke_ms floor
        floor_samples+=("$INVOKE_MS")
    done
    floor_us="$(min_of "${floor_samples[@]}")"
    floor_min="$(fmt_scaled "$(round_half_even "$floor_us" 10)" 2)"

    # Count gate, untimed: keep only lines whose counts agree exactly.
    kept=()
    suppressed=()
    for q in "${query_list[@]}"; do
        ci="$(invoke_count q-index "$root_arg" "$index_arg" "$q")" || exit 1
        cw="$(invoke_count q-walk "$root_arg" "$THREADS" "$q")" || exit 1
        if [ "$ci" -eq "$cw" ]; then
            kept+=("$q")
        else
            suppressed+=("$q (idx=$ci walk=$cw)")
        fi
    done

    # Paired rounds, arm order rotated within each round.
    row_query=()
    row_walk=()
    row_idx=()
    row_ratio=()
    row_verdict=()
    row_qstar=()
    row_qstar_num=()
    sample_dump=()
    for q in "${kept[@]}"; do
        WALK=()
        IDX=()
        for r in "${round_seq[@]}"; do
            if [ $(( r % 2 )) -eq 1 ]; then
                invoke_ms q-walk "$root_arg" "$THREADS" "$q"
                WALK+=("$INVOKE_MS")
                invoke_ms q-index "$root_arg" "$index_arg" "$q"
                IDX+=("$INVOKE_MS")
            else
                invoke_ms q-index "$root_arg" "$index_arg" "$q"
                IDX+=("$INVOKE_MS")
                invoke_ms q-walk "$root_arg" "$THREADS" "$q"
                WALK+=("$INVOKE_MS")
            fi
        done
        w_us="$(min_of "${WALK[@]}")"
        i_us="$(min_of "${IDX[@]}")"
        walk_tenths="$(round_half_even "$w_us" 100)"
        idx_hundredths="$(round_half_even "$i_us" 10)"
        walk_min="$(fmt_scaled "$walk_tenths" 1)"
        idx_min="$(fmt_scaled "$idx_hundredths" 2)"
        ratio="$(ratio_tenths "$w_us" "$i_us")"
        verdict="$(get_verdict)"
        qstar="$(qstar_of "$build_tenths" "$w_us" "$i_us")"
        row_query+=("$q")
        row_walk+=("$walk_tenths")
        row_idx+=("$idx_hundredths")
        row_ratio+=("$ratio")
        row_verdict+=("$verdict")
        row_qstar+=("$qstar")
        if [ "$qstar" = 'Infinity' ]; then
            row_qstar_num+=("9223372036854775807")
        else
            row_qstar_num+=("$qstar")
        fi
        sample_dump+=("  $q walk: $(rounded_samples 1 "${WALK[@]}")")
        sample_dump+=("  $q index: $(rounded_samples 2 "${IDX[@]}")")
    done

    md+=("## Corpus: $n names ($entries files materialized)")
    md+=("")
    md+=("Build (walk + index + persist): **$build_min ms** (min of $ROUNDS). Process floor (parse and exit): $floor_min ms (min of 7).")
    md+=("")
    md+=("| query | walk min (ms) | index min (ms) | ratio | verdict | Q* |")
    md+=("|---|---|---|---|---|---|")
    for ((i = 0; i < ${#row_query[@]}; i++)); do
        md+=("| \`${row_query[$i]}\` | $(fmt_scaled "${row_walk[$i]}" 1) | $(fmt_scaled "${row_idx[$i]}" 2) | $(fmt_scaled "${row_ratio[$i]}" 1)x | ${row_verdict[$i]} | ${row_qstar[$i]} |")
    done
    md+=("")
    if [ ${#suppressed[@]} -gt 0 ]; then
        suppressed_md=()
        for s in "${suppressed[@]}"; do suppressed_md+=("\`$s\`"); done
        md+=("Suppressed by the count gate (counts disagree; not timed): $(join_by '; ' "${suppressed_md[@]}").")
        md+=("")
    fi
    md+=("Cumulative cost on the least favorable kept query (largest Q*):")
    md+=("")
    md+=("| Q | walk | prepared |")
    md+=("|---|---|---|")
    # The row with the largest Q*: the least favorable query, the one the
    # prepared arm takes longest to amortize.
    best=0
    for ((i = 1; i < ${#row_qstar_num[@]}; i++)); do
        if [ "${row_qstar_num[$i]}" -gt "${row_qstar_num[$best]}" ]; then best="$i"; fi
    done
    if [ ${#row_query[@]} -gt 0 ]; then
        best_walk="${row_walk[$best]}"
        best_idx="${row_idx[$best]}"
    else
        best_walk=0
        best_idx=0
    fi
    for q in 1 2 4 8 16 32; do
        walk_ms="$(fmt_scaled "$(round_half_even $(( q * best_walk )) 10)" 0)"
        prepared_ms="$(fmt_scaled "$(round_half_even $(( build_tenths * 10 + q * best_idx )) 100)" 0)"
        md+=("| $q | $walk_ms ms | $prepared_ms ms |")
    done
    md+=("")
    md+=("Raw samples (ms, in round order):")
    md+=("")
    md+=("\`\`\`")
    for line in "${sample_dump[@]}"; do md+=("$line"); done
    md+=("\`\`\`")
    md+=("")
done

md+=("## Notes")
md+=("")
md+=("- The direct arm answers by enumerating the tree and matching names")
md+=("  (what \`fd\` / \`dir /s\` do); no index is built in that arm.")
md+=("- The prepared arm pays its index load inside the timed invocation")
md+=("  (setup is charged, per the protocol).")
md+=("- Q* is T_build / (walk - index) from the minima, the same ratio as")
md+=("  the content tier's thm:breakeven.")
md+=("- The fixture and the index live under \`$tmp_display\`; the corpus is")
md+=("  the deterministic name_bench generator (\`name_proto materialize\`).")

# The .ps1 joins the lines with LF and lets Set-Content add the platform's
# newline at the end: CRLF on Windows, LF elsewhere.
{
    total=${#md[@]}
    i=0
    while [ "$i" -lt "$total" ]; do
        if [ "$i" -lt $(( total - 1 )) ]; then
            printf '%s\n' "${md[$i]}"
        else
            printf '%s' "${md[$i]}"
        fi
        i=$(( i + 1 ))
    done
} > "$out_path"
write_status=$?
if [ "$write_status" -ne 0 ]; then
    echo "could not write $out_path" >&2
    exit 1
fi
if [ "$on_windows" -eq 1 ]; then printf '\r\n' >> "$out_path"; else printf '\n' >> "$out_path"; fi

echo "wrote $out_display"
