# EGM drift report — 2026-11-06

Direction: doc-to-code. The contracts were generated before commits
`3a0ddc2` (the three fsearch sources rewritten, `LICENSE-MIT-FSEARCH`
deleted, the tree made Apache-2.0 end to end) and `7348e9a` (the query
engine's cached-answer fix). This pass reconciles the tracked contract set
with those commits and with the `.egmrc` policy.

## Scope

- 95 `EGM.md` contracts existed. 44 sit inside vendored upstream trees
  (42 under `crates/rgx-*`, 2 under `crates/rar-codec`) and are out of
  scope: AGENTS.md §1 leaves those trees to
  `scripts/sync-rgx-upstream.sh`, and a tracked contract there would be a
  fork the next sync deletes. They stay ignored via scoped `.gitignore`
  rules, and the checker's `scope` excludes them from both the contract
  set and the expected-directory set.
- 51 contracts are in trees this repository owns. These are now tracked.

## Findings and resolutions

1. **Licence and provenance claims (13 lines, 8 contracts).** The
   contracts still said three sources kept fsearch's MIT terms under
   `LICENSE-MIT-FSEARCH`, that `rgy-name-core` was `Apache-2.0 AND MIT`,
   that the macOS backend was "fsearch's `getattrlistbulk` walk" and
   "transported verbatim", and quoted two `Cargo.toml` descriptions that
   named fsearch. All were rewritten to what the code now says:
   independent Apache-2.0 implementations written from Apple's published
   APIs, the notice removed, and the current description strings. The
   remaining mentions of fsearch are explicitly historical ("once
   transported", "were replaced") and are correct as history.
2. **On-disk magic (1 line).** `bins/zen-rs/src/EGM.md` named `FSIDX014`;
   the v2 blob is `FSIDX015`. Corrected. The layout itself is data, not
   code, and is kept as a compatibility contract.
3. **Verification command (51 lines).** Every contract carried
   `- Run: \`cargo build --workspace && cargo test --workspace --no-fail-fast\``,
   which is not the line CI runs. `.egmrc` now declares
   `cargo test --workspace --no-fail-fast --locked` and all 51 `Run:`
   lines were changed to match. The checker rejects any contract whose
   `Run:` differs from `verification` and any `verification` that does
   not appear in `.github/workflows/ci.yml`.
4. **Invisible inputs.** The contracts were gitignored, so a CI check
   would have seen `checked=0`. `.gitignore` no longer ignores `EGM.md`;
   the 51 owned contracts are tracked; the checker and its `.sh`/`.ps1`
   twins are tracked; `.egm/PAIRS.md`, `.egm/DRIFT_STATUS.md`,
   `.egm/CONTRACTS.index` and the dated reports are un-ignored explicitly.
5. **Drift as a filename heuristic.** The only prior artifact was
   `.dev/docs/2026-11-06_DRIFT_REPORT.md`, ignored and free-form. The
   gate now reads `DRIFT_STATUS.md`'s `date`/`report`/`unresolved` and
   validates the calendar date, the dated report's existence, the
   date↔filename agreement, the newest-report rule and the counter.
   The dated reports are never parsed. The old `.dev` report stays
   ignored historical scratch.
6. **Pair table source.** The rgx checker read the pair table from
   `AGENTS.md` §1.1, which is tracked there. Here `AGENTS.md` is
   deliberately gitignored (§8), so the table moved to tracked
   `.egm/PAIRS.md` and the pair list is derived from `git ls-files` —
   a working tree and a fresh CI checkout now agree even though
   gitignored `pgo.sh`/`pgo.ps1` exist only here.

7. **The e2e build-first guard (1 file).** `tests/harness.rs`'s
   `newest_source` counts every file under `bins/<name>/src` when it
   decides whether a shipped binary is older than its sources. With the
   contracts tracked, an edited `EGM.md` there would leave the guard red
   permanently: cargo never relinks a binary because a document changed.
   `newest_source` now skips `EGM.md`, with a comment saying exactly why.
   The guard still compares against cargo's dep-info for real sources.
8. **Published crates (20 manifests + 1 test).** A tracked `EGM.md`
   inside a crate directory is carried into the registry by
   `cargo package` -- measured: `rgy-name-core` would have shipped four.
   Every owned crate and bin manifest now declares
   `exclude = ["**/EGM.md"]`, and `tests/egm_packaging.rs` both holds
   those manifests to the rule and asks `cargo package --list` what each
   package would actually carry. The vendored manifests are untouched:
   their contracts stay ignored (§1 leaves those trees to the sync).

9. **Links to out-of-scope contracts (1 file, 12 links).** The
   `crates/EGM.md` blueprint linked to the twelve vendored trees'
   `EGM.md` files. Those files exist as ignored scratch on this machine but
   will not exist in a fresh CI checkout, so the checker reported twelve
   broken links on the Linux box and would have failed in CI. The twelve
   rows were replaced by one paragraph that names the vendored trees in
   backticks and explains why they carry no tracked contract. `CONTRACTS.index`
   was regenerated in the same pass. This is the clearest instance of the
   "invisible inputs" problem the structure exists to remove: a contract
   that was valid on one machine and not on another.

No finding is left unresolved.

## Deviations from `.egm/EGM_STRUCTURE_PROPOSAL.md`

- The proposal anticipated new tracked `gate-and-count` twins. They were
  not added: rgy's local gate is untracked `.git/gate.ps1` tooling, and
  the two tracked `check-egm-contracts.{sh,ps1}` twins are already the
  single entry point that CI runs on Windows and Unix. Adding a second
  pair would be a second thing to keep in step for no new coverage.
- The proposal listed `CONTRACTS.sha256` as a phase-2 idea. The checker
  copied from rgx already carries a generated `CONTRACTS.index` with
  per-contract hashes and `--reindex`, so it landed in phase 1 rather
  than being deferred.
- The pair table moved to tracked `.egm/PAIRS.md` and the pair list is
  derived from `git ls-files`, not from a directory walk. This is what
  makes a working tree (which has gitignored `pgo.sh`/`pgo.ps1`) and a
  fresh CI checkout agree.

## Coverage honestly stated

This was a **targeted** drift pass over the areas the two commits touched,
plus the structural rules above. It is not a fresh-context semantic review
of all 51 contracts: the interiors of contracts describing untouched
modules were not re-derived from their sources. That is the same kind of
gap the earlier `.dev` report recorded for 8 contracts, and it is the
reason `.egm/CONTRACTS.index` is tracked — a later edit to any contract
without `--reindex` fails the gate, so drift that appears later cannot
quietly accumulate. A full Goldfish review of the regenerated set is the
next review step, not a condition of this status.

## Structure

The tracked set is described by `.egm/EGM_STRUCTURE_PROPOSAL.md`, the
structural copy of rgx's `5d4c301` proposal adapted to this repository.
