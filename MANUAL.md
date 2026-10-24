# rgy — manual

How to use rgy: what you type, what it answers, and what to do when the
answer is not what you expected.

The [README](README.md) is the map — what rgy is, and where the docs live.
The [formats file](FORMATS.md) is what it can read inside a file. This is
the part you keep open while using it.

## Contents

1. [What it is](#1-what-it-is)
2. [Install it](#2-install-it)
3. [Build it](#3-build-it)
4. [Your first queries](#4-your-first-queries)
5. [The query language](#5-the-query-language)
6. [The flags](#6-the-flags)
7. [The daemon](#7-the-daemon)
8. [The pack store](#8-the-pack-store)
9. [When it goes wrong](#9-when-it-goes-wrong)
10. [See also](#10-see-also)

## 1. What it is

rgy answers two questions about the files on your computer:

- **Which file?** — every file and folder, by name, ranked the way a search
  engine ranks things: a close match first, a typo still found.
- **What is inside it?** — the text of the file, found by a second word in
  the same query.

Both in one sentence:

```sh
searchctl --root ~ "invoice ext:pdf grep:tesla"
```

That reads as: *files whose name is like "invoice", that are PDFs, that
contain the word "tesla"*.

The first part is answered from an index of names, which is why it is fast.
The second part reads the file, which is why it is the slow half.

rgy runs on Windows, Linux and macOS. The parts that differ per operating
system are compiled in and chosen at build time, so one package carries
everything with it.

## 2. Install it

**Nothing has been published yet.** The packaging is in place — the winget
manifest, the Homebrew formula, the script that builds the archives and
uploads them — but the first release has not been pushed, so the commands
below do not install anything today.

What the packages will be:

| platform | package | command |
|---|---|---|
| Windows | winget | `winget install danalec.rgy-search` |
| Windows | one-line installer | `irm https://raw.githubusercontent.com/danalec/rgy/main/install.ps1 \| iex` |
| macOS | Homebrew | `brew tap danalec/rgy https://github.com/danalec/rgy` then `brew install danalec/rgy/rgy-search` |
| macOS | Homebrew core, once the pull request is merged | `brew install rgy-search` |
| Linux | one-line installer | `curl -fsSL https://raw.githubusercontent.com/danalec/rgy/main/install.sh \| bash` |

The package is called **rgy-search** because `rgy` is taken on crates.io
(a GameBoy emulator library from 2019). The binaries inside keep the names
they have everywhere else, and the project itself is still rgy.

The one-line installers download the release archive, check its digest, put
the three binaries in `~/.local/bin` (Linux) or `%LOCALAPPDATA%\Programs\rgy`
(Windows) and add that directory to your PATH. The digest is **pinned in
the installer**: the installer carries the digest of the one artifact it is
allowed to install, so a substituted download is refused before anything is
unpacked. The pins are release data — the release flow rewrites them from
the archive it has just built.

Each package carries three binaries and the documents:

| binary | what it does |
|---|---|
| `searchctl` | the command-line tool — the one you type |
| `searchd` | the daemon: it holds the index warm between queries |
| `rgy-autostart` | puts the daemon in place at login, and takes it out again |

### Starting the daemon at login

The daemon is what makes a search find the files that were created while the
machine was off. `rgy-autostart` registers it, and running the command *is*
the consent: the tool never asks for elevation and never opens a dialog.

```sh
rgy-autostart install --binary /usr/local/bin/searchd   # the daemon's path
rgy-autostart status                                    # what is registered
rgy-autostart uninstall                                 # take it out
```

What gets registered depends on the operating system: a Task Scheduler task
(or the `HKCU` run value) on Windows, a systemd **user** unit on Linux, a
launchd agent on macOS. The tool prints what it wrote, and what it wrote is
a plain text file you can read and edit.

## 3. Build it

Rust **1.85** or newer.

```sh
cargo build --release            # the whole workspace
cargo build --release -p rgy-searchctl   # just the command-line tool
cargo build --release -p rgy-searchd     # just the daemon
```

The binaries land in `target/release/`. On Windows that is
`target\release\searchctl.exe`.

If you also want the pack store tool (section 8), it needs one extra flag,
because it is not part of a default build:

```sh
cargo build --release -p rgx-core --bin rgx-tv --features tv-runtime
```

The release scripts build the shippable archives: `scripts/build-release.sh`
on Linux and macOS, `scripts/build-release.ps1` on Windows. They gate on
clippy and on the test suite, smoke the binaries against the repository's
own sources, and write the archive — and its digest — into `.dist/`.

## 4. Your first queries

Point rgy at a folder with `--root`, and give it a query in quotes.

```sh
# by name, fuzzy: typos are fine
searchctl --root ~ "invoice"

# only PDFs
searchctl --root ~ "invoice ext:pdf"

# only folders
searchctl --root ~ "photos type:dir"

# inside the file
searchctl --root ~ "invoice grep:tesla"

# both, in one query
searchctl --root ~ "invoice ext:pdf grep:tesla"

# just the count, no list
searchctl --root ~ "invoice" --count

# machine-readable, one JSON object per hit
searchctl --root ~ "invoice" --json
```

By default you get the 50 best matches. Raise it with `--limit 100`, or
ask for the count with `--count`.

## 5. The query language

A query is a list of words. Every word narrows the answer, and a file has
to satisfy all of them.

### Words are fuzzy by default

Type `invoice` and rgy also finds `invoce`, `invoces` and `invoicing`.
That is the point: you remember the name, not the spelling.

To say exactly what you mean, mark the word:

| you write | rgy looks for |
|---|---|
| `invoice` | anything close to "invoice" |
| `'invoice` | a name that *contains* "invoice" |
| `^invoice` | something that *starts* with "invoice" |
| `invoice$` | something that *ends* with "invoice" |
| `=invoice` | a name that has "invoice" as a whole *word* |
| `"invoice pdf"` | a name that *contains* "invoice pdf", the space and all |
| `!invoice` | anything that is **not** close to "invoice" |

`=invoice` is the strict one: `port` is the word of `report_port.txt` and
not of `report`, so `=port` finds the first and not the second.

`"invoice pdf"` is the phrase: the words have to be there together, in
that order, with the space between them. `invoice pdf` (without the
quotes) is two loose words, and the name only has to carry both.

### The filters

A filter narrows by a property of the file, not by its name.

| filter | means | example |
|---|---|---|
| `ext:` | the file extension | `ext:pdf` |
| `dir:` | the folder it is in | `dir:documents` |
| `type:` | what kind of thing it is | `type:dir`, `type:link`, `type:image` |
| `size:` | how big it is | `size:+10m` (bigger than 10 MB), `size:-1k` |
| `mtime:` | when it changed | `mtime:-7d` (in the last week) |
| `in:` | where it is | `in:downloads` |
| `re:` | the path as a regular expression | `re:\.rs$` |
| `limit:` | how many hits | `limit:20` |

A bare extension goes in `ext:`; `type:` names kinds.

**An unknown `type:` fails closed.** rgy warns on stderr, returns nothing,
and exits 1 — it does not quietly search everything. If you get no
results and a warning, that is the reason.

### Alternatives and wildcards

| you write | means |
|---|---|
| `report\|invoice` | "report" **or** "invoice" |
| `*.pdf` | any file whose name ends in `.pdf` |
| `scan?` | any name with "scan" and one more character |

### Searching inside a file

Three operators look at the contents. The output is `path:line:text`, the
same shape `rg` prints.

| operator | means |
|---|---|
| `grep:` | the literal text |
| `regex:` | a regular expression |
| `sym:` | the symbols in the file, for code |

```sh
searchctl --root ~ "grep:quarterly revenue"
searchctl --root ~ "regex:fn\s+list"
```

Note what this does **not** do yet: on a PDF or a Word document, `grep:`
reads the compressed bytes and finds nothing useful. Those formats are
listed in [FORMATS.md](FORMATS.md) with their state; the adapters are in
the tree but not yet wired into the search path.

## 6. The flags

`searchctl` takes few flags, because the query does the work.

| flag | what it does |
|---|---|
| `--root <dir>` | the tree to search (default: the current folder) |
| `--limit <n>` | how many hits to show (default: 50) |
| `--count` | print the count instead of the list |
| `--json` | one JSON object per hit, for scripts |
| `--index <file>` | where to keep the name index (default: the platform's cache folder) |
| `--reindex` | rebuild the index even if a usable one exists |
| `--max-age <dur>` | rebuild an index older than this (`30m`, `2h`, `7d`) |
| `--explain` | print the query plan on stderr — what rgy did with your query |

The index is what makes the name half of the query fast. rgy keeps it
between runs and rebuilds it when it is older than `--max-age`.

The default folder is the platform's own cache folder, with an `rgy`
folder inside it: `%LOCALAPPDATA%\rgy` on Windows,
`$XDG_CACHE_HOME/rgy` (or `~/.cache/rgy`) on Linux, and
`~/Library/Caches/rgy` on macOS. Set `RGY_INDEX_DIR` to keep the index
somewhere else.

## 7. The daemon

One process can hold the index warm and answer many queries — useful if
you are searching repeatedly, or if a script is.

```sh
searchd --root ~                  # one JSON request per line on stdin
```

The root is resolved once, at startup: an absolute path, with the symlinks
resolved. That is the spelling the change feed reports, and the answers
carry it — so a path in an answer is the path to open, whatever the root
was written as.

The three requests:

```json
{"op":"search","q":"invoice ext:pdf","limit":50}
{"op":"grep","q":"regex:fn\\s+list"}
{"op":"status"}
```

```sh
echo '{"op":"search","q":"invoice"}' | searchd --root ~
```

`{"op":"status"}` answers with how much is indexed and how the index caught
up after the last restart:

```json
{"op":"status","docs":128,"event_id":0,"catch_up":"relisted"}
```

`catch_up` is `replayed` where the operating system keeps a journal of what
changed — Windows and macOS — and `relisted` where it keeps none — Linux and
FreeBSD — because there the tree is walked again instead. The field is
additive: a reader that does not know it keeps working.

It can also listen on a socket instead of stdin:

```sh
searchd --root ~ --listen uds:/tmp/rgy.sock    # Linux and macOS
searchd --root ~ --listen pipe:rgy-search      # Windows
```

`--serve` is a compatibility front for [rgx](https://github.com/danalec/rgx):
one JSON argv-array per line in, `rg --json` match lines out, framed by
`{"rgx":"begin"}` and `{"rgx":"end"}`.

## 8. The pack store

Searching a folder means reading it. Doing that again for every query is
wasteful when the folder does not change.

A **pack store** is the other shape: ingest a tree once into aggregated,
checksummed packs, and answer content queries out of the packed bytes.

```sh
rgx-tv build . --store ~/tv-store            # ingest a tree
rgx-tv search tesla --store ~/tv-store       # query it
rgx-tv update . --store ~/tv-store           # publish what changed
rgx-tv inspect --store ~/tv-store            # verify and report
```

rgy's own binaries reach the store through one flag:

```sh
searchd --root ~ --tv-dir ~/tv-store
```

That routes both the `grep` operation and the `--serve` front to the packs
instead of reading the tree. The tuning flags (engine choice, thread
count, context lines, the mapping strategy) are **not** reachable from
the daemon — it uses the defaults. Run `rgx-tv search` directly when you
want them.

| flag | what it does |
|---|---|
| `--store <dir>` | the store directory — every subcommand needs it |
| `--pack-size-mb <N>` | target pack size in MiB (default 16); `build` only |
| `--max-depth <N>` | how deep to go |
| `-g, --glob <PAT>` | include or exclude by glob, repeatable |
| `--hidden` | include hidden files |
| `-L, --follow` | follow symbolic links |
| `--no-ignore` | ignore `.gitignore` and `.ignore` |
| `--compact` | `update` only: reclaim the space of superseded payloads |
| `--engine <which>` | `auto` (default), `rgx`, `grep` or `both`; `search` only |
| `-j, --threads <N>` | search threads; `search` only |
| `-c, --count` | print the count per file; `search` only |
| `-n, --line-number` | show 1-based line numbers (on by default) |
| `-A, -B, -C <n>` | lines of context after / before / both; `search` only |
| `-a, --text` | search files the build marked binary; `search` only |
| `-M, --max-filesize <N>` | skip files bigger than N (`k`, `m`, `g` suffixes); `search` only |
| `--mmap` | map the packs instead of reading them; `search` only |
| `--no-mmap` | buffered reads — the default; `search` only |
| `--serial-buffered` | buffered reads, one pack at a time, by the coordinator |
| `--packs-per-thread <N>` | packs a worker claims per claim (default 1) |
| `--pool` | reuse one set of mappings across queries in this process |
| `--no-pool` | deprecated: the pool is off by default |
| `--leak-mmap` | skip the explicit unmap and let the exit reclaim it |
| `--validate` | check that every file the manifest claims is still there; `search` only |
| `--stats` | print the stage breakdown to stderr; `search` only |

A flag the subcommand does not own is an **error**, not a no-op: `rgx-tv
build . --store d --stats` exits 1 and says so.

## 9. When it goes wrong

**No results, and a warning on stderr.** An unknown `type:` or a filter
that does not apply fails closed. Read the warning; it names the token.

**No results, no warning.** The name is not close to what you typed. Drop
the quotes to go fuzzy, or check the spelling of the filter: `ext:pdf`, not
`ext:.pdf`.

**The same query gives different answers later.** The index was rebuilt.
`--max-age` controls how stale it may get; `--reindex` forces a rebuild
now; `--explain` shows what rgy did with the query.

**A PDF search finds nothing.** Expected today. `grep:` reads the bytes of
a PDF, which are compressed; the adapters that read inside PDFs are not
wired into the search path yet. See [FORMATS.md](FORMATS.md).

**The daemon does not answer.** It expects one JSON object per line, and
answers on stdout. `{"op":"status"}` tells you whether it is alive and
what it has indexed.

**A file cannot be read.** Permission, a broken link, a file that is
being written. rgy skips it and lists it rather than failing the run.

## 10. See also

| file | what it is |
|---|---|
| [README.md](README.md) | the map: what rgy is, how it compares to rgx |
| [FORMATS.md](FORMATS.md) | what it reads inside a file, format by format |
| [docs/design.md](docs/design.md) | the decisions this workspace is built on |
| [docs/insights-from-rgx.md](docs/insights-from-rgx.md) | what the rgx side taught about storage and access shapes |
| [benchmark/](benchmark/) | the measured numbers |
| [rgx](https://github.com/danalec/rgx) | the content-search specialist this workspace is built on |
