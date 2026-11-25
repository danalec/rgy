# rgy-discovery-dispatch src — the Source selector

## Overview
The `Source` selector: by `cfg` it picks the native walk for the platform the
build targets, with the portable walk as the answer everywhere else and as the
fallback whenever a native backend refuses. It also names, as a value, what
each platform's change feed can do about a restart (`[lib.rs:catch_up_for]`).
`[lib.rs:#![no_std]]` makes the crate `no_std` + `alloc`;
`[lib.rs:extern crate std]` is gated behind `#[cfg(feature = "std")]` and buys
the environment and the threads. The crate exists because the platform
backends depend on `discovery` for the traits, so the selector that knows all
of them cannot live there without a dependency cycle.

## Interface
- `[lib.rs:Platform]` — enum `Windows`, `MacOs`, `Linux`, `FreeBsd`, `Other`; `[lib.rs:current_platform]`.
- `[lib.rs:catch_up_for]` / `[lib.rs:catch_up]` — the restart catch-up capability returning `[lib.rs:rgy_proto::CatchUp]` (`Replayed` / `Relisted`).
- `[lib.rs:AnySource]` — the selected `Source`; `[lib.rs:source_for]` builds it from a root and thread count.
- `[lib.rs:impl AnySource]` — `backend_name`, `enumerate`, `enumerate_cancellable`, `fallback`.
- `[lib.rs:Kind]` — the internal backend pick (`Portable`, `Linux`, `Macos`, `FreeBsd`, `Mft`); `[lib.rs:MacSource]` — the thinnest adapter over `[lib.rs:rgy_discovery_macos::scan]`.
- `[lib.rs:pick]` — the `cfg`-driven backend choice; `[lib.rs:try_mft]`, `[lib.rs:mft_indexer]`, `[lib.rs:is_volume_root]` — Windows MFT selection helpers behind `#[cfg(windows)]`.

## Invariants
- `[lib.rs:#![no_std]]` — `no_std` + `alloc`; `[lib.rs:extern crate std]` is gated, and the portable `[lib.rs:fallback]` exists in every build, on every target.
- `[lib.rs:catch_up_for]` — Windows/macOS answer `CatchUp::Replayed`, Linux/FreeBSD/Other answer `CatchUp::Relisted`; the capability is stated once so it can be tested on any host.
- `[lib.rs:backend_name]` — the differential test asserts the selected backend before comparing, so a silent fallback cannot make a broken backend look right.
- `[lib.rs:enumerate_cancellable]` — a native backend that refuses falls back to `[lib.rs:fallback]` (the portable walk) rather than taking the daemon down.
- `[lib.rs:is_volume_root]` — the MFT walk is used only when `root` names a volume root, because it enumerates the whole volume.
- `[lib.rs:mft_indexer]` — the `ERS_USE_RECORDS` escape hatch is read here under `#[cfg(feature = "std")]`, not inside the backend.

## Side Effects
- `[lib.rs:mft_indexer]` — reads the `ERS_USE_RECORDS` environment variable with `std::env::var_os` under `#[cfg(feature = "std")]`.
- `[lib.rs:try_mft]` — opens a volume through `rgy_discovery_windows::volume::Volume::open` and leaks the handle with `Box::leak` so `MftSource` owns a `'static` borrow.
- `[lib.rs:source_for]` / `[lib.rs:enumerate]` / `[lib.rs:fallback]` — enumerate the filesystem through the selected native backend or the portable walk.
- `[lib.rs:enumerate_cancellable]` — honours the `[lib.rs:Cancel]` token at the boundaries of a one-shot native walk.

## Verification
- Run: `cargo test --workspace --no-fail-fast --locked`
