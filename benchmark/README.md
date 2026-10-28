# rgy: the measurements

What was measured, on which host, and what the number does not claim. Every
table here carries the machine it was taken on and the date it was taken
on; a number without those two is a rumour.

| file | what it is |
|---|---|
| [baseline-names.md](baseline-names.md) | the name-search baseline and the perf gate: the table, the host, the spread beside every number, and the A/B write-ups |
| [baseline-names.csv](baseline-names.csv) | the source of truth for the table above — the Markdown is generated from it, and the gate compares against it |
| [mmap-vs-read.md](mmap-vs-read.md) | mmap against `read()` for a sequential scan, and the cold and hot verdicts |
| [rgy-name-transcript.md](rgy-name-transcript.md) | the name-tier protocol transcript, written by `scripts/rgy-name-protocol.*` |

## The gate

A PR that regresses any gated row by more than 20% fails:

```sh
python scripts/bench-names.py check --baseline benchmark/baseline-names.csv --runs 7
```

Exit codes: 0 pass, 1 regression, 2 undecidable (the noise floor is wider
than the threshold), 3 not comparable (a different worker width, a row that
is not measured at all, or a host the baseline has no rows for).

The measurement is several independent runs at a fixed worker width, the
statistic is the best of them, and the spread across the runs is recorded
next to the number — because a gate that fires on noise is worse than no
gate. The table is generated from the CSV, and `bench-names.py render --check`
fails when the two have drifted:

```sh
python scripts/bench-names.py render --csv benchmark/baseline-names.csv --check
```
