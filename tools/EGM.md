# tools

## Overview
Eight standalone Python probe / benchmark / smoke helpers. They are not a
library and share no package: each is a script run directly. The set covers
Windows console Ctrl-C probing (`interrupt_probe.py`), FreeBSD VM serial
console boot and command dispatch (`bootstrap_serial.py`, `console_cmd.py`),
cold-vs-warm name-index benchmarks and calibration (`phase1_bench.py`,
`calibrate_bench.py`), smoke tests of the launcher and the daemon's second
layer (`smoke_rgxy.py`, `smoke_second_layer.py`), and the pseudo-terminal
driver for the terminal probe (`vtprobe_pty.py`). They spawn binaries and
VMs, open sockets and write temporary files; nothing here is imported by the
product.

## Interface
- `interrupt_probe.py`: `main()` and `CTRL_BREAK_EVENT = 1` —
  `[interrupt_probe.py:CTRL_BREAK_EVENT]`, `[interrupt_probe.py:main()]`.
- `console_cmd.py`: `main()` and `CONSOLE = ("127.0.0.1", 4444)` —
  `[console_cmd.py:CONSOLE]`, `[console_cmd.py:main()]`.
- `bootstrap_serial.py`: `Console` (with `expect()` and `sendline()`),
  `uefi_firmware()`, `start_qemu()`, `connect_console()`, `main()` —
  `[bootstrap_serial.py:Console]`, `[bootstrap_serial.py:start_qemu()]`, `[bootstrap_serial.py:connect_console()]`.
- `calibrate_bench.py`: `count()`, `BIN`, `CANDIDATES` —
  `[calibrate_bench.py:count()]`, `[calibrate_bench.py:BIN]`.
- `phase1_bench.py`: `run()`, `stat()`, `BIN`, `ENTRIES`, `REUSED`,
  `COLD_RUNS`, `WARM_RUNS` — `[phase1_bench.py:ENTRIES]`, `[phase1_bench.py:REUSED]`, `[phase1_bench.py:stat()]`.
- `smoke_rgxy.py`: `ask()`, `pipe`, `server`, `launcher` —
  `[smoke_rgxy.py:ask()]`.
- `smoke_second_layer.py`: `requests` (the `search` / `grep-scoped` JSON op
  list) and `payload` — `[smoke_second_layer.py:requests]`.
- `vtprobe_pty.py`: `drive()`, `main()`, `LOG`, `BURST`, `ESCAPE`,
  `EXPECTED_PRESSES` — `[vtprobe_pty.py:drive()]`, `[vtprobe_pty.py:BURST]`.

## Invariants
- `interrupt_probe.py` treats exit code `2` as the clean cancel and
  `0xC000013A` as the default handler (a failed handler) —
  `[interrupt_probe.py:proc.returncode == 2]`.
- `console_cmd.py` captures output by idle (three seconds of silence), not by
  a prompt — `[console_cmd.py:main()]`.
- `bootstrap_serial.py` exposes the serial console as a TCP socket on
  `127.0.0.1:4444` so it outlives the script, prefers WHPX and only falls
  back to TCG with a loud warning — `[bootstrap_serial.py:CONSOLE]`, `[bootstrap_serial.py:start_qemu()]`.
- `phase1_bench.py` uses `--reindex` for cold runs, requires an index-reuse
  line (`REUSED`) or exits, and compares cold/warm medians —
  `[phase1_bench.py:--reindex]`, `[phase1_bench.py:REUSED]`.
- `calibrate_bench.py` caps a tree count at `400_000` entries so a huge tree
  costs little — `[calibrate_bench.py:count()]`.
- `smoke_rgxy.py` starts its own daemon on a private named pipe so it does not
  talk to the developer's own daemon — `[smoke_rgxy.py:pipe]`.
- Each smoke test removes the temporary tree it created —
  `[smoke_second_layer.py:shutil.rmtree()]`.
- `vtprobe_pty.py` writes the bracketed-paste burst into the master side of
  a pseudo-terminal and reads the probe's log back: a pass is every
  character of the burst arriving together with the line break as
  `Char('\\r')` and no `Enter` at all, and the script's last line says
  which happened and its exit code carries the answer —
  `[vtprobe_pty.py:BURST]`, `[vtprobe_pty.py:EXPECTED_PRESSES]`. The
  `terminal-paste` job in `.github/workflows/ci.yml` runs this on
  `ubuntu-24.04` and `macos-15`, so the check is a gate rather than a
  procedure; the Windows halves of the same question need a real console
  and a real paste, and stay recorded with their measurements.

## Side Effects
- `interrupt_probe.py` spawns the binary under `CREATE_NEW_PROCESS_GROUP`,
  sets `RGY_INDEX_DIR` to a temp dir, writes `.out` / `.err` temp files and
  calls `kernel32.GenerateConsoleCtrlEvent` —
  `[interrupt_probe.py:GenerateConsoleCtrlEvent]`.
- `console_cmd.py` opens a TCP socket to `CONSOLE` and sends the command —
  `[console_cmd.py:CONSOLE]`.
- `bootstrap_serial.py` starts `qemu-system-x86_64` detached, writes
  `/root/.ssh/authorized_keys` and runs `sysrc sshd_enable=YES && service
  sshd start` on the guest — `[bootstrap_serial.py:start_qemu()]`, `[bootstrap_serial.py:main()]`.
- `calibrate_bench.py` unlinks index files in `TEMP` and runs
  `searchctl.exe` — `[calibrate_bench.py:BIN]`.
- `phase1_bench.py` unlinks `INDEX` and runs `searchctl.exe` with
  `--reindex` — `[phase1_bench.py:INDEX]`.
- `smoke_rgxy.py` creates temp files, spawns `searchd` on `pipe:<name>`,
  terminates it and removes the tree — `[smoke_rgxy.py:server]`.
- `smoke_second_layer.py` pipes JSON request lines into `searchd` and removes
  the tree — `[smoke_second_layer.py:payload]`.
- `vtprobe_pty.py` opens a pseudo-terminal, spawns the probe on its slave
  side, writes the burst into the master and waits for the probe to end —
  `[vtprobe_pty.py:drive()]`.

## Verification
- Run: `cargo test --workspace --no-fail-fast --locked`
