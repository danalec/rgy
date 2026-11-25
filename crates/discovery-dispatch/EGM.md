# rgy-discovery-dispatch

## Overview
The `Source` selector crate (`[Cargo.toml:package.name]` is
`rgy-discovery-dispatch`, edition 2024). Its root holds only the manifest; the
library is in [src](./src/EGM.md). `[Cargo.toml:feature.std]` is
`std = ["rgy-discovery/std", "rgy-discovery-macos/std"]`, and the
`[Cargo.toml:target.'cfg(windows)'.dependencies]` /
`[Cargo.toml:target.'cfg(target_os = "linux")'.dependencies]` /
`[Cargo.toml:target.'cfg(target_os = "macos")'.dependencies]` /
`[Cargo.toml:target.'cfg(target_os = "freebsd")'.dependencies]` tables pull in
the four platform backends per target so `[src/lib.rs:pick]` can build the
native one on its own OS; every backend is taken with
`default-features = false`.

## Subdirectories & Boundaries
- [src](./src/EGM.md) — the `no_std` selector: `[src/lib.rs:Platform]`, `[src/lib.rs:AnySource]`, `[src/lib.rs:source_for]`, `[src/lib.rs:catch_up_for]`.

## Interface
- `[src/lib.rs:Platform]` / `[src/lib.rs:current_platform]` — the platform named as a value.
- `[src/lib.rs:catch_up_for]` / `[src/lib.rs:catch_up]` — the restart catch-up capability (`CatchUp`).
- `[src/lib.rs:AnySource]` / `[src/lib.rs:source_for]` — the selected `Source`.
- `[src/lib.rs:impl AnySource]` — `backend_name`, `enumerate`, `enumerate_cancellable`, `fallback`.

## Invariants
- `[src/lib.rs:#![no_std]]` / `[src/lib.rs:extern crate std]` — `no_std` + `alloc`, `std` gated by the feature; the portable `[src/lib.rs:fallback]` exists in every build.
- `[src/lib.rs:catch_up_for]` — Windows/macOS `Replayed`, Linux/FreeBSD/Other `Relisted`; stated once and tested on any host rather than only the host the test runs on.
- `[src/lib.rs:backend_name]` — asserted by the differential test before comparison, so a silent fallback cannot hide a broken backend.
- `[src/lib.rs:is_volume_root]` — the MFT walk answers only for a volume root.
- `[src/lib.rs:enumerate_cancellable]` — a refusing native backend falls back to `[src/lib.rs:fallback]`.
- `[Cargo.toml:target.'cfg(windows)'.dependencies]` — each backend is pulled in with `default-features = false` so a backend cannot drag `std` in through a side door.

## Side Effects
- `[src/lib.rs:mft_indexer]` — reads `ERS_USE_RECORDS` from the environment with `std::env::var_os`.
- `[src/lib.rs:try_mft]` — opens and leaks a volume handle (`Volume::open` / `Box::leak`).
- `[src/lib.rs:enumerate]` / `[src/lib.rs:fallback]` — filesystem enumeration via the selected backend or the portable walk.

## Verification
- Run: `cargo test --workspace --no-fail-fast --locked`
