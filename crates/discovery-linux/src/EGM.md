# rgy-discovery-linux src — the Linux walk and inotify watch

## Overview
The Linux discovery backend: a `getdents64` bulk walk (`[walk.rs:LinuxSource]`)
producing the same DFS `[walk.rs:Listing]`s as every other `Source`, and a
recursive inotify `Watch` (`[watch.rs:InotifyWatch]`) whose overflow becomes
`[watch.rs:ChangeBatch::Rescan]`. `[lib.rs:#![no_std]]` makes the crate
`no_std`; the kernel is asked through calls declared by hand in
`[walk.rs:mod sys]` and `[watch.rs:mod sys]`, and the metadata of an entry is
asked of `[walk.rs:rgy_discovery::dir::stat]`, where the platform's `stat`
layout lives.

## Interface
- `[lib.rs:LinuxSource]` / `[lib.rs:InotifyWatch]` — `#[cfg(target_os = "linux")]` re-exports of `[walk.rs:LinuxSource]` and `[watch.rs:InotifyWatch]`.
- `[walk.rs:scan]` — the `getdents64` DFS walk; `[walk.rs:list_dir]`, `[walk.rs:Frame]`, `[walk.rs:impl rgy_discovery::Source for LinuxSource]`.
- `[watch.rs:InotifyWatch]` — `[watch.rs:impl Watch for InotifyWatch]`, `[watch.rs:impl Drop for InotifyWatch]`.
- `[watch.rs:REASON_CREATE]`, `[watch.rs:REASON_DELETE]`, `[watch.rs:REASON_RENAME_OLD]`, `[watch.rs:REASON_RENAME_NEW]` — USN reason aliases; `[watch.rs:errno_name]`.

## Invariants
- `[lib.rs:#![no_std]]` / `[lib.rs:#[cfg(target_os = "linux")]]` — `no_std`, and the whole backend is gated to Linux; off-target the crate compiles empty and `[lib.rs:rgy_discovery::PortableSource]` is the `Source`.
- `[watch.rs:InotifyWatch]` — an `IN_Q_OVERFLOW` (or watch-limit hit) reports `ChangeBatch::Rescan`; anything else is translated into `UsnRecord`-shaped changes carrying the shared create/delete/rename reason bits — one change vocabulary across backends.
- `[walk.rs:syscall]` — `getdents64` is invoked through a hand-declared raw `syscall` (no `libc`); `[walk.rs:mod sys]` declares the kernel calls.
- `[watch.rs:IN_NONBLOCK]` / `[watch.rs:IN_CLOEXEC]` — the inotify read does not wait for an event and the descriptor does not outlive the process.
- `[walk.rs:scan]` — the walk is sequential DFS with a single listing-id counter; children are resolved by id.

## Side Effects
- `[walk.rs:open]`, `[walk.rs:close]`, `[walk.rs:syscall]` — kernel calls in `[walk.rs:unsafe extern "C"]`; bulk directory reads.
- `[walk.rs:rgy_discovery::dir::stat]` — per-entry metadata from the seam's `stat` layout.
- `[watch.rs:inotify_init1]`, `[watch.rs:inotify_add_watch]`, `[watch.rs:read]`, `[watch.rs:close]` — inotify calls declared by hand; `[watch.rs:impl Drop for InotifyWatch]` closes descriptors.
- `[watch.rs:impl Watch for InotifyWatch]` — reads kernel change events from the inotify descriptor.

## Verification
- Run: `cargo test --workspace --no-fail-fast --locked`
