#!/usr/bin/env python3
"""Phase-4 FreeBSD VM: start it detached and open the door for SSH.

Boots the official FreeBSD qcow2 image with WHPX acceleration, drives the
serial console - exposed as a TCP socket precisely so the console outlives
this script - to install an ssh key and start sshd, and leaves the VM
running. Everything after this happens over SSH; the phase-4 suite recipe
is: `cargo build --workspace` first, then `cargo test --workspace`.

Usage: python tools/bootstrap_serial.py <qemu-image> <authorized-key-file> [ssh-port]
"""

import os
import re
import socket
import subprocess
import sys
import time

CONSOLE = ("127.0.0.1", 4444)


def uefi_firmware() -> str:
    """The edk2 image shipped next to qemu.

    The official FreeBSD VM images are UEFI/GPT: under the default SeaBIOS
    the firmware never reaches their loader, which looks exactly like what
    we saw - a dead serial console and an idle CPU.
    """
    import shutil

    exe = shutil.which("qemu-system-x86_64") or ""
    cand = os.path.join(os.path.dirname(exe), "share", "edk2-x86_64-code.fd")
    if os.path.exists(cand):
        return cand
    raise RuntimeError(f"UEFI firmware not found next to {exe!r}")


def start_qemu(image: str, ssh_port: int) -> subprocess.Popen:
    """Start the VM detached from this script's lifetime."""
    base = [
        "qemu-system-x86_64",
        "-drive", f"if=pflash,format=raw,readonly=on,file={uefi_firmware()}",
        "-cpu", "qemu64",
        "-smp", "4",
        "-m", "8192",
        "-drive", f"file={image},format=qcow2,if=virtio",
        "-netdev", f"user,id=n0,hostfwd=tcp:127.0.0.1:{ssh_port}-:22",
        "-device", "virtio-net-pci,netdev=n0",
        "-display", "none",
        "-serial", f"tcp:{CONSOLE[0]}:{CONSOLE[1]},server",
        "-monitor", "none",
    ]
    # WHPX is near-native; TCG would make a Rust workspace build crawl,
    # so it is a loud fallback, never a silent one.
    for accel, extra in (("whpx", []), ("tcg", ["-accel", "tcg"])):
        cmd = (base[:1] + ["-accel", accel] + base[1:]) if accel != "tcg" else (base[:1] + extra + base[1:])
        proc = subprocess.Popen(
            cmd,
            stdin=subprocess.DEVNULL,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.PIPE,
            creationflags=subprocess.DETACHED_PROCESS | subprocess.CREATE_NEW_PROCESS_GROUP,
        )
        time.sleep(3)
        if proc.poll() is None:
            if accel == "tcg":
                print("[boot] WARNING: WHPX unavailable, running under TCG (slow)", flush=True)
            return proc
        err = proc.stderr.read().decode(errors="replace")[-500:]
        print(f"[boot] accel={accel} failed: {err}", flush=True)
    raise RuntimeError("qemu did not stay up with whpx or tcg")


def connect_console(timeout: float = 120.0) -> socket.socket:
    deadline = time.time() + timeout
    while time.time() < deadline:
        try:
            s = socket.create_connection(CONSOLE, timeout=5)
            s.settimeout(5)
            return s
        except OSError:
            time.sleep(1)
    raise RuntimeError("serial console never came up")


class Console:
    def __init__(self, sock: socket.socket):
        self.sock = sock
        self.buf = bytearray()

    def expect(self, pattern: bytes, timeout: float) -> re.Match:
        rx = re.compile(pattern)
        deadline = time.time() + timeout
        while time.time() < deadline:
            m = rx.search(bytes(self.buf))
            if m:
                return m
            try:
                chunk = self.sock.recv(4096)
            except socket.timeout:
                continue
            if not chunk:
                raise RuntimeError(f"console closed; tail: {bytes(self.buf[-800:])!r}")
            self.buf += chunk
            if len(self.buf) > 200_000:
                del self.buf[:-1000]
        raise RuntimeError(f"timeout waiting for {pattern!r}; tail: {bytes(self.buf[-800:])!r}")

    def sendline(self, s: str) -> None:
        self.sock.sendall((s + "\n").encode())


def main() -> int:
    image, keyfile = sys.argv[1], sys.argv[2]
    ssh_port = int(sys.argv[3]) if len(sys.argv) > 3 else 2222
    with open(keyfile, "r", encoding="ascii") as f:
        pubkey = f.read().strip()

    # Idempotent: reuse a VM whose console is already up (a hard-killed qemu
    # leaves a dirty filesystem, so reusing beats restarting).
    try:
        con = Console(connect_console(timeout=5))
        print("[boot] reusing the running VM's console", flush=True)
    except RuntimeError:
        print(f"[boot] starting VM from {image}", flush=True)
        start_qemu(image, ssh_port)
        con = Console(connect_console())

    # A fresh TCP connection sees none of what was printed before it
    # attached, and a prompt sitting idle emits nothing - so wake the
    # console first. An empty line is harmless everywhere: getty reprints
    # "login:", and single-user's "RETURN for /bin/sh" starts the shell.
    con.sendline("")
    m = con.expect(rb"login:|Enter full pathname of shell|#", timeout=600)

    def to_root_shell() -> None:
        con.sendline("root")
        m2 = con.expect(rb"(Password:|#)", timeout=120)
        if m2.group(1) == b"Password:":
            con.sendline("")
            con.expect(rb"#", timeout=120)

    def repair_and_reboot() -> None:
        # Auto-fsck failed at boot (a hard-killed qemu left soft updates
        # inconsistent): fix it and come back through a clean boot.
        print("[boot] dirty filesystem: fsck + reboot", flush=True)
        con.sendline("fsck -y /dev/gpt/rootfs")
        con.expect(rb"#", timeout=600)
        con.sendline("reboot")
        con.expect(rb"login:", timeout=600)
        con.sendline("root")
        m2 = con.expect(rb"(Password:|#)", timeout=120)
        if m2.group(1) == b"Password:":
            con.sendline("")
            con.expect(rb"#", timeout=120)

    matched = m.group(0)
    if b"Enter full pathname" in matched:
        con.sendline("")  # accept /bin/sh
        con.expect(rb"#", timeout=120)
        repair_and_reboot()
    elif matched == b"#":
        # A shell is already up - single-user (dirty) or a logged-in root.
        # cron only runs in multi-user. The probe's tokens are built from
        # quoted fragments so the console echo of the command itself can
        # never match them (an unsplit token in the command line does).
        con.sendline('if pgrep -q cron; then echo "ZQ7""-MULTI"; else echo "ZQ7""-SINGLE"; fi')
        m2 = con.expect(rb"ZQ7-(MULTI|SINGLE)", timeout=60)
        if b"ZQ7-SINGLE" in m2.group(0):
            repair_and_reboot()
    else:
        to_root_shell()
    print("[boot] root shell on console", flush=True)

    # The door: an ssh key and a running sshd.
    con.sendline("mkdir -p /root/.ssh && chmod 700 /root/.ssh")
    con.expect(rb"#", timeout=60)
    con.sendline(f"echo '{pubkey}' > /root/.ssh/authorized_keys && chmod 600 /root/.ssh/authorized_keys")
    con.expect(rb"#", timeout=60)
    con.sendline("sysrc sshd_enable=YES && service sshd start")
    con.expect(rb"#", timeout=120)
    print("[boot] sshd up", flush=True)
    return 0


if __name__ == "__main__":
    sys.exit(main())
