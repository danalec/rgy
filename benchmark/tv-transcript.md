# rgx-tv A/B/C transcript

- date: 2026-10-10 23:17:39
- store: C:\Users\danalec\AppData\Local\Temp\rgx-tv-store-tree (26 packs)
- root: C:\Users\danalec\.rgx-bench\tree
- rounds: 7 (min reported), threads: 32 (all arms)
- rules: count gate first, arms paired inside each round, order rotated, floor +-3%

## Per-pattern

### `e`  (counts: 11764155 each, GATE PASS)

| arm | min ms | vs rg | verdict |
|---|---:|---:|---|
| rg | 239.3 ms | - | reference |
| rgx | 133.4 ms | 1.79x | faster +79.3% |
| tv | 38 ms | 6.3x | faster +530.3% |

### `pub fn`  (counts: 1492934 each, GATE PASS)

| arm | min ms | vs rg | verdict |
|---|---:|---:|---|
| rg | 225.4 ms | - | reference |
| rgx | 124.9 ms | 1.8x | faster +80.5% |
| tv | 36.8 ms | 6.13x | faster +513.2% |

### `fn main`  (counts: 297851 each, GATE PASS)

| arm | min ms | vs rg | verdict |
|---|---:|---:|---|
| rg | 212 ms | - | reference |
| rgx | 108.5 ms | 1.96x | faster +95.5% |
| tv | 32.4 ms | 6.55x | faster +554.8% |

### `let mut`  (counts: 67616 each, GATE PASS)

| arm | min ms | vs rg | verdict |
|---|---:|---:|---|
| rg | 196.6 ms | - | reference |
| rgx | 91.7 ms | 2.14x | faster +114.4% |
| tv | 30.4 ms | 6.47x | faster +546.8% |

### `a|b|c`  (counts: 10201075 each, GATE PASS)

| arm | min ms | vs rg | verdict |
|---|---:|---:|---|
| rg | 232.3 ms | - | reference |
| rgx | 130.9 ms | 1.78x | faster +77.5% |
| tv | 37.4 ms | 6.21x | faster +520.6% |

## Verdict rules

A ratio inside +-3% is a tie. Overlapping samples are not a result. The count gate
withholds every timing line where the arms disagree on how many lines matched.

