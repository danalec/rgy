# rgy — OS parity

Auto-generated from [`docs/parity.toml`](parity.toml) by
[`scripts/parity-matrix.py`](../scripts/parity-matrix.py) — regenerate with
`python scripts/parity-matrix.py > docs/parity.md`. A `G` cell is a gap
(work required) and fails CI until it is ported or documented as degraded.

**N** native &middot; **P** ported (shared code, platform hook) &middot; **D** degraded (documented limit) &middot; **G** gap (work required) &middot; **-** N/A

| # | Feature | Windows | Linux | macOS | Notes |
|---|---------|---------|-------|-------|-------|
| | **§6.1** | | | | |
| 1.1 | Full-tree enumeration | N | P | N | MFT scan (extent streaming) \| getdents64 bulk walk \| getattrlistbulk. |
| 1.2 | Live change feed | P | P | P | USN journal (the feed delivers the changes; the path a record names is rebuilt from the MFT, one record per level, so the folder to re-list is known) \| inotify (recursive) \| FSEvents, wired into the daemon. Granularity differs: USN/FSEvents are directory-level, inotify per-entry. The macOS stream is callback-based, so an adapter turns it into a pull: a folder event re-lists that folder, and the dropped-event flags (MustScanSubDirs, KernelDropped, UserDropped) become a full rescan, because the live model cannot re-list a subtree. The feed can be wider than the tree (the Windows journal is the volume's), so a change outside the indexed tree is dropped. |
| 1.3 | Restart replay (catch up while off) | D | D | D | the catch-up after a restart is a relist when the saved index is older than 30 minutes, and says so; the journal resume is wired (the feed starts at the saved watermark) and the backlog reaches the index, each record's path rebuilt from the MFT so the folder to re-list is known. The relist stays the default anyway, because the read behind that path (the MFT) needs a volume handle, which needs an administrator: until the path has been walked on a real volume, the slower answer is the safe one. On Linux the limit is permanent: there is no journal to resume from, so the age policy is the only freshness rule there is, and the status answer says which one it was: replayed or relisted. |
| 1.4 | Feed-loss detection | N | N | N | journal wrap / stale usn \| IN_Q_OVERFLOW \| MUST_SCAN_SUBDIRS, USER_DROPPED, KERNEL_DROPPED. All three map their loss signal to Rescan: the journal that wrapped or was recreated, the inotify queue that overflowed, and the FSEvents flags that say the history is gone. |
| 1.5 | Partial rescan (scope, not whole disk) | N | P | N | per-record rescan \| subtree walk on Rescan \| reliable replay by id. |
| 1.6 | Permission-denied handling | P | P | P | skip + skipped list (ACL) \| EACCES skip + list \| TCC/gated skip. Never prompt from the daemon. |
| 1.7 | Lazy/placeholder files | P | P | P | OneDrive/Cloud Files: the recall bits are asked before the open, so a placeholder is reported as skipped and never downloaded \| sparse/fscrypt visible as normal (nothing to hydrate on this OS: the holes read as the zeros they are, and fscrypt decrypts on read) \| SF_DATALESS in st_flags, translated into the same vocabulary, so an iCloud dataless file is refused the way a OneDrive placeholder is (and the reader thread's materialization policy would not fetch it anyway). The guard sits at the open, not in the index: the on-disk format does not change, and a file that is really on the disk is read exactly as before. |
| 1.8 | Multi-volume / multi-root | N | P | P | one name index per volume \| mount table + root set \| mounts + firmlinks. |
| 1.9 | Hard links & aliases | N | P | P | hardlinks + 8.3 short names \| hardlinks \| hardlinks + firmlinks + symlinks. |
| 1.10 | Symlinks | N | P | P | reparse points (type:link) \| O_NOFOLLOW walk + link entries \| KIND_LINK, not crossed. Classify, do not unify blindly (Windows reparse vs POSIX symlink). |
| 1.11 | Case sensitivity of the FS | D | D | D | case-insensitive (NTFS default) \| case-sensitive (ext4/btrfs) \| case-insensitive (APFS default). The biggest semantic parity trap - see 6.6. The smart-case policy is the documented approximation on all three: a lowercase pattern matches either case, so a case-sensitive filesystem is the one that diverges from what the user typed. |
| 1.12 | Path encoding | P | P | P | UTF-16 native -> UTF-8 at the boundary \| byte paths (locale/UTF-8) \| Unicode NFD filenames. Every comparison runs on a fold of both sides -- ASCII case fold plus Unicode NFC (UAX #15, tables generated from the UCD) -- so a name stored as NFD matches a query typed as NFC. The bytes that are displayed and opened are still the ones the filesystem gave. |
| 1.13 | Path separator | D | P | P | backslash \| slash \| slash. Internal storage normalizes to / (live::normalize / openable / rel_of); only the Win32 boundary converts. |
| 1.14 | Long paths | P | N | D | \\?\ prefix \| 4096-byte paths \| 1024-byte PATH_MAX. macOS limit is real; deep trees need relative-open walking (openat-style) there too. |
| 1.15 | mtime/size/kind metadata | P | P | N | parse $STANDARD_INFORMATION \| lstat64 per entry (statx batching is an optimisation, not the metadata) \| getattrlistbulk already returns it. |
| 1.16 | Sparse/extent awareness | N | D | D | MFT runlists (scan) \| FIEMAP optional \| SEEK_HOLE optional. |
| | **§6.2** | | | | |
| 2.1 | Interned names + DFS ranges | P | P | P | Pure; works once discovery emits DFS blocks. |
| 2.2 | name_mask + class bitmaps | P | P | P | Pure. Case classes must fold the way 6.6 dictates. |
| 2.3 | fzf-v1 scoring + caps | P | P | P | Pure. |
| 2.4 | Typo tolerance (<=5 letters) | P | P | P | Pure. |
| 2.5 | SIMD wide scorer | P | P | N | SSE2 path \| SSE2 / NEON (aarch64) \| NEON. fsearch ships NEON only; the x86_64 SSE2 wide scorer is this workspace's own, parity-tested against the scalar matcher. |
| 2.6 | Top-k + score floor + NameCache | P | P | P | Pure. |
| 2.7 | Parallel scan, interactive priority | P | P | N | MMCSS + thread priority \| sched_setaffinity + nice (no QoS class) \| QOS_CLASS_USER_INTERACTIVE. Linux has no QoS analogue: this is an approximation, not the same guarantee, and it says so in the code. The workers are pinned to the performance cores when the machine says which they are. |
| 2.8 | Query modes (' ^ $ !) | P | P | P | Pure. |
| 2.9 | Filters ext/type/kind/in/size/mtime/limit | P | P | P | type:link maps per row 1.10; type:app is macOS-shaped (map per-OS, 6.5). |
| 2.10 | re: path regex | P | P | P | One engine everywhere (rgx-core); the vendored copies die in the merge. |
| 2.11 | Globs */? + \| alternation | P | P | P | Zen-rs syntax, pure. |
| 2.12 | sym: (definitions) | P | P | P | Content-layer index (6.3); same bytes on all OSes. |
| | **§6.3** | | | | |
| 3.1 | Trigram + per-file bloom index | P | P | P | Pure. Path eligibility lists get per-OS hooks. |
| 3.2 | Content-hash dedup | P | P | P | Pure (sha256; HW accel parked on nightly - same everywhere). |
| 3.3 | TV pack store + MappedStore pool | P | P | P | Already cross-platform in rgx: the pack store is rgx-core's `tv`, behind the `tv-runtime` feature (no platform hook), and the daemon reaches it through `searchd --tv-dir`. |
| 3.4 | rgx engine confirm (VM, SIMD prefilter, literal/hybrid/fixed) | P | P | P | Byte-mode engine is OS-neutral: the same rgx-core on every OS, with the prefilter's SIMD chosen per CPU. |
| 3.5 | Adapters (archive/office/pdf/media/...) | P | P | P | Pure in-process; PATH-gated ones (tesseract, soffice) behave the same - probed once, skip when absent. The families are in the tree and build on every OS; what rgy's own binaries do NOT do is read *inside* them - the search path reads bytes. That is the same on all three operating systems, so it is not a per-OS parity question: it is in docs/STATUS.md. |
| 3.6 | grep:/regex: smart-case | P | P | P | Case policy is the only OS-sensitive part; smart-case must respect 6.6. |
| 3.7 | Verify-on-disk best-first batches | P | P | P | Pure scheduling; file handles via CreateFileW / open O_NONBLOCK\|O_NOFOLLOW. |
| 3.8 | Incremental content sync | P | P | P | USN-driven \| inotify-driven \| FSEvents-driven. Same worker; different feed. Linux inherits row 1.3's replay limit. |
| 3.9 | Text/encoding detection | P | P | P | OS-neutral (rgx encoding.rs). |
| | **§6.4** | | | | |
| 4.1 | Daemon transport | P | P | P | named pipe \| UDS \| UDS. proto JSON-lines identical on all three. |
| 4.2 | Single-instance ownership | P | P | P | lock file + LockFileEx \| flock \| flock. Follower attach + handoff. |
| 4.3 | stdio mode | P | P | P | Pure. |
| 4.4 | rgx --serve compat front | P | P | P | Pure protocol shim. |
| 4.5 | Terminal UI | P | P | P | VT + ReadConsoleInputW \| termios + ANSI \| termios + ANSI. Win32 console path is the platform hook. |
| 4.6 | UTF-16 argv fidelity | N | - | - | GetCommandLineW on Windows; UTF-8 argv elsewhere. Fixes the known zen-rs gap on Windows. |
| 4.7 | Console output encoding | P | N | N | UTF-8 codepage / WriteConsoleW \| UTF-8 \| UTF-8. Windows-only fix. |
| 4.8 | keep-warm spinner (250 ms) | P | P | P | SetThreadPriority + timeBeginPeriod \| spinner thread \| spinner + QoS. Cap + headless-off on all. |
| 4.9 | big-block allocator (>=1 MiB -> mmap, free returns) | P | P | P | fsearch rule: VirtualAlloc \| mmap \| mmap, one `platform` module. |
| 4.10 | mimalloc default heap | P | P | P | rgx measured 5-12% on 32-worker walks. searchctl/searchd/zen-rs wired; search-ui is a lib (its binary sets the allocator). |
| 4.11 | release_memory after big phases | P | P | D | VirtualFree path \| malloc_trim \| no trim (drop caches manually). macOS malloc does not return aggressively -- document RSS behaviour. |
| 4.12 | PGO release builds | P | P | P | MSVC /GENPROFILE or gcc \| -fprofile-generate/use \| -fprofile-instr-generate. pgo.ps1 / pgo.sh at the repo root. |
| 4.13 | login-item autostart | P | P | N | startup task, falling back to HKCU Run \| systemd user unit \| launchd agent. crates/autostart writes all three as plain text the user can read. A logon-triggered task needs an administrator on a stock Windows session, so the tool falls back to the registry and says so. Registering is the consent: no prompt, no elevation, no dialog. |
| 4.14 | elevated-privilege model | D | N | D | Admin for MFT raw reads \| none (walk as user) \| Full Disk Access for gated folders. Per-OS consent flow in the README; never prompt from the daemon. |
| 4.15 | packaging / install | P | N | P | winget (danalec.rgy-search) \| tarballs \| brew (rgy-search). The package is called rgy-search because rgy is taken on crates.io (a GameBoy emulator library); on winget and homebrew the name is free, and one name across the registries is worth more than the shorter one. The binaries and the project keep the name rgy, and the repository is danalec/rgy. The release script builds the archives, gates on clippy and the tests, then tags and publishes to the forgejo and to GitHub. No release has been published yet, so the manifest and the formula are staged. |
| 4.16 | CI matrix | P | P | P | windows-latest \| ubuntu-latest \| macos-latest. Parity tests (6.7) run on all three. |

<!-- 53 features; 0 carry at least one G (gap) cell:  -->

**The macOS column is unverified.** The code is transported and compiles there, but it has never run on that OS: there is no machine of that kind in the loop. The cell is a claim about the design, not about a behaviour anyone has watched.
