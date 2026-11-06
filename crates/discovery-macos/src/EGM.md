# rgy-discovery-macos src — the macOS walk and FSEvents feed

## Overview
The macOS discovery backend: a `getattrlistbulk` walk
(`[walk.rs:scan]`) and FSEvents watch (`[fsevents.rs:watch]`) written here
from Apple's published APIs, plus platform-independent decision modules (`[event.rs:Event]`,
`[feed.rs:FseventsWatch]`, `[record.rs:records]`). `[lib.rs:#![no_std]]`
makes the crate `no_std` + `alloc`, with the paths, channel and locks taken
from the tree itself (`[feed.rs:Receiver]`, `[walk.rs:Mutex]`,
`[walk.rs:OnceCell]`). The `std` feature is only the parallel walk's thread
fan-out. The type-checked, macOS-only bodies are `[lib.rs:mod fsevents]` and
`[lib.rs:mod walk]`; the module doc states the walk, the FSEvents stream and
their flags have never been exercised on a real kernel.

## Interface
- `[lib.rs:Event]`, `[lib.rs:HISTORY_DONE]`, `[lib.rs:ITEM_IS_DIR]`, `[lib.rs:KERNEL_DROPPED]`, `[lib.rs:MUST_SCAN_SUBDIRS]`, `[lib.rs:USER_DROPPED]` — re-exports from `[event.rs:Event]` and its flag constants.
- `[lib.rs:FseventsWatch]`, `[lib.rs:Grain]`, `[lib.rs:KeepAlive]` — re-exports from `[feed.rs:FseventsWatch]`, `[feed.rs:Grain]`, `[feed.rs:KeepAlive]`.
- `[lib.rs:current_id]`, `[lib.rs:watch]`, `[lib.rs:list_one]`, `[lib.rs:scan]` — `#[cfg(target_os = "macos")]` re-exports from `[fsevents.rs:watch]` / `[fsevents.rs:current_id]` / `[walk.rs:scan]` / `[walk.rs:list_one]`.
- `[event.rs:Event]` — one FSEvents event; `[event.rs:KERNEL_DROPPED]`, `[event.rs:USER_DROPPED]`, `[event.rs:MUST_SCAN_SUBDIRS]`, `[event.rs:HISTORY_DONE]`, `[event.rs:ITEM_IS_DIR]`.
- `[feed.rs:FseventsWatch]` — `[feed.rs:impl Watch for FseventsWatch]`; `[feed.rs:records_for]`, `[feed.rs:is_under]`, `[feed.rs:is_folder]`, `[feed.rs:KeepAlive]`, `[feed.rs:Grain]`.
- `[record.rs:word32]`, `[record.rs:word64]`, `[record.rs:MIN_RECORD]`, `[record.rs:records]` — checked readers for `getattrlistbulk` records.
- `[walk.rs:scan]`, `[walk.rs:list_one]`, `[walk.rs:SKIP]`, `[walk.rs:DENIED]`, `[walk.rs:LISTED]`, `[walk.rs:blocked]`.
- `[fsevents.rs:current_id]`, `[fsevents.rs:watch]`, `[fsevents.rs:Stream]`, `[fsevents.rs:Api]`.

## Invariants
- `[lib.rs:#![no_std]]` — `no_std` + `alloc`; `[lib.rs:extern crate std]` is `#[cfg(test)]` only.
- `[lib.rs:#[cfg(target_os = "macos")]]` — the walk and FSEvents bodies are macOS-only; off-target the crate compiles empty and the portable walk is the Source.
- `[record.rs:records]` — every read is checked and a record that does not fit ends the listing, so a kernel that disagrees with itself cannot panic a file walk; `[record.rs:MIN_RECORD]` is the smallest a record can be.
- `[feed.rs:is_folder]` / `[feed.rs:records_for]` — the granularity and root decisions are plain functions over `[event.rs:Event]`, testable on every platform rather than only on a Mac.
- `[feed.rs:KeepAlive]` — a handle the feed must keep alive; dropping the FSEvents stream stops the feed, so the adapter owns it.
- `[fsevents.rs:CREATE_FLAG_NO_DEFER]` — the stream uses `kFSEventStreamCreateFlagNoDefer`, not `...FileEvents`, so it stays directory-granular (names the folder whose contents changed).
- `[walk.rs:scan]` — children are opened with `[walk.rs:openat]` relative to the parent's fd, so paths are never rebuilt and PATH_MAX never bites; `[walk.rs:LISTED]` counts the entries listed so far (the first scan's progress), while `[walk.rs:Ctx::next_id]` is the shared listing id counter.

## Side Effects
- `[walk.rs:open]`, `[walk.rs:openat]`, `[walk.rs:close]`, `[walk.rs:getattrlistbulk]` — kernel calls declared by hand in `[walk.rs:unsafe extern "C"]`; `[walk.rs:scan]` fans directory reads across `rgy-par` threads when `std` is on.
- `[fsevents.rs:dlopen]` / `[fsevents.rs:dlsym]` / `[fsevents.rs:RTLD_LAZY]` — the FSEvents framework is loaded dynamically and its symbols resolved on first use; `[fsevents.rs:api]` caches the resolved `Api`.
- `[fsevents.rs:on_events]` — the framework callback pushes batches into an `[fsevents.rs:Sender]`.
- `[feed.rs:getcwd]` / `[feed.rs:working_directory]` — the working directory is resolved with a hand-declared `getcwd` (the crate has no `std::env` to reach for).
- `[walk.rs:DENIED]` / `[walk.rs:SKIP]` — mutable global state guarded by `[walk.rs:Mutex]` / `[walk.rs:OnceCell]`.

## Verification
- Run: `cargo test --workspace --no-fail-fast --locked`
