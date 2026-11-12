# crates/launcher-windows

## Overview
The `rgy-launcher-windows` crate: the launcher's floating Win32 overlay,
drawn with GDI, with no toolkit and no runtime. Its manifest names the package
`rgy-launcher-windows` (edition 2024) and declares exactly one dependency,
`rgy-launcher` (the model). It declares **no `std` feature**; `src/lib.rs`
declares `#![no_std]` first and `#![cfg(windows)]` second, so the crate is
empty off Windows and still `no_std`. All Win32 calls are hand-written in
`src/win.rs`.

## Subdirectories & Boundaries
- [src](./src/EGM.md) — the ten `.rs` sources: the window, the paint surface,
  the keyboard hook, the actions, the tray, the single-instance guard and the
  Win32 seam.

## Interface
- `[Cargo.toml:package.name]` — package `rgy-launcher-windows`.
- `[lib.rs:pub use window::{Awareness, Compositor, Event, Key, View, Window}]`,
  `[lib.rs:pub use sole::{Sole, Taken}]`,
  `[lib.rs:session_available]`, `[lib.rs:put_the_console_away]` (from `src`).
- `[window.rs:Window]`, `[hook.rs:Hook]` (from `src`).

## Invariants
- `[lib.rs:no_std]` — `#![no_std]` comes before `#![cfg(windows)]`; a
  crate-level `cfg` that empties the crate would otherwise leave it with no
  `no_std` to declare (from `src`).
- `[Cargo.toml:[dependencies]]` — one dependency (`rgy-launcher`), and no
  `[features]` stanza; the crate is `no_std` unconditionally.
- `[win.rs:GetLastError]` — the error code is read where the call is (from
  `src`).
- `[win.rs:console_process_list]` — a buffer is a slice, not a pointer (from
  `src`).

## Side Effects
- `[win.rs:CreateFileW]`, `[win.rs:CloseHandle]`, `[win.rs:FreeConsole]` —
  hand-declared Win32 platform calls (from `src`).
- `[lib.rs:put_the_console_away]` — redirects standard streams and gives the
  console back (from `src`).
- `[hook.rs:install]` — installs a low-level keyboard hook (from `src`).
- `[actions.rs:open]` — shell-executes a path (from `src`).

## Verification
- Run: `cargo test --workspace --no-fail-fast --locked`
