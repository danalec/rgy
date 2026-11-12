# The post-install choice (reference for package maintainers)

This is the canonical shape of the post-install question for package-manager
installs of rgy. Scoop (`bucket/rgy.json` in `danalec/scoop-alts`) and the
three AUR packages (`rgy-search-bin`, `rgy-search-avx2-bin`,
`rgy-search-znver4-bin`) carry a copy of it in their own repositories. The
working copies in *this* tree are the reference; keep the copies in step when
the choice changes.

## What the choice must say

1. **`searchctl` is not a systemd service and does not require systemd.** It is
   a plain CLI. The `searchd` daemon is a separate, optional binary.
2. **Ask the user** whether to install and configure the daemon right there.
   A package install never registers a login item silently.
3. **`searchctl` needs an index, not a daemon.** Index at least once:
   `searchctl --root ~ --reindex --count '*'`.
4. **`rgxy` needs a *running* `searchd`, not an index.** Indexing once does not
   make it work; with no daemon it says `no connection to the daemon`.
5. **Give a daemon-free way to try `rgxy`:** start `searchd` in the foreground
   on a temporary endpoint and point `rgxy` at it, with nothing registered.
6. **Never run the daemon and a scheduled rebuild on the same tree** — both
   write the same index file.

## The Windows distinction that matters

`rgy-autostart install` does **not** start the daemon in the current session
on Windows. It creates a Task Scheduler task with an `ONLOGON` trigger (or an
`HKCU\...\Run` value as the weaker fallback), so the daemon starts at the
*next* logon. On Linux (`systemctl --user enable --now`) and macOS
(`launchctl bootstrap`) it does start at once.

So on Windows there are genuinely three answers, and they are all useful:

| choice | what it does |
|---|---|
| `[1]` register for the next logon only | `rgy-autostart install` |
| `[2]` register **and start now** | `rgy-autostart install`, then `schtasks /Run /TN rgy-searchd` (the exact `fix:` line `rgy-autostart doctor` prints) |
| `[N]` do nothing | explain the index-vs-daemon rules above |

`install.ps1 -Activate` and the end of a normal `install.ps1` install do
option `[2]` automatically. Scoop's `post_install` offers all three.

## Rules for the Scoop implementation

The tree's `rgy-autostart` is the **only** thing that should write the login
entry. In particular:

- **Do not add a second `reg add` to `HKCU\...\Run` after calling
  `rgy-autostart install`.** `install` already wrote either a Task Scheduler
  task or an HKCU Run value. A second, hand-written value is a duplicate
  registration with a possibly different command line, and nothing removes it
  on `rgy-autostart uninstall`. If the concern is that a Scoop upgrade moves
  the binaries, register through Scoop's stable `current` symlink instead:
  pass `--binary "$dir\..\current\searchd.exe"` (or the equivalent stable
  path) to `rgy-autostart install --binary <path>`, once, and let the tool
  own the entry.
- **Do not reconstruct the pipe name by hand.** `rgy-autostart` derives it
  from the user name with the protocol's own sanitizer
  (`rgy_proto::endpoint::pipe_name`). A hand-built `pipe:rgy-searchd-<USER>`
  can disagree with the client, and then `rgxy` cannot connect.
- **Do not `Start-Process searchd.exe` on the registered pipe after
  `install`.** Use `schtasks /Run /TN rgy-searchd` (the task that `install`
  just created, carrying the exact registered command line). Running the task
  is idempotent with respect to registration; starting a second `searchd` by
  hand on the same pipe can fail to bind or race the real one.
- Use `rgy-autostart doctor` to check progress: on its first start the daemon
  rebuilds the index of the home directory before it opens its pipe, so
  `rgxy` can keep saying `no connection to the daemon` for a while.
- `rgy-autostart install --launcher` is optional; the resident launcher
  window is Windows-only. The one-line installer passes it when `rgxy.exe` is
  present. Scoop may pass it too, but it must not claim a launcher entry on
  other platforms.
- Guard any `Read-Host` prompt with `[Environment]::UserInteractive` so that
  scripted/CI installs do not hang; default to "do nothing" when it is false.

## The corrected Scoop choice, in full

The three answers are kept, but the registration is done **once** by
`rgy-autostart install` and the "start now" answer runs the task it just
created. Nothing writes a second registry value, nothing reconstructs the
pipe name, and nothing starts a second `searchd` by hand.

```powershell
info 'rgy: READ THIS. Despite the name, searchctl is NOT a systemd service and does not require systemd.'
info '  It is a plain CLI. The searchd daemon is a separate, OPTIONAL binary.'
info '  Scoop installed the binaries but did NOT register or start the daemon.'
info '  rgxy needs a RUNNING daemon; without one it says ''no connection to the daemon''.'
$choice = 'N'
if ([Environment]::UserInteractive) {
    info ''
    info '  How do you want the search daemon enabled?'
    info '    [1] Register it for the NEXT LOGON only (do not start it now).'
    info '    [2] Register it AND start it NOW in this session.'
    info '    [N] Do nothing (default).'
    info '  On its first start the daemon rebuilds the index of your home directory'
    info '  before it opens its pipe; rgxy connects when that rebuild finishes.'
    $choice = Read-Host 'Choose [1/2/N]'
} else {
    info '  (non-interactive install: not asking. Run rgy-autostart install yourself to enable it.)'
}
if ($choice -match '^[12]') {
    info '  Registering the daemon (no elevation, no dialog) ...'
    & (Join-Path $dir 'rgy-autostart.exe') install
    info '  Undo it later with: rgy-autostart uninstall'
    if ($choice -match '^2') {
        # The task install just wrote carries the exact registered command
        # line, including the pipe name the daemon and rgxy agree on.
        $run = Start-Process -FilePath 'schtasks.exe' -ArgumentList '/Run','/TN','rgy-searchd' -Wait -PassThru -WindowStyle Hidden -ErrorAction SilentlyContinue
        if ($run -and $run.ExitCode -eq 0) {
            info '  Started now. Check progress with: rgy-autostart doctor'
        } else {
            info '  No startup task to run (the weaker HKCU Run fallback was used); it will start at your next logon.'
            info '  Start it by hand with the fix: line of rgy-autostart doctor.'
        }
    } else {
        info '  Registered. It will start at your NEXT LOGON, not now.'
        info '  To start it sooner: rgy-autostart doctor, or schtasks /Run /TN rgy-searchd.'
    }
} else {
    info '  No daemon installed. Two things to know:'
    info '   - searchctl needs an INDEX, not a daemon. Index at least once:'
    info '       searchctl --root $env:USERPROFILE --reindex --count ''*'''
    info '   - Try rgxy with nothing installed: Start-Process searchd.exe -ArgumentList ''--root'',$env:USERPROFILE,''--listen'',''pipe:rgy-try''; rgxy.exe --endpoint pipe:rgy-try --query invoice'
    info '     Or just use searchctl for one question. See the README for a daily index timer.'
}
```

If the versioned Scoop path is a concern, register **once** with the stable
path instead of rewriting the registry afterwards:

```powershell
& (Join-Path $dir 'rgy-autostart.exe') install --binary (Join-Path $dir '..\current\searchd.exe')
```

## Rules for the AUR `.install` implementation

AUR `post_install()` runs as **root** during `pacman -U`. It **cannot**
register a daemon in the user's session (a systemd *user* unit belongs to the
user, not to root) and it must not try. So the AUR scriptlet asks the question
in its message and gives the user the two commands to run themselves, in
their own terminal, as their own user. It never prompts with `read` (pacman is
often non-interactive) and never writes to `/root/.config/systemd/user`.

The three AUR scriptlets are byte-identical except for their filename and the
`install=` line in their `PKGBuild`; the `apply-aur-install.sh` helper used to
distribute them adds `install='<pkgname>.install'`, regenerates `.SRCINFO`,
and stops before committing so the diff can be reviewed.
