# rgy-discovery-macos

## Overview
The macOS discovery backend crate (`[Cargo.toml:package.name]` is
`rgy-discovery-macos`, edition 2024). Its root holds only the manifest; the
library is in [src](./src/EGM.md). `[Cargo.toml:feature.std]` is
`std = ["rgy-par/std"]` — with it the `getattrlistbulk` walk's workers are
threads, without it there is one worker on the calling thread. The manifest
pulls in `[Cargo.toml:rgy-discovery]` and `[Cargo.toml:rgy-par]` with
`default-features = false`, and lists `[Cargo.toml:dev-dependencies]` for the
tests that build a name index and apply the feed's records to it.

## Subdirectories & Boundaries
- [src](./src/EGM.md) — `[src/walk.rs:scan]` (the `getattrlistbulk` walk), `[src/fsevents.rs:watch]` (the FSEvents stream), and the platform-independent `[src/event.rs:Event]` / `[src/feed.rs:FseventsWatch]` / `[src/record.rs:records]` decision modules.

## Interface
- `[src/lib.rs:FseventsWatch]`, `[src/lib.rs:Grain]`, `[src/lib.rs:KeepAlive]` — the pull-based feed adapter re-exports.
- `[src/lib.rs:Event]` and the flag re-exports `[src/lib.rs:KERNEL_DROPPED]`, `[src/lib.rs:MUST_SCAN_SUBDIRS]`, `[src/lib.rs:ITEM_IS_DIR]`.
- `[src/lib.rs:scan]`, `[src/lib.rs:list_one]`, `[src/lib.rs:watch]`, `[src/lib.rs:current_id]` — macOS-only walk and stream re-exports.
- `[src/feed.rs:records_for]`, `[src/feed.rs:is_under]`, `[src/feed.rs:is_folder]`, `[src/record.rs:word32]`, `[src/record.rs:records]`.

## Invariants
- `[src/lib.rs:#![no_std]]` — `no_std` + `alloc`; `[src/lib.rs:extern crate std]` is `#[cfg(test)]` only.
- `[src/lib.rs:#[cfg(target_os = "macos")]]` — the walk and FSEvents bodies are macOS-only; off-target the crate compiles empty.
- `[src/record.rs:records]` — every record read is checked; a record that does not fit ends the listing rather than panicking.
- `[src/feed.rs:KeepAlive]` — dropping the stream stops the feed, so the adapter owns it.
- `[src/fsevents.rs:CREATE_FLAG_NO_DEFER]` — the stream stays directory-granular (no `FileEvents` flag).
- `[src/feed.rs:Grain]` / `[src/feed.rs:is_folder]` — the granularity and root decisions are plain functions over `[src/event.rs:Event]`, tested on every platform.

## Side Effects
- `[src/walk.rs:openat]`, `[src/walk.rs:getattrlistbulk]`, `[src/walk.rs:open]`, `[src/walk.rs:close]` — kernel calls declared by hand; `[src/walk.rs:scan]` fans directory reads across `rgy-par` threads with `std`.
- `[src/fsevents.rs:dlopen]` / `[src/fsevents.rs:dlsym]` — the FSEvents framework is loaded dynamically; `[src/fsevents.rs:on_events]` is the callback that pushes batches.
- `[src/feed.rs:getcwd]` / `[src/feed.rs:working_directory]` — resolves the working directory via a hand-declared `getcwd`.

## Verification
- Run: `cargo test --workspace --no-fail-fast --locked`
