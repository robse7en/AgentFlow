# Changelog

Desktop version history, reconstructed from repository commits on 2026-10-24.
Dates are the version commits' author dates. Entries describe source changes between
version checkpoints; they do not certify that installers were built, distributed, or
tested. The frontend and Python package versions are independent of desktop versions.

Each version has a local annotated `v<version>` Git tag at the source checkpoint
shown below. Tags must be pushed separately to share them with other checkouts.

## 1.2.4 - 2026-11-26

Source: the version commit tagged `v1.2.4` on `main`.

- The Jira search on the New Task form finds sub-tasks. Jira's issue picker leaves them out
  unless asked, so a sub-task could not be found even by its exact key.

## 1.2.3 - 2026-11-18

Source: the version commit tagged `v1.2.3` on `main`.

- A parallel worker can ask for a file outside its ownership with a decision-tree choice that
  carries `grantsPaths`; picking that choice adds the paths to its ownership (refused when they
  overlap another worker). Before, a human's "yes" to a plain question changed nothing, so every
  retry re-checked finished work and was refused again for the same file.
- A worker retried after an ownership refusal is told which files are still outside its ownership
  and how to restore them or ask for them.

## 1.2.2 - 2026-11-18

Source: the version commit tagged `v1.2.2` on `main`.

- On Windows, cleaning up a task worktree that holds a `CONOUT$` file (see 1.2.1) no longer fails the
  first time. Git deregistered the worktree, then could not delete a file with that reserved device
  name ("Directory not empty"), so the cleanup showed as failed and only a second attempt removed the
  leftover folder. Cleanup, task deletion and discarding a worker's worktree now finish the delete
  themselves when git cannot. Only the desktop app is fixed; the Python backend is not.

## 1.2.1 - 2026-11-17

Source: the version commit tagged `v1.2.1` on `main`.

- On Windows, an untracked `CONOUT$` file in a task worktree no longer counts as a change to it or goes
  into a checkpoint commit. A Claude Code agent launched there can leave that 9-byte file behind (a program
  opening the console by name through a POSIX layer creates a real file), and it made every Architect plan
  fail with "Planning changed the task worktree; inspect it before retrying". Only the desktop app is fixed;
  the Python backend still has the old check.

## 1.2.0 - 2026-11-14

Source: the version commit tagged `v1.2.0` on `main`.

- **Agent activity map.** Each running agent now shows what it is doing, live, from its own transcript:
  - The task page is a canvas with an activity column beside it. The Details tabs moved to a slide-in
    drawer, and the canvas follows the theme (a light canvas in the light theme).
  - The panel has a "Now" headline (the current call and its clock), vitals (context fill, tokens, spend,
    steps) and a hull-and-docks map: the model in the middle, and docks for You, AgentFlow, Skills,
    Agents, Files, Shell, Git and Network. A call travels out to its dock and back, and a subagent
    appears as a pod on a tether. Waiting for you is amber, a failed call red.
  - A Map | Tape switch (remembered per viewer) shows the same data as a tape of lanes over time, with
    a "Run so far" strip.
  - Each agent card on the canvas shows its current call and live cost. Batons travel along the edges
    when work is handed on, each Implementer worker shows a mini strip, and a chip counts workers
    running side by side.
  - Claude Code, GitHub Copilot and OpenAI Codex are all read. Claude's subagent transcripts are
    followed and tied to the call that started them, and so are Copilot's subagents and Claude's
    background agents. A Codex script is shown as one call named from its first tool.
  - Live tokens and context fill come from each harness's transcript. Cost is shown only when the model's
    price is known; Copilot has none, because it bills premium requests.
  - Labels carry names only, never content: a file's last two path parts, a web host, a command or
    subcommand name, a tool's name. Prompts show as "Prompt". Nothing of a command line, prompt or file
    is logged or kept.
  - Updates go to the web page over the existing websocket (at most one every 250 ms per run, only when
    something changed), and `GET /api/tasks/{key}/activity` returns the whole picture. The task
    snapshot is unchanged, and the Python backend is untouched: it shows no activity.
- Codex support was built from Codex CLI 0.167.2 rollouts; earlier formats (`function_call` / `shell`)
  are not read. Live Copilot tokens and context need Copilot CLI 1.0.105 or later; older versions still
  show calls.
- Known limits: PowerShell commands such as `Get-Content` show as Shell, not Files. Two Codex roles
  sharing one working folder get no activity, because the session cannot be told apart.
- Not checked before release: Copilot's activity was verified against real Copilot transcripts (subagents,
  failed commands, permissions, usage) and in tests, but not on a fresh Copilot run just before this
  version, because GitHub rate-limited the Copilot CLI's login that day. Neither Copilot nor Codex was run
  end to end through an AgentFlow-launched task; Codex was run once with `codex exec` and read by the
  same parser. If a Copilot run shows wrong calls, report it with the transcript's shape and the parser gets
  a new fixture.

## 1.1.14 - 2026-11-06

Source: the version commit tagged `v1.1.14` on `main`.

- An agent interrupted by a restart only offers Resume when Resume can work: its session is
  still recorded running, or it saved a finished result to disk. When the agent is gone, the
  task header's button reads "Retry agent" and starts a new run instead of only opening the node.

## 1.1.13 - 2026-11-06

Source: the version commit tagged `v1.1.13` on `main`.

- Updates come from this repository's GitHub releases. `build-desktop-installer.ps1 -GitHubRepo
  owner/name` points a build at the latest release's `latest.json`; `-UpdateUrl` remains for a
  self-hosted server. Installers 1.1.6 to 1.1.12 have no updater and must install 1.1.13 once.
  Copies that update from the LAN server move to GitHub on this release. See
  [Over-the-air updates](app/src-tauri/README.md#over-the-air-updates).

## 1.1.12 - 2026-11-04

Source: the version commit tagged `v1.1.12` on `main`.

- Cost estimates from past runs: a stage's Run control shows its typical and high cost (the median
  and 90th percentile of its last 20 runs with the same harness and model). The New Task form shows
  what the chosen route would cost for one pass, plus what each worker and each fix round adds.
  Subscription harnesses are shown in tokens. Display only: budgets and launch reserves are
  unchanged. See [Cost estimates](docs/workflow.md#cost-estimates).
- Reviewed lessons: agents propose lessons for their role with `flowctl lesson`, and every briefing
  says how. The task's Lessons tab lets you edit, approve or dismiss each one. Approving appends it
  to the role's `.agentflow-instructions` file in the task's original checkout, uncommitted. No
  agent sees it until you commit it. See [Reviewed lessons](docs/workflow.md#reviewed-lessons).
- Findings tab: a compact list showing each finding once, blocking first, with Open / Blocking / All
  and reviewer filters. A finding opens in a dialog with its code, the implementer's answer, the
  reviewer's evidence and its earlier rounds, and the arrow keys step through the list. A reviewer
  node's findings chip opens the tab filtered to that reviewer. The chip now counts each finding once
  and counts it as blocking only while it is open, where it used to count every round's copy.

## 1.1.11 - 2026-11-03

Source: the version commit tagged `v1.1.11` on `main`.

- Opt-in automatic retries: with `workflow.maxAutomaticRetries` (0–3, default 0), a stage whose
  harness exits without reporting completion relaunches by itself after a short pause. It gets a
  fresh conversation and the retry briefing, and runs the same capacity, budget and
  metered-approval checks as a human's Run. It never covers `flowctl fail`, launch failures,
  interruptions or tasks that aren't Active. See [Automatic retries](docs/approvals-and-recovery.md#automatic-retries).
- A running task's icon in the task list has a loading spinner around it: a faint full ring with an
  arc that spins as it grows and shrinks, so running rows stand out. It holds still when the system
  asks for reduced motion.

## 1.1.10 - 2026-11-02

Source: the version commit tagged `v1.1.10` on `main`.

- The next pull-request step is the task header's main button: Push & open PR (into the task's base
  branch, highlighted when it isn't main or master), then Mark PR created, then a link to the PR and
  Mark merged. The Assessment tab no longer says AgentFlow doesn't open PRs, and the task list's row
  menu can push and open the PR too.
- When a task becomes PR Created, by hand or through GitHub PR sync, AgentFlow moves its Jira ticket
  to "In Code Review", from any status. Set `jira.prCreatedTransitionStatus` to your review status,
  or `null` to turn it off; `jira.prCreatedTransitionFromStatuses` narrows which statuses it moves
  from. A Jira problem never blocks or undoes the PR: it is logged on the task.

## 1.1.9 - 2026-11-02

Source: the version commit tagged `v1.1.9` on `main`.

- The task list shows which tasks are actually running. Each row starts with a status icon and
  shows the workflow as a nine-stage track instead of a percentage bar. Only a task whose agent is
  working is blue and animated; a task waiting on you reads amber, a failed one red, and finished
  tracks are muted so the running ones stand out.

## 1.1.8 - 2026-11-02

Source: the version commit tagged `v1.1.8` on `main`.

- A retried stage's briefing now says what the failed attempts did: how the latest ended, its last
  heartbeat, the human's answers to its questions and, for implementers and workers, the files
  committed and left uncommitted since the first failed attempt started. The new conversation
  continues from that work instead of starting over. See [Retry briefings](docs/approvals-and-recovery.md#retry-briefings).

## 1.1.7 - 2026-11-01

Source: the version commit tagged `v1.1.7` on `main`.

- Show the code a review finding points at, the way GitHub shows a review comment: the flagged
  lines in the file as the reviewer saw it, with the finding underneath. Switch to *Diff* for
  the task's change to that file (for a closed finding, what changed since the review) or *Now*
  for the same place in the task's latest commit. Code is read from the reviewed commit, so it
  still shows after the worktree is cleaned up. Resolved findings keep it folded until opened.
- Reviewers can flag a range: findings take an optional `endLine` (`flowctl finding
  --end-line`), and the review prompt asks for it.

## 1.1.6 - 2026-10-25

Source: `689773e` (`v1.1.6`) on `main`.

- Let agents ask decision-tree questions with `flowctl question --file`: the agent's pick,
  consequence notes, a risk level, a context line, an agent's note with its source per
  choice, and one follow-up question per branch. `--message`/`--choice` still asks a flat
  question.
- Answer by picking a path, sent to the agent as one line (`2: …`, `1.1: A → B` or
  `other: …`), from the task page, Slack (the notice switches to the follow-up's buttons in
  place) or a new Decisions inbox listing open questions across tasks.

## 1.1.5 - 2026-10-25

Source: `0907170` (`v1.1.5`) on `main`.

- Plan Parallel tasks with the Architect, resuming its discovery conversation, instead
  of a fresh Orchestrator. A configured `agents.orchestrator`, or an Orchestrator
  override on a task or project, keeps the separate planner; stored tasks are unchanged.
- Stop the example config, setup wizard and desktop onboarding writing
  `agents.orchestrator`.
- Include everything in 1.1.4, merged into `main` in `2546d4d`.

## 1.1.4 - 2026-10-24

Source: `75a9586` (`v1.1.4`). Recorded on `feature/custom-agent-instructions`, which
was merged into `main` in `2546d4d` for 1.1.5.

- Add repository-based shared agent instructions, effective briefing previews, and
  immutable task snapshots, with explicit updates for future runs.
- Include interrupted snapshot recovery and accurate uncommitted-file status.

## 1.1.3 - 2026-10-23

Source: `eb4d3fc` (`v1.1.3`).

- Fix worktree cleanup failures and log cleanup errors.
- Accept hidden Zellij client windows narrower than the configured headless width.

## 1.1.2 - 2026-10-23

Source: `f2a3aa0` (`v1.1.2`).

- Open pull requests against the task's base branch and flag pull requests targeting
  another branch.

## 1.1.1 - 2026-10-23

Source: `2e5bc6f` (`v1.1.1`).

- Keep dashboard headers and filters fixed while the task list scrolls.

## 1.1.0 - 2026-10-22

Source: `222e639` (`v1.1.0`).

- Group tasks into projects in both the Python backend and Rust core.
- Add a Projects page and a project filter on the task dashboard.
- Use "working directory" consistently in server messages.

## 1.0.1 - 2026-10-22

Source: `7af36c2` (`v1.0.1`).

- Introduce the A-shaped logo and use it for the desktop app icon.

## 1.0.0 - 2026-10-21

Source: `461ed86` (`v1.0.0`).

- Redesign the frontend with a sidebar shell, theme tokens, task rows, curved
  workflow nodes, dashboard statistics, task headers, cards, and a dark canvas.
- Keep row menus above subsequent rows and improve status text contrast to AA.

## 0.1.13 - 2026-10-21

Source: `c8c69d1` (`v0.1.13`).

- Display observational agent heartbeats on workflow nodes and worker cards.

## 0.1.12 - 2026-10-21

Source: `b884790` (`v0.1.12`).

- Prepare new worktrees using per-repository setup configuration.
- Record Orca-inspired improvement proposals checked against the Rust core.

## 0.1.11 - 2026-10-21

Source: `69c576c` (`v0.1.11`).

- Include rollback cost and error detection in the final risk assessment.
- Refresh the configuration test baseline to include `zellij.useInDesktop`.

## 0.1.10 - 2026-10-21

Source: `204fe62` (`v0.1.10`).

- Branch new tasks from the remote base when the local base branch is behind it.

## 0.1.9 - 2026-10-21

Source: `5a081f5` (`v0.1.9`).

- Open the workflow canvas zoomed in on the node needing attention.

## 0.1.8 - 2026-10-20

Source: `7713645` (`v0.1.8`).

- Add harness logos, compact chips, and time/cost footers to workflow nodes.

## 0.1.7 - 2026-10-14

Source: `11fcab6` (`v0.1.7`).

- Run task creation, worktree cleanup, and deletion in the background.
- Close leftover task Zellij sessions during cleanup and deletion.
- Apply Rust formatting and fix Clippy warnings.

## 0.1.6 - 2026-10-14

Source: `875436d` (`v0.1.6`).

- Find Jira tickets in search even when they have never been opened in Jira.

## 0.1.5 - 2026-10-12

Source: `7c5ba2f` (`v0.1.5`).

- Allow desktop agents to run in Zellij.
- Fix `flowctl` shims on installed desktops.

## 0.1.4 - 2026-10-08

Source: `b60a006` (`v0.1.4`).

- Offer forced worktree cleanup for branches merged outside a pull request.

## 0.1.3 - 2026-10-08

Source: `3430a24` (`v0.1.3`).

- Add signed over-the-air desktop updates through a configurable LAN update server.

## 0.1.2 - 2026-10-08

Source: `d15dddf` (`v0.1.2`).

- Make Jira optional and support local prompt tasks.
- Defer coding CLI connection until an agent's first run, and allow setup to skip
  Jira and CLI configuration.
- Confirm before closing the desktop with work in progress.
- Add Change instance to desktop Settings and keep new setup independent of the
  existing instance selection.
- Fix setup authorization failures caused by a null origin.
- Show Connecting during initial pairing and report the built-in terminal as available.
- Fix a Copilot implementer refusing to read its briefing.

## 0.1.1 - 2026-10-05

Source: `bf101a6` (`v0.1.1`).

- Add a Windows desktop installer and guided instance setup.

## 0.1.0 - 2026-10-05

Source: `abf1359` (`v0.1.0`). Initial desktop source baseline at the Rust port merge;
no separate release bump or distribution record was found for this version.

- Introduce the Rust desktop core, CLI callbacks, task lifecycle, configuration,
  SQLite persistence, authentication, WebSocket API, and harness integrations.
- Add embedded terminals, Git worktree handling, workflow and integration support,
  and Windows desktop rebuild scripts.

## Maintaining this record

For each future desktop release:

1. Update the desktop version and any corresponding workspace package versions.
2. Add a dated entry here describing the changes since the previous version.
3. Commit the version and changelog together on the intended release branch.
4. Create an annotated tag on that commit:
   `git tag -a vX.Y.Z -m "AgentFlow X.Y.Z"`.
5. Use the same release notes when building the installer with `-ReleaseNotes`.
6. When publishing is authorized, push the release commit and its specific tag:
   `git push origin vX.Y.Z`. Never move an existing published release tag.

The historical tags above point to the original version commits, so their source
trees predate this reconstructed changelog.
