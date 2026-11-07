# EGM drift report — 2026-11-07 (the residuals, and what the live test found)

Direction: doc-to-code. The 2026-11-06 report is the definitive record of
the EGM structural work, the 0.1.10 pairing and the UI review closure. This
report began with the two items that pass deliberately left open — the
legacy Windows console paste path and the two per-operation write threads
in the resident launcher — and grew into the record of the live console
test that followed: a real defect the test found in `poll`, a causal claim
the owner's measurements corrected, the probe as an installed command and
CI gate, and the final end-to-end measurement. All of it is closed now.
`DRIFT_STATUS.md` names this report; `unresolved: 0` means no drift
finding and no verification gap is outstanding. What remains in these
sections is a **platform limit** — a legacy conhost in raw mode cannot
paste at all — which is the console's design, measured and recorded, not
an unaddressed finding.

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

  **Confirmed at a real keyboard, last, and this is the measurement the
  whole pass was for.** The owner of the machine ran the installed probe in
  Windows Terminal with `alpha` + line break + `beta` on the clipboard: ten
  events in two milliseconds, `Char('\r')` at event six, and no `Enter`
  anywhere in the session — so a pasted two-line block reaches the field as
  text and the session is not submitted by a line break. `0x01f7` ->
  `0x03f8` and back to `0x01f7` on the way out. Two earlier real-keyboard
  runs from the same person carry the rest: a pasted command line arrived
  with its tab as `Char('\t')` — outside a burst a tab byte is the Tab key,
  so the burst was recognised — and its eighty-five events arrived within
  seven milliseconds, which is the decoded-key queue rather than one key per
  later keystroke. The defect this work started from and the defect the live
  test found are both closed by that log.
- **Legacy conhost.** It accepts the flag too, which is why the flag alone
  was the wrong test. Measured in the console's own mode: the paste worked
  on `Ctrl+V` and on `Shift+Insert`, and its line break arrived as **Enter**
  — conhost implements no bracketed paste, so there is no burst marker for
  the byte path to consume. `term.rs` therefore asks for the flag **only
  when the console's window class is the pseudoconsole's**
  (`PseudoConsoleWindow`, measured; `ConsoleWindowClass` is conhost), and a
  console it cannot classify keeps the key-record path — the behaviour it
  had before the byte path existed.

  **A hand test at a real keyboard corrected the reason, and the correction
  matters.** The first reading of these measurements blamed the flag: with
  it on, `Ctrl+V` arrived as the key and `Shift+Insert` delivered nothing,
  so the byte path was said to cost conhost its paste shortcuts. A later
  run of the installed probe, at a real keyboard, with raw mode on and **no**
  flag — the path taken now — delivered `Char('v')` and no paste at all.
  The flag was never the cause: **raw mode** is, on either read path, and it
  is right that it is, because conhost pastes through the line discipline a
  raw-mode program turns off (`ENABLE_PROCESSED_INPUT` is one of the bits
  cleared, and it is what makes the console process a chord rather than hand
  the key over). The conclusion — take the byte path only on a pseudoconsole
  — is unchanged and now rests on the real reason. The consequence for the
  user is recorded rather than hidden: a raw-mode terminal UI cannot paste
  in a legacy conhost at all, and no amount of VT-byte consumption would fix
  that.

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

**On unix the same question is asked without a hand, and that is the easier
half.** There is no console mode to read there — the bytes are the
terminal's — so the probe is driven by writing the burst into a
pseudo-terminal: `tools/vtprobe_pty.py` opens a pty, spawns the probe on the
slave side, writes `ESC[200~PASTE1\rPASTE2ESC[201~` into the master and
reads the log back. No clipboard, no keyboard, no window, deterministic, and
the same run on the Linux box over ssh. Measured on 2026-11-07: thirteen
press events within one millisecond, `Char('\r')` for the line break, no
`Enter`, exit 0. That is the shared `poll`/`read`/decoder rule exercised end
to end, which is exactly where the defect the live test found lived — so the
unix driver is a regression check for it, not just a demonstration.

**What the live test could not settle, and how it was settled.** The conhost
paste could not be driven with synthetic keys from the session that built
the probe: the control modifier did not always register in the console's key
records. That was resolved the way this kind of question should be — a hand
test at a real keyboard, by the owner of the machine, with the installed
probe. It answered more than the question asked: under Windows Terminal the
flag is accepted (`0x01f7` -> `0x03f8`), a pasted command line arrived as a
bracketed burst with its tab as `Char('\t')` (outside a burst a tab byte is
the Tab key, so this is the burst being recognised), and the eighty-five
events of that paste arrived within seven milliseconds — the decoded-key
queue, not one key per later keystroke. Under a legacy conhost the byte path
is not taken (`0x01f7` -> `0x01f8`, `vt_bit=false`), and `Ctrl+V` arrived as
`Char('v')` with no paste. So the conhost limit is measured now, and it is
"no paste in a raw-mode program" — see the correction in section 1.

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

`unresolved: 0`. No drift finding is outstanding and no verification gap
remains. The owner ran the installed probe at a real keyboard in Windows
Terminal and in a legacy conhost, and the numbers are recorded in section 1
and in `bins/search-ui/src/EGM.md`; the unix half is the `terminal-paste`
CI gate. The one thing still true is a platform limit, not a gap: a
raw-mode terminal UI cannot paste in a legacy conhost, because conhost
pastes through the line discipline raw mode turns off. It is recorded in
the contract so the next reader does not have to rediscover it.

**Release note.** The tree is paired at 0.1.10 (three numbers, the registry
takes three) and the release is **0.1.10.0** (four, the fix count of the
line). `scripts/bump-version.py 0.1.10.0` moved the installers to
`rgy-v0.1.10.0` and left the crate manifests untouched, as designed.
`docs/release-0.1.10.0.md` is the release page, with the changelog and the
assets marker for the twelve archives; `scripts/lib/version.sh` resolves it;
`README.md` and `MANUAL.md` name this release. The package-manager manifests
travel with it: the three `.winget/` files, `Formula/rgy-search.rb` and its
contract, and the winget test's rationale all moved to 0.1.10.0, with Scoop
and crates.io covered on the page. The upstream rgx's 0.1.10 stays
source-only and its binary assets stay at `rgx-v0.1.9.0`; that is a
decision about the upstream, not drift in this tree.

**The full release build (same day).** Six of the twelve archives were
built, smoked and digested here — the three x86_64 Windows variants
(portable, AVX2 `-C target-cpu=x86-64-v3`, Zen 4 `-C target-cpu=znver4`)
and the same three for x86_64 Linux glibc. `scripts/build-release.ps1` and
`build-release.sh` produced them into `.dist/` with their 17-file document
digests, and a single canonical `SHA256SUMS` (six rows, LF only) now names
all six; `sha256sum` of every archive matches its row on both hosts.
`publish-release.ps1 -SkipBuild -ManifestsOnly` and
`publish-release.sh --skip-build --manifests-only` both returned 0 with that
`.dist/` and agreed on the two pins available at the time. `.dist/` itself
is gitignored and is not, by design.

**The full release, from the CI run.** With the owner's explicit
authorization, `main` was mirrored to GitHub as a new **orphan commit**
(no parents, the flat mirror the house keeps), which triggered the
`release` matrix in `.github/workflows/ci.yml`. The first run
(`42418859771`) built all twelve archives successfully but failed
`suite on macos-15`: the test
`crates/pick`'s `a_path_that_is_not_utf8_keeps_its_bytes` tried to create
a directory whose name carries byte `0xff`, and macOS refuses it with
`EILSEQ` ("Illegal byte sequence"). That is a platform limit, not a
production defect — `candidate_in` takes `Path` and never rewrites it
through `to_string_lossy()` — so the test now stands
`#[cfg(target_os = "linux")]`, where the bytes are representable, with the
reason written beside it. The fix (commit `1971d7d`) was pushed to Forgejo
and to a fresh GitHub orphan, and the second run (`42419907051`) closed
**`success`, 32/32 jobs, 12/12 release archives green**.

`scripts/fetch-ci-assets.ps1 42419907051` brought the twelve archives and
their twelve 17-file document sidecars into one `.dist/` with a combined
SHA256SUMS; every archive's sha256 matches its row, and every sidecar
carries the tree's release-page hash. Both publishers with
`-SkipBuild`/`--skip-build` returned 0 and agreed on the rewrite: all
seven installer pins, the winget `InstallerSha256` and the Homebrew
formula's two `sha256` values are now real digests, with `winget validate`
passing. No placeholder remains in the release data. The `release` run is
therefore complete on the build side; only the publication act itself —
uploading the assets and creating the tag on the forge and the mirror —
is a separate step the publishers perform.
