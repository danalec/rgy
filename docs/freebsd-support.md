# FreeBSD support

Scope: `rgy` (the workspace) on `x86_64-unknown-freebsd` and
`aarch64-unknown-freebsd`, on the **newest stable release** — 15.1-RELEASE as
of 2026-10, retargeted on every FreeBSD release (15.2 is scheduled for
2026-12). `O_SEARCH` (13+) and the current `dirent` layouts are fair game;
there is no 12.x floor. Everything below is grounded in what the code does
today, measured rather than assumed; the numbers and file references are the
evidence. The decisions that close this plan are the specs in §5.

---

## 1. What runs today

**The walk is dispatched per platform.** `discovery-dispatch::source_for`
picks the native enumerator by `cfg` — the `getdirentries` reader here
(P1.2), `getdents64` on Linux, `getattrlistbulk` on macOS, the MFT walk
when the root is a volume root on Windows — and the portable
`discovery::walk::scan` answers everywhere else and whenever a native
backend refuses. The selector's differential test holds each backend
against `scan_sequential` and asserts which backend answered before
comparing; on this target that is
`freebsd_reader_matches_sequential_oracle` (phase 4 runs it).

**mmap is native here.** `rgx-core/src/memmap2/platform/freebsd.rs` is the
`macos.rs` shape — `mmap(2)` with `MAP_SHARED` / `MAP_PRIVATE` / `MAP_ANON`
— and `page_size()` answers `sysconf(_SC_PAGESIZE)`. The fallback path
keeps the cases it already handled (empty or unmappable files), and
`blob_from_file_maps_a_non_empty_file` asserts the `Blob::Mapped` variant,
so a regression to the owned fallback is a test failure rather than silent
RSS. Demand paging — RSS following the pages touched — is the reason the
mapping is first and the owned buffer is the fallback, never the reverse.

**`open_flags` covers FreeBSD.** `name-core/src/content.rs` (`mod open_flags`)
carries the FreeBSD arm (`O_NONBLOCK 0x4`, `O_NOFOLLOW 0x100`, read off
`sys/sys/fcntl.h`); the zero arm holds only OpenBSD, NetBSD and DragonFly,
on purpose, until their values are checked off a real kernel. The FIFO
acceptance test behind it is `cfg`-scoped to FreeBSD and runs in the phase-4
suite.

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

**P0.1 `open_flags` on FreeBSD.** ✅ landed (`8fd0c30`).
The risk: a FIFO anywhere under the scan root can block `open()` indefinitely
during content search, and a symlink's target is opened rather than refused.
The `freebsd` arm carries `O_NONBLOCK = 0x0004`, `O_NOFOLLOW = 0x0100` — the
Darwin constants — scoped to FreeBSD, so OpenBSD, NetBSD and DragonFly stay
in the zero arm until their values are checked off a real kernel.

*Acceptance*: a test creates a FIFO in a fixture tree and asserts
`open_sized` returns without hanging (with `O_NONBLOCK` an open of a FIFO
with no writer fails `ENXIO` instead of blocking). The test is `cfg`-scoped
to FreeBSD and runs in the phase-4 suite (P2.3).

**P0.2 `zen-rs` on FreeBSD.**
`bins/zen-rs` is Windows-only by construction (MFT + USN). Off-Windows every
volume-reading path is `#[cfg(windows)]`; what remains — index writing and
its tests — is platform-independent and compiles (`869481c` gates
`discovery-windows` and its `windows` dependency to the Windows target).

*Decision* (§5, 2): whether `zen-rs` should *run* on FreeBSD is deferred —
it stays out of this port's "done", and a `zen-rs` over the portable walk is
a separate project.

*Acceptance*: `cargo build --workspace` succeeds on the FreeBSD target —
`cargo check --workspace` is green as of 2026-10-13; the link step needs a
FreeBSD ELF linker/sysroot and joins the phase-4 run.

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

*Status*: ✅ landed — `platform/freebsd.rs`, the `page_size()` arm, and
`blob_from_file_maps_a_non_empty_file` (cfg unix; green on Linux). The
mapped-on-FreeBSD proof is part of the phase-4 run.

**P1.2 Directory enumeration.**
A `getdents`-based reader, taking `d_type` from the dirent so kind does not
cost a `stat`. Scope this only after P2.1 (below), because without dispatch
there is nothing to select it.

*Acceptance*: the differential test already used for the parallel walk
(`parallel_walk_matches_sequential_oracle`) runs against the FreeBSD reader
and matches `scan_sequential` output.

*Status*: ✅ landed — `crates/discovery-freebsd` (`getdirentries`, row
offsets from `offset_of!(libc::dirent, …)`, names NUL-scanned inside the
`d_reclen` stride), selected by `source_for`. The differential ships with
it (`freebsd_reader_matches_sequential_oracle`); the run is the phase-4
kernel.

### P2 — process and hygiene

**P2.1 The dispatch question.**
Either wire the seam (a `Source` selector that picks `discovery-windows` /
`-macos` / `-linux` / `-freebsd` / portable by `cfg`) or record in
`discovery/src/lib.rs` that the portable walk is the supported path and the
platform crates are experiments. The current state — crates that build but
nothing selects — is the worst of the two, because it reads as supported.

This gates P1.2. It is cheap: one `pub fn source_for(root, threads)` and a
`cfg` chain.

*Status*: ✅ landed — `crates/discovery-dispatch`: `source_for(root,
threads)` with the per-platform `cfg` chain and the portable fallback,
wired into `searchctl` and `searchd`. Its differential test asserts which
backend answered before comparing against `scan_sequential` (green on
Windows and Linux).

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
FreeBSD before calling this done — `cargo build --workspace` first: the e2e
harness resolves bins by path, so on a clean tree the e2e tests race the
rest of the workspace build and spawn nothing (they fail loudly, at
`tests/harness.rs`). That machinery is portable, but the pipe/UDS tests
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

Phases 0–2 are small and independent; 3 follows the dispatch decision in §5
(wire the seam), now made. 4 is what turns "it compiles" into "it works".

---

## 5. Specs (decided 2026-10-13)

| # | Question | Decision | What it locks in |
|---|---|---|---|
| 1 | FreeBSD version floor | **Always the newest stable** — 15.1-RELEASE as of 2026-10 | `O_SEARCH` (13+) and the current `dirent` layouts are available; the CI/VM image pins the newest release and re-pins on each one (15.2 lands 2026-12) |
| 2 | Does `zen-rs` run on FreeBSD? | **Deferred** — out of this port's "done" until tested | P0.2 stays option (a); a `zen-rs` over the portable walk is a separate project |
| 3 | Is the native backend seam worth wiring? | **Yes — parity with the other OSes** | `source_for(root, threads)` selects windows/linux/macos/freebsd/portable by `cfg`; `discovery-freebsd` (getdents + `d_type`) is in scope, and P1.2 has a home |
| 4 | v1 acceptance bar | **The same bar as the other OSes** — suite green on a real kernel | Phases 0–4 all in scope: mmap (P1.1), dispatch + getdents (P2.1/P1.2), CI (P2.2), real-kernel suite (P2.3). "It compiles" is not v1 |
| 5 | Where verification runs, and what performance means | **Correctness in a VM, performance on bare metal** | The suite may run in a VM (`vmactions/freebsd-vm`, a Gitea runner, or a local one); the latency/RSS bars in `docs/IMPLEMENTATION_PLAN.md` §2 are the FreeBSD bar, and a VM timing is not evidence for them |

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
