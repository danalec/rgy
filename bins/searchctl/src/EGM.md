# bins/searchctl/src

## Overview

Source of the `searchctl` binary, the merged name-search CLI ([main.rs:fn main],
[main.rs:use rgy_discovery_dispatch::source_for]). It combines one-shot
name-search ergonomics with the zen-rs filter vocabulary over the v2 name index
([main.rs:use rgy_name_core::name_index::Index],
[main.rs:use rgy_name_core::name_query::{Alternatives, Hit, Searcher, merge_hits}]).
The tree is enumerated with a portable walk (no privileges) or the Windows MFT
fast path via `source_for`; content `grep:` runs the engine's content layer
([main.rs:use rgy_name_core::content::{self, Grep}]). A built index can be
loaded and saved through [main.rs:use rgy_name_core::persist]; the daemon
client and the JSON escaper come from `rgy_proto`
([main.rs:fn run_daemon], [main.rs:fn daemon_name_lines]). The picker hands
results to an external fzf ([main.rs:fn run_pick], [main.rs:fn print_selection]).
The binary installs mimalloc as its global allocator ([main.rs:static GLOBAL]).

## Interface
- [main.rs:fn main] — entry point; parses [main.rs:struct Opts] from argv (`--root`, `--limit`, `--index`, `--reindex`, `--count`, `--explain`, `--threads`, `--max-age`, `--json`, `--pick`, `--daemon`, `--fzf-binary`, `--fzf-arg`, `-h`, `-V`) and calls [main.rs:fn run].
- [main.rs:fn usage] and [main.rs:fn help] — help/usage text and exit status.
- [main.rs:fn parse_age] and [main.rs:fn clamp_limit] — parse `--max-age` and cap the limit.
- [main.rs:fn widths] — scan and query worker widths from the flag, `RGY_NUM_THREADS` and the defaults.
- [main.rs:fn obtain_index] — load or build and save the name index.
- [main.rs:fn run], [main.rs:fn run_pick], [main.rs:fn run_daemon] — the local, picker and daemon execution paths.
- [main.rs:fn hit_rel], [main.rs:fn hit_path], [main.rs:fn is_folder], [main.rs:fn spelled_as_folder], [main.rs:fn marked], [main.rs:fn folder_field] — hit spelling and folder marking.
- [main.rs:struct Printed], [main.rs:fn daemon_name_lines], [main.rs:fn daemon_content_lines], [main.rs:fn print_lines], [main.rs:fn print_selection] — output shaping.
- [main.rs:fn daemon_name_note], [main.rs:fn local_name_note], [main.rs:fn age_of], [main.rs:fn now_secs] — notes and timestamps.

## Invariants
- [main.rs:fn usage] exits 0 for `--help` and 2 for a bad invocation; the module docs fix rg-compatible exit codes 0 = matches, 1 = no matches, 2 = error.
- [main.rs:fn clamp_limit] caps `--limit` at `rgy_proto::MAX_LIMIT`.
- [main.rs:fn widths] asks the flag, then `RGY_NUM_THREADS`, then the defaults once, so scan and query never take two different answers.
- [main.rs:fn obtain_index] pins the root in a sidecar via `persist::load_for` / `persist::save`; `--reindex` rebuilds, `--max-age` expires, and an index with no `--max-age` expires after `persist::DEFAULT_MAX_AGE`.
- [main.rs:fn daemon_name_lines], [main.rs:fn daemon_content_lines] and [main.rs:fn print_selection] keep one path per line: a path carrying a line break is left out by `rgy_pick::carries_line` and counted with `rgy_pick::uncarryable_paths`.
- [main.rs:fn run] is fail-closed on parse warnings: an unknown `type:` or bad `re:` is a `warning:` line on stderr and the affected alternative matches nothing.
- [main.rs:static GLOBAL] installs `mimalloc::MiMalloc` as `#[global_allocator]`; [main.rs:const REPORT_EVERY] is 500 ms.

## Side Effects
- [main.rs:fn main] initialises the console code page and the adapter view, reads `USERPROFILE` / `HOME`, writes help/version/errors to stdout and stderr, and exits with `std::process::exit`.
- [main.rs:fn obtain_index] walks the tree through `source_for` and `interrupt::cancel_on_interrupt`, and reads/writes the index file on disk through `persist::load_for` and `persist::save`.
- [main.rs:fn run_pick] and [main.rs:fn print_selection] run `rgy_pick::pick_records` (an external picker), write NUL-separated records to it and print the chosen paths to stdout.
- [main.rs:fn run_daemon] connects to the registered endpoint with `rgy_proto::endpoint::Endpoint::registered()` and `rgy_proto::Client::connect`, and writes requests/reads answers on the pipe or socket.
- [main.rs:fn run] reads files for content `grep:` through `Grep::new` and writes JSON or plain hits to stdout via `rgy_engine::outln!`.

## Verification
- Run: `cargo test --workspace --no-fail-fast --locked`
