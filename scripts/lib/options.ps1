#
# The option vocabulary every script in this directory shares, and the twin
# of scripts/lib/options.sh.
#
# A script in this directory is a twin of a script there: the same output,
# the same exit code, the same answer to the same input. That includes the
# answer to an input neither of them understands. An option the script does
# not know is an error -- the same words and the same exit code on both
# platforms.
#
# The reason is not tidiness. A script that ignores what it does not
# understand does the real thing when the caller asked for a dry run, and
# in this directory the real thing is a release: a tag, an archive, a
# release page on the forgejo and on GitHub, an installer that points at
# it. The mistake is silent, and the release is out before anybody reads
# the log. It happened: `--dry-run` is not a PowerShell parameter, it
# landed in $args, and the script published 0.1.1 for real.
#
# The vocabulary:
#
#   --dry-run      -DryRun       one option, two spellings: the long one
#                               is the twin's, the capitalised one is
#                               PowerShell's, and a caller who writes
#                               either gets the same answer on either
#                               platform.
#   --version X    -Version X    an option with a value. The value is the
#                               next argument, or the part after the
#                               colon (-Version:X).
#   --queries X    -Queries X    an option that may be given more than
#                               once. A variable named with [] collects
#                               every occurrence, so --queries a --queries
#                               b leaves the script with a list of two.
#
# The name is matched with the dashes and the case off, so --dry-run,
# -DryRun and --dryrun are one option.
#
# The caller declares what it takes and names its variables: the switches,
# the values, and the slots a positional argument goes into.
#
#   $Version = ""
#   $DryRun = $false
#   Read-RgyOptions -Switches @{ "dryrun" = "DryRun" } `
#                   -Values   @{ "version" = "Version" } `
#                   -Positionals @{ } `
#                   -Arguments $args
#
# and the variables are set in the caller's scope, so the script reads them
# as its own.

function Test-RgyHelp {
    param(
        [object[]]$Arguments = @()
    )

    # The names are matched the way the option names are, so -h, --help and
    # -Help are one question, and the answer does not depend on where the
    # token sits on the command line.
    foreach ($argument in $Arguments) {
        $key = (([string]$argument) -replace '^-{1,2}', '' -replace '[-_]', '').ToLowerInvariant()
        if ($key -eq 'h' -or $key -eq 'help') { return $true }
    }
    return $false
}

function Get-RgyPositional {
    param(
        [System.Collections.IDictionary]$Table = @{},
        [int]$Index = 0
    )

    # The slots are taken in the order the table lists them. A PowerShell
    # hashtable does not keep an order, so a script with more than one
    # positional asks for an ordered one.
    $at = 0
    foreach ($key in $Table.Keys) {
        if ($at -eq $Index) { return [string]$Table[$key] }
        $at++
    }
    return ""
}

function Read-RgyOptions {
    param(
        [hashtable]$Switches = @{},
        [hashtable]$Values = @{},
        # The slots a positional argument goes into, in the order they are
        # listed. A script that takes none says so with an empty table, and
        # then every argument that is not an option is an error.
        [System.Collections.IDictionary]$Positionals = @{},
        # The arguments are objects and not strings: PowerShell hands a
        # comma-joined argument over as an array when the script is called
        # in -Command mode, and the twin that is a shell gets the same text
        # as one word. The array is joined back below, so that both
        # platforms read the same value.
        [object[]]$Arguments = @()
    )

    # The refusal is written with the newline the shell writes, and not with
    # the one the host would: the two scripts are compared byte for byte, and
    # a CRLF here is a byte the bash twin does not write.
    function Write-RgyRefusal([string]$text) {
        [Console]::Error.Write("$text`n")
    }

    $lists = @{}
    $slot = 0
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
        # The name with the dashes and the case off: --dry-run, -DryRun and
        # --dryrun are one option.
        $key = ($name -replace '^-{1,2}', '' -replace '[-_]', '').ToLowerInvariant()
        if ($Switches.ContainsKey($key)) {
            if ($haveValue) {
                # A switch takes no value. -DryRun:$false is a name the
                # script does not know, and saying so is better than
                # guessing what the caller meant.
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
                $raw = $Arguments[$at + 1]
                $value = if ($raw -is [string]) { $raw } else { (@($raw) | ForEach-Object { [string]$_ }) -join "," }
                $at++
            }
            $target = [string]$Values[$key]
            if ($target.EndsWith("[]")) {
                # A variable named with [] takes every occurrence: an option
                # that may be given more than once collects its values
                # instead of keeping the last one.
                $base = $target.Substring(0, $target.Length - 2)
                if (-not $lists.ContainsKey($base)) { $lists[$base] = @() }
                $lists[$base] = @($lists[$base]) + @($value)
            } else {
                Set-Variable -Name $target -Value $value -Scope 1
            }
            $at++
        } else {
            # Not an option. It is a positional when it does not look like
            # one, and the script declares the slot it goes into; a
            # positional is the argument as it was written, colon and all --
            # a Windows path is not an option with a value.
            $target = ""
            if (-not $written.StartsWith('-')) {
                $target = Get-RgyPositional $Positionals $slot
                if (-not [string]::IsNullOrEmpty($target)) { $slot++ }
            }
            if ([string]::IsNullOrEmpty($target)) {
                Write-RgyRefusal "unknown option: $written"
                exit 2
            }
            Set-Variable -Name $target -Value $written -Scope 1
            $at++
        }
    }

    foreach ($name in $lists.Keys) {
        Set-Variable -Name $name -Value $lists[$name] -Scope 1
    }
}
