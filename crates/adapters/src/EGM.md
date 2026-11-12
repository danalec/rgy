# crates/adapters/src

## Overview
The two library sources of `rgy-adapters`: the registry that orders the ten
vendored rgx adapter families, and the `Adapters` reader that answers the
content engine's "what does this file render to?" seam. This crate is the
repository's `std` exception: `lib.rs` has no `#![no_std]` and its module docs
say it is `std` because the ten adapter families are built with their formats
turned on, and a decoder that reads a file is a decoder with a filesystem.
There is no `std` feature in the manifest. The decoders run **in this
process** on bytes a stranger may have written; `RGY_ADAPTERS=off` is the
opt-out, and this is the only `rgx_core::process` call site in the tree.

## Interface
- `[lib.rs:registry]` — `pub fn registry() -> Registry`, builds the registry
  with all ten vendored families in upstream's order.
- `[lib.rs:pub use reader::Adapters]` — re-exports the reader.
- `[reader.rs:Adapters]` — `pub struct Adapters`, the `AdapterReader`
  implementation.
- `[reader.rs:rendered]` — `fn rendered(&self, path: &[u8]) -> Vec<Rendered>`;
  an empty vector means no adapter claims the file.
- `[reader.rs:ADAPTERS_ENV]` — `pub const ADAPTERS_ENV: &str = "RGY_ADAPTERS"`.
- `[reader.rs:adapters_disabled_from]` — pure answer for one value of the
  switch, testable without touching the process environment.
- `[reader.rs:adapters_disabled]` — the switch read once into a
  `OnceLock<bool>`.
- `[reader.rs:rendered_when]` — `rendered` with the switch supplied by the
  caller, so a test can hold the disabled answer without mutating the
  environment.
- `[reader.rs:shared_registry]` — the registry held once in a `OnceLock`.
- `[reader.rs:read_for_adapter]` — reads a file for the adapter with the
  content engine's `MAX_FILE` cap.

## Invariants
- `[lib.rs:registry]` — the family order is upstream's own: archives, office,
  3D, llm, media, tabular, pdf, mail, docs, inspect; the first matching
  available adapter wins and a bail falls through to the next.
- `[reader.rs:shared_registry]` — the registry is built once into a
  `static REGISTRY: OnceLock<Registry>` rather than per file.
- `[reader.rs:rendered]` — a plain text file is not claimed; a file no adapter
  claims renders to nothing and the content path reads its own bytes.
- `[reader.rs:adapters_disabled_from]` — only `off`, `0`, `false`, `no`,
  `disabled` and `none` (trimmed, ASCII case-insensitive) turn the parsers
  off; an unset, empty or unrecognised value leaves them ON. A typo must not
  silently lose the formats the user asked for.
- `[reader.rs:rendered_when]` — when disabled it returns `Vec::new()` before
  `shared_registry()` is consulted, before any file is opened and before
  `rgx_core::process` is called: no decoder sees the untrusted bytes. The
  content path's plain branch then reads the file's own bytes, still capped
  at 4 MiB. Name search and plain-text content search are unchanged.
- `[reader.rs:rendered]` — `RGY_ADAPTERS` is read once per process, so a
  daemon started with the variable set covers `rgxy` and `searchctl
  --daemon`, which inherit the daemon's environment rather than their own.
- `[lib.rs:registry]` — building the registry costs a few hundred allocations
  and no I/O; the adapters are cheap to construct.

## Side Effects
- `[reader.rs:read_for_adapter]` — opens and reads the file through
  `content::open_regular` and `std::io::Read` (the adapters read files).
- `[reader.rs:rendered]` — calls `rgx_core::process` to render adapter-claimed
  bytes into documents. This is in-process parsing of untrusted bytes: no
  subprocess, no sandbox, no privilege drop and no per-parse timeout. The
  bounds are a 4 MiB input, `Ctx`'s depth 4 / 4096 members, and the
  decoders' `OutputCap`. README.md and MANUAL.md state the threat model.
- `[lib.rs:registry]` — constructs the ten adapter families, each of which
  may ask `available()` about an external program.

## Verification
- Run: `cargo test --workspace --no-fail-fast --locked`
