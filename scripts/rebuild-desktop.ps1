param(
    [switch]$Release,
    [switch]$Help
)

$ErrorActionPreference = 'Stop'

# Windows' bash.exe can be WSL. Resolve Bash from Git for Windows explicitly.
$candidates = @()
$git = Get-Command git.exe -ErrorAction SilentlyContinue
if ($git) {
    $gitRoot = Split-Path (Split-Path $git.Source -Parent) -Parent
    $candidates += Join-Path $gitRoot 'bin/bash.exe'
}
if ($env:ProgramFiles) {
    $candidates += Join-Path $env:ProgramFiles 'Git/bin/bash.exe'
}
if ($env:LOCALAPPDATA) {
    $candidates += Join-Path $env:LOCALAPPDATA 'Programs/Git/bin/bash.exe'
}
$gitBash = $candidates | Where-Object { Test-Path -LiteralPath $_ -PathType Leaf } |
    Select-Object -First 1
if (-not $gitBash) {
    throw 'Git Bash was not found. Install Git for Windows and reopen PowerShell.'
}

$script = (Join-Path $PSScriptRoot 'rebuild-desktop.sh').Replace('\', '/')
$bashArguments = @('--login', $script)
if ($Help) {
    $bashArguments += '--help'
} elseif ($Release) {
    $bashArguments += '--release'
}
& $gitBash @bashArguments
exit $LASTEXITCODE
