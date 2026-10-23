# `name-core` — the specification

The name index: what the disk looked like, and what it looks like now.
This document is the contract the four modules of `crates/name-core` are
written against. The on-disk formats it names are in
[`formats/README.md`](on-disk-formats.md).

The crate has four modules:

| Module | What it owns | Origin |
|---|---|---|
| `name_index` | The base index: a snapshot of a walk, frozen, and the format it is stored in. | carried from another project |
| `live` | The overlay: the difference between that snapshot and the disk right now. | written for this workspace |
| `name_query` | The query layer: parsing, filtering, scoring, ranking. | carried from another project |
| `content` | The content index: a trigram index over the same tree, for searching inside files. | carried from another project |

The modules marked as carried keep the terms of the project they came from,
and `NOTICE` says which. The overlay does not: it was written here, and it
is Apache-2.0 like the rest of the product.

## Two path spaces

The base index holds **root-relative** paths: `/src/main.rs`, whatever the
root was, with `/` as the separator. `Index::path` reconstructs one, and
`Index::lookup` takes one.

The overlay holds **openable** paths: the same file as the filesystem spells
it, `C:\Users\...\src\main.rs` or `/src/main.rs` when the root is `/`.
`openable(root, rel)` is the join between the two.

The two spaces coincide only when the root is `/`. Everywhere else they are
different strings, and the Live crosses between them: it knows the tree the
base was built from (`Live::with_root`), and every path it hands to the base
is translated on the way in and on the way out. The crossing is what makes
the two halves one index:

- a base entry is found by `Index::lookup(root_relative)`, and the Live
  translates the openable path it holds into that space;
- an overlay entry is found by its openable key, and the Live translates it
  back when the tree is written out (`Live::to_listings`).

So a file the base holds and the overlay also holds is one file, not two:
the overlay wins, and the base's entry for it dies with it. The consequence
worth stating plainly: **an overlay update does kill the base entry for the
same file.** Freshness comes from the overlay, and the base stops claiming
what the overlay already holds.

A tree that is not at `/` has to say so: `Live::new` means "the root is
`/`", and a caller that walks another tree builds its Live with
`Live::with_root(index, root)`.

## The base index (`name_index`)

A snapshot of one walk, stored in one file.

- `Index::build(listings, event_id, synced_at, home)` takes the DFS listing
  blocks a `discovery::Source` produces and builds the index.
- Entries are numbered in DFS order; directories own contiguous ranges of
  their children, so a folder's subtree is a range and not a filter.
- Names are interned: many entries, few distinct names. The bitmaps are
  over *names*, not over entries.
- `Index::lookup(path)` finds an entry by its root-relative path.
- `Index::path(i, out)` writes the root-relative path of entry `i`.
- `Index::save` / `Index::load` are the FSIDX012 format
  ([`formats/README.md`](on-disk-formats.md)).

## The overlay (`live`)

What changed since the snapshot. Two things and only two:

- **the dead set** — base entries that are gone, as a bitset over the base
  entry numbers. `Live::is_dead(i)` is the only reader.
- **the overlay map** — `Live::over`, a `BTreeMap` from openable path to
  `OEnt`, holding the entries the base does not have and the entries whose
  metadata moved on.

`Flat` is the overlay laid out for searching: the name masks in one array
to scan, the path and the entry in the same slot. A removed entry's slot is
reused; until it is, its mask is zero, which no name has.

`Live::trees` is the list of directories added or removed since it was
last drained; the content layer re-syncs those whole subtrees.

`Live::priors` maps an overlay folder to the prior of its nearest base
folder, so an overlay entry ranks with the folder it sits in.

### Bringing a directory in line with the disk

`Live::apply_dir(path, recursive)` reconciles one directory (or its whole
subtree) with the disk. It is split in two halves on purpose:

- `Live::fetch(path, recursive)` reads the disk and needs only `&self`, so
  a caller holding a read lock can run it while searches keep answering.
- `Live::apply(fetched)` writes the difference and needs `&mut self`.

The two halves are the same operation seen from either side, and splitting
them is what lets the daemon keep answering queries while it re-lists a
folder.

The reconciliation is a diff, and a diff is idempotent: replaying history,
a duplicate event, and an event that races a compaction all land on the
same state.

### Change events

`Live::apply_events(&[UsnRecord])` is the same reconciliation, driven by a
feed. Each record names the changed path in `name`; the directory that is
re-listed depends on the feed's granularity:

- a **file**-granular feed (the USN journal) names the file that changed,
  so its folder is re-listed;
- a **directory**-granular feed (macOS fsevents) sets
  `FILE_ATTRIBUTE_DIRECTORY` and names the folder whose *contents* changed,
  so that folder is re-listed — re-listing its parent would see only the
  folder's own mtime and miss the edit inside it.

Separators are normalized to `/` before the split, because `name` is a
platform path join (`\` on Windows) while the index speaks `/`.

A record outside the indexed tree is dropped. The feed can be wider than
the tree — the Windows journal is the volume's, and the daemon indexes a
folder of it — and a change in another tree would put entries in the index
that this tree never had.

`Live::run_watch_feed(watch, root)` drives a feed to its end. It holds
`&mut self` for the feed's whole life, which suits a caller that owns the
`Live`; a daemon sharing it behind an `RwLock` takes the write lock per
batch instead.

### Statting

`live::lstat(path)` stats a path without following links and returns its
size and mtime, or `None` if it is gone. `live::stat_pool()` is the thread
pool the parallel stat scans run on: one lstat per indexed file is how lost
fsevents history is recovered, and that is a lot of syscalls to make on one
thread.

## The query layer (`name_query`)

Parses a query, filters the entries, scores them and ranks them.

- Tokens: fuzzy (bare), exact (`'x`), prefix (`^x`), suffix (`x$`), and the
  filter vocabulary (`type:`, `dir:`, `size:`, `mtime:`, `re:`, globs).
  Alternatives are `|`-separated, and a filter that cannot be compiled
  fails closed.
- The scorer is fzf-v1: the same shape, the same tie-breaks.
- The searcher answers from the base, from the overlay, or from both, and
  merges them. A hit from the overlay carries its own path; a hit from the
  base is looked up by entry number.

`NameCache` holds the last query's scored names, so that typing on narrows
instead of rescoring. It lives with the index it was scored against, which
is why `Live` holds it.

## The content index (`content`)

A trigram index over the same tree, for searching inside files.

- Segments are immutable, mmap'd files: a doc table, then the contents
  first indexed there.
- The trigram postings and the per-file 5- and 7-gram bloom filters decide
  which files are worth reading; the file is then read from disk and
  matched for real, so a result is never stale.
- `Content::covers` vouches for a file only when its recorded stats still
  match the disk. A stale or foreign index loses coverage and is read
  from disk instead of silently answering wrong.

## The rules

- The index holds what the walk saw; the overlay holds what changed since.
  Neither is allowed to answer for the other.
- A path the overlay holds supersedes the same path in the base.
- Freshness is checked against the disk, never against the index.
