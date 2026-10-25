"""Stable dashboard actions, including parallel nodes hidden by a single phase label."""
from .states import POST_READY_STATUSES, TaskStatus


def attention_items(task, runs, questions):
    if task.status in (*POST_READY_STATUSES, TaskStatus.CANCELLED):
        return []
    actions = {
        "WAITING_FOR_USER": "Answer questions", "WAITING_FOR_APPROVAL": "Review approval",
        "FAILED": "Review failure", "INTERRUPTED": "Recover agent", "READY": "Start stage",
    }
    priorities = {state: index for index, state in enumerate(actions)}
    current_runs = {}
    for run in runs:
        if run.node_id not in current_runs or run.id > current_runs[run.node_id].id:
            current_runs[run.node_id] = run
    items = []
    for node in task.nodes:
        if node.state not in actions:
            continue
        run = current_runs.get(node.id)
        pending = [q for q in questions if not q.answered and run and q.node_run_id == run.id
                   and run.state == "WAITING_FOR_USER"]
        since = min((q.created_at for q in pending), default=node.updated_at)
        items.append({"nodeKey": node.key, "label": node.label, "state": node.state,
                      "since": since.isoformat() + ("Z" if since.tzinfo is None else "") if since else None,
                      "questions": len(pending), "action": actions[node.state]})
    return sorted(items, key=lambda item: (priorities[item["state"]], item["since"] or ""))
