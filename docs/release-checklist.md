# Cutting a release — checklist

The ordered steps for a release, and the ones that were actually run for
**0.1.10.4**, cut on 2026-11-25. This file is a maintainer's note; it does not
travel inside the archives and it is not a release page. Its name deliberately
does not match `release-<base>.<n>.md`: `scripts/lib/version.ps1` picks the
highest-numbered release page in `docs/`, and a checklist that looked like a
page would silently change the version the whole tree resolves.

The reference was the 0.1.10.3 release, cut on 2026-11-10 (Forgejo release
`rgy-v0.1.10.3`, GitHub release at the same tag, 13 assets from the publisher
and 17 from each forge after the manual MSI pair). The steps below are the
ones that were actually run, adjusted for fourteen archives and for a tree
whose launcher now answers without a daemon.

## The state of this release

**0.1.10.4 is published.** Both forges carry the release, the installers carry
its real digests, and the packages outside the tree carry it.

| thing | state |
|---|---|
| release commit | `72d1757` — the version bump, the release page and the six SRI hashes |
| Forgejo | `main` at the pins commit `0c9ec57`; release `rgy-v0.1.10.4` (id 159) with **17 assets** |
| GitHub | `main` is the flat orphan `ce8fc60` (no parents), whose tree is the Forgejo `main` tree; release `rgy-v0.1.10.4` on that orphan, **17 assets** |
| the tag | `rgy-v0.1.10.4` on the Forgejo (the release commit) and on the GitHub orphan |
| the gate | run `45644738032` on the release commit: 35 jobs, all green |
| the assets | 14 archives + `SHA256SUMS` from the publisher; the MSI pair attached by hand to both forges |
| the MSI | `rgy-0.1.10.4-x86_64.msi`, 7 659 520 bytes, SHA256 `29a27f37d024a472c5ad92fbcd284c0d5044bda2f30f8904be162e404adb3bb4`, with its `.sha256` sidecar |
| the digest pins | the **10** rewritten from the release's own `SHA256SUMS`: `install.sh` ×5, `install.ps1` ×2, winget ×1, Homebrew ×2 |
| `flake.nix` | `version = "0.1.10.4"`, and its **6 SRI hashes** recomputed by hand from the release's `SHA256SUMS` |
| the crates | stayed at `0.1.10`; `bump-version.py --check 0.1.10.4` says *nothing to change* |
| the AUR | three packages at **0.1.10.4-1**, built and pushed from the CachyOS box; the digest check passed on the build |
| Homebrew | `danalec/homebrew-rgy` `main` carries the 0.1.10.4 formula |
| Scoop | `danalec/scoop-alts` `bucket/rgy.json` at 0.1.10.4, with the x86_64 Windows digest |
| winget | PR [#474460](https://github.com/microsoft/winget-pkgs/pull/474460) is open; its manifests moved from the 0.1.10.3 folder to the 0.1.10.4 one |
| the gate's own question | the `ci/gate-0.1.10.4c` tag was deleted after the release: a published release is answered by its own tag |

**What is not done.** `nix flake check` did not run: Nix is not on the Windows
host, so the six SRI hashes are verified against the release's own
`SHA256SUMS` and not by the flake. The Forgejo has no runner alive since
2026-10-23, so the gate ran on the GitHub only. The crates.io registry was not
republished, and the winget submission still waits for a moderator.

## 0. The gate answers about a commit, and a tag is how you ask

- [x] The gate runs on a push to `main` and on a pull request whose author is
      the owner or a collaborator. A commit that is neither — a branch about to
      be merged, a commit that only exists in a fork — cannot be asked about
      at all. The workflow now carries a **tag trigger**: a tag named
      `ci/<anything>` runs the whole gate against the commit it points at, on
      every runner the file names, and the tag is the only thing to clean up
      afterwards.
- [x] So the release is asked about like this, before anything is published:
      `git tag ci/gate-0.1.10.4c <commit>` and
      `git push github ci/gate-0.1.10.4c`. The push goes to the **GitHub**
      mirror, which is the only place with runners alive: the Forgejo has
      Actions enabled but no runner has picked up a job since 2026-10-23, so
      a tag there creates a run that never starts.
- [x] Read the run before going on: `gh run list -R danalec/rgy`. Every job
      has to close **success**, including the two `no_std` jobs and the four
      `suite` jobs. A red job on `stable` is the tree's, not the runner's:
      the bare-metal build and the vendored crate were red until the rgx
      re-pin to `bb61e01` (`efa025b`), and the e2e harness was red until it
      learned the cargo 1.100 layout — both fixes are in `main`, and a branch
      cut from an older base does not have them.

## 1. Put the tree on `main`

- [x] Fast-forward `forgejo/main` to this branch:
      `git push forgejo HEAD:main`
- [x] Rebuild the **flat orphan** GitHub mirror (zero parents) from the final
      tree and force-push it. This is what triggers CI:
      `tree=$(git rev-parse 'HEAD^{tree}')`;
      `orphan=$(git commit-tree "$tree" -m 'rgy 0.1.10.4')`;
      `git push -f github "$orphan:refs/heads/main"`.
      Do **not** `git push github main` from the branch: that would replace
      the flat mirror with full history.
- [x] And again after the pins are rewritten: the public installers and the
      release have to answer with the same tree, and the pin commit is not in
      the release's tag.

## 2. CI

- [x] Wait for the run to close **success**. The release matrix has **14**
      rows (the two generic AVX-512 ones included); the `msi` job is
      independent of it. `gh run view <run-id> -R danalec/rgy`.
- [x] The two `the manifests, rewritten` steps no longer skip
      `matrix.variant == 'avx512'`: the page names those archives now, so
      every row runs the publish rehearsal.

## 3. Bring the archives down

- [x] `pwsh -File scripts/fetch-ci-assets.ps1 <run-id>`
      → 14 archives, `SHA256SUMS` and the document sidecars into `.dist/`.
- [x] Verify locally: every row of `.dist/SHA256SUMS` matches the file it
      names, and there are exactly 14 rows.
- [x] Download the `rgy-msi` artefact if the MSI is to be attached:
      `gh run download <run-id> -R danalec/rgy -n rgy-msi`.
      Its `.msi.sha256` sidecar must match the `.msi`.

## 4. The six SRI hashes (manual — nothing rewrites them)

- [x] Recompute the six `sha256-…` values in `flake.nix` from the real
      `.dist/SHA256SUMS`. Each is the base64 of the 32 raw bytes of an
      archive's hex digest (not the base64 of the hex text), for
      `x86_64-linux`, `aarch64-linux`, `avx2-linux`, `znver4-linux`,
      `x86_64-apple-darwin`, `aarch64-apple-darwin`.
- [x] Restore the truthful comment: the values now come from the 0.1.10.4
      release's `SHA256SUMS`, and `nix flake check` verifies them.
- [ ] `nix flake check` if Nix is available (it is not on the Windows host).

## 5. Publish

- [x] `RGY_FORGEJO_URL=http://192.168.0.10:3000` and
      `pwsh -NoProfile -File scripts/publish-release.ps1 -SkipBuild -DryRun`.
      Read the dry-run: 15 assets (14 archives + `SHA256SUMS`), the body
      hash, and the pin rewrites.
- [x] Run it for real: `pwsh -NoProfile -File scripts/publish-release.ps1 -SkipBuild`.
      It creates the tag `rgy-v0.1.10.4`, the Forgejo release, uploads the
      15 assets, and rewrites the **10** digest pins from the real
      `SHA256SUMS` (`install.ps1` ×2, `install.sh` ×5, winget ×1,
      Homebrew ×2). It also validates the winget manifests.
- [x] Do **not** pass `-WithGithub`: that pushes full history to the mirror.
      Create the GitHub release by hand instead:
      `gh release create rgy-v0.1.10.4 --target <orphan> --title 'rgy v0.1.10.4' --notes-file docs/release-0.1.10.4.md <15 assets>`,
      then move the tag onto the orphan:
      `git push -f github "<orphan>:refs/tags/rgy-v0.1.10.4"`.
      The body is the tree's page with the `<!-- platform -->` marker
      replaced by the sentence about the mirror — the same sentence the
      0.1.10.3 release carries.
- [x] Optional: attach `rgy-0.1.10.4-x86_64.msi` and its `.sha256` to both
      releases with `gh release upload --clobber` and the Forgejo API. The
      MSI is **not** one of the fourteen archives, is not named by the page's
      assets marker and is not in `SHA256SUMS` — say so if you attach it.

      **The MSI is attached by hand, and the pair has to match.** It is built
      from the **published** Windows archive, so it comes from the run that
      built that archive and not from an earlier one; two builds of the same
      tree are not the same bytes, and the digest of the MSI that was
      published with 0.1.10.3 (`9294b166…`) is not the digest of this one.
      Download the MSI and its sidecar from the same run as the archives and
      compare the sidecar with the file before uploading either.

      **The delete route needs the release's id.** The upload is
      `POST /api/v1/repos/<owner>/<repo>/releases/<release-id>/assets?name=…`
      and the delete is
      `DELETE /api/v1/repos/<owner>/<repo>/releases/<release-id>/assets/<asset-id>`:
      without the release's id the delete answers **404** and the old asset
      stays. Measured on 2026-11-12.

      **The two forges drift, and the drift is measured, not assumed.** On
      2026-11-12 the GitHub release carried the new MSI and the Forgejo
      release still carried the first publication's. The Forgejo's pair was
      deleted and re-uploaded from the same two files; both releases carried
      17 assets and the MSI downloaded back from each forge matched its
      sidecar. Check both forges, not one.

      **The release page does not name the MSI, and that is deliberate.**
      `docs/release-0.1.10.4.md` travels INSIDE every published archive, so a
      sentence added to it would make the tree's page differ from the page
      inside the published archives and stale every document digest that
      covers it. The page's assets marker stays as it is, and the truth
      about the manual pair lives here — in a file that is not in the
      archives and does not match the release-page glob.

      The two places below that say "15 assets" describe what
      `publish-release.ps1` uploads on its own; they are correct for the
      publisher and do not count the manual pair. Nothing rewrites the
      page's assets marker, so a later re-run of the publisher neither
      deletes nor re-attaches the MSI: its name has no `v` and misses the
      `rgy-v<version>-*` glob.

## 6. The repositories outside this tree

- [x] **AUR** (on the CachyOS box): replace each zeroed `sha256sums` with the
      real digest of its archive from `.dist/SHA256SUMS`, regenerate
      `.SRCINFO` with `makepkg --printsrcinfo`, build with
      `makepkg --nodeps --nocheck -f` (the digest check must pass), then
      commit and push all three.
      Archives: `rgy-search-bin` → `x86_64-linux`,
      `rgy-search-avx2-bin` → `avx2-linux`,
      `rgy-search-znver4-bin` → `znver4-linux`.
- [x] **Homebrew tap**: `pwsh -NoProfile -File scripts/publish-homebrew.ps1`.
      It clones `danalec/homebrew-rgy`, commits the formula whose digests
      step 5 rewrote, and pushes `main`. No fork, no PR, no
      `Homebrew/homebrew-core`.
- [x] **Scoop bucket**: bump `danalec/scoop-alts/bucket/rgy.json` to
      0.1.10.4 with the real `x86_64-windows.zip` hash. The `post_install`
      prompt (register / register-and-start / do nothing) stays as it is.
- [x] **winget**: PR #474460 is still open, so its manifests were moved to
      the 0.1.10.4 folder and its title and body were rewritten; it is still
      awaiting a moderator.

## 7. Verify and close

- [x] Forgejo release: tag, **17** assets, not a draft. The API needs Basic
      auth from `git credential fill` (an anonymous request answers 404).
- [x] GitHub release: 17 assets, tag on the orphan, `raw/install.sh` on
      `main` now serves 0.1.10.4 with a real digest.
- [x] Both published `SHA256SUMS` byte-identical to `.dist/SHA256SUMS`.
- [x] Download one published archive and compare its SHA256 end to end.
- [x] `python scripts/check-egm-contracts.py --reindex` after the pin
      rewrites, then the checker, `--generated-check` and `--self-test`.
- [x] Gates: `cargo build --workspace --locked` first, then
      `cargo test --workspace --no-fail-fast --locked`, `cargo fmt --all --
      --check`, `cargo clippy --workspace --all-targets --locked -- -D warnings`,
      licence + packaging, and `scripts/check-twins.sh`.
- [x] Commit the pin rewrites and the SRI hashes (the 0.1.10.2 release did
      this as `8ef8da4`), push to Forgejo `main`, and refresh the flat mirror
      once more so the public installers and the release agree.
- [x] Delete the `ci/gate-0.1.10.4c` tag from the mirror when the release is
      published: the tag is the gate's question, and a published release is
      answered by its own tag.

## Why the order matters

- The page's assets marker is a **contract**: `publish-release.ps1` compares
  it against the files in `.dist/`. A page that names an archive the release
  does not carry — or a release carrying one the page does not name — stops
  the publish. That is why step 2 must be green before step 5.
- Digest pins stay zeroed until step 5 rewrites them. A leftover 0.1.10.3
  digest beside a 0.1.10.4 URL refuses every new download; a zero beside a
  0.1.10.3 URL does the same. Do not publish half-moved pins.
- `docs/release-0.1.10.3.md` stays in the tree as the historical page. Do
  not edit it: it travelled inside the published 0.1.10.3 archives and its
  digest is in their sidecars.
- `build-release.ps1` checks every relative Markdown link in every document
  the archive carries, resolved against that document's own folder. A link
  in `docs/release-X.md` to `docs/release-Y.md` resolves to
  `docs/docs/release-Y.md` and stops the build. Check the links before
  step 1: `pwsh -File scripts/build-release.ps1 -SkipTests -Target
  x86_64-pc-windows-msvc -Variant portable` runs the real guard locally.
- The archive's **member list** and the **sidecar's document list** must be
  the same list. `build-release.ps1` derives `$files` from `$documents`;
  `build-release.sh` must do the same, and both now refuse when a document
  is missing from the member list. A hardcoded second list once left
  `LICENSE` out of nine `.tar.gz` archives while their sidecars named it.
  After step 3, check all fourteen: every archive must contain `LICENSE`.

## This release (0.1.10.4, cut on 2026-11-25)

The facts of this release, kept here because they are the ones the next one
repeats:

- The release carried **15 assets** from the publisher and **17** from each
  forge after the manual MSI pair: 14 archives plus `SHA256SUMS`, the MSI and
  its `.sha256` sidecar.
- The MSI was built from the **published** Windows archive (run
  `45644738032`, 35 jobs green), so it came from the run that built the
  archives the release carries: `rgy-0.1.10.4-x86_64.msi`, **7 659 520
  bytes**, SHA256
  `29a27f37d024a472c5ad92fbcd284c0d5044bda2f30f8904be162e404adb3bb4`,
  with its `.sha256` sidecar.
- The release tag is the **release commit**, not the pins commit: the
  Forgejo's `rgy-v0.1.10.4` points at `72d1757`, and the GitHub's points at
  the flat orphan, because a mirror that is flat carries a tree and not a
  history. The pins live one commit later, in both `main`s.
- The gate asked about the release through a **tag**, `ci/gate-0.1.10.4c`,
  and the tag was deleted when the release was published. The tag trigger
  is the answer to a commit that is neither a push to `main` nor a pull
  request — and the 0.1.10.4 tree needed it, because the PRs it carried came
  from a fork.
- The two PRs that carried the launcher's work were closed rather than
  merged: the GitHub `main` is a flat orphan with no common ancestor to
  merge against, and the work was cherry-picked onto the owner's own `main`
  on the Forgejo. Closing them lost nothing; the commits are there.

## The previous release (0.1.10.3, cut on 2026-11-10)

The facts of that release, kept here because they are the ones the next one
repeats:

- The release carried **13 assets** from the publisher and **17** from each
  forge after the manual MSI pair: 13 archives plus `SHA256SUMS`, the MSI and
  its `.sha256` sidecar.
- The MSI was built from the **published** Windows archive (run
  `43148965141`), so it came from the run that built the archives the
  release carries: `rgy-0.1.10.3-x86_64.msi`, **7 233 536 bytes**, SHA256
  `9294b166aeaf2214fe7daf03977c339dfd520574059edd80dd8a3dccf9e86466`,
  with its `.sha256` sidecar. The first publication's digest
  (`68491853…`) was the same size and different bytes.
- The repository was deleted and recreated by `.git/flat-mirror.py upload`,
  and the recreate took the manual pair with it: the run 42878182033 is
  gone with the old runs (404), and the GitHub release was carrying the
  fourteen assets the page names and nothing else.
