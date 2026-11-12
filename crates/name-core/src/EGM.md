# crates/name-core/src

## Overview
The library sources of `rgy-name-core`: the name index and query/scoring
engine with the zen-rs filter vocabulary merged, plus the persistence glue,
the comparison fold (ASCII case-fold + NFC), Unicode NFC and Unihan reading
tables, the tree's own hash map, the live overlay, the incremental session and
the lookup surface. The crate root declares `#![no_std]` with
`extern crate alloc`; `content`, `persist` and `placeholder` are gated behind
`#[cfg(feature = "std")]`. Every source here is an independent Apache-2.0
implementation: the three modules once transported from fsearch
(`name_index`, `name_query`, `content`) were rewritten and the crate's
doc comment says so.

## Interface
- `[lib.rs:no_std]` — crate root `#![no_std]` and `extern crate alloc`.
- `[lib.rs:no_materialize]` — `pub fn no_materialize()`, the portable
  no-op hook.
- `[lib.rs:pub mod name_index]`, `[lib.rs:pub mod name_query]`,
  `[lib.rs:pub mod rank]`, `[lib.rs:pub mod fold]`, `[lib.rs:pub mod nfc]`,
  `[lib.rs:pub mod readings]`, `[lib.rs:pub mod session]`,
  `[lib.rs:pub mod lookup]`, `[lib.rs:pub mod live]`, `[lib.rs:pub mod regex]`,
  `[lib.rs:pub mod rules]`, `[lib.rs:pub mod clock]`, `[lib.rs:pub mod fuzzy]`
  — the public module surface.
- `[name_index.rs:Index]` — `pub struct Index`, the name index v2.
- `[name_index.rs:Blob]`, `[name_index.rs:BlobMut]` — the index blob wrappers.
- `[name_index.rs:ForestFault]` — `pub enum ForestFault` for an invalid forest
  layout.
- `[name_query.rs:Query]` — `pub struct Query`, the parsed query.
- `[name_query.rs:Searcher]` — `pub struct Searcher<'a>`.
- `[name_query.rs:fuzzy_score]` — `pub fn fuzzy_score`.
- `[name_query.rs:merge_hits]` — `pub fn merge_hits(all, limit)`.
- `[name_query.rs:matched_chars]` — `pub fn matched_chars`.
- `[name_query.rs:Mode]`, `[name_query.rs:GrepMode]`,
  `[name_query.rs:SortBy]`, `[name_query.rs:Alternatives]`.
- `[fold.rs:fold]` — `pub fn fold(bytes: &[u8]) -> Cow<'_, [u8]>`, the
  comparison fold.
- `[hmap.rs:Map]` — `pub struct Map<K, V>`, the tree's own hash map.
- `[live.rs:Live]`, `[live.rs:OEnt]`, `[live.rs:Flat]`, `[live.rs:Fetched]`,
  `[live.rs:Applied]` — the live overlay types.
- `[live.rs:for_each_path]`, `[live.rs:join]`, `[live.rs:lstat]` — overlay path
  helpers.
- `[lookup.rs:Lookup]`, `[lookup.rs:Backend]` — the five-index lookup surface.
- `[readings.rs:Script]`, `[readings.rs:Shadow]` — transliteration vocabulary.
- `[clock.rs:unix_millis]`, `[clock.rs:unix_secs]`, `[clock.rs:Timespec]` — the
  wall clock.
- `[content.rs:Segment]`, `[content.rs:MAX_FILE]` — the inert shell left by
  the removed persisted trigram index; it keeps no on-disk index of its own
  (std).
- `[placeholder.rs:PLACEHOLDER_BITS]`, `[placeholder.rs:is_placeholder]`,
  `[placeholder.rs:skip_reason]`, `[placeholder.rs:is_placeholder_path]` —
  cloud-placeholder detection (std).
- `[nfc_table.rs:DECOMP]`, `[nfc_table.rs:CCC]`, `[nfc_table.rs:COMPOSE]`,
  `[nfc_table.rs:QC_NO]`, `[nfc_table.rs:QC_MAYBE]` — generated UCD tables,
  `pub(crate)`.
- `[nfc_vectors.rs:CONFORMANCE]` — generated NFC conformance vectors
  (`#[cfg(test)] mod nfc_vectors`).
- `[sync.rs:pub(crate) use rgy_par::sync::{Mutex, OnceCell}]` — the crate's
  lock/cell are re-exported from `rgy-par`, not defined here.

## Invariants
- `[lib.rs:no_std]` — the crate is `no_std` with `alloc`; `extern crate std`
  is declared only behind `#[cfg(all(feature = "std", not(test)))]` and the
  test-only `#[cfg(test)] #[macro_use] extern crate std`.
- `[lib.rs:pub mod content]` — `content`, `persist` and `placeholder` are
  `#[cfg(feature = "std")]`; the index, query, ranking, fold, NFC tables and
  transliterations compile without it.
- `[lib.rs:mod hmap]` — `hmap` is a private `mod`, and `sync` is a private
  `mod`; the public surface is the `pub mod` list above.
- `[name_index.rs:ForestFault]` — the index validates its DFS forest and
  reports a `ForestFault` rather than trusting a corrupt layout.
- `[nfc_table.rs:DECOMP]` — the normalization tables are generated (their
  headers say so) and are `pub(crate)`, not part of the public API.

## Side Effects
- `[clock.rs:clock_gettime]` — a hand-declared `unsafe extern "C"`
  `clock_gettime` on unix; `[clock.rs:GetSystemTimeAsFileTime]` — a
  hand-declared `unsafe extern "system"` clock on Windows.
- `[content.rs:verify_paths_with]` — the content grep opens candidate files
  on disk and reads their bytes through `std::fs` (std); it declares no
  platform ABI.
- `[scan_priority.rs:unsafe extern "C"]` — the platform's interactive-priority
  calls are declared by hand.
- `[placeholder.rs:attributes]` — reads the OS file attributes for a path
  (platform-specific `pub fn attributes` arms).
- `[live.rs:lstat]` — `pub fn lstat(path)` reaches the platform's stat.
- `[sync.rs:pub(crate) use rgy_par::sync::{Mutex, OnceCell}]` — the lock and
  once-cell primitives come from `rgy-par`.

## Verification
- Run: `cargo test --workspace --no-fail-fast --locked`
