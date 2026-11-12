# rgy-discovery-linux

## Overview
The Linux discovery backend crate (`[Cargo.toml:package.name]` is
`rgy-discovery-linux`, edition 2024). Its root holds only the manifest; the
library is in [src](./src/EGM.md). `[Cargo.toml:rgy-discovery]` is pulled in
with `default-features = false`; the manifest declares no `[features]` of its
own and no `libc`, and its comments state the kernel is asked through the
calls declared by hand in `[src/walk.rs:mod sys]` and
`[src/watch.rs:mod sys]`, with metadata asked of the seam.

## Subdirectories & Boundaries
- [src](./src/EGM.md) — `[src/walk.rs:LinuxSource]` (the `getdents64` walk) and `[src/watch.rs:InotifyWatch]` (recursive inotify), both behind `[src/lib.rs:#[cfg(target_os = "linux")]]`.

## Interface
- `[src/lib.rs:LinuxSource]` / `[src/lib.rs:InotifyWatch]` — the cfg-gated public re-exports.
- `[src/walk.rs:scan]` / `[src/walk.rs:LinuxSource]` — the DFS walk.
- `[src/watch.rs:InotifyWatch]` / `[src/watch.rs:impl Watch for InotifyWatch]` — the pull-based change feed.

## Invariants
- `[src/lib.rs:#![no_std]]` / `[src/lib.rs:#[cfg(target_os = "linux")]]` — `no_std`, Linux-only body; off-target the crate is empty.
- `[src/watch.rs:ChangeBatch::Rescan]` — `IN_Q_OVERFLOW` or a watch-limit hit yields a rescan; events are translated into `UsnRecord` changes with the shared reason bits.
- `[src/walk.rs:scan]` — a single shared listing-id counter; children are resolved by id.
- `[src/watch.rs:IN_NONBLOCK]` / `[src/watch.rs:IN_CLOEXEC]` — non-blocking reads; descriptors do not outlive the process.

## Side Effects
- `[src/walk.rs:open]`, `[src/walk.rs:close]`, `[src/walk.rs:syscall]` — hand-declared kernel calls; bulk directory reads.
- `[src/walk.rs:rgy_discovery::dir::stat]` — per-entry metadata through the seam.
- `[src/watch.rs:inotify_init1]`, `[src/watch.rs:inotify_add_watch]`, `[src/watch.rs:read]`, `[src/watch.rs:close]` — hand-declared inotify calls; `[src/watch.rs:impl Drop for InotifyWatch]` closes descriptors.

## Verification
- Run: `cargo test --workspace --no-fail-fast --locked`
