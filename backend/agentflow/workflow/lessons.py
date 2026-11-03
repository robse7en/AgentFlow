"""Lessons agents propose for their role's repository guidance (`flowctl lesson`).

A proposal is only a record on the task, in `<task>/lessons.json`. It reaches a briefing only after a
human approves it into `.agentflow-instructions/<role>.md` in the task's original repository checkout,
commits that file, and a later task's worktree is cut from the commit. Lesson text is agent output:
shown as plain text, and normalised to a single bullet line before it is ever written.

`normalize` and `append_lesson` are pure and byte-identical in the Rust port (golden lessons-unit)."""
import json
from pathlib import Path
from typing import Any

MAX_LESSON_CHARS = 300
MAX_PER_RUN = 5
MAX_PER_TASK = 50
MAX_FILE_BYTES = 1024 * 1024
HEADING = "## Lessons from past tasks"
FILE = "lessons.json"


class LessonError(ValueError):
    pass


def normalize(text: str) -> str:
    """One line: every whitespace run (line breaks included) becomes one space, ends trimmed. A
    lesson can therefore never add a heading, a list or any other structure of its own."""
    return " ".join(text.split())


def validate(text: str) -> str:
    """The normalised lesson, or LessonError saying what to fix."""
    line = normalize(text)
    if not line:
        raise LessonError("A lesson needs text")
    if "\x00" in line:
        raise LessonError("A lesson cannot contain NUL characters")
    if len(line) > MAX_LESSON_CHARS:
        raise LessonError(f"A lesson is at most {MAX_LESSON_CHARS} characters (this one has {len(line)})")
    return line


def append_lesson(existing: str | None, line: str) -> str:
    """`existing` guidance with `- line` added at the end, under the lessons heading. The heading is
    added when the file's last heading is another one. The file's own line endings are kept."""
    bullet = f"- {line}"
    if not existing:
        return f"{HEADING}\n\n{bullet}\n"
    newline = "\r\n" if "\r\n" in existing else "\n"
    text = existing if existing.endswith("\n") else existing + newline
    headings = [row.rstrip("\r") for row in text.split("\n") if row.startswith("#")]
    if not headings or headings[-1] != HEADING:
        text += newline + HEADING + newline + newline
    return text + bullet + newline


def path(task_dir: Path) -> Path:
    return task_dir / FILE


def read(task_dir: Path) -> list[dict[str, Any]]:
    """The task's lessons, oldest first. A missing file is no lessons; an unreadable one fails
    closed (LessonError) rather than being replaced."""
    target = path(task_dir)
    if not target.exists():
        return []
    try:
        with target.open("rb") as stream:
            raw = stream.read(MAX_FILE_BYTES + 1)
        if len(raw) > MAX_FILE_BYTES:
            raise LessonError("The task's lessons file is too large")
        data = json.loads(raw.decode("utf-8"))
        if data.get("schema") != 1 or not isinstance(data.get("lessons"), list):
            raise LessonError("The task's lessons file has an unknown format")
        return data["lessons"]
    except (OSError, UnicodeError, ValueError, AttributeError) as exc:
        if isinstance(exc, LessonError):
            raise
        raise LessonError(f"The task's lessons file is unreadable: {exc}") from exc
