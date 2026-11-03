# Configuration

## `config.json`

When running from this source checkout, AgentFlow uses (and creates on first launch) the `config.json` in
the AgentFlow folder. An existing `~/.agentflow/config.json` from an older install is still used while the
AgentFlow folder has none; installed copies without a source checkout always use it. A complete
example is in [`config.example.json`](../config.example.json). Configuration is validated with Pydantic;
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
writes the new one, offers to add the Jira MCP to each harness (see [MCP prerequisites](installation.md#mcp-prerequisites-important)), and then runs
`agentflow doctor`. Before the doctor run it also offers to trust the AgentFlow folder in Claude Code
(`hasTrustDialogAccepted` in `~/.claude.json`), Codex (`[projects.'<folder>']` in `config.toml`) and Copilot
(`trustedFolders` in `config.json`), following `CODEX_HOME`/`COPILOT_HOME` in a harness's `env`. It only
touches CLIs that are already set up, and leaves existing entries (even untrusted ones) as they are. When a tool is missing it offers to install it (see [Installing AgentFlow](installation.md#guided-recommended)). It also points each Claude Code,
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
                 "email": "you@yourcompany.com", "apiToken": "..." },  // email: Jira Cloud only, see the Jira section below
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

## Harnesses

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

## Precedence

```
built-in defaults → config.json → task-level overrides (New Task form) → node-level override (Configure)
```

Every node run records the exact harness/model/reasoning it used (`runs/<role>/<nn>/config.json` and the DB).

## AgentFlow's own Jira REST integration (optional)

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

## GitHub PR sync

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

## Slack

AgentFlow can DM you in Slack when a task needs you. From Slack you can answer agent questions and
check tasks (`list`, `status`, `digest`). If you allow them, you can also create or start tasks, and
ask about a task in plain words (`slack.ask`).
It is off by default. See [slack.md](slack.md) for setup and every setting.

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

## Cleaning up worktrees

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

## Zellij

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
