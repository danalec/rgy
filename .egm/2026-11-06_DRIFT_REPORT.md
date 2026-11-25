# EGM drift report — 2026-11-06 (second, complete pass)

Direction: doc-to-code. This is the definitive pass of the day and
**supersedes the first one**, which was a targeted reconciliation after
`3a0ddc2` / `7348e9a` and honestly recorded its own coverage gap: the
interiors of contracts describing untouched modules had not been
re-derived from their sources. This pass closes that gap. It also carries
the **0.1.10 pairing** with the upstream rgx (§9): rgy's crates move to
0.1.10, the EGM checker adopts rgx's post-`5d4c301` hardening, and the
vendored drift is recorded. No finding is left unresolved, and
`DRIFT_STATUS.md` names this report and its `decision`.

## 1. Scope and inputs

- 51 `EGM.md` contracts, all tracked, all in trees this repository owns.
- The 44 contracts that sat inside the vendored upstream trees were
  deleted as ignored scratch. `.gitignore` still ignores them by scoped
  rule and `.egmrc`'s `scope=exclude:crates/rgx-*,crates/rar-codec`
  keeps them out of both the contract set and the derived
  expected-directory set, so a future global bootstrap cannot bring
  them back into the gate.
- The stale `.dev/docs/2026-11-06_DRIFT_REPORT.md` and its empty `.dev/`
  tree were removed. The audit trail now lives only here, tracked.
- `crates/name-core/LICENSE-MIT-FSEARCH`, a tracked per-crate copy of the
  MIT notice that survived the licence cleanup in `3a0ddc2`, was deleted.
  `COPYING` and `NOTICE` say the notice was removed; that is now true of
  every path, not just the root one.

## 2. Method

1. **Four fresh-context Goldfish reviewers**, read-only, covering the 51
   contracts in disjoint batches of 13 (root, `Formula`, `bins`), 18
   (`crates` blueprint, adapters, autostart, the five discovery backends),
   11 (engine, the two launchers, `name-core`, examples and tools) and 9
   (`par`, `pick`, `proto`, `scripts`, `tools`). Each verified manifest
   fields, quoted descriptions, feature lists, symbol citations,
   `[file.rs:symbol]` anchors, markdown links against a fresh-clone
   input, and the `Run:` line against `.egmrc` and CI.
2. **The lead's independent audit**: all 2,342 `[file:symbol]` citations
   resolved to a file that exists (the ten apparent misses were roll-ups
   into `examples/` and `tools/`); every claimed file count compared with
   the tree; every platform-FFI claim checked against the five backend
   `src/` trees; a repo-wide grep for `fsearch`, `FSIDX014`,
   `Apache-2.0 AND MIT` and `LICENSE-MIT-FSEARCH` in tracked contracts.
3. **Machine checks**: `scripts/check-egm-contracts.py` (full check,
   `--generated-check`, `--self-test`), `cargo fmt`, `cargo clippy`,
   `cargo build`, `cargo test --workspace`, `bench-names.py check`,
   `check-twins.sh`, `cargo package --list` via
   `tests/egm_packaging.rs`, and the bare-metal build.

## 3. Phase 1 recap (commit `2c655ea`)

The first pass found nine things, all resolved: 13 stale fsearch/MIT
lines in 8 contracts; `FSIDX014` → `FSIDX015`; 51 `Run:` lines brought
to `cargo test --workspace --no-fail-fast --locked`; the contracts moved
from gitignored to tracked; drift moved from an ignored free-form report
to `DRIFT_STATUS.md`; the pair table moved from gitignored `AGENTS.md` to
tracked `.egm/PAIRS.md`; the build-first guard in `tests/harness.rs`
taught to skip `EGM.md`; published crates taught to exclude them; and
the twelve links to out-of-scope vendored contracts replaced by a
paragraph explaining why there are none. The self-test had 37 cases and
the checker reported `51/0`.

## 4. Phase 2 (this pass)

`--generate` now writes a deterministic structural skeleton for every
directory that owes a contract into `.egm/generated/` (a hidden,
gitignored scratch directory). `generated_problems()` derives each
contract's required child links and `Run:` line from the tree and is
folded into the same single `validate()` — there is still exactly one
definition of a valid contract, not a second checker that can disagree
with the first. `--generated-check` exposes it explicitly, and CI runs
that flag on Windows, Linux and macOS alongside the full check and
`--self-test`. The self-test grew to 41 cases.

Two cross-platform details were fixed with it: generated artifacts are
written with `write_bytes` (Windows `write_text` would translate LF to
CRLF), and `.gitattributes` now pins `.egm/CONTRACTS.index` to
`text eol=lf`, so a fresh `--reindex` produces the same bytes on both
hosts.

## 5. Findings of this pass, all resolved

1. **`content.rs` no longer declares platform ABI** (reviewer C and the
   lead, independently). `crates/name-core/EGM.md:48` and
   `crates/name-core/src/EGM.md:77` still cited
   `[content.rs:unsafe extern "C"]`. The rewritten module is std-gated,
   reads files through `std::fs` and rgx-core, and declares no ABI; the
   hand-declared extern blocks live in `clock.rs` and `scan_priority.rs`.
   Both contracts were corrected, and the same stale claim in
   `crates/name-core/Cargo.toml` and `scan_priority.rs`'s module doc was
   fixed in the code.
2. **`content.rs` keeps no on-disk index** (reviewer C, borderline note;
   promoted to a finding). `crates/name-core/src/EGM.md:47` called
   `[content.rs:Segment]` / `[content.rs:MAX_FILE]` "the content index
   (std)". They are the inert shell left by the removed persisted
   trigram index; the contract now says so.
3. **The root contract still said contracts were gitignored** (reviewer
   A). `EGM.md:20` cited a `[.gitignore:EGM.md]` rule that no longer
   exists. It now states that the 51 owned contracts are tracked and
   that only the vendored copies are ignored.
4. **`LISTED` is not the listing id counter** (reviewer B).
   `crates/discovery-macos/src/EGM.md:32` described
   `[walk.rs:LISTED]` as the shared listing id counter; it counts the
   entries listed so far, while `Ctx::next_id` is the counter. Corrected.
5. **`scripts/EGM.md` counted 20 source files** (lead). The EGM checker
   twins added one `.sh` and one `.py`; the tree has 15 `.sh` + 7 `.py`.
   The contract now says 22, names the new checker in its Overview and
   Interface, and cites `validate`, `generated_problems` and `self_test`.
6. **A tracked MIT notice survived the licence cleanup** (lead). Deleted
   `crates/name-core/LICENSE-MIT-FSEARCH`; added
   `no_third_party_notice_file_is_tracked` to `tests/license.rs` so a
   stray copy cannot come back. The guard is staging-independent: it
   fails only when the bytes are actually present, because `git ls-files`
   still names a file deleted on disk but not yet staged.
7. **`CONTRACTS.index` line endings** (lead). See §4.

Reviewers A, B, C and D reported 1, 1, 2 and 0 findings respectively;
the lead's audit found 3 more. All are resolved. `CONTRACTS.index` was
regenerated in the same pass, so the trail and the decision cannot
disagree.

## 6. Coverage honestly stated

All 51 contracts received a fresh-context semantic review in this pass,
and all 51 passed the machine-derived structure check. That is the gap
the first report recorded, closed. What no generator or reviewer can
guarantee is the future: semantic prose is still hand-maintained, so a
later code change needs a later drift pass. `CONTRACTS.index` (hash per
contract), `--generated-check` (structure derived from the tree) and the
CI matrix are what make a later change fail loudly instead of quietly
accumulating.

## 7. Evidence

- `check-egm-contracts.py`: `checked=51 failed=0`;
  `--generated-check`: `checked=51 failed=0`;
  `--self-test`: `cases=55 failed=0` (41 at the first pass, 55 after the
  0.1.10 pairing).
- `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets
  -- -D warnings`, `cargo build --workspace` and
  `cargo test --workspace --no-fail-fast` (82 targets, 0 failed) on
  Windows; the same suite, `bench-names.py check --runs 7` and
  `check-twins.sh` (13 agreeing pairs) on the CachyOS box; the
  bare-metal `x86_64-unknown-none` build on Windows.
- `tests/egm_packaging.rs` asks `cargo package --list` and finds no
  `EGM.md`; `tests/license.rs` now has 5 tests including the notice
  guard.
- The Linux mirror is an exact clone of the final commit, produced by an
  offline `git bundle` (Forgejo asks for credentials from that box), so
  its inputs are byte-identical to a fresh CI checkout. Earlier patch
  based mirrors lacked files added in intervening commits and produced
  16 false "missing script" failures that were never code defects.

## 8. Structure

`.egm/EGM_STRUCTURE_PROPOSAL.md` is the structural copy of rgx's
`5d4c301` proposal and contains the English reply to its four
questions. `.egm/DRIFT_STATUS.md` is the machine-readable decision;
`.egm/PAIRS.md` the tracked pair table; `.egm/CONTRACTS.index` the
generated hashes. Phase 1 and Phase 2 are both implemented; there is no
Phase 3 outstanding.

## 9. Reply to the rgx 0.1.10 handoff

Reply in English, as asked by
`../rgx/.egm/RGY_HANDOFF_0.1.10.md` (2026-11-06). The handoff reports
rgx `f327bf8` / tag `rgx-v0.1.10`, a source-only release whose binary
assets stay at `rgx-v0.1.9.0`, and asks rgy to re-check its vendored copy,
read sections 5–6 of rgx's proposal, decide which of seven items to adopt,
run a drift pass with a `decision`, and keep the gate green.

### 9.1 The seven items, and what rgy adopted

| # | Item | rgy decision |
|---|---|---|
| 1 | `CONTRACTS.index` lockfile, CRLF→LF, `.gitattributes` LF pin | **Adopted.** Already implemented in the first pass of this date; Windows and Linux produce byte-identical bytes (SHA-256 verified on both hosts). |
| 2 | `--reindex` / `--fix` (`-Reindex` / `-Fix`) | **Adopted.** `--reindex` already existed; `--fix` and `-Fix` are added in this pairing pass. CI never uses it, and no other mode writes a file. |
| 3 | One identical check everywhere; 43-case self-test | **Adopted, with a higher count: 55/55.** rgy's self-test carries the common cases plus its Phase 2 generation cases and the newly ported rules (`--fix` recovery, duplicate rows, path escapes, `decision`, date bound, byte stability). One checker, one `validate()`. |
| 4 | `.egmrc` enforced with four keys | **Adopted, with two adaptations.** rgy keeps a fifth key, `scope=exclude:crates/rgx-*,crates/rar-codec`, and a different `verification` (`cargo test --workspace --no-fail-fast --locked`, not `--release`). `source_extensions` is asserted equal to rgy's own `SOURCE_SUFFIXES`. |
| 5 | `DRIFT_STATUS.md` with `date`, `report`, `unresolved`, `decision` | **Adopted.** `decision` names this tracked report. A clean status left `pending` is refused. |
| 6 | Path safety for `report:` and index rows | **Adopted.** Paths resolve inside the repository; reports additionally inside `.egm/`. |
| 7 | Version bump via `scripts/bump-version.py` | **Adopted.** Crates, requirement lines and `Cargo.lock` move to `0.1.10` together; the 12 vendored manifest patches are re-derived. Installers stay at `rgy-v0.1.9.0`, the last tag with binary assets — the same policy rgx states. |

### 9.2 Corrections to the handoff's assumptions

- **rgy tracks 51 contracts, not 95.** 95 was the initial bootstrap's
  generated set before the 44 contracts inside the vendored upstream trees
  were excluded by `scope` and deleted as ignored scratch. The handoff's
  "your contract set is 95 directories" describes that earlier state.
- rgy's `.egmrc` has the `scope` key and the verification line above; rgx's
  four-key config is neither sufficient nor correct here.
- **rgy has a Phase 2 that rgx does not**: `--generate` writes deterministic
  structural skeletons and `--generated-check` derives each contract's
  required child links and `Run:` line from the tree. It does **not**
  regenerate prose. This is compatible with the "derived facts generated /
  authored decisions human" split in rgx §5: the derived half is the shape,
  the authored half is the meaning, and the meaning is still reviewed by
  dated drift passes. rgy keeps it because its own history is the evidence
  the split answers — one rewrite left 95 stale contracts behind.

### 9.3 Vendored drift, recorded (handoff item 1)

`scripts/check-drift.ps1 -Upstream ../rgx` against `f327bf8`: **12 of 12
vendored trees have moved since pin `8c32dc09`, 49/49 rows checked, 0
unpinned.** The changed upstream paths in that range are the vendored
`Cargo.toml` files (version 0.1.9 → 0.1.10) and newly tracked `EGM.md`
contracts. **No `.rs` source changed.** `scripts/check-vendoring.ps1`
passes (12 vendored trees, 37 patches, pinned to `8c32dc09`); after the
bump, `scripts/derive-vendored-patches.py --upstream ../rgx` re-derived the
12 manifest patches and its `--check` is clean.

**Decision: do not re-pin in this commit.** A sync to `f327bf8` would copy
61 `EGM.md` files back into the vendored trees — the ignored scratch this
repository deliberately removed — and would force a patch re-derivation
against a new pin for zero source-code delta. The version difference is
carried by rgy's own patches, which is exactly what those patches are for.
`check-drift` is a local informational report here, not a CI gate (CI runs
`check-vendoring`, which is green). Re-pin when upstream next changes
vendored *code*.

### 9.4 The four questions, from rgy's measured state

1. **Status manifest or generated index?** Both, split by kind: the status
   carries the authored decision, the index carries the derived hashes. A
   checksum cannot say `unresolved: 0`; a manifest cannot detect a silent
   contract edit. Adopted from rgx §5.
2. **Generated or hand-maintained contracts?** Generate **structure**, not
   prose. rgy's `--generate` is a skeleton writer and `--generated-check` a
   structural verifier; semantic meaning stays authored. This is the one
   deliberate divergence from rgx §5, for the reason in §9.2.
3. **Remaining failure modes?** Monorepo growth, renames and partial
   checkouts are closed by the derived expected-directory set plus the
   index. An `unresolved` cleared without a decision is now closed by the
   required `decision` field, the same-commit dated report, the 30-day date
   bound and the newest-report rule. Manifest/report disagreement is closed
   by the date↔filename agreement and the report-inside-`.egm/` check.
4. **One check everywhere, plus byte-identical regeneration?** Yes to the
   first: one checker, one `validate()`, identical full check and self-test
   on all three operating systems. No to regenerating prose byte-for-byte —
   infeasible and undesirable, as rgx §5 concludes. rgy derives structural
   facts instead and runs `--generated-check` on all three OSes.

### 9.5 Trade-offs and reasoning

- Keeping `scope` costs one extra config key and a divergence from rgx's
  four-key `.egmrc`; it buys correctness, because 44 vendored contracts
  must stay out of the gate.
- Keeping the generator costs a determinism obligation (`write_bytes` LF on
  every host, `.gitattributes` LF pin, byte-stability self-test); it buys
  shape-drift detection.
- Deferring the re-pin costs a standing `check-drift` report; it buys no
  churn in the vendored trees and no invented code delta.

### 9.6 Verification

The pairing pass is committed together with this report and the updated
`DRIFT_STATUS.md` (including `decision`).

Windows, on the paired tree: `cargo fmt --all -- --check` 0;
`cargo clippy --workspace --all-targets -- -D warnings` 0;
`cargo build --workspace` 0; `cargo test --workspace --no-fail-fast`
**82 targets, 0 failed** (the new `winget_manifest` target included);
checker `51/0`, `--generated-check` `51/0`, `--self-test` `55/55`,
`--fix` `51/0`; `--reindex` twice is byte-identical and LF-only;
`scripts/check-vendoring.ps1` passes (12 trees, 37 patches, pinned to
`8c32dc09`); the bare-metal `x86_64-unknown-none` build and
`bench-names.py check --runs 7` pass.

The same suite, `bash scripts/check-twins.sh` (13 agreeing pairs) and the
`.sh` checker twins are re-run on the CachyOS box after the mirror is
reset to the commit by an offline `git bundle`. The Windows and Linux
`CONTRACTS.index` SHA-256 values are compared byte-for-byte before the
commit; both are `c83e205cb3548843fe33472980edfd34ee193a10e17bd4b5ff27c925ebdd002c`.

## 10. Closing the remaining UI review findings

The UI review (§5's sibling pass, and the three fresh-context reviewers
whose findings are listed below) produced 24 findings. Ten were fixed in
`661a2ff`; this pass closes the other fourteen. Two contracts were edited
to match the new behaviour — `bins/rgxy/src/EGM.md` and
`bins/search-ui/src/EGM.md` — and `CONTRACTS.index` was regenerated in the
same commit that carries this report, so the derived half and the reviewed
half travel together.

Closed here, with regression tests:

- **Terminal input** (`bins/search-ui`): a truncated multibyte run at the
  end of a read is dropped instead of blocking `read()` forever; both
  platform read loops wait on a bounded tick and emit `Event::Resize` so a
  resize while idle repaints; Windows sets `ENABLE_WINDOW_INPUT` and the
  previously dead `WINDOW_BUFFER_SIZE_EVENT` branch is live; bracketed
  paste is enabled and a `\r` inside a burst is a character, not Enter;
  the tautological attribute oracle test now goes through `Styled`'s
  `Display` (including reverse and the combined bold+dim+reverse span).
- **Picker** (`crates/pick`): the PATH scan checks the execute bit on
  unix, tries `.exe`/`.bat`/`.cmd` before a bare name on Windows, and keeps
  the path as `PathBuf` so a non-UTF-8 PATH entry is not mangled; a real
  error reading the picker's stdout is an error, not "no matches".
- **Resident launcher** (`bins/rgxy`): stat, reader and preview threads go
  through bounded worker pools instead of one detached thread per query;
  daemon-state news goes through ONE long-lived stderr writer behind a
  bounded channel; a worker thread that cannot start degrades to a dead
  daemon instead of panicking.
- **Launcher model and window** (`crates/launcher*`): the preview decision
  is separated from `IconKind` behind an explicit text-extension allowlist
  (a `.ps1` is previewable, a PDF is not); every struct the platform
  writes into has a compile-time size/offset assertion; the actions column
  no longer squeezes the settings panel; the write-only `painted` field was
  removed and the picture size now drives a real release-build decision.
- **Working notes**: the stale claim that the NFC/NFD fold "does not exist
  yet" was corrected against `crates/name-core/src/fold.rs`.

One residual is recorded rather than hidden: conhost (the legacy Windows
console) delivers key records, not bracketed-paste byte sequences, so a
`\r` in a conhost paste still arrives as `VK_RETURN` -> Enter. The byte
decoder path is fully fixed and the enable/disable escapes are written on
all platforms; closing the conhost gap needs a VT byte parser in the
Windows read loop, a separate change with its own regression surface. The
reason is written into `bins/search-ui/src/EGM.md` so the next reader does
not have to rediscover it.

Verification of this pass: Windows `cargo fmt --all -- --check`,
`cargo clippy --workspace --all-targets -- -D warnings`,
`cargo build --workspace`, `cargo test --workspace --no-fail-fast`
(82 targets, 0 failed), checker `51/0` in all modes, self-test `55/55`,
the bare-metal build and the perf gate; the CachyOS box re-runs the same
suite under rustfmt 1.9.0 plus `cargo check`/clippy for the FreeBSD and
both macOS targets, `check-twins.sh` and `check-vendoring.sh`.
