"""One offline historical task for installer migration acceptance, with no running agents."""
import sqlite3
import sys
from pathlib import Path

from_database, database, source = sys.argv[1:]
root = Path(source)
with sqlite3.connect(database) as connection:
    # Include outstanding WAL pages; copying the .db file alone can omit the entire schema.
    with sqlite3.connect(from_database) as original:
        original.backup(connection)
    connection.execute(
        """insert into tasks (jira_key, repository_path, repository_root, base_branch,
        branch_name, worktree_path, zellij_session_name, task_dir, status, review_iteration,
        max_review_iterations, max_requirements_revisions, agent_overrides, created_at, updated_at)
        values (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)""",
        ("HISTORY-1", source, source, "main", "agent/HISTORY-1",
         str(root / "worktrees"), "installer-history", str(root / "data" / "tasks"),
         "CANCELLED", 0, 3, 3, "{}", "2026-10-01 00:00:00.000000", "2026-10-01 00:00:00.000000"),
    )
