# rgy-discovery src — the portable discovery seam

## Overview
The library source of the portable discovery seam: the `[lib.rs:Source]` and
`[lib.rs:Watch]` traits, the shared USN change vocabulary, the DFS
`[walk.rs:Listing]` block type, the portable `[walk.rs:scan]` walk, and the
platform calls for directories, files, paths and Ctrl-C declared by hand.
`[lib.rs:#![no_std]]` makes the crate `no_std` + `alloc`;
`[lib.rs:extern crate std]` is gated behind `#[cfg(feature = "std")]`, and the
walk asks the operating system directly through `[lib.rs:pub mod dir]`, so the
block stream is identical with the feature on or off — the feature only buys
the thread fan-out.

## Interface
- `[lib.rs:Source]` — `fn enumerate(&self) -> Result<Vec<walk::Listing>, String>`; `[lib.rs:Watch]` — `next_batch` and `position`.
- `[lib.rs:ChangeBatch]` — `Events(Vec<UsnRecord>)` / `Rescan`; `[lib.rs:UsnRecord]` — the USN journal tail fields.
- `[lib.rs:USN_REASON_FILE_CREATE]`, `[lib.rs:USN_REASON_PATH_RELEVANT]`, `[lib.rs:FILE_ATTRIBUTE_REPARSE_POINT]`, `[lib.rs:FILE_ATTRIBUTE_DIRECTORY]` — shared constants.
- `[lib.rs:PortableSource]` — `root`, `threads`, `DEFAULT_THREADS`, `new`, `enumerate_cancellable`; `[lib.rs:impl Source for PortableSource]`.
- `[lib.rs:pub mod dir]`, `[lib.rs:pub mod file]`, `[lib.rs:pub mod interrupt]`, `[lib.rs:pub mod path]`, `[lib.rs:pub mod walk]`.
- `[walk.rs:Listing]`, `[walk.rs:RawEnt]`, `[walk.rs:NONE]`, `[walk.rs:KIND_FILE]`, `[walk.rs:KIND_DIR]`, `[walk.rs:KIND_LINK]`, `[walk.rs:FLAG_HIDDEN]`, `[walk.rs:FLAG_MOUNT]` — the DFS block vocabulary.
- `[walk.rs:scan]`, `[walk.rs:scan_with]`, `[walk.rs:scan_sequential]`, `[walk.rs:scan_sequential_with]`, `[walk.rs:list_one]` — walk entry points.
- `[walk.rs:Cancel]`, `[walk.rs:Cancelled]`, `[walk.rs:Progress]`, `[walk.rs:SKIP]`, `[walk.rs:blocked]` — cancellation, progress and skip filter.
- `[dir.rs:DirList]`, `[dir.rs:Stat]`, `[dir.rs:list_dir]`, `[dir.rs:stat]` — the directory reader.
- `[file.rs:read_file]`, `[file.rs:write_file]`, `[file.rs:canonicalize]` — the file layer.
- `[path.rs:join]`, `[path.rs:cstring]`, `[path.rs:as_c_ptr]` — byte-path helpers; `[path.rs:win_text]`, `[path.rs:win_wide]`, `[path.rs:win_verbatim]` — Windows spelling helpers behind `#[cfg(windows)]` / `#[cfg(any(windows, test))]`.
- `[interrupt.rs:cancel_on_interrupt]`, `[interrupt.rs:line]`, `[interrupt.rs:TRIPPED]` — cooperative Ctrl-C stop.

## Invariants
- `[lib.rs:#![no_std]]` — `no_std` + `alloc`; `[lib.rs:extern crate std]` appears only under `#[cfg(feature = "std")]`.
- `[lib.rs:UsnRecord]` — the USN change vocabulary lives in the seam, not in a platform backend, so `name-core` can apply live updates without depending on a backend.
- `[dir.rs:Win32FindDataW]` — `[dir.rs:const _]` asserts `core::mem::size_of::<Win32FindDataW>() == 592`, including `[dir.rs:c_alternate_file_name]` (`[u16; 14]`); a struct the platform writes must be declared whole, and `[dir.rs:const _]` also asserts the unix `PlatformStat`/`PlatformDirent` sizes and offsets.
- `[dir.rs:list_dir]` — reader promises: `.` and `..` never listed, a link's metadata is read without following it, a refused read is `None` (not an empty listing), and the order is the platform's.
- `[walk.rs:NONE]` — `u32::MAX` is the "no child listing" sentinel; `[walk.rs:scan_sequential]` is the correctness oracle every backend is compared against.
- `[path.rs:join]` — `/` is the only separator and a path is bytes; `[interrupt.rs:cancel_on_interrupt]` — a cut-short scan returns `[walk.rs:Cancelled]` and the caller writes no index.
- `[walk.rs:SKIP]` — a global `OnceCell` skip set consulted by `[walk.rs:blocked]`.

## Side Effects
- `[dir.rs:openat]`, `[dir.rs:fdopendir]`, `[dir.rs:readdir]`, `[dir.rs:closedir]`, `[dir.rs:close]`, `[dir.rs:fstatat]` — unix directory/stat calls; `[dir.rs:FindFirstFileW]`, `[dir.rs:FindNextFileW]`, `[dir.rs:FindClose]`, `[dir.rs:CreateFileW]` — Windows calls; all declared by hand in `[dir.rs:unsafe extern "C"]` / `[dir.rs:unsafe extern "system"]`.
- `[file.rs:open]`, `[file.rs:read]`, `[file.rs:write]`, `[file.rs:close]`, `[file.rs:realpath]`, `[file.rs:CreateFileW]` — file read/write/path-resolution calls declared by hand; `[file.rs:write_file]` performs a disk write.
- `[interrupt.rs:signal]` (unix) / `[interrupt.rs:SetConsoleCtrlHandler]` (windows) — installs a process-global Ctrl-C handler via `[interrupt.rs:install]`.
- `[walk.rs:scan]` — reads the filesystem through `[walk.rs:crate::dir]`; with `std`, `[walk.rs:walk_parallel]` fans directory reads across `rgy-par` threads.

## Verification
- Run: `cargo test --workspace --no-fail-fast --locked`
