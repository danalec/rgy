#!/usr/bin/env python3
"""Run one command on the FreeBSD VM's serial console and print the result.

The console is a TCP socket (`bootstrap_serial.py`'s side channel); this
talks to a VM that is already up. Output capture is idle-based (three
seconds of silence) because guest prompts vary - the answer is printed
raw so the caller judges.

Usage: python tools/console_cmd.py "<command>"
"""

import socket
import sys
import time

CONSOLE = ("127.0.0.1", 4444)


def main() -> int:
    cmd = sys.argv[1]
    s = socket.create_connection(CONSOLE, timeout=5)
    s.settimeout(1)
    # Wake whatever prompt is sitting idle: a fresh connection never sees
    # what was printed before it attached.
    s.sendall(b"\n")
    time.sleep(1.5)
    try:
        while True:
            s.recv(65536)
    except socket.timeout:
        pass
    s.sendall((cmd + "\n").encode())
    out = bytearray()
    idle = time.time()
    while time.time() - idle < 3:
        try:
            chunk = s.recv(65536)
        except socket.timeout:
            continue
        if not chunk:
            break
        out += chunk
        idle = time.time()
    sys.stdout.write(out.decode(errors="replace"))
    return 0


if __name__ == "__main__":
    sys.exit(main())
