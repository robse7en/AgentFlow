The Windows desktop uses the shared Rust core, an ephemeral loopback API, the existing Vue UI,
single-instance focus and embedded terminals. It remains a separate Cargo workspace.

Run the Windows installer `AgentFlow_1.1.13_x64-setup.exe`. It installs for the current user,
adds Start menu integration and includes the UI, `flowctl` and Microsoft's WebView2 bootstrapper.
When WebView2 is absent, its bootstrapper downloads the runtime. This build is unsigned.
Python, Node.js and Zellij are not needed on a user's machine. Git and an installed, signed-in
coding CLI are required for coding work, but not to finish setup and look around: before an agent's
first run, AgentFlow shows the CLI's install and sign-in steps and asks you to confirm.

On first launch, choose **New installation** or **Use an existing instance**. An already configured
home instance opens directly. New setup collects the CLI and billing source, and optionally a default
repository and Jira Cloud or Server/Data Center credentials, assigning all five roles to the selected
CLI. Without Jira, tasks start from a prompt (local tasks). Setup checks the fields locally; it does
not claim a live Jira or MCP authentication check. After an error it fills your choices back in, but
never the Jira token. Setup also opens when the configuration cannot be read, or when Jira is enabled
without its URL or token; repairing with the Jira fields blank turns such an incomplete Jira section
off (in the default configuration; fix another selected instance's file by hand, as below). The setup page gives installation commands for missing Git/CLIs and the Jira Cloud MCP endpoint
for user-scope setup. Use Settings or the existing config format for optional integrations and
advanced role settings.

Closing the window while agents or verification commands are running asks first and names the
tasks; **No** keeps working. **Yes** stops them, keeps task history and files, and quits. If the
window loses its connection to the local service, **Retry connection** pairs it again without
stopping agents, and **Export diagnostics** saves the diagnostic log.

Agents run in built-in terminals by default, which end with the app. To run them in Zellij instead
(0.44 or newer), as the web app does, set `"useInDesktop": true` in the `zellij` section of
`config.json` and restart AgentFlow. Startup refuses to continue when Zellij is missing or too old.
In this mode the desktop serves the configured `server.host`/`server.port` rather than a random
port, so agents keep running after the app closes and can report back after it reopens: their
steps show as interrupted, and **Resume** reattaches each surviving agent. Quitting asks first and
stops only verification commands. Settings shows which terminal is in use.

For an existing source desktop or web server, close it first, then select its `config.json` using
the full path. Setup detects the original checkout for relative paths; for a custom config outside
that checkout, supply the old base directory. The original config, database, task files and
worktrees stay in place. Keep those folders after installing. Only their location is recorded in
`~/.agentflow/desktop-instance.json` (or the `AGENTFLOW_HOME` folder). `AGENTFLOW_CONFIG` takes
precedence over this saved selection. To select another instance, choose **Change instance…** in
Settings: AgentFlow asks first (stopping any running agents), then restarts into setup with the
current selection filled in. Closing setup without saving keeps the current instance. Launching
`agentflow-desktop.exe --setup` from the installation folder, with AgentFlow closed, does the same.
In setup, **New installation** (and its update option) always uses the default
`~/.agentflow/config.json` (or the `AGENTFLOW_HOME` folder), never another selected instance's
config.json. To fix that file, edit it by hand and choose it again as an existing instance.

Re-running the installer upgrades the application files and keeps user data. Installation and
uninstall refuse to force-close this executable when it is running. A desktop also refuses a
database already locked by another desktop, or a configured web-server port that is still in use.
Uninstall retains configuration and data, including the selected-instance record. An explicit
setup repair keeps unrelated settings and writes a uniquely named backup before updating the config.

Build the installer from the checkout root with PowerShell:

```powershell
.\scripts\build-desktop-installer.ps1 -InstallTools
```

`-InstallTools` downloads the pinned official Tauri CLI into `.desktop-tools`; omit it on later
builds. Tauri downloads its NSIS tooling there on first use. The script builds the frontend, shared
Rust binaries and desktop in order, uses locked offline Cargo dependencies, and stops on failure.
It needs Node/npm, Rust/Cargo, the Windows C++ build tools (see
[Building the Rust code](../../docs/development.md#building-the-rust-code)) and frontend dependencies installed with
`npm ci` in `frontend`. Outputs and SHA-256 files are under
`app/src-tauri/target/installer/release/bundle/nsis/`. `-SkipBuild` reuses the existing frontend and
release `flowctl` inputs while still compiling the desktop and packaging it.

## Over-the-air updates

A build packaged with an update source checks it a minute after an instance opens and every six
hours, and from **Check for updates…** in Settings. A newer version is downloaded and its signature
checked, then AgentFlow asks before installing. Accepting it goes through the same "active work"
confirmation as closing the window. AgentFlow then stops agents, closes, runs the installer
(passive, with a progress bar) and reopens. A session restarted by **Change instance…** is not
reopened automatically, because it would return to setup. Builds without an update source never check.

Official releases update from this repository's GitHub releases: the app reads `latest.json` from
the release marked Latest (`releases/latest/download/latest.json`), and that file points at the
installer under its own tag. Published installers from 1.1.13 on are built this way. The public
installers 1.1.6 to 1.1.12 have no updater, so install 1.1.13 or later from the releases page once.

Once, generate the updater signing key. It is separate from Windows code signing, and updates
require it. Keep the private key and its password out of the repository and backed up: without
them, installed copies can never update again.

```powershell
.\.desktop-tools\node_modules\.bin\tauri.cmd signer generate -w $HOME\.tauri\agentflow.key
```

For each release, bump the version, tag it, push the commit and tag, then build against the
repository:

```powershell
$env:TAURI_SIGNING_PRIVATE_KEY_PASSWORD = '<key password>'
.\scripts\build-desktop-installer.ps1 -GitHubRepo robse7en/AgentFlow -ReleaseNotes 'What changed'
```

This writes the installer, its `.sig` and `.sha256`, and `latest.json` into the bundle folder.
Publish them as a draft first, and publish the release as Latest only once every file is attached:

```powershell
gh release create v<version> --verify-tag --draft --title 'AgentFlow <version>' --notes-file <notes>
gh release upload v<version> <bundle folder>\AgentFlow_<version>_x64-setup.exe `
    <bundle folder>\AgentFlow_<version>_x64-setup.exe.sig `
    <bundle folder>\AgentFlow_<version>_x64-setup.exe.sha256 <bundle folder>\latest.json
gh release edit v<version> --draft=false --latest
```

Every release marked Latest must carry `latest.json`. If it does not, every installed copy reports that
it could not check for updates until a release that does becomes Latest. Pre-releases and drafts are
never offered.

The update source is a build setting merged over `tauri.conf.json`, so the checked-in configuration
names none, and a build from a fork never checks this repository. The key defaults to
`$HOME\.tauri\agentflow.key` and its `.pub` (`-UpdateSigningKey`, `-UpdatePublicKey`, or
`TAURI_SIGNING_PRIVATE_KEY`). The installer refuses downgrades.

### Self-hosted update server

`-UpdateUrl` (or `AGENTFLOW_UPDATE_URL`) points a build at your own static server instead. That
server serves `latest.json` and the installers. `deploy/update-server` runs one with Docker on
port 8080 (`AGENTFLOW_UPDATE_PORT`), serving `./releases` (`AGENTFLOW_RELEASES_DIR`):

```powershell
cd deploy\update-server; docker compose up -d
.\scripts\build-desktop-installer.ps1 -UpdateUrl http://<server>:8080 `
    -ReleaseNotes 'What changed' -PublishTo <releases folder>
```

`-PublishTo` copies the installer there first and replaces `latest.json` last. Plain `http` is meant
for a trusted local network: the signature still rejects a tampered installer. Installed copies
keep the source they were built with. To move them to a different source, including from a server
to GitHub, publish one release built for the new source to the old one. AgentFlow 1.1.13 moved the
LAN installs to GitHub this way.
Copies installed before the updater was added (0.1.2 and earlier) must install the first updater
build by hand.

For an executable without an installer, use `.\scripts\rebuild-desktop.ps1` (add `-Release`).
This selects Git Bash explicitly, since plain `bash` on Windows can invoke WSL.

Verify a separately named installer without replacing the normal installation:

```powershell
.\scripts\build-desktop-installer.ps1 -ValidationPackage
.\scripts\build-desktop-installer.ps1 -ValidationPackage -PreviousValidationVersion -SkipBuild
.\app\src-tauri\scripts\verify-installer.ps1
```

The verifier requires PowerShell 7 and the checkout's Python venv for its SQLite fixture. It
installs into a GUID-named workspace folder with shortcuts disabled and a distinct product,
binary and application identity. It uses placeholder CLIs and Jira settings; it makes no live
provider calls. It checks installed resource bytes, fresh setup, rejection of invalid token/origin/
Host requests, existing config selection with relative paths, historical task visibility, restart,
same-version reinstall, a 0.1.0-to-1.1.13 upgrade, downgrade refusal, refusal to force-close a running
app, and uninstall with `/UPDATE` data retention. It leaves JSON reports under `.installer-test-tmp`.
Temporary validation registry entries are scoped to the validation product. The normal AgentFlow
desktop remains independent.
The older-version fixture uses the same source, so it tests installer version handling rather
than claiming additional legacy backend parity.

The real installed WebView renders Vue, pairs, clears its pairing fragment, calls the authenticated
API, reloads settings and connects to the WebSocket during these checks. Setup is submitted through
its real form endpoint while the window is hidden, preserving its no-script CSP. Explicit test mode
is `--setup-smoke-test REPORT_PATH`; it writes a local setup URL sidecar for the verifier. Ordinary
launches have no test reporting. The existing `app/src-tauri/scripts/smoke.ps1` checks a built source
executable.

Live Jira/Slack/GitHub/model authentication, a real coding ticket, manual visible-window usability,
code signing and publishing updates from CI remain separate release acceptance work.
