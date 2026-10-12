"""Phase 1 measurement: cold (walk + build) vs warm (mmap) name index.

Runs the release `searchctl` against a real tree and times both paths with
the same query, so the delta is attributable to index reuse alone.
"""

import os
import re
import statistics
import subprocess
import sys
import time
from pathlib import Path

BIN = Path(r"C:\Users\danalec\Documents\src\rgy\target\release\searchctl.exe")
ROOT = sys.argv[1] if len(sys.argv) > 1 else r"C:\Users\danalec\Documents\src"
QUERY = sys.argv[2] if len(sys.argv) > 2 else "walk"
INDEX = Path(os.environ["TEMP"]) / "rgy-phase1-bench.idx"
COLD_RUNS = int(os.environ.get("COLD_RUNS", "3"))
WARM_RUNS = int(os.environ.get("WARM_RUNS", "7"))

ENTRIES = re.compile(r"indexed ([\d,]+) entries")
REUSED = re.compile(r"reusing index .* \((\d+) entries\)")


def run(extra):
    t0 = time.perf_counter()
    p = subprocess.run(
        [str(BIN), "--root", ROOT, "--limit", "5", "--index", str(INDEX), *extra, QUERY],
        capture_output=True,
        text=True,
    )
    return time.perf_counter() - t0, p


def stat(xs):
    return (
        f"min {min(xs)*1000:8.1f} ms   med {statistics.median(xs)*1000:8.1f} ms"
        f"   max {max(xs)*1000:8.1f} ms"
    )


if not BIN.exists():
    sys.exit(f"missing {BIN}")

for p in (INDEX, Path(str(INDEX) + ".meta")):
    p.unlink(missing_ok=True)

print(f"bin   : {BIN}")
print(f"root  : {ROOT}")
print(f"query : {QUERY!r}")
print(f"index : {INDEX}")
print()

# Cold: --reindex forces walk + build every time, so no run is helped by a
# leftover index. The first one also warms the OS page cache for the others,
# which is the fair comparison against a warm index.
cold = []
entries = None
for i in range(COLD_RUNS):
    dt, p = run(["--reindex"])
    cold.append(dt)
    m = ENTRIES.search(p.stderr)
    if m:
        entries = m.group(1)
print(f"cold (walk + build) x{COLD_RUNS}: {stat(cold)}")
print(f"entries indexed    : {entries or '?'}")

# Warm: first run pays the write of a fresh index, the rest are pure reuse.
dt, p = run([])
m = REUSED.search(p.stderr)
if not m:
    sys.exit(f"expected an index reuse, got: {p.stderr.strip()!r}")
warm = []
for _ in range(WARM_RUNS):
    dt, p = run([])
    warm.append(dt)

size = INDEX.stat().st_size / (1024 * 1024)
print(f"warm (mmap load)  x{WARM_RUNS}: {stat(warm)}")
print(f"index on disk      : {size:.1f} MiB")

cm, wm = statistics.median(cold), statistics.median(warm)
print()
print(f"median cold {cm*1000:.1f} ms  ->  warm {wm*1000:.1f} ms")
print(f"speedup: {cm/wm:.1f}x   saved {cm-wm:.3f}s per query")
if entries:
    n = int(entries.replace(",", ""))
    print(f"index is {size*1024*1024/max(n,1):.1f} bytes/entry on disk")