# Approvals, retries and recovery

How AgentFlow keeps the human gates in your hands, and what happens when an agent fails or the
backend restarts.

## Local approval protection and recovery

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

## Retry briefings

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

## Automatic retries

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
