"""What a task snapshot says the human should hear about, for chat notifications.

Pure and platform-neutral: a snapshot goes in, `Notice`s keyed by a stable condition key come
out. The chat service announces keys it has not announced before and forgets keys whose
condition has cleared, so a condition that returns is announced again.

Only fixed labels, node labels and the task phase are used. Task status messages, node errors
and review findings are never included: they can carry finding titles or command output.
Question text and choices come from the agent and are untrusted; adapters must escape them.
"""

from __future__ import annotations

from dataclasses import dataclass, field
from typing import Any, Collection

from .states import NodeState, TaskStatus

# Statuses still worth announcing when chat first meets a task. Anything else is history, and is
# recorded silently rather than DMed as a burst when the integration is first switched on.
LIVE_STATUSES = {TaskStatus.ACTIVE, TaskStatus.NEEDS_HUMAN_REVIEW}

STATUS_SUMMARIES = {
    TaskStatus.NEEDS_HUMAN_REVIEW: "Needs human review: the review loop stopped. Open AgentFlow to decide how to continue.",
    TaskStatus.READY_FOR_PR: "Ready for PR: final approval is recorded.",
    TaskStatus.FAILED: "Task failed.",
    TaskStatus.CANCELLED: "Task cancelled.",
}

PR_BASE_MISMATCH = "The PR targets a different branch than the task started from. Open AgentFlow to retarget it."

NODE_SUMMARIES = {
    NodeState.WAITING_FOR_USER: "{label} is waiting for your answer",
    NodeState.WAITING_FOR_APPROVAL: "{label} is waiting for your approval",
    NodeState.FAILED: "{label} failed",
    NodeState.INTERRUPTED: "{label} was interrupted; choose Resume or Stop & retry in AgentFlow",
    NodeState.READY: "{label} is ready to start",
}


@dataclass(frozen=True)
class Notice:
    key: str
    summary: str
    # Untrusted agent text, present only when question text may be posted.
    question: str | None = None
    choices: tuple[str, ...] = field(default_factory=tuple)
    question_uid: str | None = None


def notices(snapshot: dict[str, Any], notify: Collection[str], include_question_text: bool) -> dict[str, Notice]:
    """Every condition in `snapshot` that is currently true and enabled in `notify`."""
    found: dict[str, Notice] = {}
    status = snapshot.get("status")
    if status in notify and status in STATUS_SUMMARIES:
        key = f"status:{status}"
        found[key] = Notice(key, STATUS_SUMMARIES[status])

    # An open PR into a branch other than the one the task started from. Part of the PR
    # notifications, so it follows READY_FOR_PR; clears once the PR is retargeted.
    target = snapshot.get("prBaseBranch")
    if (status == TaskStatus.PR_CREATED and TaskStatus.READY_FOR_PR in notify
            and target and target != snapshot.get("baseBranch")):
        key = "pr:base-mismatch"
        found[key] = Notice(key, PR_BASE_MISMATCH)

    questions = [q for q in snapshot.get("questions", []) if q.get("actionable")]
    for item in snapshot.get("attention", []):
        state, node_key, label = item.get("state"), item.get("nodeKey"), item.get("label") or item.get("nodeKey")
        if state not in notify or state not in NODE_SUMMARIES:
            continue
        pending = [q for q in questions if q.get("nodeKey") == node_key] if state == NodeState.WAITING_FOR_USER else []
        for question in pending:
            key = f"question:{question['uid']}"
            found[key] = Notice(
                key, f"{label} asked a question",
                question=question.get("message") if include_question_text else None,
                choices=tuple(question.get("choices") or ()) if include_question_text else (),
                question_uid=question["uid"],
            )
        if not pending:
            key = f"node:{node_key}:{state}"
            found[key] = Notice(key, NODE_SUMMARIES[state].format(label=label))
    return found
