# -ZellijPort runs agents in Zellij (zellij.useInDesktop), served on that fixed port.
param([string]$Binary, [int]$ZellijPort)
$ErrorActionPreference = 'Stop'
$workspace = (Resolve-Path (Join-Path $PSScriptRoot '../../..')).Path
if (-not $Binary) { $Binary = Join-Path $workspace 'app/src-tauri/target/debug/agentflow-desktop.exe' }
$binaryPath = [IO.Path]::GetFullPath($Binary)
if (-not $binaryPath.StartsWith($workspace + [IO.Path]::DirectorySeparatorChar,
        [StringComparison]::OrdinalIgnoreCase)) { throw 'Use a desktop binary in this workspace.' }
if (-not (Test-Path -LiteralPath $binaryPath)) { throw 'Build the desktop binary first.' }
$testDirectory = Join-Path $workspace ('.port-test-tmp/desktop-' + [Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $testDirectory -Force | Out-Null
$configPath = Join-Path $testDirectory 'config.json'
$reportPath = Join-Path $testDirectory 'report.json'
$placeholder = @{type='generic-cli'; command='where.exe'; displayName='Local smoke placeholder'}
$config = @{
    app=@{dataDirectory=$testDirectory; databasePath=(Join-Path $testDirectory 'agentflow.db')}
    workspace=@{worktreeDirectory=(Join-Path $testDirectory 'worktrees'); taskDirectory=(Join-Path $testDirectory 'tasks')}
    harnesses=@{smoke=$placeholder}
    agents=@{architect=@{harness='smoke'}; implementer=@{harness='smoke'};
        codeReviewer=@{harness='smoke'}; securityReviewer=@{harness='smoke'}}
    slack=@{enabled=$false}
    jira=@{}
}
if ($ZellijPort) {
    $config.zellij = @{useInDesktop=$true; sessionPrefix='agentflow-smoke-'}
    $config.server = @{port=$ZellijPort}
}
$config | ConvertTo-Json -Depth 10 | Set-Content -LiteralPath $configPath -Encoding UTF8
$names = @('AGENTFLOW_HOME','AGENTFLOW_CONFIG','AGENTFLOW_SKIP_JIRA_CHECK','TEMP','TMP')
$previous = @{}
foreach ($name in $names) { $previous[$name] = [Environment]::GetEnvironmentVariable($name,'Process') }
$desktop = $null
try {
    $env:AGENTFLOW_HOME = $testDirectory
    $env:AGENTFLOW_CONFIG = $configPath
    $env:AGENTFLOW_SKIP_JIRA_CHECK = '1'
    $env:TEMP = $testDirectory
    $env:TMP = $testDirectory
    $desktop = Start-Process -FilePath $binaryPath -WorkingDirectory $workspace -WindowStyle Hidden `
        -ArgumentList @('--smoke-test',('"' + $reportPath + '"')) -PassThru `
        -RedirectStandardOutput (Join-Path $testDirectory 'stdout.log') `
        -RedirectStandardError (Join-Path $testDirectory 'stderr.log')
    if (-not $desktop.WaitForExit(45000)) { throw "Desktop smoke timed out; see $testDirectory" }
    $desktop.Refresh()
    if (-not (Test-Path -LiteralPath $reportPath)) { throw "Desktop did not report; see $testDirectory" }
    $report = Get-Content -LiteralPath $reportPath -Raw | ConvertFrom-Json
    if ($desktop.ExitCode -ne 0 -or -not $report.ok) { throw "Desktop smoke failed: $($report.error)" }
    $expected = if ($ZellijPort) { 'zellij' } else { 'embedded' }
    if ($report.terminal -ne $expected) { throw "Desktop used $($report.terminal) terminals, expected $expected" }
    if ($ZellijPort -and $report.port -ne "$ZellijPort") { throw "Desktop served port $($report.port), expected $ZellijPort" }
    $report | ConvertTo-Json
}
finally {
    if ($desktop -and -not $desktop.HasExited) { Stop-Process -Id $desktop.Id -ErrorAction SilentlyContinue }
    foreach ($name in $names) { [Environment]::SetEnvironmentVariable($name,$previous[$name],'Process') }
}
