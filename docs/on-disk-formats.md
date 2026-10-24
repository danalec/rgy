# On-disk index formats

Consumers: this workspace, `zen-c` (C port), and anything that reads
a saved index without the daemon. Source of truth is the code named below —
this file is the cross-language contract, not a second implementation.

## FSIDX014 / "ERI v3" target (the name index v2 blob)

fsearch's mmap index, the v2 name index in `crates/name-core/src/name_index.rs`.
One file, mmap-loadable, atomic replace (`*.tmp` + rename):

```
offset 0       4096-byte header
               magic "FSIDX014" (8 bytes)
               6 x u64 LE fields:
                 n          entry count
                 d          directory count
                 u          distinct-name count
                 names_len  names blob byte length
                 event_id   live-update watermark (FSEvents id / USN)
                 synced_at  unix seconds all changes through which are applied
offset 4096    the 22 sections, in the order below, each 64-byte aligned
```

`FSIDX014` is `FSIDX013` with one change: the priority tier each entry
inherits from its directory rides in the bits of the entry's kind byte that
were free (bits 5 to 7), so the search reads the tier with the kind byte it
reads anyway instead of computing it from the prior. An index written before
that says something different about the same file, and is not loaded: it is
rebuilt.

`FSIDX013` is `FSIDX012` with one change: each entry carries the location
prior it inherits from its directory, so the search reads a ranking
property that it reads per candidate from the entry itself instead of
walking to the entry's parent to find it. An index written before that
says something different about the same file, and is not loaded: it is
rebuilt.

`FSIDX012` is `FSIDX011` with one change: the name masks describe the
**folded** name (ASCII case fold plus Unicode NFC), because a mask says
which characters a name has, and a name's characters are not the same once
it has been folded. An index written before the fold therefore says
something different about the same file, and is not loaded: it is rebuilt.

The section directory is derived, not stored: the sections follow the
header one after another, each starting on the next 64-byte boundary, in
the order of the `Sec` enum. A reader needs this table and nothing else:

| # | Section | Element | Count | What it holds |
|---|---|---|---|---|
| 0 | `NameBits` | `u64` | `bits_len` | the name bitmaps (see `Index::bitmap`) |
| 1 | `NameOff` | `u32` | `u1` | per distinct name: its offset in the names blob |
| 2 | `Names` | `u8` | `names_len` | the names blob |
| 3 | `EntName` | `u32` | `n` | per entry: its name id |
| 4 | `Kind` | `u8` | `n` | per entry: `walk::KIND_*` with the `walk::FLAG_*` bits |
| 5 | `RankAdj` | `i8` | `n` | per entry: the location prior it inherits from its directory |
| 6 | `Parent` | `u32` | `n` | per entry: the dir id of its parent |
| 7 | `Size` | `u32` | `n` | per entry: the size, encoded (`enc_size`) |
| 8 | `Mtime` | `u32` | `n` | per entry: its mtime, unix seconds |
| 9 | `DirEntry` | `u32` | `d` | per dir: the entry id of the folder |
| 10 | `DirStart` | `u32` | `d` | per dir: its first child entry |
| 11 | `DirLen` | `u32` | `d` | per dir: how many children it has |
| 12 | `DirEnd` | `u32` | `d` | per dir: the end of its subtree |
| 13 | `DirPrior` | `i8` | `d` | per dir: its location prior |
| 14 | `DirParent` | `u32` | `d` | per dir: its parent dir id |
| 15 | `NameEntsOff` | `u32` | `u1` | per distinct name: its offset in `NameEnts` |
| 16 | `NameEnts` | `u32` | `n` | the entries of each name, in order |
| 17 | `NamePrior` | `i8` | `u` | per distinct name: its prior |
| 18 | `NameInfo` | `NameInfo` | `u` | per distinct name: key, length, stem, head |
| 19 | `NameExt` | `u8` | `u` | per distinct name: its extension slot |
| 20 | `Exts` | `[u8; 16]` | `ext_slots` | the extension table |
| 21 | `Zones` | `Zone` | `zones_len` | what a filter on size, mtime or kind can skip |

Two sizes are not in the header and follow from the others: `bits_len`
(the name bitmaps) and `zones_len`. `u1` is the size of the distinct-name
tables, which is `u` plus the names that are not bytes on their own.

The plan's ERI v3 names this blob plus the event_id/synced_at fields —
they are already present. What v3 still owes: the C port's reader.

## TV pack store

The Tesla Valve corpus store (`rgx-core` `tv/` + `tv_runtime/`): packs of
corpus bytes with an incremental manifest, searched via
`tv_runtime::searcher::search_store` (the `rgy_engine::tv` glue). Pack header
(`tv/format.rs`, `PackHeader::SIZE`) precedes each pack's content; the
manifest lists packs with ids, filenames, content lengths. The daemon's
`--tv-dir` points at a store directory.

## Content segments

The trigram/bloom side (`rgy_name_core::content`): the same 4 KiB header
convention (`HDR`, magic + u64 fields) for its segment files, built by
`build_segment`/`merge` under a store directory.
