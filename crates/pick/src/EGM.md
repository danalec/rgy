# crates/pick/src

## Overview
The single library source of `rgy-pick`, `lib.rs`: handing rgy's results to
an external picker (`fzf`, `sk`, `peco`, `fz`). The crate root declares
`#![no_std]` with `extern crate alloc`; everything that touches the machine —
the process, the pipes, `PATH` — is behind the `std` feature. The default
record separator is NUL because a NUL byte cannot appear in a path on any
platform, and only `fzf`/`sk` (which know `--read0`/`--print0`) are looked for
when the caller names none.

## Interface
- `[lib.rs:no_std]` — crate root `#![no_std]` and `extern crate alloc`.
- `[lib.rs:Error]`, `[lib.rs:Result]` — the sentence-shaped error type.
- `[lib.rs:NUL_CAPABLE]`, `[lib.rs:CANDIDATES]`, `[lib.rs:ORDER_AWARE]` — the
  picker name tables.
- `[lib.rs:Separator]`, `[lib.rs:separator_for]`, `[lib.rs:Order]`,
  `[lib.rs:Request]`, `[lib.rs:Search]` — the picker contract.
- `[lib.rs:status]`, `[lib.rs:pick_records]`, `[lib.rs:pick]`,
  `[lib.rs:takes_colour_flags]` — entry points (the run-time ones are
  `#[cfg(feature = "std")]`).
- `[lib.rs:carries_line]`, `[lib.rs:carryable_in]`,
  `[lib.rs:uncarryable_paths]` — the public byte rules (not behind `std`).

## Invariants
- `[lib.rs:no_std]` — `no_std` + `alloc`; `extern crate std` is declared only
  behind `#[cfg(any(test, feature = "std"))]`.
- `[lib.rs:NUL_CAPABLE]` — `&["fzf", "sk"]`; `[lib.rs:CANDIDATES]` is the same
  list, since the default separator is NUL.
- `[lib.rs:status]` — the exit-status table: 0 means a selection, 1 means
  nothing matched or the user dismissed (0/1/130), anything else is a broken
  picker error.
- `[lib.rs:separator_for]` — the picker has the last word on the list: a
  picker outside `NUL_CAPABLE` gets `Separator::Line`.
- `[lib.rs:carries_line]` — a record carrying `\n` or `\r` is not offered at
  all (half of a path does not exist); `[lib.rs:carryable_in]` is the same rule
  for NUL.
- `[lib.rs:ETXTBSY]` — on unix, `ETXTBSY = 26`; the constant is `#[cfg(unix)]`
  and `#[cfg(feature = "std")]`.

## Side Effects
- `[lib.rs:pick_records]`, `[lib.rs:run]` — spawn the picker through
  `std::process::Command` with piped stdin/stdout and inherited stderr.
- `[lib.rs:run]` — spawns a `std::thread` for the search writer and reads the
  picker's stdout; prints warnings to stderr.
- `[lib.rs:scan]` — scans `PATH` via `std::env::var_os("PATH")` and
  `std::env::split_paths`, memoised in a `OnceLock<Mutex<HashMap<...>>>`.
- `[lib.rs:spawn]` — unix-only retry on `ETXTBSY` with
  `std::thread::sleep` for up to 2 seconds.
- `[lib.rs:pick]` — prints the selection to stdout.

## Verification
- Run: `cargo test --workspace --no-fail-fast --locked`
