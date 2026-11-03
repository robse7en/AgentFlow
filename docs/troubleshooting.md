# Troubleshooting

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
| Architect cannot read the ticket | The Architect's harness has no Jira MCP, or isn't signed in to it (`agentflow doctor` shows which). For Jira Cloud, `agentflow setup` can add it; otherwise configure it in that CLI (linked issues/custom fields always need it; see [Jira integration](configuration.md#agentflows-own-jira-rest-integration-optional) for what AgentFlow's own Jira integration covers instead). |
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

## Local diagnostics

After restarting the backend, AgentFlow writes rotating JSONL diagnostics to
`.agentflow/logs/agentflow.jsonl` beside the selected `config.json`. The active file is limited to
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
