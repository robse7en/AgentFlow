# Orchestrated implementation

[Agent heartbeats](heartbeats.md) and a first version of repeatable worktree setup are
implemented; inline diff feedback and the rest of the worker health proposals are not built yet.

Tasks created with the Parallel route run Architect discovery → human requirements approval → subtask planning
→ implementer workers → integration implementer → the existing reviews and final human gate.
Tasks already stored without an Orchestrator node retain their original workflow.
New tasks default to Direct; select Parallel in the task form or set `workflow.implementationMode`.
See [security and efficiency controls](security-and-efficiency.md) for requirements snapshots,
verification commands, global capacity and budgets shared by both routes.

## Roles and context

The planner reads the approved scope and proposes a structured plan automatically.
It does not write source code. On new tasks the planner is the Architect: the
"Architect Planning" stage resumes the conversation Architect discovery ran in, so the
plan is made with what discovery learned about the repository instead of a fresh agent
re-exploring it. A harness without resume, or a resume that crashed, gets a fresh
conversation with the same self-contained briefing. Each subtask has a focused brief, acceptance criteria,
explicit owned files/directories and dependencies. Plans reject cycles, unknown dependencies,
overlapping ownership and unsafe paths. Shared wiring belongs to one owner or to integration.
This validates the shape of the plan; whether it faithfully implements the approved scope
still requires the existing reviews and final verification.

All workers inherit the parent Implementer's harness/model/reasoning configuration. Each gets
a fresh conversation, its own GUID, branch, worktree and Zellij tab. At most three workers
run per ticket; workers waiting for answers or recovery occupy slots. Retries use a fresh
conversation in the same worktree, preserving local work, and their briefing says what the
unfinished attempts did (see [Retry briefings](../README.md#retry-briefings)). No worker-specific model overrides
are allowed. Focused briefs include dependency summaries, not other workers' conversations.

A separate Orchestrator in its own fresh conversation is still available. Configure
`agents.orchestrator` (same shape as `agents.architect`), or give the task an Orchestrator
override in the New Task form or its project's defaults. Either one makes new Parallel tasks use
an "Orchestrator" stage instead of Architect Planning; omitting it or setting it to `null`
leaves planning to the Architect. The choice is made when the task is created: stored tasks keep
the planner they were created with. Worker configuration comes from the parent Implementer,
including its task/node overrides.

## Scheduling and integration

AgentFlow owns scheduling and durable lifecycle tracking. A worker starts after its dependencies
complete. Its worktree branches from the original local checkpoint and merges completed dependency
checkpoints before launch. These operations do not fetch from a remote. Workers must limit their
changes to their owned paths; AgentFlow verifies the committed diff before accepting completion.
Worktrees are workflow isolation, not an OS sandbox against unrestricted same-user agents.

Accepted worker completion closes only that worker's tracked terminal before releasing its slot.
The final integration implementer receives all checkpoint SHAs, summaries, acceptance criteria and
known issues. It merges the checkpoints, resolves conflicts and runs combined tests in the ticket's
main worktree. AgentFlow verifies that every accepted worker checkpoint is an ancestor of HEAD
before creating the review checkpoint. Reviews and their fix loop retain their existing behavior;
review fixes are handled by the integration implementer.

## UI and recovery

Agent nodes and worker cards show observational [agent heartbeats](heartbeats.md), including
overdue-signal warnings. These never release worker slots or trigger automatic recovery.

Select the Implementer stage to expand individual worker cards. Each card shows the assigned
brief, owned paths, dependencies, acceptance criteria, worktree, state and checkpoint. Active
workers offer **Show Zellij terminal**, using the existing read-only embedded terminal. All tabs
belong to the ticket's Zellij session; the task's attach command also provides native access.
Completed workers retain their result and checkpoint, but their live terminals are closed.

Failed workers can be retried individually. After backend restart, active workers require explicit
resume or stop-and-retry; eligible pending work is made available for an explicit start. Pending
dependency work cannot bypass its prerequisites or the concurrency limit. Cancellation stops all
tracked task agent tabs and preserves worker branches, worktrees and artifacts.

Worker worktrees are intentionally retained. The existing final cleanup action removes only the
ticket's primary worktree; worker worktrees are not silently deleted. A later cleanup action should
verify each retained branch/checkpoint and check for local files before offering removal.

## Validation boundary

Verification for this change: 137 backend tests passed, including actual local Git worktree
creation and dependency/integration merges. `npm run build` passed TypeScript checking and
production bundling. `node scripts/verify-worker-ui.mjs` passed server-rendered worker-card
checks for terminal/retry/recovery controls and escaped text.

Backend tests cover plan validation, scheduling, callback identity, terminal targeting, recovery,
ownership and integration gates. Local Git tests exercise worktree creation and checkpoint merges.
Mocked harness launches do not prove end-to-end operation with an authenticated coding CLI or a
real Zellij client; those require a local acceptance run with the chosen harness.
