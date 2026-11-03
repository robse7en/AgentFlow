from types import SimpleNamespace

from agentflow.workflow.engine import _finding_counts


def row(id: int, finding_id: str, iteration: int, status: str, blocking: bool = True) -> SimpleNamespace:
    return SimpleNamespace(id=id, finding_id=finding_id, iteration=iteration, status=status, blocking=blocking)


def test_each_finding_counts_once_and_blocks_only_while_open():
    rows = [
        # Raised in round 1, re-raised in round 2 and still open: one finding, blocking.
        row(1, "CODE-001", 1, "superseded"),
        row(4, "CODE-001", 2, "open"),
        # Fixed in round 2: counted, no longer blocking.
        row(2, "CODE-002", 1, "resolved"),
        # Open but not blocking.
        row(3, "SEC-001", 1, "open", blocking=False),
    ]
    assert _finding_counts(rows) == (3, 1)


def test_newest_row_wins_whatever_the_order():
    rows = [row(5, "CODE-001", 2, "resolved"), row(1, "CODE-001", 1, "open")]
    assert _finding_counts(rows) == (1, 0)


def test_no_findings():
    assert _finding_counts([]) == (0, 0)
