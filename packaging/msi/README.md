# The rgy Windows MSI

A per-user Windows Installer package for rgy. It exists to close the one
gap the one-line `install.ps1` could not: nothing reversed it, and a
fresh install left `searchd` registered but **not running** in the current
session, so `rgxy` said "no connection to the daemon" until the next logon.

## What it does

The MSI reproduces the Scoop bucket's `post_install` behaviour, but as
first-class installer actions that can also be undone:

1. Installs the five binaries (`searchctl`, `searchd`, `rgy-autostart`,
   `rgxy`, `rgy`) and the seven documents into
   `%LOCALAPPDATA%\Programs\rgy` — the same directory `install.ps1` uses.
2. Prepends that directory to the **per-user** PATH (`Environment` table,
   `HKCU`, no machine-wide change).
3. Adds a Start Menu shortcut for `rgxy`.
4. Runs `rgy-autostart install --launcher`, which writes the Task
   Scheduler `ONLOGON` task (or the `HKCU\...\Run` fallback) for the daemon
   and the launcher. **No elevation, no UAC, no Windows Service.**
5. Starts `searchd` **now**, detached and hidden, with
   `--root %USERPROFILE% --listen pipe:rgy-searchd-<user>`, and runs the
   logon task with `schtasks /Run`. This is the "register AND start it now"
   default of the Scoop `post_install` (choice `[2]`).
6. On uninstall: `rgy-autostart uninstall`, remove the PATH entry, remove
   the shortcut, and let MSI remove the files. The ARP entry has an
   uninstaller, which `install.ps1` never had.

## The warning and the advanced opt-out

The install shows a **warning before it starts**: the daemon will be
launched now and its first index build of your home folder can temporarily
use **about 4 GB of RAM**; `rgxy` connects when the index is ready. The
figure is the owner's measurement on their machine, not a number measured
by this tree.

The same dialog has a checkbox, **"Start the search daemon automatically
(recommended)"**, ticked by default. Unticking it sets `STARTDAEMON=0`:
the MSI then installs the binaries, documents, PATH and shortcut but does
**not** register or start `searchd`. You can opt in later with
`rgy-autostart install`. In a silent install the same choice is a
property:

```powershell
msiexec /i rgy-0.1.10.3.msi STARTDAEMON=0 /qn
```

The `RgyRegisterDaemon` custom action is conditioned on
`NOT Installed AND STARTDAEMON="1"`, so a repair or an uninstall never
starts the daemon again.

## Why per-user

`rgy-autostart` uses a per-user Task Scheduler logon task and an `HKCU` Run
value. A per-machine MSI would write a task under the SYSTEM account and
the GUI launcher would land in session 0, where it cannot be seen. Keeping
the whole package per-user also keeps the house rule: *nothing is elevated,
nothing asks*. Windows Installer needs no admin rights for a per-user
install, so there is no UAC prompt.

## ProductVersion and the fourth number

Windows Installer **ignores the fourth field** of `ProductVersion`:
`0.1.10.0` and `0.1.10.1` are the same version to MSI, so an upgrade would
silently do nothing. The MSI therefore maps the release's fix count into
the third field:

```
release  0.1.10.3   ->   ProductVersion 0.1.103
release  0.1.10.1   ->   ProductVersion 0.1.101
release  0.1.10.0   ->   ProductVersion 0.1.100
```

`scripts/msi-version.ps1` computes it, and both the build script and the
CI job call it. A new fix release is a new, higher ProductVersion, so
`MajorUpgrade` removes the old package before installing the new one.

## The WiX version

WiX Toolset **7**, installed as a `dotnet tool` (`wix`), so the CI needs no
admin and no preinstalled SDK. WiX 7 requires the OSMF EULA to be accepted
once per machine (`wix eula accept wix7`); the build script does that on
every run, which is harmless on a fresh runner and a no-op elsewhere.
The owner explicitly authorised accepting it.

There is deliberately **no separate EXE installer**: a Burn wrapper would
double the release matrix, the sidecars, the EGM contracts, the twin pairs
and the signing surface for no additional capability.

## Files

| file | job |
|---|---|
| `Product.wxs` | the package: directories, files, PATH, shortcut, upgrade, custom actions |
| `build-msi.ps1` | harvest the release archive, compute the version, run `wix build` |
| `scripts/msi-version.ps1` | the release-version -> ProductVersion mapping |
| `post-install.ps1` | the daemon-launch step, run by the MSI as a custom action |
| `EGM.md` | the contract of this subtree |

## CI

The `msi` job in `.github/workflows/ci.yml` runs on `windows-2022`
**independently of the release matrix** — a 504 downloading a FreeBSD VM
helper must not stop a Windows installer from being built. It builds its
own `x86_64-windows` archive with `scripts/build-release.ps1` (so the MSI
packages bytes smoke-tested by that same script), calls `build-msi.ps1`,
installs and removes the MSI silently with `STARTDAEMON=0` as a smoke test,
and keeps the `.msi` plus its `.sha256` as the `rgy-msi` artifact. The MSI
is one extra release asset, not a second installer implementation: it adds
no sidecar to the twelve archives, no twin pair and no new EGM contract for
them.

## Build

```powershell
pwsh -File packaging/msi/build-msi.ps1 -Version 0.1.10.3 -Archive .dist/rgy-v0.1.10.3-x86_64-windows.zip
```

The script extracts the archive into its own stage directory, checks all
twelve members, builds the MSI into `.dist/` and validates it.

## Verify

`msiexec /i rgy-0.1.10.3.msi /L*v install.log` installs without a UAC
prompt; `rgy-autostart doctor` then reports the daemon up; `msiexec /x`
removes it and `doctor` reports nothing registered. The CI job also runs
`wix msi validate` on the built package.
