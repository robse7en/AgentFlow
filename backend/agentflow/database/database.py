"""SQLite engine / session factory."""

from __future__ import annotations

from contextlib import contextmanager
from pathlib import Path
from typing import Iterator
from uuid import uuid4

from sqlalchemy import create_engine, event, inspect, text
from sqlalchemy.orm import Session, sessionmaker

from .models import Base


class Database:
    def __init__(self, path: Path) -> None:
        path.parent.mkdir(parents=True, exist_ok=True)
        self.path = path
        self.engine = create_engine(
            f"sqlite:///{path.as_posix()}",
            connect_args={"check_same_thread": False},
            future=True,
        )

        @event.listens_for(self.engine, "connect")
        def _set_pragmas(dbapi_connection, _record):  # pragma: no cover - trivial
            cursor = dbapi_connection.cursor()
            cursor.execute("PRAGMA journal_mode=WAL")
            cursor.execute("PRAGMA foreign_keys=ON")
            cursor.close()

        self._factory = sessionmaker(bind=self.engine, expire_on_commit=False, future=True)

    def create_all(self) -> None:
        Base.metadata.create_all(self.engine)
        self._upgrade_agent_questions()
        self._upgrade_tasks_pr_tracking()
        self._upgrade_agent_sessions_screen_state()
        self._upgrade_tasks_jira_title()
        self._upgrade_tasks_requirements_revisions()
        self._upgrade_usage_billing()
        self._upgrade_review_findings_lineage()
        self._upgrade_tasks_archived()
        self._upgrade_tasks_project()
        self._upgrade_review_findings_end_line()

    def _upgrade_review_findings_end_line(self) -> None:
        """Add the last line of a multi-line finding to existing local databases."""
        with self.engine.begin() as connection:
            columns = {column["name"] for column in inspect(connection).get_columns("review_findings")}
            if "end_line" not in columns:
                connection.exec_driver_sql("ALTER TABLE review_findings ADD COLUMN end_line INTEGER")

    def _upgrade_tasks_project(self) -> None:
        """Add the project assignment to existing local databases (create_all made the projects table)."""
        with self.engine.begin() as connection:
            columns = {column["name"] for column in inspect(connection).get_columns("tasks")}
            if "project_id" not in columns:
                connection.exec_driver_sql("ALTER TABLE tasks ADD COLUMN project_id INTEGER REFERENCES projects (id)")
            connection.exec_driver_sql("CREATE INDEX IF NOT EXISTS ix_tasks_project_id ON tasks (project_id)")

    def _upgrade_tasks_archived(self) -> None:
        """Add the archive marker to existing local databases."""
        with self.engine.begin() as connection:
            columns = {column["name"] for column in inspect(connection).get_columns("tasks")}
            if "archived_at" not in columns:
                connection.exec_driver_sql("ALTER TABLE tasks ADD COLUMN archived_at DATETIME")

    def _upgrade_review_findings_lineage(self) -> None:
        """Add finding lineage, reviewer verdicts and implementer responses to existing local databases."""
        with self.engine.begin() as connection:
            columns = {column["name"] for column in inspect(connection).get_columns("review_findings")}
            if "prior_finding_id" not in columns:
                connection.exec_driver_sql("ALTER TABLE review_findings ADD COLUMN prior_finding_id INTEGER")
            if "occurrence" not in columns:
                connection.exec_driver_sql("ALTER TABLE review_findings ADD COLUMN occurrence INTEGER NOT NULL DEFAULT 1")
            if "resolution" not in columns:
                connection.exec_driver_sql("ALTER TABLE review_findings ADD COLUMN resolution TEXT")
            if "response" not in columns:
                connection.exec_driver_sql("ALTER TABLE review_findings ADD COLUMN response VARCHAR(16)")
            if "response_note" not in columns:
                connection.exec_driver_sql("ALTER TABLE review_findings ADD COLUMN response_note TEXT")

    def _upgrade_usage_billing(self) -> None:
        """Preserve historical reports without guessing how historical runs were billed."""
        with self.engine.begin() as connection:
            task_columns = {column["name"] for column in inspect(connection).get_columns("tasks")}
            if "api_budget_usd" not in task_columns:
                connection.exec_driver_sql("ALTER TABLE tasks ADD COLUMN api_budget_usd FLOAT")
            columns = {column["name"] for column in inspect(connection).get_columns("run_evidence")}
            if "billing_mode" not in columns:
                connection.exec_driver_sql(
                    "ALTER TABLE run_evidence ADD COLUMN billing_mode VARCHAR(16) NOT NULL DEFAULT 'unknown'"
                )

    def _upgrade_agent_questions(self) -> None:
        """Add question response fields to existing local databases."""
        with self.engine.begin() as connection:
            columns = {column["name"] for column in inspect(connection).get_columns("agent_questions")}
            if "uid" not in columns:
                connection.exec_driver_sql("ALTER TABLE agent_questions ADD COLUMN uid VARCHAR(36)")
            if "choices" not in columns:
                connection.exec_driver_sql("ALTER TABLE agent_questions ADD COLUMN choices JSON NOT NULL DEFAULT '[]'")
            if "answer" not in columns:
                connection.exec_driver_sql("ALTER TABLE agent_questions ADD COLUMN answer TEXT")
            if "decision" not in columns:
                connection.exec_driver_sql("ALTER TABLE agent_questions ADD COLUMN decision JSON")

            missing_uids = connection.execute(
                text("SELECT id FROM agent_questions WHERE uid IS NULL")
            ).fetchall()
            for (question_id,) in missing_uids:
                connection.execute(
                    text("UPDATE agent_questions SET uid = :uid WHERE id = :id"),
                    {"uid": str(uuid4()), "id": question_id},
                )
            connection.exec_driver_sql(
                "CREATE UNIQUE INDEX IF NOT EXISTS ix_agent_questions_uid ON agent_questions(uid)"
            )

    def _upgrade_tasks_pr_tracking(self) -> None:
        """Add PR-created/merged tracking (and the GitHub sync pause) to existing local databases."""
        with self.engine.begin() as connection:
            columns = {column["name"] for column in inspect(connection).get_columns("tasks")}
            if "pr_url" not in columns:
                connection.exec_driver_sql("ALTER TABLE tasks ADD COLUMN pr_url TEXT")
            if "pr_created_at" not in columns:
                connection.exec_driver_sql("ALTER TABLE tasks ADD COLUMN pr_created_at DATETIME")
            if "merged_at" not in columns:
                connection.exec_driver_sql("ALTER TABLE tasks ADD COLUMN merged_at DATETIME")
            if "pr_sync_paused" not in columns:
                connection.exec_driver_sql("ALTER TABLE tasks ADD COLUMN pr_sync_paused BOOLEAN NOT NULL DEFAULT 0")
            if "merged_head_sha" not in columns:
                connection.exec_driver_sql("ALTER TABLE tasks ADD COLUMN merged_head_sha VARCHAR(64)")
            if "pr_base_branch" not in columns:
                connection.exec_driver_sql("ALTER TABLE tasks ADD COLUMN pr_base_branch VARCHAR(255)")

    def _upgrade_agent_sessions_screen_state(self) -> None:
        """Add live screen-classification fields to existing local databases."""
        with self.engine.begin() as connection:
            columns = {column["name"] for column in inspect(connection).get_columns("agent_sessions")}
            if "screen_state" not in columns:
                connection.exec_driver_sql("ALTER TABLE agent_sessions ADD COLUMN screen_state VARCHAR(16)")
            if "screen_state_at" not in columns:
                connection.exec_driver_sql("ALTER TABLE agent_sessions ADD COLUMN screen_state_at DATETIME")

    def _upgrade_tasks_jira_title(self) -> None:
        """Add the Jira title field to existing local databases."""
        with self.engine.begin() as connection:
            columns = {column["name"] for column in inspect(connection).get_columns("tasks")}
            if "jira_title" not in columns:
                connection.exec_driver_sql("ALTER TABLE tasks ADD COLUMN jira_title TEXT")

    def _upgrade_tasks_requirements_revisions(self) -> None:
        """Add the requirements-revision cap to existing local databases."""
        with self.engine.begin() as connection:
            columns = {column["name"] for column in inspect(connection).get_columns("tasks")}
            if "max_requirements_revisions" not in columns:
                connection.exec_driver_sql(
                    "ALTER TABLE tasks ADD COLUMN max_requirements_revisions INTEGER NOT NULL DEFAULT 3"
                )

    @contextmanager
    def session(self) -> Iterator[Session]:
        session = self._factory()
        try:
            yield session
            session.commit()
        except Exception:
            session.rollback()
            raise
        finally:
            session.close()
