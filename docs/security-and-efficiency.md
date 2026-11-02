# Security and execution controls

## Routes and quality

New tasks default to `workflow.implementationMode: "direct"`. The New Task form can override
this with `parallel`; existing tasks keep their graph. Direct executes discovery, implementation,
two independent reviews and final verification. Parallel adds planning, isolated workers and
integration. Both require human requirements and final approval. Model capability and reasoning
defaults are preserved; measure accepted-task cost and rework before lowering them.

Requirements, acceptance criteria and implementation plan must be nonempty. Approval checks
the exact document digest displayed by the browser, stores the document snapshot in SQLite,
and binds later runs to it. Changes block launches and final approval, even with a risk override.
Restore the saved snapshot (visible in Requirements) to continue, or create a new task for changed
scope. The snapshot is an application-level integrity control, not protection from a process
that can directly modify the backend database.

Older tasks have no snapshot: stop active agents, open Requirements, review the documents and
approve their snapshot. This does not restart completed stages. Old running launchers need
Stop & retry because the credential lookup protocol changed. Restart the backend after upgrading.

## Independent verification

Configure `workflow.verificationCommands` as a list of argv arrays, for example:

```json
"verificationCommands": [["python", "-m", "pytest", "tests", "-q"]]
```

Use commands appropriate to the selected repository and installed tools, preferably absolute
executable paths. No shell is used: pipes, shell builtins and `&&` are not interpreted. Commands
run in the configured project directory of the task worktree after the integrated checkpoint.
Each command has a five-minute timeout and at most 16 KB of retained combined output; for longer output the
start and the end are kept, since test runners print their summary last. Cancel task
also cancels an active verifier without waiting for that timeout. Evidence
contains command, exit code and checkpoint SHA. Changed tracked/untracked worktree content or
HEAD invalidates completion; ignored test outputs are permitted by normal Git status semantics.
The app does not provision dependencies or infer verification commands from untrusted ticket text.

Reviewers of the checkpoint are shown this evidence (each command, its exit code and the tail of its output) ahead
of the implementer's account, which is labelled unverified. With no commands configured they are told that every
test result is the implementer's claim. When a command fails, reviewers are told to check whether it also fails
on the base branch before blocking on it; AgentFlow does not run that comparison itself, so pick commands that pass
on the base branch.

Missing commands, failed commands and missing evidence block ordinary final approval. Explicit
human risk override remains available and preserves the actual failed/unverified evidence.
Verification runs with the task lock held but outside any open database transaction, so a long
test run on one task does not stall other tasks' agent signals or UI actions.

## Worktree drift after review

Final approval and review-loop override re-check the worktree against the reviewed checkpoint.
Uncommitted changes and new commits since that checkpoint are reported as blockers alongside the
evidence blockers above. Uncommitted-only drift can be discarded from the UI (`git reset --hard`
to the reviewed SHA; refused when untracked files or new commits are present). Any drift can
instead be overridden with a reason, which records the drift details and the reviewed SHA in the
event history. A stale checkpoint reference — the browser holding an older SHA than the task —
is rejected outright and cannot be overridden.
Agent-reported test counts remain separately checked. The task's Execution cost and verification
panel displays independent evidence. Test commands execute repository code with the backend's
OS permissions; use an isolated account or environment for untrusted repositories.

## Usage and monitoring

`maxConcurrentAgents` (default 4) bounds active/interrupted runs globally in this backend process;
a paused node stays Ready and explains why, and starts once a slot is free. Waiting/interrupted runs
occupy capacity. Run only one backend writer per DB.

An automatic retry (`workflow.maxAutomaticRetries`, off by default; see the README's
[Automatic retries](../README.md#automatic-retries)) is a new run and is charged like one. It goes
through the same capacity, budget and metered-approval checks as a human's Run. It never carries an
approval, so a role that needs metered approval is never retried automatically. A blocked retry
leaves the stage Failed with the reason, instead of Ready.

Cost controls are opt-in under Settings → API spend controls. Existing configurations retain
their role assignments and unlimited launches until limits or approval policies are enabled.
Configure a default ticket USD budget, an optional ticket-specific override, and a shared daily
USD budget. Daily totals use UTC completion dates (start date for unfinished runs). Active runs
from previous days still reserve capacity. Each non-subscription launch reserves the configured
amount under the same global launch lock, so concurrent tickets cannot take the same remaining
reservation. Warnings default to 80%. Missing completed-run costs pause further metered launches
when a budget is enabled. A paused stage remains Ready and explains the next action; use Run after
correcting costs or changing limits. Subscription launches are exempt from API budgets.

Cost estimates (README, [Cost estimates](../README.md#cost-estimates)) are display only. They show
the median and 90th-percentile cost of a stage's last runs on the Run control and in the New Task
form. They never feed `launchReserveUsd`, a budget check or an approval: the fixed reserve still
applies to every metered launch.

These are launch controls, **not hard provider spend caps**: an already-running agent may exceed
its reservation. Budgets count only provider-billed costs of metered runs; list-price estimates
never count. Missing usage/model/cost stays unknown, never zero.
Classify each harness as metered, subscription/unlimited, or unknown. The billing classification
is saved with each new run; later settings changes do not relabel history. API totals exclude
subscription costs and distinguish token-report coverage from API-cost coverage. Historical
unclassified runs remain unknown until a human corrects them with a provider report.

Cost is shown for every run, whatever its billing mode or whether limits are enabled, separately
from API spend. Each report's `costKind` says what it is: `billed` is an amount the provider
metered, and `estimated` prices the tokens at public list rates. The two are never summed.
The bundled collectors report:

- **Claude Code** — if it runs through the Copilot proxy (`copilot-api`), the proxy's ledger
  (`~/.local/share/copilot-api/copilot-api.sqlite`, or `$COPILOT_API_HOME`) gives the AI credits
  GitHub billed for the session, background calls included: `billed`. Otherwise the transcript is
  priced per request, including long-context tiers and one-hour cache writes: `estimated`. The
  agent's `flowctl complete` runs mid-turn, so while the transcript's last message is still a tool
  call the collector prints the usage so far and exits 75. AgentFlow records that (marked
  `"partial": true`) and replaces it once the turn has ended, from the same background retry Copilot uses.
- **Copilot CLI** — the AI credits (`totalNanoAiu`, 1 credit = US$0.01) the CLI records in usage
  checkpoints and at shutdown: `billed`. Cost only: these records carry no token counts, so Copilot
  runs show no tokens. Copilot 1.0.89 writes a checkpoint about two seconds after each turn ends,
  which is after the agent's own `flowctl complete`; until then the collector exits 75 ("not written
  yet") and AgentFlow collects again in the background for about four minutes. Earlier releases
  checkpoint only after an autopilot task, so there the cost arrives at shutdown: before closing an
  idle Copilot tab (next run of the role, cancel, cleanup, delete), AgentFlow types `/exit` and waits
  up to 10s for it. Releases before 1.0.49 record no credits, so their cost stays unknown. Orchestrated
  workers are closed during their own `flowctl complete`, before they can write it.
- **Codex CLI** — tokens priced at list rates: `estimated` (a ChatGPT login meters quota, not dollars).

List prices come from the public models.dev catalog: the copy the Copilot proxy caches, else
`~/.cache/agentflow/models-dev-api.json`, refreshed weekly. A model missing from the catalog
leaves that session's cost unknown. A resumed session whose baseline has a different `costKind`
than the new report leaves that run's cost unknown.

For harnesses with provider telemetry, configure `harnesses.<name>.usageCommand` to an explicitly
trusted local argv command. `config.example.json` points Claude Code, Codex and Copilot at the bundled
collectors portably (`["python", "scripts/usage_<type>.py"]`): a leading `python` runs the backend's
own interpreter, and a relative argument naming a file in the AgentFlow install resolves to it.
`agentflow setup` writes the absolute form (`["<backend venv python>", "<install root>/scripts/usage_<type>.py"]`)
when it is unset, and `agentflow doctor` warns about a harness without one. AgentFlow runs it on completion/failure/exit with `AGENTFLOW_RUN` and
`AGENTFLOW_SESSION_ID`, a 15-second timeout and 16 KB output limit. It must return JSON:

```json
{"inputTokens":1000,"outputTokens":200,"cachedInputTokens":300,
 "model":"actual-provider-model","source":"provider run report","costUsd":0.02}
```

A cost-only report omits all three token fields and must include `costUsd`. Exit code 75 means the
usage is not all written yet; AgentFlow retries instead of recording the run as unknown. A report
printed with exit 75 is the usage so far: it is recorded as partial, replaced by the next complete
report, and used as-is for a resumed session's baseline (that process is gone).

By default, counts must be **cumulative for that AgentFlow run**, not the whole resumed conversation;
cachedInputTokens is a subset of inputTokens. Repeated reports replace that run's usage, avoiding
double counting. Omit costUsd if unavailable, and set `"costKind":"estimated"` when it is a
list-price estimate (omitted means billed). Model catalogs are not treated as proof of the actual model. If the CLI has no telemetry, leave usageCommand null.
Bundled collectors declare `"scope":"session"`. AgentFlow records an immutable baseline before
resuming the session and subtracts it on each collection. Fresh sessions start with a zero baseline;
failed/missing resumed baselines leave usage unknown. Baselines live in the run's
`usage-baseline.json`, not beside provider transcripts. Codex collection requires an exact captured
session ID and never chooses the latest session sharing a directory. Worker collectors run in the
worker's own directory. Old transcript cursor files are no longer used or modified.

A paired human can import this JSON in Execution cost and verification, or PUT it to
`/api/tasks/<key>/runs/<run-id>/usage`, optionally supplying `billingMode`. Imports must describe
one run, not an entire session. Agent credentials
cannot use this endpoint. Reports retain provenance; agent completion JSON is not trusted as
provider usage. Collector failures leave usage unknown and record an event.

Settings can prefer Copilot for selected roles. The configured Copilot model is used only when
the harness is available and classified as subscription. Explicit task/stage settings take
precedence. Metered/unclassified fallback from a preferred Copilot role always needs human
approval; a separate policy can require approval for every metered launch. Approval includes a
reason and the exact displayed harness/model, is logged, applies to one launch, and never bypasses
budgets, requirements approval, review checks, or the global concurrency limit. Workers retain
the shared Implementer configuration. Availability detection does not prove model access.

The dashboard prioritizes actionable questions, approvals, failed/interrupted agents and ready
stages, including parallel workers. Search/filter tasks and follow a stage link directly to its
controls. Shared daily API spend refreshes every 30 seconds; launch checks always read current
database state. Implementation prompts reference approved documents instead of embedding their
full contents a second time; fix rounds begin with relevant findings and acceptance criteria.
Compare spend per completed ticket, including retries and verification, before claiming savings.

New runs use fresh processes with the requested settings and scoped credentials. Exact conversation
IDs are used for resume when recorded. Otherwise the agent starts fresh with requirements and a
bounded task handoff; it never selects a conversation via `--last` or `--continue`. Follow-up
review prompts supply the prior reviewed SHA and findings, ask for focused delta review, and
require expansion for security boundaries, shared interfaces or scope changes. Final acceptance
still checks the full change.

## Process trust and credentials

The CLI reads only `AGENTFLOW_CREDENTIAL_FILE` for its run; it no longer searches other run files.
The backend and generated launchers allowlist inherited environment variables. Explicit harness
`env` entries are the opt-in route for required credentials. Automatic Claude trust is disabled
unless the selected repository root exactly matches a configured `trustedRepositories` path.
Settings displays this policy. Trusting a repository includes its generated worktrees and allows
the adapter to update the user's Claude trust configuration.

These controls reduce accidental credential crossover. Worktrees and same-user file permissions
are **not OS isolation**: an unrestricted same-account agent can still read or modify other task
files, invoke tools outside its role, or read the backend DB. For adversarial agents/repositories,
run agents under separate OS identities/containers with only assigned credentials and artifacts
mounted, read-only reviewer source access and restricted networking. AgentFlow does not provision
that OS sandbox in this change. Retain the browser/backend terminal separation and human gates.

## Jira downloads

Jira requires HTTPS. Initial attachment URLs must match the configured Jira origin before
credentials are attached. Redirect destinations must match that origin or an exact configured
`attachmentHosts` host on HTTPS/443; cross-origin redirects receive no Jira credentials. The
default media host is `api.media.atlassian.com`. Other CDN hosts need explicit configuration.
Redirect count, attachment count and streamed byte limits are enforced. IDs must be decimal,
filenames are sanitized, resolved paths must remain inside the destination, and existing files
are never overwritten. Partial oversized downloads are removed. Download errors do not include
signed URLs or authentication material.
