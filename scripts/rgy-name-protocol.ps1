# rgy-name-protocol.ps1 -- the paper's protocol (sec:eval) applied to the
# name tier: paired samples, arm order rotated within each round, minimum
# of seven as estimator, a count gate in front of every timing line, and a
# verdict that can refuse (a +/-3% floor is a tie; a winner is named only
# when every sample of one arm lies below every sample of the other;
# overlapping distributions are reported as such).
#
# The timer is the shell's wall clock around the whole invocation --
# setup, search and teardown -- with query output discarded, and the count
# gate runs in a separate untimed pass, so no stage timer adjudicates
# anything (sec:mmap is why). The process floor is measured here, in this
# session, never carried over.
#
# Usage:
#   pwsh scripts/rgy-name-protocol.ps1 -Scales 200000,5000000 `
#        -Rounds 7 -Threads 16 -Out benchmark/rgy-name-transcript.md

param(
    [string]$Repo = (Split-Path $PSScriptRoot -Parent),
    [int[]]$Scales = @(200000, 5000000),
    [int]$Rounds = 7,
    [int]$Threads = 16,
    [string]$Out = 'benchmark/rgy-name-transcript.md',
    [string[]]$Queries = @('report', 'file', "'report_00000.pdf", '^src', 'dat$', 'mod', 'rep', 'f1')
)

$ErrorActionPreference = 'Stop'
$bin = Join-Path $Repo 'target\release\examples\name_proto.exe'
if (!(Test-Path $bin)) {
    cargo build --release --manifest-path (Join-Path $Repo 'Cargo.toml') `
        -p engine --example name_proto
}

$tree = (git -C $Repo rev-parse HEAD).Trim()
$date = (Get-Date).ToString('yyyy-MM-dd HH:mm zzz')
$cpu = (Get-CimInstance Win32_Processor).Name.Trim()
$os = (Get-CimInstance Win32_OperatingSystem).Caption + ' build ' +
    (Get-CimInstance Win32_OperatingSystem).BuildNumber

function Invoke-Ms([string[]]$Cli) {
    # Wall clock around the whole process, query output discarded.
    $sw = [System.Diagnostics.Stopwatch]::StartNew()
    & $bin @Cli 2>$null | Out-Null
    $sw.Stop()
    return $sw.Elapsed.TotalMilliseconds
}

function Invoke-Count([string[]]$Cli) {
    # Fail loudly: a missing index or a panic must never read as zero hits,
    # or a 0 == 0 accident passes the gate.
    $out = (& $bin @Cli 2>$null | Out-String).Trim()
    if ($out -notmatch '^\d+$') {
        throw "name_proto $($Cli -join ' ') produced no count (got: '$out')"
    }
    return [int]$out
}

function Get-Min([double[]]$xs) { ($xs | Measure-Object -Minimum).Minimum }

function Get-Verdict([double[]]$a, [double[]]$b) {
    # a = walk samples, b = index samples. The paper's gate: within +/-3%
    # a tie; outside it a winner is named only if every sample of one arm
    # lies below every sample of the other; overlapping distributions are
    # reported as such rather than as a ratio.
    $floor = 3.0
    $ra = (Get-Min $a) / (Get-Min $b) * 100.0
    if ([math]::Abs($ra - 100.0) -lt $floor) { return 'tie' }
    $aMin = Get-Min $a; $aMax = ($a | Measure-Object -Maximum).Maximum
    $bMin = Get-Min $b; $bMax = ($b | Measure-Object -Maximum).Maximum
    if ($aMin -gt $bMax) { return 'index (separated)' }
    if ($bMin -gt $aMax) { return 'walk (separated)' }
    return 'overlap, not a result'
}

$md = @()
$md += "# Name-tier protocol transcript (rgy name_proto)"
$md += ""
$md += "Host: $env:COMPUTERNAME --- $cpu --- $os --- 64 GB DDR5-6000."
$md += "Tree: rgy $tree. Date: $date. Threads: walk arm scans with $Threads;"
$md += "the index arm's scan parallelism is its own runtime default (the walk"
$md += "arm being wider is the conservative direction: it understates the"
$md += "saving). Timer: shell wall clock around the whole process, query output"
$md += "discarded. Estimator: minimum of $Rounds paired samples per arm, arm"
$md += "order rotated within each paired round. Count gate: a separate untimed"
$md += "pass compares both arms' match counts; a disagreeing line is"
$md += "suppressed, not timed. Warm cache: the untimed pass and the first"
$md += "round populate it. Floor: measured below, in this session."
$md += ""

foreach ($n in $Scales) {
    $root = Join-Path $env:TEMP "rgy_name_proto_$n"
    $index = Join-Path $env:TEMP "rgy_name_proto_$n.idx"
    if (!(Test-Path $root)) {
        & $bin materialize $root $n $Threads 2>$null
    }
    $entries = (Get-ChildItem $root -Recurse -File | Measure-Object).Count

    # Build arm first: it creates the index the count gate's q-index arm
    # loads. T_build of the break-even ratio.
    $buildSamples = 1..$Rounds | ForEach-Object { Invoke-Ms @('build', $root, $index, "$Threads") }
    $buildMin = [math]::Round((Get-Min $buildSamples), 1)

    # Floor, this session.
    $floorSamples = 1..7 | ForEach-Object { Invoke-Ms @('floor') }
    $floorMin = [math]::Round((Get-Min $floorSamples), 2)

    # Count gate, untimed: keep only lines whose counts agree exactly.
    $kept = @()
    $suppressed = @()
    foreach ($q in $Queries) {
        $ci = Invoke-Count @('q-index', $root, $index, $q)
        $cw = Invoke-Count @('q-walk', $root, "$Threads", $q)
        if ($ci -eq $cw) { $kept += $q } else { $suppressed += "$q (idx=$ci walk=$cw)" }
    }

    # Paired rounds, arm order rotated within each round.
    $rows = @()
    $sampleDump = @()
    foreach ($q in $kept) {
        $walk = @(); $idx = @()
        for ($r = 1; $r -le $Rounds; $r++) {
            if ($r % 2 -eq 1) {
                $walk += Invoke-Ms @('q-walk', $root, "$Threads", $q)
                $idx  += Invoke-Ms @('q-index', $root, $index, $q)
            } else {
                $idx  += Invoke-Ms @('q-index', $root, $index, $q)
                $walk += Invoke-Ms @('q-walk', $root, "$Threads", $q)
            }
        }
        $wMin = Get-Min $walk; $iMin = Get-Min $idx
        $rows += [pscustomobject]@{
            Query = $q; WalkMin = [math]::Round($wMin, 1)
            IdxMin = [math]::Round($iMin, 2)
            Ratio = [math]::Round($wMin / $iMin, 1)
            Verdict = (Get-Verdict $walk $idx)
            Qstar = [math]::Ceiling($buildMin / ($wMin - $iMin))
        }
        $sampleDump += "  $q walk: " + (($walk | ForEach-Object { [math]::Round($_, 1) }) -join ', ')
        $sampleDump += "  $q index: " + (($idx | ForEach-Object { [math]::Round($_, 2) }) -join ', ')
    }

    $md += "## Corpus: $n names ($entries files materialized)"
    $md += ""
    $md += "Build (walk + index + persist): **$buildMin ms** (min of $Rounds)." +
        " Process floor (parse and exit): $floorMin ms (min of 7)."
    $md += ""
    $md += "| query | walk min (ms) | index min (ms) | ratio | verdict | Q* |"
    $md += "|---|---|---|---|---|---|"
    foreach ($r in $rows) {
        $md += "| ``$($r.Query)`` | $($r.WalkMin) | $($r.IdxMin) | $($r.Ratio)x | $($r.Verdict) | $($r.Qstar) |"
    }
    $md += ""
    if ($suppressed.Count -gt 0) {
        $md += "Suppressed by the count gate (counts disagree; not timed): " +
            (($suppressed | ForEach-Object { "``$_``" }) -join '; ') + "."
        $md += ""
    }
    $md += "Cumulative cost on the least favorable kept query (largest Q*):"
    $md += ""
    $md += "| Q | walk | prepared |"
    $md += "|---|---|---|"
    $w = $rows | Sort-Object Qstar -Descending | Select-Object -First 1
    foreach ($q in 1, 2, 4, 8, 16, 32) {
        $md += "| $q | $([math]::Round($q * $w.WalkMin, 0)) ms | $([math]::Round($buildMin + $q * $w.IdxMin, 0)) ms |"
    }
    $md += ""
    $md += "Raw samples (ms, in round order):"
    $md += ""
    $md += '```'
    $md += $sampleDump
    $md += '```'
    $md += ""
}

$md += "## Notes"
$md += ""
$md += "- The direct arm answers by enumerating the tree and matching names"
$md += "  (what ``fd`` / ``dir /s`` do); no index is built in that arm."
$md += "- The prepared arm pays its index load inside the timed invocation"
$md += "  (setup is charged, per the protocol)."
$md += "- Q* is T_build / (walk - index) from the minima, the same ratio as"
$md += "  the content tier's thm:breakeven."
$md += "- The fixture and the index live under ``$env:TEMP``; the corpus is"
$md += "  the deterministic name_bench generator (``name_proto materialize``)."

$md -join "`n" | Set-Content -Path (Join-Path $Repo $Out) -Encoding utf8
Write-Host "wrote $(Join-Path $Repo $Out)"
