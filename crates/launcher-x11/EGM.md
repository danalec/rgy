# crates/launcher-x11

## Overview

The `rgy-launcher-x11` crate: the launcher's floating X11 overlay and its
XEmbed system-tray icon, drawn in pure Rust with no toolkit and no runtime.
Its manifest names the package `rgy-launcher-x11` (edition 2024) and declares
exactly one dependency, `rgy-launcher` (the model). **One X11 backend serves
Linux and FreeBSD**: both desktops speak X11, so the bodies are gated
`#[cfg(any(target_os = "linux", target_os = "freebsd"))]` rather than copied
into a second crate. All Xlib and libc calls are hand-written in
[src](./src/EGM.md)'s `x.rs` seam; Xlib is resolved at runtime with
`dlopen("libX11.so.6")` + `dlsym`, so no target needs a `libX11.so`
development symlink at build time and a machine without X11 falls back to the
terminal interface. It exports the SAME interface as the Windows backend —
`Window`, `Event`, `Key`, `View`, `Control`, `Hook`, `Combo`, `Sole`, `Taken`,
`actions`, `session_available` — so `bins/rgxy`'s window driver is one
driver reached through `src/platform.rs`.

The crate is `no_std` + `alloc`. Unlike the Windows backend it does NOT empty
itself off its two hosts with a crate-level `cfg`: the decisions that need no
display (the vocabulary, the keysym decoder, the geometry, the icon's pixels)
are un-gated modules with tests that run on every host; only the bodies that
call Xlib are behind the shared `cfg`.

The kernels differ in a handful of libc facts, and those differences live in
`x.rs` and `sole.rs` only: `O_NONBLOCK`, `O_CLOEXEC`, `SOCK_CLOEXEC`,
`SOCK_NONBLOCK` and `SIGCHLD` have FreeBSD values, and FreeBSD's
`sockaddr_un` has a length byte, a one-byte family and a 104-byte path —
and **no abstract Unix sockets**, so the single-instance claim uses a
filesystem socket in `/tmp` there.

## Subdirectories & Boundaries
- [src](./src/EGM.md) — the seam, the window and its renderer, the tray, the hotkey, the single-instance guard, the session check and the actions.

## Interface
- `[Cargo.toml:package.name]` — `rgy-launcher-x11`; one dependency (`rgy-launcher`), no `[features]` stanza.
- `[lib.rs:pub use window::{Awareness, Compositor, Event, Key, View, Window}]`, `[lib.rs:pub use sole::{Sole, Taken}]`, `[lib.rs:session_available]`, `[lib.rs:put_the_console_away]`.
- `[src/lib.rs:pub mod window]`, `[src/lib.rs:pub mod actions]`, `[src/lib.rs:pub mod sole]`, `[src/lib.rs:pub mod hook]`, `[src/lib.rs:pub mod combo]` — the module names the driver uses on every host.

## Invariants
- `[Cargo.toml:[dependencies]]` — one dependency (`rgy-launcher`); no external Rust crate, no toolkit, no `std` feature.
- `[src/lib.rs:#![no_std]]` — `#![no_std]` is unconditional, and no `#[link]` declaration is compiled on any host; Xlib is resolved at runtime.
- `[src/types.rs:Key]`, `[src/types.rs:Event]`, `[src/types.rs:View]`, `[src/types.rs:Control]` — the same types `rgy-launcher-windows` exports; the driver does not know the platform.
- `[src/keys.rs]` — X11's protocol numbers are plain Rust integers in an un-gated module, so a `c_ulong` that is four bytes on Windows cannot change their meaning.
- `[src/x.rs:XEvent]` — `XEvent` is the whole 24-word union the platform writes; its size and every struct's offsets are asserted at compile time, on both LP64 kernels.
- `[src/x.rs:SockaddrUn]` — two compile-time layouts: 110 bytes on Linux, 106 on FreeBSD; the difference is confined to this seam and `sole.rs`.
- `[src/runtime.rs:wake]` — the daemon's worker wakes the loop with a byte in a non-blocking pipe; no Xlib call is made from a thread that did not open the display.

## Side Effects
- `[src/x.rs:XOpenDisplay]`, `[src/x.rs:XCreateWindow]`, `[src/x.rs:XGrabKey]`, `[src/x.rs:XSetSelectionOwner]` — hand-declared Xlib calls (from [src](./src/EGM.md)).
- `[src/actions.rs:open]` — double-fork + `execvp` of `xdg-open`; `[src/actions.rs:copy_path]` takes the CLIPBOARD selection.
- `[src/tray.rs:Tray::new]` — docks an XEmbed icon into the freedesktop system tray.
- `[src/sole.rs:Sole::take]` — binds an abstract Unix socket on Linux, a filesystem socket in `/tmp` on FreeBSD (with a connect-first probe and `unlink` of a stale file); one launcher per session on both.

## Verification
- Run: `cargo test --workspace --no-fail-fast --locked`
