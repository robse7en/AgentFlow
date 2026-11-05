param([string]$Installer, [string]$PreviousInstaller)
$ErrorActionPreference = 'Stop'
$workspace = (Resolve-Path (Join-Path $PSScriptRoot '../../..')).Path
if (-not $Installer) {
    $Installer = Join-Path $workspace 'app/src-tauri/target/installer/release/bundle/nsis/AgentFlow Installer Validation_0.1.1_x64-setup.exe'
}
$installerPath = [IO.Path]::GetFullPath($Installer)
if (-not $PreviousInstaller) {
    $PreviousInstaller = Join-Path $workspace 'app/src-tauri/target/installer/release/bundle/nsis/AgentFlow Installer Validation_0.1.0_x64-setup.exe'
}
$previousPath = [IO.Path]::GetFullPath($PreviousInstaller)
if (-not $installerPath.StartsWith($workspace + '\', [StringComparison]::OrdinalIgnoreCase) -or
    -not (Test-Path -LiteralPath $installerPath)) { throw 'Build the validation installer in this workspace first.' }
if ((Split-Path $installerPath -Leaf) -notlike 'AgentFlow Installer Validation_*-setup.exe') {
    throw 'Use the validation package to keep the normal AgentFlow installation untouched.'
}
if (-not $previousPath.StartsWith($workspace + '\', [StringComparison]::OrdinalIgnoreCase) -or
    -not (Test-Path -LiteralPath $previousPath) -or
    (Split-Path $previousPath -Leaf) -notlike 'AgentFlow Installer Validation_0.1.0_*-setup.exe') {
    throw 'Build the previous validation package with -ValidationPackage -PreviousValidationVersion first.'
}
$root = Join-Path $workspace ('.installer-test-tmp/' + [Guid]::NewGuid().ToString('N'))
$installation = Join-Path $root 'installed app'
$newHome = Join-Path $root 'new-user'
$oldHome = Join-Path $root 'existing-user'
$source = Join-Path $root 'old web instance'
$bin = Join-Path $root 'tools'
New-Item -ItemType Directory -Force -Path $root, $newHome, $oldHome, $source, $bin | Out-Null
$names = @('AGENTFLOW_HOME','AGENTFLOW_CONFIG','AGENTFLOW_SKIP_JIRA_CHECK','AGENTFLOW_SETUP_TEST_SOURCE',
    'AGENTFLOW_SETUP_TEST_ROOT','AGENTFLOW_SETUP_TEST_REPOSITORY','PATH','TEMP','TMP','LOCALAPPDATA')
$previous = @{}
foreach ($name in $names) { $previous[$name] = [Environment]::GetEnvironmentVariable($name, 'Process') }
$live = $null
$script:smokeProcess = $null

function Assert-True([bool]$Condition, [string]$Message) {
    if (-not $Condition) { throw $Message }
}
function Run-Installer([string]$Package = $installerPath) {
    $process = Start-Process -FilePath $Package -WindowStyle Hidden -PassThru `
        -ArgumentList ("/S /NS /D=" + $installation)
    if (-not $process.WaitForExit(45000)) { throw "Installer is still running (PID $($process.Id))." }
    $process.Refresh()
    return $process.ExitCode
}
function Remove-ValidationInstall {
    $uninstall = Join-Path $installation 'uninstall.exe'
    Assert-True ($uninstall.StartsWith($root + '\', [StringComparison]::OrdinalIgnoreCase)) 'Uninstaller escaped its isolated directory.'
    # /UPDATE retains app data and skips shortcut operations; _?= runs in this workspace.
    $process = Start-Process -FilePath $uninstall -WindowStyle Hidden -PassThru `
        -ArgumentList ("/S /UPDATE _?=" + $installation)
    Assert-True ($process.WaitForExit(45000)) 'Uninstaller is still running.'
    $process.Refresh()
    Assert-True ($process.ExitCode -eq 0) 'NSIS uninstall failed.'
    Assert-True (-not (Test-Path (Join-Path $installation 'agentflow-installer-validation.exe'))) 'Uninstall left the packaged app.'
}
function Run-Smoke([string]$Mode, [string]$Name) {
    $report = Join-Path $root ($Name + '.json')
    $binary = Join-Path $installation 'agentflow-installer-validation.exe'
    $process = Start-Process -FilePath $binary -WorkingDirectory $installation -WindowStyle Hidden -PassThru `
        -ArgumentList @($Mode, ('"' + $report + '"'))
    $script:smokeProcess = $process
    if ($Mode -eq '--setup-smoke-test') {
        $ready = [IO.Path]::ChangeExtension($report, 'setup.json')
        $deadline = [DateTime]::UtcNow.AddSeconds(10)
        while (-not (Test-Path -LiteralPath $ready) -and [DateTime]::UtcNow -lt $deadline -and -not $process.HasExited) {
            Start-Sleep -Milliseconds 100
        }
        Assert-True (Test-Path -LiteralPath $ready) "Setup did not report its local URL: $ready"
        $url = (Get-Content -LiteralPath $ready -Raw | ConvertFrom-Json).url
        Assert-True ($url -match '^http://127\.0\.0\.1:\d+$') 'Setup exposed a non-loopback URL.'
        $page = Invoke-WebRequest -Uri ($url + '/')
        Assert-True ($page.Content.Contains('Set up AgentFlow')) 'The setup page did not render.'
        $token = [regex]::Match($page.Content, 'name="token" value="([^"]+)"').Groups[1].Value
        $body = @{token='invalid'; mode='new'}
        $rejected = Invoke-WebRequest -Uri ($url + '/setup') -Method Post -Headers @{Origin=$url} `
            -Body $body -SkipHttpErrorCheck
        Assert-True ($rejected.StatusCode -eq 403) 'Setup accepted an invalid token.'
        $body.token = $token
        $rejected = Invoke-WebRequest -Uri ($url + '/setup') -Method Post -Headers @{Origin='http://localhost:1'} `
            -Body $body -SkipHttpErrorCheck
        Assert-True ($rejected.StatusCode -eq 403) 'Setup accepted an external origin.'
        $rejected = Invoke-WebRequest -Uri ($url + '/') -Headers @{Host='invalid.local'} -SkipHttpErrorCheck
        Assert-True ($rejected.StatusCode -eq 403) 'Setup accepted a different Host header.'
        if ($Name -eq 'existing-user') {
            $fields = @{mode='existing'; config_path=(Join-Path $source 'config.json'); base_directory=$source; closed='yes'}
        } else {
            $fields = @{mode='new'; repository=$root; harness='claude'; billing='subscription';
                jira_kind='cloud'; jira_url='https://jira.invalid'; jira_email='local-smoke@example.invalid';
                jira_token='local-smoke-placeholder'}
        }
        # The window fills and submits its own form, so the POST carries WebView2's real Origin.
        $fields | ConvertTo-Json | Set-Content -LiteralPath ([IO.Path]::ChangeExtension($report, 'form.json')) -Encoding utf8
    }
    if (-not $process.WaitForExit(45000)) {
        Stop-Process -Id $process.Id -ErrorAction SilentlyContinue
        throw "Desktop smoke timed out: $report"
    }
    $process.Refresh()
    Assert-True (Test-Path -LiteralPath $report) "Desktop did not report: $report"
    $result = Get-Content -LiteralPath $report -Raw | ConvertFrom-Json
    Assert-True ($process.ExitCode -eq 0 -and $result.ok) "Desktop smoke failed: $($result.error)"
    $script:smokeProcess = $null
    return $result
}
try {
    $env:TEMP = Join-Path $root 'temp'
    $env:TMP = $env:TEMP
    $env:LOCALAPPDATA = Join-Path $root 'local-app-data'
    New-Item -ItemType Directory -Force -Path $env:TEMP, $env:LOCALAPPDATA | Out-Null
    foreach ($name in @('claude','codex','copilot')) {
        [IO.File]::WriteAllText((Join-Path $bin ($name + '.cmd')), "@echo local installer smoke CLI`r`n@exit /b 0`r`n")
    }
    $env:PATH = $bin + ';' + $env:PATH
    $env:AGENTFLOW_CONFIG = $null
    $env:AGENTFLOW_SKIP_JIRA_CHECK = $null
    Assert-True ((Run-Installer) -eq 0) 'Fresh NSIS installation failed.'
    foreach ($resource in @('agentflow-installer-validation.exe','flowctl.exe','ui/index.html','uninstall.exe')) {
        Assert-True (Test-Path -LiteralPath (Join-Path $installation $resource)) "Missing installed resource: $resource"
    }
    $flowctl = Join-Path $workspace 'app/target/release/flowctl.exe'
    Assert-True ((Get-FileHash $flowctl).Hash -eq (Get-FileHash (Join-Path $installation 'flowctl.exe')).Hash) 'flowctl differs from the build input.'
    foreach ($file in Get-ChildItem (Join-Path $workspace 'frontend/dist') -File -Recurse) {
        $relative = $file.FullName.Substring((Join-Path $workspace 'frontend/dist').Length + 1)
        Assert-True ((Get-FileHash $file.FullName).Hash -eq (Get-FileHash (Join-Path $installation ('ui/' + $relative))).Hash) "UI differs: $relative"
    }
    $env:AGENTFLOW_HOME = $newHome
    $env:AGENTFLOW_SETUP_TEST_SOURCE = $null
    $env:AGENTFLOW_SETUP_TEST_ROOT = $null
    $env:AGENTFLOW_SETUP_TEST_REPOSITORY = $root
    $fresh = Run-Smoke '--setup-smoke-test' 'new-user'
    Assert-True ((Test-Path (Join-Path $newHome 'config.json')) -and (Test-Path (Join-Path $newHome 'agentflow.db'))) 'New setup did not create isolated configuration and database.'
    $newConfig = Get-Content (Join-Path $newHome 'config.json') -Raw | ConvertFrom-Json
    Assert-True ($newConfig.jira.enabled -and $newConfig.jira.apiToken -eq 'local-smoke-placeholder') 'Jira configuration was not saved.'
    $newConfig.github.prSync = $false
    $newConfig.app.dataDirectory = 'data'
    $newConfig.app.databasePath = 'data/agentflow.db'
    $newConfig.workspace.taskDirectory = 'data/tasks'
    $newConfig.workspace.worktreeDirectory = 'worktrees'
    $newConfig | ConvertTo-Json -Depth 30 | Set-Content -LiteralPath (Join-Path $source 'config.json') -Encoding utf8
    New-Item -ItemType Directory -Force -Path (Join-Path $source 'data/tasks'), (Join-Path $source 'worktrees') | Out-Null
    [IO.File]::WriteAllText((Join-Path $source 'data/tasks/keep.txt'), 'existing task artifacts')
    [IO.File]::WriteAllText((Join-Path $source 'worktrees/keep.txt'), 'existing worktrees')
    $python = Join-Path $workspace 'backend/.venv/Scripts/python.exe'
    & $python (Join-Path $PSScriptRoot 'installer-history-fixture.py') (Join-Path $newHome 'agentflow.db') `
        (Join-Path $source 'data/agentflow.db') $source
    Assert-True ($LASTEXITCODE -eq 0) 'Could not seed historical task.'
    $configHash = (Get-FileHash (Join-Path $source 'config.json')).Hash
    $env:AGENTFLOW_HOME = $oldHome
    $env:AGENTFLOW_SETUP_TEST_SOURCE = Join-Path $source 'config.json'
    $env:AGENTFLOW_SETUP_TEST_ROOT = $source
    $existing = Run-Smoke '--setup-smoke-test' 'existing-user'
    Assert-True ($existing.tasks -eq 1) 'The imported instance did not show the historical task.'
    Assert-True ((Get-FileHash (Join-Path $source 'config.json')).Hash -eq $configHash) 'Import changed the original configuration.'
    $selectionHash = (Get-FileHash (Join-Path $oldHome 'desktop-instance.json')).Hash
    $restart = Run-Smoke '--smoke-test' 'existing-restart'
    Assert-True ($restart.tasks -eq 1) 'Restart forgot the selected instance or its history.'
    Assert-True ((Run-Installer) -eq 0) 'NSIS reinstall/upgrade failed.'
    Assert-True ((Get-FileHash (Join-Path $oldHome 'desktop-instance.json')).Hash -eq $selectionHash) 'Upgrade changed the selected instance.'
    $upgrade = Run-Smoke '--smoke-test' 'after-upgrade'
    Assert-True ($upgrade.tasks -eq 1) 'Upgrade lost the historical task.'
    # Keep the candidate open, then prove a silent upgrade refuses to force-close it.
    $live = Start-Process -FilePath (Join-Path $installation 'agentflow-installer-validation.exe') `
        -WorkingDirectory $installation -WindowStyle Hidden -PassThru -ArgumentList '--setup'
    Start-Sleep -Seconds 2
    Assert-True (-not $live.HasExited) 'The running candidate exited before the upgrade check.'
    Assert-True ((Run-Installer) -ne 0) 'An upgrade was allowed while the installed app was running.'
    $live.Refresh()
    Assert-True (-not $live.HasExited) 'The installer force-closed the running desktop.'
    Stop-Process -Id $live.Id
    $live.WaitForExit(5000) | Out-Null
    $live = $null
    Remove-ValidationInstall
    # Both versions use this source; this exercises NSIS version handling and data retention.
    Assert-True ((Run-Installer $previousPath) -eq 0) 'Could not install the 0.1.0 validation fixture.'
    $binaryVersion = (Get-Item (Join-Path $installation 'agentflow-installer-validation.exe')).VersionInfo.FileVersion
    Assert-True ($binaryVersion -match '^0\.1\.0') 'The previous fixture did not install version 0.1.0.'
    Assert-True ((Run-Installer) -eq 0) 'The 0.1.0 to 1.1.14 installer upgrade failed.'
    $binaryVersion = (Get-Item (Join-Path $installation 'agentflow-installer-validation.exe')).VersionInfo.FileVersion
    Assert-True ($binaryVersion -match '^1\.1\.14') 'The upgrade did not install version 1.1.14.'
    $versionUpgrade = Run-Smoke '--smoke-test' 'after-version-upgrade'
    Assert-True ($versionUpgrade.tasks -eq 1) 'Version upgrade lost the historical task.'
    Assert-True ((Run-Installer $previousPath) -ne 0) 'An installer downgrade was allowed.'
    Remove-ValidationInstall
    foreach ($path in @((Join-Path $source 'config.json'),(Join-Path $source 'data/agentflow.db'),
        (Join-Path $source 'data/tasks/keep.txt'),(Join-Path $source 'worktrees/keep.txt'),
        (Join-Path $oldHome 'desktop-instance.json'),(Join-Path $newHome 'config.json'),(Join-Path $newHome 'agentflow.db'))) {
        Assert-True (Test-Path -LiteralPath $path) "Uninstall removed user data: $path"
    }
    Assert-True ((Get-FileHash (Join-Path $source 'config.json')).Hash -eq $configHash) 'Upgrade or uninstall changed original configuration.'
    $report = @{ok=$true; freshSetup=$true; existingImport=$true; historyTasks=$upgrade.tasks;
        restart=$true; upgrade=$true; versionUpgrade=$true; downgradeRefused=$true;
        runningUpgradeRefused=$true; uninstallPreservedData=$true; root=$root}
    $report | ConvertTo-Json | Set-Content -LiteralPath (Join-Path $root 'installer-report.json') -Encoding utf8
    $report | ConvertTo-Json
}
finally {
    if ($script:smokeProcess -and -not $script:smokeProcess.HasExited) {
        Stop-Process -Id $script:smokeProcess.Id -ErrorAction SilentlyContinue
    }
    if ($live -and -not $live.HasExited) { Stop-Process -Id $live.Id -ErrorAction SilentlyContinue }
    foreach ($name in $names) { [Environment]::SetEnvironmentVariable($name, $previous[$name], 'Process') }
}
