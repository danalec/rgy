# crates/par/src

## Overview
The library sources of `rgy-par`: the workspace's parallelism floor. It has
no dependencies at all. The crate root declares `#![no_std]` with
`extern crate alloc`; with the `std` feature off the pieces run in order on
the calling thread, and under `std` they run on a persistent pool. `sync`
(lock/once-cell) and `mpsc` (channel) have `no_std` and `std`
implementations behind the feature; `cores` (SMT width) and `pool` exist only
under `std`.

## Interface
- `[lib.rs:no_std]` — crate root `#![no_std]` and `extern crate alloc`.
- `[lib.rs:pub mod sync]`, `[lib.rs:pub mod mpsc]` — the `no_std` primitives.
- `[lib.rs:workers]`, `[lib.rs:set_workers]`, `[lib.rs:configured]`,
  `[lib.rs:default_workers]` — the width, from `set_workers`, then
  `RGY_NUM_THREADS`, then the machine's cores.
- `[lib.rs:map_collect]`, `[lib.rs:map_collect_with]`,
  `[lib.rs:map_collect_into]`, `[lib.rs:for_each]`, `[lib.rs:for_each_mut]`,
  `[lib.rs:map_chunks]`, `[lib.rs:map_owned]`, `[lib.rs:map_owned_at]`,
  `[lib.rs:for_each_owned]` — map/for_each shapes.
- `[lib.rs:sort_unstable_by]`, `[lib.rs:sort_unstable_by_key]`,
  `[lib.rs:sort_unstable]`, `[lib.rs:split_mut]`, `[lib.rs:join]` — sort and
  join.
- `[lib.rs:each_index]`, `[lib.rs:each_index_at]`,
  `[lib.rs:each_index_prepared]`, `[lib.rs:each_index_prepared_at]`,
  `[lib.rs:each_owned]`, `[lib.rs:each_owned_at]` — shared-counter index and
  owned-item scans.
- `[sync.rs:Mutex]`, `[sync.rs:MutexGuard]`, `[sync.rs:Poisoned]`,
  `[sync.rs:OnceCell]` — the lock/cell types.
- `[mpsc.rs:Sender]`, `[mpsc.rs:Receiver]`, `[mpsc.rs:channel]`,
  `[mpsc.rs:SendError]`, `[mpsc.rs:RecvError]`, `[mpsc.rs:TryRecvError]` —
  the channel.
- `[cores.rs:smt_width]` — `pub(crate) fn smt_width`, the machine's
  threads-per-core (std only).
- `[pool.rs:run]`, `[pool.rs:threads]` — `pub(crate)` pool entry points (std
  only).

## Invariants
- `[lib.rs:no_std]` — no `std` without the feature; `extern crate std` is
  declared only behind `#[cfg(feature = "std")]`.
- `[lib.rs:workers]` — the environment is read once and the width cached in a
  `static WIDTH: AtomicUsize`; zero means "not chosen yet".
- `[lib.rs:configured]` — `RGY_NUM_THREADS` is the product's own name; a value
  that is not a positive number is ignored.
- `[lib.rs:default_workers]` — the width is the machine's cores, not logical
  processors (a core with two threads is one core); without `std` it is one.
- `[lib.rs:join]` — under `std` it uses `std::thread::scope`; without it the
  two closures run in order.
- `[lib.rs:Cells]` — `unsafe impl<T: Send> Sync for Cells<T>`; the shared
  counter hands each index to exactly one worker.
- `[sync.rs:pub use std::sync::Mutex]` — the `std` arm re-exports
  `std::sync::{Mutex, OnceLock as OnceCell}`; the `no_std` arm defines its own
  spinning lock.

## Side Effects
- `[lib.rs:default_workers]` — under `std` calls
  `std::thread::available_parallelism()`.
- `[lib.rs:configured]` — under `std` reads `RGY_NUM_THREADS` from the
  environment.
- `[pool.rs:spawn]` — under `std` spawns and keeps worker threads;
  `[lib.rs:join]` uses `std::thread::scope`.
- `[cores.rs:smt_width]` — under `std` reads the platform's processor
  topology (windows/linux/macOS arms).

## Verification
- Run: `cargo test --workspace --no-fail-fast --locked`
