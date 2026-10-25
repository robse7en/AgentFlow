param(
    [switch]$InstallTools,
    [switch]$SkipBuild,
    [switch]$ValidationPackage,
    [switch]$PreviousValidationVersion,
    # Update server the installed app checks, e.g. http://192.168.1.20:8080. Without it (and
    # without AGENTFLOW_UPDATE_URL) the build has no updater and never checks.
    [string]$UpdateUrl = $env:AGENTFLOW_UPDATE_URL,
    # Public half of the updater signing key (`tauri signer generate`).
    [string]$UpdatePublicKey = $(if ($env:AGENTFLOW_UPDATE_PUBLIC_KEY) { $env:AGENTFLOW_UPDATE_PUBLIC_KEY } else { Join-Path $HOME '.tauri/agentflow.key.pub' }),
    # Private half, used when TAURI_SIGNING_PRIVATE_KEY is not set.
    [string]$UpdateSigningKey = $(Join-Path $HOME '.tauri/agentflow.key'),
    [string]$ReleaseNotes = '',
    # Folder the update server serves (its releases volume): receives the installer, then latest.json.
    [string]$PublishTo
)

$ErrorActionPreference = 'Stop'
if ($PreviousValidationVersion -and -not $ValidationPackage) {
    throw '-PreviousValidationVersion is only for the isolated validation package.'
}
if ($PublishTo -and -not $UpdateUrl) { throw '-PublishTo needs -UpdateUrl.' }
$updateBase = $null
if ($UpdateUrl) {
    $uri = $null
    if (-not [Uri]::TryCreate($UpdateUrl.TrimEnd('/'), [UriKind]::Absolute, [ref]$uri) -or $uri.Scheme -notin @('http', 'https')) {
        throw "-UpdateUrl must be an http or https address, not '$UpdateUrl'."
    }
    $updateBase = $uri.AbsoluteUri.TrimEnd('/')
    if (-not (Test-Path -LiteralPath $UpdatePublicKey)) {
        throw "Updater public key not found at $UpdatePublicKey. Run 'tauri signer generate' first, or pass -UpdatePublicKey."
    }
    $updatePublicKeyText = (Get-Content -LiteralPath $UpdatePublicKey -Raw).Trim()
    if (-not $env:TAURI_SIGNING_PRIVATE_KEY -and -not (Test-Path -LiteralPath $UpdateSigningKey)) {
        throw "Updater signing key not found at $UpdateSigningKey. Set TAURI_SIGNING_PRIVATE_KEY or pass -UpdateSigningKey."
    }
}
$workspace = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
$toolRoot = Join-Path $workspace '.desktop-tools'
$cli = Join-Path $toolRoot 'node_modules/.bin/tauri.cmd'
$cargo = Get-Command cargo.exe -ErrorAction SilentlyContinue
if (-not $cargo) {
    $cargoPath = Join-Path $env:USERPROFILE '.cargo/bin/cargo.exe'
    if (-not (Test-Path -LiteralPath $cargoPath)) { throw 'Install Rust/Cargo first.' }
    $env:PATH = (Split-Path $cargoPath -Parent) + ';' + $env:PATH
}
if ($InstallTools) {
    # Explicit opt-in: this command downloads the official CLI into the workspace only.
    & npm.cmd install --prefix $toolRoot --cache (Join-Path $toolRoot 'npm-cache') `
        --no-audit --no-fund --save-exact '@tauri-apps/cli@2.12.1'
    if ($LASTEXITCODE -ne 0) { throw 'Tauri CLI installation failed.' }
}
if (-not (Test-Path -LiteralPath $cli)) {
    throw 'Run this script with -InstallTools once to download the pinned Tauri CLI.'
}
if (-not $SkipBuild) {
    & npm.cmd --prefix (Join-Path $workspace 'frontend') run build
    if ($LASTEXITCODE -ne 0) { throw 'Frontend build failed.' }
    & cargo.exe build --locked --offline --release --workspace --manifest-path (Join-Path $workspace 'app/Cargo.toml') `
        --target-dir (Join-Path $workspace 'app/target')
    if ($LASTEXITCODE -ne 0) { throw 'Rust backend/flowctl build failed.' }
}
# A separate output directory avoids replacing a source desktop executable still in use.
$names = @('CARGO_TARGET_DIR', 'TAURI_BUNDLER_TOOLS_DIR', 'LOCALAPPDATA', 'TEMP', 'TMP', 'TAURI_SIGNING_PRIVATE_KEY')
$previous = @{}
foreach ($name in $names) { $previous[$name] = [Environment]::GetEnvironmentVariable($name, 'Process') }
try {
    $env:CARGO_TARGET_DIR = Join-Path $workspace 'app/src-tauri/target/installer'
    $env:TAURI_BUNDLER_TOOLS_DIR = Join-Path $toolRoot 'bundler'
    # Keep packaging caches and NSIS scratch files inside this checkout.
    $env:LOCALAPPDATA = Join-Path $toolRoot 'local-app-data'
    $env:TEMP = Join-Path $toolRoot 'tmp'
    $env:TMP = $env:TEMP
    New-Item -ItemType Directory -Force -Path $env:LOCALAPPDATA, $env:TEMP | Out-Null
    $arguments = @('build', '--bundles', 'nsis', '--ci')
    $config = $null
    if ($ValidationPackage) {
        $config = Join-Path $workspace 'app/src-tauri/scripts/installer-validation.conf.json'
        if ($PreviousValidationVersion) {
            $validation = Get-Content -LiteralPath $config -Raw | ConvertFrom-Json
            $validation | Add-Member -NotePropertyName version -NotePropertyValue '0.1.0'
            $config = Join-Path $toolRoot 'previous-validation.conf.json'
            [IO.File]::WriteAllText($config, ($validation | ConvertTo-Json -Depth 10))
        }
    }
    if ($updateBase) {
        # The update server is a build setting, merged over tauri.conf.json (and any validation
        # settings) so the checked-in configuration stays free of it.
        $overlay = if ($config) { Get-Content -LiteralPath $config -Raw | ConvertFrom-Json } else { [pscustomobject]@{} }
        $bundle = if ($overlay.bundle) { $overlay.bundle } else { [pscustomobject]@{} }
        $bundle | Add-Member -NotePropertyName createUpdaterArtifacts -NotePropertyValue $true -Force
        $overlay | Add-Member -NotePropertyName bundle -NotePropertyValue $bundle -Force
        $updater = [pscustomobject]@{
            pubkey = $updatePublicKeyText
            endpoints = @($updateBase + '/latest.json')
            # Plain http is for a trusted local network; the signature still protects the installer.
            dangerousInsecureTransportProtocol = ($updateBase -like 'http:*')
            windows = [pscustomobject]@{ installMode = 'passive' }
        }
        $overlay | Add-Member -NotePropertyName plugins -NotePropertyValue ([pscustomobject]@{ updater = $updater }) -Force
        $config = Join-Path $toolRoot 'updater.conf.json'
        [IO.File]::WriteAllText($config, ($overlay | ConvertTo-Json -Depth 10))
        if (-not $env:TAURI_SIGNING_PRIVATE_KEY) {
            $env:TAURI_SIGNING_PRIVATE_KEY = Get-Content -LiteralPath $UpdateSigningKey -Raw
        }
    }
    if ($config) { $arguments += @('--config', $config) }
    if ($PreviousValidationVersion) { $arguments += '--ignore-version-mismatches' }
    $arguments += @('--', '--locked', '--offline')
    Push-Location (Join-Path $workspace 'app/src-tauri')
    try { & $cli @arguments } finally { Pop-Location }
    if ($LASTEXITCODE -ne 0) { throw 'Desktop installer build failed.' }
    $filter = if ($ValidationPackage) { 'AgentFlow Installer Validation_*-setup.exe' } else { 'AgentFlow_*-setup.exe' }
    $bundleDir = Join-Path $env:CARGO_TARGET_DIR 'release/bundle/nsis'
    $packages = Get-ChildItem -LiteralPath $bundleDir -Filter $filter
    foreach ($package in $packages) {
        $hash = (Get-FileHash -LiteralPath $package.FullName -Algorithm SHA256).Hash.ToLowerInvariant()
        [IO.File]::WriteAllText($package.FullName + '.sha256', $hash + '  ' + $package.Name + "`n")
        Write-Output $package.FullName
    }
    if ($updateBase) {
        $settings = Get-Content -LiteralPath (Join-Path $workspace 'app/src-tauri/tauri.conf.json') -Raw | ConvertFrom-Json
        $overlay = Get-Content -LiteralPath $config -Raw | ConvertFrom-Json
        $version = if ($overlay.version) { $overlay.version } else { $settings.version }
        $product = if ($overlay.productName) { $overlay.productName } else { $settings.productName }
        $package = Get-Item -LiteralPath (Join-Path $bundleDir ($product + '_' + $version + '_x64-setup.exe'))
        $manifest = [ordered]@{
            version = $version
            notes = $ReleaseNotes
            pub_date = (Get-Date).ToUniversalTime().ToString('yyyy-MM-ddTHH:mm:ssZ')
            platforms = [ordered]@{
                'windows-x86_64' = [ordered]@{
                    signature = (Get-Content -LiteralPath ($package.FullName + '.sig') -Raw).Trim()
                    url = $updateBase + '/' + [Uri]::EscapeDataString($package.Name)
                }
            }
        }
        $latest = Join-Path $bundleDir 'latest.json'
        [IO.File]::WriteAllText($latest, ($manifest | ConvertTo-Json -Depth 10))
        Write-Output $latest
        if ($PublishTo) {
            New-Item -ItemType Directory -Force -Path $PublishTo | Out-Null
            $PublishTo = (Resolve-Path -LiteralPath $PublishTo).Path
            Copy-Item -LiteralPath $package.FullName -Destination $PublishTo -Force
            # Replace latest.json last and in one step, so clients never see a missing installer.
            $staged = Join-Path $PublishTo 'latest.json.tmp'
            Copy-Item -LiteralPath $latest -Destination $staged -Force
            $published = Join-Path $PublishTo 'latest.json'
            if (Test-Path -LiteralPath $published) { [IO.File]::Replace($staged, $published, [NullString]::Value) }
            else { Move-Item -LiteralPath $staged -Destination $published }
            Write-Output ('Published ' + $package.Name + ' to ' + $PublishTo)
        }
    }
}
finally {
    foreach ($name in $names) { [Environment]::SetEnvironmentVariable($name, $previous[$name], 'Process') }
}
