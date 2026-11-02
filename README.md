# AgentFlow — Local Human-Gated AI Engineering Workflow

AgentFlow orchestrates existing AI coding CLIs (Claude Code, Codex, GitHub Copilot CLI, or any
generic CLI agent) through a fixed, human-gated software-engineering workflow for one task, either a
Jira ticket or a prompt you write (a *local task*):

Windows desktop users can use the **AgentFlow setup EXE** for new installation or a data-preserving
upgrade. First launch can select an existing web/desktop `config.json` or create a new configuration.
See [desktop installation, migration and installer builds](app/src-tauri/README.md).
See [version history and release tagging](CHANGELOG.md) for desktop release changes.

The task dashboard groups stages needing attention across concurrent tickets. Settings includes
optional API budgets, per-harness billing classification, Copilot role preferences and metered
launch approvals. Defaults preserve existing routing and leave budgets disabled. See
[cost controls and accounting](docs/security-and-efficiency.md) for limitations and provider usage imports.

New tasks default to direct implementation for cohesive changes. Choose Parallel in the New Task
form for independently owned work. Both routes retain requirements approval, both reviews and
final human approval. In parallel mode, the Architect (in its discovery conversation) splits the approved scope
after requirements approval into focused subtasks for up to three concurrent implementer workers, each with a separate
context, worktree and Zellij tab. All workers share the Implementer configuration. A final
implementer integrates their checkpoints and runs combined tests before review. Select the
Implementer stage to expand worker cards and access their live terminals.
See [worker orchestration](docs/implementer-workers.md) for configuration and recovery.

See [security, verification and usage controls](docs/security-and-efficiency.md) before upgrading
existing tasks. Requirements approvals now freeze the exact documents; independent verification
commands must be configured for ordinary final approval, and their results are the test evidence reviewers are
shown (the implementer's own summary is presented to them as an unverified claim). Usage stays unknown unless collected
from a configured provider-report command or explicitly imported by the human.

After final approval you still push the branch and open the PR yourself. With the GitHub CLI logged in,
AgentFlow then notices the PR and its merge on GitHub and moves the task to **PR Created** and **Merged**
without you pasting URLs. See [GitHub PR sync](#github-pr-sync). Optional [Slack DMs](#slack) tell you
when a task needs you, and can answer questions about a task's pull request and Jira ticket.

```
Architect discovery ─▶ Requirements approval ─▶ Subtask plan ─▶ Workers ─▶ Integration ─▶ checkpoint SHA
        ▲                                                          │
        │                                        ┌─────────────────┴─────────────────┐
        │                                        ▼                                   ▼
        │                                   Code Review                       Security Review
        │                                        └─────────────────┬─────────────────┘
        │                                                          ▼
        │                                              Consolidate findings
        │                                     blocking? ──yes──▶ Implementer fixes ──▶ new SHA ──▶ reviews again
        │                                        │ no                                   (max N iterations, default 5)
        └── same Architect session ◀─────────────┘
                 Final verification ─▶ risk assessment ─▶ READY_FOR_PR
```

AgentFlow is **not** another coding agent. It owns workflow state, Git worktrees and the zellij
session; the harnesses own the AI interaction (and their own Jira/Git MCPs); you stay in control
through a node-based Vue UI and can always drop into the agent's terminal.

AgentFlow's own backend makes **no LLM calls of its own** — the state machine, prompt templates and
contract validation are plain deterministic Python. Every model call happens inside the harness
process you configured (Claude Code, Codex, Copilot CLI, or a generic CLI); AgentFlow only validates
the *shape* of what a harness reports back (`flowctl`'s structured JSON), never the quality of its
reasoning. The one optional exception is free-text questions in Slack (`slack.ask`, off by default,
see [docs/slack.md](docs/slack.md)). Even then AgentFlow holds no model credentials: it runs your
Claude Code or Copilot CLI headless, with every tool disabled, on your own login.

### Local approval protection and recovery

On backend startup, open the **browser pairing link** printed in its terminal, or copy its one-time
code into the UI. The paired browser receives an HttpOnly, SameSite=Strict session cookie that stays
valid across backend restarts. Pairing again (with the new code from a later start) replaces the
session and signs out the previous browser. Keep the backend terminal separate from agent terminals.
Agent credentials authorize only the assigned task, role and run; they cannot approve, cancel,
change settings or override blockers. `flowctl` commands in generated prompts include the explicit
run ID; use that ID even when a persistent terminal retains an older environment.

This is application-level protection against accidental approvals, not OS isolation from an
unrestricted process running under your account. The human session token lives only in the browser
cookie; the backend stores just its SHA-256 hash (`browser-session` in the data directory), and
neither is written into agent prompts or launcher environments. Agent run
credentials are stored under task data, outside the source worktree. Never commit task data.
The backend accepts loopback host configuration only and validates browser origins.

Requirements and final readiness always require human approval. The legacy
`requireRequirementsApproval` and `requireFinalApproval` configuration fields are retained for
compatibility but no longer disable these gates. Failed or unverified tests, unverified requirements,
unanswered questions, high overall risk, outstanding uncertainty, incomplete assessments and review
blockers prevent ordinary final approval. **Override & approve** requires a reason and records the
blockers and exact commit in the event history, without marking failed evidence as passed.
Worktree drift after review — uncommitted changes or new commits since the reviewed checkpoint —
is reported as a blocker too. **Discard drift** restores the reviewed checkpoint when only
tracked files were modified (a background build, codegen, an IDE); it refuses if untracked files
appeared or real commits were added. Otherwise create a new checkpoint and review it, or override with a reason: the
override records the drift and the reviewed commit in the event history. A stale checkpoint
reference (the browser is behind the task) is never overridable — refresh first.

At the review limit, either run the implementer for another numbered attempt or explicitly override
review blockers to request a final assessment.

Nothing in the fix loop is resolved silently. In each fix round the implementer must answer every
blocking finding in `findingResponses` (`fixed` or `disputed`, with a rationale). Each follow-up
reviewer must give a verdict on every one of its previous blocking findings in `priorFindings`
(`fixed`, `not_fixed` or `withdrawn`, with evidence). A `not_fixed` finding is re-raised with
`priorId` and keeps its id. AgentFlow refuses a result that skips any of this, while the agent is
still running, so the agent can resubmit. Blockers that were confirmed fixed show as `resolved` and
dropped ones as `withdrawn`, each with the reviewer's evidence. Other findings from older rounds are
labelled `superseded`, not claimed to be fixed. The loop stops early with `NEEDS_HUMAN_REVIEW` when
a blocking finding is raised a third time, or when the reviewer upholds a finding the implementer
disputed.

**Cancel task** closes the tracked agent tabs and verifies they disappeared before marking the task
cancelled. It preserves the worktree, branch, artifacts and non-agent tabs. If stopping a tab fails,
the UI reports the error and cancellation can be retried. Processes deliberately detached from an
agent's terminal are outside this terminal-based lifecycle control.

After a backend restart, active runs become `INTERRUPTED`. In the node configuration, choose
**Resume existing agent** or **Stop & retry**. Recovery never silently launches another agent.
Older runs without scoped credentials require Stop & retry. Database upgrades add credential and
process-identity tables without rewriting existing task records.

### Retry briefings

When a stage is run again after its last run in the same review round failed (the harness exited,
the agent reported `flowctl fail`, a launch failed, or a human chose **Stop & retry**), the new
briefing ends with a *Previous attempt at this stage* section. It lists:

- how many attempts failed in a row and how the latest one ended (its failure message or exit code);
- its last heartbeat phase;
- the questions the human answered during those attempts, with the answers.

For an implementer or worker it also lists what the attempts left in the worktree: files changed by
commits since the first failed attempt started, and `git status --porcelain` for uncommitted
changes. The new agent is told to continue from that work rather than start over. Each implementer
run records its starting HEAD as `startSha` in its run's `config.json`. A run from before this
feature has none, so its retry lists only uncommitted changes (a worker falls back to its base
checkpoint). The section lists file names, not diff content. A reviewer's retry is told that the
findings its failed attempt reported were discarded and must be reported again. Failure messages
and questions appear fenced, marked as evidence, not instructions, and quoted verbatim (AgentFlow
adds `--run` only to its own `flowctl` examples). If git cannot list the commits or the uncommitted
changes, that list is replaced by a pointer to the git command to run, the other list still
appears, and the launch goes ahead. A resumed conversation gets the same section, as restated
context.

### Automatic retries

Off by default. Set `workflow.maxAutomaticRetries` (0–3) in `config.json` to have AgentFlow
relaunch a stage when its harness process exits without reporting `flowctl complete`. The node
shows *automatic retry 1 of N starts shortly*, and about five seconds later the stage relaunches
with a fresh conversation and the retry briefing above. The task log records each automatic
retry.

- **What it covers:** only a harness exit before completion. It never covers an agent that ran
  `flowctl fail`, a launch that failed, an interrupted run (backend restart), a quiet or overdue
  agent, or a task that is not Active (cancelled or awaiting human review).
- **How many:** a stage is retried while it has failed at most N times in a row in the current
  review round. The count is taken from its runs, so a human's Retry does not reset it.
- **Same checks as a human's Run:** global capacity, budgets and metered approval still apply.
  An automatic retry carries no approval, so a role that needs metered approval is never retried
  automatically. When a check blocks the retry, the stage stays Failed and says why. Each retry
  is a new run, and is charged like one.
- **A human acts first:** a Retry, cancel or other change during the pause wins, and the automatic
  retry does nothing. A pending retry is dropped if the backend stops during the pause.
- **Not detected:** an agent closed on purpose in its terminal, for example with `/exit` or
  Ctrl-C, looks the same as a crash and is retried. Cancel the task, or leave the setting at 0,
  if you close agents by hand.

Key rules it enforces:

- one Jira ticket = one primary Git worktree plus isolated worker worktrees — agents never
  run in your original checkout;
- one Jira ticket = one zellij session (`agentflow-<KEY>`) with one tab per agent;
- agents may use different harnesses; conversations resume by exact ID when available, otherwise from a fresh briefing;
  reviewers start each review round in a fresh conversation so an earlier round's view doesn't anchor the next;
- agents talk to AgentFlow only through `flowctl` and structured JSON — no terminal parsing;
- reviewers only inspect; both reviewers review the same checkpoint SHA and AgentFlow rejects mismatches;
- the review/fix loop is counted by AgentFlow; after the maximum, or when a finding keeps coming back or is disputed and
  upheld, it stops with `NEEDS_HUMAN_REVIEW`;
- nothing destructive: existing branches/worktrees are never overwritten, PRs are never created
  (PR sync only reads from GitHub).

---

## 1. Prerequisites

The guided setup (§2) checks all of these and offers to install whatever is missing, asking first.

| Requirement | Notes |
|---|---|
| Python **3.12+** | `uv` is the easiest way to get one; the setup scripts use it (`uv venv --python 3.13` downloads Python). |
| Node 18+ / npm | for the Vue frontend (Copilot CLI itself needs Node 22+) |
| Git | on `PATH` |
| [zellij](https://zellij.dev) | **0.44+** (needs `subscribe --format json` for the embedded read-only pane view, plus `zellij action write-chars --pane-id`, `list-panes --json` and `attach --create-background`). Windows builds work. |
| At least one coding CLI | `claude`, `codex`, `copilot`, or any CLI you configure as `generic-cli` |
| **Jira** (optional) | Jira Cloud (account email + API token) or Server/Data Center (personal access token). Without it, create local tasks from a prompt; see *AgentFlow's own Jira REST integration* below. |
| [GitHub CLI](https://cli.github.com) (optional) | `gh auth login`ed; lets AgentFlow detect PRs and merges itself (see *GitHub PR sync*) |
| [ffmpeg](https://ffmpeg.org) (optional) | keyframes from video attachments on Jira tickets |

The coding CLIs must be **signed in** — AgentFlow just launches them and never manages API keys. The setup
wizard tells you how to sign in to each one and waits while you do.

### Building the Rust code (contributors)

Only needed to build from source: the desktop app, the Rust `flowctl`, or `cargo test` in `app/`. The
desktop installer ships prebuilt binaries, so installing AgentFlow never compiles anything.

Install Rust with [rustup](https://rustup.rs). On Windows, Rust links with Microsoft's linker and the
Windows SDK, and some dependencies (the bundled SQLite, `ring`) compile C code, so you also need the
**MSVC C++ build tools**, either of:

- Visual Studio (Community is enough): in the Visual Studio Installer choose *Modify* and tick the
  **Desktop development with C++** workload.
- The standalone Build Tools, from a terminal:
  `winget install Microsoft.VisualStudio.2022.BuildTools --override "--passive --wait --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended"`

Without them `cargo build` stops at the link step, with `linker 'link.exe' not found` or, when Git for
Windows puts its own `link` first on `PATH`, `link: missing operand`. `cargo build` in `app/` checks the
setup.

### MCP prerequisites (important)

AgentFlow's own Jira client (below) covers search, the ticket's text and attachments, and two
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
issues or custom fields. The REST integration below still covers the title, description, comments and
attachments.

### AgentFlow's own Jira REST integration (optional)

Jira is optional. With `jira.enabled` false (the default), AgentFlow starts normally and you create
local tasks from a prompt; you can still enter a Jira key by hand and let the Architect read the ticket
through its Jira MCP. To turn the integration on, `agentflow setup` asks whether to configure Jira,
writes `jira.enabled`, `jira.baseUrl` and `jira.apiToken`, and checks that they sign in. Once
`jira.enabled` is true, the backend refuses to start (and *Reload Configuration* refuses a file) until
`jira.baseUrl` and `jira.apiToken` are set; `start.sh` runs the wizard first in that case. Jira Cloud
also needs `jira.email`; Server/Data Center uses a personal access token with no email. For
development and tests only, start with `agentflow --skip-jira-check` (or `AGENTFLOW_SKIP_JIRA_CHECK=1`),
which also covers `--reload` and config reloads. Startup only checks that the settings are present;
`agentflow doctor` signs in for real.

With it, AgentFlow itself, rather than the harness, talks to Jira for two things:

- **Search when creating a task** — the New Task form gets a type-ahead search box (Jira's own
  `issue/picker` endpoint) instead of requiring you to know the exact key.
- **The ticket synced to disk before every Architect run** — at task creation and again before
  discovery, each revision and final verification, so the Architect never plans from a stale
  snapshot. Under the task's `context/` directory:
  - `attachments/` — every attachment, downloaded once (later syncs fetch only what's new);
    attachments deleted from the ticket move to `attachments-removed/`.
  - `jira-issue.md` — summary, description and comments as plain text, with each embedded
    screenshot or video replaced by an inline marker such as `[image: attachments/60578-….png]`, so
    the Architect sees which screenshot illustrates which step.
  - `frames/<id>/` — keyframes (scene changes, or evenly spaced for a static recording; up to
    `jira.maxFramesPerVideo`, default 20) extracted with **ffmpeg** from each video attachment, since
    agents can view images but not video. Needs `ffmpeg`/`ffprobe` on PATH (or `jira.ffmpegPath`);
    without them, videos are still downloaded and the task log says why there are no frames.
    Extraction shares one `jira.frameExtractionTimeoutSeconds` budget (default 120) per sync, so a
    ticket with many videos never holds up a launch for long; any left over are done next sync.
  - `jira-manifest.json` — what was synced, skipped or removed, and what's new since the last run.

  The prompt lists all of this and flags attachments new since the Architect's last run. A file over
  `jira.maxAttachmentBytes`, beyond `jira.maxAttachments`, or failing to download is skipped and
  listed (with the reason) rather than stopping the rest. The whole sync is best-effort and
  non-fatal: a task still gets created and the Architect still starts if Jira is unreachable, and it
  falls back to its MCP. Set `jira.fetchComments` to `false` to leave comments to the MCP.
- **Optional: auto-transition the ticket when you click Run on the Architect.** Set
  `jira.autoTransitionStatus` (e.g. `"In Progress"`) and it fires the moment you start discovery,
  only when the ticket's current status is in `jira.autoTransitionFromStatuses` (default `["To Do"]`)
  — so it won't touch a ticket someone's already moved elsewhere. It never blocks or fails the run:
  the Architect always starts, and a Jira problem just logs a warning on the task.
- **On by default: move the ticket to "In Code Review" when its pull request is created.** When a task
  becomes PR Created (you click *Mark PR created*, or GitHub PR sync finds the PR), AgentFlow moves
  the ticket to `jira.prCreatedTransitionStatus` (default `"In Code Review"`; `null` turns it off)
  from any status in `jira.prCreatedTransitionFromStatuses` (default `["*"]`, any status; list names
  such as `["In Progress"]` to narrow it). A PR first seen already merged doesn't move the ticket.
  It runs after the PR status is saved, so it never blocks or undoes it. Each outcome goes on the
  task's events, including a warning when the ticket's workflow has no transition to that status.
  `"*"` works in `autoTransitionFromStatuses` too.

Jira Cloud uses Basic auth (`email` + an API token from *Jira Settings → Security → API tokens*);
for Server/Data Center, leave `email` unset and put a Personal Access Token in `apiToken` (sent as a
Bearer token instead). The token is only ever read by the backend — it is never sent to the browser
UI or to any harness.

---

## 2. Installation

### Guided (recommended)

```bash
git clone <this repo> agentflow
cd agentflow
./setup.sh                                                  # macOS / Linux
powershell -ExecutionPolicy Bypass -File setup.ps1         # Windows (PowerShell 5.1 or 7)
```

The script gets Python ready: it uses `uv` if present, otherwise a Python 3.12+ on `PATH`, otherwise it
offers to install `uv`. It creates `backend/.venv` and installs the backend, then hands over to
`agentflow setup` (§3). Any arguments are passed through, e.g. `./setup.sh --config ~/agentflow.json`.
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

---

## 3. Configuration — `config.json`

When running from this source checkout, AgentFlow uses (and creates on first launch) the `config.json` in
the AgentFlow folder. An existing `~/.agentflow/config.json` from an older install is still used while the
AgentFlow folder has none; installed copies without a source checkout always use it. A complete
example is in [`config.example.json`](config.example.json). Configuration is validated with Pydantic;
an invalid file makes the backend refuse to start (and *Reload Configuration* in the UI reports the error).

A new config written by `agentflow setup` or on first launch keeps the database, task files and worktrees
in `data/.agentflow` inside the AgentFlow folder (e.g. `D:\AgentFlow\data\.agentflow`, ignored by git).
Relative paths in `app` and `workspace` are relative to the AgentFlow folder, not to where AgentFlow was
started, so the data moves with the folder. An existing config keeps its paths, and one that leaves
them out still uses `~/.agentflow`. Installed copies without a source checkout also default to `~/.agentflow`.

To write it interactively, run `agentflow setup` (same `--config` option). It checks the prerequisites,
lets you pick the harnesses and assigns the five agent roles to them, optionally signs in to Jira, and
optionally sets up Slack. Each prompt is prefilled from the existing file, and pressing Enter keeps the value, so
re-running it changes only what you change. It validates the result, keeps the previous file as `config.json.bak`,
writes the new one, offers to add the Jira MCP to each harness (see "MCP prerequisites"), and then runs
`agentflow doctor`. Before the doctor run it also offers to trust the AgentFlow folder in Claude Code
(`hasTrustDialogAccepted` in `~/.claude.json`), Codex (`[projects.'<folder>']` in `config.toml`) and Copilot
(`trustedFolders` in `config.json`), following `CODEX_HOME`/`COPILOT_HOME` in a harness's `env`. It only
touches CLIs that are already set up, and leaves existing entries (even untrusted ones) as they are. When a tool is missing it offers to install it (§2). It also points each Claude Code,
Codex and Copilot harness at its bundled usage collector (`scripts/usage_<type>.py`) when `usageCommand`
is unset. `agentflow doctor` warns about a Claude Code, Codex or Copilot harness without one.

Override the location with `AGENTFLOW_CONFIG=<file>` or set `AGENTFLOW_HOME=<dir>` to use
`<dir>/config.json`. The command line also accepts `agentflow --config <file>`.

```jsonc
{
  "app":       { "dataDirectory": "data/.agentflow", "databasePath": "data/.agentflow/agentflow.db" },
  "server":    { "host": "127.0.0.1", "port": 8765 },          // local only by default
  "workspace": { "worktreeDirectory": "data/.agentflow/worktrees", "taskDirectory": "data/.agentflow/tasks", "branchPrefix": "" },
  "workflow":  { "maxReviewIterations": 5, "defaultExecutionMode": "manual",
                 "requireRequirementsApproval": true, "requireFinalApproval": true, "commitCheckpoints": true },
  "zellij":    { "command": "zellij", "sessionPrefix": "agentflow-", "webClientEnabled": false,
                 "webClientBaseUrl": "http://127.0.0.1:8082", "shell": null,
                 "headlessCols": 200, "headlessRows": 50,                    // Windows: agent pane size
                 "useInDesktop": false },                                    // desktop app: zellij instead of built-in terminals
  "jira":      { "enabled": true, "baseUrl": "https://yourcompany.atlassian.net",
                 "email": "you@yourcompany.com", "apiToken": "..." },  // required, see §1
  "github":    { "prSync": true, "pollIntervalSeconds": 120, "command": "gh", "hosts": ["github.com"] },
  "harnesses": { "claude": {...}, "codex": {...}, "copilot": {...}, "my-agent": { "type": "generic-cli", ... } },
  "agents":    { "architect": { "harness": "claude", "model": null, "reasoning": "high" }, "implementer": {...},
                 "codeReviewer": {...}, "securityReviewer": {...} }
}
```

An empty `branchPrefix` uses the Jira key itself as the branch name; set a value such as `agent/` to add a prefix.

On Windows, `"server": { "runAsAdministrator": true }` makes `start.sh` restart itself as administrator
(a UAC prompt each launch). Worktree cleanup can then close processes running as administrator that
hold a worktree, such as a shell or agent in an elevated terminal. Everything the backend launches,
including agents and their tools, then runs as administrator too. Off by default.

Task creation can recover a missing, unlocked worktree registration at the requested task path.
Recovery removes only that stale registration, using Git without force or repository-wide pruning;
branch references and commits are kept. If the requested branch already exists, AgentFlow selects the
first unused suffix (`<KEY>-1`, `<KEY>-2`, and so on) and records that actual name on the task.
Locked registrations, untracked detached commits, and existing directories still require inspection.

### GitHub PR sync

AgentFlow tracks each task's pull request on GitHub, so you don't have to paste the PR URL or click
**Mark merged**. It's on by default. It needs the [GitHub CLI](https://cli.github.com) on `PATH` and logged in:

```
gh auth login        # once; AgentFlow uses whatever account gh is logged in as
gh auth status       # should show "Logged in to github.com"
```

**What it does.** It runs once when the backend starts, then every `github.pollIntervalSeconds`, and only
looks at tasks in **Ready for PR** or **PR Created**:

| Task is | AgentFlow asks GitHub | When GitHub shows | The task becomes |
|---|---|---|---|
| Ready for PR | PRs whose head is the task's branch, in the repo `origin` points at (`gh pr list --head <branch>`) | an open PR | **PR Created**, with the PR's URL and creation time |
| | | an already merged PR | **Merged** straight away |
| PR Created | the PR on record, including a URL you typed in (`gh pr view <url>`) | merged | **Merged**, with GitHub's merge time |
| | | closed without merging | unchanged, with a note in the status message (cleared if the PR is reopened) |

A task never moves backwards on its own. PRs from forks are ignored. PRs opened *before* the task was
created are ignored too, so a reused branch name can't pick up an old merged PR. If a branch has more
than one PR, the newest open one wins, then the newest merged one.

**Polling, not webhooks.** AgentFlow listens on loopback only, so GitHub can't send it webhooks. It asks
GitHub instead, so a new PR or merge shows up within one poll interval (about 2 minutes by default; 30
seconds at the minimum). Changes to the `github` settings take effect on *Reload Configuration*, with no
restart. AgentFlow only reads from GitHub. It never pushes, opens, comments on or merges PRs.

**How to tell it worked.** The task's event log shows *Detected the pull request on GitHub* or
*Detected the pull request merge on GitHub*, and the status message reads *Merged on GitHub…* rather
than *Merged…*. The PR panel on the task page also says when AgentFlow will pick up the next step.

**Manual buttons and Undo.** **Mark PR created** and **Mark merged** still work, for PRs anywhere.
**Undo** on either status *pauses* sync for that task, so AgentFlow doesn't re-apply what you just undid.
The task page shows the pause. Marking the PR yourself turns sync back on for that task.

| Setting (`github.*`) | Default | Meaning |
|---|---|---|
| `prSync` | `true` | Set to `false` to turn sync off entirely. |
| `pollIntervalSeconds` | `120` | Time between checks (30–3600). |
| `command` | `"gh"` | GitHub CLI executable, or an absolute path to it. |
| `hosts` | `["github.com"]` | Only remotes and PR URLs on these hosts are checked; add your GitHub Enterprise host here. |

`gh` runs with AgentFlow's reduced environment, plus `GH_TOKEN`, `GITHUB_TOKEN`, `GH_ENTERPRISE_TOKEN`,
`GITHUB_ENTERPRISE_TOKEN`, `GH_HOST` and `GH_CONFIG_DIR` when those are set. Keyring logins from
`gh auth login` work as they are.

### Slack

AgentFlow can DM you in Slack when a task needs you. From Slack you can answer agent questions and
check tasks (`list`, `status`, `digest`). If you allow them, you can also create or start tasks, and
ask about a task in plain words (`slack.ask`).
It is off by default. See [docs/slack.md](docs/slack.md) for setup and every setting.

`slack.details` controls what Slack may post about a task, and what the `ask` model is given. Each
section is opted in separately:

| Section | What is shared |
|---|---|
| `requirements`, `findings`, `errors`, `cost`, `activity` | Requirement excerpts, open review findings, errors, spend and recent events |
| `pr` | The task's pull request URL, when it was created and merged, and whether PR sync is paused |
| `ticket` | The Jira ticket as AgentFlow last synced it: description excerpt and latest three comments |

With `"details": ["pr", "ticket"]`, `details KEY pr` and `details KEY ticket` post those sections. You
can also ask "has JR-9's PR merged?" or "what did the last Jira comment on JR-9 ask for?". Answers come
from AgentFlow's copy (the ticket is synced at task creation and before each Architect run), not live
GitHub or Jira. PRs and tickets that aren't AgentFlow tasks are not visible. Enabling `ticket` copies
Jira text into your Slack history. Slack settings take effect after a backend restart.

### Cleaning up worktrees

Completed tasks keep their worktrees. Once the task is **Merged** (by PR sync or by hand), use
**Clean up worktree** to remove the checkout. Close agent panes first. Cleanup checks the managed task path, registered
branch, final reviewed commit, active runs and panes, and local changes (including untracked and ignored
files).

The task branch must still be at the final reviewed commit, or at the head commit of the PR as GitHub
merged it. The second case covers commits added after final review, such as fixes for PR feedback: once
they're in the merged PR, nothing unmerged is lost. PR sync records that merged head when it detects the
merge. For a task you marked Merged by hand, it looks the head up on the next poll, as long as the PR URL
is a GitHub link and the worktree still exists. A branch with commits beyond the merged PR stays blocked.

If the branch passes but the directory can't be deleted (on Windows, typically a terminal or agent you
started inside the worktree yourself, outside the task's Zellij session), cleanup kills every process
whose working directory is inside the worktree and tries once more, without asking. The task log names
each process it killed. Git removes only the unlocked, clean worktree, without force; the branch and task artifacts remain.
An unsuccessful safety check leaves cleanup blocked with an explanation. A completed cleanup can be retried.

### Harnesses

`command` may be a bare name (resolved on `PATH`) or an absolute path such as `C:/Users/me/.local/bin/claude.exe`.
Each harness accepts `args`, `env`, and — for `generic-cli` — `modelArgument`, `reasoningArgument`,
`promptArgument` (`"positional"` or a flag such as `-i`) and `reasoningMap` (AgentFlow's `low|medium|high` →
the harness' own vocabulary). Built-in adapters:

| type | model flag | reasoning | resume |
|---|---|---|---|
| `claude` | `--model` | `--effort low/medium/high` | `--session-id` recorded at start, `--resume` |
| `codex` | `--model` | `-c model_reasoning_effort="…"` | exact recorded session ID only; otherwise fresh |
| `copilot` | `--model` | `--effort` | exact recorded session ID only; otherwise fresh |
| `generic-cli` | configurable | configurable | none |

Model names are never hard-coded: `model: null` means "the harness' own default".

### Precedence

```
built-in defaults → config.json → task-level overrides (New Task form) → node-level override (Configure)
```

Every node run records the exact harness/model/reasoning it used (`runs/<role>/<nn>/config.json` and the DB).

---

## 4. Zellij setup

Nothing special is needed beyond having `zellij` on `PATH` (or `zellij.command` pointing at it). AgentFlow creates a
**detached** session per task and adds tabs (`Tests`, `Shell`, then `Architect`, `Implementer`, `Code Review`,
`Security` as agents start). Each agent tab runs a generated launcher script (`runs/<role>/<nn>/launch.ps1|.sh`) that
sets `AGENTFLOW_*` environment variables, starts the harness in the worktree (or the project directory inside it),
and reports the process exit code back when the harness ends.

"View pane" opens a live, read-only view of the selected agent pane inside AgentFlow. The task-level "View Zellij in
AgentFlow" button opens the current active pane. This view uses `zellij subscribe` and does not start Zellij's web
server. The terminal drawer's *Open window* button (Windows) opens an interactive window on that agent's tab; you can
also attach manually when you need a full interactive Zellij session:

```
zellij attach agentflow-JR-1234
```

**Windows runs sessions headless.** No console window opens for a task. Each session gets one hidden `zellij attach`
client, because Zellij can only lay out a new tab against an attached client's viewport; its size
(`zellij.headlessCols` × `zellij.headlessRows`, default 200×50) is the size every agent pane gets. A window opened with
*Open window* is maximized so it never shrinks a tab below that size. If the backend restarts, the hidden clients keep
running and are reused.

If you separately run the Zellij web client (`zellij web`), set `zellij.webClientEnabled: true`; the task page then
shows its session URL. This is optional and is separate from AgentFlow's embedded read-only view. Do **not** start the
backend from inside a zellij pane you care about: it scrubs `ZELLIJ_*` variables from child processes, but keeping it
in its own terminal is simplest.

---

## 5. Starting AgentFlow

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

For frontend development run the Vite dev server as well (it proxies `/api` and the WebSocket to the backend):

```bash
cd frontend && npm run dev        # http://127.0.0.1:5173
```

`scripts/dev.ps1` / `scripts/dev.sh` start both.

---

## 6. Creating your first workflow

1. **Tasks → New Task.** Choose the task source. For a **local prompt**, describe the change and what a
   successful result looks like; AgentFlow names the task `LOCAL-<id>` and its first line becomes the title.
   For a **Jira ticket**, enter the key (or, with Jira integration configured, start typing a summary/key into
   the same field for a search dropdown). Then browse to the repository (validation shows *Git repository detected /
   Root / Base branch*), optionally pick a project directory *inside* the repository (stored relative to the root and
   used as the agents' starting directory — they may still touch the whole worktree), choose the base branch and add
   any context. Optionally override harness/model/reasoning per role for this task.
2. **Create Task.** AgentFlow creates a branch based on `<KEY>` (adding `-1`, `-2`, etc. if needed) and
   `<worktreeDirectory>/<KEY>` from the base branch, the task workspace under `<taskDirectory>/<KEY>/`, and the
   zellij session. The task records the selected branch name. It also syncs
   the ticket's attachments, description and comments into `<taskDirectory>/<KEY>/context/` (best-effort;
   failures are logged but don't block task creation), and syncs again before every Architect run.
3. **Run the Architect.** It reads the locally-synced ticket, attachments and video keyframes first, then the rest of the
   ticket through its Jira MCP, explores the code, and asks questions with
   `flowctl question`. The node shows **⚠ INPUT REQUIRED** and the question opens as a decision card over the
   canvas (see [Agent questions](#agent-questions)); answer there, in the **Decisions** inbox, or in Slack.
   AgentFlow sends that one-line answer to the tracked agent pane and marks the agent running again once its last
   question is answered. When done it writes `requirements/*.md` and calls `flowctl complete`.
4. **Approve requirements.** Review requirements, acceptance criteria, assumptions, plan and risks in the side panel.
    The embedded pane is read-only; attach manually for freeform discussion if needed, then *Approve*.
5. **Run the Implementer.** It implements the plan and calls `flowctl complete` with a structured result; AgentFlow
   commits a checkpoint and records the SHA.
6. **Run Code Review and Security Review** (both become READY; run them in any order — they execute in parallel and
   both receive the same SHA). Findings are normalised (`CODE-001`, `SEC-001`, severity, blocking) and persisted.
   The Findings tab shows each finding's code the way GitHub shows a review comment: the flagged lines (`line` to
   `endLine`) in the file as the reviewer saw it, read from the reviewed commit so it still works after cleanup. Switch to
   *Diff* for the task's change to that file (or, for a closed finding, what changed since the review) or *Now* for the
   same place in the task's latest commit. Resolved findings keep their code folded until opened.
7. **Consolidation is automatic.** Blocking findings → the Implementer becomes READY again in fix mode with the
   consolidated package; run it, then the reviews again. No blocking findings → *Final Architect Verification* becomes READY.
8. **Run Final Architect Verification.** A fresh process resumes the exact Architect conversation when supported, or reads a fresh briefing. It writes
   `final/assessment.md` + `assessment.json` (complexity, findings, fixes, tests, regression/security/overall risk,
   human-review recommendation).
9. **Ready for PR.** Confirm the assessment; the task becomes `READY_FOR_PR`. Push the task's branch and open the PR yourself. With GitHub PR sync, the task moves to PR Created and then Merged on its own; otherwise mark each step by hand. Once it's Merged, use **Clean up worktree** to remove the local checkout when its safety checks pass.

If the maximum number of review iterations is reached with blocking findings left, or a blocking finding is raised a
third time, or a reviewer upholds a finding the implementer disputed, the task becomes `NEEDS_HUMAN_REVIEW` and lists
the remaining findings. You may still run the Implementer manually to continue.

Every step is logged in the task's event log (side panel → Events).

### Agent questions

An agent asks with `flowctl question`, either flat (`--message` plus up to eight `--choice`) or as a **decision
tree** (`--file -` with one JSON object on stdin, or a file path):

```json
{"message": "312 inactive accounts still own open invoices. What should the cleanup do?",
 "context": "4,108 accounts inactive for over a year · billing reads invoice.owner_id",
 "risk": "high",
 "choices": [
   {"label": "Soft-delete, keep invoices", "recommended": true,
    "followUp": {"message": "When are they purged for good?", "choices": [
      {"label": "After their invoices close", "recommended": true, "detail": "A nightly job purges them"},
      {"label": "Never", "warning": "Table grows ~4k rows a year",
       "note": "Legal asked for a purge date on all personal data.", "source": "docs/privacy.md"}]}},
   {"label": "Hard-delete everything", "warning": "Breaks history on 312 invoices"}]}
```

`message` is required; `context` (one line, ≤500 characters) and `risk` (`low`/`medium`/`high`) are optional. Up to
eight `choices`, each with a unique `label` (≤250, no `→`) and optionally `recommended` (the agent's pick, one per
level), `detail` (≤200), `warning` (≤120), `note` (≤300, the evidence behind the choice) and `source` (where the note
came from: a repository-relative path or an `https://` URL; needs a note). A top-level choice may carry one `followUp`
with 2–8 choices of its own; nothing deeper. AgentFlow displays notes and sources but never fetches or opens them
itself (an `https://` source is a link the human may follow). A question that breaks a rule is rejected with the field
at fault and is not recorded.

The human answers in one of three places:

- **The task page**: the question opens as a card over the workflow canvas: the agent's pick highlighted, each
  branch's consequences or follow-up beside it, and an *Agent's note* popover on hover or focus. Keys `1`–`N` pick a
  branch and then a follow-up option, `O` opens *Other…* for free text, `Esc` goes back a level. Keys only act while
  the card (or the page itself) has focus, never while you type in a field or in the terminal drawer.
- **Decisions** (sidebar, with a waiting count): every open question across tasks, oldest first, one card at a time;
  `→` shows the next one without answering.
- **Slack** (with `slack.allowActions` including `answer`): one button per branch on the notice; a branch with a
  follow-up (`›`) swaps the notice's buttons for its options and *‹ Change branch*; a leaf sends the answer.

Whatever the surface, the server builds the one line typed into the agent's pane from the stored question:

| Picked | Line |
| --- | --- |
| Top-level choice 2 | `2: Pin pydantic to v2` |
| Choice 1, then option 1 of its follow-up | `1.1: Soft-delete, keep invoices → After their invoices close` |
| *Other…* on a tree | `other: <text>` |
| A flat question | the choice text, or the free text, unchanged |

After sending, the card shows *Sent to {node}*, the node's state afterwards (running again, or still waiting when
the same run asked several questions) and the exact line. Answering the same question again with the same choice is a
no-op; a different answer is refused, so a question answered in Slack closes in the browser and the other way round.
Questions asked before this feature, and flat questions, render in the same card with plain branches.

### Task workspace layout (`<taskDirectory>/<KEY>/`)

```
task.json
context/additional-context.md
requirements/{requirements,acceptance-criteria,assumptions,implementation-plan,risks}.md, questions.json
runs/{architect,implementer,code-review,security-review}/NN/{prompt.md,config.json,launch.*,result.json}
findings/review-NN.json, security-NN.json, consolidated-NN.json
final/assessment.json, assessment.md
```

Nothing is written into your application repository except the code changes on the task branch.

---

## 7. `flowctl` — how agents talk to AgentFlow

Agents run these inside their Zellij pane (task/role/run id come from the environment):

```
flowctl status                                   # workflow state
flowctl question --message "Should this apply to all tenants?"   # → WAITING_FOR_USER
flowctl question --file - <<'EOF'                # a decision tree (see "Agent questions")
{"message": "...", "risk": "medium", "choices": [{"label": "...", "recommended": true}, {"label": "..."}]}
EOF
flowctl running                                  # optional: AgentFlow resumes the agent after the last answer
flowctl complete --file result.json              # structured completion (see prompt for the schema)
flowctl finding --id CODE-001 --severity high --title "..." --file src/x.cs --line 12 --end-line 14   # incremental findings
flowctl finding --prior-id CODE-001 --blocking --title "..."    # re-raise a previous-round blocker
flowctl fail --message "Blocked because ..."
```

`flowctl` is a thin HTTP client for `http://127.0.0.1:<port>/api/tasks/<KEY>/agent/*`; it never touches the database.
The prompts written to `runs/<role>/NN/prompt.md` tell each agent exactly which command and JSON shape to use.

---

## 8. Verifying the installation — smoke test

A stub harness (`scripts/stub_agent.py`) behaves like an interactive agent and lets you exercise the entire engine
without real AI sessions or a Jira MCP:

```bash
backend/.venv/Scripts/python.exe scripts/smoke_test.py     # Windows
backend/.venv/bin/python scripts/smoke_test.py             # POSIX
```

It creates a throw-away repo and isolated `AGENTFLOW_HOME`, starts a backend on port 8799 and walks a task through:
question → answer, approval, implementation, checkpoint, parallel reviews, fix loop, clean reviews, persistent-Architect
final verification, `READY_FOR_PR`; then a second task that hits the iteration cap (`NEEDS_HUMAN_REVIEW`) and the
review-SHA mismatch guard (the mismatch assertion is best-effort: on a slow machine the stub reviewer may report first, in which case that check is skipped and printed as such). Requires git and zellij.

---

## 9. Troubleshooting

| Symptom | What to check |
|---|---|
| `Configuration error` on start | The JSON is invalid or references an unknown harness in `agents.*.harness`. The message names the field. `agentflow setup` fixes harness references. |
| `Jira is enabled but not configured` on start | Run `agentflow setup` (or `start.sh`, which runs it for you) and either complete the Jira settings or decline Jira to use local tasks. For development only: `agentflow --skip-jira-check`. |
| Not sure what's wrong | `agentflow doctor` checks the tools, each harness the roles use, a live Jira sign-in, the Jira MCPs, gh, ffmpeg and Slack, and changes nothing. |
| `setup.ps1 cannot be loaded because running scripts is disabled` | Run it as `powershell -ExecutionPolicy Bypass -File setup.ps1`. |
| A tool the wizard just installed is "not found" in a new terminal | The installer changed `PATH` for new sessions only: open a new terminal. On Linux/macOS, add `~/.local/bin` to `PATH` in your shell profile. |
| `No module named pip` | The venv was made by `uv venv`, which has no pip: use `uv pip install --python <venv python> ...`. |
| `Slack is enabled but its libraries are missing` even after `pip install -e "backend[slack]"` | A bare `pip` installed into a different Python than the backend's venv. Install with the venv's own interpreter, from the project root, then restart: `backend\.venv\Scripts\python.exe -m pip install -e "backend[slack]"` (Windows), `backend/.venv/bin/python -m pip install -e "backend[slack]"` (macOS / Linux). A venv made by uv has no pip: use the `uv pip install --python <venv python> ...` form. |
| Harness shows *unavailable* | `command` is not on `PATH`; set an absolute path. *Re-detect* in Settings after fixing. |
| `zellij session ... did not appear` | `zellij` not on `PATH`, or a stale session with the same name is broken: `zellij delete-session agentflow-<KEY> --force`. |
| Node goes **FAILED: Harness exited (code N)** | The CLI crashed or exited without `flowctl complete`. Open the tab (the shell stays open with the output), fix the cause, press *Retry*. |
| **Review state mismatch** | The reviewer reported a different SHA than the checkpoint. Rerun the review; it must run against the worktree HEAD. |
| Agent says `flowctl` not found | The launcher prepends `<dataDirectory>/bin` and the venv `Scripts` dir to `PATH`; make sure the backend runs from the venv (`.venv/Scripts/python.exe -m agentflow.main`). |
| Branch/worktree already exists | Missing unlocked task registrations at the selected base are recovered automatically. Existing directories, changed commits, locks, and unrelated branches require inspection; no branches are reset or deleted. |
| Architect cannot read the ticket | The Architect's harness has no Jira MCP, or isn't signed in to it (`agentflow doctor` shows which). For Jira Cloud, `agentflow setup` can add it; otherwise configure it in that CLI (linked issues/custom fields always need it; see §1 for what AgentFlow's own Jira integration covers instead). |
| Jira search box missing, or attachments never appear under `context/` | `jira.enabled`/`baseUrl`/`apiToken` are unset, or the credentials are rejected — check the task's event log for a "Could not sync Jira ticket context" entry. A "Jira context synced" entry lists any attachments it skipped and why. |
| Video attachments have no `frames/` | `ffmpeg`/`ffprobe` aren't on the backend's PATH (set `jira.ffmpegPath`), or the video was skipped for size — the "Jira context synced" event says which. |
| Windows: `Hidden Zellij client did not attach` | The session's sizing client couldn't start or attach. Check the `zellij.client.*` diagnostics events; `zellij kill-session agentflow-<KEY>` and retry the node to start fresh. |
| Embedded pane view is unavailable | Check that the agent pane is active and Zellij 0.44+ is installed. Use the `zellij attach ...` command for a full interactive session. |
| UI shows *offline* | The WebSocket to `/api/ws` dropped; it reconnects automatically. In dev, check the Vite proxy target (`AGENTFLOW_API`). |
| Windows: `pwsh` not found in tabs | Set `zellij.shell` to `powershell` (or the path to `pwsh.exe`). |
| Task stays **Ready for PR** after you opened the PR | Wait one poll interval. Then check the backend log for a `GitHub PR sync` warning (`gh` not found, or not logged in: run `gh auth login`). Also check that the PR's head is the task's branch name, pushed to `origin` rather than a fork, and that `origin` is on a host in `github.hosts`. |
| Task stays **PR Created** after the merge | Check that the recorded PR URL is a `https://github.com/<owner>/<repo>/pull/<n>` link (other hosts aren't checked) and that the PR panel doesn't say sync is paused. If it's paused, mark it merged by hand. |
| Cleanup says *Task branch changed since final review and doesn't match a merged pull request* | The branch has commits after final review that AgentFlow can't match to the merged PR. If the PR is merged on GitHub, wait one poll interval: PR sync looks up the merged head (needs a GitHub PR URL and `gh` logged in). A branch behind the merged head (the PR got a commit outside the worktree, such as a GitHub web edit) is fine. If it still refuses, the branch has commits beyond what was merged. Push them in a new PR, or, if you merged the branch yourself (for example into a local branch), choose **Force remove worktree**: it skips this check, keeps the branch and its commits, and logs a warning with the branch tip. |
| Cleanup says *Could not remove leftover worktree directory* (or *git worktree remove failed*) | Something still has the worktree open in a way cleanup can't end: an elevated (Run as administrator) process working inside it, or an editor or IDE with the folder or a file in it open. Close it and run cleanup again. Processes that merely have their working directory there are killed automatically. When cleanup can tell what still has the folder open (an editor started on it, say), the message ends *(still open in Code.exe (1234): close it and try again)*; the OS error in the message says why the delete failed. |
| Cleanup says *Worktree is on branch … not the task branch* | The worktree was switched or renamed after the task made it (for example to push a `-rc` branch). Cleanup accepts that when the commits it has are the reviewed checkpoint or the merged PR's head (or behind it), so this means it has commits nothing vouches for. Push or merge them first, or remove the worktree yourself; **Force remove** doesn't apply to another branch. |
| Cleanup says *The task branch no longer exists* | The branch was deleted after the merge, leaving the worktree directory behind, so there is nothing to check it against. If you deleted it on purpose, choose **Force remove worktree** to delete the directory. |
| A failed cleanup left a *Not cleaned up* chip on a merged task | The task page shows why in its status message and the activity log (*Worktree cleanup failed while …*). The chip clears once the worktree is gone. |
| Task shows *GitHub PR sync is paused* | You used **Undo** on a PR status. Mark the PR created or merged yourself to turn sync back on for that task. |

Logs: the backend prints every workflow event; per-task events are in the UI and the DB (`workflow_events`).
Prompts, launcher scripts and results live under `runs/` in the task workspace.

---

## 10. Repository layout

```
backend/agentflow/
  main.py            FastAPI app, static UI serving, WebSocket; `agentflow setup` / `doctor` dispatch
  config.py          Pydantic configuration model + loader, Jira startup check
  setup_wizard.py    `agentflow setup` (interactive config.json writer, Jira MCP step)
  doctor.py          `agentflow doctor` (read-only checks, shared with the wizard)
  installers.py      install-with-consent routes for missing tools (winget / brew / Linux)
  api/               tasks, agent (flowctl) endpoints, config/fs, harnesses, jira (search proxy)
  workflow/          engine.py (state machine), default_workflow.py, states.py, models.py (contracts), prompts.py,
                     pr_sync.py (GitHub PR poller)
  harnesses/         base.py adapter contract, claude.py, codex.py, copilot.py, generic_cli.py, registry.py
  services/          git_workspace.py, zellij.py, workspace.py, process.py, events.py, jira_client.py,
                     github.py (read-only gh CLI lookups)
  database/          SQLAlchemy models + SQLite session factory
  cli/flowctl.py     agent-side CLI
frontend/src/        Vue 3 + TypeScript + Pinia + Vue Flow (views/, components/, stores/, api/, workflow/)
scripts/             stub_agent.py, smoke_test.py, dev.ps1 / dev.sh, usage_*.py (usage collectors)
setup.ps1 / setup.sh guided setup (Python + backend, then `agentflow setup`)
start.sh             one-click launcher (runs setup first when Jira isn't configured)
config.example.json
```

Out of scope for the MVP (by design): arbitrary workflow editing, automatic PR creation, general Jira
write-back (comments, custom fields, linked issues — still the harness's MCP's job; the narrow
exceptions are the two status transitions in §1: opt-in To Do → In Progress, and In Code Review once
the PR is created), multi-user/remote execution, full interactive browser terminal. Nodes carry an `executionMode` (`manual | automatic | approval_required`)
so steps can be automated later without changing the engine.
# Local diagnostics

After restarting the backend, AgentFlow writes rotating JSONL diagnostics to
`.agentflow/logs/agentflow.jsonl` beside the selected `config.json` (in this checkout:
`D:\Dev\AgentFlow\.agentflow\logs\agentflow.jsonl`). The active file is limited to
2 MB with four backups. Coding agents can read these local files to investigate failures
before a task exists, worktree branch conflicts, and Zellij session startup failures.
The logs include timestamps, request IDs, branch/path details and exception locations.
They deliberately omit HTTP bodies, query strings, credentials, command output and exception
messages. Repository paths and branch names remain local diagnostic information.

In a paired browser, open `/api/diagnostics` on the AgentFlow server to download the
current log. Workflow agent credentials cannot read this global endpoint. Every HTTP
response has an `X-Request-ID`; internal error messages include the same reference.
For a different-branch worktree conflict, compare `requestedBranch` and `registeredBranch`
in the log and the error message before correcting `workspace.branchPrefix` or choosing
how to preserve the old branch. Recovery never discards a conflicting branch automatically.

## Repository role instructions

Shared role guidance can be versioned in each target repository's `.agentflow-instructions/`
directory and edited from project actions or the task view. Tasks freeze a version at first launch;
updates are explicit and preserve historical briefings. See
[repository agent instructions](docs/custom-agent-instructions.md) for layout, editing, snapshots,
conversation choices, operator APIs and validation.

## License

AgentFlow is © 2026 Rob Rodolfo, released under the [Apache License 2.0](LICENSE). You may use,
modify and redistribute it, including commercially. Redistributions, modified or not, must keep the
credit in [NOTICE](NOTICE) and mark the files they changed. Data copied from other projects is listed
in [THIRD-PARTY-NOTICES.md](THIRD-PARTY-NOTICES.md).
