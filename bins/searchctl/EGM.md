# bins/searchctl

## Overview

Crate root of the merged name-search CLI. The directory has no source of its
own: `[Cargo.toml:package.name]` is `rgy-search` at
`[Cargo.toml:package.version]` 0.1.10, `[Cargo.toml:package.edition]` 2024 and
`[Cargo.toml:package.rust-version]` 1.89; `[Cargo.toml:package.description]`
calls it "The merged name-search CLI (name tokens + zen-rs filters)".
It declares one binary target, `[Cargo.toml:[[bin]].name]` = `searchctl` at
`[Cargo.toml:[[bin]].path]` = `src/main.rs` — the shipped binary keeps its
name while the package is `rgy-*`. All implementation is in the
[src](./src/EGM.md) child contract.

## Subdirectories & Boundaries
- [src](./src/EGM.md) — the CLI implementation in `main.rs`: argv parsing, index load/build, local and daemon query paths, content grep, JSON/plain output and the fzf picker.

## Interface
- `[Cargo.toml:[[bin]].name]` — `searchctl`, built from `[Cargo.toml:[[bin]].path]` (`src/main.rs`).
- `[Cargo.toml:package.name]` — `rgy-search`; `[Cargo.toml:package.version]` 0.1.10; `[Cargo.toml:package.edition]` 2024; `[Cargo.toml:package.rust-version]` 1.89.
- `[Cargo.toml:dependencies.rgy-discovery]`, `[Cargo.toml:dependencies.rgy-adapters]`, `[Cargo.toml:dependencies.rgy-discovery-dispatch]`, `[Cargo.toml:dependencies.rgy-engine]`, `[Cargo.toml:dependencies.rgy-name-core]`, `[Cargo.toml:dependencies.rgy-par]`, `[Cargo.toml:dependencies.rgy-pick]`, `[Cargo.toml:dependencies.rgy-proto]`, `[Cargo.toml:dependencies.mimalloc]` — the nine dependency entries.
- Rolled up from [src](./src/EGM.md): [main.rs:fn main], [main.rs:struct Opts], [main.rs:fn run], [main.rs:fn obtain_index], [main.rs:fn widths], [main.rs:fn run_pick], [main.rs:fn run_daemon], [main.rs:struct Printed], [main.rs:fn daemon_name_lines], [main.rs:fn daemon_content_lines].

## Invariants
- `[Cargo.toml:[[bin]]]` — exactly one binary target, `searchctl`; `[Cargo.toml:dependencies.mimalloc]` is the single external crate and the binary's global allocator ([main.rs:static GLOBAL]).
- Rolled up from [src](./src/EGM.md): [main.rs:fn usage] (help exits 0, bad invocation 2; rg-compatible 0/1/2 codes), [main.rs:fn clamp_limit] (cap at `rgy_proto::MAX_LIMIT`), [main.rs:fn widths] (one shared worker-width answer), [main.rs:fn obtain_index] (root pinned in a sidecar, `--reindex`/`--max-age`), [main.rs:fn daemon_name_lines] / [main.rs:fn print_selection] (one path per line, uncarryable paths counted), [main.rs:fn run] (parse warnings fail closed).

## Side Effects
- Rolled up from [src](./src/EGM.md): [main.rs:fn main] (console code page, adapter view, `USERPROFILE`/`HOME`, stdout/stderr, `std::process::exit`), [main.rs:fn obtain_index] (portable/MFT walk, on-disk index load/save), [main.rs:fn run_pick] and [main.rs:fn print_selection] (external picker process, NUL-separated records), [main.rs:fn run_daemon] (pipe/UDS client via `rgy_proto::Client::connect`), [main.rs:fn run] (file reads for content grep, JSON/plain output).

## Verification
- Run: `cargo test --workspace --no-fail-fast --locked`
