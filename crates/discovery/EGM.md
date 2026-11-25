# rgy-discovery

## Overview
The portable discovery seam crate (`[Cargo.toml:package.name]` is
`rgy-discovery`, edition 2024, version 0.1.10). Its root holds only the
manifest; the library lives in [src](./src/EGM.md) and a throughput benchmark
in [examples](./examples/EGM.md). `[Cargo.toml:feature.std]` is
`std = ["rgy-par/std"]`: the walk asks the operating system directly through
`[src/lib.rs:pub mod dir]`, so the portable walk needs no `std` and the
feature only adds the thread pool the directory reads are fanned out over.

## Subdirectories & Boundaries
- [src](./src/EGM.md) — the `no_std` seam: `[src/lib.rs:Source]`, `[src/lib.rs:Watch]`, the `[src/walk.rs:Listing]` block type, the portable walk, and the hand-declared platform calls.
- [examples](./examples/EGM.md) — `[examples/walk_bench.rs:main]`, the `std` benchmark that times `[src/walk.rs:scan_sequential]` against `[src/walk.rs:scan]`.

## Interface
- `[src/lib.rs:Source]` / `[src/lib.rs:Watch]` — the traits every platform backend implements (`enumerate`, `next_batch`, `position`).
- `[src/lib.rs:ChangeBatch]` / `[src/lib.rs:UsnRecord]` — the shared change vocabulary (`Events` / `Rescan`).
- `[src/lib.rs:PortableSource]` — the portable `Source` (`root: Vec<u8>`, `threads`, `DEFAULT_THREADS`, `new`, `enumerate_cancellable`).
- `[src/walk.rs:Listing]`, `[src/walk.rs:RawEnt]`, `[src/walk.rs:NONE]`, `[src/walk.rs:KIND_FILE]` — the DFS block vocabulary.
- `[src/walk.rs:scan]`, `[src/walk.rs:scan_sequential]`, `[src/walk.rs:scan_with]`, `[src/walk.rs:list_one]` — the walk entry points.
- `[src/dir.rs:list_dir]`, `[src/dir.rs:stat]`, `[src/file.rs:read_file]`, `[src/file.rs:write_file]`, `[src/file.rs:canonicalize]`, `[src/path.rs:join]`, `[src/interrupt.rs:cancel_on_interrupt]` — the seam modules.
- `[examples/walk_bench.rs:main]` — the `std` benchmark entry point.

## Invariants
- `[src/lib.rs:#![no_std]]` / `[src/lib.rs:extern crate std]` — the library is `no_std` + `alloc`; `std` is present only under `#[cfg(feature = "std")]`.
- `[Cargo.toml:feature.std]` — `std = ["rgy-par/std"]`; the portable walk asks the OS through `[src/dir.rs:list_dir]` and so exists in every build, on every target.
- `[src/lib.rs:UsnRecord]` — the USN change vocabulary lives in the seam, not in a platform backend, to avoid a dependency cycle with `name-core`.
- `[src/dir.rs:Win32FindDataW]` / `[src/dir.rs:const _]` — `assert!(size_of::<Win32FindDataW>() == 592)` including `[src/dir.rs:c_alternate_file_name]`; a struct the platform writes is declared whole.
- `[src/dir.rs:list_dir]` — `.`/`..` are excluded, links are not followed, a refused read is `None`, and the order is the platform's.
- `[src/walk.rs:NONE]` — `u32::MAX` is the "no child" sentinel; `[src/walk.rs:scan_sequential]` is the oracle every backend is compared with.
- `[src/path.rs:join]` — paths are bytes with `/` as the only separator.
- `[examples/walk_bench.rs:rounds]` — clamped with `.max(1)` so the timed sample set is never empty.

## Side Effects
- `[src/dir.rs:openat]`, `[src/dir.rs:readdir]`, `[src/dir.rs:fstatat]`, `[src/dir.rs:FindFirstFileW]`, `[src/dir.rs:CreateFileW]` — directory/stat platform calls declared by hand.
- `[src/file.rs:open]`, `[src/file.rs:realpath]`, `[src/file.rs:CreateFileW]`, `[src/file.rs:write_file]` — file read/write/path-resolution calls, including a disk write.
- `[src/interrupt.rs:signal]` / `[src/interrupt.rs:SetConsoleCtrlHandler]` / `[src/interrupt.rs:install]` — process-global Ctrl-C handler installation.
- `[src/walk.rs:scan]` / `[src/walk.rs:walk_parallel]` — filesystem enumeration and, with `std`, thread fan-out.
- `[examples/walk_bench.rs:main]` — stdout CSV and stderr output; filesystem enumeration via the walk.

## Verification
- Run: `cargo test --workspace --no-fail-fast --locked`
