# crates/launcher-windows/src

## Overview
The ten library sources of `rgy-launcher-windows`: the launcher's floating
Win32 overlay, drawn with GDI. The crate root declares `#![no_std]` followed
by `#![cfg(windows)]` — the `no_std` attribute comes first so an emptied crate
still declares `no_std` — and off Windows the crate is empty. The Win32 calls
are hand-written in `win.rs`; there is no `windows` crate in the tree. The
only dependency is `rgy-launcher`, the model. The manifest declares no
`std` feature.

## Interface
- `[lib.rs:no_std]` — `#![no_std]`, then `#![cfg(windows)]`, then
  `extern crate alloc`.
- `[lib.rs:pub mod actions]`, `[lib.rs:pub mod combo]`,
  `[lib.rs:pub mod hook]`, `[lib.rs:pub mod paint]`,
  `[lib.rs:pub mod session]`, `[lib.rs:pub mod sole]`,
  `[lib.rs:pub mod window]`, `[lib.rs:mod tray]`, `[lib.rs:mod win]`.
- `[lib.rs:pub use sole::{Sole, Taken}]`,
  `[lib.rs:pub use window::{Awareness, Compositor, Event, Key, View, Window}]`.
- `[lib.rs:session_available]`, `[lib.rs:put_the_console_away]` — the two free
  functions.
- `[window.rs:Window]`, `[window.rs:Key]`, `[window.rs:Event]`,
  `[window.rs:View]`, `[window.rs:Compositor]`, `[window.rs:Awareness]`,
  `[window.rs:WM_TOGGLE]`, `[window.rs:WM_SHOW]`, `[window.rs:WM_WAKE]`,
  `[window.rs:wake]` — the window surface.
- `[hook.rs:Hook]`, `[hook.rs:install]`, `[hook.rs:hook_proc]` — the low-level
  keyboard hook.
- `[sole.rs:Sole]`, `[sole.rs:Taken]`, `[combo.rs:Combo]`,
  `[actions.rs:open]`, `[actions.rs:reveal]`, `[actions.rs:copy_path]`,
  `[actions.rs:paste_text]`, `[session.rs:available]`,
  `[paint.rs:Colours]`, `[paint.rs:Fonts]`, `[paint.rs:Icons]`,
  `[tray.rs:Tray]`.
- `[win.rs:HWND]`, `[win.rs:RECT]`, `[win.rs:MSG]`, `[win.rs:WNDCLASSEXW]`,
  `[win.rs:WM_TIMER]`, `[win.rs:CreateFileW]`, `[win.rs:CloseHandle]`,
  `[win.rs:GetLastError]`, `[win.rs:GetConsoleWindow]`,
  `[win.rs:FreeConsole]`, `[win.rs:SetStdHandle]`,
  `[win.rs:console_process_list]` — hand-declared Win32 types, constants and
  calls.

## Invariants
- `[lib.rs:no_std]` — `#![no_std]` is declared **before** `#![cfg(windows)]`;
  a crate-level `cfg` that empties the crate would otherwise leave it with no
  `no_std` to declare.
- `[lib.rs:the_console_is_the_launcher_s_own]` — one process attached means
  the console is the launcher's own; zero is the call's refusal, two or more
  means a console somebody else is typing in.
- `[win.rs:WM_TIMER]` — `0x0113`, the platform's timer message.
- `[win.rs:GetLastError]` — the error code is read where the call is; the
  wrappers return the code so a caller cannot read it too late.
- `[win.rs:console_process_list]` — a buffer is a slice, not a pointer: the
  wrapper takes `&mut [DWORD]` and passes its length.
- `[session.rs:available]` — a session has a desktop only when the platform
  says so; the question is asked before the window is built.

## Side Effects
- `[win.rs:CreateFileW]`, `[win.rs:CloseHandle]`, `[win.rs:SetStdHandle]`,
  `[win.rs:FreeConsole]`, `[win.rs:GetConsoleWindow]`,
  `[win.rs:GetConsoleProcessList]` — Win32 platform calls declared by hand in
  `unsafe extern "system"` blocks.
- `[lib.rs:put_the_console_away]` — opens the `NUL` device, redirects the
  standard streams and gives the console back.
- `[hook.rs:install]` — installs a low-level keyboard hook
  (`WH_KEYBOARD_LL`); `[window.rs:Window]` — creates and composites a layered
  Win32 window.
- `[actions.rs:open]`, `[actions.rs:reveal]` — shell-execute a path;
  `[actions.rs:copy_path]` — writes to the clipboard.
- `[tray.rs:Tray]` — a notification-area icon.

## Verification
- Run: `cargo test --workspace --no-fail-fast --locked`
