# mmap vs read — sequential scan

Measured by `crates/name-core/examples/mmap_vs_read` (deterministic 256 MiB
corpus, checksums proven equal before timing, 1 warmup, min-of-N=15).
Closes the Linux question from rgx's own numbers: rgx measured
mmap 1.51× SLOWER than `read()` for scans on Windows and kept its mmap
path alive to check whether Linux's cheap soft faults reverse the ordering.
They do — and the resident-scan rows show where the difference lives.

## Linux (WSL2, x86_64, NixOS userspace)

| path | min ms | median ms | MiB/s (min) |
|---|---|---|---|
| read() + scan (I/O included) | 335.41 | 341.34 | 763 |
| read() scan only (bytes resident) | 214.34 | 215.74 | 1194 |
| mmap scan (mapping live) | 211.82 | 214.78 | 1209 |
| read() cold first-touch | 454.59 | — | 563 |
| mmap cold first-touch | 327.19 | — | 782 |

cold verdict: mmap is **0.72×** the read() cost (faster).
hot verdict: mmap scan is **0.63×** the read()-included cost.

Cold rows ran as root (`sudo`) with real `drop_caches`; a non-root run
prints "unavailable" rather than faking cold with a warm first round.

## Windows 11 (x86_64, msvc)

| path | min ms | median ms | MiB/s (min) |
|---|---|---|---|
| read() + scan (I/O included) | 258.21 | 263.74 | 991 |
| read() scan only (bytes resident) | 208.98 | 210.32 | 1225 |
| mmap scan (mapping live) | 208.98 | 214.07 | 1225 |

hot verdict: mmap scan is **0.81×** the read()-included cost.
Cold rows unavailable (dropping the standby list needs a kernel driver;
user-mode cache-empty tricks are not honest "cold").

## What the numbers say

- **The scan itself costs the same either way** once the bytes are
  resident: 210 ms for both paths on both OSes. mmap does not make the
  scan loop faster or slower.
- **The delta is the I/O path**: `read()` pays a syscall + a copy into
  fresh pages (~45–135 ms here). mmap pays page faults instead — cheap
  soft faults on Linux (cold 327 ms vs 455 ms), and no copy ever.
- **rgx's Windows "1.51× slower" does not reproduce in the resident
  shape.** Its harness scanned a 246 MB file where first-touch fault
  costs sat inside the timed window; here the warmup keeps those out.
  Both stories can hold: on Windows the *first* pass over a fresh map
  may fault expensively, while a live mapping scans at memcpy speed.
  Measuring Windows first-touch cold needs cache drop we cannot do
  without a driver, so that row stays honestly unmeasured.
- **Ordering for rgy's access-shape table is unchanged and now measured
  on both OSes**: name index and content segments (random access) stay
  mmap-first with owned fallback; TV packs stay `ParallelBuffered`
  (rgx's own 44% win); verify stays plain reads. No promotion threshold.

## Reproduce

```sh
cargo run --release -p rgy-name-core --example mmap_vs_read -- 256
# cold rows on Linux:
sudo target/release/examples/mmap_vs_read 256
```
