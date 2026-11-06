# EGM drift report — 2026-11-06 (second, complete pass)

Direction: doc-to-code. This is the definitive pass of the day and
**supersedes the first one**, which was a targeted reconciliation after
`3a0ddc2` / `7348e9a` and honestly recorded its own coverage gap: the
interiors of contracts describing untouched modules had not been
re-derived from their sources. This pass closes that gap. No finding is
left unresolved, and `DRIFT_STATUS.md` names this report.

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
  `--self-test`: `cases=41 failed=0`.
- `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets
  -- -D warnings`, `cargo build --workspace` and
  `cargo test --workspace --no-fail-fast` (81 targets, 0 failed) on
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
