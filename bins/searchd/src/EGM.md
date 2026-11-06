# bins/searchd/src

## Overview

Source of the `searchd` binary, the merged search daemon ([main.rs:fn main]).
It indexes `--root` once at startup and answers JSON-lines requests over three
transports (stdio by default, `--listen uds:` with a path on Unix, `--listen
pipe:` with a name on Windows) and two framings (the proto lines shared with
`searchctl`, and `--serve`, the rgx compatibility front) ([main.rs:fn run_stream],
[main.rs:fn serve_stream], [main.rs:use rgy_proto::{CatchUp, GrepFile, Hit as WireHit, Request, Response}]).
The index stays warm in memory through `rgy_engine::keep_warm`
([main.rs:use rgy_engine::{keep_warm, warm_stop}]); content `grep:` runs the
engine's content layer ([main.rs:use rgy_name_core::content::{self, Grep}]) or
the TV pack store when `--tv-dir` is given. A change feed keeps the live index
fresh ([main.rs:fn spawn_watch], [main.rs:fn open_watch]), and a restart resumes
a Windows journal where it left off ([main.rs:struct Journal],
[main.rs:fn startup_plan]). The daemon installs mimalloc as its global
allocator ([main.rs:static GLOBAL]).

## Interface
- [main.rs:fn main] — parses `--root`, `--index`, `--reindex`, `--max-age`, `--tv-dir`, `--listen`, `--serve`, `--threads`, `-h`, `-V` and dispatches to [main.rs:fn run_stream], [main.rs:fn serve_uds] or [main.rs:fn serve_pipe].
- [main.rs:struct Ctx] and [main.rs:impl Ctx] — the shared daemon state (root, an `RwLock` around `Live`, catch-up/feed atomics, sessions, content index).
- [main.rs:fn handle] — dispatches a `rgy_proto::Request` to a vector of [main.rs:enum Answer] values, shaped by [main.rs:fn status_frame].
- [main.rs:fn run_stream] and [main.rs:fn serve_stream] — the proto and `--serve` front loops; [main.rs:fn serve_pattern], [main.rs:fn serve_file], [main.rs:fn serve_end] serve rgx `--json` messages.
- [main.rs:fn open_index], [main.rs:fn startup_plan], [main.rs:enum Startup], [main.rs:struct Ready], [main.rs:struct Journal], [main.rs:fn volume_error_code] — startup index and journal handling.
- [main.rs:fn scoped_content], [main.rs:fn grep], [main.rs:fn content_answer_is_complete], [main.rs:fn unread_files], [main.rs:fn resolve] — content search and path resolution.
- [main.rs:struct Feed] and [main.rs:fn open_watch] (per-platform), [main.rs:fn usn_watch] — live-change feeds; [main.rs:fn spawn_watch], [main.rs:fn spawn_trim], [main.rs:fn spawn_content] — the daemon's threads.
- [main.rs:fn bind_private_socket], [main.rs:fn serve_uds], [main.rs:fn serve_pipe], [main.rs:fn spawn_pipe_acceptor], [main.rs:fn open_pipe_server] — the transport servers.
- [main.rs:fn list_all], [main.rs:fn apply_all], [main.rs:fn folders_of], [main.rs:fn on_disk], [main.rs:fn platform_path], [main.rs:fn same_path] — relisting and path spelling.
- [main.rs:fn usage], [main.rs:fn usage_text], [main.rs:fn parse_age], [main.rs:fn absolute_root], [main.rs:fn now_secs], [main.rs:struct Changed], [main.rs:impl Changed].

## Invariants
- [main.rs:const CATCH_UP_REPLAYED] is 0 and [main.rs:const CATCH_UP_RELISTED] is 1: the status answer says whether a restart replayed a journal or relisted the tree.
- [main.rs:const FEED_NONE] is 0, [main.rs:const FEED_LIVE] is 1 and [main.rs:const FEED_GONE] is 2; [main.rs:const FEED_ARM_WAIT] is 5 s and the feed is armed before the tree is walked again.
- [main.rs:const SERVE_LIMIT] is 100; [main.rs:const REPORT_EVERY] is 500 ms; [main.rs:const FSEVENTS_LATENCY] is 0.5 on macOS.
- [main.rs:struct Ctx] holds `live` in an `RwLock`: readers take `read()`, the updater `write()`, and a searcher holds a read guard for one query.
- [main.rs:fn startup_plan] decides replay vs relist; [main.rs:fn open_index] pins the root in a sidecar and honours `--reindex`, `--max-age` and `persist::DEFAULT_MAX_AGE`.
- [main.rs:fn serve_pattern] reads an argv-array per line and [main.rs:fn serve_end] closes the rgx framing; [main.rs:fn serve_stream] uses [main.rs:const SERVE_LIMIT].
- [main.rs:fn content_answer_is_complete], [main.rs:fn unread_files] and [main.rs:fn scoped_content] keep a scoped content answer within its scope.
- [main.rs:static GLOBAL] installs `mimalloc::MiMalloc` as `#[global_allocator]`.

## Side Effects
- [main.rs:fn main] initialises the console code page (input and output) and the adapter view, reads `USERPROFILE` / `HOME`, pins the worker width with `rgy_par::set_workers`, canonicalises the root with `std::fs::canonicalize`, spawns the watch/trim/content threads, then serves on stdio, a UDS or a named pipe.
- [main.rs:fn open_index] and [main.rs:fn list_all] / [main.rs:fn apply_all] read and write the on-disk index and walk the tree; [main.rs:fn grep] and [main.rs:fn scoped_content] read file bytes for content search.
- [main.rs:fn run_stream] and [main.rs:fn serve_stream] read requests from stdin/line input and write responses to stdout, flushing the writer.
- [main.rs:fn serve_uds] binds a `UnixListener`, sets the socket to mode 0o600, creates the parent dir and removes a stale socket file; [main.rs:fn bind_private_socket] does the bind.
- [main.rs:fn open_pipe_server] calls `CreateNamedPipeW` and `ConnectNamedPipe` and wraps the handle in `std::fs::File::from_raw_handle`; [main.rs:fn spawn_pipe_acceptor] respawns the acceptor on a thread.
- [main.rs:fn open_watch] arms the platform feed (USN journal / directory notifications / FSEvents); [main.rs:fn usn_watch] reads the Windows journal.
- [main.rs:fn spawn_watch], [main.rs:fn spawn_trim] and [main.rs:fn spawn_content] each start a `std::thread::spawn` worker.

## Verification
- Run: `cargo test --workspace --no-fail-fast --locked`
