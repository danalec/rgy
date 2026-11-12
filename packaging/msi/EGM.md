# packaging/msi

## Overview

The per-user Windows Installer package for rgy. One MSI, built by the
`msi` job in `.github/workflows/ci.yml` from an `x86_64-windows` archive
that the job builds itself with `scripts/build-release.ps1`. The job is
deliberately independent of the release matrix: a failure downloading a
FreeBSD VM helper must not stop a Windows installer from being built.
There is deliberately **no separate EXE**: a Burn wrapper would double
the release matrix, the sidecars, the twin pairs and the signing surface
without adding a capability.

Its reason for existing is the one gap `install.ps1` could not close: a
fresh install registered `searchd` for the next logon but did **not** start
it in the current session, so `rgxy` said "no connection to the daemon"
until the user signed out and back in. The MSI reproduces the Scoop
bucket's `post_install` default (register AND start now) as reversible
installer actions.

## Subdirectories & Boundaries

- One directory, five files plus this contract. The parent is the repository
  root; the two version helpers live beside the build script in this
  directory, not in `scripts/`: a `.ps1` in `scripts/` with no `.sh` twin
  makes the twins gate fail, and these are MSI-only tools, not general
  repository scripts.

## Interface

- `[build-msi.ps1]` — `-Version`, `-Archive`, `-Out`, `-SkipToolInstall`;
  extracts the archive, verifies all twelve members, computes the
  ProductVersion and ProductCode, runs `wix build`, then `wix msi validate`.
- `[msi-version.ps1]` — `0.1.10.3` -> `0.1.103`.
- `[msi-product-code.ps1]` — a stable UUIDv5 ProductCode per
  release, from the UpgradeCode namespace and the version string.
- `[Product.wxs]` — the package. `[post-install.ps1]` — the daemon step.
- `[README.md]` — the why, the build and the verify commands.

## Invariants

- `[Product.wxs:Scope]` — `perUser`, with `MSIINSTALLPERUSER=1` and
  `ALLUSERS=2`. No elevated custom action, no UAC, no Windows Service. The
  Task Scheduler `ONLOGON` task and the `HKCU\...\Run` value belong to the
  installing user; a per-machine package would put `rgxy` in session 0.
- `[Product.wxs:Version]` — the ProductVersion is **three** fields. Windows
  Installer ignores the fourth, so `0.1.10.0` and `0.1.10.1` would be the
  same version and upgrades would silently do nothing. The fix count is
  folded into the third field by `msi-version.ps1`.
- `[Product.wxs:UpgradeCode]` — `B6F1A2C4-7D3E-4A19-9C58-2E0B6D4F8A31`,
  fixed forever. `MajorUpgrade` uses it to remove the previous package.
- `[Product.wxs:ProductCode]` — derived, not `*`. A wildcard generates a
  new GUID per build; two builds of one version then share component GUIDs
  and Windows Installer refuses to remove either ("another client exists").
  Measured on this host: with a stable ProductCode, install + uninstall
  removes the directory and the PATH entry with no such refusal.
- `[Product.wxs:STARTDAEMON]` — default `1`. The checkbox on
  `RgyDaemonDlg` and the `msiexec ... STARTDAEMON=0` property both set it.
  `RgyRegisterDaemon` is conditioned on `NOT Installed AND STARTDAEMON="1"`,
  so the opt-out installs the files and PATH **without** registering or
  starting the daemon. Measured: with `STARTDAEMON=0` no `rgy-searchd` Run
  value and no task; with `1`, `searchd` runs detached on
  `pipe:rgy-searchd-<user>`.
- `[Product.wxs:RgyDaemonDlg]` — the warning is shown BEFORE the install:
  the daemon will start NOW, the first index build can temporarily use
  about 4 GB of RAM, and the checkbox is the advanced opt-out. The figure
  is the owner's measurement on their machine, not a number measured by
  this tree (the code documents ~190 bytes per name of transient buffers).
- `[post-install.ps1]` — derives its install directory from `$PSScriptRoot`,
  never from a quoted MSI property: a formatted `[INSTALLFOLDER]` ends in
  a backslash, which escapes a closing quote, and quote characters arrive
  literally. Measured: both failure modes were hit and fixed.
- `[post-install.ps1]` — idempotent and reversible. `install --launcher`
  writes the login entries; `schtasks /Run` starts them now; a detached
  `Start-Process searchd.exe` is the fallback when the mechanism was the
  HKCU Run value (Task Scheduler refused on the test host). `-Uninstall`
  removes both entries; MSI removes the files, PATH, shortcut and ARP entry.

## Side Effects

- `[Product.wxs]` — writes five binaries and seven documents to
  `%LOCALAPPDATA%\Programs\rgy`, prepends that directory to the **user**
  PATH, and adds a Start Menu shortcut.
- `[post-install.ps1]` — runs `rgy-autostart install --launcher`,
  `reg add HKCU\...\Run`, `schtasks /Run /TN rgy-searchd`, and
  `Start-Process searchd.exe --root %USERPROFILE% --listen
  pipe:rgy-searchd-<user>`. On uninstall it runs `rgy-autostart uninstall
  --launcher` and deletes the task and the Run value.
- `[build-msi.ps1]` — installs the WiX 7 `dotnet tool`, accepts the OSMF
  EULA (`wix eula accept wix7`, explicitly authorised by the owner), adds
  `WixToolset.UI.wixext`, and writes the `.msi` plus a `.sha256` sidecar.

## Verification

- Run: `cargo test --workspace --no-fail-fast --locked`
- The package's own gates: `wix msi validate
  .dist/rgy-<version>-x86_64.msi`, and
  `pwsh -File packaging/msi/build-msi.ps1 -Version <version> -Archive
  <windows-archive.zip>`.
- Local: `wix msi validate` with zero ICE errors (the ICE91 warnings are
  expected for a per-user package and say only that the files would not be
  copied to every user in a per-machine install, which this is not).
- Local: silent install (`STARTDAEMON=0`) installs 13 files, the ARP entry
  and the PATH; silent uninstall removes the directory and the PATH entry.
- Local: silent install (`STARTDAEMON=1`) starts `searchd` (verified by
  `Win32_Process`) and registers the Run value; uninstall removes them.
- CI: `msi` job builds its own Windows archive, runs the same
  install/uninstall smoke test on `windows-2022`, and keeps the `.msi` plus
  its `.sha256` as the `rgy-msi` artifact.
- `cargo test --workspace --no-fail-fast --locked`.
