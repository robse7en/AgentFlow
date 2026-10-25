# Guided setup for AgentFlow on Windows. Run from PowerShell:
#   powershell -ExecutionPolicy Bypass -File setup.ps1
#
# Gets Python ready and installs the backend, then hands over to `agentflow setup`, which checks and
# offers to install everything else and writes config.json. Nothing is installed without a yes.
# Arguments go to `agentflow setup` (e.g. setup.ps1 --config C:\agentflow.json).
# Works in Windows PowerShell 5.1 and PowerShell 7.
$ErrorActionPreference = 'Stop'
$root = $PSScriptRoot
$py = Join-Path $root 'backend\.venv\Scripts\python.exe'
$backend = Join-Path $root 'backend'

function Step($text) { Write-Host "`n== $text ==" -ForegroundColor Cyan }

function Confirm-Yes($question) {
    $reply = Read-Host "$question [y/N]"
    return $reply -match '^[Yy]'
}

# $ErrorActionPreference does not cover native commands: check their exit code.
function Invoke-Checked {
    param([string]$Command, [string[]]$Arguments)
    & $Command @Arguments
    if ($LASTEXITCODE -ne 0) { throw "$Command $($Arguments -join ' ') failed (exit $LASTEXITCODE)" }
}

# Installers write PATH to the registry, which this session never re-reads.
function Update-SessionPath {
    $machine = [Environment]::GetEnvironmentVariable('Path', 'Machine')
    $user = [Environment]::GetEnvironmentVariable('Path', 'User')
    $env:Path = "$machine;$user"
}

function Get-Uv {
    $command = Get-Command uv -ErrorAction SilentlyContinue
    if ($command) { return $command.Source }
    return $null
}

# The first Python 3.12+ found, as a command plus its leading arguments. Probing each candidate skips
# the Microsoft Store 'python' alias and older installs.
function Find-Python {
    $probe = 'import sys; sys.exit(0 if sys.version_info >= (3, 12) else 1)'
    $candidates = @(@('py', '-3.13'), @('py', '-3.12'), @('python3'), @('python'))
    foreach ($candidate in $candidates) {
        if (-not (Get-Command $candidate[0] -ErrorAction SilentlyContinue)) { continue }
        $arguments = @($candidate | Select-Object -Skip 1)
        try {
            & $candidate[0] @arguments -c $probe 2>$null
            if ($LASTEXITCODE -eq 0) { return ,$candidate }
        } catch { }
    }
    return $null
}

# --- Python and the backend venv -----------------------------------------------
if (-not (Test-Path $py)) {
    $uv = Get-Uv
    $python = $null
    if (-not $uv) { $python = Find-Python }
    if (-not $uv -and -not $python) {
        Write-Host 'AgentFlow needs Python 3.12 or newer. uv (https://docs.astral.sh/uv/) can provide it.'
        if (Get-Command winget -ErrorAction SilentlyContinue) {
            $install = @('winget', 'install', '--id', 'astral-sh.uv', '--exact', '--source', 'winget')
        } else {
            $install = @('powershell', '-ExecutionPolicy', 'ByPass', '-c', 'irm https://astral.sh/uv/install.ps1 | iex')
        }
        Write-Host "It can be installed with:`n  $($install -join ' ')"
        if (-not (Confirm-Yes 'Install uv now?')) {
            Write-Host 'Install Python 3.12+ or uv, then run setup.ps1 again.' -ForegroundColor Yellow
            exit 1
        }
        Invoke-Checked $install[0] ($install | Select-Object -Skip 1)
        Update-SessionPath
        # uv's own installer uses ~\.local\bin, which may not be on the registry PATH yet.
        $env:Path = "$env:Path;$HOME\.local\bin"
        $uv = Get-Uv
        if (-not $uv) { throw 'uv was installed but is not on PATH; open a new terminal and run setup.ps1 again.' }
    }
    Step 'Creating the Python venv (backend\.venv)'
    if ($uv) {
        Invoke-Checked $uv @('venv', '--python', '3.13', (Join-Path $backend '.venv'))  # downloads Python if needed
    } else {
        $arguments = @($python | Select-Object -Skip 1) + @('-m', 'venv', (Join-Path $backend '.venv'))
        Invoke-Checked $python[0] $arguments
    }
}

# In Windows PowerShell 5.1, a redirected native stderr line becomes a terminating error under 'Stop'.
$installed = $false
try { & $py -c 'import agentflow' 2>$null; $installed = ($LASTEXITCODE -eq 0) } catch { }
if (-not $installed) {
    Step 'Installing the backend'
    $uv = Get-Uv
    if ($uv) {
        Invoke-Checked $uv @('pip', 'install', '--python', $py, '-e', $backend)
    } else {
        Invoke-Checked $py @('-m', 'pip', 'install', '-e', $backend)  # a venv made by `python -m venv` has pip
    }
}

# --- the wizard ----------------------------------------------------------------
& $py -m agentflow.main setup @args
exit $LASTEXITCODE
