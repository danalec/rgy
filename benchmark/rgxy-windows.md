# rgxy on Windows: session observations

These are observations, not a baseline, and are not part of the perf gate.

**Host:** Windows 11 Pro build 26300, Ryzen 7 7745HX (8C/16T), 93.7 GiB RAM,
local NTFS.
**Dates:** Nov 23–24, 2026 (local; range for the session runs).

## 1. Debug and release queries

Rust 1.100.0, fallback commit `f59bf78`, before startup prewarm. The harness
used the actual rgxy `DaemonSource`, retained one connected source, set limit
50, and alternated the queries `ntuser.dat` (plain) and `*.txt` (wildcard) for
five rounds. The daemon used a separate temporary index over the same tree, so
entry counts differ slightly. OS caches were not flushed.

Release is the shipped profile. These builds do **not** compare startup prewarm
against no prewarm.

### Warm query wall time

Median of five rounds, in µs. Raw samples, in round order, are in
[rgxy-windows-warm.csv](rgxy-windows-warm.csv).

| profile | backend | `ntuser.dat` median (range) | `*.txt` median (range) |
|---|---|---:|---:|
| debug | local | 1,592 (1,377–184,166) | 1,365,432 (1,356,707–1,373,609) |
| debug | daemon | 208,935 (206,516–225,679) | 1,422,694 (1,419,965–1,439,369) |
| release | local | 378 (295–29,651) | 163,428 (160,475–167,023) |
| release | daemon | 5,675 (5,599–20,728) | 117,913 (113,644–119,215) |

The cause of the first plain-query outlier was not established.

### Cold start

The local first cold query excludes the GUI. Daemon startup is the time until
the temporary `searchd` pipe was ready.

| measurement | debug | release |
|---|---:|---:|
| local entries | 1,963,559 | 1,964,037 |
| daemon entries | 1,963,562 | 1,964,090 |
| local first query, wall | 24,379,899 µs | 5,971,112 µs |
| local index build | 23,024,611 µs | 5,802,121 µs |
| local query | 1,354,029 µs | 168,008 µs |
| `searchd` startup, pipe ready | 14,728 ms | 5,153 ms |

Priority suggested by these numbers: optimize the startup wait ahead of
wildcard speed.

## 2. GUI fixture

Windows debug build, before startup prewarm, 20,402 indexed entries. A
**single** visually verified run; no raw samples were retained.

| measurement | result |
|---|---:|
| launch to visible | 68 ms |
| final input to cold result layout | 258 ms |
| warm result layout | 76 ms |
| maximum `WM_NULL` response | 65 ms (zero timeouts) |

"Layout" is the time of result layout, not of the exact paint.

## 3. Installed rgy and Everything: live changes

Installed rgy v0.1.10.3, with a temporary standard-user `searchd` on `C:` and
live notifications: 2,915,254 entries were queryable in 12.08 s. It used
`ReadDirectoryChangesW` (RDCW), not the USN journal, because standard raw volume
open was denied. The Everything service ran as LocalSystem.

Five create/rename/delete fixture rounds were run in Downloads, with no misses.
Times include polling and CLI overhead. No raw samples were retained for
Everything.

| operation | rgy median | Everything median |
|---|---:|---:|
| create | 45.0 ms | 937.0 ms |
| rename | 43.2 ms | 915.0 ms |
| delete | 42.0 ms | 1,083.1 ms |

The two sides used different polling and client paths and different
configurations. This is an observation of these setups, not a measure of
intrinsic engine speed or NTFS speed.

No `rg` throughput comparison was measured in this session.
