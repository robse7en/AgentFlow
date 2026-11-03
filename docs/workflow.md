# How the workflow runs

AgentFlow orchestrates existing AI coding CLIs (Claude Code, Codex, GitHub Copilot CLI, or any
generic CLI agent) through a fixed, human-gated software-engineering workflow for one task, either a
Jira ticket or a prompt you write (a *local task*).

The task dashboard groups stages needing attention across concurrent tickets. Settings includes
optional API budgets, per-harness billing classification, Copilot role preferences and metered
launch approvals. Defaults preserve existing routing and leave budgets disabled. See
[cost controls and accounting](security-and-efficiency.md) for limitations and provider usage imports.

New tasks default to direct implementation for cohesive changes. Choose Parallel in the New Task
form for independently owned work. Both routes retain requirements approval, both reviews and
final human approval. In parallel mode, the Architect (in its discovery conversation) splits the approved scope
after requirements approval into focused subtasks for up to three concurrent implementer workers, each with a separate
context, worktree and Zellij tab. All workers share the Implementer configuration. A final
implementer integrates their checkpoints and runs combined tests before review. Select the
Implementer stage to expand worker cards and access their live terminals.
See [worker orchestration](implementer-workers.md) for configuration and recovery.

See [security, verification and usage controls](security-and-efficiency.md) before upgrading
existing tasks. Requirements approvals now freeze the exact documents; independent verification
commands must be configured for ordinary final approval, and their results are the test evidence reviewers are
shown (the implementer's own summary is presented to them as an unverified claim). Usage stays unknown unless collected
from a configured provider-report command or explicitly imported by the human.

After final approval you still push the branch and open the PR yourself. With the GitHub CLI logged in,
AgentFlow then notices the PR and its merge on GitHub and moves the task to **PR Created** and **Merged**
without you pasting URLs. See [GitHub PR sync](configuration.md#github-pr-sync). Optional [Slack DMs](configuration.md#slack) tell you
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
see [slack.md](slack.md)). Even then AgentFlow holds no model credentials: it runs your
Claude Code or Copilot CLI headless, with every tool disabled, on your own login.

## Rules it enforces

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

## Your first task

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
   The Findings tab lists each finding once (a re-raised finding keeps its id), grouped by the iteration that last
   raised it with blocking ones first, and filters by Open / Blocking / All and by reviewer. A reviewer node's findings
   chip opens the tab filtered to that reviewer; both count each finding once, and "blocking" means open and blocking.
   Clicking a finding opens it in a dialog (← / → step through the list) with its earlier rounds and its code the way
   GitHub shows a review comment: the flagged lines (`line` to `endLine`) in the file as the reviewer saw it, read from
   the reviewed commit so it still works after cleanup. Switch to *Diff* for the task's change to that file (or, for a
   closed finding, what changed since the review) or *Now* for the same place in the task's latest commit.
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

## Agent questions

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

## Task workspace layout (`<taskDirectory>/<KEY>/`)

```
task.json
context/additional-context.md
requirements/{requirements,acceptance-criteria,assumptions,implementation-plan,risks}.md, questions.json
runs/{architect,implementer,code-review,security-review}/NN/{prompt.md,config.json,launch.*,result.json}
findings/review-NN.json, security-NN.json, consolidated-NN.json
final/assessment.json, assessment.md
```

Nothing is written into your application repository except the code changes on the task branch.

## Cost estimates

AgentFlow estimates what a stage usually costs from its past runs. An estimate is a guide only:
budgets, launch reserves and approvals never use it.

- **Where:** a stage's Run, Retry and Approve API launch controls show *Typical run …, up to …
  (N past runs)*. The New Task form shows, for the chosen route and agents, one pass through
  every stage, plus what each worker and each review round that sends fixes back adds. How many
  of those there will be isn't known upfront.
- **From what:** the last 20 finished runs (completed or failed, since failed runs are paid for
  too) of the same stage with the same harness and model, across all tasks. Workers share one
  stage. *Typical* is the median and *up to* the 90th percentile. With fewer than 3 such runs,
  the stage says there isn't enough history and is left out of the route totals.
- **Units:** a subscription harness is shown in tokens, since its runs cost nothing extra. Any
  other harness is shown in money, marked when the figures are list-price estimates rather than
  provider-billed amounts.

## Reviewed lessons

Agents can propose a lesson for their role with `flowctl lesson --message "…"`: something durable
about the repository that a later agent in that role should know. Nothing an agent proposes reaches
a briefing on its own:

1. You review the proposal in the task's **Lessons** tab. Edit it, approve it or dismiss it.
2. Approving appends it to that role's `.agentflow-instructions/<role>.md` in the task's original
   repository checkout. It is saved, not committed, and never written to the task worktree.
3. Commit the file with your normal review. Tasks created after that commit get it in their
   instruction snapshot.

Lessons are single lines of at most 300 characters, shown as plain text. See
[Lessons proposed by agents](custom-agent-instructions.md#lessons-proposed-by-agents).

## Repository role instructions

Shared role guidance can be versioned in each target repository's `.agentflow-instructions/`
directory and edited from project actions or the task view. Tasks freeze a version at first launch;
updates are explicit and preserve historical briefings. See
[repository agent instructions](custom-agent-instructions.md) for layout, editing, snapshots,
conversation choices, operator APIs and validation.

## Out of scope

By design, AgentFlow does not do: arbitrary workflow editing, automatic PR creation, general Jira
write-back (comments, custom fields, linked issues — still the harness's MCP's job; the narrow
exceptions are the two status transitions in [Jira integration](configuration.md#agentflows-own-jira-rest-integration-optional): opt-in To Do → In Progress, and In Code Review once
the PR is created), multi-user/remote execution, full interactive browser terminal. Nodes carry an `executionMode` (`manual | automatic | approval_required`)
so steps can be automated later without changing the engine.
