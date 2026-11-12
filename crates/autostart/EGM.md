# crates/autostart

## Overview
The `rgy-autostart` crate: registering the rgy search daemon to start at
login, per platform, without elevation or prompts. Its manifest names the
package `rgy-autostart` (edition 2024), declares `default = ["std"]` with
`std = ["rgy-proto/std"]`, and declares one `[[bin]]` target `rgy-autostart`
(`path = "src/main.rs"`, `required-features = ["std"]`). Its only dependency
is `rgy-proto` (so the rule for where the daemon listens is shared with the
client); the platform calls are `schtasks`/`reg`, `systemctl`, files and
`launchctl`, not bindings.

## Subdirectories & Boundaries
- [src](./src/EGM.md) — the seven `.rs` sources: `lib.rs` (decisions),
  `plan.rs` (platform/mechanism), `render.rs` (artifact text), `linux.rs`,
  `macos.rs`, `windows.rs` (platform backends) and `main.rs` (the CLI).

## Interface
- `[Cargo.toml:package.name]` — package `rgy-autostart`.
- `[Cargo.toml:[[bin]]]` — the `rgy-autostart` binary target.
- `[lib.rs:pub use plan::{Mechanism, Platform, choose_mechanism, default_name, detect_platform, launcher_entry_supported}]`,
  `[lib.rs:Launcher]`, `[lib.rs:Report]`, `[lib.rs:Removal]` (from `src`).
- `[plan.rs:Platform]`, `[plan.rs:Mechanism]`, `[plan.rs:Transport]` (from
  `src`).
- `[main.rs:main]` — the CLI entry point (from `src`).

## Invariants
- `[lib.rs:no_std]` — `#![no_std]` + `extern crate alloc`; `std` is declared
  only behind `#[cfg(feature = "std")]` (from `src`).
- `[Cargo.toml:features.std]` — `std = ["rgy-proto/std"]`.
- `[Cargo.toml:required-features]` — the binary needs the `std` feature; the
  `no_std` build of the library is what the bare-metal gate checks.
- `[lib.rs:default_args]` — the root and transport are always explicit in a
  login entry (from `src`).
- `[lib.rs:RUN_KEY]` — user hive only; nothing is elevated and nothing prompts
  (from `src`).

## Side Effects
- `[main.rs:install]`, `[main.rs:uninstall]` — systemd units, launchd plists,
  Task Scheduler tasks and registry values are written/removed (from `src`).
- `[windows.rs:install_task]`, `[linux.rs:enable]`, `[macos.rs:load]` —
  external platform commands are run (from `src`).
- `[lib.rs:find_daemon]`, `[lib.rs:home_dir]` — environment and filesystem
  lookups (from `src`).

## Verification
- Run: `cargo test --workspace --no-fail-fast --locked`
