# crates/par

## Overview
The `rgy-par` crate: the workspace's parallelism floor, with no dependencies
at all. Its manifest names the package `rgy-par` (edition 2024) and declares
`default = ["std"]` with an empty `std = []` feature: under `std` the work
runs on a persistent pool of threads, without it in order on the calling
thread. The library target is imported by the query engine as `rgy_par`
(seen in `name-core`'s `sync.rs`).

## Subdirectories & Boundaries
- [src](./src/EGM.md) — the five `.rs` sources: `lib.rs` (parallel shapes),
  `sync.rs` (lock/once-cell), `mpsc.rs` (channel), `cores.rs` and `pool.rs`
  (std-only width and pool).

## Interface
- `[Cargo.toml:package.name]` — package `rgy-par`, imported as `rgy_par`.
- `[lib.rs:workers]`, `[lib.rs:set_workers]`, `[lib.rs:configured]`,
  `[lib.rs:default_workers]` — the width (from `src`).
- `[lib.rs:map_collect]`, `[lib.rs:for_each]`, `[lib.rs:each_index]`,
  `[lib.rs:each_owned]`, `[lib.rs:sort_unstable_by]`, `[lib.rs:join]` — the
  parallel shapes (from `src`).
- `[sync.rs:Mutex]`, `[sync.rs:OnceCell]`, `[mpsc.rs:channel]` — the `no_std`
  primitives (from `src`).

## Invariants
- `[lib.rs:no_std]` — `#![no_std]` + `extern crate alloc`; `std` only behind
  `#[cfg(feature = "std")]` (from `src`).
- `[Cargo.toml:features.std]` — `std = []`; the `no_std` floor drags no
  dependency in.
- `[lib.rs:workers]` — the environment is read once and the width cached; the
  order is `set_workers`, `RGY_NUM_THREADS`, then the machine's cores (from
  `src`).
- `[cores.rs:smt_width]` — the width is cores, not logical processors: a core
  with two threads is one core (from `src`).
- `[sync.rs:pub use std::sync::Mutex]` — under `std` the primitives are
  `std::sync`'s; without it the crate defines its own spinning lock (from
  `src`).

## Side Effects
- `[lib.rs:default_workers]` — under `std` calls
  `std::thread::available_parallelism()` (from `src`).
- `[lib.rs:configured]` — under `std` reads `RGY_NUM_THREADS` (from `src`).
- `[pool.rs:run]`, `[pool.rs:spawn]` — under `std` spawns and keeps worker
  threads (from `src`).
- `[cores.rs:smt_width]` — under `std` reads the platform's processor topology
  (from `src`).

## Verification
- Run: `cargo test --workspace --no-fail-fast --locked`
