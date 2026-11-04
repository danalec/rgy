<!-- The release page, in the repository.

     This file IS the body of the release: the Forgejo publish script and the
     GitHub mirror both read it, so the two pages say the same thing, and the
     promise the README makes -- "the release page is the changelog" -- is
     kept by the tree and not by a memory.

     Two markers below are for the scripts, and the rendered page does not
     show them. `<!-- assets: ... -->` names the files the release carries,
     and the publish step compares that list with the files it is about to
     upload: a page that names an archive the release does not carry cannot
     be published. `<!-- platform -->` is where each publisher puts its own
     sentence about where the history lives.

     The text below is the page. It is written for the person who lands on
     the release with nothing else in hand. -->

Whole-filesystem name search and content search in one stack — a daemon, a CLI
and a launcher, native underneath and no_std above it.

**Which file?** — asked over the whole filesystem, then **what is inside it?**
in the same query:

```sh
searchctl --root ~ "invoice ext:pdf"
searchctl --root ~ "invoice grep:tesla"
```

The release carries four binaries: `searchctl` (the command-line tool),
`searchd` (the daemon: JSON lines over stdio, a pipe or a unix socket),
`rgy-autostart` (put the daemon in place at login) and `rgxy` (the launcher
over the daemon: a floating window where there is a desktop, the terminal where
there is not).

## Install

One line, and it installs the tool, the daemon and the launcher:

```sh
# Windows
irm https://raw.githubusercontent.com/danalec/rgy/main/install.ps1 | iex

# Linux and macOS
curl -fsSL https://raw.githubusercontent.com/danalec/rgy/main/install.sh | bash
```

The installer checks the archive's SHA256 before it unpacks anything, puts the
binaries in `~/.local/bin` (Linux) or `%LOCALAPPDATA%\Programs\rgy` (Windows),
adds that directory to the PATH, and registers the daemon and the launcher to
start at login. The digest is pinned inside the installer, so a substituted
download is refused before anything is unpacked.

## What changed in 0.1.4

This release changes no code of this tree. It moves the numbers forward and
makes the two repositories' lines agree — and the numbers are the part a
resolver reads.

- **The crates are at 0.1.4.** The thirty-two crates the binaries are made of
  are published at 0.1.4, and the lockfile carries them: the CI builds with
  `--locked`, so a lockfile that still named the previous numbers would
  describe a tree this repository does not have.
- **The engine's crates keep the upstream's names, at the upstream's next
  version.** The twelve the upstream's engine is transported as — `rgx-core`,
  the ten `rgx-adapters-*` and `rar-codec` — are upstream's crates, and they
  are published under the upstream's names because that is what they are.
  Their numbers are **one line that both repositories draw from**: the registry
  does not replace a published version, so a release takes the next free
  number of it. The upstream took 0.1.3 on the third of November, and this
  repository takes **0.1.4**.
- **The copy is the upstream's tree at its release of the third of November.**
  Measured between the two upstream commits this tree has carried — the
  twenty-eighth of October and the third of November — the upstream changed
  nothing inside the crates except the numbers in the twelve manifests. So the
  engine in this release is the engine of 0.1.3, carried at the number this
  repository's line has free, and the patches that keep it this tree's are the
  same patches with the same fixes in them.
- **The formula names this release.** The brew formula named 0.1.2 — a release
  that does not exist — so the two places that promise a download pointed at
  nothing. It names 0.1.4 now. Its digests are still placeholders, because
  this release publishes no macOS archive: the CI builds those on a Mac and
  publishes none of them.

The rest of 0.1.3's work — the audit's fixes, the archive carrying the
documents the README points at, the page whose name is the version's, the
version with one name — is in this release too, and its changelog is the
0.1.3 page.

## The archives

- `rgy-v0.1.4-x86_64-windows.zip` — the four binaries, the `rgy` alias and the
  documents.
- `rgy-v0.1.4-x86_64-linux.tar.gz` — the same four binaries, the `rgy` alias and
  the documents.
- `SHA256SUMS` — the digest of each archive.

The other nine archives are not in this release: the CI builds them on their
own runners — the macOS ones on a Mac, the aarch64 ones on an arm64 machine —
and it publishes none of them. The two archives here are the portable x86_64
ones; on macOS and on aarch64 the installer says so and installs nothing.

Apache-2.0.

<!-- assets: rgy-v0.1.4-x86_64-windows.zip rgy-v0.1.4-x86_64-linux.tar.gz SHA256SUMS -->
<!-- platform -->
