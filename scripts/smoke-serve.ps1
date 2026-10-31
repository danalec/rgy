# The directory is the only thing this script takes, and it is positional.
# An option it does not know is an error, and so is a second directory: the
# twin refuses both with the same words and the same exit code, because a
# script that ignores what it does not understand does the real thing when
# the caller asked for something else.
. (Join-Path $PSScriptRoot "lib/options.ps1")
$Dir = $null
$Bin = ""
Read-RgyOptions -Values @{ "bin" = "Bin" } -Positionals @{ "dir" = "Dir" } -Arguments $args
$dir = $Dir

# The binary to ask, in the order the question is asked:
#
#   1. the one the caller named (--bin, or RGX_BIN),
#   2. the sibling rgx checkout, in the layout each platform has it,
#   3. this repository's own build.
#
# The order is the same on both platforms, and so is the refusal: a smoke
# that cannot ask the question says so and fails, instead of exiting 0
# without having smoked anything.
#
# The refusal is written with the newline the shell writes, and not with
# the one the host would: the two scripts are compared byte for byte, and a
# CRLF here is a byte the bash twin does not write.
$here = $PSScriptRoot
$repo_root = Split-Path -Parent $here
$bin = $Bin
if (-not $bin) { $bin = $env:RGX_BIN }
if ($bin) {
    if (-not (Test-Path -LiteralPath $bin -PathType Leaf)) {
        [Console]::Error.Write("no rgx binary at $bin; build it first: cargo build --release`n")
        exit 1
    }
} else {
    $bin = ""
    $candidates = @(
        (Join-Path (Split-Path -Parent $repo_root) 'rgx/target/release/rgx'),
        (Join-Path (Split-Path -Parent $repo_root) 'rgx/target/release/rgx.exe'),
        (Join-Path $repo_root 'target/release/rgx'),
        (Join-Path $repo_root 'target/release/rgx.exe')
    )
    foreach ($candidate in $candidates) {
        if (Test-Path -LiteralPath $candidate -PathType Leaf) {
            $bin = $candidate
            break
        }
    }
    if (-not $bin) {
        [Console]::Error.Write("no rgx binary: build it first: cargo build --release`n")
        exit 1
    }
}

# The request is two JSON lines, and a line ends with a newline: the
# daemon reads JSON lines, so the request is one line and the line ends
# where the next one begins.
#
# The bytes are the twin's: a newline and not a CRLF, and one request per
# line with no empty line after it. PowerShell's own pipeline would write a
# CRLF and an extra line, and the two platforms would then be asked two
# different questions -- so the bytes are written to the child's standard
# input as they are.
$requests = @(
    (ConvertTo-Json -Compress -InputObject @('needle', $dir)),
    (ConvertTo-Json -Compress -InputObject @('-c', 'needle', $dir))
)
$payload = ($requests -join "`n") + "`n"
$bytes = [System.Text.Encoding]::UTF8.GetBytes($payload)

# The child is started with its standard input redirected and nothing else
# redirected, so the bytes it writes are its own and reach the console as
# they are -- the way the unix twin's pipeline delivers them.
$start = [System.Diagnostics.ProcessStartInfo]::new()
$start.FileName = $bin
$start.Arguments = '--serve'
$start.UseShellExecute = $false
$start.RedirectStandardInput = $true
$process = [System.Diagnostics.Process]::Start($start)
$stream = $process.StandardInput.BaseStream
$stream.Write($bytes, 0, $bytes.Length)
$stream.Flush()
$stream.Close()
$process.WaitForExit()
exit $process.ExitCode
