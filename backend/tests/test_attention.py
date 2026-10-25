from datetime import datetime, timezone
from types import SimpleNamespace as Item

from agentflow.workflow.attention import attention_items


def test_parallel_question_wins_over_running_agent_and_ignores_old_questions():
    now = datetime.now(timezone.utc)
    task = Item(status="ACTIVE", nodes=[
        Item(id=1, key="first", label="First", state="RUNNING", updated_at=now),
        Item(id=2, key="second", label="Second", state="WAITING_FOR_USER", updated_at=now),
    ])
    runs = [Item(id=1, node_id=2, state="WAITING_FOR_USER"),
            Item(id=2, node_id=2, state="WAITING_FOR_USER")]
    questions = [Item(node_run_id=i, answered=False, created_at=now) for i in (1, 2)]
    result = attention_items(task, runs, questions)
    assert len(result) == 1
    assert result[0]["nodeKey"] == "second"
    assert result[0]["questions"] == 1
    task.status = "CANCELLED"
    assert attention_items(task, runs, questions) == []
