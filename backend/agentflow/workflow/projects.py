"""Project naming and the JSON the UI reads; the project actions live on WorkflowEngine."""
from __future__ import annotations

import re
from datetime import datetime, timezone
from typing import Any

from ..database.models import Project

# Names, not colours: the UI maps each to a light and a dark value.
PROJECT_COLORS = ("blue", "purple", "orange", "teal", "green", "red", "pink", "slate")
# `?project=none` is the dashboard's "No project" filter.
RESERVED_SLUGS = frozenset({"none"})
MAX_SLUG_LENGTH = 48


def slug_base(name: str) -> str:
    """Lowercase ASCII words joined by '-'; "project" when the name has none."""
    words = re.findall(r"[a-z0-9]+", name.lower())
    return "-".join(words)[:MAX_SLUG_LENGTH].strip("-") or "project"


def unique_slug(name: str, taken: set[str]) -> str:
    base = slug_base(name)
    slug, n = base, 1
    while slug in taken or slug in RESERVED_SLUGS:
        n += 1
        suffix = f"-{n}"
        slug = base[:MAX_SLUG_LENGTH - len(suffix)] + suffix
    return slug


def next_color(used: list[str]) -> str:
    """The least-used colour, in palette order, so a new project stands apart from the rest."""
    return min(PROJECT_COLORS, key=lambda color: (used.count(color), PROJECT_COLORS.index(color)))


def clean_overrides(overrides: dict[str, Any]) -> dict[str, dict[str, str]]:
    """Drop empty fields and roles, as task creation stores them."""
    cleaned = {role: o.model_dump(exclude_none=True) for role, o in overrides.items()}
    return {role: o for role, o in cleaned.items() if o}


def _iso(value: datetime | None) -> str | None:
    if value is None:
        return None
    if value.tzinfo is None:
        value = value.replace(tzinfo=timezone.utc)
    return value.isoformat()


def project_dict(project: Project) -> dict[str, Any]:
    return {
        "id": project.id, "slug": project.slug, "name": project.name, "color": project.color,
        "description": project.description,
        "defaultRepositoryPath": project.default_repository_path,
        "defaultProjectRelativePath": project.default_project_relative_path,
        "defaultBaseBranch": project.default_base_branch,
        "agentOverrides": project.agent_overrides or {},
        "archivedAt": _iso(project.archived_at),
        "createdAt": _iso(project.created_at), "updatedAt": _iso(project.updated_at),
    }
