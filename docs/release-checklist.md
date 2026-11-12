# Cutting a release — checklist

The ordered steps for the next release, **0.1.10.3**. This file is a
maintainer's note; it does not travel inside the archives and it is not a
release page. Its name deliberately does not match `release-<base>.<n>.md`:
`scripts/lib/version.ps1` picks the highest-numbered release page in
`docs/`, and a checklist that looked like a page would silently change the
version the whole tree resolves.

The reference is the 0.1.10.2 release, cut on 2026-11-09 (Forgejo release
`rgy-v0.1.10.2`, GitHub release at the same tag, 13 assets). The steps below
are the ones that were actually run, adjusted for fourteen archives.

## State of this branch right now

Everything here is **prepared, not published**.

| thing | state |
|---|---|
| branch | `msi-windows-installer` @ `34ff2ba`; also on Forgejo and GitHub |
| `forgejo/main` | `62fdb28` — still 0.1.10.2, untouched |
| `github/main` | `74cfff3` — flat orphan mirror, still 0.1.10.2 |
| version helpers | `0.1.10.3` (both `version.ps1` and `version.sh`) |
| release body | `docs/release-0.1.10.3.md`, one assets marker naming **14** archives + `SHA256SUMS`, one platform marker |
| crates | stay at `0.1.10`; `bump-version.py --check 0.1.10.3` says *nothing to change* |
| digest pins | all **10 zeroed**: 5 in `install.sh`, 2 in `install.ps1`, 1 winget `InstallerSha256`, 2 in `Formula/rgy-search.rb` |
| `flake.nix` | `version = "0.1.10.3"`, but its **6 SRI hashes are still the 0.1.10.2 ones** — the comment says so; the publish script does *not* rewrite them |
| `.dist/` | stale 0.1.10.2 artefacts only; do not reuse |
| AUR | three PKGBUILDs prepared **locally on the CachyOS box** (`7cd2ed6`, `dd130fc`, `881d3e9`), `pkgver=0.1.10.3`, zeroed `sha256sums`, `LICENSE` installed. **Not pushed to AUR.** |
| Homebrew tap | `danalec/homebrew-rgy` still carries the 0.1.10.2 formula; `publish-homebrew.ps1` now pushes to it (not to `Homebrew/homebrew-core`) |
| Scoop bucket | `danalec/scoop-alts` still at 0.1.10.2; licence already `Apache-2.0` |
| winget | PR [#474460](https://github.com/microsoft/winget-pkgs/pull/474460) open, awaiting a moderator |
| licence | root `LICENSE` (verbatim Apache-2.0); GitHub API reports `Apache-2.0` |

**Do not touch `main` or the mirror until step 1.** Until then the public
`install.sh` must still say `version="rgy-v0.1.10.2"` with its real digest.

## 1. Put the tree on `main`

- [ ] Fast-forward `forgejo/main` to this branch:
      `git push forgejo HEAD:main`
- [ ] Rebuild the **flat orphan** GitHub mirror (zero parents) from the final
      tree and force-push it. This is what triggers CI:
      `tree=$(git rev-parse 'HEAD^{tree}')`;
      `orphan=$(git commit-tree "$tree" -m 'rgy 0.1.10.3')`;
      `git push -f github "$orphan:refs/heads/main"`.
      Do **not** `git push github main` from the branch: that would replace
      the flat mirror with full history.

## 2. CI

- [ ] Wait for the run to close **success**. The release matrix now has
      **14** rows (the two generic AVX-512 ones included); the `msi` job is
      independent of it. `gh run view <run-id> -R danalec/rgy`.
- [ ] The two `the manifests, rewritten` steps no longer skip
      `matrix.variant == 'avx512'`: the page names those archives now, so
      every row runs the publish rehearsal.

## 3. Bring the archives down

- [ ] `pwsh -File scripts/fetch-ci-assets.ps1 <run-id>`
      → 14 archives, `SHA256SUMS` and the **18**-row document sidecars into
      `.dist/`. The sidecars gained one row over 0.1.10.2's 17: the new root
      `LICENSE` now travels in every archive.
- [ ] Verify locally: every row of `.dist/SHA256SUMS` matches the file it
      names, and there are exactly 14 rows.
- [ ] Download the `rgy-msi` artefact if the MSI is to be attached:
      `gh run download <run-id> -R danalec/rgy -n rgy-msi`.
      Its `.msi.sha256` sidecar must match the `.msi`.

## 4. The six SRI hashes (manual — nothing rewrites them)

- [ ] Recompute the six `sha256-…` values in `flake.nix` from the real
      `.dist/SHA256SUMS`. Each is the base64 of the 32 raw bytes of an
      archive's hex digest (not the base64 of the hex text), for
      `x86_64-linux`, `aarch64-linux`, `avx2-linux`, `znver4-linux`,
      `x86_64-apple-darwin`, `aarch64-apple-darwin`.
- [ ] Restore the truthful comment: the values now come from the 0.1.10.3
      release's `SHA256SUMS`, and `nix flake check` verifies them.
- [ ] `nix flake check` if Nix is available (it is not on the Windows host).

## 5. Publish

- [ ] `RGY_FORGEJO_URL=http://192.168.0.10:3000` and
      `pwsh -NoProfile -File scripts/publish-release.ps1 -SkipBuild -DryRun`.
      Read the dry-run: 15 assets (14 archives + `SHA256SUMS`), the body
      hash, and the pin rewrites.
- [ ] Run it for real: `pwsh -NoProfile -File scripts/publish-release.ps1 -SkipBuild`.
      It creates the tag `rgy-v0.1.10.3`, the Forgejo release, uploads the
      15 assets, and rewrites the **10** digest pins from the real
      `SHA256SUMS` (`install.ps1` ×2, `install.sh` ×5, winget ×1,
      Homebrew ×2). It also validates the winget manifests.
- [ ] Do **not** pass `-WithGithub`: that pushes full history to the mirror.
      Create the GitHub release by hand instead:
      `gh release create rgy-v0.1.10.3 --target <orphan> --title 'rgy v0.1.10.3' --notes-file docs/release-0.1.10.3.md <15 assets>`,
      then move the tag onto the orphan:
      `git push -f github "<orphan>:refs/tags/rgy-v0.1.10.3"`.
- [ ] Optional: attach `rgy-0.1.10.3-x86_64.msi` and its `.sha256` to both
      releases with `gh release upload --clobber` and the Forgejo API. The
      MSI is **not** one of the fourteen archives, is not named by the page's
      assets marker and is not in `SHA256SUMS` — say so if you attach it.

      **Done for 0.1.10.3 (2026-11-10), by hand, after the release was cut.**
      The `rgy-msi` artifact of the final CI run (42878182033) supplied
      `rgy-0.1.10.3-x86_64.msi` (7 233 536 bytes, SHA256
      `68491853433f3fdca0ec5c5502a34e927a187ad07103daab745a4af859cbb8da`)
      and its `.sha256` sidecar; both were uploaded to the GitHub release
      with `gh release upload --clobber` and to the Forgejo release with
      `POST /releases/<id>/assets?name=…` (Basic auth from
      `git credential fill`). Both releases went from 15 to **17** assets,
      and the published `.msi` was downloaded back from each forge and
      matched the sidecar.

      **Re-attached on 2026-11-12, after the GitHub repository was made
      again.** The repository had been deleted and was recreated by
      `.git/flat-mirror.py upload`, and **the recreate took the manual
      pair with it**: the run 42878182033 is gone with the old runs
      (404), and the GitHub release was carrying the fourteen assets the
      page names and nothing else.

      The MSI now comes from the run that built the PUBLISHED archives
      (**43148965141**), so it is built from the same Windows archive the
      release carries: `rgy-0.1.10.3-x86_64.msi`, **7 233 536 bytes**,
      SHA256
      **`9294b166aeaf2214fe7daf03977c339dfd520574059edd80dd8a3dccf9e86466`**,
      with its `.sha256` sidecar. The digest differs from the one first
      published and the size does not: two builds of the same tree are not
      the same bytes.

      **The two forges had drifted apart and now agree.** Measured on
      2026-11-12: the GitHub release carried the new MSI (digest
      `9294b166…`, matching its sidecar) while the Forgejo release still
      carried the first publication's (`68491853…`, not matching it). The
      Forgejo's pair was deleted and re-uploaded from the same two files;
      both releases now carry 17 assets, and the MSI downloaded back from
      each forge has the digest `9294b166…` — the same one the sidecar
      names.

      **The delete route needs the release's id.** The upload is
      `POST /api/v1/repos/<owner>/<repo>/releases/<release-id>/assets?name=…`
      and the delete is
      `DELETE /api/v1/repos/<owner>/<repo>/releases/<release-id>/assets/<asset-id>`:
      without the release's id the delete answers **404** and the old
      asset stays. Measured on 2026-11-12.

      **The release page does not name the MSI, and that is deliberate.**
      `docs/release-0.1.10.3.md` travels INSIDE every published archive
      (measured: `docs/release-0.1.10.3.md` is a member of
      `rgy-v0.1.10.3-x86_64-windows.zip`), so a sentence added to it would
      make the tree's page differ from the page inside the published
      archives and stale every document digest that covers it. The page's
      assets marker stays as it is, and the truth about the manual pair
      lives here — in a file that is not in the archives and does not
      match the release-page glob.

      The two places below that say "15 assets" describe what
      `publish-release.ps1` uploads on its own; they are correct for the
      publisher and do not count the manual pair. Nothing rewrites the
      page's assets marker, so a later re-run of the publisher neither
      deletes nor re-attaches the MSI: its name has no `v` and misses the
      `rgy-v<version>-*` glob. **For the next release, do not forget this
      step** — or make it automatic by adding the MSI to the marker, the
      member list and `SHA256SUMS`, which would also change every pin.

## 6. The repositories outside this tree

- [ ] **AUR** (on the CachyOS box, `/home/danalec/aur-rgy/*`): replace each
      zeroed `sha256sums` with the real digest of its archive from
      `.dist/SHA256SUMS`, regenerate `.SRCINFO` with
      `makepkg --printsrcinfo`, build with `makepkg --nodeps --nocheck -f`
      (the digest check must pass), then commit and push all three.
      Archives: `rgy-search-bin` → `x86_64-linux`,
      `rgy-search-avx2-bin` → `avx2-linux`,
      `rgy-search-znver4-bin` → `znver4-linux`.
- [ ] **Homebrew tap**: `pwsh -NoProfile -File scripts/publish-homebrew.ps1`.
      It clones `danalec/homebrew-rgy`, commits the formula whose digests
      step 5 rewrote, and pushes `main`. No fork, no PR, no
      `Homebrew/homebrew-core`.
- [ ] **Scoop bucket**: bump `danalec/scoop-alts/bucket/rgy.json` to
      0.1.10.3 with the real `x86_64-windows.zip` hash. The `post_install`
      prompt (register / register-and-start / do nothing) stays as it is.
- [ ] **winget**: if PR #474460 is still open, update the manifests there;
      otherwise note that it is still awaiting a moderator.

## 7. Verify and close

- [ ] Forgejo release: tag, **15** assets, not a draft. The API needs Basic
      auth from `git credential fill` (an anonymous request answers 404).
- [ ] GitHub release: 15 assets, tag on the orphan, `raw/install.sh` on
      `main` now serves 0.1.10.3 with a real digest.
- [ ] Both published `SHA256SUMS` byte-identical to `.dist/SHA256SUMS`.
- [ ] Download one published archive and compare its SHA256 end to end.
- [ ] `python scripts/check-egm-contracts.py --reindex` after the pin
      rewrites, then the checker, `--generated-check` and `--self-test`.
- [ ] Gates: `cargo build --workspace --locked` first, then
      `cargo test --workspace --no-fail-fast --locked`, `cargo fmt --all --
      --check`, `cargo clippy --workspace --all-targets --locked -- -D warnings`,
      licence + packaging, and `scripts/check-twins.sh` on the CachyOS mirror.
- [ ] Commit the pin rewrites and the SRI hashes (the 0.1.10.2 release did
      this as `8ef8da4`), push to Forgejo `main`, and refresh the flat mirror
      once more so the public installers and the release agree.

## Why the order matters

- The page's assets marker is a **contract**: `publish-release.ps1` compares
  it against the files in `.dist/`. A page that names an archive the release
  does not carry — or a release carrying one the page does not name — stops
  the publish. That is why step 2 must be green before step 5.
- Digest pins stay zeroed until step 5 rewrites them. A leftover 0.1.10.2
  digest beside a 0.1.10.3 URL refuses every new download; a zero beside a
  0.1.10.2 URL does the same. Do not publish half-moved pins.
- `docs/release-0.1.10.2.md` stays in the tree as the historical page. Do
  not edit it: it travelled inside the published 0.1.10.2 archives and its
  digest is in their sidecars.
- `build-release.ps1` checks every relative Markdown link in every document
  the archive carries, resolved against that document's own folder. A link
  in `docs/release-X.md` to `docs/release-Y.md` resolves to
  `docs/docs/release-Y.md` and stops the build. Check the links before
  step 1: `pwsh -File scripts/build-release.ps1 -SkipTests -Target
  x86_64-pc-windows-msvc -Variant portable` runs the real guard locally.
  It also confirms the sidecar row count (18 for 0.1.10.3).
- The archive's **member list** and the **sidecar's document list** must be
  the same list. `build-release.ps1` derives `$files` from `$documents`;
  `build-release.sh` must do the same, and both now refuse when a document
  is missing from the member list. A hardcoded second list once left
  `LICENSE` out of nine `.tar.gz` archives while their sidecars named it.
  After step 3, check all fourteen: every archive must contain `LICENSE`.
