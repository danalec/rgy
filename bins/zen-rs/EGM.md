# bins/zen-rs

## Overview

Crate root of the zen-rs CLI/TUI. The directory has no source of its own:
`[Cargo.toml:package.name]` is `rgy-zen` at
`[Cargo.toml:package.version]` 0.1.10, `[Cargo.toml:package.edition]` 2024 and
`[Cargo.toml:package.rust-version]` 1.89; `[Cargo.toml:package.description]`
calls it "The zen-rs CLI/TUI, unchanged behaviour, now built on the rgy
workspace crates". It declares one binary target,
`[Cargo.toml:[[bin]].name]` = `zen-rs` at
`[Cargo.toml:[[bin]].path]` = `src/main.rs`. All implementation is in the
[src](./src/EGM.md) child contract.

## Subdirectories & Boundaries
- [src](./src/EGM.md) — the CLI implementation in `main.rs`: NTFS MFT/USN indexing, index save/load, volume and record inspection, the interactive UI and the query/list commands.

## Interface
- `[Cargo.toml:[[bin]].name]` — `zen-rs`, built from `[Cargo.toml:[[bin]].path]` (`src/main.rs`).
- `[Cargo.toml:package.name]` — `rgy-zen`; `[Cargo.toml:package.version]` 0.1.10; `[Cargo.toml:package.edition]` 2024; `[Cargo.toml:package.rust-version]` 1.89.
- `[Cargo.toml:dependencies.rgy-name-core]`, `[Cargo.toml:dependencies.rgy-discovery]`, `[Cargo.toml:dependencies.rgy-discovery-windows]`, `[Cargo.toml:dependencies.rgy-search-ui]`, `[Cargo.toml:dependencies.rgy-engine]`, `[Cargo.toml:dependencies.rgy-pick]`, `[Cargo.toml:dependencies.mimalloc]` — the seven dependency entries.
- Rolled up from [src](./src/EGM.md): [main.rs:fn main] (both `#[cfg(windows)]` and `#[cfg(not(windows))]`), [main.rs:fn usage_text], [main.rs:fn cmd_index], [main.rs:fn cmd_save], [main.rs:fn cmd_ui], [main.rs:fn cmd_search], [main.rs:fn cmd_tail], [main.rs:fn build_index], [main.rs:fn load_or_build], [main.rs:use rgy_search_ui as ui].

## Invariants
- `[Cargo.toml:[[bin]]]` — exactly one binary target, `zen-rs`; `[Cargo.toml:dependencies.mimalloc]` is the binary's global allocator ([main.rs:static GLOBAL]).
- `[Cargo.toml:dependencies.rgy-discovery-windows]` is the Windows-only indexer backend; rolled up from [src](./src/EGM.md): [main.rs:fn main] (`#[cfg(not(windows))]`) answers only help/version off-Windows, [main.rs:fn usage_text] is the same on every platform.
- Rolled up from [src](./src/EGM.md): [main.rs:fn now_secs] (u32 seconds since `UNIX_EPOCH`), [main.rs:fn full_path] (root + relative path, same as `searchd`), [main.rs:fn query_home] (`USERPROFILE`), [main.rs:fn build_index] (`ERS_USE_RECORDS` chooses streamed records), [main.rs:fn cmd_tail] (reports when journal history is lost and a full rescan is due).

## Side Effects
- Rolled up from [src](./src/EGM.md): [main.rs:fn main] (argv reads, stdout/stderr, `std::process::exit`), [main.rs:fn cmd_save] / [main.rs:fn load_or_build] (on-disk index blob via `persist::save` / `persist::load_saved`), [main.rs:fn cmd_record] / [main.rs:fn cmd_index] / [main.rs:fn cmd_volumes] / [main.rs:fn cmd_tail] / [main.rs:fn build_index] (NTFS volume handles via `Volume::open`, MFT walks via `Indexer`, USN journal via `seam::UsnWatch`), [main.rs:fn cmd_search] / [main.rs:fn cmd_list] (path output, `rgy_pick::uncarryable_paths`), [main.rs:fn query_home] (environment read), [main.rs:fn cmd_ui] (terminal UI via `ui::run`).

## Verification
- Run: `cargo test --workspace --no-fail-fast --locked`
