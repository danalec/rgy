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

**Verification limit, stated honestly.** Verified here: compilation and
linking on Windows MSVC (including that a bare `extern "system" ReadFile`
resolves without an explicit `#[link]`), clippy `-D warnings`, the Linux
cross-target check, rustfmt, and unit tests with simulated VT bytes. **Not
verified here: a live conhost / Windows Terminal paste session.** Whether a
given conhost accepts the flag, whether `ReadFile` emits the paste markers
for a real paste, and the live resize/key interaction on the byte path need
a session with a real console. The fallback for a console that refuses the
flag remains the documented limit. This report does not claim a live-console
proof; it claims the read loop now consumes VT bytes when the console
supplies them.

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
