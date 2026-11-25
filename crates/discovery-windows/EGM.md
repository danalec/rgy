# rgy-discovery-windows

## Overview
The Windows discovery backend crate (`[Cargo.toml:package.name]` is
`rgy-discovery-windows`, edition 2024). Its root holds only the manifest; the
library is in [src](./src/EGM.md). `[Cargo.toml:dependencies]` pulls in
`rgy-discovery` with `default-features = false` — the backend only speaks the
block types and change vocabulary, so the seam must not drag `std` in. The
manifest states the Win32 calls are declared by hand in `src/win.rs`, so
`[Cargo.toml:dependencies]` names no `windows` crate and the crate declares
no features of its own. `[Cargo.toml:dev-dependencies]` keeps `hashbrown` as
the differential oracle the tree's own path cache is held to.

## Subdirectories & Boundaries
- [src](./src/EGM.md) — `[src/seam.rs:MftSource]` / `[src/seam.rs:UsnWatch]` (MFT walk and USN journal), `[src/rdcw.rs:RdcwWatch]` (privilege-free fallback), `[src/win.rs:CreateFileW]` and the hand-declared Win32 seam, `[src/ntfs.rs:parse_file_record]`, `[src/hmap.rs:Cache]`.

## Interface
- `[src/lib.rs:MftSource]`, `[src/lib.rs:UsnWatch]`, `[src/lib.rs:RdcwWatch]` — the public `Source`/`Watch` re-exports.
- `[src/win.rs:create_file_w]`, `[src/win.rs:device_io_control]`, `[src/win.rs:read_directory_changes_w]`, `[src/win.rs:last_error]` — the seam wrappers.
- `[src/volume.rs:Volume]`, `[src/indexer.rs:Indexer]`, `[src/indexer.rs:listings_from_rows]`, `[src/paths.rs:Paths]`, `[src/ntfs.rs:parse_file_record]`, `[src/hmap.rs:Cache]`.

## Invariants
- `[src/lib.rs:#![no_std]]` / `[src/lib.rs:#![cfg(windows)]]` — `no_std` is declared before the crate-level `cfg`; off Windows the crate compiles empty.
- `[src/win.rs:unsafe extern "system"]` — the Win32 calls are declared by hand, once, in the seam; `[Cargo.toml:dependencies]` names no `windows` crate.
- `[src/win.rs:last_error]` — the error code is read where the call is; `[src/win.rs:read_file]` takes a slice, not a bare pointer.
- `[src/win.rs:const _]` / `[src/win.rs:FILE_ID_INFO]` — `size_of::<FILE_ID_INFO>() == 24` and `offset_of!(FILE_NOTIFY_INFORMATION, FileName) == 12` are asserted at compile time.
- `[src/ntfs.rs:ROOT_FRN]` — `5`, the volume root FRN; `[src/ntfs.rs:pub use rgy_discovery]` re-exports the shared change vocabulary.
- `[src/volume.rs:GENERIC_READ_ACCESS]` — a volume handle needs `GENERIC_READ`; `[src/rdcw.rs:RdcwWatch]` is the privilege-free fallback.
- `[src/paths.rs:MAX_DEPTH]` / `[src/indexer.rs:MAX_RECORDS]` — bounds that stop a corrupt or cyclic MFT from spinning.
- `[src/hmap.rs:Cache]` — nothing is removed from the path cache, so the open-addressing table has no tombstones.

## Side Effects
- `[src/win.rs:CreateFileW]`, `[src/win.rs:CloseHandle]`, `[src/win.rs:ReadFile]`, `[src/win.rs:DeviceIoControl]`, `[src/win.rs:ReadDirectoryChangesW]`, `[src/win.rs:GetLastError]` — Win32 calls declared by hand in `[src/win.rs:unsafe extern "system"]`.
- `[src/volume.rs:Volume::open]` / `[src/volume.rs:read_usn]` / `[src/volume.rs:read_file_record]` — opens `\\.\C:` and reads the MFT and USN journal.
- `[src/rdcw.rs:RdcwWatch]` — directory-handle reads of `FILE_NOTIFY_INFORMATION` records.
- `[src/seam.rs:UsnWatch]` / `[src/seam.rs:MftSource]` — journal tailing and whole-volume enumeration.

## Verification
- Run: `cargo test --workspace --no-fail-fast --locked`
