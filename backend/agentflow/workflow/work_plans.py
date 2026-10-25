"""Validated, bounded task decomposition without conversation history."""

from __future__ import annotations

from pathlib import PurePosixPath

from pydantic import BaseModel, ConfigDict, Field, field_validator, model_validator


class WorkItemPlan(BaseModel):
    model_config = ConfigDict(populate_by_name=True, extra="forbid")

    key: str = Field(pattern=r"^[a-z][a-z0-9_-]{0,47}$")
    title: str = Field(min_length=1, max_length=120)
    brief: str = Field(min_length=1, max_length=12000)
    owned_paths: list[str] = Field(alias="ownedPaths", min_length=1, max_length=80)
    acceptance_criteria: list[str] = Field(alias="acceptanceCriteria", min_length=1, max_length=40)
    dependencies: list[str] = Field(default_factory=list, max_length=30)

    @field_validator("owned_paths")
    @classmethod
    def safe_paths(cls, paths: list[str]) -> list[str]:
        for path in paths:
            parts = PurePosixPath(path).parts
            if (not parts or path.startswith("/") or "\\" in path or ":" in path
                    or any(p.casefold() in ("..", ".git") for p in parts)
                    or any(p.endswith((".", " ")) for p in parts)
                    or any(c in path for c in "*?[]\x00\n\r")):
                raise ValueError("ownedPaths must contain explicit repository-relative files or directories")
        return [str(PurePosixPath(path)) for path in paths]


class WorkPlan(BaseModel):
    model_config = ConfigDict(extra="forbid")

    summary: str = Field(min_length=1, max_length=4000)
    subtasks: list[WorkItemPlan] = Field(min_length=1, max_length=30)

    @model_validator(mode="after")
    def validate_dependencies(self):
        by_key = {item.key: item for item in self.subtasks}
        if len(by_key) != len(self.subtasks):
            raise ValueError("Subtask keys must be unique")
        resolved: set[str] = set()
        while len(resolved) < len(by_key):
            ready = {key for key, item in by_key.items()
                     if key not in resolved and set(item.dependencies) <= resolved}
            if not ready:
                raise ValueError("Dependencies contain a cycle or unknown subtask")
            resolved.update(ready)
        # A shared file must have a single owner. Cross-cutting edits belong to integration.
        for index, left in enumerate(self.subtasks):
            for right in self.subtasks[index + 1:]:
                for a in left.owned_paths:
                    for b in right.owned_paths:
                        if owns_path(a, b) or owns_path(b, a):
                            raise ValueError(f"Overlapping ownership: {left.key} and {right.key}")
        return self


def owns_path(owner: str, path: str) -> bool:
    """Use case-insensitive comparison so Windows paths cannot bypass ownership."""
    owner, path = owner.casefold().rstrip("/"), path.casefold().rstrip("/")
    return path == owner or path.startswith(owner + "/")
