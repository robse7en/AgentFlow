"""Focused briefings for planning, workers and integration."""

import json

from .prompts import PromptContext, _flowctl_block


def _architect_planning(ctx: PromptContext) -> str:
    return f"""# Subtask planning for {ctx.jira_key}
The human approved the requirements. As the Architect you now also plan the implementation:
split the approved scope into subtasks for parallel implementer workers. If you ran discovery
for this ticket in this conversation, reuse what you learned about the repository at
{ctx.worktree_root} instead of exploring it again. The approved documents in
{ctx.task_dir / 'requirements'} override anything you wrote or said earlier: read them first.
Do not modify source code or launch processes for other agents.
"""


def orchestrator(ctx: PromptContext) -> str:
    intro = _architect_planning(ctx) if ctx.role == "architect" else f"""# Orchestrator for {ctx.jira_key}
Read the approved documents in {ctx.task_dir / 'requirements'} and inspect the repository
at {ctx.worktree_root}. Do not modify source code or launch processes for other agents.
"""
    return f"""{intro}Split only the approved scope into at most 30 bounded subtasks. AgentFlow schedules up to
three implementers concurrently. Every worker uses the same implementer configuration,
a fresh conversation and a separate worktree. Integration and combined tests are delegated
to a final implementer, not to you.

Each brief must be self-contained: include the relevant requirements, shared interfaces,
repository conventions, test commands and any constraints. Do not copy conversation history.
Use explicit relative files/directories for ownership (no globs, root or overlapping owners).
Put shared-file changes in one subtask or leave them explicitly for the integration implementer.
Dependencies refer to other subtask keys and must be acyclic. Workers see committed dependencies.
Do not add scope or weaken acceptance criteria. Ask the human if approved scope is ambiguous.

Submit this exact JSON shape (no status field):
{{"summary":"Plan rationale and integration instructions", "subtasks":[
 {{"key":"api", "title":"Implement API", "brief":"Focused instructions",
   "ownedPaths":["backend/api"], "acceptanceCriteria":["Required behavior and tests"],
   "dependencies":[]}}
]}}
{_flowctl_block(ctx)}
"""


def worker(ctx: PromptContext, item, dependency_results: list[dict]) -> str:
    return f"""# Implementer worker: {item.plan_key}
Ticket: {ctx.jira_key}
Work exclusively in {item.worktree_path}, branch {item.branch_name}.
Your base checkpoint, including completed dependencies, is {item.base_sha}.
You are one of several implementers. Never revert others' work or edit another worktree.
Follow repository instructions. Do not read other agents' conversation logs or full task
history. Ask via flowctl if your focused brief lacks a required decision.

## Assigned task
{item.brief}

## Owned files/directories
{json.dumps(item.owned_paths)}
Only modify these paths. If changes elsewhere are required, report them as known issues
for integration or ask the human. Do not modify dependency code outside your ownership.

## Acceptance criteria
{json.dumps(item.acceptance_criteria)}

## Completed dependency summaries
{json.dumps(dependency_results)}

Implement and run relevant tests. Do not create PRs or launch child agents.
Commit only in your assigned branch. Once completion is accepted, stop editing and wait.
Report JSON:
{{"status":"completed", "summary":"Changes and integration notes", "filesChanged":[],
 "tests":{{"executed":1,"passed":1,"failed":0,"notes":"Actual command and evidence"}},
 "assumptions":[],"knownIssues":[]}}
Use failed or blocked status if unfinished; never claim tests ran when they did not.
{_flowctl_block(ctx)}
"""


def integration(ctx: PromptContext, workers: list[dict], summary: str) -> str:
    return f"""# Integration implementer for {ctx.jira_key}
Work in {ctx.worktree_root}, branch {ctx.branch_name}. All workers have completed.
Read the approved requirements and acceptance criteria in {ctx.task_dir / 'requirements'}.
Do not read worker conversation histories. Their checkpoint summaries are below.

## Orchestrator instructions
{summary}

## Worker checkpoints
{json.dumps(workers)}

Merge each exact checkpoint SHA below into this branch using local Git operations, in the
listed dependency order. Skip checkpoints already ancestors of HEAD. Never reset or discard
existing changes. Resolve integration conflicts here, never in worker worktrees.
Complete any shared wiring required by the approved scope. Run the combined test suite and
verify all acceptance criteria. Report limitations honestly. Only this integrated checkpoint
will advance to code/security review. Do not create PRs or launch more agents.
Report JSON:
{{"status":"completed", "summary":"Integration, conflicts and verification", "filesChanged":[],
 "tests":{{"executed":1,"passed":1,"failed":0,"notes":"Actual combined test evidence"}},
 "assumptions":[],"knownIssues":[]}}
{_flowctl_block(ctx)}
"""
