# Implementation Plan: merging fsearch + everything-rs + rgx into one search stack

Goal: one Rust workspace, one daemon, one CLI — **whole-filesystem name search**
(fsearch's engine + everything-rs's Windows discovery) and **search inside files**
(rgx's engine + adapter families + pack/index stores), on Windows, Linux, and
macOS, at fsearch-grade interactive latency and rgx-grade content throughput.

Section §6 is the complete feature × Windows/Linux/macOS pairing this plan
assumes (per-OS gap lists, semantic traps, parity gates). No feature is
"done" until its row there is `N`/`P` on every OS or explicitly `D` with a
named reason.

Sources of truth for this plan:
- `C:\Users\danalec\Documents\src\fsearch` — macOS whole-disk name + content search (Rust, MIT)
- `C:\Users\danalec\Documents\src\everything\everything-rs` — Windows NTFS MFT/USN indexer (Rust)
- `../rgx` — content search engine, adapters, Tesla Valve packs, `--serve`
- `C:\Users\danalec\Documents\src\everything\everything-c` — C port; **out of scope**
  except as a consumer of shared format specs (keep ERIs/packs readable)

---

## 1. What each project brings (capability map)

| Layer | fsearch | everything-rs | rgx |
|---|---|---|---|
| Name discovery (crawl) | `getattrlistbulk` walk (macOS) | **MFT scan + USN journal (Windows)** | generic walker (rg-compatible) |
| Live updates | FSEvents | USN tail (`tail_usn`) | none (re-walk) |
| Name index | **mmap blob, interned names, DFS ranges, ~149 class bitmaps** | ERI2 paths + `SearchTable` (linear) | none |
| Name matching | **fzf-v1 scoring, typos, NEON SIMD, top-k, NameCache** | `fuzzy.rs` subsequence + rank | none |
| Query syntax | tokens, modes (`'`, `^`, `$`, `!`), `ext/type/kind/in/size/mtime/re/path/grep/regex/sym/limit` | terms, `!/`, `ext/dir/type/size`, globs, `\|`, `re:` | **rg-compatible flags**, `-t` types, `--json`, `--chunk` |
| Content retrieval index | **trigram postings + per-file 5/7-gram bloom, content-hash dedup** | none | **TV pack store** (corpus bytes, incremental, mmap pool) + **token inverted index** |
| Content matcher | memchr/regex verify-on-disk | vendored rgx Pike VM | **byte NFA + Pike VM, SIMD nibble prefilter, literal/hybrid/fixed compilers** |
| Container formats | text files only | text only | **PDF, Office, archives, mail, LLM, media, tabular, 3D, inspect** |
| Daemon / API | UDS JSON-lines (`fsearch stdio`/`serve`) | none | `--serve` line-JSON on stdio |
| Interactive UI | CLI one-shot | terminal UI (crossterm, latency sparkline) | CLI one-shot |
| Latency/memory tricks | keep-warm spinner, mmap big-alloc, FxHash, pools, QoS pools | SearchTable scan rule (30 ns/entry) | **mimalloc**, mmap cache, `MappedStore` pool |
| Platforms | macOS | Windows | Windows + Linux (releases, winget) |

Three takeaways that shape everything below:

1. **The name-search engine and the content-search engine do not overlap at
   all.** fsearch owns names; rgx owns content. everything-rs contributes the
   best discovery backend (MFT/USN) and a query-filter vocabulary.
2. **There are three regex engines in the tree today.** rgx-core is the
   original; everything-rs (and everything-c) vendor copies of it. The merge
   deletes the copies and depends on the original. (everything-c keeps its C
   copy — it is a separate budget.)
3. **Content indexing has two mature, complementary designs** (see §4.3) —
   the merge does not pick a winner between them, it layers them.

---

## 2. Performance targets

Name-search metrics follow the fsearch plan; content metrics follow rgx's
rg-parity bars; merge adds the cross-layer ones. Corpus: ~5M entries for names,
Chromium/kernel trees plus a fixture set of containers for content.

| Metric | Target | Reference point |
|---|---|---|
| Name search, median @ 5M | **≤ 0.5 ms** (fsearch table: 0.13 ms) | fsearch |
| Typing a whole filename (all keystrokes) | **≤ 8 ms total** | fsearch 3.0 ms |
| Content query via index (trigram/pack), median | **≤ 5 ms** | fsearch 2.3 ms |
| Content query via index, p90 worst tree | **≤ 25 ms** | fsearch Chromium p90 2.3 ms |
| Raw content scan (rgx engine, big file) | **≥ rg parity** (rgx already +32…111% on engine rows) | rgx `benchmark/status.md` |
| Walker (`--files`-style enumeration) | **≥ rg parity** (rgx already +37.5%) | rgx |
| Mixed query (name filter + content), median | **≤ 10 ms** via indexes | new |
| Daemon warm query (name or content) | **≤ 8 ms** | fsearch CLI 3.3 ms |
| Live change reflected in name index | **≤ 200 ms** | fsearch ~0.1 s |
| First full index, 5M files | ≤ 60 s (Windows MFT faster; Linux walk is the floor) | fsearch 49 s @ 8.3M |
| Peak memory, first index | **≤ 1.2 GB** | fsearch 0.9 GB |
| Resident name layer @ 5M | **≤ 64 MB** | fsearch 63 MB |
| Content index on disk | ≤ 2.5× corpus text (bloom+postings; packs hold bytes once) | fsearch 2.4× |

Perf gate: the bench harness (Phase 0) blocks a PR on >20% regression in any
row. rgx's existing harness rules carry over (thread-matched rounds, corpus
sha256 fingerprints, min-of-N with warmups, counts proven equal before timing).

---

## 3. Target workspace

One workspace, thin bins, one protocol. `rgx-core` stays a clean crate (it is
already `no_std`-aware and feature-gated); fsearch's engine and
everything-rs's discovery enter as crates of their own.

```
search/  (new root workspace — or extend `../rgx`'s workspace; decide once)
├── crates/
│   ├── name-core            # from fsearch: index, bitmaps, scoring, typos, SIMD
│   │                        #   + everything-rs Query filters merged into the parser
│   ├── discovery             # Source/Watch traits (the platform seam)
│   ├── discovery-windows    # from everything-rs: ntfs.rs, volume.rs, indexer.rs (MFT+USN)
│   ├── discovery-macos      # from fsearch: walk.rs (getattrlistbulk), fsevents.rs
│   ├── discovery-linux      # new: getdents64 bulk walk + inotify
│   ├── live                 # from fsearch live.rs: overlay (dead bits + overrides)
│   ├── content-core         # fsearch content.rs trigram+bloom + rgx token index glue
│   ├── rgx-core             # UNCHANGED home of the engine, tv/, tv_runtime/, adapters trait
│   ├── rgx-adapters-*       # unchanged, feature-gated as today
│   ├── engine               # orchestration: index lifecycle, sync, pools, keep-warm
│   └── proto                # JSON-lines protocol types (shared by daemon, stdio, CLI)
├── bins/
│   ├── searchctl            # the merged CLI (fsearch-style + rg-compatible flags)
│   ├── searchd              # daemon (named pipe / UDS / stdio)
│   └── search-ui            # terminal UI (from everything-rs ui.rs)
└── docs/formats/            # ERI v3, pack v3, content-segment specs (C port follows)
```

Binaries keep the old names as aliases for one release (`fsearch`, `rgx`,
`everything-rs` shim bins calling the merged engine) so existing scripts and
the sibling C port do not break while the dust settles.

---

## 4. The merge decisions (the hard part)

### 4.1 Name search: fsearch's engine wins, everything-rs's syntax wins

- Adopt fsearch `index.rs`/`query.rs` wholesale: interned names, DFS range
  layout, `name_mask` + class bitmaps, fzf-v1 scoring, typo regime, NEON + a
  **new x86_64 SSE2 wide scorer** (fsearch is NEON-only; Windows/Linux are
  x86-first), top-k with score floor, NameCache + pools.
- Adopt everything-rs's filter vocabulary on top of fsearch's token modes:
  `ext/dir/type` (incl. `type:link`, media kinds), `size:` semantics
  (dirs never match size bounds), `|` alternation, globs, fail-closed unknown
  `type:` + warning, `re:` path regex via rgx-core (not the vendored copy —
  see §4.4).
- Persistence: ERI evolves to **v3** (names blob + rows + masks + bitmaps +
  meta + event_id, mmap-loadable in ≤ 50 ms @ 5M). Keep the ERI2 reader.
  everything-rs's atomic-replace save trap (`MoveFileExA`) and fsearch's
  single-owner lock discipline both survive.

### 4.2 Discovery: three backends behind one seam

`discovery::Source` (full enumeration as DFS directory blocks) +
`discovery::Watch` (change batches, or `Rescan` when the feed loses history):

- **Windows**: everything-rs MFT streaming + USN. Carry its trap suite
  verbatim (FRN = `stream_offset / rec_size`, echoed-FRN rejection,
  `OVERLAPPED` reads, 44-byte USN input, resume-USN peel, record-size
  derivation). Fold the `everything-c` RE-AUDIT fixes (M1 cluster floor, M2
  resident `ValueOffset`, D1 USN offsets, M3 extension records, D2 resume
  advance) into this crate — they are shared bugs.
- **macOS**: fsearch `walk.rs` + `fsevents.rs` (incl. `no_materialize`
  iCloud guard, gated-folder/Full Disk Access handling, relist-on-history-loss).
- **Linux**: new `getdents64` bulk walk (8 threads, `O_NOFOLLOW`, skip rules)
  + recursive `inotify` (`IN_Q_OVERFLOW` → `Rescan`); `statx` for meta.
  fanotify FID is a later hardening, not MVP.

### 4.3 Content: layer the indexes, don't choose

The two designs solve different problems and compose:

1. **Candidate selection (which files can match):** fsearch's trigram
   postings + per-file 5/7-gram bloom. This is a *retrieval filter*: it turns
   "scan the corpus" into "scan 0.1% of the corpus" for literal-ish patterns.
2. **Corpus storage (bytes to search):** rgx's **Tesla Valve pack store** —
   corpus bytes packed, mmap'd, incremental updates, atomic manifest, warm
   `MappedStore` pool for the daemon. The pack searcher never runs the
   adapter registry by default (deliberate); the daemon path always has the
   direct engine + adapters available beside it.
3. **Token queries (`foo AND bar` docs):** rgx's token inverted index stays
   as the specialized path it already is.
4. **Per-file confirmation:** rgx engine (literal/hybrid/fixed/VM + SIMD
   nibble prefilter) — strictly better than fsearch's verify path
   (memchr/regex), so verification moves to rgx. fsearch's "verify-on-disk,
   best-first in batches" scheduling survives as the policy around it.
5. **Containers:** rgx adapters apply to *direct* search and to
   `--tv-pre-extract` pack builds. A trigram index over extracted text
   (rather than raw bytes) is a later experiment — extraction changes what a
   trigram means, and the pack path already covers recurrent container
   queries.

`sym:` (definitions) becomes a small posting table built by the same content
worker that builds trigrams (fsearch `content.rs:497` definition keys), not a
regex pass at query time.

### 4.4 One regex engine, one daemon, one allocator

- **Delete the vendored regex copies.** everything-rs `src/regex/` becomes a
  dependency on `rgx-core` (same MIT OR Apache-2.0 lineage it was copied
  from). fsearch keeps its `regex` crate only where the byte-regex crate's
  Unicode mode is needed for `regex:` content queries; path `re:` uses
  rgx-core. *Everything-c keeps its C copy — noted in docs/formats as a
  frozen fork.*
- **One protocol:** fsearch's JSON-lines op model (`{"q", "limit"}`,
  `{"op":"grep"|"status"|"…}`) becomes `proto`, spoken over named pipe
  (Windows), UDS (Unix), and stdio. rgx `--serve` (argv-array per line) stays
  as a **compatibility front** that translates to the same engine calls, so
  rg harnesses keep working. rgx `--json` output stays as-is (rg-compatible).
- **One allocator story:** mimalloc for the general heap (rgx already
  measured 5–12% on 32-worker walks on Windows) + fsearch's big-block rule
  (≥ 1 MiB → mmap/VirtualAlloc, returned on free) for index builds and pack
  segments + `release_memory` after big phases.
- **Latency:** fsearch's keep-warm spinner (250 ms window, cancelled by the
  search itself) wraps the daemon; QoS/priority pools come from fsearch's
  engine.rs (`USER_INTERACTIVE`) via the platform seam.

### 4.5 UX surfaces

- `searchctl` (name): fsearch one-shot ergonomics + everything-rs filters +
  rg-compatible exit codes where they don't conflict; `--json` from rgx.
- `search-ui`: everything-rs terminal UI (sparkline stays; add p50/p90 and
  candidates-skipped), backed by the new name searcher + content grep.
- `searchd`: owns the indexes, keep-warm, follower takeover (fsearch
  `try_upgrade` + single-instance lock).

---

## 5. Feature / contribution matrix

| # | Work item | Source of truth | Lands in | Phase |
|---|---|---|---|---|
| 1 | Bench harness (names + content + walk, corpus digests) | rgx harness rules + fsearch tables | `engine` benches | 0 |
| 2 | RE-AUDIT fixes applied to shared Windows discovery | everything-c `RE-AUDIT.md` | `discovery-windows` | 0 |
| 3 | Delete vendored regex; depend on rgx-core | rgx-core | `discovery-windows`, `name-core` | 1 |
| 4 | Name index v2 (intern, DFS, masks, bitmaps, ERI v3) | fsearch `index.rs` | `name-core` | 1 |
| 5 | Query syntax merge (fsearch modes + everything-rs filters) | both | `name-core` | 1 |
| 6 | fzf-v1 scoring + typos + SSE2/NEON SIMD + top-k + NameCache | fsearch `query.rs` | `name-core` | 2 |
| 7 | mtime in index + `mtime:` | fsearch + MFT `$STANDARD_INFORMATION` | `name-core`, `discovery-*` | 2 |
| 8 | `Source`/`Watch` seam; MFT+USN and mac walk behind it | new (spec §4.2) | `discovery*` | 3 |
| 9 | Linux walk + inotify backend | new | `discovery-linux` | 3 |
| 10 | Live overlay + rescan semantics | fsearch `live.rs` | `live` | 3 |
| 11 | Content trigram+bloom store + verify policy | fsearch `content.rs` | `content-core` | 4 |
| 12 | rgx engine as the confirm path (replace memchr verify) | rgx-core | `content-core` | 4 |
| 13 | TV packs as daemon corpus store (`MappedStore` pool) | rgx `tv_runtime` | `content-core` | 4 |
| 14 | Adapters in the direct path + `--tv-pre-extract` packs | rgx adapters | unchanged crates | 4 |
| 15 | `grep:`/`regex:`/`sym:` + incremental content sync | fsearch + rgx VM | `content-core` | 4 |
| 16 | Daemon `searchd` + `proto` (pipe/UDS/stdio) + `--serve` compat | fsearch + rgx `--serve` | `engine`, `proto` | 5 |
| 17 | CLI merge (`searchctl`, rg-flag compat, `--json`) | all three | bins | 5 |
| 18 | keep-warm + big-alloc + mimalloc + pools | fsearch + rgx | `engine` | 6 |
| 19 | PGO, release profiles, README perf table | fsearch `pgo.sh` + rgx benchmarks | build | 6 |
| 20 | Format specs for the C port (ERI v3, packs) | docs/formats | docs | 6 |

---

## 6. OS parity: every feature × Windows / Linux / macOS

The sections above define *what* to build and *in which order*; this section is
the pairing contract: **every feature on every OS**, what is native, what is
ported, what is degraded (and why), and the work each gap costs.

**Parity rule:** a feature ships as either (a) full behaviour on all three
OSes, or (b) an explicitly *degraded* cell with a named OS reason and a
documented workaround — never silent divergence. A cell may say `—` (N/A) only
when the OS has no corresponding concept at all.

Legend: **N** native · **P** ported (shared code, platform hook) · **D**
degraded (documented limit) · **G** gap (work required, see §6.5) · **—** N/A.

### 6.1 Discovery & metadata

| # | Feature | Windows | Linux | macOS | Notes |
|---|---|---|---|---|---|
| 1.1 | Full-tree enumeration | **N** MFT scan (extent streaming) | **G** `getdents64` bulk walk | **N** `getattrlistbulk` | Linux has no global metadata table; walk is the floor. Faster than readdir loops if bulk syscalls + 8 threads. |
| 1.2 | Live change feed | **N** USN journal | **G** inotify (recursive) | **N** FSEvents | Granularity differs: USN = per-record, FSEvents = coalesced paths (~0.1 s), inotify = per-dir cookie events. |
| 1.3 | Restart replay (catch up while off) | **N** resume from saved USN | **G** **none** — no journal | **N** FSEvents since `event_id` | Linux gap is structural: inotify has no history. Mitigation: mtime-based relist of changed dirs (fsearch `relist_changed` pattern) + optional fanotify FID/markers later. |
| 1.4 | Feed-loss detection | **N** journal wrap / stale usn | **N** `IN_Q_OVERFLOW` | **N** `MUST_SCAN_SUBDIRS`, `USER_DROPPED`, `KERNEL_DROPPED` | All three map to one `Rescan(scope)` in the `Watch` trait. |
| 1.5 | Partial rescan (scope, not whole disk) | **N** per-record rescan | **P** subtree walk on `Rescan` | **N** relist changed dirs | fsearch's `relist_changed` is the reference policy. |
| 1.6 | Permission-denied handling | **P** skip + `skipped` list (ACL) | **P** EACCES skip + list | **P** TCC/gated skip + rescan on grant | macOS Full Disk Access is its own consent model (prompt-avoiding probe exists: `has_full_disk_access`). |
| 1.7 | Lazy/placeholder files | **G** OneDrive/Cloud Files (do not hydrate) | **D** sparse/fscrypt visible as normal | **N** `no_materialize` (iCloud dataless fails fast) | Windows needs the same "never hydrate" guard fsearch has for iCloud. |
| 1.8 | Multi-volume / multi-root | **N** volume ordinal in FRN key | **P** mount table + root set | **P** mounts + firmlinks (cross, don't loop) | `Index::add_root` per root survives everywhere. |
| 1.9 | Hard links & aliases | **N** hardlinks + 8.3 short names | **P** hardlinks | **P** hardlinks + firmlinks + symlinks | Alias rows are searchable extra paths over the same identity. |
| 1.10 | Symlinks | **N** reparse points (`type:link`) | **G** `O_NOFOLLOW` walk + link entries | **P** `KIND_LINK`, not crossed | Windows reparse ≠ POSIX symlink (junctions, mounts) — classify, don't unify blindly. |
| 1.11 | Case sensitivity of the FS | **D** case-insensitive (NTFS default) | **G** case-sensitive (ext4/btrfs/…) | **D** case-insensitive (APFS default) | The biggest *semantic* parity trap — see §6.6. |
| 1.12 | Path encoding | **P** UTF-16 native → UTF-8 at the boundary | **P** byte paths (locale/UTF-8) | **G** Unicode NFD filenames | macOS stores NFD; queries typed in NFC must match. Needs a folding layer (see §6.6). |
| 1.13 | Path separator | **D** `\` | **P** `/` | **P** `/` | Internal storage normalizes to `/` (fsearch `live::normalize`); only the Win32 boundary converts. |
| 1.14 | Long paths | **P** `\\?\` prefix | **N** 4096-byte paths | **D** 1024-byte `PATH_MAX` | macOS limit is real; deep trees need relative-open walking (`openat`-style) there too. |
| 1.15 | mtime/size/kind metadata | **G** parse `$STANDARD_INFORMATION` | **G** `statx` | **N** `getattrlistbulk` already carries it | fsearch filters (`mtime:`, `size:`, `type:`) need this on all three. |
| 1.16 | Sparse/extent awareness | **N** MFT runlists (scan) | **D** FIEMAP optional | **D** `SEEK_HOLE` optional | Only needed for content-index skip heuristics; degrade openly. |

### 6.2 Name search

| # | Feature | Windows | Linux | macOS | Notes |
|---|---|---|---|---|---|
| 2.1 | Interned names + DFS ranges | **P** | **P** | **N** source design | Pure; works once discovery emits DFS blocks. |
| 2.2 | `name_mask` + class bitmaps | **P** | **P** | **N** | Pure. Case classes must fold the way §6.6 dictates. |
| 2.3 | fzf-v1 scoring + caps | **P** | **P** | **N** | Pure. |
| 2.4 | Typo tolerance (≥5 letters) | **P** | **P** | **N** | Pure. |
| 2.5 | SIMD wide scorer | **G** SSE2 path | **G** SSE2 / NEON (aarch64) | **N** NEON | fsearch ships NEON only; x86 SSE2 is new first-class work; aarch64 Linux shares the NEON path. |
| 2.6 | Top-k + score floor + NameCache | **P** | **P** | **N** | Pure. |
| 2.7 | Parallel scan, interactive priority | **P** MMCSS + thread priority | **G** `sched_setaffinity` + nice (no QoS class) | **N** `QOS_CLASS_USER_INTERACTIVE` | Linux has no QoS analogue — document the approximation; pin to perf cores when available. |
| 2.8 | Query modes (`'` `^` `$` `!`) | **P** | **P** | **N** | Pure. |
| 2.9 | Filters `ext/type/kind/in/size/mtime/limit` | **P** | **P** | **N** | `type:link` maps per row 1.10; `type:app` is macOS-shaped (map per-OS, §6.5). |
| 2.10 | `re:` path regex | **P** rgx-core | **P** | **P** | One engine everywhere (the vendored copies die in the merge). |
| 2.11 | Globs `*`/`?` + `\|` alternation | **P** | **P** | **N** | Everything-rs syntax, pure. |
| 2.12 | `sym:` (definitions) | **P** | **P** | **N** | Content-layer index (§6.3); same bytes on all OSes. |

### 6.3 Content search

| # | Feature | Windows | Linux | macOS | Notes |
|---|---|---|---|---|---|
| 3.1 | Trigram + per-file bloom index | **P** | **P** | **N** source design | Pure. Path eligibility lists get per-OS skip dirs (`build/`, `bin/obj` on Win…). |
| 3.2 | Content-hash dedup | **P** | **P** | **N** | Pure (sha256; HW accel parked on nightly — same everywhere). |
| 3.3 | TV pack store + `MappedStore` pool | **P** rgx | **N** rgx ships Linux | **P** | Already cross-platform in rgx. |
| 3.4 | rgx engine confirm (VM, SIMD prefilter, literal/hybrid/fixed) | **P** | **N** | **P** | Byte-mode engine is OS-neutral; SIMD nibble prefilter needs a NEON twin check on Apple Silicon. |
| 3.5 | Adapters (archive/office/pdf/media/…) | **P** | **N** | **P** | Pure in-process; PATH-gated ones (tesseract, soffice) behave the same — probed once, skip when absent. |
| 3.6 | `grep:` / `regex:` smart-case | **P** | **G** smart-case must respect §6.6 | **P** | Case policy is the only OS-sensitive part. |
| 3.7 | Verify-on-disk best-first batches | **P** | **P** | **N** | Pure scheduling; file handles via `CreateFileW` / `open` `O_NONBLOCK\|O_NOFOLLOW` (fsearch already does the latter). |
| 3.8 | Incremental content sync | **P** USN-driven | **G** inotify-driven | **N** FSEvents-driven | Same worker; different feed. Linux inherits row 1.3's replay limit. |
| 3.9 | Text/encoding detection | **P** rgx `encoding.rs` | **N** | **P** | OS-neutral. |

### 6.4 Daemon, UI, platform services

| # | Feature | Windows | Linux | macOS | Notes |
|---|---|---|---|---|---|
| 4.1 | Daemon transport | **P** named pipe | **P** UDS | **P** UDS | `proto` JSON-lines identical on all three. |
| 4.2 | Single-instance ownership | **P** lock file + `LockFileEx` | **P** `flock` | **P** `flock` | Follower attach + takeover (fsearch `try_upgrade`) is portable. |
| 4.3 | stdio mode | **P** | **P** | **P** | Pure. |
| 4.4 | rgx `--serve` compat front | **P** | **N** | **P** | Pure protocol shim. |
| 4.5 | Terminal UI | **P** VT + `ReadConsoleInputW` | **G** termios + ANSI | **P** termios + ANSI | Win32 console path is the odd one (everything-rs `ui.rs` already owns it). |
| 4.6 | UTF-16 argv fidelity | **N** `GetCommandLineW` | — UTF-8 argv | — UTF-8 argv | Fixes the known everything-rs gap on Windows only; N/A elsewhere. |
| 4.7 | Console output encoding | **G** UTF-8 codepage / `WriteConsoleW` | **N** UTF-8 | **N** UTF-8 | Windows-only footgun (CP 65001 vs legacy console). |
| 4.8 | keep-warm spinner (250 ms) | **P** `SetThreadPriority` + `timeBeginPeriod` | **P** spinner thread | **P** spinner + QoS | Same policy; different priority knobs. Cap + headless-off on all. |
| 4.9 | Big-block allocator (≥1 MiB → map, free returns) | **P** `VirtualAlloc` | **P** `mmap` | **P** `mmap` | fsearch rule, one `platform` module. |
| 4.10 | mimalloc default heap | **N** rgx (measured 5–12% on 32 workers) | **P** | **P** | Gated feature stays. |
| 4.11 | `release_memory` after big phases | **P** `VirtualFree` path | **P** `malloc_trim` | **D** no trim (drop caches manually) | macOS malloc does not return aggressively — document RSS behaviour. |
| 4.12 | PGO release builds | **P** MSVC `/GENPROFILE` or gcc | **P** `-fprofile-generate/use` | **P** clang `-fprofile-instr-generate` | One `make`/`cargo` profile per toolchain. |
| 4.13 | Login-item autostart | **G** Startup task / `HKCU Run` | **G** systemd user unit | **N** launchd (`fsearch install --login`) | fsearch has the macOS path only; the other two are new. |
| 4.14 | Elevated-privilege model | **D** Admin for MFT raw reads | **N** none needed (walk as user) | **D** Full Disk Access for gated folders | Documented per-OS consent flow in the README; never prompt from the daemon. |
| 4.15 | Packaging / install | **G** winget (rgx has manifests) | **N** tarballs (rgx) | **G** brew or `.pkg` | rgx release machinery is the template. |
| 4.16 | CI matrix | **P** windows-latest | **P** ubuntu-latest | **P** macos-latest | Parity tests (§6.7) run on all three. |

### 6.5 Gap work lists (what each OS still owes)

**Windows (G cells)**

1. **1.7** OneDrive/Cloud Files: detect placeholders (`FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS` / `FILE_ATTRIBUTE_RECALL_ON_OPEN`) and refuse to hydrate — the `no_materialize` twin.
2. **1.15** mtime: parse `$STANDARD_INFORMATION` in the pure NTFS layer (RE-AUDIT M-traps apply).
3. **2.5** SSE2 wide scorer (first-class; this is the main Windows typing-latency lever).
4. **2.7** interactive priority via MMCSS ("Games") + `SetThreadPriority`.
5. **4.5/4.7** console: UTF-8 codepage handling / `WriteConsoleW` fallback.
6. **4.13** autostart registration (Startup task preferred over registry Run).
7. **4.15** winget manifest update once the merged binary name settles.
8. **2.9** `type:app` is macOS-shaped — map Windows equivalents (`.exe`/`.lnk`/UWP) explicitly instead of reusing the name blindly.

**Linux (G cells)**

1. **1.1/1.2** `discovery-linux`: `getdents64` bulk walk (8 threads, `O_NOFOLLOW`, skip rules) + recursive inotify.
2. **1.3** *Structural gap*: no journal. Implement the mtime-relist policy (`relist_changed` port) and mark restart-catch-up as **degraded** until fanotify FID/markers land (later hardening, not MVP).
3. **1.10** symlink semantics: `lstat` classification + never-follow in walks.
4. **1.11** case-sensitive FS: smart-case query policy (lowercase pattern → insensitive; any uppercase → sensitive); bitmap classes fold ASCII-only until Unicode folding arrives (same limitation as rgx's byte engine).
5. **1.15** `statx` for mtime/size/kind (batch per directory where possible).
6. **2.5** SSE2 on x86_64 (shared with Windows) + NEON on aarch64 (shared with macOS).
7. **2.7** priority: affinity + nice; document "no QoS class exists".
8. **3.8** incremental content sync driven by inotify (with the row 1.3 caveat).
9. **4.5** termios/ANSI UI port.
10. **4.13** systemd user unit for autostart.

**macOS (G cells)**

1. **1.7** keep `no_materialize` (done in fsearch — port, don't regress).
2. **1.12** NFD/NFC: fold paths and queries through one normalization (compare NFC-folded bytes; store original bytes for display/open). Applies to name masks and content path keys.
3. **1.14** deep-tree walking: relative `openat`-style traversal when paths approach `PATH_MAX`.
4. **2.5** keep NEON; add the same equivalence tests the SSE2 path gets.
5. **4.11** document RSS behaviour without `malloc_trim`.
6. **4.13** launchd login item exists (fsearch `install --login`) — carry into the merged CLI.
7. **4.15** packaging (brew or pkg) once names settle.

**Cross-cutting (all OSes)**

1. Query-semantic diff suite: same corpus image, same expected hits per OS (with per-OS expected sets where semantics legitimately differ: case, links, gated paths).
2. Event-feed replay tests per OS (USN wrap, FSEvents history loss, inotify overflow) — all must land in `Rescan` with correct scope.
3. Path normalization golden tests: `\`↔`/`, NFD↔NFC, `\\?\` prefixes, trailing dots/spaces on Windows, drive-relative paths.

### 6.6 Semantic traps that break "parity" if ignored

1. **Case sensitivity (row 1.11)** — the same query means different things per FS. Rule: query strings carry smart-case; the index stores a folded mask plus the original; exact-mode (`'`) matches FS semantics of the host OS and the docs say so.
2. **Unicode normalization (row 1.12)** — macOS NFD vs typed NFC; Windows preserves case but collapses some legacy short names; Linux is raw bytes. Rule: all comparisons run on a fold of the path (ASCII case-fold + NFC); display and I/O keep the original bytes/UTF-16.
3. **Separator and prefix shapes (rows 1.13/1.14)** — `C:\`, `\\?\C:\`, `/`, and trailing `\`/`/` must collapse to one internal identity (fsearch `normalize` + everything-rs `volume_key` both survive; extend, don't replace).
4. **Link identity (rows 1.9/1.10)** — 8.3 names, hardlinks, firmlinks, junctions, and symlinks are alias rows over one identity on their own OS; the merge must not pretend an NTFS junction equals a POSIX symlink in `type:`.
5. **Live-feed honesty (rows 1.2/1.3)** — "reflected ≤ 200 ms" is true on Windows/macOS; on Linux after a reboot it means "relisted", not "replayed". The API distinguishes `Replayed` vs `Relisted` in `Status`.
6. **Privilege surfaces (row 4.14)** — Admin (Windows raw volume), Full Disk Access (macOS gated dirs), nothing (Linux walk). The daemon never prompts; it degrades to `skipped` lists like fsearch already does.

### 6.7 Parity acceptance gates

- **Matrix test suite**: one test ID per table row above; each runs on the CI matrix (windows-latest, ubuntu-latest, macos-latest). A `G` cell fails CI until it is `P`/`N` or flipped to `D` with a linked issue and README note.
- **Behavioural corpus**: one fixture tree (links, case twins, unicode names, deep paths, gated dirs, container files) checked into `docs/formats/fixtures`, materialized per OS; expected results per OS live beside it.
- **Status surface**: `searchctl status` prints per-OS capability flags (journal replay: yes/no, QoS: native/approx, privilege: full/gated) so "degraded" is visible at runtime, not only in docs.
- **Docs**: the merged README carries this matrix (auto-generated from a single source-of-truth table in `docs/formats/parity.toml` so it cannot rot).

## 7. Phases and gates

### Phase 0 — Baseline & shared correctness (blocking)
- Bench harness reproducing both families of numbers: fsearch name/content
  tables and rgx's rg-parity tables (thread-matched, corpus-sha256, counts
  proven equal first). Checked-in "before" table.
- Apply the everything-c RE-AUDIT criticals (M1, M2, D1, M3, D2) to
  `discovery-windows` — same bugs exist in everything-rs's copy of the code.
- **Gate**: harness ±5% run-to-run; trap suites of both parents green.

### Phase 1 — Name core converges
- `name-core` crate from fsearch; ERI v3; query syntax merge; delete vendored
  regex (depend on rgx-core); `searchctl name` path replaces
  everything-rs's linear SearchTable scan (API-compatible with `ui.rs`).
- **Gate**: name search p50 ≤ 2 ms @ 1M (pre-SIMD); ERI load ≤ 50 ms @ 5M;
  query-visible behaviour change only where the merge deliberately unified
  semantics (listed in the changelog); both parents' test suites green.

### Phase 2 — Scoring, typos, SIMD
- fzf-v1 + typos + SSE2/NEON scorer + top-k + NameCache + parallel
  interactive-QoS scan + mtime filters.
- **Gate**: name p50 ≤ 0.5 ms @ 5M; typing ≤ 8 ms total; typo recall ≥ 99%
  (fsearch suite); scorer zero-alloc on the ASCII path.

### Phase 3 — Discovery seam + platforms
- `Source`/`Watch` traits; Windows and macOS behind them (pure refactor);
  Linux backend lands; `live` overlay unifies USN/FSEvents/inotify semantics
  (`Rescan` on feed loss, replay-quiet rules from fsearch).
- **Gate**: Linux first index ≤ 60 s / 5M; live change ≤ 200 ms on all three
  OSes; CI matrix green (windows-latest, ubuntu-latest, macos-latest).

### Phase 4 — Content converges
- trigram+bloom store (`content-core`), rgx engine as confirm path, TV packs
  as the daemon's corpus store with the warm pool, adapters on the direct
  path, `grep:`/`regex:`/`sym:`, incremental sync from `live`.
- **Gate**: indexed content p50 ≤ 5 ms / p90 ≤ 25 ms (Chromium); raw-scan
  rows ≥ rg parity (existing rgx gates hold); zero false negatives on the
  fixture suite (bloom may false-positive, never miss); containers still
  searchable in the direct path (rgx adapter tests green).

### Phase 5 — One daemon, one CLI
- `searchd` (pipe/UDS/stdio, `proto`), follower takeover, keep-warm armed
  around queries; `searchctl` merges the flag surfaces; rgx `--serve`
  compatibility front.
- **Gate**: daemon warm query ≤ 8 ms; two-process handoff test; rg harness
  still passes through `--serve`/`--json`; fsearch socket clients work
  against `proto` via a shim or a version bump.

### Phase 6 — Polish & release
- mimalloc default + big-alloc + release-memory; PGO profiles per OS; README
  with the merged perf table; `docs/formats/` for the C port; deprecation
  shims for old bin names; winget/tarball packaging follows rgx's existing
  release machinery.
- **Gate**: full §2 table green on Windows, Linux, macOS.

---

## 8. Testing & verification strategy

- **Inherit both parents' oracles.** rgx: differential oracle vs the `regex`
  crate (406 cells), CLI differential vs ripgrep, adapter fixtures, pack
  round-trips, corpus sha256 discipline. fsearch: typo suite, scoring goldens,
  bloom zero-false-negative tests, MFT/USN trap regressions (from
  everything-rs + RE-AUDIT fixes).
- **Merge-specific:** query-semantic diff table (old everything-rs query →
  new unified query → same result sets on a fixture volume image);
  name-cache extension property tests; ERI v2→v3 migration round-trips;
  protocol golden files (`proto` request → reply) for daemon/stdio/`--serve`.
- **Privileged/integration (opt-in):** real-volume A/B (`ERS_USE_RECORDS`),
  USN/FSEvents/inotify replay, two-process daemon handoff, keep-warm
  behaviour under load (no starvation of the UI thread).
- **Bench gate** (Phase 0 harness) on fixed corpora in CI; >20% regression
  blocks merge.

## 9. Risks & trade-offs

| Risk | Mitigation |
|---|---|
| Semantic drift between three query dialects | one dialect with documented mappings + diff-table tests; keep rg-flag compat only where rg defines it |
| SSE2 scorer is new (fsearch is NEON-only) | scalar is the fallback of record; equivalence tests vs scalar |
| Windows discovery bugs are shared latent defects | RE-AUDIT fixes are Phase 0, not a later nice-to-have |
| Content index scope (trigram vs packs vs tokens) confuses users | one daemon auto-selects (like rgx `--engine auto`), `--explain` says which layer answered |
| Packs don't run adapters by default (deliberate rgx behaviour) | keep it; document; `--tv-pre-extract` and the direct path cover the gap |
| mimalloc + custom big-alloc interactions | big-alloc bypasses the global heap entirely (mmap/VirtualAlloc); instrumented tests |
| Workspace merge churn breaks both release trains | shims for old bin names; phases land as compatible releases; C port only reads formats |
| keep-warm looks like malware to AV | 250 ms cap, headless-off, documented |
| Scope: "merge everything" is huge | phases ship independently; Phase 1–2 already deliver the fsearch feel on Windows |

## 10. First PR sequence

1. Bench harness (names + content + walk) with checked-in baseline tables.
2. RE-AUDIT criticals (M1/M2/D1/M3/D2) in the shared Windows discovery code.
3. Workspace skeleton: `name-core`, `discovery`, `engine`, `proto` crates; old
   bins compile against them unchanged.
4. Delete vendored regex in everything-rs's code path; depend on rgx-core.
5. Name index v2 (interning, DFS, masks, bitmaps, ERI v3) + syntax merge.
6. SSE2/NEON scorer + fzf-v1 + typos + top-k + NameCache.
7. mtime + `Source`/`Watch` seam + live overlay (Windows + macOS first).
8. Linux discovery backend.
9. Content: trigram+bloom store + rgx engine as confirm + `grep:`.
10. TV packs as daemon store + adapters on direct path + `regex:`/`sym:`.
11. `searchd` + `proto` + `--serve` compat + `searchctl` merge.
12. keep-warm + mimalloc/big-alloc + PGO + merged README perf table.

PRs 5–6 alone make the merged stack feel like fsearch on Windows; 9–10 make
content queries index-fast; 11 is when the three products become one.
