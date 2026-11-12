# crates/proto

## Overview
The `rgy-proto` crate: the JSON-lines protocol's wire types and the std
client/endpoint that reach a running daemon. Its manifest names the package
`rgy-proto` (edition 2024) and declares `default = ["std"]` with an empty
`std = []` feature; there are no runtime dependencies. The bare-metal gate
builds it with the feature off so the wire types stay linkable into an
embedded search box.

## Subdirectories & Boundaries
- [src](./src/EGM.md) — the three `.rs` sources: wire types in `lib.rs`, the
  daemon-location rules in `endpoint.rs`, and the std client in `client.rs`.

## Interface
- `[Cargo.toml:package.name]` — package `rgy-proto`; consumers import it as
  `rgy_proto` (seen in `rgy-autostart`'s `rgy_proto::endpoint::cache_dir_from`).
- `[lib.rs:Request]`, `[lib.rs:Response]`, `[lib.rs:CatchUp]`,
  `[lib.rs:Feed]`, `[lib.rs:Hit]`, `[lib.rs:LineError]` — the wire types
  (from `src`).
- `[lib.rs:pub use endpoint::Endpoint]`, `[endpoint.rs:Endpoint]` — the daemon
  endpoint enum (from `src`).
- `[client.rs:Client]` — the std client (from `src`).

## Invariants
- `[lib.rs:no_std]` — `#![no_std]` + `extern crate alloc`; `std` is declared
  only behind `#[cfg(any(test, feature = "std"))]` (from `src`).
- `[Cargo.toml:features.std]` — `std = []`; the machine-free protocol has no
  dependency to turn on.
- `[lib.rs:pub mod client]` — `client` is `#[cfg(feature = "std")]` (from `src`).
- `[lib.rs:WIRE_LIMIT]` — `1 << 53`, the last integer the wire carries exactly
  (from `src`).

## Side Effects
- `[client.rs:Client]` — opens a stdio / unix-socket / Windows-named-pipe
  transport to the daemon (from `src`).
- `[endpoint.rs:pipe_name]` — reads `USERNAME`; `[endpoint.rs:cache_dir]` —
  reads `XDG_CACHE_HOME`, `HOME`/`USERPROFILE`, `LOCALAPPDATA` (from `src`).

## Verification
- Run: `cargo test --workspace --no-fail-fast --locked`
