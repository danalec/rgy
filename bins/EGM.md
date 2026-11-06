# bins

## Overview

Directory grouping this workspace's shipped binaries. It has no source file or
manifest of its own: a listing of `bins/*` returns no files, only the five
crate-root subdirectories below. Each crate root holds a `Cargo.toml` and a
`src/` directory that receives its own contract. The package names carry the
`rgy-` prefix while the binary targets keep their shipped names —
`[rgxy/Cargo.toml:package.name]` is `rgxy`,
`[search-ui/Cargo.toml:package.name]` is `rgy-search-ui`,
`[searchctl/Cargo.toml:package.name]` is `rgy-search`,
`[searchd/Cargo.toml:package.name]` is `rgy-searchd` and
`[zen-rs/Cargo.toml:package.name]` is `rgy-zen`.

## Subdirectories & Boundaries
- [rgxy](./rgxy/EGM.md) — the floating launcher (`rgxy`), a client of the daemon with a Windows window and a terminal fallback.
- [search-ui](./search-ui/EGM.md) — the terminal search UI library (`rgy-search-ui`), with its own hand-declared terminal seam.
- [searchctl](./searchctl/EGM.md) — the one-shot name-search CLI (`searchctl`).
- [searchd](./searchd/EGM.md) — the resident search daemon (`searchd`), JSON-lines over stdio/pipe/UDS plus the rgx `--serve` front.
- [zen-rs](./zen-rs/EGM.md) — the Windows NTFS MFT/USN indexer CLI/TUI (`zen-rs`).

## Interface
- `[rgxy/Cargo.toml:[[bin]].name]` — `rgxy` at `[rgxy/Cargo.toml:[[bin]].path]`; rolled up from [rgxy](./rgxy/EGM.md): [rgxy/src/main.rs:pub struct Options], [rgxy/src/main.rs:fn main], [rgxy/src/gui.rs:pub fn run], [rgxy/src/daemon.rs:pub struct DaemonSource].
- `[searchctl/Cargo.toml:[[bin]].name]` — `searchctl` at `[searchctl/Cargo.toml:[[bin]].path]`; rolled up from [searchctl](./searchctl/EGM.md): [searchctl/src/main.rs:fn main], [searchctl/src/main.rs:struct Opts], [searchctl/src/main.rs:fn run], [searchctl/src/main.rs:fn obtain_index].
- `[searchd/Cargo.toml:[[bin]].name]` — `searchd` at `[searchd/Cargo.toml:[[bin]].path]`; rolled up from [searchd](./searchd/EGM.md): [searchd/src/main.rs:fn main], [searchd/src/main.rs:struct Ctx], [searchd/src/main.rs:fn handle], [searchd/src/main.rs:fn run_stream], [searchd/src/main.rs:fn serve_stream].
- `[zen-rs/Cargo.toml:[[bin]].name]` — `zen-rs` at `[zen-rs/Cargo.toml:[[bin]].path]`; rolled up from [zen-rs](./zen-rs/EGM.md): [zen-rs/src/main.rs:fn main], [zen-rs/src/main.rs:fn cmd_ui], [zen-rs/src/main.rs:fn cmd_tail].
- `[search-ui/Cargo.toml:package.name]` — the library `rgy-search-ui` (no `[[bin]]` table); rolled up from [search-ui](./search-ui/EGM.md): [search-ui/src/lib.rs:pub mod term], [search-ui/src/lib.rs:pub use ui::*], [search-ui/src/ui.rs:pub fn run], [search-ui/src/term.rs:pub struct Screen], [search-ui/src/term.rs:pub fn enable_raw_mode].

## Invariants
- Each crate root declares exactly one shipped binary except `search-ui`: `[rgxy/Cargo.toml:[[bin]]]`, `[searchctl/Cargo.toml:[[bin]]]`, `[searchd/Cargo.toml:[[bin]]]` and `[zen-rs/Cargo.toml:[[bin]]]`; `[search-ui/Cargo.toml:package.name]` is a library whose root is [search-ui/src/lib.rs:pub mod term] / [search-ui/src/lib.rs:pub use ui::*].
- `[rgxy/Cargo.toml:target.'cfg(windows)'.dependencies]`, `[searchd/Cargo.toml:target.'cfg(windows)'.dependencies]`, `[searchd/Cargo.toml:target.'cfg(target_os = "linux")'.dependencies]` and `[searchd/Cargo.toml:target.'cfg(target_os = "macos")'.dependencies]` are per-platform; rolled up from [zen-rs](./zen-rs/EGM.md), [zen-rs/src/main.rs:fn main] (`#[cfg(not(windows))]`) answers only help/version off-Windows.
- `[rgxy/Cargo.toml:package.edition]`, `[search-ui/Cargo.toml:package.edition]`, `[searchctl/Cargo.toml:package.edition]`, `[searchd/Cargo.toml:package.edition]` and `[zen-rs/Cargo.toml:package.edition]` are all 2024; `[rgxy/Cargo.toml:package.rust-version]`, `[search-ui/Cargo.toml:package.rust-version]`, `[searchctl/Cargo.toml:package.rust-version]`, `[searchd/Cargo.toml:package.rust-version]` and `[zen-rs/Cargo.toml:package.rust-version]` are all 1.89.
- Rolled up from [rgxy](./rgxy/EGM.md): [rgxy/src/daemon.rs:enum Presence], [rgxy/src/daemon.rs:pub const LAUNCHER_WINDOW]; from [searchctl](./searchctl/EGM.md): [searchctl/src/main.rs:fn clamp_limit], [searchctl/src/main.rs:fn widths]; from [searchd](./searchd/EGM.md): [searchd/src/main.rs:const CATCH_UP_REPLAYED], [searchd/src/main.rs:const FEED_ARM_WAIT], [searchd/src/main.rs:const SERVE_LIMIT]; from [search-ui](./search-ui/EGM.md): [search-ui/src/ui.rs:impl Drop for TerminalGuard], [search-ui/src/ui.rs:const MAX_RESULTS].

## Side Effects
- Rolled up from [rgxy](./rgxy/EGM.md): [rgxy/src/main.rs:fn main] (stdout/stderr, console code page), [rgxy/src/gui.rs:pub fn run] (window, hotkey hook, worker threads), [rgxy/src/rgx.rs:pub fn grep] (spawns the `rgx` process), [rgxy/src/mru.rs:pub fn save] / [rgxy/src/settings_store.rs:pub fn save] (usage/settings files).
- Rolled up from [searchctl](./searchctl/EGM.md): [searchctl/src/main.rs:fn obtain_index] (tree walk, on-disk index), [searchctl/src/main.rs:fn run_pick] (external picker), [searchctl/src/main.rs:fn run_daemon] (pipe/UDS client); from [searchd](./searchd/EGM.md): [searchd/src/main.rs:fn serve_uds] (Unix socket bind), [searchd/src/main.rs:fn open_pipe_server] (`CreateNamedPipeW` / `ConnectNamedPipe`), [searchd/src/main.rs:fn open_watch] (platform change feeds), [searchd/src/main.rs:fn spawn_watch] / [searchd/src/main.rs:fn spawn_trim] / [searchd/src/main.rs:fn spawn_content] (threads).
- Rolled up from [zen-rs](./zen-rs/EGM.md): [zen-rs/src/main.rs:fn cmd_save] / [zen-rs/src/main.rs:fn load_or_build] (on-disk index blob), [zen-rs/src/main.rs:fn build_index] (NTFS volume handles, MFT walk, USN journal); from [search-ui](./search-ui/EGM.md): [search-ui/src/term.rs:pub struct Screen] (ANSI escape writes), [search-ui/src/term.rs:mod imp] (unix `tcgetattr`/`ioctl` and Windows `GetConsoleMode`/`ReadConsoleInputW` calls), [search-ui/src/preview.rs:fn read_head] (file open and stat).

## Verification
- Run: `cargo test --workspace --no-fail-fast --locked`
