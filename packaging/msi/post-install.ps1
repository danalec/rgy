# post-install.ps1 -- register AND start the daemon after an MSI install.
#
# This is the MSI's answer to the Scoop bucket's `post_install`: the same
# "register it and start it NOW" default (choice [2]), with no prompt and
# no elevation. The MSI runs it as a deferred, impersonated custom action,
# so it executes as the installing user -- the Task Scheduler ONLOGON task
# and the HKCU Run value belong to that user, not to SYSTEM.
#
# WHY the start is a separate step: `rgy-autostart install` writes a logon
# entry, and a logon entry only runs at the NEXT logon. Without this step a
# fresh install leaves rgxy saying "no connection to the daemon" until the
# user signs out and back in. The one-line installer already learned this
# (`schtasks /Run`); the MSI does the same, plus a detached start as the
# fallback for a machine where the mechanism was the HKCU Run value.
#
# Idempotent: running it twice is harmless. `schtasks /Create /F` replaces
# the task, `reg add /f` replaces the value, and a daemon that is already
# listening simply refuses the second pipe.

param(
    [string]$InstallDir = '',
    [switch]$Uninstall
)

$ErrorActionPreference = 'Continue'

# The script ships INSIDE the install folder, so its own location is the
# install directory. A path passed by the installer is only a fallback: MSI
# formatted strings and shell quoting do not compose safely (a trailing
# backslash escapes a closing quote, and quote characters arrive literally).
if (-not $InstallDir) { $InstallDir = $PSScriptRoot }
$InstallDir = $InstallDir.Trim().Trim('"').Trim("'").TrimEnd('\')

$autostart = Join-Path $InstallDir 'rgy-autostart.exe'
$daemon = Join-Path $InstallDir 'searchd.exe'
$runKeyPs = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Run'
$runKeyReg = 'HKCU\Software\Microsoft\Windows\CurrentVersion\Run'
$taskName = 'rgy-searchd'

if ($Uninstall) {
    # Take out exactly what the install put in. The files and the PATH entry
    # are MSI's business and are removed by the uninstall sequence.
    if (Test-Path -LiteralPath $autostart -PathType Leaf) {
        & $autostart uninstall --launcher | Out-Host
    }
    # The task may still be there if the tool used the HKCU fallback; remove
    # it too so a future install does not find a stale ONLOGON task.
    & schtasks.exe /Delete /TN $taskName /F 2>$null | Out-Null
    Remove-ItemProperty -Path $runKeyPs -Name $taskName -ErrorAction SilentlyContinue
    [Console]::Out.WriteLine('rgy: the daemon and launcher login entries were removed.')
    exit 0
}

if (-not (Test-Path -LiteralPath $autostart -PathType Leaf)) {
    [Console]::Error.WriteLine("rgy: rgy-autostart.exe is not in $InstallDir; the daemon was not registered.")
    exit 1
}

# 1. The login entry. --launcher registers rgxy beside searchd, which is
#    what makes the window available at logon as well as on demand.
& $autostart install --launcher | Out-Host
$code = $LASTEXITCODE
if ($code -ne 0 -and $code -ne 2) {
    [Console]::Error.WriteLine("rgy: rgy-autostart install returned $code; the daemon was not registered.")
    exit 1
}

# 2. The command that the login entry runs. `rgy-autostart` chose the pipe
#    name; read it back from the value it wrote so the manual start uses the
#    SAME endpoint and does not open a second, competing daemon.
$installed = (Get-ItemProperty -Path $runKeyPs -Name $taskName -ErrorAction SilentlyContinue |
    Select-Object -ExpandProperty $taskName)
$listen = if ($installed -match '--listen\s+(\S+)') { $Matches[1] } else {
    'pipe:rgy-searchd-' + $env:USERNAME
}
$args = @('--root', $env:USERPROFILE, '--listen', $listen)
$runCmd = '"' + $daemon + '" --root ' + $env:USERPROFILE + ' --listen ' + $listen
& reg.exe add $runKeyReg /v $taskName /t REG_SZ /d $runCmd /f | Out-Null

# 3. Start it NOW. `schtasks /Run` is the same command `rgy-autostart
#    doctor` prints as its fix:. It is best-effort: on a machine whose
#    mechanism was the HKCU Run fallback there is no task to run.
$ran = $false
$run = Start-Process -FilePath 'schtasks.exe' -ArgumentList '/Run','/TN',$taskName `
    -Wait -PassThru -WindowStyle Hidden -ErrorAction SilentlyContinue
if ($run -and $run.ExitCode -eq 0) { $ran = $true }

if (-not $ran) {
    # No task to run (or it failed): start the daemon directly, detached
    # and hidden, so it keeps running after the installer exits.
    Start-Process -FilePath $daemon -ArgumentList $args -WindowStyle Hidden -ErrorAction SilentlyContinue | Out-Null
}

[Console]::Out.WriteLine("rgy: the daemon is registered for logon and was started now on $listen.")
[Console]::Out.WriteLine('rgy: it indexes your home directory before it opens the pipe; rgxy connects when that finishes.')
[Console]::Out.WriteLine('rgy: check with: rgy-autostart doctor')
exit 0
