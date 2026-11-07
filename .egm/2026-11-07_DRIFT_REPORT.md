# EGM drift report — 2026-11-07 (the two residuals)

Direction: doc-to-code. The 2026-11-06 report is the definitive record of
the EGM structural work, the 0.1.10 pairing and the UI review closure. **This
report carries only the two items that the previous pass deliberately left
open**: the legacy Windows console paste path and the two per-operation
write threads in the resident launcher. Both are now closed in code, with
regression tests. `DRIFT_STATUS.md` names this report; `unresolved: 0`
means no drift finding is outstanding. The one remaining limit is a
platform/verification limit, recorded below and in `bins/search-ui/src/EGM.md`,
not an unaddressed finding.

## 1. Windows console paste (conhost)

Microsoft's `SetConsoleMode` documentation is the authority for the mode
semantics used here: `ENABLE_VIRTUAL_TERMINAL_INPUT` (`0x0200`) makes the
console convert user input into Console Virtual Terminal Sequences that
`ReadFile` / `ReadConsole` can retrieve, and it has no effect on
`ReadConsoleInput`. `ENABLE_WINDOW_INPUT` records, by contrast, are filtered
out of `ReadFile`, so a resize on the byte path has to come from the existing
bounded `RESIZE_TICK` + `size()` comparison rather than from input records.

`bins/search-ui/src/term.rs` (Windows `imp` only) now:

- tries `ENABLE_VIRTUAL_TERMINAL_INPUT` in `enable_raw_mode()` and retries
  without it when the console refuses the flag. `VT_INPUT` records which
  path is live; the original input and output modes are still saved and
  restored exactly by `disable_raw_mode()`, which also resets `VT_INPUT`,
  the pending-byte buffer and the decoder, so a half-read paste cannot leak
  into a new session.
- reads console bytes with `ReadFile` into the same persistent `Decoder`
  that the unix loop uses. `ESC[200~ ... ESC[201~` therefore delivers `\r`
  and `\n` as `KeyCode::Char` (never Enter), including when the burst is
  split across reads. A lone `ESC` still resolves to the Escape key through
  a zero-timeout `WaitForSingleObject` readiness check, mirroring the unix
  `ready(fd)` call — an always-true `more_may_come` would have left a lone
  escape permanently `Incomplete`.
- keeps the entire `ReadConsoleInputW` key-record path, its surrogate
  handling and its oracle test byte-for-byte unchanged as the fallback for
  consoles that refuse the flag. `read()` dispatches on `VT_INPUT`; the
  public API is unchanged.

Six Windows-only unit tests cover the paste burst, `\r` outside a burst,
a paste split across reads, the mode-bit computation, the refused-flag
fallback selection and the state reset. `bins/search-ui/src/EGM.md` was
rewritten: the old "Windows console input is a platform limit" invariant
now describes the VT byte path, the fallback and the verification limit.

**Verified live on 2026-11-07, and the live test changed the design.** A
throwaway probe drove the same seam the TUI drives (raw mode, bracketed
paste, `poll`, `read`) inside a real console window, with a two-line block
on the clipboard and the paste chord sent through `keybd_event`:

- **Windows Terminal (the pseudoconsole path).** `SetConsoleMode` accepted
  `ENABLE_VIRTUAL_TERMINAL_INPUT` (input mode `0x01f7` -> `0x03f8`), a real
  `Ctrl+V` paste arrived as a bracketed burst, and the whole block reached
  the field as characters — including `Char('\r')` for the line break —
  within two milliseconds of the paste. `disable_raw_mode` restored
  `0x01f7` exactly.
- **Legacy conhost.** It accepts the flag too, which is why the flag alone
  was the wrong test. Measured with the flag on: `Ctrl+V` arrived as the
  key rather than pasting, and `Shift+Insert` delivered nothing. Measured in
  the console's own mode: the paste worked on both chords and its line break
  arrived as **Enter** — conhost implements no bracketed paste, so there is
  no burst marker for the byte path to consume. The byte path therefore buys
  nothing on conhost and costs it its paste shortcuts, so `term.rs` now asks
  for the flag **only when the console's window class is the pseudoconsole's**
  (`PseudoConsoleWindow`, measured; `ConsoleWindowClass` is conhost). A
  console it cannot classify keeps the key-record path — the behaviour it
  had before the byte path existed.

**The instrument is in the tree now.** The probe that produced these
measurements started as a throwaway and is tracked as
`bins/search-ui/examples/vtprobe.rs`, with its own contract
(`bins/search-ui/examples/EGM.md`, the 52nd). That is deliberate: the two
questions it answers — whether the console accepts the flag, and what a real
paste delivers — are platform answers no unit test can give, so the
instrument that asks them has to outlive the session that built it. It
claims the foreground so it can be started in the background, logs the
console's class with every mode it reads, and needs nothing but a paste and
an `Escape`.

It is installed as a command rather than built per test:
`cargo install --path bins/search-ui --example vtprobe --force` puts
`vtprobe` on `PATH`, and both consoles are then reachable from the same
binary without touching the machine's default terminal — `vtprobe` in a
Windows Terminal tab, and `conhost.exe cmd /c vtprobe` for the legacy host.
Measured on 2026-11-07 with the installed binary: the first reports
`PseudoConsoleWindow` and `0x01f7` -> `0x03f8` (`vt_bit=true`), the second
reports `ConsoleWindowClass` and `0x01f7` -> `0x01f8` (`vt_bit=false`). The
delegation-key change the first measurement used is not needed for a
reproduction, which is the better outcome: a test that edits `HKCU` is a
test that can be left half-applied.

**What the live test could not settle, stated honestly.** The conhost paste
could not be driven reliably with synthetic keys: the control modifier did
not always register in the console's key records, so the flag-off conhost
paste was measured in the console's default mode rather than under raw mode.
The conhost half of the limit therefore rests on the console's documented
lack of bracketed paste — and on the measured loss of its paste shortcuts —
rather than on a measured raw-mode paste. A hand test at a real conhost
keyboard is still the way to close that sentence.

## 1a. The live test found a real defect: `poll` could not see decoded keys

The manual test was not a formality. The first live run delivered **only the
first character** of the pasted block and held the remaining twelve until
the next keypress. The cause was not the console: `read()` decodes a whole
chunk of bytes but returns one event, and the events it had already produced
from the rest of the chunk were kept in the program's own pending buffer.
`poll()` asked the *platform* whether more input was waiting, and the
platform truthfully said no. The terminal UI's drain loop polls before every
read (`bins/search-ui/src/ui.rs`), so it took the first key of a paste and
went back to waiting.

Both platforms had it, not just Windows: a unix read of a 64-byte chunk can
hold several keys the same way. The fix is a queue of already-decoded events
in `term.rs`: both read loops drain their pending bytes into it with one
shared rule (`drain_ready`), `poll` answers from it before it asks the
platform, and `disable_raw_mode` clears it. Five tests state the rule: keys
in one run come out in order, a bracketed burst keeps its `\r` as a
character, a cut sequence waits while more may come and is dropped when none
will, and the queue is what `poll` reads.

Verified by the same live probe, before and after: before, one event at the
paste and twelve at the next keypress; after, thirteen events within two
milliseconds. The Windows-only simulated-byte tests now call the production
rule (`decode_vt` was folded into `drain_ready`, so there is one
implementation and the tests exercise it).

## 2. Per-operation write threads

`bins/rgxy/src/gui.rs`: `record_use` and `save_settings` each spawned one
`std::thread::spawn` per use / per settings key. Both closures capture the
whole of their state (`History` and `Settings`), so the newest job is the
only one whose state must land. They now go through `WRITES`, a
`BoundedPool` with **one** worker and a bounded queue of 16: no per-operation
thread, no unbounded backlog, and one worker keeps the writes in
submission order (a stale job cannot overwrite a newer one after it). The
existing stat/reader/preview pools and the single stderr writer are
unchanged. `bins/rgxy/src/EGM.md` records the pool and the reasoning.
`BoundedPool` gained a `#[cfg(test)] worker_count()` seam; two regression
tests hold the pool to its bound and prove the newest whole-state settings
write lands last.

## 3. Contracts and derived state

Two contracts were edited — `bins/search-ui/src/EGM.md` and
`bins/rgxy/src/EGM.md` — and `CONTRACTS.index` was regenerated in the same
commit that carries this report, so the derived hashes and the reviewed
prose travel together. `AGENTS.md` was updated to name this pass. The
2026-11-06 report's section 10 is left as the record of the fourteen
findings; this report is the next dated entry in the trail.

## 4. Verification

Windows: `cargo fmt --all -- --check`, `cargo clippy --workspace
--all-targets -- -D warnings`, `cargo build --workspace`,
`cargo test --workspace --no-fail-fast` (82 targets, 0 failed; `rgxy` 179
unit tests and `rgy-search-ui` 85 + 15 oracle tests), the bare-metal
`x86_64-unknown-none` build, `bench-names.py check --runs 7`,
`check-vendoring.ps1`, and the checker `51/0`, `--generated-check` `51/0`
and a 55-case self-test. The CachyOS box re-runs the same suite under
rustfmt 1.9.0, including `cargo check`/clippy for `x86_64-unknown-freebsd`,
`x86_64-apple-darwin` and `aarch64-apple-darwin`, `check-twins.sh` (13
pairs) and `check-vendoring.sh`. `CONTRACTS.index` is compared byte-for-byte
on both hosts before the commit.

## 5. Status

`unresolved: 0`. No drift finding is outstanding. The live-console paste
session is a verification gap to close when a real Windows console is
available, recorded in the contract so the next reader does not have to
rediscover it.
