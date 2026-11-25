# bins/searchd

## Overview

Crate root of the merged search daemon. The directory has no source of its
own: `[Cargo.toml:package.name]` is `rgy-searchd` at
`[Cargo.toml:package.version]` 0.1.10, `[Cargo.toml:package.edition]` 2024 and
`[Cargo.toml:package.rust-version]` 1.89; `[Cargo.toml:package.description]`
calls it "The merged search daemon: JSON-lines over stdio/pipe/UDS, plus the
rgx --serve front". It declares one binary target,
`[Cargo.toml:[[bin]].name]` = `searchd` at
`[Cargo.toml:[[bin]].path]` = `src/main.rs`. All implementation is in the
[src](./src/EGM.md) child contract.

## Subdirectories & Boundaries
- [src](./src/EGM.md) — the daemon implementation in `main.rs`: warm index, JSON-lines request loop, rgx `--serve` front, live-change feed, stdio/UDS/pipe transports and the keep-warm/content threads.

## Interface
- `[Cargo.toml:[[bin]].name]` — `searchd`, built from `[Cargo.toml:[[bin]].path]` (`src/main.rs`).
- `[Cargo.toml:package.name]` — `rgy-searchd`; `[Cargo.toml:package.version]` 0.1.10; `[Cargo.toml:package.edition]` 2024; `[Cargo.toml:package.rust-version]` 1.89.
- `[Cargo.toml:dependencies.rgy-discovery]`, `[Cargo.toml:dependencies.rgy-discovery-dispatch]`, `[Cargo.toml:dependencies.rgy-engine]`, `[Cargo.toml:dependencies.rgy-name-core]`, `[Cargo.toml:dependencies.rgy-par]`, `[Cargo.toml:dependencies.rgy-proto]`, `[Cargo.toml:dependencies.rgx-core]` (`package = "rgy-vendored-core"`), `[Cargo.toml:dependencies.rgy-adapters]`, `[Cargo.toml:dependencies.mimalloc]` — the nine dependency entries.
- `[Cargo.toml:target.'cfg(windows)'.dependencies.rgy-discovery-windows]`, `[Cargo.toml:target.'cfg(target_os = "linux")'.dependencies.rgy-discovery-linux]`, `[Cargo.toml:target.'cfg(target_os = "macos")'.dependencies.rgy-discovery-macos]` — per-platform live-change feed implementations for `open_watch`.
- Rolled up from [src](./src/EGM.md): [main.rs:fn main], [main.rs:struct Ctx], [main.rs:fn handle], [main.rs:fn run_stream], [main.rs:fn serve_stream], [main.rs:fn open_index], [main.rs:fn startup_plan], [main.rs:fn scoped_content], [main.rs:fn open_watch], [main.rs:fn serve_uds], [main.rs:fn serve_pipe].

## Invariants
- `[Cargo.toml:[[bin]]]` — exactly one binary target, `searchd`; `[Cargo.toml:dependencies.mimalloc]` is the binary's global allocator ([main.rs:static GLOBAL]).
- `[Cargo.toml:target.'cfg(windows)'.dependencies]`, `[Cargo.toml:target.'cfg(target_os = "linux")'.dependencies]` and `[Cargo.toml:target.'cfg(target_os = "macos")'.dependencies]` are per-platform, matching the `#[cfg]`-gated [main.rs:fn open_watch] halves.
- Rolled up from [src](./src/EGM.md): [main.rs:const CATCH_UP_REPLAYED] / [main.rs:const CATCH_UP_RELISTED] (status says replay vs relist), [main.rs:const FEED_NONE] / [main.rs:const FEED_LIVE] / [main.rs:const FEED_GONE] (feed states), [main.rs:const FEED_ARM_WAIT] (5 s, feed armed before relist), [main.rs:const SERVE_LIMIT] (100), [main.rs:fn startup_plan] and [main.rs:fn open_index] (root pinned in sidecar), [main.rs:struct Ctx] (an `RwLock` around `Live`, readers/writer).

## Side Effects
- Rolled up from [src](./src/EGM.md): [main.rs:fn main] (console code page, adapter view, env reads, worker-width pin, root canonicalisation, thread spawns), [main.rs:fn open_index] / [main.rs:fn list_all] (on-disk index and tree walk), [main.rs:fn grep] / [main.rs:fn scoped_content] (file reads), [main.rs:fn run_stream] / [main.rs:fn serve_stream] (stdin reads, stdout writes, flush), [main.rs:fn serve_uds] (Unix socket bind, 0o600 permissions, stale socket removal), [main.rs:fn open_pipe_server] (`CreateNamedPipeW` / `ConnectNamedPipe`), [main.rs:fn open_watch] (platform change feeds), [main.rs:fn spawn_watch] / [main.rs:fn spawn_trim] / [main.rs:fn spawn_content] (threads).

## Verification
- Run: `cargo test --workspace --no-fail-fast --locked`
