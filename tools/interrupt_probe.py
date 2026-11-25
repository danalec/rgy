#!/usr/bin/env python3
"""Send a real console Ctrl event to a running index build and report what it did.

`Process.Kill` (and Python's `kill()`) is TerminateProcess, which never
reaches a console handler -- so it proves nothing about Ctrl-C support. This
spawns the child in its own process group and calls GenerateConsoleCtrlEvent,
which is what a real Ctrl-C does.

Usage: interrupt_probe.py <exe> <root> <query> [delay_seconds]
"""

import ctypes
import os
import subprocess
import sys
import tempfile
import time

CTRL_BREAK_EVENT = 1


def main() -> int:
    exe, root, query = sys.argv[1], sys.argv[2], sys.argv[3]
    delay = float(sys.argv[4]) if len(sys.argv) > 4 else 0.7

    env = dict(os.environ)
    # Keep the probe off the real index directory.
    env["RGY_INDEX_DIR"] = tempfile.mkdtemp(prefix="rgy-interrupt-")

    out = tempfile.NamedTemporaryFile(delete=False, suffix=".out", mode="w+")
    err = tempfile.NamedTemporaryFile(delete=False, suffix=".err", mode="w+")

    proc = subprocess.Popen(
        [exe, "--root", root, "--reindex", query],
        stdout=out,
        stderr=err,
        env=env,
        creationflags=subprocess.CREATE_NEW_PROCESS_GROUP,
    )

    time.sleep(delay)
    if proc.poll() is not None:
        print(f"FAIL: the build finished in under {delay}s -- too fast to interrupt.")
        print("Use a larger tree or a longer delay.")
        return 1

    # Attach to the child's console and raise the event on its group.
    kernel32 = ctypes.windll.kernel32
    # 0xFFFF == the process group we created above.
    ok = kernel32.GenerateConsoleCtrlEvent(CTRL_BREAK_EVENT, proc.pid)
    print(f"GenerateConsoleCtrlEvent -> {ok}")

    try:
        proc.wait(timeout=20)
    except subprocess.TimeoutExpired:
        print("FAIL: still running 20s after the event -- the handler never fired.")
        proc.kill()
        return 1

    print(f"exit code: {proc.returncode}")
    out.seek(0)
    err.seek(0)
    stdout, stderr = out.read(), err.read()
    print("--- stdout ---")
    print(stdout.strip() or "(empty)")
    print("--- stderr ---")
    print(stderr.strip() or "(empty)")

    # 2 is the cancel path in both binaries; -1073741510 is 0xC000013A, the
    # "terminated by Ctrl-C" status the default handler would give.
    if proc.returncode == 2:
        print("PASS: the build stopped cleanly and reported the cancel.")
        return 0
    if proc.returncode in (-1073741510, 0xC000013A):
        print("FAIL: killed by the default handler, so our handler never ran.")
        return 1
    print(f"FAIL: unexpected exit code {proc.returncode}")
    return 1


if __name__ == "__main__":
    sys.exit(main())
