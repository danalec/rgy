"""Calibrate the Phase 1 benchmark: how big is each candidate tree, and how
long does ONE cold walk+build take? Sizes decide the run budget."""

import os
import subprocess
import sys
import time
from pathlib import Path

BIN = Path(r"C:\Users\danalec\Documents\src\rgy\target\release\searchctl.exe")
CANDIDATES = [
    r"C:\Users\danalec\Documents\src\rgy\crates",
    r"C:\Users\danalec\Documents\src\rgy\crates\rgx-core",
    r"C:\Users\danalec\.cargo\registry",
    r"C:\Users\danalec\.rustup\toolchains",
]


def count(root, cap=400_000):
    """Entries under root, bailing out at `cap` so a huge tree costs little."""
    files = dirs = 0
    t0 = time.time()
    for dp, dns, fns in os.walk(root):
        dirs += len(dns)
        files += len(fns)
        if files + dirs > cap:
            return f">{cap:,}+", time.time() - t0
    return f"{files+dirs:,}", time.time() - t0


for root in CANDIDATES:
    if not Path(root).exists():
        print(f"{root}: MISSING")
        continue
    n, dt = count(root)
    print(f"{n:>12}  entries  ({dt:5.1f}s to count)  {root}")

print()
# One cold run on the smallest real tree, to price a run.
root = sys.argv[1] if len(sys.argv) > 1 else CANDIDATES[-2]
idx = Path(os.environ["TEMP"]) / "rgy-calib.idx"
for p in (idx, Path(str(idx) + ".meta")):
    p.unlink(missing_ok=True)
t0 = time.perf_counter()
p = subprocess.run(
    [str(BIN), "--root", root, "--limit", "3", "--index", str(idx), "--reindex", "e"],
    capture_output=True,
    text=True,
)
cold = time.perf_counter() - t0
t0 = time.perf_counter()
p2 = subprocess.run(
    [str(BIN), "--root", root, "--limit", "3", "--index", str(idx), "e"],
    capture_output=True,
    text=True,
)
warm = time.perf_counter() - t0
print(f"root        : {root}")
print(f"cold  1 run : {cold*1000:8.1f} ms")
print(f"warm  1 run : {warm*1000:8.1f} ms")
print(f"speedup     : {cold/warm:.1f}x")
print(f"stderr cold : {p.stderr.strip()[:120]}")
print(f"stderr warm : {p2.stderr.strip()[:120]}")