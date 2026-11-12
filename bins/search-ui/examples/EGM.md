# bins/search-ui/examples

## Overview
One program, and it is an instrument rather than a demonstration:
`vtprobe.rs` is the console probe for the keyboard paths. The read loop's
rules are unit-tested with simulated bytes, but the two questions that
decide *which* loop runs can only be answered in a real console — does
`SetConsoleMode` accept `ENABLE_VIRTUAL_TERMINAL_INPUT`, and does a real
paste arrive as a bracketed burst whose `\r` is a character? The probe
drives the same seam the terminal UI drives ([src/term.rs](../src/EGM.md):
`enable_raw_mode`, `Screen::enable_bracketed_paste`, `poll`, `read`) and
writes what it saw to a log, so the answer is readable after the window is
closed. It exists because a platform behaviour that no test can reach is a
behaviour nobody has checked.

It runs on both platforms, because only the *questions* differ. On Windows
the console answers them with a class and a bit, and the probe reports
both. On a unix terminal there is no console mode to read: the bytes are
the terminal's, so the probe is driven by writing those bytes into a
pseudo-terminal — `tools/vtprobe_pty.py` does exactly that, which makes the
unix check fully automated: no clipboard, no keyboard, no window, and the
same run on a box reached over ssh. That is the easier half, and it is the
half that exercises the shared `poll`/`read`/decoder rule end to end.

## Interface
- `[vtprobe.rs:main]` — the entry point; it hands off to the probe on every
  platform.
- `[vtprobe.rs:probe::run]` — the probe: puts the terminal in raw mode,
  enables bracketed paste, then reports every event until `Escape` or the
  timeout. First argument is the log path (default `<temp>/vtprobe.log`),
  second the timeout in seconds (default 60).
- `[vtprobe.rs:probe::win::console_class]` — the class of the window the
  console reports (Windows only).
- `[vtprobe.rs:probe::win::input_mode]`, `[vtprobe.rs:probe::win::describe_mode]`
  — the console's input mode and the words the decision is made in
  (Windows only).
- `[vtprobe.rs:probe::win::claim_foreground]` — the console window's
  keyboard claim, so a backgrounded probe still receives the test's keys
  (Windows only).
- `[vtprobe.rs:probe::win::ENABLE_VIRTUAL_TERMINAL_INPUT]`,
  `[vtprobe.rs:probe::win::ENABLE_WINDOW_INPUT]` — the two mode bits
  (Windows only).

## Invariants
- The probe is installed as a command, not built per test:
  `cargo install --path bins/search-ui --example vtprobe --force` puts
  `vtprobe` in cargo's bin directory, which is on `PATH`. Measured on
  2026-11-07: installed to `~/.cargo/bin/vtprobe.exe`, and both consoles
  reached from the same installed binary.
- Either console can be reached without changing the machine's default
  terminal. A Windows Terminal tab runs `vtprobe <log> <seconds>`
  directly; the legacy host is reached by asking `conhost.exe` to host it,
  `conhost.exe cmd /c vtprobe <log> <seconds>`. Measured on 2026-11-07:
  the first reports `PseudoConsoleWindow` with `0x01f7` -> `0x03f8`
  (`vt_bit=true`), the second reports `ConsoleWindowClass` with `0x01f7` ->
  `0x01f8` (`vt_bit=false`). The probe's own console class is therefore
  the whole answer to "which path will this console take", and no registry
  change is needed to ask it.
- The console's class is the decision `enable_raw_mode` makes: a
  `PseudoConsoleWindow` is a terminal that implements bracketed paste
  (Windows Terminal today) and takes the VT byte path;
  `ConsoleWindowClass` is the legacy host, which keeps the key-record path.
  Measured on 2026-11-07 in three states, which is why the reason is not
  the flag: in conhost's own mode the paste worked on `Ctrl+V` and on
  `Shift+Insert` and its line break arrived as `Enter`; with raw mode on
  and the flag accepted, `Ctrl+V` arrived as the key with the control
  modifier and `Shift+Insert` delivered nothing; with raw mode on and no
  flag, `Ctrl+V` arrived as `Char('v')` and nothing pasted. Raw mode is
  what costs conhost its paste, on either read path, because conhost is
  right that paste belongs to the line discipline the program turned off.
  The probe reports the class so the mode it logs can be read against the
  console it belongs to.
- The mode is logged before raw mode, after it, and after the console is
  given back: the first and last lines are the proof that
  `disable_raw_mode` restored exactly what it changed (`0x01f7` ->
  `0x03f8` -> `0x01f7` on the pseudoconsole, `0x01f7` -> `0x01f8` ->
  `0x01f7` on conhost, measured on 2026-11-07).
- A passing paste on the pseudoconsole shows `Char('...')` for every
  character and `Char('\r')` for a pasted line break; an `Enter` in that
  place is the pre-fix behaviour. Measured on 2026-11-07 at a real keyboard:
  a pasted command line arrived as a bracketed burst with its tab as
  `Char('\t')` — outside a burst a tab byte is the Tab key — and a pasted
  two-line block (`alpha`, line break, `beta`) delivered ten events in two
  milliseconds with `Char('\r')` at the sixth and no `Enter` in the session.
  On conhost no paste arrives at all, so there is no line break to judge.
- The recipe that exercises the line break needs the clipboard prepared:
  `Set-Clipboard "alpha`nbeta"` before running the probe. A one-line
  clipboard proves the burst but never the `\r`.
- On unix a passing run is the same shape, and it is the one the
  pseudo-terminal driver asserts: every character of the burst arriving
  together, `Char('\r')` for the line break, no `Enter`, and the exit code
  of `tools/vtprobe_pty.py` carrying the verdict. Measured on 2026-11-07 on
  the Linux box: thirteen press events within one millisecond and
  `carriage_return_as_char=True`, `saw_enter=False`.
- Every key the probe sees is printed as the `Debug` of the shared
  `Event`, so the log is the same vocabulary the tests and the drain loop
  speak.

## Side Effects
- `[vtprobe.rs:probe::run]` puts the console into raw mode, claims the
  foreground and enables bracketed paste, then gives the console back
  before it returns; it writes the log file in the temp folder and the
  same lines to stdout. The foreground claim is what makes the probe
  usable from a launcher that starts it in the background: the keys of the
  test must land in this console, not in the shell that started it.
- `[vtprobe.rs:probe::input_mode]` reads the console's mode through
  `GetStdHandle` and `GetConsoleMode`; `[vtprobe.rs:probe::console_class]`
  reads the console's window class through `GetConsoleWindow` and
  `GetClassNameW` (user32).

## Verification
- Run: `cargo test --workspace --no-fail-fast --locked`
- Install it once with
  `cargo install --path bins/search-ui --example vtprobe --force`.
- Windows, by hand: `vtprobe <log> <seconds>` in a Windows Terminal tab, or
  `conhost.exe cmd /c vtprobe <log> <seconds>` for the legacy host. Press
  `Ctrl+V`, then `Escape`, and read the log.
- unix, automated: `python3 tools/vtprobe_pty.py target/debug/examples/vtprobe`
  writes the burst into a pseudo-terminal and exits non-zero on a fail, so
  the check needs no paste, no window and nobody watching. The `terminal-paste`
  job in `.github/workflows/ci.yml` runs exactly this on `ubuntu-24.04` and
  `macos-15`: the one paste check that is a gate rather than a procedure,
  and the regression gate for the decoded-key queue. The Windows halves are
  not there — they need a real console and a real paste, and are recorded
  with their measurements in `.egm/2026-11-07_DRIFT_REPORT.md`.
