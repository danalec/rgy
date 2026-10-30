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
#
# The name is matched with the dashes and the case off, so --dry-run,
# -DryRun and --dryrun are one option.
#
# The caller declares what it takes and names its variables:
#
#   $Version = ""
#   $DryRun = $false
#   Read-RgyOptions -Switches @{ "dryrun" = "DryRun" } `
#                   -Values   @{ "version" = "Version" } `
#                   -Arguments $args
#
# and the variables are set in the caller's scope, so the script reads them
# as its own.

function Read-RgyOptions {
    param(
        [hashtable]$Switches = @{},
        [hashtable]$Values = @{},
        [string[]]$Arguments = @()
    )

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
                [Console]::Error.WriteLine("unknown option: $written")
                exit 2
            }
            Set-Variable -Name $Switches[$key] -Value $true -Scope 1
            $at++
        } elseif ($Values.ContainsKey($key)) {
            if (-not $haveValue) {
                if ($at + 1 -ge $Arguments.Count) {
                    [Console]::Error.WriteLine("option $written needs a value")
                    exit 2
                }
                $value = [string]$Arguments[$at + 1]
                $at++
            }
            Set-Variable -Name $Values[$key] -Value $value -Scope 1
            $at++
        } else {
            [Console]::Error.WriteLine("unknown option: $written")
            exit 2
        }
    }
}
