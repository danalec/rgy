# bins/search-ui/src

## Overview

Source of the `rgy-search-ui` library: the terminal search UI transported from
zen-rs's `ui.rs` ([lib.rs:pub mod term], [lib.rs:pub use ui::*]). It renders an
interactive full-screen search over an in-memory index, with a latency
sparkline and percentiles ([ui.rs:struct App],
[ui.rs:use rgy_launcher::latency::{LatencyMeter, duration}]) and uses the
launcher's command and key vocabulary ([ui.rs:use rgy_launcher::Action as LauncherAction],
[ui.rs:use rgy_name_core::live::Live],
[ui.rs:use rgy_name_core::name_query::{Alternatives, Hit, Searcher}]).
`term.rs` is the tree's own terminal seam: escape sequences, key decoding and
raw mode declared by hand rather than through `crossterm`
([term.rs:pub const RESET], [term.rs:pub enum KeyCode],
[term.rs:pub fn enable_raw_mode], [term.rs:pub struct Decoder]), with a unix
half and a Windows half ([term.rs:mod imp] under `#[cfg(unix)]` and
`#[cfg(windows)]`). The decoder is stateful across reads so a bracketed paste
can be split, and both halves wait on a bounded tick that notices a resize
while idle. `preview.rs` reads the head of a file for the preview panel
([preview.rs:pub fn of], [preview.rs:use rgy_launcher::Preview]).
`runner.rs` is the seam the caller fills: [runner.rs:pub trait Runner] opens
a result, opens its folder, takes and gives the clipboard, and reads and
writes the settings; [runner.rs:pub struct NoRunner] keeps the older
descriptive notices when no machine was handed over. With a runner the
terminal has an interactive actions menu ([ui.rs:struct ActionMenu]), an
editable settings panel ([ui.rs:enum Setting]) and real action outcomes, so
its options match the window's.

## Interface
- [lib.rs:pub mod term] and [lib.rs:pub use ui::*] — the crate's public surface; [lib.rs:mod preview] and [lib.rs:mod ui] are private.
- [ui.rs:pub fn run], [ui.rs:pub fn run_with_lang], [ui.rs:pub fn run_with_source], [ui.rs:pub fn run_with_source_and_lang], [ui.rs:pub fn run_with_source_lang_and_theme] — interactive search entry points over a `Live` index or a boxed `rgy_launcher::Source` trait object; [ui.rs:pub fn run_with_source_lang_theme_and_runner] adds a boxed [runner.rs:pub trait Runner].
- [runner.rs:pub trait Runner] (`available`, `open`, `open_folder`, `copy_path`, `paste_text`, `settings`, `save_settings`) and [runner.rs:pub struct NoRunner] — the action seam; re-exported by [lib.rs:pub use runner::{NoRunner, Runner}].
- [term.rs:pub struct Screen] (generic over a `Write` output) and its method impl: `new`, `move_to`, `hide`, `show`, `clear_all`, `clear_line`, `enter_alternate_screen`, `leave_alternate_screen`, `enable_bracketed_paste`, `disable_bracketed_paste`, `background`, `foreground`, `reset_colour`, `print`, `flush`.
- [term.rs:pub trait Paint] with [term.rs:impl Paint for &str], [term.rs:impl Paint for String], [term.rs:impl Paint for char] and [term.rs:impl Paint for Styled]; [term.rs:pub struct Styled]; [term.rs:pub enum Color].
- [term.rs:pub enum KeyCode], [term.rs:pub struct KeyModifiers] (with `NONE`, `SHIFT`, `CONTROL`, `ALT`, `contains`, `union`), [term.rs:pub enum KeyEventKind], [term.rs:pub enum Event] (including `Resize`), [term.rs:pub struct KeyEvent] with [term.rs:pub fn new], [term.rs:pub enum Decoded], [term.rs:pub struct Decoder] and [term.rs:pub fn decode].
- [term.rs:pub fn enable_raw_mode], [term.rs:pub fn disable_raw_mode], [term.rs:pub fn size], [term.rs:pub fn poll], [term.rs:pub fn read] — the platform-facing terminal functions.
- [preview.rs:pub fn of] — the head-of-file preview.
- [ui.rs:mod palette] with [ui.rs:pub fn truecolor] and [ui.rs:pub fn no_colour].

## Invariants
- [term.rs:pub const RESET] is `\x1b[0m`; [term.rs:pub fn decode], [term.rs:fn decode_char], [term.rs:fn decode_escape], [term.rs:fn decode_ss3] and [term.rs:fn decode_csi] decode the bytes the platform gives into one `KeyEvent`. A truncated multibyte run with no more bytes coming is dropped as `Decoded::Unknown`, not `Incomplete`: waiting for a byte that will never arrive would freeze `read()`.
- [term.rs:pub struct Decoder] carries the bracketed-paste state across reads. Inside `ESC[200~ ... ESC[201~` every byte, including `\r` and `\n`, is a `KeyCode::Char` for the field, never `Enter`; outside a burst `\r` is still Enter, and a genuinely unknown CSI sequence is still dropped. Both read loops hold one persistent decoder each, so a paste split across reads still arrives whole.
- [term.rs:pub fn enable_raw_mode] and [term.rs:pub fn disable_raw_mode] bracket the raw mode; both read loops wait on a bounded tick and emit `Event::Resize` when `size()` changes, so a resize while idle repaints instead of waiting for the next key. [ui.rs:impl Drop for TerminalGuard] restores colours, cursor, alternate screen, bracketed paste and raw mode on panic or early error.
- A key the decoder has already produced is a key waiting: [term.rs:static READY] holds the events the decoder made from bytes already read, [term.rs:fn drain_ready] fills it, and [term.rs:pub fn poll] answers from it before it asks the platform. Without that, `poll` asked the terminal whether more *bytes* were waiting — and a run that arrived whole had already been read, so its second key was in this program and invisible: a caller that polls before it reads delivered the first key of a paste and held the rest until the next keypress. Both reads drain their pending bytes into the queue, so the queue holds only whole events and the pending buffer holds only a token that is still cut short.
- Windows consumes console VT bytes only on a pseudoconsole: [term.rs:mod imp] (Windows) asks for `ENABLE_VIRTUAL_TERMINAL_INPUT` (`0x0200`) when the console's window class is the pseudoconsole's, and then reads the VT stream with `ReadFile` through the SAME [term.rs:pub struct Decoder], so a bracketed paste — even one split across reads — delivers `\r` as `KeyCode::Char` and never ends the session. `disable_raw_mode()` restores both original modes exactly and resets the VT byte state. A legacy conhost is deliberately left on the existing `ReadConsoleInputW` key-record path: it accepts the flag but implements no bracketed paste, so the byte path has nothing to consume there. Measured 2026-11-07 with the console probe, in three states: in the console's own mode the paste worked on `Ctrl+V` and on `Shift+Insert`, and its line break arrived as the enter key; with raw mode on and the VT flag accepted, `Ctrl+V` arrived as the key with the control modifier and `Shift+Insert` delivered nothing; with raw mode on and no flag — the path taken now — `Ctrl+V` arrived as `Char('v')` and nothing pasted at all. The flag was therefore never what cost conhost its paste: **raw mode** does, on either read path, because conhost pastes through the line discipline that raw mode turns off. The consequence is recorded rather than hidden: a raw-mode terminal UI cannot paste in a legacy conhost, which is the console's design and not something consuming VT bytes could fix. `WaitForSingleObject` serves both the bounded poll and the zero-timeout readiness check on the byte path; a resize there is detected by the `size()` tick, not by window-input records (`ReadFile` filters them).
- Measured at a real keyboard on 2026-11-07, with the console probe installed as a command. Under Windows Terminal the flag is accepted (`0x01f7` -> `0x03f8`), a pasted command line arrived as a bracketed burst — every character as `Char`, the tab as `Char('\t')` rather than as the Tab key, which only happens inside a burst — and the eighty-five events of a long paste arrived within seven milliseconds, which is the decoded-key queue doing its job rather than one key per later keystroke. A pasted two-line block — `alpha`, line break, `beta` — delivered ten events in two milliseconds with `Char('\r')` at the sixth and no `Enter` anywhere in the session: the line break reaches the field as text and the session is not submitted. That last one is the measurement the whole pass was for, and it was taken at a real keyboard. Under a legacy conhost the byte path is not taken (`0x01f7` -> `0x01f8`, `vt_bit=false`) and `Ctrl+V` arrived as `Char('v')` with no paste: the limit above, measured rather than inferred.
- [ui.rs:mod palette] sends 24-bit RGB only when the host advertises it (`COLORTERM`, `WT_SESSION`, `TERM_PROGRAM`) and honours `NO_COLOR` ([ui.rs:pub fn truecolor], [ui.rs:pub fn no_colour]).
- [ui.rs:const MAX_RESULTS] is 20_000; [ui.rs:const PAGE_WINDOW] is 8; [ui.rs:const LATENCY_SAMPLES] is 64; [ui.rs:const MIN_WIDTH] is 40; [ui.rs:const MIN_HEIGHT] is 8.
- [ui.rs:enum Action], [ui.rs:enum Outcome] and [ui.rs:struct Notice] carry the model's decisions; [ui.rs:fn handle_key] and [ui.rs:fn drain_goes_on] bound what a key and a drain do.
- [preview.rs:const MAX_BYTES] is 64 KiB and [preview.rs:const MAX_LINES] is 12; [preview.rs:fn is_binary] decides what is not text.
- [ui.rs:fn strip_ansi], [ui.rs:fn display_width], [ui.rs:fn truncate_visible], [ui.rs:fn is_wide] and [ui.rs:fn is_combining] keep the drawn cells within the terminal width.
- [ui.rs:pub fn run] prints the selected path to stdout after the screen is restored, so the result can be piped onward. `Enter` chooses and prints; [ui.rs:enum Action::Choose] is that one decision, and an action from the menu or a command does not end the session.
- [ui.rs:fn panel_key] owns the keyboard while the actions menu or the settings panel is open; `Esc` closes the panel before it closes the session, the same order the window uses. The actions offered are `rgy_launcher::Action::for_kind(IconInfo::of(path))`, so a binary or folder never offers a preview.
- With [runner.rs:pub struct NoRunner], `>copy`, `>folder` and `>settings` keep their descriptive notices exactly as before; with a runner they call the machine and say what it did.

## Side Effects
- [ui.rs:impl TerminalGuard] enters raw mode, the alternate screen and bracketed paste through `term::enable_raw_mode` and `term::Screen`; [ui.rs:impl Drop for TerminalGuard] writes the reset bytes — including `\x1b[?2004l` — to `rgy_engine::console::stdout()` and calls `term::disable_raw_mode`.
- [term.rs:pub struct Screen] writes ANSI escape sequences to its `Write` output — `\x1b[?2004h` on enter, `\x1b[?2004l` on leave — and [term.rs:pub fn flush] flushes it.
- [term.rs:mod imp] (unix) calls `tcgetattr`, `tcsetattr`, `ioctl`, `poll_fds` and `read_bytes`; [term.rs:mod imp] (Windows) calls `GetStdHandle`, `GetConsoleMode`, `SetConsoleMode`, `GetConsoleScreenBufferInfo`, `ReadConsoleInputW` (fallback path), `ReadFile` (VT byte path), `WaitForSingleObject` (both the bounded poll and the zero-timeout readiness check), `GetForegroundWindow`, `GetWindowThreadProcessId` and `GetKeyboardLayout`.
- [ui.rs:mod palette] reads `COLORTERM`, `WT_SESSION`, `TERM_PROGRAM` and `NO_COLOR`; [ui.rs:fn home_dir] reads `USERPROFILE` / `HOME`.
- [ui.rs:fn run_app] initialises the console code page, reads keys and paints frames, then writes the chosen path to stdout through `rgy_pick::carries_line` and reports an uncarryable path on stderr via `rgy_pick::uncarryable_paths`.
- [preview.rs:fn read_head] opens the file with `std::fs::File::open` and stats it with `std::fs::metadata`.

## Verification
- Run: `cargo test --workspace --no-fail-fast --locked`
