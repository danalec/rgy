# rgy-discovery-windows src — the Windows MFT/USN backend

## Overview
The Windows discovery backend: an NTFS MFT scan (`[seam.rs:MftSource]`) and a
USN journal tail (`[seam.rs:UsnWatch]`), plus a privilege-free
`ReadDirectoryChangesW` fallback (`[rdcw.rs:RdcwWatch]`).
`[lib.rs:#![no_std]]` is declared before `[lib.rs:#![cfg(windows)]]`, so off
Windows the crate compiles empty. The Win32 calls this backend makes are
declared by hand in `[win.rs:unsafe extern "system"]` — the module doc states
"The tree does not link the `windows` crate", and the crate root's manifest
names no such dependency. `[ntfs.rs:parse_file_record]` is pure byte parsing
of the documented on-disk formats; the tree's own `[hmap.rs:Cache]` replaced
`hashbrown` in the library tree.

## Interface
- `[lib.rs:MftSource]`, `[lib.rs:UsnWatch]`, `[lib.rs:RdcwWatch]` — the public re-exports.
- `[lib.rs:pub mod indexer]`, `ntfs`, `paths`, `rdcw`, `seam`, `volume`, `win`; `[lib.rs:mod hmap]`.
- `[win.rs:BOOL]`, `[win.rs:DWORD]`, `[win.rs:HANDLE]`, `[win.rs:PCWSTR]`; `[win.rs:GENERIC_READ]`, `[win.rs:OPEN_EXISTING]`, `[win.rs:FILE_FLAG_BACKUP_SEMANTICS]`, `[win.rs:FILE_LIST_DIRECTORY]`.
- `[win.rs:OVERLAPPED]`, `[win.rs:FILE_NOTIFY_INFORMATION]`, `[win.rs:FILE_ID_INFO]`, `[win.rs:FILE_ID_128]`.
- `[win.rs:FSCTL_GET_NTFS_FILE_RECORD]`, `[win.rs:FSCTL_GET_NTFS_VOLUME_DATA]`, `[win.rs:FSCTL_QUERY_USN_JOURNAL]`, `[win.rs:FSCTL_READ_USN_JOURNAL]`.
- `[win.rs:last_error]`, `[win.rs:is_invalid]`, `[win.rs:create_file_w]`, `[win.rs:close_handle]`, `[win.rs:read_file]`, `[win.rs:device_io_control]`, `[win.rs:get_drive_type_w]`, `[win.rs:get_volume_information_w]`, `[win.rs:read_directory_changes_w]`, `[win.rs:get_file_information_by_handle_ex]` — wrappers over the extern block.
- `[volume.rs:Volume]`, `[volume.rs:VolumeError]`, `[volume.rs:VolumeResult]`, `[volume.rs:USN_BUFFER_SIZE]`, `[volume.rs:MFT_RECORD_SIZE]`, `[volume.rs:list_fixed_ntfs_roots]`, `[volume.rs:impl Drop for Volume]`.
- `[indexer.rs:Indexer]`, `[indexer.rs:NameRow]`, `[indexer.rs:IndexStats]`, `[indexer.rs:listings_from_rows]`, `[indexer.rs:MAX_RECORDS]`.
- `[ntfs.rs:parse_file_record]`, `[ntfs.rs:parse_file_names]`, `[ntfs.rs:apply_fixups]`, `[ntfs.rs:parse_usn_record]`, `[ntfs.rs:walk_usn_buffer]`, `[ntfs.rs:decode_data_runs]`, `[ntfs.rs:ROOT_FRN]`, `[ntfs.rs:pub use rgy_discovery]`.
- `[paths.rs:Paths]`, `[rdcw.rs:RdcwWatch]`, `[seam.rs:MftSource]`, `[seam.rs:UsnWatch]`, `[hmap.rs:Cache]`.

## Invariants
- `[lib.rs:#![no_std]]` / `[lib.rs:#![cfg(windows)]]` — `no_std` is declared before the crate-level `cfg`; off Windows the crate is empty, which keeps `cargo check --workspace` honest on the other platforms.
- `[win.rs:CreateFileW]` / `[win.rs:DeviceIoControl]` / `[win.rs:ReadDirectoryChangesW]` — the Win32 calls are declared by hand, once, in the seam; `[win.rs:#![allow(non_snake_case, non_camel_case_types)]]` keeps the platform's own names.
- `[win.rs:last_error]` — "the error code is read where the call is": every wrapper returns the `GetLastError` code itself, so a caller cannot read it too late; `[win.rs:read_file]` — "a buffer is a slice, not a pointer".
- `[win.rs:const _]` / `[win.rs:FILE_ID_INFO]` — `assert!(size_of::<FILE_ID_INFO>() == 24)` and `[win.rs:FILE_NOTIFY_INFORMATION]` `assert!(offset_of!(FILE_NOTIFY_INFORMATION, FileName) == 12)` fix the platform struct layouts at compile time.
- `[ntfs.rs:ROOT_FRN]` — `5`, the volume root file reference; `[ntfs.rs:pub use rgy_discovery]` re-exports the shared change vocabulary so the old `ntfs::UsnRecord` paths keep working.
- `[volume.rs:GENERIC_READ_ACCESS]` — a volume handle must be opened with `GENERIC_READ`; an ordinary process is refused with `ERROR_ACCESS_DENIED`, so `[rdcw.rs:RdcwWatch]` is the privilege-free fallback.
- `[paths.rs:MAX_DEPTH]` — the parent-chain walk gives up after 64 levels, so a cyclic (corrupt) MFT cannot spin; `[indexer.rs:MAX_RECORDS]` — a 16,000,000-record bound so a corrupt volume cannot spin forever.
- `[hmap.rs:Cache]` / `[hmap.rs:EMPTY]` — nothing is ever taken out of the path cache, so the open-addressing table has no tombstones.
- `[volume.rs:USN_HEADER_PREFIX]` — `FSCTL_READ_USN_JOURNAL` output begins with an 8-byte resume USN before any `USN_RECORD`.

## Side Effects
- `[win.rs:CreateFileW]`, `[win.rs:CloseHandle]`, `[win.rs:GetLastError]`, `[win.rs:ReadFile]`, `[win.rs:DeviceIoControl]`, `[win.rs:GetDriveTypeW]`, `[win.rs:GetVolumeInformationW]`, `[win.rs:ReadDirectoryChangesW]`, `[win.rs:GetFileInformationByHandleEx]` — the Win32 calls declared by hand in `[win.rs:unsafe extern "system"]`; a hand-written declaration still links against `kernel32`.
- `[volume.rs:Volume::open]` — opens `\\.\C:` with `GENERIC_READ`; `[volume.rs:read_usn]` / `[volume.rs:read_file_record]` / `[volume.rs:read_at]` — volume, USN journal and MFT reads.
- `[rdcw.rs:RdcwWatch]` — opens a directory handle with `FILE_LIST_DIRECTORY` and reads `FILE_NOTIFY_INFORMATION` records; `[rdcw.rs:impl Drop for RdcwWatch]` closes it.
- `[seam.rs:UsnWatch]` / `[seam.rs:MftSource]` — tails the USN journal and enumerates the volume.
- `[win.rs:last_error]` — reads the thread's last-error code immediately after a call.

## Verification
- Run: `cargo test --workspace --no-fail-fast --locked`
