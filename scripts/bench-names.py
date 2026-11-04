#!/usr/bin/env python3
"""Measure the name-search bench, and gate a change against the baseline.

The perf gate is a number: a >20% regression on any row of
`benchmark/baseline-names.md` fails the PR. This script is how that number
is produced and compared, because a single run of the bench is not a number
this machine can be trusted with.

The bench measures min/med/p90 over 30 in-process rounds, which is the right
discipline inside one process and not enough between them: the same code,
three runs, moved the `mod` row's min between 65 us and 116 us on the machine
that wrote the table -- and the commit that wrote the table did not reproduce
it either. A gate that compares one run against one recorded run cannot tell
a regression from a noisy neighbour.

So a measurement here is K independent runs of the bench, and the statistic
the gate compares is the **best** of them: the fastest the machine managed.
Noise only ever makes a run slower, so the best run is the one closest to the
code's real cost, and it converges as K grows. The spread across the runs is
recorded beside it, because a spread wider than the threshold means the
measurement cannot decide the gate -- and the script says so instead of
guessing.

A baseline is also a machine. The same Rust is not the same machine code
on Linux and on Windows -- the code generator is the OS's -- and the
measurement is a memory-bound one, so the CPU's memory system is in it
too. A baseline recorded on one host cannot rule on another, so the rows
carry the host they were taken on, the gate compares against the rows of
the host it runs on, and the write-up carries a table per host. The host
is derived from the OS and the CPU, and `--host` overrides it when the
machine has more than one name.

The width of the worker pool is fixed too, because the noise is a function
of it. On a 32-thread host with other work on it, the `mod` row measured over
five runs of the same code, at 200k names:

    4 threads    102.1 108.6 108.5 107.2 106.1 us    spread  6.4%
    8 threads     81.2  70.1  74.0  71.3  76.9 us    spread 15.8%
   16 threads     60.5  56.6  83.1  83.4  77.5 us    spread 47.3%
   32 threads     62.8 117.6 105.3 117.3  68.1 us    spread 87.3%

The wide pool is faster and unmeasurable: with 32 workers on a host that has
other work, the spread is wider than the threshold the gate applies, so no
comparison against it decides anything. Four workers on the same host spread
6.4% -- under the threshold -- and a thread-matched measurement is the only
kind two machines, or two hosts, can be compared on at all. The width is
therefore fixed here, recorded in the baseline, and refused when it does not
match.

    # measure and print the table
    python scripts/bench-names.py measure --runs 7

    # record a baseline (the CSV the gate reads)
    python scripts/bench-names.py measure --runs 7 --names 200000 --names 5000000 \
        --out benchmark/baseline-names.csv

    # gate a change against the recorded baseline, on this host
    python scripts/bench-names.py check --baseline benchmark/baseline-names.csv --runs 7

    # regenerate the table in the write-up, and prove it has not drifted
    python scripts/bench-names.py render --csv benchmark/baseline-names.csv
    python scripts/bench-names.py render --csv benchmark/baseline-names.csv --check

Exit codes: 0 pass, 1 regression, 2 undecidable (the noise floor is wider
than the threshold), 3 the measurement itself failed.

The table in `benchmark/baseline-names.md` is generated from the CSV: the
CSV is the source of truth, the Markdown is what a reader reads, and
`--check` fails when the two have drifted.
"""
import argparse
import csv
import io
import os
import platform
import re
import statistics
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
BASELINE_CSV = ROOT / "benchmark" / "baseline-names.csv"
BASELINE_MD = ROOT / "benchmark" / "baseline-names.md"


# The threshold the gate uses, and the one the write-up states.
THRESHOLD_PCT = 20.0

# The worker width the measurement is taken at. Four is narrow enough that
# the host's other work does not decide the number, and it is the same on
# every machine, which is what makes two hosts comparable.
THREADS = 4

# Below this, a row's percentage is the timer's resolution talking, not the
# code's cost: the exact-match row answers in 0.3 us, where a 20% move is
# 0.06 us. The gate does not rule on rows this small, and the table does
# not print a spread for them.
MIN_GATE_US = 10.0


def cpu_model() -> str:
    """The CPU, as the machine says it."""
    if sys.platform.startswith("linux"):
        try:
            for line in Path("/proc/cpuinfo").read_text(encoding="utf-8").splitlines():
                if line.startswith("model name"):
                    return line.split(":", 1)[1].strip()
        except OSError:
            pass
    elif sys.platform == "win32":
        try:
            import winreg

            with winreg.OpenKey(
                winreg.HKEY_LOCAL_MACHINE,
                r"HARDWARE\DESCRIPTION\System\CentralProcessor\0",
            ) as key:
                return str(winreg.QueryValueEx(key, "ProcessorNameString")[0])
        except OSError:
            pass
    return platform.processor() or platform.machine() or "unknown"


def default_host() -> str:
    """A stable name for the machine class a measurement was taken on.

    The OS is in it because the code generator is: the same Rust is not the
    same machine code on Linux and on Windows, and a baseline taken on one
    of them cannot rule on the other. The CPU is in it because the memory
    system is, and the measurement is a memory-bound one.
    """
    raw = f"{platform.system()} {cpu_model()}"
    # A VM's copy of a CPU carries the vendor's graphics tail ("AMD Ryzen 7
    # 255 w/ Radeon 780M Graphics"); the CPU is the part before it.
    raw = raw.split(" w/ ")[0]
    return re.sub(r"[^a-z0-9]+", "-", raw.lower()).strip("-")


def bench_command(names: int) -> list[str]:
    """The command that measures one run.

    `--csv` puts the machine-readable table on stdout and the human summary
    on stderr. `--no-walk` drops the walk row: it writes one file per corpus
    entry, which at 5M is a fixture on disk for a number the gate does not
    compare.
    """
    return [
        "cargo", "run", "--release", "-p", "rgy-engine", "--example", "name_bench",
        str(names), "--csv", "--no-walk",
    ]


def run_once(names: int, threads: int) -> list[dict]:
    """One run of the bench, as the rows it printed."""
    env = dict(os.environ)
    # The worker width the measurement is taken at. The bench measures a
    # parallel scan, so the width is part of what is being measured, and
    # an unpinned width is how a table stops being comparable. The name
    # is the crate's own (`RGY_NUM_THREADS`); the width is the user's,
    # and this is where the bench says what it wants.
    env["RGY_NUM_THREADS"] = str(threads)
    proc = subprocess.run(
        bench_command(names), cwd=ROOT, capture_output=True, text=True, env=env
    )
    if proc.returncode != 0:
        sys.stderr.write(proc.stderr)
        raise SystemExit(
            f"bench-names: name_bench failed with exit code {proc.returncode}"
        )
    rows = list(csv.DictReader(io.StringIO(proc.stdout)))
    if not rows:
        raise SystemExit("bench-names: name_bench printed no rows")
    return rows


def gate_stat(row: dict) -> float:
    """The number the gate compares for one row, in microseconds.

    For a query row it is the bench's own min-of-30: the fastest of the
    timed rounds, which is the closest the round came to the code's real
    cost. The walk row is measured once per run and has no rounds.
    """
    if row["kind"] == "walk":
        return float(row["elapsed_s"]) * 1e6
    return float(row["min_us"])


def aggregate(
    host: str, corpus: int, runs: list[list[dict]], threads: int, gated: bool
) -> list[dict]:
    """Fold K runs into one row per query, with the spread recorded.

    The spread is the dispersion of the per-run statistics, relative to the
    best of them: it is the width of the noise floor the gate has to be
    read against.
    """
    by_query: dict[str, list[dict]] = {}
    for run in runs:
        for row in run:
            if row["kind"] == "walk":
                continue
            by_query.setdefault(row["query"], []).append(row)

    out = []
    for query, rows in by_query.items():
        stats = [gate_stat(r) for r in rows]
        best = min(stats)
        if best <= 0:
            raise SystemExit(f"bench-names: {query} measured {best} us")
        hits = {r["hits"] for r in rows}
        if len(hits) != 1:
            # The corpus is deterministic, so two runs cannot disagree on the
            # match count. If they do, one of them is not the same code, and
            # a timing measured against a different answer is not a timing.
            raise SystemExit(
                f"bench-names: {query} matched {sorted(hits)} across runs; "
                "the runs are not comparable"
            )
        plans = {r["plan"] for r in rows}
        if len(plans) != 1:
            raise SystemExit(
                f"bench-names: {query} was answered by {sorted(plans)} across runs"
            )
        out.append({
            "host": host,
            "corpus": corpus,
            "kind": "query",
            "query": query,
            "hits": rows[0]["hits"],
            "best_us": best,
            "med_us": statistics.median(float(r["med_us"]) for r in rows),
            "p90_us": statistics.median(float(r["p90_us"]) for r in rows),
            "spread_pct": (max(stats) - best) / best * 100.0,
            "runs": len(rows),
            "threads": threads,
            "gated": gated,
            "plan": rows[0]["plan"],
        })
    out.sort(key=lambda r: r["query"])
    return out


def measure(
    host: str, names: list[int], runs: int, threads: int, gate: list[int] | None
) -> list[dict]:
    """Measure every corpus size asked for, and fold each into its rows.

    `gate` lists the corpus sizes the gate rules on. A corpus whose
    measurement does not reproduce is still measured and still reported --
    it is just not a row the gate can decide.
    """
    rows: list[dict] = []
    for corpus in names:
        gated = gate is None or corpus in gate
        print(
            f"bench-names: measuring {corpus} names, {runs} runs, "
            f"{threads} threads, {'gated' if gated else 'reported only'}...",
            file=sys.stderr,
        )
        runs_rows = [run_once(corpus, threads) for _ in range(runs)]
        rows.extend(aggregate(host, corpus, runs_rows, threads, gated))
    return rows


def gated_corpora(base: list[dict]) -> list[int]:
    """The corpus sizes the baseline says the gate rules on."""
    return sorted({r["corpus"] for r in base if as_bool(r.get("gated", 1))})


CSV_FIELDS = [
    "host", "corpus", "kind", "query", "hits", "best_us", "med_us", "p90_us",
    "spread_pct", "runs", "threads", "gated", "plan",
]


def as_bool(value) -> bool:
    """Read a flag the way the CSV and Python each spell it."""
    if isinstance(value, bool):
        return value
    text = str(value).strip().lower()
    return text in {"1", "true", "yes"}


def write_csv(rows: list[dict], path: Path) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("w", newline="", encoding="utf-8") as fh:
        writer = csv.DictWriter(fh, fieldnames=CSV_FIELDS, lineterminator="\n")
        writer.writeheader()
        for row in rows:
            row = dict(row)
            row["gated"] = 1 if as_bool(row.get("gated")) else 0
            writer.writerow(row)


def read_csv(path: Path) -> list[dict]:
    with path.open(newline="", encoding="utf-8") as fh:
        rows = list(csv.DictReader(fh))
    if not rows:
        raise SystemExit(f"bench-names: {path} has no rows")
    if "host" not in (rows[0].keys() if rows else {}):
        raise SystemExit(
            f"bench-names: {path} has no host column; a baseline without a "
            "host cannot say which machine it was taken on"
        )
    # The corpus size and the worker width are numbers everywhere else, and a
    # row is matched by (host, corpus, query): a string here would make every
    # row look new, and a string width would make every row look incomparable.
    for row in rows:
        row["corpus"] = int(row["corpus"])
        row["threads"] = int(row["threads"])
        row["gated"] = 1 if as_bool(row.get("gated", 1)) else 0
    return rows


def render_table(rows: list[dict]) -> str:
    """The Markdown table, from the measured rows.

    The corpus is in the heading rather than in every row: one table per
    corpus size, the way the write-up reads.
    """
    out = []
    for corpus in sorted({r["corpus"] for r in rows}, key=int):
        block = [r for r in rows if r["corpus"] == corpus]
        runs = block[0]["runs"]
        threads = block[0].get("threads", THREADS)
        width = "1 thread" if int(threads) == 1 else f"{threads} threads"
        out.append(
            f"Corpus: {int(corpus):,} names, {width}, "
            f"best of {runs} runs."
        )
        out.append("")
        out.append("| query | hits | min µs | med µs | p90 µs | spread | plan |")
        out.append("|---|---|---|---|---|---|---|")
        for r in block:
            best = float(r["best_us"])
            spread = "—" if best < MIN_GATE_US else f"{float(r['spread_pct']):.0f}%"
            out.append(
                f"| `{r['query']}` | {int(r['hits']):,} | {best:.0f} "
                f"| {float(r['med_us']):.0f} | {float(r['p90_us']):.0f} "
                f"| {spread} | {r['plan']} |"
            )
        out.append("")
    return "\n".join(out).rstrip() + "\n"


def compare(base: list[dict], now: list[dict], threshold: float) -> int:
    """Compare a measurement against a baseline.

    Returns the exit code: 0 pass, 1 regression, 2 undecidable.

    A row is a regression when the new best is more than `threshold` per
    cent above the recorded best. A row whose noise floor is wider than the
    threshold cannot decide that on its own -- noise and a regression look
    the same from here -- so it is reported as undecidable rather than
    counted either way.
    """
    base_by_key = {(r["corpus"], r["query"]): r for r in base}
    now_by_key = {(r["corpus"], r["query"]): r for r in now}

    regressions = []
    undecidable = []
    missing = []
    plan_changed = []
    reported = []

    for key, b in base_by_key.items():
        n = now_by_key.get(key)
        if n is None:
            missing.append(key)
            continue
        b_threads = int(b["threads"])
        n_threads = int(n["threads"])
        if b_threads != n_threads:
            print(
                f"bench-names: THREAD MISMATCH {key[1]} (corpus {key[0]}): "
                f"the baseline was taken at {b_threads} threads and this "
                f"measurement at {n_threads}; they are not comparable",
                file=sys.stderr,
            )
            return 3
        b_best = float(b["best_us"])
        n_best = float(n["best_us"])
        ratio = (n_best / b_best - 1.0) * 100.0
        if not as_bool(b.get("gated", 1)):
            # The baseline says this row is reported, not gated: its
            # measurement does not reproduce on this host, so it is not a row
            # the gate can rule on. It is still worth saying what it did.
            reported.append((key, ratio))
            continue
        if b_best < MIN_GATE_US:
            print(
                f"bench-names: SKIP {key[1]} (corpus {key[0]}): the baseline "
                f"is {b_best:.1f} us, below the {MIN_GATE_US:.0f} us the gate "
                "rules on"
            )
            continue
        ratio = (n_best / b_best - 1.0) * 100.0
        floor = max(float(b["spread_pct"]), float(n["spread_pct"]))
        if b["plan"] != n["plan"]:
            plan_changed.append((key, b["plan"], n["plan"]))
        if ratio > threshold:
            if floor > threshold:
                undecidable.append((key, ratio, floor))
            else:
                regressions.append((key, ratio, b_best, n_best))

    for key in now_by_key:
        if key not in base_by_key:
            print(f"bench-names: {key[1]} is a new row (corpus {key[0]})")

    for key, ratio in reported:
        print(
            f"bench-names: REPORTED {key[1]} (corpus {key[0]}): {ratio:+.0f}% "
            "-- this row is measured but not gated"
        )
    for key, ratio, floor in undecidable:
        print(
            f"bench-names: UNDECIDABLE {key[1]} (corpus {key[0]}): "
            f"{ratio:+.0f}% but the noise floor is {floor:.0f}%, "
            f"wider than the {threshold:.0f}% threshold"
        )
    for key, ratio, b_best, n_best in regressions:
        print(
            f"bench-names: REGRESSION {key[1]} (corpus {key[0]}): "
            f"{b_best:.0f} us -> {n_best:.0f} us ({ratio:+.0f}%)"
        )
    for key, was, now_plan in plan_changed:
        print(
            f"bench-names: PLAN CHANGED {key[1]} (corpus {key[0]}): "
            f"{was} -> {now_plan}"
        )
    for key in missing:
        print(f"bench-names: MISSING {key[1]} (corpus {key[0]}): the row is not measured")

    if regressions or missing or plan_changed:
        return 1
    if undecidable:
        return 2
    print(f"bench-names: pass -- no row regressed by more than {threshold:.0f}%")
    return 0


def block_markers(host: str) -> tuple[str, str]:
    """The markers of one host's table."""
    return (
        f"<!-- BEGIN: bench-names-table host={host} -->",
        f"<!-- END: bench-names-table host={host} -->",
    )


def replace_block(md: str, table: str, host: str) -> str:
    """Put the generated table between the markers."""
    begin, end = block_markers(host)
    i = md.find(begin)
    j = md.find(end)
    if i < 0 or j < 0 or j < i:
        raise SystemExit(
            f"bench-names: {BASELINE_MD} has no generated block for {host}; "
            f"it needs {begin} and {end}"
        )
    head = md[: i + len(begin)]
    tail = md[j:]
    return f"{head}\n\n{table}\n{tail}"


def blocks_in(md: str) -> list[str]:
    """The hosts the write-up carries a table for."""
    return re.findall(r"<!-- BEGIN: bench-names-table host=([a-z0-9\-]+) -->", md)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    sub = parser.add_subparsers(dest="command", required=True)

    p_measure = sub.add_parser("measure", help="measure and print the table")
    p_measure.add_argument("--runs", type=int, default=7)
    p_measure.add_argument(
        "--names", type=int, action="append", default=None,
        help="corpus size; repeatable (default: 200000)",
    )
    p_measure.add_argument(
        "--threads", type=int, default=THREADS,
        help=f"the worker width the measurement is taken at (default: {THREADS})",
    )
    p_measure.add_argument(
        "--gate", type=int, action="append", default=None,
        help="corpus size the gate rules on; repeatable (default: all of them)",
    )
    p_measure.add_argument("--out", type=Path, default=None)
    p_measure.add_argument(
        "--host", default=None,
        help="the host the rows are taken on (default: this machine)",
    )

    p_check = sub.add_parser("check", help="gate a change against a baseline")
    p_check.add_argument("--baseline", type=Path, default=BASELINE_CSV)
    p_check.add_argument("--runs", type=int, default=7)
    p_check.add_argument(
        "--names", type=int, action="append", default=None,
        help="corpus size; repeatable (default: the baseline's own)",
    )
    p_check.add_argument(
        "--threads", type=int, default=None,
        help="the worker width (default: the baseline's own)",
    )
    p_check.add_argument("--threshold", type=float, default=THRESHOLD_PCT)
    p_check.add_argument(
        "--host", default=None,
        help="the host to compare against (default: this machine)",
    )

    p_render = sub.add_parser("render", help="render the table from a CSV")
    p_render.add_argument("--csv", type=Path, default=BASELINE_CSV)
    p_render.add_argument(
        "--check", action="store_true",
        help="fail when the write-up's table is not what this renders",
    )
    p_render.add_argument(
        "--write", action="store_true",
        help="put the rendered table into the write-up, between the markers",
    )
    p_render.add_argument(
        "--host", default=None,
        help="the host to render (default: every host in the CSV)",
    )

    args = parser.parse_args()

    if args.command == "measure":
        names = args.names or [200_000]
        host = args.host or default_host()
        print(f"bench-names: host {host}", file=sys.stderr)
        rows = measure(host, names, args.runs, args.threads, args.gate)
        if args.out:
            write_csv(rows, args.out)
            print(f"bench-names: wrote {args.out}", file=sys.stderr)
        print(render_table(rows))
        return 0

    if args.command == "check":
        base = read_csv(args.baseline)
        host = args.host or default_host()
        hosts = {r["host"] for r in base}
        if host not in hosts:
            print(
                f"bench-names: {args.baseline} has no rows for host {host}; "
                f"it has {sorted(hosts)}. A baseline recorded on another host "
                "cannot rule on this one.",
                file=sys.stderr,
            )
            return 3
        base = [r for r in base if r["host"] == host]
        print(f"bench-names: host {host}, against {args.baseline}", file=sys.stderr)
        names = args.names or sorted({int(r["corpus"]) for r in base})
        threads = args.threads
        if threads is None:
            widths = {r.get("threads", "") for r in base}
            if len(widths) != 1:
                raise SystemExit(
                    "bench-names: the baseline was taken at more than one "
                    f"worker width ({sorted(widths)}); say which one to use"
                )
            threads = int(widths.pop() or THREADS)
        now = measure(host, names, args.runs, threads, gated_corpora(base))
        return compare(base, now, args.threshold)

    if args.command == "render":
        rows = read_csv(args.csv)
        hosts = [args.host] if args.host else sorted({r["host"] for r in rows})
        tables = {h: render_table([r for r in rows if r["host"] == h]) for h in hosts}
        if args.write:
            md = BASELINE_MD.read_text(encoding="utf-8")
            # LF, whatever the platform: `.gitattributes` says `*.md text
            # eol=lf`, and a Python that writes CRLF on Windows would make the
            # file differ from the tree for a reason that has nothing to do
            # with the measurement.
            for host in hosts:
                md = replace_block(md, tables[host], host)
            with BASELINE_MD.open("w", encoding="utf-8", newline="\n") as fh:
                fh.write(md)
            print(f"bench-names: wrote the table into {BASELINE_MD}")
            return 0
        if not args.check:
            for host in hosts:
                print(tables[host], end="")
            return 0
        md = BASELINE_MD.read_text(encoding="utf-8")
        for host in blocks_in(md):
            if host not in hosts:
                print(
                    f"bench-names: {BASELINE_MD} carries a table for {host}, "
                    f"which {args.csv} does not have rows for",
                    file=sys.stderr,
                )
                return 1
        for host in hosts:
            wanted = replace_block(md, tables[host], host)
            if wanted != md:
                print(
                    f"bench-names: {BASELINE_MD} does not carry the table this "
                    f"renders for {host}; regenerate it",
                    file=sys.stderr,
                )
                return 1
            md = wanted
        print(f"bench-names: {BASELINE_MD} carries the table this renders")
        return 0

    parser.error("unknown command")
    return 2


if __name__ == "__main__":
    sys.exit(main())
