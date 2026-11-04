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

## What changed in 0.1.8

This release carries a fix, and it carries a rename. The rename is the bigger
of the two: the twelve trees the upstream's engine is transported as go out
under **this repository's** names now, and the reason is the registry's.

- **The vendored crates are published under names of their own.** They used to
  go out as `rgx-core`, the ten `rgx-adapters-*` and `rar-codec` — the
  upstream's names — because that is what they are. It was also a collision
  waiting for its date: the upstream publishes the same names, and on
  crates.io a name and a version is one thing, so two publishers cannot both
  take it. The upstream took 0.1.8 with the OpenEXR fix below, and this
  repository wanted the same number with the same code in it. The copies are
  now `rgy-vendored-core`, the `rgy-vendored-adapters-*` family and
  `rgy-vendored-rar-codec`; the upstream's names are the upstream's, and the
  two stand at 0.1.8 side by side without either of them giving way.
- **Not one line of the upstream's source changed to make that happen.** The
  crate's Rust name is still the upstream's (`rgx_core::`), so the sources are
  the upstream's byte for byte, and a dependant writes the same code either
  way. The *package* carries this repository's name on the registry; the
  *library* keeps the name its own code says. The two are one word apart on
  purpose, and the gate that checks the vendoring reads both.
- **The OpenEXR reader no longer overflows on a large attribute.** The reader
  took an attribute's size as a signed `i32`, and a file whose attribute
  claims a size past two gigabytes turned that into a negative number and
  then into an offset that pointed nowhere sensible. The size is read as
  unsigned and the arithmetic is checked, so a file that cannot be parsed is
  a refusal and not a crash. Every published version of that adapter before
  0.1.8 carried it — the 0.1.6 one did too, and the crash that proved it was
  found by a scheduled fuzz run whose failure went unread for two hours.
- **The crates are at 0.1.8.** The thirty-two crates the binaries are made of
  are published at 0.1.8, and the lockfile carries them: the CI builds with
  `--locked`, so a lockfile that still named the previous numbers would
  describe a tree this repository does not have. The requirement lines that
  name the vendored crates say 0.1.8 too — the resolver reads them, and a
  requirement left at the number of a release whose bytes are not the bytes
  here installs the release before this one, silently, at the user's machine.
- **The versions this repository published under the upstream's names stay
  where they are.** 0.1.2, 0.1.4 and 0.1.6 are on crates.io under
  `rgx-core` and the `rgx-adapters-*` family, and they are not yanked: they
  are the artifacts of older builds, and the registry keeps what it keeps.
  A dependant that asks for `rgy-vendored-core ^0.1.8` gets this
  repository's copy; one that asks for `rgx-core ^0.1.8` gets the
  upstream's.

The rest of 0.1.6's work — the first OpenEXR fix, the archive carrying the
documents the README points at, the page whose name is the version's — is in
this release too, and its changelog is the 0.1.6 page.

## The archives

- `rgy-v0.1.8-x86_64-windows.zip` — the four binaries, the `rgy` alias and the
  documents.
- `rgy-v0.1.8-x86_64-linux.tar.gz` — the same four binaries, the `rgy` alias and
  the documents.
- `SHA256SUMS` — the digest of each archive.

The other nine archives are not in this release: the CI builds them on their
own runners — the macOS ones on a Mac, the aarch64 ones on an arm64 machine —
and it publishes none of them. The two archives here are the portable x86_64
ones; on macOS and on aarch64 the installer says so and installs nothing.

Apache-2.0.

<!-- assets: rgy-v0.1.8-x86_64-windows.zip rgy-v0.1.8-x86_64-linux.tar.gz SHA256SUMS -->
<!-- platform -->
