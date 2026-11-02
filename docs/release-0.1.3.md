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

## What changed in 0.1.3

This release is the one that made the archive answer for itself.

- **The archive carries the documents the README points at.** The README and
  the manual travel inside the archive and point at their neighbours by
  relative paths: the release page — the changelog — and the benchmark folder
  with the measured numbers. A link that resolves on the forge and not in the
  archive is broken exactly where the reader is offline, so the build now
  refuses an archive whose documents point at something the archive does not
  carry, and it says which link it was.
- **The page's name is the version's.** The page was named for one release and
  read for another in three places at once — the tree, the CI and the two
  publish scripts — and the drift was silent, because a publish reads the file
  that is there and does not complain. The name is derived from the version the
  tree carries, in all of them.
- **The record beside the archive names everything the archive packs.** It
  listed the documents and the pictures, and not the page and the benchmark, so
  it was a record of a part of the archive — and the check the publish step
  makes, the one that catches an archive built from an older tree, was about a
  part of the archive too.
- **The version has one name.** The bump script sets the crates' version and
  the installer's tag. An installer that named another release downloaded that
  release's archive and refused it, because the digest pinned inside it was
  the digest of this release's archive.
- **The tree says which rustc it needs.** The README promised 1.85, and the
  promise was older than the code: the SIMD kernels use the AVX-512 target
  features, which landed in 1.89, and the byte engine uses `as_chunks`, which
  landed in 1.88. The promise is 1.89 now — the floor on every 64-bit x86
  target — the manifests declare it, and a CI job builds the tree with that
  exact rustc, so the sentence cannot rot with nobody noticing.
- **The crates are at 0.1.3.** The twenty crates this tree owns are published
  at 0.1.3. The command-line tool's crate is `rgy-search`; it was
  published first as `rgy-searchctl`, and that name is **yanked** now, so
  `cargo install rgy-searchctl` says the version was yanked instead of
  installing a crate this project no longer has.
- **The engine's crates keep the upstream's names, at the upstream's next
  version.** The twelve the upstream's engine is transported as — `rgx-core`,
  the ten `rgx-adapters-*` and `rar-codec` — are upstream's crates, and they
  are published under the upstream's names because that is what they are.
  They were published that way before, which meant that a crate installed
  from crates.io got the upstream's engine: the same code without the fixes
  this tree carries in it. That is over — the code inside is the upstream's,
  with this tree's fixes on top, at the upstream's next version, **0.1.2**,
  because 0.1.1 is published and a version that exists cannot be published
  again — and the archives and the crates.io artifacts are the same product
  now.
- **The crates the registry no longer needs are out of the registry.** crates.io
  does not delete a published version — the most it does is a yank, and a yank
  is reversible — so that is what happened to the thirty versions this
  release replaces: this project's first name (`rgy-searchctl` 0.1.0 and
  0.1.1), the launcher's own old name (`rgy-rgxy` 0.1.1 and 0.1.2 — the
  program is `rgxy` now), the upstream's engine under the upstream's names
  (`rgx-core`, the ten `rgx-adapters-*` and `rar-codec`, each of them in both
  of its published versions) and the upstream's command-line tool (`rgx-search`,
  likewise). A crate on crates.io resolves its dependencies from the registry,
  so those names were the ones the 0.1.2 artifacts of this project were built
  against — which is the same sentence the other way round: the 0.1.2 artifacts
  of this project stopped resolving, and `cargo install rgy-search` asked for a
  version that was withdrawn, until the 0.1.3 artifacts are out. Those carry
  `rgx-core` 0.1.2 and the rest, and they resolve; and the 0.1.2 artifacts
  resolve again too, because `^0.1.0` finds the 0.1.2 that replaced the
  yanked one.

The platform work of 0.1.2 is in this release too: an arm64 Linux machine
opens its folders, a macOS daemon answers on a socket that fits, a macOS answer
that stops in the middle is an error everywhere, and a picker that is being
written still runs. Those four were in 0.1.2's bytes; this release is the
next number for the same tree with the archive's own documents in it.

## What the audit fixed

The tree was read end to end before this release — one reader per area, and
every finding checked by hand before a line changed. What came out of it is a
list of things that were wrong in ways a user would never see from the
outside.

- **A crash in the SQLite reader.** A table whose name was not on a character
  boundary — a name with an emoji in it, say — panicked the reader. The name
  comes out of the file, and a name is not the tree's own doing: it does not
  get to stop the answer.
- **The long path on Windows.** A path that already carried the `\\?\` prefix
  was rewritten as `\\?\UNC\?\…`, and every such path failed with
  `ERROR_BAD_NETPATH`. A path that is already spelled the long way is left
  as it is.
- **The index cache could serve another tree.** The stamp beside the index
  file named the tree the index was built for, and the index file itself said
  nothing. They are two files, written one after the other, and a crash
  between the two writes left them disagreeing — the stamp naming the tree
  built *before*, the file holding the tree built *after*. The index file now
  says which tree it is for, and a loader checks that before it answers. The
  format changed with it (`FSIDX015`), so an index from an older release is
  rebuilt rather than trusted.
- **A query with nine words lost the ninth.** The engine keeps eight scored
  slots per folder, and a ninth word had nowhere to be scored. It is still
  not scored — but it is now said out loud, because a query that ran with
  fewer words than it was given answers a narrower question than the one
  asked.
- **A date the filter could not hold was wrapped into the past.** A year past
  2106 overflowed the seconds the filter carries, and the answer was a date in
  the wrong decade. The filter refuses what it cannot hold, and it takes the
  slash form the manual documents (`mtime:2024/01/01`).
- **One bad line in the rules file turned the whole file off.** A file with
  one broken rule is a file with one rule missing, not a file that says
  nothing: the rules that parsed are the rules, and the broken line is named
  with its line number.
- **The icon in the tray.** The mask of a one-bit icon is padded to a word
  and not to four bytes, and the tree padded it to four: the icon came out
  garbled at every size where the two paddings disagree. The tip also kept
  the tail of the one before it, and the shell showed two sentences at once.
- **The root a caller named was followed on one platform and not on the
  others.** The portable walk resolved a symlinked root and the three unix
  backends did not, so the same tree answered differently depending on which
  backend served it. The root the caller named is resolved everywhere, and
  the folders under it are not.
- **The count of the paths that cannot be spelled counted spellings.** The
  daemon spells the search root once per hit, so one root that is not UTF-8
  was counted as many paths as there were hits. The count is of paths now,
  and the root is spelled once for the answer.
- **A limit of 1.9 hits became a limit of 1.** The answer carries whole
  hits, and a request for one and a half of them is a request for a number
  of hits that does not exist. The reader casts it today and truncates;
  it refuses it, the way the wire already refuses the other numbers that
  cannot be true.
- **`~` on a drive root meant "the current directory on C:".** A home of
  `C:\`, trimmed of its separator, is `C:` — which on Windows is not the
  root of the drive but wherever the program was started. The separator is
  what says which one it is, so it is kept.
- **The terminal drew its rows in characters, not in cells.** A CJK
  ideograph takes two cells and a combining mark takes none, so a row
  measured in characters was a row twice as wide as the terminal it was
  drawn for — and the terminal wrapped it. The rows are measured in cells
  now, and the cut is on a character boundary, because half a character is
  not a character.
- **The burst of typing was a search per letter.** The interface reads the
  keys already queued behind the one it has, so that a burst costs one
  search — and the console hands over a *release* for every press, and the
  drain stopped at the first one. So the drain stopped at the first key,
  and never fired at all. A release carries no news for the query, and the
  drain goes on.
- **The picker was handed the whole tree.** `--pick 'grep:needle'` asked
  the *name* layer a question about *contents*: the content operator was
  parsed away, the query was left with no name token, and the name layer
  matched everything. The user chose from the whole tree and opened a file
  that does not hold the text they searched for — while the same query
  without `--pick` answered correctly.
- **A `*` inside `regex:` voided the search.** `regex:need.*` is a regex
  the engine takes, but the wildcard test ran first and the whole word
  became a glob for a file called `regex:need.*`. Nothing matched, and
  nothing said why: the tool reported "nothing found" for a search it
  never ran.
- **One refusal had three answers.** A pattern the engine refuses is a
  warning and an empty answer; the local route made it an error with the
  exit code 2, and the daemon's scoped route made it an `{"op":"error"}`
  that ended the answer. The three now say the same thing.
- **The daemon issued a session and then refused it.** The id entered the
  map only inside the search that used it, and a query with `|` never got
  that far — so the daemon put an id on the wire and then said it was not
  its own. The answers were right; the session was never the same one
  twice.

## The archives

- `rgy-v0.1.3-x86_64-windows.zip` — the four binaries, the `rgy` alias and the
  documents.
- `rgy-v0.1.3-x86_64-linux.tar.gz` — the same four binaries, the `rgy` alias and
  the documents.
- `SHA256SUMS` — the digest of each archive.

The other nine archives are not in this release: the CI builds them on their
own runners — the macOS ones on a Mac, the aarch64 ones on an arm64 machine —
and it publishes none of them. The two archives here are the portable x86_64
ones; on macOS and on aarch64 the installer says so and installs nothing.

Apache-2.0.

<!-- assets: rgy-v0.1.3-x86_64-windows.zip rgy-v0.1.3-x86_64-linux.tar.gz SHA256SUMS -->
<!-- platform -->
