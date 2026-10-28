# The docs of rgy

One document per subject, and this is the map. The two a person uses come
first; the rest are for whoever works on the code. Nothing here is a plan
that was never followed: each file answers a question a reader has now.

## Use it

| file | what it is |
|---|---|
| [../README.md](../README.md) | what this is, and where everything else lives |
| [../MANUAL.md](../MANUAL.md) | how to use it: the query language, the flags, the daemon, the pack store |
| [../FORMATS.md](../FORMATS.md) | what it reads inside a file, format by format |

## Work on it

| file | what it is |
|---|---|
| [design.md](design.md) | the decisions the code is built on, and the rules that keep it honest |
| [rgy-name-core.md](rgy-name-core.md) | the name index and the query engine |
| [on-disk-formats.md](on-disk-formats.md) | the on-disk formats: the index blob, the packs, the content segments |
| [insights-from-rgx.md](insights-from-rgx.md) | what the rgx side taught about storage and access shapes |

## Check it

| file | what it is |
|---|---|
| [parity.md](parity.md) | both matrices in one file: the 53-feature per-OS matrix, and what Listary does that rgy does not — written by hand, and checked by `scripts/parity.py` |

**The parity document is written by hand**, and the gate is what keeps it
honest:

```sh
python scripts/parity.py --check
```

A `G` cell in the parity matrix is a gap — work required — and it fails CI
until it is ported or documented as degraded. A Listary gap with no work
attached is a gap nobody has scheduled. A row that claims parity without
the verification behind it is worse than a `G`.

## The measurements

| file | what it is |
|---|---|
| [../benchmark/](../benchmark/) | the measured tables and the write-ups behind them |
| [../benchmark/baseline-names.md](../benchmark/baseline-names.md) | the name-search baseline and the perf gate that fails a PR on a regression |

The numbers are quoted with the host and the date they were taken on,
because a number without its host and its date is a rumour. The gate
measures at a fixed worker width, compares against the rows of the host it
runs on, and refuses — exit 3 — to compare two machines.
