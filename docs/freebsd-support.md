# FreeBSD support

Scope: `rgy` (the workspace) on `x86_64-unknown-freebsd` and
`aarch64-unknown-freebsd`. Everything below is grounded in what the code does
today, measured rather than assumed; the numbers and file references are the
evidence.

---

## 1. What runs today

**The portable walk is what actually executes.** `discovery::walk::scan`
(std `read_dir` fanned out over a rayon pool) is the only enumerator any
consumer reaches. `discovery-linux` and `discovery-macos` are workspace
members with **zero consumers** — no crate depends on them, and nothing
selects between them and the portable path. They expose `LinuxSource` /
`InotifyWatch` and the macOS equivalent, and no one calls them.

That is the single most important fact for this plan: there is a *seam* but
no *dispatch*. Writing `discovery-freebsd` today would join them as dead
code.

**mmap is absent, and the code degrades rather than fails.**
`rgx-core/src/memmap2/platform/mod.rs` dispatches on linux / macos / windows
and falls through to `fallback.rs` otherwise. On FreeBSD:

| call | today | effect |
|---|---|---|
| `MmapInner::map` | `Err(Unsupported)` | `Blob::from_file` (`name_index.rs:56`) falls back to `Blob::Owned` — reads the whole index into memory |
| `MmapInner::map_anon` | allocates a `Vec<u8>` | index build works, but not as a real anonymous map |
| `map_file` / `flush` / `advise` | read-into-memory / no-ops | correct, unoptimised |

So nothing breaks. What is lost is demand paging: an index that is mmap'd
today costs RSS proportional to the pages actually touched, and on FreeBSD it
costs the whole blob up front. For the 310k-entry tree measured earlier
(8.3 MiB) that is irrelevant; for an order of magnitude more it is not.

**`open_flags` silently disables itself.** `name-core/src/content.rs`
(`mod open_flags`, line 3262) picks `O_NONBLOCK` / `O_NOFOLLOW` per
OS and, for anything that is not Linux/Android/macOS/iOS, sets **both to 0**.
The comment already names the consequence: "a FIFO could block open() here".
FreeBSD lands in that arm today.

---

## 2. Confirmed FreeBSD specifics

From `sys/sys/fcntl.h` (values, not guesses):

```
O_NONBLOCK   0x0004
O_NOFOLLOW   0x0100
O_DIRECTORY  0x00020000
```

`O_NONBLOCK` and `O_NOFOLLOW` are **identical to Darwin's**, which the file
already carries as `0x4` / `0x100`. So the FreeBSD arm is the same two
constants, not new ones. (`O_DIRECTORY` is `0x20000` on both FreeBSD and
Linux and `0x100000` on Darwin — do not reuse a Darwin value.)

Directory enumeration: FreeBSD has `getdents(2)` and `getdirentries(2)`, and
its `struct dirent` carries **`d_type`**, so entry kind comes from the dirent
without a `stat` — the same property the Linux backend uses. Two constraints
the Linux path does not have:

- `getdirentries` requires `nbytes` >= the filesystem block size; a small
  buffer returns `EINVAL` on some filesystems. Size the buffer accordingly.
- `struct dirent` layout is version-dependent (`d_off` present or not). Read
  via `d_reclen` strides and never assume a fixed stride.

There is **no** FreeBSD equivalent of macOS `getattrlistbulk`. Batch stat is
where the macOS backend earns most of its speed, so a FreeBSD backend is
closer to the Linux shape than the macOS one, and the expected win is
smaller.

---

## 3. The gaps, ranked

### P0 — wrong behaviour (not just slow)

**P0.1 `open_flags` is zero on FreeBSD.**
A FIFO anywhere under the scan root can block `open()` indefinitely during
content search, and a symlink's target is opened rather than refused.

*Fix*: add a `freebsd` / BSD arm with `O_NONBLOCK = 0x0004`,
`O_NOFOLLOW = 0x0100`, sharing the Darwin constants.

*Acceptance*: a test creates a FIFO in a fixture tree and asserts
`open_sized` returns without hanging (with `O_NONBLOCK` an open of a FIFO
with no writer fails `ENXIO` instead of blocking). Today that test hangs.

**P0.2 `zen-rs` does not compile.**
`bins/zen-rs` depends on `discovery-windows`, which is Windows-only.

*Fix*: decide one of
(a) gate the dependency and the binary out of the FreeBSD build;
(b) make `zen-rs` use the portable walk on non-Windows and keep MFT on Windows.

(a) is the smaller change; (b) is the honest one if `zen-rs` is meant to be
cross-platform.

*Acceptance*: `cargo build --workspace` succeeds on the FreeBSD target.

### P1 — performance

**P1.1 No mmap.**
Add `memmap2/platform/freebsd.rs`. It is `mmap(2)` with `MAP_SHARED` /
`MAP_PRIVATE` / `MAP_ANON` — structurally the `macos.rs` implementation, not
a new design. Then `page_size()` in `platform/mod.rs` needs a FreeBSD arm
(`sysconf(_SC_PAGESIZE)`, same call Darwin uses).

*Acceptance*: `Blob::from_file` returns `Blob::Mapped` on FreeBSD for a
non-empty file, and returns `Blob::Owned` only for the empty/failed cases it
already handles. Add a test that asserts the variant, so a regression to the
fallback is visible.

**P1.2 Directory enumeration.**
A `getdents`-based reader, taking `d_type` from the dirent so kind does not
cost a `stat`. Scope this only after P2.1 (below), because without dispatch
there is nothing to select it.

*Acceptance*: the differential test already used for the parallel walk
(`parallel_walk_matches_sequential_oracle`) runs against the FreeBSD reader
and matches `scan_sequential` output.

### P2 — process and hygiene

**P2.1 The dispatch question.**
Either wire the seam (a `Source` selector that picks `discovery-windows` /
`-macos` / `-linux` / `-freebsd` / portable by `cfg`) or record in
`discovery/src/lib.rs` that the portable walk is the supported path and the
platform crates are experiments. The current state — crates that build but
nothing selects — is the worst of the two, because it reads as supported.

This gates P1.2. It is cheap: one `pub fn source_for(root, threads)` and a
`cfg` chain.

**P2.2 CI.** There is no `.github/workflows` at all, so this is greenfield.
FreeBSD options, in the order I would try them:

1. `cargo check --target x86_64-unknown-freebsd` on the existing host —
   catches cfg and dependency breakage without a VM, and is free. This is
   the floor and should land first.
2. `vmactions/freebsd-vm` (GitHub Actions) or Cirrus CI for a real kernel.
   The precedent matters here: the Linux run is recorded in the history as
   "two silent failures found", which a cross-check alone would not have
   caught.

**P2.3 Real-kernel run.** Plan a full `cargo test --workspace` on actual
FreeBSD before calling this done. The e2e harness resolves bins by path and
has a staleness guard; that machinery is portable, but the pipe/UDS tests
need a real socket path and the FIFO test needs a real FIFO.

---

## 4. Suggested order

| phase | work | exit criterion |
|---|---|---|
| 0 | `cargo check --target x86_64-unknown-freebsd` + a target in the toolchain | the workspace type-checks for FreeBSD |
| 1 | P0.1 open flags, P0.2 zen-rs gating | builds, and the FIFO test passes instead of hanging |
| 2 | P1.1 freebsd mmap + `page_size` | `Blob::Mapped` on FreeBSD; RSS demand-paged |
| 3 | P2.1 dispatch, then P1.2 getdents reader | differential test passes against the sequential oracle |
| 4 | P2.2 CI + P2.3 real-kernel suite | full suite green on FreeBSD, not just checked |

Phases 0–2 are small and independent; 3 is the only one that needs a design
decision first (P2.1). 4 is what turns "it compiles" into "it works".

---

## 5. Open questions for you

1. **FreeBSD version floor?** It decides whether `O_SEARCH` (13+) and some
   `dirent` layouts are available. Targeting 12.x keeps `getdents` and drops
   `O_SEARCH`.
2. **Is `zen-rs` supposed to run on FreeBSD?** It is the one binary that is
   Windows-only by construction (MFT + USN). Gating it out is honest;
   porting it is a separate project.
3. **Is the native backend seam worth wiring?** Without it, P1.2 has nowhere
   to live. With it, `discovery-linux` and `discovery-macos` stop being dead
   code and become selectable — which is a change to how those two crates are
   understood.

---

## 6. What is already fine on FreeBSD

Recorded so it is not re-investigated:

- `searchd`'s `#[cfg(unix)] serve_uds` — Unix domain sockets work.
  `serve_pipe` is Windows-only and already says so instead of misbehaving.
- `discovery::interrupt` — the `#[cfg(unix)]` arm is `signal(2)`, which
  FreeBSD has.
- `discovery::walk` — portable by construction; `scan_sequential` is the
  oracle and is platform-independent.
- `Blob`'s fallback path is exercised and correct, so Phase 2 is an
  optimisation, not a rescue.
