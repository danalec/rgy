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

It is Windows-only on purpose: there is no console mode to read anywhere
else, so the non-Windows `main` says so and stops.

## Interface
- `[vtprobe.rs:main]` — the platform's entry point; on Windows it hands off
  to the probe, elsewhere it prints that there is no console mode to read.
- `[vtprobe.rs:probe::run]` — the probe: puts the console in raw mode,
  enables bracketed paste, then reports every event until `Escape` or the
  timeout. First argument is the log path (default
  `<temp>/vtprobe.log`), second the timeout in seconds (default 60).
- `[vtprobe.rs:probe::console_class]` — the class of the window the console
  reports.
- `[vtprobe.rs:probe::input_mode]`, `[vtprobe.rs:probe::describe_mode]` —
  the console's input mode and the words the decision is made in.
- `[vtprobe.rs:probe::ENABLE_VIRTUAL_TERMINAL_INPUT]`,
  `[vtprobe.rs:probe::ENABLE_WINDOW_INPUT]` — the two mode bits.

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
  `ConsoleWindowClass` is the legacy host, which keeps the key-record path
  and its own paste shortcuts. Measured on 2026-11-07: conhost accepts the
  mode flag too, but with it on `Ctrl+V` arrives as the key and
  `Shift+Insert` delivers nothing, and conhost has no bracketed paste for
  the byte path to consume. The probe reports the class so the mode it
  logs can be read against the console it belongs to.
- The mode is logged before raw mode, after it, and after the console is
  given back: the first and last lines are the proof that
  `disable_raw_mode` restored exactly what it changed (`0x01f7` ->
  `0x03f8` -> `0x01f7` on the pseudoconsole, `0x01f7` -> `0x01f8` ->
  `0x01f7` on conhost, measured on 2026-11-07).
- A passing paste on the pseudoconsole shows `Char('...')` for every
  character and `Char('\r')` for a pasted line break; an `Enter` in that
  place is the pre-fix behaviour. On conhost the line break is expected to
  arrive as `Enter`, because the console marks nothing.
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
- The probe is the answer to a question no test can ask, so it is run by
  hand. Install it once with
  `cargo install --path bins/search-ui --example vtprobe --force`; then
  `vtprobe <log> <seconds>` in a Windows Terminal tab, or
  `conhost.exe cmd /c vtprobe <log> <seconds>` for the legacy host. Press
  `Ctrl+V`, then `Escape`, and read the log.
