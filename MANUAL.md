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
   - [Untrusted files and the format adapters](#untrusted-files-and-the-format-adapters)
6. [The flags](#6-the-flags)
7. [The daemon](#7-the-daemon)
8. [The window](#8-the-window)
9. [The pack store](#9-the-pack-store)
10. [When it goes wrong](#10-when-it-goes-wrong)
11. [See also](#11-see-also)

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

rgy runs on Windows, Linux, macOS and FreeBSD. The parts that differ per
operating system are compiled in and chosen at build time, so one package
carries everything with it.

## 2. Install it

The release is **0.1.10.3**. It is on
[GitHub](https://github.com/danalec/rgy/releases/tag/rgy-v0.1.10.3), and the
crates are on crates.io at 0.1.10 — the release's number carries a fourth
place, the fix count of the line, and the registry takes three.

### From crates.io

```sh
cargo install rgy-search --version 0.1.10         # gives you `searchctl`
```

That is the whole install for the tool you actually type. It needs **Rust
1.89 or newer** and a **C compiler** (the allocator is C), and it drops the
binary into `~/.cargo/bin` — already on your PATH if Rust was installed the
usual way.

The other binaries are separate crates, because most people only want the
first one:

```sh
cargo install rgy-searchd --version 0.1.10       # the daemon: searchd
cargo install rgy-autostart --version 0.1.10     # the login item: rgy-autostart
cargo install rgxy --version 0.1.10            # the window: rgxy
cargo install rgy-zen --version 0.1.10           # the zen CLI: zen-rs (Windows)
```

### From the release

The archive carries the four binaries, the `rgy` alias and the documents, and
it needs neither Rust nor a compiler. The `rgy` alias is `searchctl` under
the project's name: a copy of the binary on Windows — a link there needs an
administrator — and a symlink on Linux, macOS and FreeBSD:

| platform | archive |
|---|---|
| Windows x86_64 | `rgy-v0.1.10.3-x86_64-windows.zip` |
| Linux x86_64 | `rgy-v0.1.10.3-x86_64-linux.tar.gz` |
| FreeBSD x86_64 | `rgy-v0.1.10.3-x86_64-freebsd.tar.gz` |

The release page carries a `SHA256SUMS` beside the archives, and every arch
the CI builds has one: aarch64 Windows, AVX2 and Zen 4 x86_64, static musl,
aarch64 Linux and both macOS arches are on it too. The archive
that matches the machine is the one that gets installed: the operating
system and the architecture decide it, and an aarch64 machine never gets the
x86_64 binary.

### With the one-line installer

```sh
# Windows
irm https://raw.githubusercontent.com/danalec/rgy/main/install.ps1 | iex

# Linux, macOS and FreeBSD
curl -fsSL https://raw.githubusercontent.com/danalec/rgy/main/install.sh | bash
```

The installer downloads the release archive, checks its digest, puts the four
binaries in `~/.local/bin` (Linux, macOS and FreeBSD) or
`%LOCALAPPDATA%\Programs\rgy` (Windows), adds that directory to your PATH,
and registers the daemon to start at login on Windows, Linux and macOS
(FreeBSD has no login item yet). The **launcher** gets a login entry on
Windows alone, where it is a resident floating window; on the other systems
the launcher is the terminal interface, a login entry has no terminal, and
there is no unit to write — `rgy-autostart install --launcher` says so. The
digest
is **pinned in the installer**: the installer carries the digest of the one
artifact it is allowed to install, so a substituted download is refused
before anything is unpacked. The pins are release data — the release flow
rewrites them from the archive it has just built.

An archive the release does not carry is refused too, and the installer says
which one it wanted. **This release carries every archive the CI builds** —
portable, AVX2 and Zen 4 Windows, aarch64 Windows, glibc, musl and Zen 4
Linux, aarch64 Linux, FreeBSD and both macOS arches — and the CI builds each
one on the runner that has the platform.

### The packages

| platform | package | state |
|---|---|---|
| Windows | Scoop | **out** — `scoop bucket add danalec_scoop-alts https://github.com/danalec/scoop-alts` and `scoop install danalec_scoop-alts/rgy`. The same bucket carries `rgx`, the upstream engine this repository is built on |
| Windows | winget | the manifest in this repository validates and is refreshed by every release; the submission to `microsoft/winget-pkgs` ([#474460](https://github.com/microsoft/winget-pkgs/pull/474460)) is open awaiting a moderator — `winget install danalec.rgy-search` installs nothing until it merges |
| Arch Linux | AUR | **out** — three binary packages at 0.1.10.3: `rgy-search-bin` (portable), `rgy-search-avx2-bin` and `rgy-search-znver4-bin`. They conflict, because all three install `/usr/bin/searchctl`: `yay -S rgy-search-bin` (or `paru -S`) installs one of them |
| macOS | Homebrew | the formula pins the real digests of both macOS archives; the tap is not published — `brew install danalec/rgy/rgy-search` installs nothing today |
| any | crates.io | all thirty-two crates at 0.1.10 — `cargo install rgy-search --version 0.1.10` gives you `searchctl`; `rgy-searchd`, `rgy-autostart` and `rgxy` are separate crates |

The **packages** — the winget manifest, the Scoop manifest, the Homebrew
formula and the AUR PKGBUILDs — are called **rgy-search** (or a suffixed
form such as `rgy-search-bin`), and not `rgy`, because `rgy` is taken on
crates.io (a GameBoy emulator library from 2019). The command-line tool's
crate carries the same name, so the package manager and the registry say one
thing: beside it are `rgy-searchd`, `rgy-autostart` and `rgxy`, and the
binaries keep the names they have everywhere else. The project itself is
still rgy.

### The binaries

| binary | what it does |
|---|---|
| `searchctl` | the command-line tool — the one you type |
| `searchd` | the daemon: it holds the index warm between queries |
| `rgxy` | the window: a floating launcher over the daemon |
| `rgy-autostart` | puts the daemon in place at login, and takes it out again |

| command | what it is |
|---|---|
| `rgy` | another name for `searchctl` — the same binary |

`cargo install rgy-search` gives you `searchctl` and nothing else: the
`rgy` alias is in the release archives and in what the installers unpack.

### The library

The crates are on crates.io too, all thirty-two at 0.1.10, and fifteen of them
are this project's own library:

| crate | what it gives you |
|---|---|
| `rgy-name-core` | the name index, the query filters, the ranking |
| `rgy-discovery` | the discovery seam: the walk, the change feed |
| `rgy-discovery-dispatch` | the per-platform walk selector |
| `rgy-discovery-linux` | the Linux backend: `getdents64` and inotify |
| `rgy-discovery-macos` | the macOS backend: `getattrlistbulk` and FSEvents |
| `rgy-discovery-freebsd` | the FreeBSD backend: `getdirentries` (the walk; no live feed yet) |
| `rgy-discovery-windows` | the Windows backend: the MFT walk and the USN journal |
| `rgy-engine` | the orchestration: the keep-warm index, the console |
| `rgy-proto` | the wire: the request and response types, the client |
| `rgy-launcher` | the launcher's model: the query, the rows, the actions |
| `rgy-launcher-windows` | the window on Windows, drawn with GDI |
| `rgy-pick` | handing the results to an external picker |
| `rgy-adapters` | the registry that orders the format adapters |
| `rgy-par` | the parallelism the tree carries |
| `rgy-search-ui` | the terminal interface |

```sh
cargo add rgy-name-core       # the index and the query language
cargo add rgy-proto           # the wire
cargo add rgy-discovery       # the walk, behind the seam
```

The library tree is `no_std` + `alloc`. What needs an operating system sits
behind a `std` feature, on by default; `default-features = false` is what
makes a crate linkable into something with no operating system at all. Two
crates are `std` outright, on purpose: `rgy-adapters`, because the upstream
format families it orders are `std`, and `rgy-search-ui`, because a terminal
is a machine.

### Starting the daemon at login

**A name that misleads: `searchctl` is not a systemd service, is not part of
systemd, and does not require systemd.** It is an ordinary CLI — run it, it
walks `--root`, it exits. The `searchd` daemon is a separate, optional
binary, and "starting the daemon at login" is about that binary, not about
`searchctl`. `rgy-autostart` uses a systemd **user** unit on Linux simply as
one way to start it; on Windows it uses Task Scheduler or the `HKCU` run
value, on macOS launchd, and on FreeBSD there is no login item yet. rgy
works on machines with no systemd, with systemd disabled, or with no init
at all. Nothing is installed into the system's init.

`searchctl` walks the tree it is given, so it answers on its own: the
daemon is what keeps an index warm between queries, and it is the daemon
that has to be there at login for that to happen.

`rgy-autostart` registers it, and running the command *is* the consent:
the tool never asks for elevation and never opens a dialog.

```sh
rgy-autostart install      # the daemon beside the tool, indexing your home
rgy-autostart status       # what is registered
rgy-autostart doctor       # what is registered, whether it is up, and what to do
rgy-autostart uninstall    # take it out
```

`install` needs nothing: the daemon is the `searchd` beside the tool, and
the tree it indexes is your home directory. The one-line installers do this
for you; every other way in (Scoop, AUR, an extracted archive, crates.io)
leaves it to you. Run `rgy-autostart install` once after installing by hand,
or `install.sh --activate` / `install.ps1 -Activate` — the installer's
post-install switch that does the same thing and downloads nothing. If you
would rather not run a daemon at all, [a daily index refresh](#without-the-daemon-a-daily-index-refresh)
is the `locate` / `updatedb` alternative.

What gets registered depends on the operating system: a Task Scheduler task
(or the `HKCU` run value) on Windows, a systemd **user** unit on Linux, a
launchd agent on macOS. The tool prints what it wrote, and what it wrote is
a plain text file you can read and edit.

The tree the daemon indexes is written into the entry, because the
directory a login entry starts in is not your home: it is
`C:\Windows\System32` on Windows and `/` on macOS. A daemon that inherited
that would walk the wrong tree, and would say nothing about it. To index
another tree, say so:

```sh
rgy-autostart install --args "--root /data"
```

The entry also names where the daemon listens, and that is not decoration.
The daemon's own default reads its requests from standard input, and a
login entry has no standard input: the daemon would start, walk the tree,
and exit — registered, and gone. So the registered daemon listens on a
socket (`$XDG_CACHE_HOME/rgy/searchd.sock`, or `~/.cache/rgy/searchd.sock`
on Linux and FreeBSD; `~/Library/Caches/rgy/searchd.sock` on macOS) or on a
named pipe (`\\.\pipe\rgy-searchd-<you>` on Windows). FreeBSD has no login
item yet, so this section is the login entry on Windows, Linux and macOS.
`doctor` prints the line, so a client knows where to reach the daemon:

```
platform:   linux
daemon:     /home/you/.local/bin/searchd (next to this tool)
registered: yes (systemd user unit)
entry:      /home/you/.config/systemd/user/rgy-searchd.service
running:    yes
root:       /home/you
listens:    uds:/home/you/.cache/rgy/searchd.sock
path:       /home/you/.local/bin is on the PATH
```

`doctor` also prints a `fix:` line when something is not right — the command
that repairs it.

### Without the daemon: a daily index refresh

**If you installed from a package manager and did nothing else, be precise
about what needs what:**

- **`searchctl` needs an index, not a daemon.** Run the one-shot command below
  at least once to build it; from then on `searchctl` answers with no service
  at all (the snapshot can be up to 30 minutes stale until the next rebuild).
- **`rgxy` needs a *running* `searchd`, not an index.** It is a client of the
  daemon over a socket or named pipe. Indexing once does **not** make `rgxy`
  work — with no daemon it prints `no connection to the daemon`.

`rgy-autostart install` (or `install.sh --activate` / `install.ps1 -Activate`)
installs and configures that daemon; `rgy-autostart doctor` reports what is
registered and whether it is up.

### Trying `rgxy` without installing the daemon

You can try the launcher with **nothing installed or configured**: start
`searchd` in the foreground on a temporary endpoint, point `rgxy` at it, and
stop it with Ctrl-C when you are done. No systemd unit, no login entry, no
registry value — a plain process you own:

```sh
# Linux, macOS, FreeBSD
searchd --root ~ --listen uds:/tmp/rgy-try.sock &
rgxy --endpoint uds:/tmp/rgy-try.sock --query invoice
rgxy --endpoint uds:/tmp/rgy-try.sock --tui
```

```powershell
# Windows
Start-Process searchd.exe -ArgumentList '--root', $env:USERPROFILE, '--listen', 'pipe:rgy-try'
rgxy.exe --endpoint pipe:rgy-try --query invoice
```

Stop the foreground `searchd` when finished; it wrote nothing permanent.
For one question with no daemon at all, use `searchctl --root ~ "invoice"`
instead — that is the tool that needs only an index.

The daemon is one way to keep the index fresh; the other is the one rgy shares
with `locate` / `updatedb` — a **snapshot rebuilt on a schedule**, with no
resident process. Both `searchctl` and `searchd` read and write the **same**
index file (the platform cache folder, keyed by `--root`), so the two are
interchangeable and you should run **one of them, not both**: a scheduled
`--reindex` would overwrite the daemon's live state, and a running daemon
would keep rewriting the snapshot a timer just built.

The one command that recreates the index and picks up every file created,
changed or deleted since the last build is:

```sh
searchctl --root ~ --reindex --count '*'
```

`--reindex` forces a full walk even when a usable snapshot exists; `'*'` is a
whole-path glob matching every entry (a query is required); `--count` prints
one number instead of every hit. This is rgy's `updatedb`. Without the
daemon, a later plain query reuses the snapshot for 30 minutes (`--max-age`),
then rebuilds it on demand; `--max-age 0` means *never reuse*, so every
query walks the tree again. A full walk of a large home directory is a real
cost — that is exactly what the daemon avoids.

`rgy-autostart install` is itself the **systemd integration for the daemon**:
it writes a **user** unit at
`~/.config/systemd/user/rgy-searchd.service` and arms it with
`systemctl --user enable --now rgy-searchd.service`, so the daemon starts now
and at every login. It listens on a unix socket and applies changes through
inotify as they happen; no timer is needed. `rgy-autostart status` and
`rgy-autostart doctor` report what is registered and whether it is up.

That "starts now" is Linux and macOS only. On Windows the same command
writes a Task Scheduler task with an `ONLOGON` trigger (or an `HKCU` Run
value as the weaker fallback): it registers the daemon but does **not** start
it in the current session. `rgy-autostart doctor` prints the fix —
`schtasks /Run /TN "rgy-searchd"` — and `install.ps1 -Activate` (and the end
of a normal `install.ps1` install) runs it automatically right after
registering. FreeBSD has no login item yet.

For the **scheduled** model instead, write a oneshot service and a timer in
that same user-unit directory and enable the *timer*, never the service:

```ini
# ~/.config/systemd/user/rgy-index.service
[Unit]
Description=Rebuild the rgy name index once
[Service]
Type=oneshot
ExecStart=%h/.local/bin/searchctl --root %h --reindex --count '*'
```

```ini
# ~/.config/systemd/user/rgy-index.timer
[Unit]
Description=Daily rgy index refresh
[Timer]
OnCalendar=daily
Persistent=true
[Install]
WantedBy=timers.target
```

```sh
systemctl --user daemon-reload
systemctl --user enable --now rgy-index.timer
systemctl --user list-timers rgy-index.timer     # when it last ran, and next
systemctl --user start rgy-index.service         # force a rebuild right now
```

`%h` is systemd's home-directory specifier. Change the `ExecStart` path to
`/usr/bin/searchctl` when a package manager (AUR) installed the binary
there, and to the Scoop shim directory on Windows. `Persistent=true` runs a
missed rebuild at the next boot, so a laptop that was off at 03:00 still
refreshes. `OnCalendar=` accepts any systemd calendar expression —
`*-*-* 03:00`, `weekly`, `Mon..Fri 08:00` — so "once a day" is a choice,
not a limit.

The same idea without systemd:

```sh
# cron (Linux, macOS, FreeBSD): every day at 03:00
0 3 * * * $HOME/.local/bin/searchctl --root "$HOME" --reindex --count '*'
```

On macOS use a launchd agent with `StartCalendarInterval`, and on Windows a
Task Scheduler task with a daily trigger running
`searchctl.exe --root %USERPROFILE% --reindex --count *`. Both are the same
oneshot command on a schedule; neither needs the daemon.

**After installing from a package manager.** Scoop, the AUR packages, an
extracted archive and crates.io put the binaries on the PATH but do not run
the one-line installer, so neither the daemon nor a timer is armed. Their
post-install message offers both options above. The daemon option is also
available directly from the installer after the fact, with no download:

```sh
# Linux, macOS and FreeBSD
install.sh --activate
# Windows
install.ps1 -Activate
```

`--activate` runs `rgy-autostart install` on the installed tree, is
re-runnable, and is the same action the end of a normal install takes.
`--uninstall` reverses it.

## 3. Build it

Rust **1.89** or newer — the tree's SIMD kernels use the AVX-512 target
features, and those landed in 1.89. The manifests declare the floor, so cargo
refuses an older rustc with a sentence instead of a wall of errors.

```sh
cargo build --release            # the whole workspace
cargo build --release -p rgy-search       # just the command-line tool
cargo build --release -p rgy-searchd     # just the daemon
```

The binaries land in `target/release/`. On Windows that is
`target\release\searchctl.exe`.

The pack store tool (section 9) is `rgx-tv`, and a default build already
makes it: the engine's `std` feature asks for `rgy-vendored-core/tv-runtime`, and that
feature is what carries the binary. `cargo build --release` therefore leaves
`rgx-tv` in `target/release/` as well. To build that one binary on its own:

```sh
cargo build --release -p rgy-vendored-core --bin rgx-tv --features tv-runtime
```

The release scripts build the shippable archives: `scripts/build-release.sh`
on Linux, macOS and FreeBSD, `scripts/build-release.ps1` on Windows. They gate
on clippy and on the test suite, smoke the binaries against the repository's
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

With `--json`, each hit also carries the characters the query matched, so
a script can show the same span the TUI paints:

```json
{"path":"/home/you/Documents/invoice_2024.pdf","score":211,"match":[0,1,2,3,4,5,6]}
```

The `match` array holds the offsets of the characters of the *name* that
matched, counted from the start of the name. A name that matched through a
typo, or through its reading in another script, has no `match`: the span
would be in another text.

A hit that is a folder carries `"folder":true`. The path does not say it — a
folder's path is spelled like a file's — and a script that paints a file's
icon on a folder paints the wrong picture of the thing the user is about to
open.

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
| `type:` | what kind of thing it is | `type:dir`, `type:link`, `type:image` |
| `size:` | how big it is | `size:+10m` (at least 10 MB), `size:-1k` (at most 1 KB) |
| `mtime:` | when it changed | `mtime:-7d` (in the last week), `mtime:today`, `mtime:week`, `mtime:month`, `mtime:year`, `mtime:older`, `mtime:2024-01-01` (that day), `mtime:<2024-01-01` (before it) |
| `in:` | the scope: the folder the search is limited to | `in:downloads` |
| `re:` | the path as a regular expression | `re:\.rs$` |
| `limit:` | how many hits | `limit:20` |

`in:` is also spelled `dir:` — the two are one filter, the scope. The value
is resolved to a real folder (`~` works) and the search looks only inside it.
The folder *axis*, the part of the path a word matches, is `folder:` (below).

A bare extension goes in `ext:`; `type:` names kinds.

**A word can say which part of the path it matches.** A bare word is the
file's name, which is what it has always meant. The other parts have their
own prefix:

| prefix | the word matches |
|---|---|
| `name:` | the name of the file (the default) |
| `ext:` | the extension of the name |
| `folder:` | a folder of the path |
| `attr:` | an attribute: `dir`, `file`, `link`, `hidden`, `mount` |

So `folder:documents invoice` finds the invoices under a `documents`
folder, and `attr:dir report` finds the folders named `report`. The
prefixes that are also filters (`ext:`) keep the filter's meaning unless
the word carries one of the matcher's own markers: `ext:^pdf` is "the
extension, starting with pdf", while `ext:pdf` is "the extension is pdf".

`sort:` picks the order the results come in:

| you write | rgy answers in |
|---|---|
| `sort:score` | the ranking: the best match first (the default) |
| `sort:name` | the name, A to Z |
| `sort:mtime` | the newest first |
| `sort:none` | the order the index holds |

**An unknown `type:` fails closed.** rgy warns on stderr, returns nothing,
and exits 1 — it does not quietly search everything. If you get no
results and a warning, that is the reason.

**The date filter takes a date or an age.** `mtime:2024-01-01` is the files
changed that day; `mtime:2024-01-01..2024-02-01` is the days in between;
`mtime:-7d` is the last seven days. The operator says which side of the
value you mean: `mtime:<2024-01-01` is "before that day" and
`mtime:>2024-01-01` is "after it". A date that does not exist
(`2023-02-29`) is refused, not silently ignored.

### The priority rules

Some places are worth more than others. A file in your `Documents` is worth
more than the same name in a build directory, and rgy says so without
being asked — the priority of a place is a rule.

The rules live in a file beside the index — the index is `<digest>.idx` and
the rules are `<digest>.rules`, the extension replaced, not extended — and
the index is built with them:

```
# <level> <regex> [flags]
#   level: ignored | low | normal | internal | high | highfile
#   flags: f (the files under the folder), d (the folder itself)
#          both by default
low  ^node_modules$  fd
high ^Documents$    fd
```

The regex matches a **name** — a folder's or a file's. The flags say who
the rule is for: `d` is the folder whose name matched, `f` is the files
under it. The level is what the place is worth, and it decides the order
before the name does: a file in `node_modules` never outranks the same
name in `Documents`, however well it matches.

There is no file, and the defaults are the rules: `node_modules`,
`target`, `Caches`, the dot-directories and the macOS bundles are low,
`Applications` is above normal, and a folder nobody wrote a rule about is
normal. A line that does not parse is reported and skipped: the rules that
parsed are the rules, and the broken line is said out loud with its line
number — a file with one bad line is a file with one rule missing, not a
file that says nothing.

The rules are read when the index is built, so a change to them needs a
new index (`--reindex`).

### Alternatives and wildcards

| you write | means |
|---|---|
| `report\|invoice` | "report" **or** "invoice" |
| `*.pdf` | any path ending in `.pdf`, at any depth — a folder included |
| `*scan?` | any path ending in "scan" and one more character |

A term carrying `*` or `?` is a **whole-path glob**: it has to match the path
from its first byte to its last, and an indexed path always starts with `/`.
So `*.pdf` reaches a PDF at any depth, while `scan?` on its own asks for a
path spelled exactly `scan` plus one byte — which no indexed path is. Write
`*scan?` for the name.

### Searching inside a file

Three operators look at the contents. The output is `path:line:text`, the
same shape `rg` prints.

| operator | means |
|---|---|
| `grep:` | the literal text |
| `regex:` | a regular expression |
| `sym:` | the symbols in the file, for code |

```sh
searchctl --root ~ 'grep:"quarterly revenue"'
searchctl --root ~ "regex:fn\s+list"
```

The regular expression is the tree's own engine, and that engine works on
**bytes**: literals, classes and their ranges, alternation, groups, `*`,
`+`, `?` and `{m,n}`, the anchors `^`, `$`, `\A`, `\z`, `\b` and `\B`,
and the flags `(?i)` and `(?m)`.

Two things a byte engine has no answer for are **lookaround** (`(?=...)`
and `(?<!...)`) and the **Unicode property classes** (`\p{L}`). A pattern
with either is refused, and the refusal says which pattern it was and why
it would not be taken -- a refused pattern is an error, not a search that
found nothing.

The refusal is a `warning:` line on stderr and the exit code is 1, and the
two routes say the same thing: the local search and the daemon's answer
about the same refused pattern are the same refusal, and the answer that
comes with it is empty because the pattern was never read.

A pattern of more than one word has to be **quoted**. Without the quotes
the second word is not part of the pattern: it is part of the file's
*name*, and `grep:quarterly revenue` asks for the files called revenue
that contain quarterly — one file, where the quoted form finds two.

The outer quotes are single so that the inner ones reach the engine
whole.

On a PDF or a Word document, `grep:` reads the document and not its
compressed bytes: the format adapters are in the search path, and the words
of a PDF's text layer are words the query can match. Those formats are listed
in [FORMATS.md](FORMATS.md) with their state.

### Untrusted files and the format adapters

Name queries never open a file. Content queries do, and a file an adapter
claims is **decoded in the same process** as `searchctl` or `searchd` — the
ten adapter families (archives, office, PDF, mail, media, 3D, ...) are
linked into the binary, not run in a sandbox. The bytes being decoded may
have been written by somebody else: a PDF downloaded from the internet, a zip
in a mail attachment, a document a colleague sent. Rust makes memory
corruption unlikely, but it does not stop a decoder from panicking, looping
forever, or allocating far more than the file is worth. The daemon runs as
you, so a decoder bug is a bug with your privileges.

This is the whole reason the distinction between your own notes and a
Downloads folder matters: the first is data you trust, the second is input
from strangers fed to a parser.

What is in place today:

| bound | value | what it stops |
|---|---|---|
| bytes read per file | 4 MiB (`MAX_FILE * 4`) | reading an entire disk into memory |
| archive nesting | 4 levels | an archive that contains itself forever |
| members per level | 4096 | a zip with millions of entries |
| decoder output | `OutputCap`, counted as bytes are emitted | decompression bombs |
| whole search | wall-clock budget, checked between files | a scan that never finishes |

What is **not** in place:

- **No process isolation.** There is no subprocess, no seccomp, no Landlock,
  no privilege drop around a parse.
- **No per-parse timeout.** The wall-clock budget is checked between files,
  not inside `adapter.extract`; a decoder that hangs hangs the process. The
  fuzz test harness has a watchdog, but it is a test harness, and a Rust
  thread that will not stop cannot be killed from outside it.
- **No memory ceiling on a rendering.** The `OutputCap` covers the
  compression decoders; a renderer that builds a large document is not
  bounded by it.
- `rgx-core` contains `unsafe` code in its codecs and memory-mapping paths.
  The adapter crates themselves contain none.

#### Turning the parsers off

`RGY_ADAPTERS` accepts `off`, `0`, `false`, `no`, `disabled` or `none`
(case-insensitive, surrounding spaces ignored). An unset or unrecognised
value leaves the adapters **on** — failing safe here means not silently
losing the formats you asked for.

```sh
# one command, on Linux, macOS, FreeBSD
RGY_ADAPTERS=off searchctl --root ~/Downloads --count 'grep:invoice'
```

```powershell
# Windows, per process
$env:RGY_ADAPTERS = 'off'
.\searchctl.exe --root $env:USERPROFILE\Downloads --count 'grep:invoice'
```

With the switch off, `Adapters::rendered` answers with nothing before the
registry is even consulted, so **no parser is invoked**. The content path
then reads the file's own bytes (still capped at 4 MiB) and searches those;
plain-text content search and all name queries are unchanged. What you lose
is exactly what the adapters would have added: PDF text layers, `.docx`
paragraphs, archive members, and the rest of [FORMATS.md](FORMATS.md). On a
folder of PDFs, `grep:` over the raw bytes will usually find nothing — that
is the expected, safe answer.

**The daemon is a separate process.** `searchctl --daemon` and `rgxy` ask
`searchd`, which has its own environment. If you want the whole stack off,
set the variable when you **start** `searchd`:

```sh
RGY_ADAPTERS=off searchd --root "$HOME" --listen pipe:rgy-searchd-$USER
```

Setting it only in the shell that launches `rgxy` does not reach the daemon.

For a corpus you believe may be actively hostile, no in-process switch is a
substitute for a boundary: run the search in a container, a VM, or a
separate unprivileged account with no access to anything you care about.

## 6. The flags

`searchctl` takes few flags, because the query does the work.

| flag | what it does |
|---|---|
| `--root <dir>` | the tree to search (default: the current folder) |
| `--limit <n>` | how many hits to show (default: 50; capped at 100 000) |
| `--count` | print the count instead of the list |
| `--json` | one JSON object per hit, for scripts |
| `--index <file>` | the file that holds the name index (default: `<cache>/rgy/<digest>.idx`) |
| `--reindex` | rebuild the index even if a usable one exists |
| `--max-age <dur>` | rebuild an index older than this (default `30m`; `0` never reuses) |
| `--explain` | print the query plan on stderr — what rgy did with your query |
| `--threads <n>` | how many workers to use, for the scan and for the query |
| `--pick` | hand the results to a picker and print the choice |
| `--fzf-binary <p>` | the picker to run (default: the first of `fzf` or `sk` on the PATH) |
| `--fzf-arg <arg>` | one argument for the picker, repeatable |
| `--daemon` | ask the running `searchd` instead of walking the tree yourself |
| `-h, --help` | what the command does — it prints it and stops |
| `-V, --version` | the version — it prints it and stops |

A path that begins with `~` is the home directory — in `--root`, in
`--index`, everywhere. The tree expands it and not only the shell does: a
POSIX shell expands `~` before the command runs and Windows PowerShell does
not, so `--root ~` is the home on every platform.

`--daemon` asks the daemon that is already running, and it never falls
back: if the daemon is not there, the command says so and stops. The
daemon answers about the tree it was started on, which is not the tree
`--root` names — so the two commands answer two different questions,
and the flag says which one you asked. `--count` is one of the things the
daemon cannot answer: a count taken from a page is a preview wearing a
count's clothes, so the command says that instead of guessing.

`--pick` hands the list to a picker and prints what you chose. The default
picker is `fzf` or `sk` — the two that read a list separated by NUL. A picker
named with `--fzf-binary` that cannot read one (`peco`, `fz`) gets the list
one path per line.

```sh
searchctl --root ~ --pick "invoice"           # fzf or sk, whichever is there
searchctl --root ~ --pick --fzf-binary peco "invoice"
```

The index is what makes the name half of the query fast. rgy keeps it
between runs and rebuilds it when it is older than `--max-age`.

The default folder is the platform's own cache folder, with an `rgy`
folder inside it: `%LOCALAPPDATA%\rgy` on Windows,
`$XDG_CACHE_HOME/rgy` (or `~/.cache/rgy`) on Linux, and
`~/Library/Caches/rgy` on macOS. `RGY_INDEX_DIR` moves the cache base and
the `rgy` folder still goes inside it, so the index lands in
`$RGY_INDEX_DIR/rgy/<digest>.idx`.

### The exit codes

`searchctl` follows the convention `rg` set, so a script that knows one knows
the other:

| code | what it means |
|---|---|
| 0 | the query matched something |
| 1 | the query matched nothing, the daemon reported an error, the picker failed, or a pattern was refused |
| 2 | the command could not run: an unknown option, `--daemon` with no daemon to ask, `--count --daemon` |

`rgxy` answers the same way, with two differences: a command line it cannot
make sense of is 1 there and not 2, and a write that did not reach the caller
— a closed pipe — is 2 as well.

| code | what it means |
|---|---|
| 0 | the paths were printed |
| 1 | the answer was empty, or the command line was wrong |
| 2 | the picker failed, or the output did not arrive |

### How many workers

rgy uses **four** workers to walk the tree, and as many as the machine
has cores to answer the query. Both numbers are yours to change.

```sh
searchctl --root ~ --threads 8 "invoice"
```

The flag wins over everything. Without it, the environment decides:

```sh
RGY_NUM_THREADS=8 searchctl --root ~ "invoice"
```

`RGY_NUM_THREADS` is the setting for both widths, and it applies to
every run of the program — put it in the same place you put the other
environment settings and you never think about it again. Without the
flag and without the environment, the scan uses four workers and the
query uses every core.

Four is the default for the scan on purpose: walking a tree is disk
work, and eight workers on one disk mostly queue behind each other
instead of reading any faster. The query is arithmetic, so it does want
every core.

The daemon takes the same flag — `searchd --root ~ --threads 8` — and
the same environment variable.

## 7. The daemon

One process can hold the index warm and answer many queries — useful if
you are searching repeatedly, or if a script is.

```sh
searchd --root ~                  # one JSON request per line on stdin
```

| flag | what it does |
|---|---|
| `--root <dir>` | the tree to hold the index for (default: the current folder) |
| `--index <file>` | the file that holds the name index (default: `<cache>/rgy/<digest>.idx`, the same file `searchctl` keeps) |
| `--reindex` | rebuild the index even if a usable one exists |
| `--max-age <dur>` | rebuild an index older than this (default `30m`; `0` never reuses) |
| `--threads <n>` | how many workers to use |
| `--tv-dir <dir>` | where the pack store lives |
| `--listen <where>` | answer on a socket instead of stdin (default `stdio`): `uds:<path>` or `pipe:<name>` |
| `--serve` | the rgx-compatible front: one JSON argv-array per line in, `rg --json` match lines out |
| `-h, --help` | what the daemon does — it prints it and stops |
| `-V, --version` | the version — it prints it and stops |

A path that begins with `~` is the home directory, in `--root`, in `--index`
and in `--tv-dir` alike.

The root is resolved once, at startup: an absolute path, with the symlinks
resolved. That is the spelling the change feed reports, and the answers
carry it — so a path in an answer is the path to open, whatever the root
was written as.

The five requests:

```json
{"op":"search","q":"invoice ext:pdf","limit":50}
{"op":"grep","q":"regex:fn\\s+list"}
{"op":"grep-scoped","q":"tesla","paths":["/home/you/Documents/invoice.pdf"]}
{"op":"resolve","path":"/home/you/Documents/invoice.pdf"}
{"op":"status"}
```

`grep` searches the whole tree. `grep-scoped` searches **only** the paths it
is given — the files a name search already chose, and the question is what
is inside them. It answers to two names: `grep-scoped` and `content` are
the same request, and a client may send either. Its `q` and its `paths` are
both required — a request that names no scope is refused, never widened
into a search of the whole tree, because a scope that is quietly dropped is
a search that answers with the whole tree and says nothing about it.

```sh
echo '{"op":"search","q":"invoice"}' | searchd --root ~
```

A search answers with the hits it ranked, and with two facts about the
answer:

```json
{"op":"hits","hits":[{"path":"invoices/2026-07.pdf","score":42}],"took_us":1234,"complete":true,"session":4523615625714364}
```

A hit carries the path and the score, and two fields that are there only when
they say something:

```json
{"path":"invoices/2026-07.pdf","score":42,"span":[0,7],"folder":true}
```

`span` is the character positions of the **name** that matched, so an interface
can underline what matched; `folder` says the hit is a folder, which the path
does not say — a folder's path is spelled like a file's. A hit without `span`
has no highlight to offer, and a hit without `folder` is a file.

`complete` says the answer is whole. The rule is the number of hits: fewer
hits than the limit is the whole answer, and a list that reached the limit
may have been cut short. It is additive — a reader that does not know it
keeps working, and a line without it reads as "not complete", the answer
that promises the least.

The limit is capped at **100 000** hits, and a request that asks for more
is clamped to the cap: the daemon holds the whole index, and a request for
the whole index is a request to make it materialise the whole index. A
request that names no limit at all gets 50. A clamped limit is not a lie —
the answer says whether it was cut short, so a caller that asked for a
million and got the cap knows the answer is the first hundred thousand and
not all of them.

`session` names the answer, and it is the number the next request carries
back. The daemon keeps the result set under it, so a query that narrows
the previous one is answered by re-ranking that one instead of walking the
index again. The daemon names the session when the request does not, and
the answer says which session it was computed for: a client that cannot
name the session it is in cannot ask for the re-rank, and a session the
client cannot name is a session the client cannot use.

The two spaces cannot meet. The daemon's own sessions are in
`[2^52, 2^53)` and the ids a client names are below `2^52`, so the two
kinds of session never land on each other by accident. The session in the
example above is one of the daemon's own: `4523615625714364` is in the
daemon's space, and it is the kind of number the daemon puts on the wire.

The ids the daemon names are **unguessable**, and that is what makes the
round trip safe. The name is a capability: the client that holds it asks
for the re-rank of the answer the daemon keeps under it. A client that
names an id in the daemon's space that the daemon did not issue is
refused one and given a fresh session instead — a client cannot land on
somebody else's result set, because it cannot guess the name of one. The
ids used to come out of a counter, and a counter cannot be a capability:
the second client could name the first client's id, so the daemon refused
every id in its own space, including the ones it had just issued, and the
name on the wire was one the daemon would not take back.

Both spaces are under `2^53` for a second reason: the protocol carries a
number as an `f64`, and an integer above `2^53` does not survive the trip
exactly. The limit is `2^53` itself, inclusive — a power of two is exactly
representable — and a session that breaks the rule is **no session at
all**. The reader drops three kinds: a negative id is not an id, neither
is a number that reads as one above the limit, and neither is a number
that is not a whole one — the cast truncates, and a peer that sent `1.5`
would have been read as the id `1`, a session the peer never named. The
request that carried one names no session, and the daemon answers it as a
fresh question — not an error, and not a different id: there is no
session, and the answer is the answer of a fresh search.

The content route has no session. The name search has one because the
daemon keeps the previous result set; the content answer is the files on
the disk, it is read and not kept, and there is nothing to re-rank.

`{"op":"resolve"}` answers with the path of an entry, whether it is a folder,
and the volume that owns it:

```json
{"op":"resolved","path":"/home/you/Documents/invoice.pdf","folder":false,"backend":0}
```

A path the index does not have is an error, not a wrong answer.

`{"op":"status"}` answers with how much is indexed and how the index caught
up after the last restart:

```json
{"op":"status","docs":128,"event_id":0,"catch_up":"relisted","tv":false,"feed":"live"}
```

`catch_up` is what this process did, not what the platform could do:
`replayed` when the change journal was resumed at the position the index
carries and the backlog applied, and `relisted` when the tree was walked
again instead. That is the answer on Linux and FreeBSD, where no journal
exists — and also on a Windows machine where the journal is there and this
process was not allowed to open it. The field is additive: a reader that
does not know it keeps working.

`feed` says whether the tree is still being followed: `live` while the
feed runs, `gone` when it died — the daemon answers on a snapshot from
then on — and `none` when there is no feed. `tv` says whether the daemon
has a pack store, which is the corpus packed once and mmapped. Both are
additive, and a reader that does not know them reads the status as it
was before they existed.

#### Watching the index being built, and asking its state

There are three ways to see what indexing is doing, and they answer three
different questions.

**1. While it starts (stderr).** `searchd` writes its progress to stderr, so
it never pollutes the answers on stdout. On a machine with no usable saved
index it walks the tree:

```
searchd: walking the tree with the linux backend
searchd: scanning… 899 dirs, 221021 entries
searchd: indexed 3 entries → /home/you/.cache/rgy/<hash>.idx
searchd: following the tree with inotify
```

`scanning…` is printed every 500 ms during the walk, with a real tally of
directories listed and entries seen — it is the sign that a long first run is
working, not hung. `indexed N entries → <path>` is the moment the snapshot is
saved. With a reusable saved index the walk is skipped:

```
searchd: reusing index /home/you/.cache/rgy/<hash>.idx (3 entries, built 4m) — no change journal: this platform keeps no change history, so the changes made since the index was built are not in it
searchd: following the tree with inotify
searchd: the tree was walked again and diffed
```

The daemon only binds its socket **after** the index is ready, so a client
that connects is talking to a daemon whose index is built. On Linux and
FreeBSD the tree is walked again and diffed at every start (there is no
journal to resume); on Windows it resumes the USN journal at the watermark
the index carries when the journal still covers it.

**2. `rgy-autostart doctor` — is it up?** This is the quick check: it reports
`registered`, `running: yes/no`, the `root` the registered command indexes,
and the `listens:` endpoint, with a `fix:` line when something is wrong. It
opens and closes a connection, sends no request, and changes nothing — so it
tells you the daemon is reachable, **not** how much it has indexed.

**3. `{"op":"status"}` — what has it indexed?** Send that one JSON line to
the daemon's endpoint and read one line back. The `docs` field is the live
count (base index minus dead entries plus the overlay), `event_id` is the
journal position the index is current as of, `catch_up` is `replayed` or
`relisted`, `feed` is `live` / `gone` / `none`, and `tv` says whether a pack
store backs content search. On Linux/FreeBSD, with no `socat` or `nc`
installed, a Python one-liner is enough — use the path `doctor` printed on
its `listens:` line:

```sh
python3 -c 'import socket; s=socket.socket(socket.AF_UNIX); s.connect("/home/you/.cache/rgy/searchd.sock"); s.sendall(b"{\"op\":\"status\"}\n"); print(s.recv(4096).decode().strip())'
```

On Windows the daemon listens on a named pipe, so the same question goes
through the client library (or `rgxy`), which knows the pipe's name. The
`rgxy` launcher asks `status` itself: it draws `docs` in its header and warns
`the tree is no longer followed by the daemon` when `feed` is `gone` and
`the daemon does not follow this tree: the index is a snapshot` when it is
`none`.

A `feed: "gone"` is the state to act on: the daemon is still answering, but
from a frozen snapshot. `rgy-autostart doctor` will show `running: yes` while
the tree is no longer followed — only the `status` frame says so.

A content search answers with the files it read and says whether it read
all of them:

```json
{"op":"grep-files","files":[{"path":"/home/you/notes.txt","lines":[[12,"invoice"]]}],"took_us":1234,"complete":true,"skipped":0,"skips":[]}
```

`complete` says the search read every candidate it was given, and
`skipped` says how many files it did not read. A truncated search and a
whole one are different answers, and the difference used to be invisible.
The two fields are additive: a reader that does not know them sees the
files and the time, exactly as before.

`skips` says **why** the files were not read, and it is the count that
turns a number into an answer — a budget that ran out and a file that is a
cloud placeholder are different problems, and the reader can only fix one
of them:

```json
{"op":"grep-files","files":[],"took_us":8,"complete":false,"skipped":3,"skips":[{"reason":"a cloud placeholder; reading it would download it","count":2,"path":"C:/a.txt"},{"reason":"the search's budget ran out before it reached them","count":1}]}
```

The counts in `skips` add up to `skipped`. A group with one file carries
the file's path, and a group of files the search never reached does not —
there is no single file to name. The field is additive, like the two
beside it.

A stream of hits ends with a terminator:

```json
{"op":"done","count":1}
```

`count` is the number of entries the answer carried — the hits on the name
stream, the files on the content stream. The frame is the end of the
stream, and it is the only thing that says there is nothing more coming: a
client that does not know it cannot tell a finished answer from a partial
one. A `status` and a `resolve` answer on their own frame — there is one
thing in them — and an `error` ends the stream too.

The count is a count of things, so it is a whole number, and a count the
reader cannot vouch for is refused: a frame that carries `1.5` names a
number that cannot be true, and the cast that would have read it as `1`
would have reported a count the peer never stated. A terminator without a
count is refused for the same reason — the reader has no slot for "not
known" in a `usize`, and a zero would be a claim the peer never made.

A refusal travels with the answer as a frame of its own, and it comes
before the answer it belongs to:

```json
{"op":"warning","message":"unknown type: pdf"}
```

`warning` is a fail-closed refusal: an unknown `type:`, a `re:` that does not
compile. The alternative it belongs to matches nothing, and the daemon keeps
serving — the warning is part of the answer, not the end of it. The frame is
additive: a reader that does not know it reads the answer and finds nothing
where the refusal was, which claims less than the answer holds.

### What the answer says about itself

An answer that did not read everything says so, in the answer. There are two
holes, and the sentence names which one it is:

| the hole | the sentence |
|---|---|
| the daemon did not read some files — a budget ran out, or a file was a cloud placeholder | `the daemon did not read N file(s): the answer is partial` |
| the list was longer than the limit | `the answer stops at the limit` |

`searchctl --daemon` prints the sentence on stderr, and the window shows
it under the field. The two layers say the same words about the same answer,
because an answer that does not say so looks whole.

The reasons travel with the answer as well, in `skips`, and **both** say
them — the command line and the window: a file that is a cloud placeholder
and a budget that ran out are different problems, and "the answer is
partial" on its own does not say which one of them the reader can fix.

The daemon's own warnings travel with the answer, and they are said on every
path — the scoped search, the pack store's route and the walk's route — so a
search that could not read everything says so whichever route it took. A
query rgy cannot make sense of fails closed: an unknown `type:` is a
`{"op":"warning",…}` frame on the wire, and the daemon keeps serving — the
warning is part of the answer, not the end of it. On the command line the
same refusal is a `warning:` line on stderr, no matches, and exit 1. Neither
of them searches everything.

### The live feed

The daemon follows the tree while it runs, and the line it prints at startup
says how:

```
searchd: following the tree with the USN journal
searchd: following the tree with the tree's directory notifications
searchd: following the tree with inotify
searchd: following the tree with FSEvents
```

and, where the platform has no feed at all:

```
searchd: no live feed for this tree: the index is a snapshot until the next rebuild
```

FreeBSD is in the last group: the `getdirentries` walk is its native backend,
and kqueue is not wired, so its index is a snapshot until the next rebuild.

On Windows the two fast paths — the USN journal and the MFT walk — need a
volume handle, and a volume handle needs the rights an elevated process
has. A daemon that is not elevated is refused the journal, and the feed
falls back to the tree's own directory notifications: the line says which
one it is. The MFT walk falls back to the portable walk the same way.

The daemon also says when it could not bring an index up to date, and names
the cause rather than the platform:

```
searchd: reusing index /home/you/.cache/rgy/caf45dc303365b6b.idx (128 entries, built 3m ago) — no change journal: the volume would not open; a volume handle on Windows needs the rights an elevated process has (win32 error 5), so the changes made since the index was built are not in it
```

The `status` answer's `feed` field says the same thing on the wire:
`live` while the feed runs, `gone` when it died, `none` when there is none.

It can also listen on a socket instead of stdin:

```sh
searchd --root ~ --listen uds:"$HOME/.cache/rgy/searchd.sock"   # Linux, macOS and FreeBSD
searchd --root ~ --listen pipe:rgy-search      # Windows
```

The socket is created `0600`, the user's alone: a socket another local user
can connect to is a daemon that answers somebody else's questions, so the
path should be one only you can reach. `$HOME/.cache/rgy` is that place;
`/tmp` is not, and a socket there is narrowed to `0600` but still in a
directory anyone can write to.

`--serve` is a compatibility front for [rgx](https://github.com/danalec/rgx):
one JSON argv-array per line in, `rg --json` match lines out, framed by
`{"rgx":"begin"}` and `{"rgx":"end"}`.

## 8. The launcher, and its window

`rgxy` is the daemon's **client**. The index lives in `searchd`, and a
keystroke is a question sent down a pipe — which is why the launcher is
there in a few milliseconds: it is not building an index, it is asking
one that is already built.

```sh
rgxy                  # the launcher: resident window on Windows (Control twice); TUI elsewhere
rgxy --query invoice  # one search, printed, and out
rgxy --tui            # the terminal interface (the launcher on Linux, macOS and FreeBSD)
```

| flag | what it does |
|---|---|
| `--query <text>` | one search: prints the paths and leaves |
| `--limit <n>` | how many results (default: 50) |
| `--tui` | the terminal interface, instead of the window |
| `--endpoint <e>` | where the daemon is: `pipe:<name>`, `uds:<path>` or `stdio` |
| `--show` | opens the window right away |
| `--no-hook` | does not install the global shortcut |
| `--pick` | hands the results to a picker and prints the choice |
| `--fzf-binary <p>` | the picker to run — `fzf` or `sk` by default |
| `--fzf-arg <arg>` | one argument for the picker, repeatable |
| `-h, --help` | what the launcher does — it prints it and stops |
| `-V, --version` | the version — it prints it and stops |

Without `--endpoint` the launcher asks the daemon this machine has
registered: the socket on Linux, macOS and FreeBSD, the named pipe on Windows.

### The settings

Everything the launcher looks like is one file: `settings.tsv`, in the
same folder as the index (`%LOCALAPPDATA%\rgy` on Windows, `~/.cache/rgy`
elsewhere). The panel writes it and a hand can too: a file edited by hand
is read on the next start, and the file the panel writes is the same one.

Type `>settings` in the bar: `↑`/`↓` choose the row, `←`/`→` and `Enter`
adjust it, `Esc` closes — and every other key is the field's, so typing
closes the panel and lands. An adjustment is worn at once: the theme, the
width, the seat, the glass, the key. The last row restores the defaults.

The keys the file knows:

| key | what it holds |
|---|---|
| `overlay.theme` | the window's template: `warm`, `midnight`, `dark-cool`, `light`, `high-contrast`, `nord` |
| `overlay.width` | how wide the window is, in model units (400–1000, default 600) |
| `overlay.rows` | how many rows the list shows (1–30, capped by the screen; default 12) |
| `overlay.x`, `overlay.y` | where the window sits on the work area, in thousandths (default 500 and 180) |
| `overlay.alpha` | how opaque the panel is (64–255, default 230) |
| `overlay.colour.<role>` | one role over the template: `background`, `text`, `dim`, `accent`, `matched`, `good`, `selected`, `border`, as `#rrggbb` |
| `hotkey` | the key that opens the window |
| `tui.theme` | the terminal's template, with `tui.colour.<role>` beside it |
| `fzf.theme` | the picker's template, with `fzf.colour.<role>` beside it |
| `fzf-arg` | one argument for the picker, repeatable |

The hotkey has two shapes, and the file spells both:

```sh
hotkey    double ctrl 400    # a lone modifier, tapped twice inside 400 ms
hotkey    combo alt+space    # modifiers held with a key, the platform's own registration
```

The double tap watches `ctrl`, `alt` or `shift` — the panel cycles them,
with the window in milliseconds beside (`hotkey    double shift 350`). A
combo is a line in the file: `combo ctrl+shift+p`, the modifiers in any
order, joined by `+` or spaces. Only one shape is armed at a time, and
the panel re-arms where it stands.

The three faces have their **own** themes: the window's, the terminal's
and the picker's are three settings, so a warm window and a cool terminal
is one line each. The picker's theme becomes `fzf`'s own `--color` flags
(`fzf` only — `sk` and `peco` keep their own clothes), and the picker's
free arguments are the file's `fzf-arg` lines: they come after the
colours, and the command line's `--fzf-arg` is the last word.

A file that cannot be read is the defaults plus a sentence on the line
under the field — the launcher still comes up.

### The two faces

The launcher has two faces, and the binary is the same on every platform: a
floating graphical window on Windows — resident, where two taps of Control
open it — and the terminal interface on Linux, macOS and FreeBSD, where it is
the command you run in a terminal. **The window is Windows-only today, on
every desktop**: there is no Linux or macOS window yet, and a Linux machine
with a desktop gets the terminal interface like every other one. Both faces
carry the name `rgxy` — the window in its title, the terminal in its header.

The terminal interface is the launcher, so it is also the fallback the window
uses when a Windows session has no desktop to put a window on.

The header says how many entries the index holds, and the number is the
daemon's own: the client does not walk the tree and cannot know the size of
an index it does not have. When the daemon has not answered, the header
says **count not known** — it does not say zero, because zero is a fact
("there is nothing here") and a header that claims an empty index under a
daemon holding thousands of entries is a wrong fact.

### The language

The launcher reads the environment once, at start-up, and speaks the language
the environment asks for: `RGY_LANG` first — the program's own name for the
question — then `LC_ALL`, then `LANG`. Anything that starts with `pt` is
Portuguese; everything else is English, and a machine that says nothing gets
English. A launcher that changed its language in the middle of a sentence
would be a launcher that says two things at once, so the answer is read once
and handed down.

### The icon in the notification area

On Windows the launcher leaves an icon in the notification area — the row of
icons at the right of the taskbar — so that it can be found when the
shortcut is not remembered. The icon is a magnifying glass in the
launcher's own blue, and the launcher draws it itself.

| you do | the launcher does |
|---|---|
| click the icon | the window comes up |
| double-click the icon | the window comes up |
| right-click the icon | the menu: **Search**, **Settings** and **Quit** |

**Settings** is the panel `>settings` opens: the window comes up with the
panel already at the keyboard, so the icon is the way there for a user who
has forgotten the command.

The icon never closes the window: a double click is two clicks, and a
window that opens and closes again is a window that is not there. The
shortcut closes it, and so does `Escape`.

### The two layers

The field takes a query, and the query has two halves:

| you write | the launcher asks |
|---|---|
| `invoice` | the **name** layer: the engine ranks the names in the index |
| `grep:total` | the **content** layer: which files *say* it |
| `invoice grep:total` | both: the files called invoice, that say total |
| `regex:^inv` | the content layer, with a regular expression |

The content prefix **splits** the query. What comes before it is the
scope — the files the name layer chose — and what comes after it is the
pattern that is read inside them. With nothing before it, the whole tree
is the scope.

The launcher quotes the pattern for you. Everything after the prefix is
the pattern, all of it: `grep:total due` asks for the files that *say*
"total due", and not the files called due that say total. On the command
line the same rule is yours to carry — see
[searching inside a file](#searching-inside-a-file).

That split is the whole performance story of the second layer: a content
search over the tree reads the tree, and a content search over the
sixty-four files a name search chose reads sixty-four files.

Both layers are asked on the **window's clock** (120 ms): a keystroke
sets the question and the clock asks it, so a burst of typing is one
question and not one question per letter. The name layer is a re-rank of
a warm index and the content layer **reads files** — the clock is the
same for both, and the difference is in how long the answer takes, not
in when it is asked.

The window is up **before** the daemon is asked, and the daemon is asked
on a **thread of its own**: the thread that draws the window never waits
for the daemon, so the window keeps taking keys while the question is on
its way. The question goes down on the window's clock, and the clock only
ticks while the window is up: a daemon that takes the connection and then
says nothing does not keep the window off the screen, and it does not keep
the hotkey from working either. The field is there, the line under it says
`procurando…` while the question is on its way, and the answer lands
in a window that is already on the screen.

One question is on its way at a time. A keystroke typed while the answer
is in flight does not start a second question, and the answer that comes
back is the answer to the question that was asked: an answer for a
question the user has moved on from is dropped, and the line under the
field says which question the rows on the screen answer.

The window itself never waits. The **two seconds** are the budget of the
whole answer — the scope, the files and the fallback share it — and
it is the worker that gives up when they are gone. The client's own window
is thirty, which is right for a program that waits in the background and
wrong for a search box — thirty seconds of silence is a window that
stopped answering the keyboard.

A content query waits for the scope before it reads a
byte. A daemon that is slow says so and the launcher carries on — a
daemon that is slow is not a daemon that is gone, and the line under the
field still shows what the last answer said.

The **command line** is another thing: `rgxy --query "invoice"` waits as
long as the client always did — thirty seconds — because a program that
waits in the background can afford to and a search box cannot.

### The short filters

The launcher has a small vocabulary of its own, and it rewrites it into
the engine's:

| you write | the engine reads |
|---|---|
| `file:` | `type:file` |
| `folder:` | `type:dir` |
| `doc:` | `type:doc` |
| `pic:` | `type:image` |
| `video:` | `type:video` |
| `audio:` | `type:audio` |

The short word is a **prefix**: `doc:invoice` is the filter *and* a name
— "the documents called invoice" — and the engine reads the two as
separate words.

### The order of the results

The daemon's order is the relevance order, and it rules. On top of it the
launcher keeps what **you** used: a result you opened in the last thirty
days comes up a few places — one for a single use, two for a few, three for
many — and never more than three.

It is a nudge and not a takeover. The order underneath is still the
daemon's, and what the nudge passes goes down one place to make room: the
file you opened every morning comes first, and the three better matches it
passes are still there, just lower. The history can reorder the top of the
list; it cannot put a file the daemon ranked last on top of it.

The memory is a file of its own, read when the launcher starts and written
when a result is opened **and the open worked** — a file that could not be
opened was not used. The machine-readable form (`rgxy --query`) does
not use it: a script's output should not change because you opened a file
yesterday.

### The picker

The launcher can hand its results to **your own** picker, the same way
`searchctl` does:

| flag | what it does |
|---|---|
| `--pick` | opens the picker over the results and prints the chosen path |
| `--fzf-binary <path>` | the picker to run — `fzf` or `sk` by default; a picker that cannot read a NUL-separated list (`peco`, `fz`) gets one path per line |
| `--fzf-arg <arg>` | one argument for the picker, repeatable |

`--pick` needs `--query`: the picker chooses among the results of a search.
The chosen path goes to stdout, one per line — the same contract as
`--query`.

The default is `fzf` or `sk`: they are the ones that read a list separated
by NUL, and a path with a newline in it is no trouble to them. A picker
you name yourself and that cannot read one — `peco`, `fz` — gets the list
one path per line.

The line is one path, and a path that carries a line break cannot be one
line: it is **left out** of the output, and the launcher says on stderr
how many were left out. Half of a path is a path that does not exist, and
a reader that cannot tell the halves apart opens nothing. The rule is the
same for `--query` and for `--pick`, and it is the same shape as the rule
the picker follows with the list it is handed.

Every front-end that prints a path a script would read goes through it:
`rgxy --query`, `rgxy --pick`, `searchctl --pick`, the list `searchctl`
prints, the path the terminal interface prints when it is done, and the
paths `zen-rs` lists. A path that cannot be one line is not half a path
and not a rewritten one: it is left out, and the count says so.

A path that carries the separator the picker reads cannot be carried: it
is offered to the picker **whole**, or not at all, and never split in
half — half of a path is a path that does not exist. The launcher says on
stderr how many were left out, so a result that is missing from the list
is a result you were told about.

### The commands

Some things are not searches. A line that starts with `>` is a command:

| you write | it does |
|---|---|
| `>open` | opens the selected result |
| `>folder` | opens the folder that holds it |
| `>copy` | puts the path on the clipboard |
| `>preview` | shows the beginning of the file |
| `>menu` | the actions of the selected result |

The command is the line's **first word**: everything after it is not
read. That is what makes the commands reachable — the rows a search
found stay on the screen while the line is a command, so the search you
already typed becomes a command by putting `>copy` in front of it:
`>copy invoice` is the copy of the selected row of the search for
`invoice`. A command typed into an **empty** field has no row to act on,
and the line under it says so instead of failing in silence.

### The side bar

The actions of the selected result are a **column on the right** of the
window. The window does not grow for it: the results give up the room,
and the names get narrower while the column is there. A window with one
result is shorter than four actions, and the column says so instead of
drawing half of itself.

The column is the list of what the selected result can do, and it
**follows the row**. A folder is not previewed, and neither is an image,
a video, a sound file or a program: the preview is the head of a file as
text, and for those the head is noise. `Control+P` on one of those says
why it cannot, instead of failing in silence.

### The keys

| key | what it does |
|---|---|
| `Control` twice | opens and closes the window |
| arrows | moves the selection |
| `Enter` | opens the selected result |
| `Control+Enter` | opens the folder that holds it |
| `Escape` | closes the preview, the menu, and then the window |
| `Control+C` | copies the path |
| `Control+V` | pastes the clipboard's text into the field |
| `Control+P` | previews the file, when its head is text |
| `Control+K` | the actions |
| `Control+W` | deletes the word |
| `Control+U` | empties the field |

These are the **window's** keys. The terminal has the same ones with one
exception: `Control+C` is how a user quits a terminal, so it quits
there — and the copy is the `>copy` command, because a terminal has no
clipboard to put the path in.

A paste inside the **actions** menu is the field's: the menu closes and
the text lands in the field, with the field's own words for a clipboard
that cannot be pasted. A key that does nothing and says nothing is a
launcher the user cannot tell from a frozen one.

The window gets out of the way when you click somewhere else. A launcher
floating over the editor you just clicked into is a launcher you have to
close by hand.

### Where it runs

The window needs a desktop. A machine without one gets the terminal
interface instead, over the same daemon: the same query, the same list,
the same answers — in a terminal. The terminal says the same sentence the
window says when the daemon is not there: the two are the same interface
over the same source, and the one that keeps quiet is the one the user
cannot trust.

The terminal knows the commands and the keys as well — `>open`, `>folder`,
`>copy`, `>preview`, `>menu`, and `Control+P`, `Control+K` — and what it
cannot do, it says. It has no window to open a file in, so `>open` prints
the path, which is what `Enter` does there, and `>folder` prints the
folder that holds it; it owns no clipboard, so `>copy` prints the path
and says that there is no clipboard to put it in. `Control+C` is the one
key that is not the launcher's there: in a terminal it is how a user
quits, so it quits the session. The list moves with the arrows and with
`Control+N`: `Control+P` previews the file in the terminal as in the
window.

The window itself is a Windows thing for now. The double-`Control`
hotkey is a low-level keyboard hook, and the hook this repository has is
the Windows one; the rest of the launcher is the same everywhere.

### At login

The installers already do this: `install.ps1` and `install.sh` register the
daemon *and* the launcher, so the window is there after the next login. The
command, when you install by hand:

```sh
rgy-autostart install --launcher
```

The launcher is a program of its own, and it needs its own login entry:
the window is there in a few milliseconds because the *daemon* has the
index warm, not because the launcher built one. A launcher that started
at login would pay for the startup on every opening.

On a machine with no desktop and no terminal — a service, a login entry
with no session — the launcher has nowhere to draw and says nothing. It
leaves, and it leaves well: an entry that fails on every login is an
entry the user learns to ignore.

## 9. The pack store

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

`rgx-tv` is upstream's tool and it is **not in the release archive**: a
workspace build puts it in `target/release/` (section 3).

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
| `--engine <which>` | `auto` (default) or `rgx`; `grep` and `both` are retired names that still parse, resolve to `rgx` and say so on stderr; `search` only |
| `-j, --threads <N>` | search threads; `search` only |
| `-c, --count` | print the count per file; `search` only |
| `-n, --line-number` | show 1-based line numbers (on by default) |
| `-A, -B, -C <n>` | lines of context after / before / both; `search` only |
| `-a, --text` | search files the build marked binary; `search` only |
| `-M, --max-filesize <N>` | skip files bigger than N (`k`, `m`, `g` suffixes); `search` only |
| `--mmap` | map the packs instead of reading them; `search` only |
| `--no-mmap` | buffered reads — the default; `search` only |
| `--serial-buffered` | buffered reads, one pack at a time, by the coordinator; `search` only |
| `--packs-per-thread <N>` | packs a worker claims per claim (default 1); `search` only |
| `--pool` | reuse one set of mappings across queries in this process; `search` only |
| `--no-pool` | deprecated: the pool is off by default; `search` only |
| `--leak-mmap` | skip the explicit unmap and let the exit reclaim it; `search` only |
| `--validate` | check that every file the manifest claims is still there; `search` only |
| `--stats` | print the stage breakdown to stderr; `search` only |

A flag the subcommand does not own is an **error**, not a no-op: `rgx-tv
build . --store d --stats` exits 1 and says so.

## 10. When it goes wrong

**No results, and a warning on stderr.** An unknown `type:` or a filter
that does not apply fails closed. Read the warning; it names the token.

**No results, no warning.** The name is not close to what you typed. Drop
the quotes to go fuzzy, or check the spelling of the filter — `ext:pdf` and
`ext:.pdf` are the same filter, and the leading dot is optional.

**The same query gives different answers later.** The index was rebuilt.
`--max-age` controls how stale it may get; `--reindex` forces a rebuild
now; `--explain` shows what rgy did with the query.

**A PDF search finds its text.** `grep:` reads the document through the
format adapters, so a PDF's text layer is searched like any other file's
bytes and a `.docx` is a zip with XML inside. What the adapters do not
reach is the document's own structure — a PDF's page, a spreadsheet's cell
— which is not in the path. See [FORMATS.md](FORMATS.md).

**The daemon does not answer.** It expects one JSON object per line, and
answers on stdout. `{"op":"status"}` tells you whether it is alive and
what it has indexed.

**The answer says it is partial.** `the daemon did not read N file(s): the
answer is partial` and `the answer stops at the limit` are two different
holes — files the daemon did not read, and a list longer than the limit —
and the sentence says which one it is. See
[the daemon](#7-the-daemon).

**A file cannot be read.** Permission, a broken link, a file that is
being written. rgy skips it and lists it rather than failing the run.

## 11. See also

| file | what it is |
|---|---|
| [README.md](README.md) | the map: what rgy is, how it compares to rgx |
| [FORMATS.md](FORMATS.md) | what it reads inside a file, format by format |
| [docs/release-0.1.10.3.md](docs/release-0.1.10.3.md) | the 0.1.10.3 release page, with the changelog in it |
| [benchmark/](benchmark/) | the measured numbers |
| [rgx](https://github.com/danalec/rgx) | the content-search specialist this workspace is built on |

| link | what it is |
|---|---|
| [the 0.1.10.3 release](https://github.com/danalec/rgy/releases/tag/rgy-v0.1.10.3) | the archives, the `SHA256SUMS`, and the changelog |
| [crates.io](https://crates.io/crates/rgy-search) | the thirty-two crates, all at 0.1.10 |
| [docs.rs](https://docs.rs/rgy-name-core) | the API of each crate |
