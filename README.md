# rgy

Whole-filesystem name search and content search in one stack: fsearch's name
engine and query ergonomics, zen-rs's Windows discovery, rgx's content engine,
adapters and pack store. Windows, Linux and macOS.

## rgy or rgx?

[rgx](https://github.com/danalec/rgx) is the content-search specialist: an
`rg` replacement that reads *inside* files — PDFs, Office documents,
archives, mail, media metadata, 3D models, LLM checkpoints — with `rg`'s
flags, output and exit codes, the Tesla Valve pack store, and engine knobs
for harnesses. The `rgx-*` crates in this workspace are its engine and
adapters, transported verbatim.

rgy is the broader question: **which file?** — asked over the whole
filesystem, then **what is inside it?** in the same query.

| | rgy | rgx |
|---|---|---|
| Which file, anywhere on the disk | ✅ name index, fzf-v1 ranking, typos, live overlay | walk + path globs |
| What is inside it | ✅ `grep:` / `regex:` / `sym:` | ✅ rg patterns |
| Inside PDF / Office / archives / mail / 3D / LLM | ⬜ families vendored, search path not wired yet | ✅ |
| Name and content in one query | ✅ `report ext:pdf grep:tesla` | pattern + globs |
| Drop-in `rg` (flags, output, exit codes) | exit codes only | ✅ |
| Persistent index across runs | ✅ `--index` (pinned root, `--max-age`) | walk per run (pack store opt-in) |
| Pack-store control | `searchd --tv-dir`, `rgx-tv build/search/update/inspect` | ✅ `--tv-build`, `--engine auto\|tv\|rgx` |
| Forensic triage (magic ID, carving, strings, entropy, x509) | — | ✅ `inspect` adapter |

**The tech-savvy pick is rgx.** If you already think in `rg` flags and
regexes, script against `--json` submatches, pick engines, build pack stores,
or triage binaries — take [rgx](https://github.com/danalec/rgx).

**The broad, simple pick is rgy.** It answers both everyday questions over
the whole filesystem in one plain query language — words, plus
`ext:`/`type:`/`size:`/`dir:`/`re:`, `|` to alternate — with one daemon
underneath and nothing to configure:

```sh
searchctl --root ~ "invoice ext:pdf"      # which file
searchctl --root ~ "invoice grep:tesla"   # and what is inside it
```

> **Where the numbers live.** rgy's are targets until the plan's per-OS
> verification lands — name lookup **≤ 0.5 ms** median at 5M entries
> ([`docs/IMPLEMENTATION_PLAN.md`](docs/IMPLEMENTATION_PLAN.md) §2) — with
> measurements so far in [`benchmark/`](benchmark/). rgx's are measured and
> dated in its
> [`benchmark/`](https://github.com/danalec/rgx/tree/main/benchmark); quote
> them with their run date and host.

## Requirements

Rust **1.85** or newer. The workspace mixes editions on purpose: the `rgy`
crates and binaries are edition 2024, the vendored `rgx` crates stay on 2021
so they keep diffing cleanly against upstream.

Nothing to install beyond the OS. Everything else — the allocator, the
terminal layer, the SIMD — is compiled in; the platform code is chosen by
`cfg` at build time.

## Try it

```sh
# name: fuzzy words, filters narrow, hits ranked
cargo run --release -p searchctl -- --root crates "walk ext:rs"
# content: `grep:` literal, `regex:`, `sym:` — output is path:line:text
cargo run --release -p searchctl -- --root crates "grep:Listing"
# both in one query: the name narrows what is read, the content confirms
cargo run --release -p searchctl -- --root crates "walk ext:rs grep:Listing"
# the daemon: JSON lines in, hits frames out, index warm between queries
echo '{"op":"grep","q":"regex:fn.list"}' | cargo run --release -p searchd -- --root crates
# rgx --serve compat: one JSON argv-array per line, begin/end framing
echo '["-e","Listing"]' | cargo run --release -p searchd -- --root crates --serve
# transports: --listen uds:/tmp/rgy.sock (unix) or pipe:rgy-search (windows)
```

`type:` names kinds (`file`/`dir`/`link`, media kinds like `image`); a bare
extension goes in `ext:`. An unknown `type:` fails closed — warning on
stderr, zero matches, exit 1.

## The pack store (`rgx-tv`)

`grep:`/`regex:`/`sym:` search file bytes on demand. A **pack store** is the
other shape: ingest a tree once into aggregated, CRC32-checked packs, then
answer content queries out of the packed bytes — a corpus that does not
change re-reads nothing.

Four subcommands, one store. `rgx-tv` is a target of the vendored
`rgx-core` crate behind `required-features = ["tv-runtime"]`, so a plain
`cargo build` skips it — build it explicitly:

```sh
cargo build --release -p rgx-core --bin rgx-tv --features tv-runtime

./target/release/rgx-tv build . --store ~/tv-store          # ingest a tree
./target/release/rgx-tv search tesla --store ~/tv-store --stats
./target/release/rgx-tv update . --store ~/tv-store --compact
./target/release/rgx-tv inspect --store ~/tv-store          # CRC32 + stats
```

| Subcommand | Does | Takes |
|---|---|---|
| `build <root> --store <dir>` | ingest a raw tree into verified packs | ingest flags |
| `search <pattern> --store <dir>` | query the packs, keeping strict file boundaries | search flags |
| `update <root> --store <dir>` | publish a new generation from new/modified files | ingest flags |
| `inspect --store <dir>` | verify pack checksums, report store statistics | `--store` only |

**Flags are per-subcommand and they fail loudly.** A flag the subcommand does
not own is an error, not a no-op: `rgx-tv build . --store d --stats` exits 1
with `rgx-tv build: unrecognized option '--stats'`.

### Ingest flags — `build` and `update`

| Flag | Effect |
|---|---|
| `--store <dir>` | the store directory — required by every subcommand |
| `--pack-size-mb <N>` | target pack size in MiB (default 16). `build` only |
| `-g, --glob <PAT>` | include/exclude glob, repeatable, gitignore-style |
| `--hidden` | ingest hidden files, as rgx's `--hidden` means it |
| `-L, --follow` | follow symbolic links |
| `--no-ignore` | do not read `.gitignore`, `.ignore` or parents' |
| `--max-depth <N>` | maximum directory depth |
| `--compact` | **update only.** Updates are append-only, so a store updated often grows without bound; this reclaims the superseded payloads. It is accepted but missing from `rgx-tv --help` |

### Search flags — `search` only

| Flag | Effect |
|---|---|
| `--engine <which>` | `auto` (default: the in-house engine for a line-anchored pattern, `grep` otherwise — measured, see `EngineMode::pick`), `rgx` (always in-house where the pattern compiles), `grep`, or `both` (cross-checked, divergences named on stderr) |
| `-j, --threads <N>` | search threads. Default is three quarters of the physical cores, opened up to every logical processor when the pack's own timings say the scan, not the copy, is the expensive half. `-j` pins it |
| `-c, --count` | print only the count of matching lines per file |
| `-n, --line-number` | show 1-based line numbers (default: already on) |
| `-A/-B/-C <n>` | after / before / both context lines |
| `-a, --text` | search files the build marked binary |
| `-M, --max-filesize <N>` | skip files larger than N; suffixes `k`/`m`/`g`, as in `rg` |
| `--no-mmap` | **default.** Buffered reads; workers claim whole packs in parallel, nothing is mapped |
| `--mmap` | map every pack instead of reading it. The unmap is the largest single stage of a one-shot query on Windows — for comparison, and for processes that hold a store open across queries |
| `--serial-buffered` | buffered reads by the coordinator, one pack at a time; the `--no-mmap` behaviour before that flag existed |
| `--packs-per-thread <N>` | packs a worker claims per claim, buffered mode only (default 1) |
| `--pool` | reuse one set of pack mappings across queries in this process. Only useful with `--mmap`, and only when the process queries the store more than once |
| `--no-pool` | deprecated — the pool is off by default, so this asks for what a bare invocation already does. Accepted with a stderr note so old scripts notice |
| `--leak-mmap` | skip the explicit unmap, let process exit reclaim the mappings. Single-shot only; a no-op in buffered mode |
| `--validate` | stat every file the manifest claims and report the ones the filesystem disagrees about. A stat per file — the cost the pack exists to avoid — and what it buys is the difference between this generation and now |
| `--stats` | print the stage breakdown to stderr |

### What rgy's own binaries expose

`searchctl` has **no** pack-store flag. `searchd` has exactly one:
`--tv-dir <store>`, and it routes both the `grep` op and the `--serve`
compat front to the packs instead of the direct engine.

**The tuning flags stop at the daemon's edge.** `searchd` calls the pack
searcher with a hardcoded `SearchConfig::default()`
([`crates/engine/src/tv.rs`](crates/engine/src/tv.rs)), so none of the search
flags above are reachable from `searchd` — no `--engine`, `-j`, `--stats`,
`-A/-B/-C`, `-M`, or mmap choice. Set those by running `rgx-tv search`
directly.

## Workspace status

The merge is landed end to end (plan first-PR sequence 1–12): name search,
content search, one daemon/CLI, and the polish layer. What is left is
verification on machines this host does not have.

### Name search

| Piece | Where | State |
|---|---|---|
| Name index v2 (intern, DFS, bitmaps, mmap blob) | `crates/name-core` (`name_index`) | ✅ from fsearch |
| Query syntax merge (fsearch tokens + zen-rs filters) | `crates/name-core` (`name_query`) | ✅ `type:`/`dir:`/`size:`/`re:`/globs/\|, fail-closed |
| fzf-v1 scorer + typos + top-k + NameCache + **SSE2 wide scorer** | `crates/name-core` (`name_query`) | ✅ parity-tested vs scalar |
| Live overlay | `crates/name-core` (`live`) | ✅ portable `lstat` |
| Index persistence (sidecar-pinned root) | `crates/name-core` (`persist`) | ✅ |

### Discovery

| Piece | Where | State |
|---|---|---|
| Seam (`Source`/`Watch`, DFS `Listing` blocks) | `crates/discovery` | ✅ portable walk |
| Walk selector (`source_for`, per-platform by `cfg`) | `crates/discovery-dispatch` | ✅ differential vs `scan_sequential`, green on Windows + Linux |
| Windows backend (MFT + USN, RE-audit fixes) | `crates/discovery-windows` | ✅ `MftSource` / `UsnWatch` |
| Linux backend (getdents64 + inotify) | `crates/discovery-linux` | ✅ suite green on a real kernel |
| macOS backend (getattrlistbulk + FSEvents) | `crates/discovery-macos` | ⬜ transported, unverified — needs a real Mac |
| FreeBSD backend (getdirentries + `d_type`) | `crates/discovery-freebsd` | ✅ suite green on a FreeBSD 15.1 kernel (2026-10-14) |

> **The platform walks are selected, not stranded.** `source_for(root,
> threads)` picks the native backend per platform — the MFT walk when the
> root is a volume root on Windows, `getdents64` on Linux, `getattrlistbulk`
> on macOS, `getdirentries` on FreeBSD — and the portable walk answers
> everywhere else and whenever a native backend refuses. The pick is a speed
> decision, never a semantics one: each backend is held against
> `scan_sequential` by the dispatch's differential test. What remains
> unverified is macOS (needs a real Mac); the FreeBSD reader is verified
> against a FreeBSD 15.1 kernel (2026-10-14, `docs/freebsd-support.md` §4).

### Content

| Piece | Where | State |
|---|---|---|
| Content index (trigram + bloom) | `crates/name-core` (`content`) | ✅ transported from fsearch |
| Confirm via rgx engine + `grep:`/`regex:`/`sym:` | `crates/name-core` (`content`), `crates/rgx-core` | ✅ |
| rgx engine, TV packs, adapters | `crates/rgx-*` | ✅ verbatim (re-synced with the cleanroom codec rewrites) |
| One regex engine (vendored copy deleted) | `crates/name-core` (`regex`) → `rgx-core` | ✅ (`rgx-core` stays `no_std`) |
| Pack compaction | `crates/rgx-core` (`tv_runtime`) | ✅ opt-in, `--compact` |

> **`grep:`/`regex:`/`sym:` search file bytes today, not containers.** The
> `rgx-adapters-*` families are transported verbatim, but no binary wires
> them into the search path yet — searching *inside* PDF/Office/archives is
> [rgx](https://github.com/danalec/rgx)'s job until they are wired in.

### Binaries and tooling

| Piece | Where | State |
|---|---|---|
| `searchctl` (name + content CLI, rg exit codes, `--json`) | `bins/searchctl` | ✅ |
| `searchd` (JSON-lines daemon) | `bins/searchd` | ✅ name + `grep:`/`regex:`/`sym:` ops |
| Named pipe / UDS fronts, `--serve` compat, `--tv-dir` | `bins/searchd` | ✅ |
| zen-rs bin / TUI (unchanged behaviour) | `bins/zen-rs`, `bins/search-ui` | ✅ |
| keep-warm / mimalloc / big-alloc (`BlobMut::anon`) | `crates/engine`, `crates/name-core` | ✅ |
| PGO (trained on the name bench) | `pgo.sh` (unix), `pgo.ps1` (Windows) | ✅ validated end to end on Windows |
| Bench harness + baselines | `crates/engine/examples`, `crates/name-core/examples`, `benchmark/` | ✅ |

### Perf gate

`cargo run --release -p engine --example name_bench` — a >20% regression on
any `benchmark/baseline-names.md` row fails the PR.
`benchmark/mmap-vs-read.md` records the mmap-vs-read measurements
(Linux + Windows) behind the access-shape choices in
[`docs/insights-from-rgx.md`](docs/insights-from-rgx.md).

## OS parity

Auto-generated from [`docs/formats/parity.toml`](docs/formats/parity.toml) by
[`scripts/parity-matrix.py`](scripts/parity-matrix.py) -- regenerate with
`python scripts/parity-matrix.py`. A `G` cell is a gap (work required) and
fails CI until it is ported or documented as degraded.


**N** native &middot; **P** ported (shared code, platform hook) &middot; **D** degraded (documented limit) &middot; **G** gap (work required) &middot; **-** N/A

| # | Feature | Windows | Linux | macOS | Notes |
|---|---------|---------|-------|-------|-------|
| | **§6.1** | | | | |
| 1.1 | Full-tree enumeration | N | G | N | MFT scan (extent streaming) \| getdents64 bulk walk \| getattrlistbulk. |
| 1.2 | Live change feed | N | G | N | USN journal \| inotify (recursive) \| FSEvents. Granularity differs: USN/FSEvents are directory-level, inotify per-entry. |
| 1.3 | Restart replay (catch up while off) | N | G | N | resume from saved USN \| none - no journal \| FSEvents by event id. Linux has no journal to replay. |
| 1.4 | Feed-loss detection | N | N | N | journal wrap / stale usn \| IN_Q_OVERFLOW \| MUST_SCAN_SUBDIRS, USER_DROPPED, KERNEL_DROPPED. Windows and Linux map their loss signal to Rescan; the macOS flags are in the tree, but its watch is not wired into the daemon yet (searchd's open_watch says why). |
| 1.5 | Partial rescan (scope, not whole disk) | N | P | N | per-record rescan \| subtree walk on Rescan \| reliable replay by id. |
| 1.6 | Permission-denied handling | P | P | P | skip + skipped list (ACL) \| EACCES skip + list \| TCC/gated skip. Never prompt from the daemon. |
| 1.7 | Lazy/placeholder files | G | D | N | OneDrive/Cloud Files (do not hydrate) \| sparse/fscrypt visible as normal \| no_materialize (iCloud dataless fails fast). Windows needs the same never-hydrate guard fsearch has for iCloud. |
| 1.8 | Multi-volume / multi-root | N | P | P | one name index per volume \| mount table + root set \| mounts + firmlinks. |
| 1.9 | Hard links & aliases | N | P | P | hardlinks + 8.3 short names \| hardlinks \| hardlinks + firmlinks + symlinks. |
| 1.10 | Symlinks | N | G | P | reparse points (type:link) \| O_NOFOLLOW walk + link entries \| KIND_LINK, not crossed. Classify, do not unify blindly (Windows reparse vs POSIX symlink). |
| 1.11 | Case sensitivity of the FS | D | G | D | case-insensitive (NTFS default) \| case-sensitive (ext4/btrfs) \| case-insensitive (APFS default). The biggest semantic parity trap - see 6.6. |
| 1.12 | Path encoding | P | P | G | UTF-16 native -> UTF-8 at the boundary \| byte paths (locale/UTF-8) \| Unicode NFD filenames. macOS stores NFD; queries typed in NFC must match (needs a folding layer, 6.6). |
| 1.13 | Path separator | D | P | P | backslash \| slash \| slash. Internal storage normalizes to / (live::normalize / openable / rel_of); only the Win32 boundary converts. |
| 1.14 | Long paths | P | N | D | \\?\ prefix \| 4096-byte paths \| 1024-byte PATH_MAX. macOS limit is real; deep trees need relative-open walking (openat-style) there too. |
| 1.15 | mtime/size/kind metadata | G | G | N | parse $STANDARD_INFORMATION \| statx \| getattrlistbulk already returns it. |
| 1.16 | Sparse/extent awareness | N | D | D | MFT runlists (scan) \| FIEMAP optional \| SEEK_HOLE optional. |
| | **§6.2** | | | | |
| 2.1 | Interned names + DFS ranges | P | P | N | Pure; works once discovery emits DFS blocks. |
| 2.2 | name_mask + class bitmaps | P | P | N | Pure. Case classes must fold the way 6.6 dictates. |
| 2.3 | fzf-v1 scoring + caps | P | P | N | Pure. |
| 2.4 | Typo tolerance (<=5 letters) | P | P | N | Pure. |
| 2.5 | SIMD wide scorer | G | G | N | SSE2 path \| SSE2 / NEON (aarch64) \| NEON. fsearch ships NEON only; x86 SIMD is the gap. |
| 2.6 | Top-k + score floor + NameCache | P | P | N | Pure. |
| 2.7 | Parallel scan, interactive priority | P | G | N | MMCSS + thread priority \| sched_setaffinity + nice (no QoS class) \| QOS_CLASS_USER_INTERACTIVE. Linux has no QoS analogue - document the approximation; pin to perf cores when available. |
| 2.8 | Query modes (' ^ $ !) | P | P | N | Pure. |
| 2.9 | Filters ext/type/kind/in/size/mtime/limit | P | P | N | type:link maps per row 1.10; type:app is macOS-shaped (map per-OS, 6.5). |
| 2.10 | re: path regex | P | P | P | One engine everywhere (rgx-core); the vendored copies die in the merge. |
| 2.11 | Globs */? + \| alternation | P | P | N | Zen-rs syntax, pure. |
| 2.12 | sym: (definitions) | P | P | N | Content-layer index (6.3); same bytes on all OSes. |
| | **§6.3** | | | | |
| 3.1 | Trigram + per-file bloom index | P | P | N | Pure. Path eligibility lists get per-OS hooks. |
| 3.2 | Content-hash dedup | P | P | N | Pure (sha256; HW accel parked on nightly - same everywhere). |
| 3.3 | TV pack store + MappedStore pool | P | N | P | Already cross-platform in rgx. |
| 3.4 | rgx engine confirm (VM, SIMD prefilter, literal/hybrid/fixed) | P | N | P | Byte-mode engine is OS-neutral. |
| 3.5 | Adapters (archive/office/pdf/media/...) | P | N | P | Pure in-process; PATH-gated ones (tesseract, soffice) behave the same - probed once, skip when absent. |
| 3.6 | grep:/regex: smart-case | P | G | P | Case policy is the only OS-sensitive part; smart-case must respect 6.6. |
| 3.7 | Verify-on-disk best-first batches | P | P | N | Pure scheduling; file handles via CreateFileW / open O_NONBLOCK\|O_NOFOLLOW. |
| 3.8 | Incremental content sync | P | G | N | USN-driven \| inotify-driven \| FSEvents-driven. Same worker; different feed. Linux inherits row 1.3's replay limit. |
| 3.9 | Text/encoding detection | P | N | P | OS-neutral (rgx encoding.rs). |
| | **§6.4** | | | | |
| 4.1 | Daemon transport | P | P | P | named pipe \| UDS \| UDS. proto JSON-lines identical on all three. |
| 4.2 | Single-instance ownership | P | P | P | lock file + LockFileEx \| flock \| flock. Follower attach + handoff. |
| 4.3 | stdio mode | P | P | P | Pure. |
| 4.4 | rgx --serve compat front | P | N | P | Pure protocol shim. |
| 4.5 | Terminal UI | P | G | P | VT + ReadConsoleInputW \| termios + ANSI \| termios + ANSI. Win32 console path is the platform hook. |
| 4.6 | UTF-16 argv fidelity | N | - | - | GetCommandLineW on Windows; UTF-8 argv elsewhere. Fixes the known zen-rs gap on Windows. |
| 4.7 | Console output encoding | G | N | N | UTF-8 codepage / WriteConsoleW \| UTF-8 \| UTF-8. Windows-only fix. |
| 4.8 | keep-warm spinner (250 ms) | P | P | P | SetThreadPriority + timeBeginPeriod \| spinner thread \| spinner + QoS. Cap + headless-off on all. |
| 4.9 | big-block allocator (>=1 MiB -> mmap, free returns) | P | P | P | fsearch rule: VirtualAlloc \| mmap \| mmap, one `platform` module. |
| 4.10 | mimalloc default heap | N | P | P | rgx measured 5-12% on 32-worker walks. searchctl/searchd/zen-rs wired; search-ui is a lib (its binary sets the allocator). |
| 4.11 | release_memory after big phases | P | P | D | VirtualFree path \| malloc_trim \| no trim (drop caches manually). macOS malloc does not return aggressively -- document RSS behaviour. |
| 4.12 | PGO release builds | P | P | P | MSVC /GENPROFILE or gcc \| -fprofile-generate/use \| -fprofile-instr-generate. pgo.ps1 / pgo.sh at the repo root. |
| 4.13 | login-item autostart | G | G | N | Startup task / HKCU Run \| systemd user unit \| launchd. fsearch has the macOS path only; the other two are new. |
| 4.14 | elevated-privilege model | D | N | D | Admin for MFT raw reads \| none (walk as user) \| Full Disk Access for gated folders. Per-OS consent flow in the README; never prompt from the daemon. |
| 4.15 | packaging / install | G | N | G | winget (rgx has manifests) \| tarballs (rgx) \| brew or .pkg. rgx release machinery is the template. |
| 4.16 | CI matrix | P | P | P | windows-latest \| ubuntu-latest \| macos-latest. Parity tests (6.7) run on all three. |

<!-- 53 features; 16 carry at least one G (gap) cell: 1.1, 1.2, 1.3, 1.7, 1.10, 1.11, 1.12, 1.15, 2.5, 2.7, 3.6, 3.8, 4.5, 4.7, 4.13, 4.15 -->

## Docs

| File | What it is |
|---|---|
| [`docs/IMPLEMENTATION_PLAN.md`](docs/IMPLEMENTATION_PLAN.md) | The plan this workspace was built from: capability map, perf targets, per-OS parity matrix, phases. |
| [`docs/insights-from-rgx.md`](docs/insights-from-rgx.md) | What the rgx side taught about storage and access shapes. |
| [`docs/freebsd-support.md`](docs/freebsd-support.md) | Porting plan for FreeBSD, with the gaps ranked by whether they are wrong or merely slow. |
| [`benchmark/`](benchmark/) | Measured tables and the write-ups behind them. |

## Related projects

| Project | Repo | What it provides |
|---|---|---|
| `rgx` | [github.com/danalec/rgx](https://github.com/danalec/rgx) | content search engine, adapters, Tesla Valve packs |

## License

Split-licensed — the full map is in `COPYING`:

- **Apache-2.0 only**: the Zen components (`bins/zen-rs`, `bins/search-ui`,
  `crates/discovery-windows`) and the Tesla Valve pack
  store (`rgx-core` `tv`/`tv_runtime`). Each file carries its
  `SPDX-License-Identifier: Apache-2.0` header.
- **MIT OR Apache-2.0**: everything else. Commercial use is intended
  under Apache-2.0; the MIT option remains for anyone who prefers it.

Full texts: `LICENSE-APACHE`, `LICENSE-MIT`. Third-party notices: `NOTICE`.
