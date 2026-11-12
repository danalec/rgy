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

## What changed in 0.1.6

This release carries a fix. The engine's OpenEXR reader could be crashed by
a file it was asked to read, and it is the upstream's fix, carried here with
the upstream's code.

- **The OpenEXR reader no longer walks off the end of the file.** The reader
  takes an attribute's name from the header, and the header's bytes are not
  necessarily text: the reader decoded the name with a *lossy* conversion,
  which replaces every byte that is not valid UTF-8 with U+FFFD — three bytes
  each — and then moved its offset by the length of the **string** it had been
  given, instead of the number of **bytes** it had read. Ten invalid bytes
  were thirty bytes of string, and the offset walked past the end of the
  file, where the next read panicked instead of refusing the file. A
  thirty-eight byte file was enough to do it. The reader now counts the bytes
  the name took in the data, and a read that starts past the end of the file
  is a refusal and not a crash: the answer to a file that cannot be parsed is
  a refusal, and this release is the one that answers it.
- **The engine's crates are the upstream's, at the upstream's 0.1.5.** The
  twelve the upstream's engine is transported as — `rgx-core`, the ten
  `rgx-adapters-*` and `rar-codec` — are upstream's crates, and they are
  published under the upstream's names because that is what they are. Their
  numbers are **one line that both repositories draw from**: the registry
  does not replace a published version, so a release takes the next free
  number of it. The upstream took 0.1.5 on the third of November, and this
  repository takes **0.1.6** — the binary skips 0.1.5 on purpose, because a
  number one repository has taken is a number the other cannot have.
- **The crates are at 0.1.6.** The thirty-two crates the binaries are made of
  are published at 0.1.6, and the lockfile carries them: the CI builds with
  `--locked`, so a lockfile that still named the previous numbers would
  describe a tree this repository does not have. The requirement lines that
  name the vendored crates say 0.1.6 too — the resolver reads them, and a
  requirement left at the number of a release whose bytes are not the bytes
  here installs the release before this one, silently, at the user's machine.
- **The formula names this release.** It names 0.1.6 now. Its digests are
  still placeholders, because this release publishes no macOS archive: the CI
  builds those on a Mac and publishes none of them.

The rest of 0.1.4's work — the audit's fixes, the archive carrying the
documents the README points at, the page whose name is the version's, the
version with one name — is in this release too, and its changelog is the
0.1.4 page.

## The archives

- `rgy-v0.1.6-x86_64-windows.zip` — the four binaries, the `rgy` alias and the
  documents.
- `rgy-v0.1.6-x86_64-linux.tar.gz` — the same four binaries, the `rgy` alias and
  the documents.
- `SHA256SUMS` — the digest of each archive.

The other nine archives are not in this release: the CI builds them on their
own runners — the macOS ones on a Mac, the aarch64 ones on an arm64 machine —
and it publishes none of them. The two archives here are the portable x86_64
ones; on macOS and on aarch64 the installer says so and installs nothing.

Apache-2.0.

<!-- assets: rgy-v0.1.6-x86_64-windows.zip rgy-v0.1.6-x86_64-linux.tar.gz SHA256SUMS -->
<!-- platform -->
