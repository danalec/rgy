# bins/rgxy

## Overview

Crate root of the `rgxy` floating-launcher binary. The directory has no source
of its own: `[Cargo.toml:package.name]` is `rgxy` at
`[Cargo.toml:package.version]` 0.1.10, `[Cargo.toml:package.edition]` 2024 and
`[Cargo.toml:package.rust-version]` 1.89. It declares exactly one binary
target, `[Cargo.toml:[[bin]].name]` = `rgxy` at
`[Cargo.toml:[[bin]].path]` = `src/main.rs`. All implementation is in the
[src](./src/EGM.md) child contract: a client of the search daemon with a
shared window driver (a Windows backend and an X11 backend for Linux and
FreeBSD) and a terminal interface that carries out the same actions through a
runner.

## Subdirectories & Boundaries
- [src](./src/EGM.md) — the launcher's implementation: `main.rs`, the daemon client, the window driver, the terminal action runner, the usage/settings stores, the preview, the `rgx` fallback and the language/help text.

## Interface
- `[Cargo.toml:[[bin]].name]` — `rgxy`, built from `[Cargo.toml:[[bin]].path]` (`src/main.rs`).
- `[Cargo.toml:package.name]` — `rgxy`; `[Cargo.toml:package.version]` 0.1.10; `[Cargo.toml:package.edition]` 2024; `[Cargo.toml:package.rust-version]` 1.89.
- `[Cargo.toml:dependencies.rgy-launcher]`, `[Cargo.toml:dependencies.rgy-proto]`, `[Cargo.toml:dependencies.rgy-search-ui]`, `[Cargo.toml:dependencies.rgy-engine]`, `[Cargo.toml:dependencies.rgy-pick]` — the five workspace dependencies; `[Cargo.toml:target.'cfg(windows)'.dependencies.rgy-launcher-windows]` and `[Cargo.toml:target.'cfg(any(target_os = "linux", target_os = "freebsd"))'.dependencies.rgy-launcher-x11]` are the two window backends, behind their platform's `cfg`; Linux and FreeBSD share the X11 one.
- Rolled up from [src](./src/EGM.md): [main.rs:pub struct Options], [main.rs:fn main], [gui.rs:pub fn run], [daemon.rs:pub struct DaemonSource], [daemon.rs:pub const LAUNCHER_WINDOW], [rgx.rs:pub fn grep], [preview.rs:pub fn of], [mru.rs:pub fn save], [settings_store.rs:pub fn store_path], [usage.rs:pub fn text], [lang.rs:pub fn from_env], [tools.rs:pub fn rgx].

## Invariants
- `[Cargo.toml:[[bin]]]` — exactly one binary target, named `rgxy`; the package and the program carry the same name.
- `[Cargo.toml:target.'cfg(windows)'.dependencies]` and `[Cargo.toml:target.'cfg(target_os = "linux")'.dependencies]` — one window backend per platform, both exporting the same interface; [main.rs:mod gui] and [main.rs:mod preview] are `#[cfg(any(windows, target_os = "linux"))]`, [main.rs:mod platform] names the backend the driver uses, and the launcher falls back to the terminal on other hosts ([main.rs:fn run_tui]).
- Rolled up from [src](./src/EGM.md): [daemon.rs:enum Presence] (Gone / Slow / Answered), [daemon.rs:pub const LAUNCHER_WINDOW] (2 s), [mru.rs:const HEADER] (the usage-store version), [settings_store.rs:pub fn save] (atomic write), [gui.rs:const LIMIT] (50).

## Side Effects
- Rolled up from [src](./src/EGM.md): [main.rs:fn main] (stdout/stderr, console code page), [daemon.rs:pub fn connect] (pipe/UDS client), [gui.rs:pub fn run] (window, hotkey hook, worker threads), [rgx.rs:pub fn grep] (spawns the `rgx` process), [mru.rs:pub fn save] and [settings_store.rs:pub fn save] (usage/settings files in the cache dir), [tools.rs:pub fn scan] (PATH scan), [lang.rs:pub fn from_env] (environment variables).

## Verification
- Run: `cargo test --workspace --no-fail-fast --locked`
