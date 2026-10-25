"""Task workspace layout under <taskDirectory>/<JIRA-KEY>/."""

from __future__ import annotations

import json
import os
from datetime import datetime, timezone
from pathlib import Path
from typing import Any


class TaskWorkspace:
    def __init__(self, task_root: Path, jira_key: str) -> None:
        self.root = task_root / jira_key
        self.context_dir = self.root / "context"
        self.attachments_dir = self.context_dir / "attachments"
        self.requirements_dir = self.root / "requirements"
        self.runs_dir = self.root / "runs"
        self.findings_dir = self.root / "findings"
        self.final_dir = self.root / "final"

    def create(self, task_meta: dict[str, Any], additional_context: str | None) -> None:
        for directory in (self.root, self.context_dir, self.requirements_dir, self.runs_dir, self.findings_dir, self.final_dir):
            directory.mkdir(parents=True, exist_ok=True)
        self.write_json(self.root / "task.json", task_meta)
        (self.context_dir / "additional-context.md").write_text(
            (additional_context or "").strip() + "\n", encoding="utf-8"
        )

    # helpers -----------------------------------------------------------------------
    def run_dir(self, role_dir: str, run_number: int) -> Path:
        directory = self.runs_dir / role_dir / f"{run_number:02d}"
        directory.mkdir(parents=True, exist_ok=True)
        return directory

    @staticmethod
    def write_credential(path: Path, data: dict) -> None:
        """Create a new run credential without a world-readable creation window."""
        descriptor = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
        with os.fdopen(descriptor, "w", encoding="utf-8") as output:
            json.dump(data, output)

    @staticmethod
    def write_json(path: Path, data: Any) -> Path:
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(json.dumps(data, indent=2, default=str) + "\n", encoding="utf-8")
        return path

    @staticmethod
    def read_json(path: Path) -> Any:
        return json.loads(path.read_text(encoding="utf-8"))

    def read_text(self, relative: str, default: str = "") -> str:
        path = self.root / relative
        return path.read_text(encoding="utf-8") if path.exists() else default

    def requirement_files(self) -> dict[str, str]:
        names = {
            "requirements": "requirements.md",
            "acceptanceCriteria": "acceptance-criteria.md",
            "assumptions": "assumptions.md",
            "implementationPlan": "implementation-plan.md",
            "risks": "risks.md",
        }
        return {key: self.read_text(f"requirements/{name}") for key, name in names.items()}

    def jira_issue(self) -> dict[str, Any] | None:
        """The last synced copy of the Jira ticket (jira-issue.md) and when it was synced, or None
        before the first sync."""
        path = self.context_dir / "jira-issue.md"
        if not path.is_file():
            return None
        try:
            synced_at = self.read_json(self.context_dir / "jira-manifest.json").get("syncedAt")
        except (OSError, ValueError, AttributeError):
            synced_at = None
        return {"markdown": path.read_text(encoding="utf-8"), "syncedAt": synced_at}

    def requirements_feedback(self) -> list[dict[str, Any]]:
        """Past "Request changes" rounds on the requirements gate, oldest first."""
        if not self.requirements_dir.is_dir():
            return []
        entries = []
        for path in sorted(self.requirements_dir.glob("feedback-*.md")):
            try:
                iteration = int(path.stem.split("-")[-1])
            except ValueError:
                continue
            entries.append({
                "iteration": iteration,
                "feedback": path.read_text(encoding="utf-8").strip(),
                "at": datetime.fromtimestamp(path.stat().st_mtime, tz=timezone.utc).isoformat(),
            })
        return entries
