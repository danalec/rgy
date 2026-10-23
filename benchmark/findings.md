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

## 6. The engine-alone edge is not portable across hosts

**Question.** The Windows dashboard reports rgx ahead of ripgrep on all nine
single-file pattern shapes by +33% to +114%. Is that a property of the code?

**Evidence.** Same corpus (byte-identical `big235.rs`, sha256
`9c38d37f…`), same gated protocol (7 rounds, 2 warmups, ±3% floor, count
gate, `-j1`), same release build flags, on a second host: AMD Ryzen 7 255
w/ Radeon 780M, CachyOS, `ripgrep 15.2.0`. Results
([`status-linux-ryzen7-255.md`](https://github.com/danalec/rgx/blob/main/benchmark/status-linux-ryzen7-255.md) §1.1):

| shape class | Windows 7950X3D | Linux Ryzen 7 255 |
|---|---|---|
| single-byte (`e`, `a\|b\|c`) | rgx +86% / +114% | rgx +40% / +20% |
| `e.` | rgx +40% | tie |
| multi-byte literal (`pub fn`, `fn main`, `let mut`) | rgx +33% to +59% | **ripgrep −37% to −44%** |
| class / dot-star (`e[0-9]`, `foo.*bar`) | rgx +43% to +48% | **ripgrep −34% to −41%** |
| alternation-with-class (`(pub\|fn) [a-z]+`) | rgx +39% | **ripgrep −15%** |

All 18 rows pass their count gates and all 18 separate. The split is by
pattern shape, not by noise: ripgrep's packed-pair literal kernel is strong
when the needle offers two rare bytes, and rgx's in-house engine is strong
when the scan is dominated by a high-frequency byte whose candidates the
mask rectifies in-register.

**Conclusion.** The sign of the engine-alone edge is a property of the host
and the pattern shape together. What travels is the shape dependence. Any
claim of engine superiority must name the host; the Windows table's
headline is true of that machine and false of this one. The scale-separation
claim the paper actually needs — that on a many-small-file tree the engine
share is small — holds on both hosts and does not depend on which matcher
is ahead on a single file.

## 7. `ParallelBuffered` over mmap: direction two hosts, magnitude one

**Question.** [`optimal-techniques.md`](optimal-techniques.md) claims −44.3%
mean wall-clock for `ParallelBuffered` over memory mapping. Does it travel?

**Evidence.** Same three read modes, paired and order-rotated, 7 rounds,
`fn main`, `-j10`, on the digest-named generated tree (26 packs): mmap is
slower than `ParallelBuffered` on **every** paired round, by 3–18% (mean
≈12%). `serial-buffered` is ~8× slower than either. Full table:
[`status-linux-ryzen7-255.md`](https://github.com/danalec/rgx/blob/main/benchmark/status-linux-ryzen7-255.md) §3.

**Conclusion.** The direction reproduces on two hosts, two corpora and two
OSes; the magnitude does not. −44.3% is contradicted in magnitude by a
second host and stays withdrawn as a quotable figure. Quote ≈12% with the
host's name, or quote the direction alone.

## 8. The 512-bit n-gram "inversion" was a column mislabel

**Question.** The paper's n-gram table had 512-bit admitting more files
than 256-bit, contradicting the Bloom model. Was the filter wrong, the
hash weak, or the measurement unreplicable?

**Evidence.** The harness prints `Admitted Files | Pruned Files | Prune
Efficiency`. The paper's table copied the **pruned** count and the
**prune efficiency** percent into a column labelled "admitted". The `fn
main` 256-bit row is the proof: paper "admitted 2,275 (15.17%)" against a
second-host re-run's **pruned 2,274 (15.16%)** — one count apart. Under
the correct labels the paper's own numbers already say 512-bit prunes more
(27.78% vs 15.17%).

**Conclusion.** Closed as a reporting error. There was never an inversion;
both runs show textbook ordering. The error manufactured a phenomenon and
invited a mechanism (weakly-avalanched half-selector) to explain a
non-event. Residual, narrower: absolute prune rates differ across hosts on
3 of 4 patterns because the harness samples 15,000 files in `readdir`
order (unstable); latency differs 4–6× in the opposite direction to host
speed (timer scope, not filter). Fix for the residual: digest-name the
n-gram subset the way `scripts/make-corpus.py` names the dashboard
corpora.

---

## 9. `--json` allocated six strings per matched record

**Question.** The dense shape -- few files, many matches each -- prices the
machine-readable path at roughly three times the text path for the same
400,000 matched lines. The text printer batches its sends and allocates
nothing per line; where does `--json` spend the difference?

**Evidence.** `scripts/bench-json-print.py dense`, count-gated at 400,000 in
all three modes, paired rotated rounds, the process floor measured and
subtracted (12.65 ms here): text **65.3 ms**, `--json` **189.3 ms**, `-c`
**8.8 ms**, every sample separated. Per record the printer allocated about
six strings -- the path and the adapter tag (identical on every record of
one file), the escaped line text, a second one appending the terminator,
each submatch text, and the `format!` temporary the submatch record was
built in -- and the escape walked one character at a time.

**Conclusion.** Hoisting the two per-file escapes out of the line loop,
reusing two scratch buffers for the line text and the `submatches` body,
copying plain runs of the escaped text in one pass, and formatting the four
integers of a record without `fmt` takes the paired A/B against the
pre-change binary to **-43.4%** (346.6 to 196.1 ms minus the floor, every
sample below every sample), with byte-identical output over 12 invocations
-- unterminated last line, CRLF, control characters, quotes, a binary file
with a NUL, and `-n`/`-A`/`--no-filename`/`-i`/`-o`/`-w`/`-F`/`--sort`.
The wide shape is where the lesson lives: 20,000 files with one match each
puts `--json` **11%** over text and `text`/`-c` in `overlap, not a result`,
because a per-line cost is invisible in a corpus with one line per file. The
first measurement of this defect ran on that shape and read
`overlap, not a result`.

---

*Status of every item: 1 - fix shipped; 2 - fixed, then re-fixed for memory;
3 - fixed, regression tested; 4 - negative, closed; 5 - one negative and two
sub-floor cuts; 6 - closed as a non-portability finding; 7 - direction
confirmed, magnitude withdrawn; 8 - closed as a reporting error; 9 - fixed,
-43.4% dense and 11% wide.*
