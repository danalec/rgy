# Name-tier protocol transcript (rgy name_proto)

Host: RAPHAEL --- AMD Ryzen 9 7950X3D 16-Core Processor --- Microsoft Windows 11 Pro N build 26300 --- 64 GB DDR5-6000.
Tree: rgy ffb34f3e5fe1e1256b998c59b87862991ac20e5c. Date: 2026-10-15 16:11 -03:00. Threads: walk arm scans with 16;
the index arm's scan parallelism is its own runtime default (the walk
arm being wider is the conservative direction: it understates the
saving). Timer: shell wall clock around the whole process, query output
discarded. Estimator: minimum of 7 paired samples per arm, arm
order rotated within each paired round. Count gate: a separate untimed
pass compares both arms' match counts; a disagreeing line is
suppressed, not timed. Warm cache: the untimed pass and the first
round populate it. Floor: measured below, in this session.

## Corpus: 200000 names (200000 files materialized)

Build (walk + index + persist): **85.5 ms** (min of 7). Process floor (parse and exit): 13.88 ms (min of 7).

| query | walk min (ms) | index min (ms) | ratio | verdict | Q* |
|---|---|---|---|---|---|
| `report` | 47.6 | 21.46 | 2.2x | index (separated) | 4 |
| `file` | 46 | 24.19 | 1.9x | index (separated) | 4 |
| `'report_00000.pdf` | 45.1 | 17.51 | 2.6x | index (separated) | 4 |
| `^src` | 45.3 | 21.02 | 2.2x | index (separated) | 4 |
| `dat$` | 45.1 | 23.76 | 1.9x | index (separated) | 5 |
| `mod` | 47.5 | 23.7 | 2x | index (separated) | 4 |
| `rep` | 46.9 | 20.9 | 2.2x | index (separated) | 4 |

Suppressed by the count gate (counts disagree; not timed): `f1 (idx=80547 walk=0)`.

Cumulative cost on the least favorable kept query (largest Q*):

| Q | walk | prepared |
|---|---|---|
| 1 | 45 ms | 109 ms |
| 2 | 90 ms | 133 ms |
| 4 | 180 ms | 181 ms |
| 8 | 361 ms | 276 ms |
| 16 | 722 ms | 466 ms |
| 32 | 1443 ms | 846 ms |

Raw samples (ms, in round order):

```
  report walk: 49.5, 60.2, 49.4, 52.6, 49, 47.6, 48.8
  report index: 22.28, 21.46, 22.6, 21.62, 22.45, 23.18, 23.76
  file walk: 50.1, 46.2, 51, 47.7, 46, 50.9, 50
  file index: 26.32, 24.19, 24.77, 25.33, 27.56, 25.79, 26.78
  'report_00000.pdf walk: 46.5, 47.3, 47.6, 47.6, 45.7, 45.1, 47.1
  'report_00000.pdf index: 18.22, 17.51, 17.92, 18.9, 18.56, 17.98, 18.95
  ^src walk: 47.6, 46.3, 45.3, 50.2, 45.6, 49.4, 48.7
  ^src index: 22.62, 23.77, 23.41, 22.33, 22.18, 21.02, 23.07
  dat$ walk: 46, 45.1, 48.5, 48, 56, 48, 46.3
  dat$ index: 26.64, 23.76, 24.67, 24.11, 27.09, 25.65, 25.27
  mod walk: 48.1, 47.5, 49.8, 52.5, 50.7, 48.9, 51.6
  mod index: 24.59, 23.7, 24.75, 26.45, 24.88, 25.62, 24.1
  rep walk: 58, 46.9, 47, 50.4, 48.1, 47.8, 49.2
  rep index: 20.9, 21.87, 22.19, 23.67, 22.33, 21.53, 22.72
```

## Corpus: 5000000 names (5000000 files materialized)

Build (walk + index + persist): **2221.1 ms** (min of 7). Process floor (parse and exit): 12.01 ms (min of 7).

| query | walk min (ms) | index min (ms) | ratio | verdict | Q* |
|---|---|---|---|---|---|
| `report` | 604.6 | 91.49 | 6.6x | index (separated) | 5 |
| `file` | 586.1 | 134.52 | 4.4x | index (separated) | 5 |
| `'report_00000.pdf` | 570.7 | 69.45 | 8.2x | index (separated) | 5 |
| `^src` | 565.9 | 110.8 | 5.1x | index (separated) | 5 |
| `dat$` | 560.1 | 136.12 | 4.1x | index (separated) | 6 |
| `mod` | 613.1 | 108.57 | 5.6x | index (separated) | 5 |
| `rep` | 614.1 | 91.62 | 6.7x | index (separated) | 5 |

Suppressed by the count gate (counts disagree; not timed): `f1 (idx=1639704 walk=0)`.

Cumulative cost on the least favorable kept query (largest Q*):

| Q | walk | prepared |
|---|---|---|
| 1 | 560 ms | 2357 ms |
| 2 | 1120 ms | 2493 ms |
| 4 | 2240 ms | 2766 ms |
| 8 | 4481 ms | 3310 ms |
| 16 | 8962 ms | 4399 ms |
| 32 | 17923 ms | 6577 ms |

Raw samples (ms, in round order):

```
  report walk: 612.1, 624.5, 604.6, 613.7, 647.6, 615.1, 615.5
  report index: 97.99, 92.84, 93.05, 91.49, 102.9, 96.66, 95.69
  file walk: 587.8, 616.3, 594.5, 593, 586.1, 595.6, 614.3
  file index: 140.55, 137.99, 137.14, 135.83, 135.81, 134.52, 140.94
  'report_00000.pdf walk: 593.2, 576.1, 574.3, 570.7, 570.7, 570.7, 579.8
  'report_00000.pdf index: 69.83, 70.35, 71.46, 71.32, 69.45, 77.79, 74.48
  ^src walk: 601.1, 573, 592.1, 572.6, 565.9, 571, 580.3
  ^src index: 116.92, 116.59, 113.45, 110.91, 116.67, 113.29, 110.8
  dat$ walk: 571.8, 561.9, 563.9, 584.7, 657.4, 573.9, 560.1
  dat$ index: 140.42, 136.12, 143.52, 137.86, 138.1, 139.22, 137.46
  mod walk: 625.4, 622.5, 623.5, 613.1, 636.5, 626.8, 637
  mod index: 113.39, 111.16, 110.09, 108.57, 115.87, 114, 112.56
  rep walk: 623.1, 614.1, 627.3, 634.6, 620.5, 618.5, 619.2
  rep index: 91.98, 91.62, 96.32, 95.4, 95.19, 94.98, 102.34
```

## Notes

- The direct arm answers by enumerating the tree and matching names
  (what `fd` / `dir /s` do); no index is built in that arm.
- The prepared arm pays its index load inside the timed invocation
  (setup is charged, per the protocol).
- Q* is T_build / (walk - index) from the minima, the same ratio as
  the content tier's thm:breakeven.
- The fixture and the index live under `C:\Users\danalec\AppData\Local\Temp`; the corpus is
  the deterministic name_bench generator (`name_proto materialize`).
