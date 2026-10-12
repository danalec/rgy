# rgx: Findings

Measurements and defects that shaped the code. One section each: the
question, the evidence, the conclusion. Working that no longer needs its own
file lives here; the tables live in [`status.md`](status.md).

---

## 1. Where a walk-dominated corpus spends its time

**Question.** The `crates` row prices as a tie against ripgrep and is called
per-walker fixed cost. Where does the time actually go?

**Evidence.** `scripts/walk_patterns.rs` against the same tree, varying one
input at a time: the walk is **85.7%** of a 12.43 ms run; `--no-ignore` takes
it to 3.54 ms, so **7.01 ms is `.gitignore` matching** against 0.88 ms for
read plus engine combined. The mechanism is the **COMPILE of the rule set**,
once per `.gitignore`, at ~58 µs per pattern — not a per-entry match. Holding
patterns at 65 and moving entries from 1 to 1024 moves 4.55 → 7.86 ms; a
per-(entry × pattern) cost would move 16×. It is gated on `.git` presence: a
tree without one has its `.gitignore` compiled and never consulted.

**Conclusion.** The fast-shape path for `.gitignore` existed and was
unreachable for years because the builder demanded *every* file glob have a
`*.ext` or `pre*` shape, and this repository's own ignore file is 17 of 35.
One anchored line cost the whole file its fast path. Fixing it took the
`crates` walk from 10.73 to 6.09 ms. Full working and three unbuilt fixes:
the file's history in `git log -- benchmark/walk-cost-attribution.md`.

## 2. `--sort path` sorted `--files` and not search output

**Question.** Does `--sort path` make output order deterministic?

**Evidence.** `rgx --sort path 'needle' /tmp/so` printed `inner/b.txt` before
`a.txt` on every run — walk order, not path order. Caught by a flaky
equivalence test. The first fix sorted a `Vec<Msg>` at the printer: correct,
but it buffered every matched file, unbounded on a pattern matching a whole
tree.

**Conclusion.** The walker now emits in path order itself
(`Builder::ordered`), `--sortr path` walks backwards, and the printer streams
one message at a time. Peak working set is flat across 16× the matched files
(6432 / 6480 / 6716 KiB at 500 / 2000 / 8000). Engine impact: none — proved
with `status_table --rgx-base`, every row inside ±3.0%.

## 3. `--files` compiled a path as a regex

**Question.** Why does `rgx --files C:\src\p0` fail?

**Evidence.** `error: regex parse error at 9: unicode property \p is not
supported by the byte engine`. The path was passed to the regex compiler.
Any Windows path whose component begins with an escape introducer (`\p`,
`\x`, `\d`, …) failed outright.

**Conclusion.** Found by running the tool, not the suite. Fixed; regression
test added. The general question — every place a user string reaches a parser
— was audited afterwards (`user-string-to-regex`): no second defect in code,
but two wrong CLI claims in `AGENTS.md` §4 were corrected.

## 4. Picking a better pair for the literal prefilter does not help

**Question.** `scan.rs::pair_offsets` tests `needle[0]` and `needle[k]`. Does
testing two *other* offsets prune more candidates?

**Evidence.** Measured against `let mut` (the row the pair is tuned for) and
14 further patterns. Some patterns do have a rarer pair; none of them run
faster. The current pair is already the best of its 21 candidates on `let
mut`; `frequency::rarest_pair` is 22% worse. Candidates are only 12% of that
row — the line walk is the rest.

**Conclusion.** Negative result. Do not re-derive. The same reasoning killed
the rewrite of the pair kernels for throughput: `pair_offsets` measures
**40–46 GB/s** against a 10 GB/s naive byte walk, so the kernel is 7 ms of a
43 ms `search=` and the rest is the line walk.

## 5. Line-walk bookkeeping: three cuts, one negative

**Question.** The pair finding attributed ~57% of `search=` to "the line walk
and its per-fill bookkeeping". Can any of it be removed?

**Evidence.** Three cuts measured with `status_table --rgx-base`, interleaved,
±3% floor: hoisting `fast.plan` out of the per-line confirm is ~0.4%; the
per-file micro-optimisations (double `stat`+`open` above `BUFFER_PATH_MAX`,
three per-worker regex compiles, a `GetConsoleMode` per matched file) all
measure **inside the floor** — every published row came out "overlap, not a
result". Raising `LineBuffer::INITIAL_BUF` from 64 KiB to 256 KiB to cut
`ReadFile` syscalls is **slower**: the two bit maps scale with the fill and
evict the haystack from L2 (~50 ms against ~43 on the absent literal).

**Conclusion.** Redundant work is worth removing; it is not worth a number.
The `MAX_BITMAP_INPUT` verdict ("raising it loses 14%") was later reversed
after the LineBuffer drop fix — see `ROADMAP.md` §2.1. Both the old "raise
it and you lose" and "the fix is a windowed map" are superseded.

---

*Status of every item: 1 — fix shipped; 2 — fixed, then re-fixed for memory;
3 — fixed, regression tested; 4 — negative, closed; 5 — one negative and two
sub-floor cuts.*
