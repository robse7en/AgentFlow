# Installing AgentFlow

There are two ways to run AgentFlow: the Windows desktop app, or the Python backend and Vue UI from a
source checkout (Windows, macOS and Linux). Both run the same workflow.

## Windows desktop app

Windows desktop users can use the **AgentFlow setup EXE** for new installation or a data-preserving
upgrade. First launch can select an existing web/desktop `config.json` or create a new configuration.
Download `AgentFlow_<version>_x64-setup.exe` from the [latest release](https://github.com/robse7en/AgentFlow/releases/latest).
The installer is unsigned, so Windows SmartScreen may warn before it runs: choose *More info → Run anyway*.
See [desktop installation, migration and installer builds](../app/src-tauri/README.md).
See [version history and release tagging](../CHANGELOG.md) for desktop release changes.

## Prerequisites (running from source)

The [guided setup](#guided-recommended) checks all of these and offers to install whatever is missing, asking first.

| Requirement | Notes |
|---|---|
| Python **3.12+** | `uv` is the easiest way to get one; the setup scripts use it (`uv venv --python 3.13` downloads Python). |
| Node 18+ / npm | for the Vue frontend (Copilot CLI itself needs Node 22+) |
| Git | on `PATH` |
| [zellij](https://zellij.dev) | **0.44+** (needs `subscribe --format json` for the embedded read-only pane view, plus `zellij action write-chars --pane-id`, `list-panes --json` and `attach --create-background`). Windows builds work. |
| At least one coding CLI | `claude`, `codex`, `copilot`, or any CLI you configure as `generic-cli` |
| **Jira** (optional) | Jira Cloud (account email + API token) or Server/Data Center (personal access token). Without it, create local tasks from a prompt; see [AgentFlow's own Jira REST integration](configuration.md#agentflows-own-jira-rest-integration-optional). |
| [GitHub CLI](https://cli.github.com) (optional) | `gh auth login`ed; lets AgentFlow detect PRs and merges itself (see [GitHub PR sync](configuration.md#github-pr-sync)) |
| [ffmpeg](https://ffmpeg.org) (optional) | keyframes from video attachments on Jira tickets |

The coding CLIs must be **signed in** — AgentFlow just launches them and never manages API keys. The setup
wizard tells you how to sign in to each one and waits while you do.

## MCP prerequisites (important)

AgentFlow's own [Jira client](configuration.md#agentflows-own-jira-rest-integration-optional) covers search, the ticket's text and attachments, and two
status transitions. Linked issues, custom fields and write-back are left to the harness: the Architect is
told to *use its configured Jira MCP* for those, and agents use their own Git/filesystem tools. AgentFlow
implements no MCP itself. So, in every harness you intend to use:

```
Claude Code   → Jira MCP configured (+ Git MCP if you use one)
Codex         → Jira MCP configured
Copilot       → relevant MCPs configured
```

For Jira Cloud, `agentflow setup` offers to add Atlassian's official MCP server
(`https://mcp.atlassian.com/v2/mcp`) to each harness an agent role uses. It asks first, and uses the
harness's own command: `claude mcp add --transport http --scope user atlassian <url>`,
`codex mcp add atlassian --url <url>`, or `copilot mcp add --transport http atlassian <url>`. The entry
goes into your **user-level** harness settings, because agents run in worktrees of any repository, so
your own sessions of that harness get it too. Sign-in is OAuth in the browser, done inside the harness:
`/mcp` in Claude Code, `codex mcp login atlassian` (setup offers to run it), or `/mcp auth atlassian` in
Copilot. Setup never changes or removes an existing Jira/Atlassian entry. For Jira Server/Data Center,
or a generic CLI harness, configure a Jira MCP by hand.

If the harness assigned to the Architect has no Jira MCP, discovery will not be able to read linked
issues or custom fields. The [REST integration](configuration.md#agentflows-own-jira-rest-integration-optional) still covers the title, description, comments and
attachments.

## Install from source

### Guided (recommended)

```bash
git clone https://github.com/robse7en/AgentFlow.git agentflow
cd agentflow
./setup.sh                                                  # macOS / Linux
powershell -ExecutionPolicy Bypass -File setup.ps1         # Windows (PowerShell 5.1 or 7)
```

The script gets Python ready: it uses `uv` if present, otherwise a Python 3.12+ on `PATH`, otherwise it
offers to install `uv`. It creates `backend/.venv` and installs the backend, then hands over to
`agentflow setup` (see [Configuration](configuration.md)). Any arguments are passed through, e.g. `./setup.sh --config ~/agentflow.json`.
The wizard then:

1. checks git, Node.js and zellij, offers to install any that are missing, and builds the UI;
2. lets you pick the coding CLIs, offers to install them, and waits while you sign in to each;
3. assigns the five agent roles, optionally signs in to Jira, and writes `config.json`;
4. offers to add the Jira MCP to each harness (see *MCP prerequisites*), and to add the AgentFlow folder
   to the trusted folders of each coding CLI whose config exists (`~/.claude.json`, `~/.codex`, `~/.copilot`);
5. optionally sets up gh, ffmpeg and Slack, then runs `agentflow doctor`.

Installs use winget on Windows, Homebrew on macOS, and on Linux the distro's package manager, the
vendor's install script or, for zellij, the official release binary in `~/.local/bin`. Each one shows
the exact command first and defaults to **no**. Where there is no dependable automated route, such as
Node.js and gh on Linux, the wizard links to the download page instead. Run the script again at any time.
Every answer is prefilled, so it changes only what you change.

### Manual

```bash
# backend (creates a Python 3.13 venv; system Python 3.11 is not enough)
cd backend
uv venv --python 3.13 .venv
uv pip install --python .venv/Scripts/python.exe -e .        # Windows
# uv pip install --python .venv/bin/python -e .              # macOS / Linux
cd ..

# frontend
cd frontend
npm install
npm run build          # produces frontend/dist, served by the backend
cd ..
```

Without `uv` (note the `./` — plain `backend` would resolve to an unrelated PyPI package):

```bash
python3.13 -m venv backend/.venv
backend/.venv/Scripts/pip install -e ./backend      # Windows
backend/.venv/bin/pip install -e ./backend          # macOS / Linux
```

A venv made by `uv venv` has no `pip`: install extras such as Slack with
`uv pip install --python <venv python> -e "backend[slack]"`. Then write `config.json`. Either run
`backend/.venv/Scripts/agentflow setup` (or `backend/.venv/bin/agentflow setup`), or copy
`config.example.json` and edit it (the `jira` section is optional).

The backend installs two console scripts into the venv: `agentflow` (the server) and `flowctl` (the agent CLI).
AgentFlow also writes `flowctl` shims into `<dataDirectory>/bin` and puts them on the PATH of every agent tab,
so agents can call `flowctl` no matter how the venv was created.

## Starting AgentFlow

```bash
# backend — serves the API and, when frontend/dist exists, the UI on the same port
cd backend
.venv/Scripts/python.exe -m agentflow.main            # Windows
# .venv/bin/python -m agentflow.main                  # POSIX
# or simply:  agentflow   (console script; --config, --host, --port, --reload)
```

Open <http://127.0.0.1:8765>.

Or run `./start.sh` (Git Bash on Windows). It rebuilds the UI when its sources changed, stops a previous
instance, starts the backend and opens the browser. When there is no `config.json` yet, or Jira is
enabled without its settings, it runs `agentflow setup` first. The backend itself refuses to start with
Jira enabled but incomplete, and names the missing settings; pass `--skip-jira-check` only for
development and tests.

To check the setup without starting anything, run `agentflow doctor` (same `--config` option). It
checks git, Node.js, zellij 0.44+, the harness each agent role uses, signs in to Jira, and reports
whether each harness has a Jira MCP, plus gh, ffmpeg and Slack when those are enabled. It changes
nothing and exits non-zero when a required check fails.

For frontend development, see [Developing AgentFlow](development.md#frontend-development).
