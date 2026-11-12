"""Drive the terminal probe through a pseudo-terminal.

The probe (`bins/search-ui/examples/vtprobe.rs`) answers one question: what
does a real terminal deliver, and what does the read loop make of it. On a
unix terminal that question needs no clipboard and no keyboard -- a
pseudo-terminal is a terminal, and the bytes below are exactly the ones a
terminal that implements bracketed paste sends when the user pastes two
lines. Putting them into the master side of the pty drives the product's
own read loop (`poll`, `read`, the shared decoder) end to end, and the
answer comes back as the probe's log.

Usage: `python3 tools/vtprobe_pty.py <path-to-vtprobe>`

A pass is every character of the burst arriving together, and the line
break arriving as `Char('\\r')` rather than as `Enter`. The last line of
this script says which of the two happened and exits non-zero on a fail.
"""

import os
import pty
import subprocess
import sys
import time

LOG = "/tmp/vtprobe-pty.log"
BURST = b"\x1b[200~PASTE1\rPASTE2\x1b[201~"
ESCAPE = b"\x1b"
# The burst is thirteen characters, and the Escape that ends the probe is a
# press of its own: the count below is of every press in the log.
EXPECTED_PRESSES = 14


def drive(probe: str) -> int:
    if os.path.exists(LOG):
        os.remove(LOG)

    master, slave = pty.openpty()
    child = subprocess.Popen(
        [probe, LOG, "20"],
        stdin=slave,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    os.close(slave)
    # The probe has to reach raw mode before the burst means anything.
    time.sleep(0.8)
    os.write(master, BURST)
    time.sleep(1.2)
    os.write(master, ESCAPE)
    try:
        child.wait(timeout=25)
    except subprocess.TimeoutExpired:
        child.kill()
        print("PROBE_TIMEOUT")

    with open(LOG, encoding="utf-8", errors="replace") as handle:
        text = handle.read()
    print(text)

    presses = text.count("kind: Press")
    carriage_return_as_char = "Char('\\r')" in text
    saw_enter = "code: Enter" in text
    print("---")
    print(
        f"press_events={presses} "
        f"carriage_return_as_char={carriage_return_as_char} "
        f"saw_enter={saw_enter}"
    )
    passed = (
        presses >= EXPECTED_PRESSES and carriage_return_as_char and not saw_enter
    )
    print(
        "PASS: the burst arrived whole, its line break as a character"
        if passed
        else "FAIL: see the log above"
    )
    return 0 if passed else 1


def main() -> int:
    if len(sys.argv) < 2:
        print(__doc__)
        return 2
    return drive(sys.argv[1])


if __name__ == "__main__":
    raise SystemExit(main())
