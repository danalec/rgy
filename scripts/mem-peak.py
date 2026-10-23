#!/usr/bin/env python3
"""Run a command and report the peak RSS it reached (plan item 4).

The memory goals (peak <=1.2 GB, resident <=64 MB @5M) are measured by
running an index build or the name bench under this wrapper:

    python scripts/mem-peak.py -- cargo run --release -p rgy-engine --example name_bench -- 5000000

It prints the command's exit code and its peak resident set (the "peak" the
plan means: the high-water mark, not the current RSS). Peak working set on
Windows, VmHWM on Linux, ru_maxrss on macOS. Emits one line to stdout so it
can be logged beside the other benchmark tables.
"""
import argparse
import os
import subprocess
import sys

WINDOWS = os.name == "nt"


def peak_rss_bytes(pid: int) -> int:
    """Peak resident set of `pid`, in bytes. 0 if unavailable."""
    if WINDOWS:
        import ctypes
        from ctypes import wintypes

        class PROCESS_MEMORY_COUNTERS(ctypes.Structure):
            _fields_ = [
                ("cb", wintypes.DWORD),
                ("PageFaultCount", wintypes.DWORD),
                ("PeakWorkingSetSize", ctypes.c_size_t),
                ("WorkingSetSize", ctypes.c_size_t),
                ("QuotaPeakPagedPoolUsage", ctypes.c_size_t),
                ("QuotaPagedPoolUsage", ctypes.c_size_t),
                ("QuotaPeakNonPagedPoolUsage", ctypes.c_size_t),
                ("QuotaNonPagedPoolUsage", ctypes.c_size_t),
                ("PagefileUsage", ctypes.c_size_t),
                ("PeakPagefileUsage", ctypes.c_size_t),
            ]

        k32 = ctypes.WinDLL("kernel32", use_last_error=True)
        psapi = ctypes.WinDLL("psapi", use_last_error=True)
        handle = k32.OpenProcess(0x1000, False, pid)  # PROCESS_QUERY_INFORMATION
        if not handle:
            return 0
        try:
            counters = PROCESS_MEMORY_COUNTERS()
            counters.cb = ctypes.sizeof(counters)
            if psapi.GetProcessMemoryInfo(handle, ctypes.byref(counters), counters.cb):
                return int(counters.PeakWorkingSetSize)
        finally:
            k32.CloseHandle(handle)
        return 0

    # Linux: VmHWM is the peak resident set. macOS: ru_maxrss via wait4 below.
    try:
        with open(f"/proc/{pid}/status", encoding="utf-8") as f:
            for line in f:
                if line.startswith("VmHWM:"):
                    return int(line.split()[1]) * 1024
    except OSError:
        pass
    return 0


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--quiet", action="store_true", help="only print the peak line")
    ap.add_argument(
        "--hold-seconds",
        type=float,
        default=0.0,
        help="daemon mode: run for N seconds, then kill it and report the last "
        "RSS (the steady state) as well as the peak. 0 = run to completion.",
    )
    ap.add_argument("cmd", nargs=argparse.REMAINDER, help="command to run")
    args = ap.parse_args()
    cmd = args.cmd[1:] if args.cmd[:1] == ["--"] else args.cmd
    if not cmd:
        ap.error("no command given (use: mem-peak.py -- <command...>)")

    import time

    proc = subprocess.Popen(cmd)
    peak = 0
    last = 0
    start = time.monotonic()
    # Sample while it runs: the process may exit before we can query it once.
    try:
        while proc.poll() is None:
            now = peak_rss_bytes(proc.pid)
            peak = max(peak, now)
            last = now or last
            if args.hold_seconds > 0 and time.monotonic() - start >= args.hold_seconds:
                proc.kill()
                break
            # Windows peak working set is monotone; Linux VmHWM too. Sleep
            # just enough to not spin; 50 ms is well under a build's phases.
            time.sleep(0.05)
    except KeyboardInterrupt:
        proc.kill()
    peak = max(peak, peak_rss_bytes(proc.pid))
    code = proc.wait()

    # macOS has no /proc; fall back to the reaped child's ru_maxrss (KiB).
    if peak == 0 and not WINDOWS:
        try:
            import resource
            peak = resource.getrusage(resource.RUSAGE_CHILDREN).ru_maxrss * 1024
        except (ImportError, ValueError):
            pass

    steady = f" resident_rss_mib={last / (1 << 20):.1f}" if args.hold_seconds > 0 else ""
    print(f"peak_rss_bytes={peak} peak_rss_mib={peak / (1 << 20):.1f}{steady} exit={code}")
    return code


if __name__ == "__main__":
    sys.exit(main())
