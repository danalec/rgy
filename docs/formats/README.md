# On-disk index formats

Consumers: this workspace, `everything-c` (C port), and anything that reads
a saved index without the daemon. Source of truth is the code named below —
this file is the cross-language contract, not a second implementation.

## ERI2 (legacy, still read)

The everything-rs binary index (`name_core::index`, `ERI_MAGIC = "ERI2"`).
Everything-c reads and writes the same format — keep it readable. Layout
lives in `crates/name-core/src/index.rs` (`Index::read_from`/`write_to`)
and `everything-c`'s port of it. Multi-volume: a root table of
`(volume ordinal, root string)` pairs precedes the record stream; FRN keys
carry the volume ordinal in their high 16 bits.

## FSIDX011 / "ERI v3" target (the name index v2 blob)

fsearch's mmap index, the v2 name index in `crates/name-core/src/name_index.rs`.
One file, mmap-loadable, atomic replace (`*.tmp` + rename):

```
offset 0       4096-byte header
               magic "FSIDX011" (8 bytes)
               6 x u64 LE fields:
                 n          entry count
                 d          directory count
                 u          distinct-name count
                 names_len  names blob byte length
                 event_id   live-update watermark (FSEvents id / USN)
                 synced_at  unix seconds all changes through which are applied
offset 4096    packed sections (see `Index::build`):
               names blob, name bitmaps (BM_*), class/double counts,
               entry tables (ent_name, kind, parent, size, mtime),
               dir DFS ranges (dir_start/dir_end), zones, size encoding
               (enc_size/dec_size)
```

The plan's ERI v3 names this blob plus the event_id/synced_at fields —
they are already present. What v3 still owes: a stable section-directory
(the current layout is internal to `build`), and the C port's reader.

## TV pack store

The Tesla Valve corpus store (`rgx-core` `tv/` + `tv_runtime/`): packs of
corpus bytes with an incremental manifest, searched via
`tv_runtime::searcher::search_store` (the `engine::tv` glue). Pack header
(`tv/format.rs`, `PackHeader::SIZE`) precedes each pack's content; the
manifest lists packs with ids, filenames, content lengths. The daemon's
`--tv-dir` points at a store directory.

## Content segments

The trigram/bloom side (`name-core::content`): the same 4 KiB header
convention (`HDR`, magic + u64 fields) for its segment files, built by
`build_segment`/`merge` under a store directory.
