# bins/zen-rs/src

## Overview

Source of the `zen-rs` binary, the NTFS file indexer built on the MFT and the
USN journal ([main.rs:fn main], [main.rs:use rgy_discovery_windows::{indexer, ntfs, seam, volume}]).
The monitor drives `FSCTL_READ_USN_JOURNAL` (0x900bb) and tails journal
deltas; one on-disk index format (the name index blob FSIDX015) is built from
MFT rows and saved with a root stamp beside it, then reopened by `load`, `ui`,
`search` and `list` ([main.rs:fn build_index], [main.rs:fn load_or_build],
[main.rs:use rgy_name_core::persist]). Everything that reads a volume is gated
`#[cfg(windows)]`; on other platforms only help and version remain
([main.rs:fn main] under `#[cfg(not(windows))]`, [main.rs:fn usage_text]).
The interactive UI is `rgy_search_ui` ([main.rs:use rgy_search_ui as ui],
[main.rs:fn cmd_ui]). The binary installs mimalloc as its global allocator
([main.rs:static GLOBAL]).

## Interface
- [main.rs:fn main] (`#[cfg(windows)]`) — dispatches `index`, `index-all`, `volumes`, `search-all`, `save`, `load`, `ui`, `record`, `list`, `search`, `tail`, `-h`, `-V`; [main.rs:fn main] (`#[cfg(not(windows))]`) answers only help/version.
- [main.rs:fn usage], [main.rs:fn usage_text], [main.rs:fn version] — help text and version.
- [main.rs:fn cmd_record], [main.rs:fn cmd_save], [main.rs:fn cmd_index], [main.rs:fn cmd_index_all], [main.rs:fn cmd_volumes], [main.rs:fn cmd_ui], [main.rs:fn cmd_search], [main.rs:fn cmd_search_all], [main.rs:fn cmd_list], [main.rs:fn cmd_tail] — the subcommands.
- [main.rs:fn now_secs], [main.rs:fn full_path], [main.rs:fn query_home], [main.rs:fn search_paths], [main.rs:fn build_index], [main.rs:fn load_or_build] — index building, path spelling and query helpers.
- [main.rs:use indexer::Indexer], [main.rs:use volume::Volume], [main.rs:use rgy_name_core::live::Live], [main.rs:use rgy_name_core::name_index::Index], [main.rs:use rgy_name_core::name_query::{Alternatives, Searcher, merge_hits}].

## Invariants
- [main.rs:fn usage_text] is the same on every platform and says where the tool cannot run; [main.rs:fn main] (`#[cfg(not(windows))]`) exits 2 for any command but help/version.
- [main.rs:fn now_secs] is seconds since `UNIX_EPOCH` as `u32`, the index's timestamp base, "same as searchd".
- [main.rs:fn full_path] composes root plus the index-relative path with the same spelling `searchd` puts on the wire.
- [main.rs:fn query_home] resolves `~` against `USERPROFILE`.
- [main.rs:fn build_index] reads `ERS_USE_RECORDS` to choose streamed MFT records; [main.rs:fn load_or_build] either `persist::load_saved` or opens a `Volume` and builds.
- [main.rs:fn cmd_tail] applies pending USN journal records through `seam::UsnWatch::new` and says when history was lost at the root (a full rescan is due).
- [main.rs:static GLOBAL] installs `mimalloc::MiMalloc` as `#[global_allocator]`.

## Side Effects
- [main.rs:fn main] reads `std::env::args()`, writes help/version/errors to stdout and stderr and exits with `std::process::exit`; [main.rs:fn cmd_search], [main.rs:fn cmd_list] and [main.rs:fn cmd_search_all] print matched paths and report uncarryable ones via `rgy_pick::uncarryable_paths`.
- [main.rs:fn cmd_save] and [main.rs:fn load_or_build] write/read the index blob on disk through `persist::save` / `persist::load_saved`.
- [main.rs:fn cmd_record], [main.rs:fn cmd_index], [main.rs:fn cmd_index_all], [main.rs:fn cmd_volumes], [main.rs:fn cmd_tail] and [main.rs:fn build_index] open NTFS volume handles with `Volume::open`, walk MFT rows through `Indexer` and drive the USN journal through `seam::UsnWatch`.
- [main.rs:fn build_index] reads `ERS_USE_RECORDS`; [main.rs:fn query_home] reads `USERPROFILE`.
- [main.rs:fn cmd_ui] runs the terminal UI over `ui::run(Live::with_root(idx, ...))`.

## Verification
- Run: `cargo test --workspace --no-fail-fast --locked`
