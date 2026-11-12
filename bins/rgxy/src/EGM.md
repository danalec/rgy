# bins/rgxy/src

## Overview

Source of the `rgxy` binary, the floating launcher that sits on top of the rgy
search daemon ([main.rs:fn main], [main.rs:mod daemon]). It is a **client**: the
index lives in the daemon and a keystroke is a question sent down a pipe
([daemon.rs:use rgy_proto::client::Client], [daemon.rs:use rgy_proto::endpoint::Endpoint]).
`main.rs` declares the module set — [main.rs:mod lang], [main.rs:mod mru],
[main.rs:mod rgx], [main.rs:mod tools], [main.rs:mod usage], [main.rs:mod platform],
[main.rs:mod tui_actions] — plus the two window modules [main.rs:mod gui] and [main.rs:mod preview], gated
`#[cfg(any(windows, target_os = "linux", target_os = "freebsd"))]`. The window driver is
[gui.rs:pub fn run], which wires the model, source and window through
`rgy_launcher` and the platform backend named by [platform.rs:pub use ... as backend]
([gui.rs:use rgy_launcher::{Action, Answer, App, Layer, Mode, Preview, Row, Source, commands}],
[gui.rs:use crate::platform::backend::window::{Compositor, Control, Event, Key, View, Window}]).
The backend is `rgy_launcher_windows` on Windows and `rgy_launcher_x11` on
Linux and FreeBSD; both export the same interface. On a machine without a desktop the binary
runs the terminal interface instead ([main.rs:fn run_tui]), and it hands that
interface a [tui_actions.rs:pub struct TuiActions] implementing
`rgy_search_ui::Runner` so the terminal opens, copies and edits settings with
the same machine the window uses. Supporting modules provide the localized help
([usage.rs:pub fn text]), the language ([lang.rs:pub fn from_env]), the `rgx`
fallback ([rgx.rs:pub fn grep]), the preview ([preview.rs:pub fn of]), the
settings file ([settings_store.rs:pub fn store_path]) and the usage history
([mru.rs:pub fn save]).

## Interface
- [main.rs:pub struct Options] — CLI options `tui`, `query`, `limit`, `endpoint`, `hook`, `show`, `pick`, `fzf_binary`, `fzf_arg`; [main.rs:impl Default for Options] sets `limit = 50` and `hook = true`.
- [main.rs:fn main] — entry point returning `std::process::ExitCode`; [main.rs:enum Request] and [main.rs:fn parse] dispatch help, version, one-shot, pick, TUI or window through [main.rs:fn run].
- [usage.rs:pub fn text] — help text in English or Portuguese ([usage.rs:fn english], [usage.rs:fn portuguese]).
- [lang.rs:pub fn from_env] — language from `RGY_LANG`, `LC_ALL`, `LANG` via [lang.rs:fn from].
- [tools.rs:pub fn rgx] and [tools.rs:pub fn scan] — locate the `rgx` fallback binary on `PATH`.
- [rgx.rs:pub fn grep] — content search fallback over the name layer's chosen files; [rgx.rs:pub fn split_pattern] splits a query into `ContentMode` and pattern; [rgx.rs:fn only_files] and [rgx.rs:fn stat_files] shape the scope.
- [preview.rs:pub fn of] and [preview.rs:pub fn pending] — the head-of-file preview.
- [settings_store.rs:pub fn store_path], [settings_store.rs:pub fn load], [settings_store.rs:pub fn save] — the settings file.
- [tui_actions.rs:pub struct TuiActions] with [tui_actions.rs:impl Runner for TuiActions] — the terminal's action machine: `open`/`open_folder` delegate to the platform backend where there is one, `copy_path` uses the Win32 clipboard on Windows and OSC 52 to `/dev/tty` on unix, and `settings`/`save_settings` go through [settings_store.rs:pub fn load] / [settings_store.rs:pub fn save].
- [mru.rs:pub fn store_path], [mru.rs:pub fn now], [mru.rs:pub fn load], [mru.rs:pub fn read_error], [mru.rs:pub fn parse], [mru.rs:pub fn save], [mru.rs:pub fn render], [mru.rs:pub fn escape_path], [mru.rs:pub fn unescape_path] — the usage-history store.
- [daemon.rs:pub struct DaemonSource] with [daemon.rs:pub fn new], [daemon.rs:pub fn with_window], [daemon.rs:pub fn connect], [daemon.rs:pub fn ask_the_daemon], [daemon.rs:pub fn status], [daemon.rs:pub fn endpoint], [daemon.rs:pub fn rgx], [daemon.rs:pub fn snapshot] and [daemon.rs:impl Source for DaemonSource]; [daemon.rs:pub struct Status]; [daemon.rs:pub const LAUNCHER_WINDOW].
- [gui.rs:pub fn run] — the resident window driver, over [gui.rs:trait WindowSource], [gui.rs:trait Worker] and [gui.rs:struct State].

## Invariants
- [daemon.rs:enum Presence] has exactly `Gone`, `Slow`, `Answered`; [daemon.rs:fn presence] maps `NotFound`, `BrokenPipe`, `ConnectionRefused`, `ConnectionReset`, `ConnectionAborted` to `Gone`, `TimedOut` to `Slow`, and everything else to `Answered` — only an unreachable daemon is gone.
- [daemon.rs:pub const LAUNCHER_WINDOW] is 2 s; [daemon.rs:COUNT_WINDOW] is 250 ms; [daemon.rs:SCOPE_LIMIT] is 64.
- [gui.rs:const LIMIT] is 50; [gui.rs:const RETRY_TICKS] is 8; [gui.rs:enum Waiting] and [gui.rs:enum Arm] encode the worker and hotkey states.
- [mru.rs:const HEADER] (`# rgy usage v2: count\tlast\tpath`) is the format version; [mru.rs:pub fn parse] reads pre-escape stores by the absence of that header; [mru.rs:pub fn escape_path] and [mru.rs:pub fn unescape_path] escape `\n`, `\r`, `\t` and `\\`.
- [mru.rs:fn merge] makes a write a read-merge-write; [mru.rs:struct Lock] holds the lock on an open `std::fs::File` and releases it on close; [mru.rs:const LOCK_WAIT_MS] bounds the wait.
- [settings_store.rs:pub fn save] writes through a temporary file and [settings_store.rs:fn replace] renames it atomically; [settings_store.rs:fn quarantine] moves a broken store aside.
- [rgx.rs:pub fn grep] answers only the files the name layer chose, drops folders, honours a deadline and treats `rg` exit 1 as an answer; [rgx.rs:fn case_insensitive] keeps the engine's case rule.
- [preview.rs:const MAX_BYTES] is 64 KiB and [preview.rs:const MAX_LINES] is 12; [preview.rs:fn is_binary] decides what is not text.
- [lang.rs:pub fn from_env] reads the language once, in the order `RGY_LANG` → `LC_ALL` → `LANG`, defaulting to `Lang::English`.
- [main.rs:fn resident_launch] treats a bare command line as the resident launcher; [main.rs:fn one_shot] uses exit 0 for a match and 1 for none; [main.rs:const OUTPUT_NOT_DELIVERED] is 2; [main.rs:const DAEMON_WAIT] is 2 s.
- [gui.rs:fn said_once] emits a machine message only when it changes and hands it to [gui.rs:struct Stderr], the single long-lived stderr writer behind a bounded channel; [gui.rs:fn adds_to_the_query] and [gui.rs:fn menu_key] bound which keys the field and the actions menu own.

## Side Effects
- [main.rs:fn main] writes stdout/stderr, initialises the console code page and, on Windows, gives the console back through [main.rs:fn resident_launch]; [main.rs:fn one_shot] and [main.rs:fn pick] write results and selections to stdout.
- [daemon.rs:pub fn connect] opens the pipe/UDS/stdio client; [daemon.rs:impl Source for DaemonSource] writes the request to the connected peer.
- [gui.rs:pub fn run] creates the window, installs the low-level hotkey hook, checks the compositor and drives [gui.rs:fn spawn_worker]; a worker that cannot start degrades to a dead daemon instead of panicking, and [gui.rs:struct Stderr] writes every machine message on one bounded channel, not one thread per change.
- [gui.rs:fn record_use] and [gui.rs:fn save_settings] write their stores through [gui.rs:static WRITES], a single-worker [rgx.rs:struct BoundedPool] with a bounded queue: both jobs carry the whole of their state, so the newest write is the one that has to land, and one worker keeps them in submission order. No per-keystroke or per-use thread is started.
- [rgx.rs:pub fn grep] starts the `rgx` process and reads its JSON lines through [rgx.rs:struct BoundedPool]'s bounded worker pool; [rgx.rs:fn only_files] stats the scoped paths on the same pool, so a blocked stat cannot grow the resident process's thread count.
- [settings_store.rs:pub fn load], [settings_store.rs:pub fn save] and [settings_store.rs:fn quarantine] read and write `settings.tsv` in the cache dir, with `std::fs::create_dir_all`, `std::fs::write` and `std::fs::rename`.
- [mru.rs:pub fn load], [mru.rs:pub fn save] and [mru.rs:fn write_store] read and write the usage file; [mru.rs:struct Lock] uses `std::fs::OpenOptions` and, on unix, [mru.rs:fn flock]; [mru.rs:static TEMP_ID] is an `AtomicU64`.
- [preview.rs:fn read_head] opens the file and reads its head (`std::fs::File::open`, `std::fs::metadata`).
- [tools.rs:pub fn scan] reads `std::env::var_os("PATH")` and stats candidate files; [lang.rs:pub fn from_env] and [main.rs:fn home_dir] read environment variables.
- [main.rs:fn pick] delegates to `rgy_pick::pick_records` and writes the chosen paths to stdout.
- [main.rs:fn run_tui] hands [tui_actions.rs:pub struct TuiActions] to `rgy_search_ui::run_with_source_lang_theme_and_runner`; [tui_actions.rs:impl Runner for TuiActions] spawns the platform's opener (or delegates to the window backend), reads and writes the Win32 clipboard on Windows, writes OSC 52 to `/dev/tty` on unix, and loads/saves `settings.tsv`. A detached child is reaped on a thread of its own so a resident launcher does not collect zombies.

## Verification
- Run: `cargo test --workspace --no-fail-fast --locked`
