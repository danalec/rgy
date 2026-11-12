#Requires -Version 5.1
<#
.SYNOPSIS
    Install rgy on Windows.

.DESCRIPTION
    Downloads the rgy release zip, verifies its SHA256, unpacks the four
    binaries into the install directory, prepends it to the user PATH and
    registers the daemon -- and the launcher -- to start at login.

    -Activate is the post-install option: it registers the daemon AND
    starts it now on Windows, and downloads nothing. A package manager
    (Scoop, an extracted archive, crates.io) puts the binaries on the PATH
    but never runs this installer, so the daemon is neither registered nor
    running after those installs -- this switch is the one command that
    turns it on. It is the same action the end of a normal install takes,
    so an install made with -NoAutostart, or an `rgy-autostart uninstall`,
    is put back with the same words and the same result. It is re-runnable:
    the registration is idempotent.

    -Uninstall takes out exactly what install put in: the login entries,
    the copied files, the alias and the user PATH entry. It removes no
    directory it did not create, and no file install did not put there.

.EXAMPLE
    irm https://raw.githubusercontent.com/danalec/rgy/main/install.ps1 | iex

.EXAMPLE
    .\install.ps1 -InstallDir C:\tools\rgy

.EXAMPLE
    .\install.ps1 -NoAutostart

.EXAMPLE
    .\install.ps1 -Activate

.EXAMPLE
    .\install.ps1 -Uninstall
#>

# The options, the digest and the refusal are the tree's own
# (scripts/lib/options.ps1 and scripts/lib/digest.ps1), so that this
# script and its .sh twin answer the same question the same way. The
# helpers live in the tree, and this script is also run from a URL -- piped
# straight into the shell, where there is no tree to read them from -- so
# they are used when they are there, and the same behaviour is spelled out
# here when they are not.
#
# The options are read from $args and not from a `param` block. A param
# block hands an argument the script does not know to PowerShell's own
# binder, which prints the whole parameter declaration and exits 1; the
# tree's refusal is one sentence and exit 2, and it is the sentence the
# .sh twin prints.
#
# The refusals -- the sentences on stderr -- are written with the newline
# the shell writes and not with the one the host would, the way
# scripts/lib/options.ps1 writes its own: the two scripts are compared
# byte for byte, and a CRLF here is a byte the bash twin does not write.

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

# The tree this installer was run from, when it was run from a checkout.
# The path is only a hint: piped into the shell there is no checkout.
$here = $PSScriptRoot

if ($here -and (Test-Path -LiteralPath (Join-Path $here 'scripts/lib/options.ps1'))) {
    . (Join-Path $here 'scripts/lib/options.ps1')
} else {
    # The helper's own loop with the table lookup spelled out, for the run
    # that has no tree to read the helper from. The refusal is the same
    # sentence and the same exit code.
    function Read-RgyOptions {
        param(
            [hashtable]$Switches = @{},
            [hashtable]$Values = @{},
            [System.Collections.IDictionary]$Positionals = @{},
            [object[]]$Arguments = @()
        )

        # The refusal is written with the newline the shell writes, and not
        # with the one the host would: the two scripts are compared byte
        # for byte, and a CRLF here is a byte the bash twin does not write.
        function Write-RgyRefusal([string]$text) {
            [Console]::Error.Write("$text`n")
        }

        $at = 0
        while ($at -lt $Arguments.Count) {
            $written = [string]$Arguments[$at]
            $name = $written
            $value = ""
            $haveValue = $false
            # -Name:value carries the value with it.
            if ($written -match '^(?<head>[^:]+):(?<tail>.*)$') {
                $name = $Matches["head"]
                $value = $Matches["tail"]
                $haveValue = $true
            }
            # The name with the dashes and the case off.
            $key = ($name -replace '^-{1,2}', '' -replace '[-_]', '').ToLowerInvariant()
            if ($Switches.ContainsKey($key)) {
                if ($haveValue) {
                    Write-RgyRefusal "unknown option: $written"
                    exit 2
                }
                Set-Variable -Name $Switches[$key] -Value $true -Scope 1
                $at++
            } elseif ($Values.ContainsKey($key)) {
                if (-not $haveValue) {
                    if ($at + 1 -ge $Arguments.Count) {
                        Write-RgyRefusal "option $written needs a value"
                        exit 2
                    }
                    $value = [string]$Arguments[$at + 1]
                    $at++
                }
                Set-Variable -Name $Values[$key] -Value $value -Scope 1
                $at++
            } else {
                # Not an option, and not a name this script knows. The
                # installers take no positional argument, so every one of
                # them is a name the script does not know.
                Write-RgyRefusal "unknown option: $written"
                exit 2
            }
        }
    }
}

if ($here -and (Test-Path -LiteralPath (Join-Path $here 'scripts/lib/digest.ps1'))) {
    . (Join-Path $here 'scripts/lib/digest.ps1')
} else {
    # The same answer the helper gives, for the run that has no tree to
    # read the helper from: the lowercase hex of the digest.
    function Get-RgySha256([string]$path) {
        (Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash.ToLower()
    }
}

# The options, and their defaults. The names are the tree's: the long
# spelling and the PowerShell one are the same option, and an option
# neither of them knows is refused with the tree's words and the tree's
# exit code.
$InstallDir = "$env:LOCALAPPDATA\Programs\rgy"
$BaseUrl = 'https://github.com/danalec/rgy/releases/download'
# The tag of the release that ships the binary archives: the crates carry
# three numbers (0.1.10) and the release carries a fourth (0.1.10.3), which
# is the fix count of the line. An installer that names another tag downloads
# another release's archives and refuses them on the digest pinned below.
$Version = 'rgy-v0.1.10.3'
$SkipChecksum = $false
$NoAutostart = $false
$NoLauncher = $false
$Activate = $false
$Uninstall = $false

Read-RgyOptions -Switches @{ "skipchecksum" = "SkipChecksum"; "noautostart" = "NoAutostart"; "nolauncher" = "NoLauncher"; "activate" = "Activate"; "uninstall" = "Uninstall" } `
                -Values   @{ "installdir" = "InstallDir"; "baseurl" = "BaseUrl"; "version" = "Version" } `
                -Positionals @{ } `
                -Arguments $args

# -Uninstall takes out exactly what install put in.
#
# The install is four things: the login entries, the binaries and documents
# copied into the install directory, the alias, and the user PATH entry. The
# uninstall reverses those four and nothing else: it never removes the
# install directory itself, because that directory belongs to the user.
#
# The autostart tool removes both entries in one call -- the daemon's and
# the launcher's -- and says so in its own words. A missing tool is not an
# error: an install with -NoAutostart wrote no entries to remove.
function Remove-RgyInstallation {
    param([string]$Dir)

    $tool = Join-Path $Dir 'rgy-autostart.exe'
    if (Test-Path -LiteralPath $tool -PathType Leaf) {
        $before = $ErrorActionPreference
        $ErrorActionPreference = 'SilentlyContinue'
        try {
            & $tool uninstall
            $code = $LASTEXITCODE
        } catch {
            $code = 1
        } finally {
            $ErrorActionPreference = $before
        }
        if ($code -eq 0) {
            Write-Host "The login entries were removed."
        } else {
            [Console]::Error.Write("The login entries could not be removed (see above).`n")
        }
    } else {
        Write-Host "No rgy-autostart in $Dir; no login entries to remove."
    }

    $removed = 0
    foreach ($f in @('searchctl.exe', 'searchd.exe', 'rgy-autostart.exe', 'rgxy.exe', 'rgy.exe',
                     'README.md', 'MANUAL.md', 'FORMATS.md', 'COPYING', 'NOTICE',
                     'LICENSE-APACHE', 'LICENSE-MIT')) {
        $p = Join-Path $Dir $f
        if (Test-Path -LiteralPath $p) {
            Remove-Item -LiteralPath $p -Force
            $removed++
        }
    }
    $docs = Join-Path $Dir 'docs'
    if (Test-Path -LiteralPath $docs -PathType Container) {
        Remove-Item -LiteralPath $docs -Recurse -Force
    }
    Write-Host "Removed $removed rgy files from $Dir."

    # The exact entry install prepended, and only that one. The user PATH
    # is split on ';' and compared entry by entry, because a substring
    # check would also match a directory whose name merely starts with it.
    $userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
    if ($userPath -and (($userPath -split ';') -contains $Dir)) {
        $entries = @($userPath -split ';' | Where-Object { $_ -and $_ -ne $Dir })
        [Environment]::SetEnvironmentVariable('Path', ($entries -join ';'), 'User')
        Write-Host "Removed $Dir from the user PATH (open a new terminal to pick it up)."
    } else {
        Write-Host "$Dir is not on the user PATH."
    }
    Write-Host "Uninstalled rgy from $Dir."
}

# Register and start the daemon on an install that is already in place.
#
# This is the post-install activation and the last step of a normal install,
# and it is ONE function so the two answer the same question the same way:
# no download, no PATH edit, no copy -- just `rgy-autostart install`. The
# tool is looked for next to where the install put it first, and on the PATH
# second, because a package manager put the binaries on the PATH and not in
# $InstallDir, and that install still has a daemon to activate.
#
# `rgy-autostart install` writes the login entry. What it does NOT do on
# Windows is start the daemon in THIS session: a Task Scheduler task with an
# ONLOGON trigger, or an HKCU Run value, only runs at the next logon. Linux
# (`systemctl --user enable --now`) and macOS (`launchctl bootstrap`) do
# start it at once. So after a successful install this function runs the
# task with `schtasks /Run` on Windows -- the same command `rgy-autostart
# doctor` prints as its `fix:` -- so the daemon is up now too. The start is
# best-effort: a machine with no Task Scheduler task (the RegistryRun
# fallback) simply starts at the next logon, and the install still succeeds.
# Running this command IS the consent -- no prompt, no elevation.
#
# The function returns the tool's exit code; a normal install ignores it and
# -Activate propagates it, because activation is the whole of that action.
function Invoke-RgyRegisterDaemon {
    $exe = Join-Path $InstallDir 'rgy-autostart.exe'
    $tool = $null
    if (Test-Path -LiteralPath $exe -PathType Leaf) {
        $tool = $exe
    } else {
        $found = Get-Command 'rgy-autostart.exe' -ErrorAction SilentlyContinue
        if ($found) { $tool = $found.Source }
    }
    if (-not $tool) {
        [Console]::Error.Write("rgy-autostart was not found: install rgy first, then -Activate it.`n")
        return 1
    }

    # The launcher is asked for only when it is actually on the machine: an
    # entry that points at a binary that is not there is an entry that fails
    # at every login. The autostart tool decides whether the platform has
    # that entry (Windows only) and says why when it does not.
    $launcher = Join-Path $InstallDir 'rgxy.exe'
    $haveLauncher = Test-Path -LiteralPath $launcher -PathType Leaf
    if (-not $haveLauncher) {
        $foundLauncher = Get-Command 'rgxy.exe' -ErrorAction SilentlyContinue
        if ($foundLauncher) { $haveLauncher = $true }
    }
    $autostartArgs = @('install')
    if ((-not $NoLauncher) -and $haveLauncher) { $autostartArgs += '--launcher' }

    # The call is made with the error preference that does not turn a
    # failing native command into a terminating error, and the answer is
    # read from $LASTEXITCODE -- the way the .sh twin reads it. Out-Host
    # shows the tool's own sentences without letting them become the
    # function's return value; only the integer code comes back.
    $code = 1
    $before = $ErrorActionPreference
    $ErrorActionPreference = 'SilentlyContinue'
    try {
        & $tool @autostartArgs | Out-Host
        $code = $LASTEXITCODE
    } catch {
        # The tool is not there, or the platform refused to start it.
        $code = 1
    } finally {
        $ErrorActionPreference = $before
    }

    if ($code -eq 0) {
        Write-Host "The daemon starts at login. To take it out: rgy-autostart uninstall"
        if ($autostartArgs -contains '--launcher') {
            Write-Host "The launcher starts at login too: Control twice opens the window."
        }
        # Windows: `install` registered a logon task but did not start it in
        # this session. Run it now, so rgxy works immediately instead of
        # after the next logon. This is exactly the `fix:` line of
        # `rgy-autostart doctor`. It is best-effort: on a machine whose
        # mechanism was the HKCU Run fallback there is no task to run, and
        # the daemon simply starts at the next logon.
        $run = Start-Process -FilePath 'schtasks.exe' -ArgumentList '/Run','/TN','rgy-searchd' -Wait -PassThru -WindowStyle Hidden -ErrorAction SilentlyContinue
        if ($run -and $run.ExitCode -eq 0) {
            Write-Host "The daemon was started now too; it is indexing before it opens its pipe."
        } else {
            Write-Host "No startup task to run now (the daemon starts at your next logon). Start it by hand with: rgy-autostart doctor"
        }
    } elseif ($code -eq 2) {
        [Console]::Error.Write("The entries were written but the platform could not arm them (see above).`n")
    } else {
        [Console]::Error.Write("The daemon was not registered. To register it: rgy-autostart install`n")
    }
    return $code
}

if ($Uninstall) {
    Remove-RgyInstallation -Dir $InstallDir
    exit 0
}

# -Activate is a post-install action, so it never reaches the download. It
# works on an install made by a package manager, by an extracted archive, by
# crates.io, or by this installer with -NoAutostart: the binaries are on the
# PATH already, and the only thing left to do is turn the daemon on.
if ($Activate) {
    Write-Host "Activating the rgy daemon on the install that is already here ..."
    $code = Invoke-RgyRegisterDaemon
    exit $code
}

# The digests of the archives this installer is allowed to install.
#
# The digest is uppercase hex, and it is rewritten in place by
# scripts/publish-release.ps1 from the real SHA256SUMS on the day the
# release is cut -- so the shape of these lines is part of the contract,
# not a formatting choice. Both are real: each Windows archive was built
# and smoked on the runner that has its architecture. The installer still
# refuses a zero placeholder by name before it downloads anything, so a
# future release whose archives are not built yet cannot hand out
# something it cannot vouch for. A digest left over from an earlier tag
# would refuse every download of the new one.
#
# There is one digest per architecture, because the architecture decides
# the archive: an aarch64 machine does not run the x86_64 binary without
# an emulator, and an installer that hands it one installs something that
# will not start.
$expected = @{
    'x86_64-windows.zip' = '889C7A4F2FAD413A4E7E950E9121668BECF88CD98B282DCD013938948BD45BF9'
    'aarch64-windows.zip' = '6FE25CC453CE2A7DC688A2373A031A48D7DD143A4C277BC3A58416D8F89DD5DC'
}

# The digest of an archive nobody has published yet.
$placeholder = '0000000000000000000000000000000000000000000000000000000000000000'

# The architecture decides the archive. The release carries a portable
# build for each one it targets, and nothing else: the AVX2 and the Zen 4
# archives are for a caller that knows its own CPU.
#
# PROCESSOR_ARCHITEW6432 is the 64-bit architecture seen from a 32-bit
# PowerShell, and it is the one that answers when the host is 64-bit.
#
# The name the machine gives is not the name the release gives: Windows
# says AMD64 and the release says x86_64. The message says the release's
# name, because install.sh -- the unix twin of this script -- prints that
# one, and a pair of scripts that answer the same question in two
# different words is a pair nobody can compare.
$arch = [Environment]::GetEnvironmentVariable('PROCESSOR_ARCHITEW6432')
if (-not $arch) { $arch = [Environment]::GetEnvironmentVariable('PROCESSOR_ARCHITECTURE') }
switch ($arch) {
    'AMD64' { $assetName = 'x86_64-windows.zip'; $archName = 'x86_64' }
    'ARM64' { $assetName = 'aarch64-windows.zip'; $archName = 'aarch64' }
    default {
        [Console]::Error.Write("there is no rgy release for this architecture: $arch`n")
        exit 1
    }
}
$asset = "$Version-$assetName"
$digest = $expected[$assetName]

# The pin is the digest of the archive this release carries, and the check
# below refuses a download that does not match it: an installer that hands
# out an archive it cannot vouch for is worse than one that says there is
# none.
#
# A platform whose archive this release does not carry would hold the
# placeholder digest, and that is refused by name, before anything is
# downloaded. The sentence names the platform and the architecture, the way the unix twin
# does: the architecture alone is a lie on some of them -- there is no
# published rgy release for x86_64 macOS, and there is a published one for
# x86_64 Linux, and a sentence that said only "x86_64" would be wrong on
# one of the two.
#
# A caller that asked for no digest check is asking for exactly that, and
# the flag is theirs to use.
if (-not $SkipChecksum -and $digest -eq $placeholder) {
    [Console]::Error.Write("there is no published rgy release for Windows $archName yet`n")
    [Console]::Error.Write(" expected $asset`n")
    exit 1
}

$url = "$BaseUrl/$Version/$asset"
$tmp = Join-Path $env:TEMP "rgy-install-$asset"

Write-Host "Downloading $url ..." -ForegroundColor Cyan
try {
    Invoke-WebRequest -Uri $url -OutFile $tmp -UseBasicParsing
} catch {
    # The same line and the same exit code the .sh twin prints: the
    # archive is not there, and nothing was installed.
    Remove-Item $tmp -Force -ErrorAction SilentlyContinue
    [Console]::Error.Write("the download failed`n")
    exit 1
}

if (-not $SkipChecksum) {
    # The digest is asked of the tree's helper, the way the unix twin asks
    # it: the machine that runs the installer is not the machine the
    # release was built on, and the helper answers with the tool the
    # machine has. The pin is uppercase hex and the helper answers in
    # lowercase.
    $actual = (Get-RgySha256 $tmp).ToUpperInvariant()
    if ($actual -ne $digest) {
        Remove-Item $tmp -Force
        [Console]::Error.Write("SHA256 mismatch for $asset`n")
        [Console]::Error.Write(" expected $digest`n")
        [Console]::Error.Write(" got      $actual`n")
        exit 1
    }
    Write-Host "SHA256 verified." -ForegroundColor Green
} else {
    Write-Host "SHA256 verification skipped."
}

$staging = Join-Path $env:TEMP "rgy-staging-$(Get-Random)"
Expand-Archive -Path $tmp -DestinationPath $staging -Force

# The file the archive carries, found where the archive keeps it.
#
# The release is flat, but a build that put the binaries in a subdirectory
# would otherwise install nothing and say nothing. The search is bounded to
# the staging directory on purpose: a path that is not there must not turn
# into a walk of the temp folder -- the staging directory lives inside it,
# and the temp folder holds thousands of files that are not ours.
function Find-InStaging([string]$name) {
    if (-not (Test-Path $staging)) { return $null }
    Get-ChildItem -LiteralPath $staging -Recurse -Filter $name -File -ErrorAction SilentlyContinue |
        Select-Object -First 1
}

New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null
foreach ($exe in @('searchctl.exe', 'searchd.exe', 'rgy-autostart.exe', 'rgxy.exe')) {
    $src = Join-Path $staging $exe
    if (-not (Test-Path $src)) {
        $found = Find-InStaging $exe
        if (-not $found) {
            # The same line and the same exit code the .sh twin prints: an
            # archive that does not carry a binary is a failed install, and
            # a pair of twins that answer that in two different words is a
            # pair nobody can compare.
            [Console]::Error.Write("no $exe in $asset`n")
            exit 1
        }
        $src = $found.FullName
    }
    Copy-Item -Path $src -Destination $InstallDir -Force
}
foreach ($doc in @('README.md', 'MANUAL.md', 'FORMATS.md', 'COPYING', 'NOTICE',
                   'LICENSE-APACHE', 'LICENSE-MIT')) {
    $src = Join-Path $staging $doc
    if (-not (Test-Path $src)) {
        $found = Find-InStaging $doc
        if ($found) { $src = $found.FullName }
    }
    if (Test-Path $src) { Copy-Item -Path $src -Destination $InstallDir -Force }
}

# The picture the README shows travels with it.
#
# The README points at it by a relative path, so an install that leaves it
# behind has a README with a hole in it -- and a README that shows nothing
# is a README that says less than the window does. The path inside the
# install is the one the README uses, so the link works where the README
# is.
$docs = Join-Path $staging 'docs'
if (Test-Path $docs) {
    $target = Join-Path $InstallDir 'docs'
    New-Item -ItemType Directory -Force -Path $target | Out-Null
    Copy-Item -Path (Join-Path $docs '*') -Destination $target -Recurse -Force
}

# user PATH (idempotent prepend)
#
# The entry goes into the PATH of the user, and not into the PATH of the
# machine: the shell the user opens reads the first one. The message says
# which of the two it wrote, the way the .sh twin names the profile file
# it wrote -- a pair of twins that answer the same question in two
# different words is a pair nobody can compare.
$userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
if (($userPath -split ';') -notcontains $InstallDir) {
    [Environment]::SetEnvironmentVariable('Path', "$InstallDir;$userPath", 'User')
    Write-Host "Added $InstallDir to the user PATH (open a new terminal to pick it up)." -ForegroundColor Yellow
} else {
    Write-Host "$InstallDir is already on the PATH."
}

# The command the project is named after.
#
# The alias travels with the release: it is the same binary under another
# name, and it is copied as the file the archive carries. On Windows a link
# is something only an administrator can make, so the archive carries the
# binary itself.
#
# The alias is optional. The four binaries above are the product, and an
# archive that does not carry the alias still installs them: an installer
# that aborts over a second name leaves a half-installed tree behind, and
# the .sh twin does not abort. It says the same thing this one says, and
# the install goes on.
$aliasSrc = Join-Path $staging 'rgy.exe'
if (-not (Test-Path $aliasSrc)) {
    $found = Find-InStaging 'rgy.exe'
    $aliasSrc = if ($found) { $found.FullName } else { $null }
}
if ($aliasSrc) {
    Copy-Item -Path $aliasSrc -Destination $InstallDir -Force
    Write-Host "Added the rgy command: it runs searchctl."
} else {
    [Console]::Error.Write("the archive carries no rgy command: the install continues without it.`n")
}

# The staging directory goes last: the alias above is still in it.
Remove-Item $tmp -Force
Remove-Item $staging -Recurse -Force

# The daemon at login.
#
# The same action -Activate takes, through the same function: the autostart
# tool finds the daemon next to itself -- the two were just copied into the
# same directory -- and registers it with the tree it indexes. The entry
# goes into the user's own session: no administrator, no prompt, no dialog.
#
# This step is a convenience, and a failure here is not a reason to tell
# the user the install failed: by the time it runs, the daemon, the
# launcher, the alias and the PATH entry are all in place. So the step
# warns and the install finishes -- the .sh twin does the same, and an
# installer that aborted here would leave a half-installed tool behind
# and a non-zero exit on top of it.
if (-not $NoAutostart) {
    # A normal install ignores the code: the function has already said what
    # happened, and the install is complete with or without the daemon.
    $null = Invoke-RgyRegisterDaemon
}

Write-Host "Installed: searchctl, searchd, rgy-autostart and rgxy in $InstallDir" -ForegroundColor Green
