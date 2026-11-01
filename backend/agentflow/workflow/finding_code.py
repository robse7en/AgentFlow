"""The code a review finding points at, for the Findings tab: the file at the reviewed commit, the
task's change to it, and the same place in the task's current commit. Everything is read from the
repository's object store, so it still works after the task worktree is cleaned up.

The Rust port is `engine/finding_code.rs`; both answer `GET /api/tasks/{key}/findings/{id}/code`
with the same JSON.
"""

from __future__ import annotations

import string
from pathlib import Path
from typing import Any

from ..database.models import ReviewFinding, Task
from ..services.git_workspace import GitError, GitService
from ..services.process import ProcessError

# Larger files (or diffs) are not sent to the UI.
MAX_BYTES = 1_000_000

_ASCII_LOWER = str.maketrans(string.ascii_uppercase, string.ascii_lowercase)


def repository_path(file: str, roots: tuple[str, ...] = ()) -> str | None:
    """A finding's file as a path inside the repository (`/` separated, no leading `./`), or None
    when it is absolute or climbs out with `..`. An absolute path under one of `roots` (the task
    worktree or repository, compared without regard to ASCII case) is made relative to it first."""
    path = file.strip().replace("\\", "/")
    for root in roots:
        prefix = root.strip().replace("\\", "/").rstrip("/") + "/"
        if len(prefix) > 1 and len(path) > len(prefix) and \
                path[:len(prefix)].translate(_ASCII_LOWER) == prefix.translate(_ASCII_LOWER):
            path = path[len(prefix):]
            break
    while path.startswith("./"):
        path = path[2:]
    absolute = path.startswith("/") or path[1:2] == ":"
    if not path or absolute or ".." in path.split("/"):
        return None
    return path


def split_lines(text: str) -> list[str]:
    """A file's lines without their line endings."""
    text = text[:-1] if text.endswith("\n") else text
    if not text:
        return []
    return [line[:-1] if line.endswith("\r") else line for line in text.split("\n")]


def _range(text: str) -> tuple[int, int] | None:
    """`-a,b` or `+c,d` from a hunk header: the start and count (count 1 when left out)."""
    start, _, count = text[1:].partition(",")
    try:
        return int(start), int(count or "1")
    except ValueError:
        return None


def parse_hunks(diff: str) -> list[dict[str, Any]]:
    """The hunks of a unified diff of one file, each with its rows numbered on both sides."""
    hunks: list[dict[str, Any]] = []
    old = new = 0
    for line in diff.split("\n"):
        line = line[:-1] if line.endswith("\r") else line
        if line.startswith("@@ "):
            parts = line[3:].split(" ")
            ranges = [_range(part) for part in parts[:2]] if len(parts) >= 2 else [None, None]
            if None in ranges:
                continue
            (old_start, old_count), (new_start, new_count) = ranges
            old, new = old_start, new_start
            hunks.append({"header": line, "oldStart": old_start, "oldCount": old_count,
                          "newStart": new_start, "newCount": new_count, "rows": []})
            continue
        if not hunks:
            continue
        kind = {" ": "context", "-": "del", "+": "add"}.get(line[:1])
        if kind is None:  # "\ No newline at end of file", or the blank line after the last row
            continue
        text = line[1:]
        if kind == "context":
            row = {"kind": kind, "old": old, "new": new, "text": text}
        elif kind == "del":
            row = {"kind": kind, "old": old, "new": None, "text": text}
        else:
            row = {"kind": kind, "old": None, "new": new, "text": text}
        old += kind != "add"
        new += kind != "del"
        hunks[-1]["rows"].append(row)
    return hunks


def _span(start: int, count: int) -> tuple[int, int]:
    """A hunk side's lines as a half-open range; git gives an empty side the line before it."""
    start = start + 1 if count == 0 else start
    return start, start + count


def map_line(hunks: list[dict[str, Any]], n: int) -> int:
    """Where old line `n` is after the change the hunks describe. A deleted line maps to the next
    line that replaced it."""
    shift = 0
    for hunk in hunks:
        old_start, old_end = _span(hunk["oldStart"], hunk["oldCount"])
        new_start, new_end = _span(hunk["newStart"], hunk["newCount"])
        if n < old_start:
            break
        if n >= old_end:
            shift = new_end - old_end
            continue
        rows = hunk["rows"]
        at = next((i for i, row in enumerate(rows) if row["old"] == n), None)
        if at is None:
            break
        return next((row["new"] for row in rows[at:] if row["new"] is not None), max(new_end - 1, new_start, 1))
    return n + shift


def touches(hunks: list[dict[str, Any]], first: int, last: int) -> bool:
    """Whether the hunks delete or replace any old line from `first` to `last`, or add lines between
    two of them."""
    for hunk in hunks:
        # The old line the next row comes after; git gives an empty old side the line before it.
        before = hunk["oldStart"] - (hunk["oldCount"] != 0)
        for row in hunk["rows"]:
            if row["old"] is not None:
                before = row["old"]
            if row["kind"] == "del" and first <= before <= last:
                return True
            if row["kind"] == "add" and first <= before < last:
                return True
    return False


def _unshowable(path: str, text: str) -> str | None:
    """Why a file's text can't be shown, if it can't."""
    if "\0" in text:
        return f"{path} is a binary file"
    if len(text.encode("utf-8", errors="replace")) > MAX_BYTES:
        return f"{path} is too large to show"
    return None


async def finding_code(git: GitService, task: Task, finding: ReviewFinding) -> dict[str, Any]:
    out: dict[str, Any] = {"file": finding.file, "line": finding.line, "endLine": finding.end_line,
                           "reviewedSha": finding.reviewed_sha, "snapshot": None, "diff": None, "now": None}
    unavailable: dict[str, str] = {}
    try:
        await _views(git, task, finding, out, unavailable)
    except _Unavailable as exc:
        message = str(exc)
        for view in ("snapshot", "diff", "now"):
            if out[view] is None and view not in unavailable:
                unavailable[view] = message
    out["unavailable"] = unavailable
    return out


class _Unavailable(Exception):
    pass


async def _views(git: GitService, task: Task, finding: ReviewFinding, out: dict[str, Any],
                 unavailable: dict[str, str]) -> None:
    file = finding.file if finding.file and finding.file.strip() else None
    if file is None:
        raise _Unavailable("The finding names no file")
    reviewed = finding.reviewed_sha
    if not reviewed:
        raise _Unavailable("The finding has no reviewed commit")
    path = repository_path(file, (task.worktree_path, task.repository_root))
    if path is None:
        raise _Unavailable(f"{file} is outside the repository")
    root = Path(task.repository_root)
    try:
        text = await git.file_at(root, reviewed, path, MAX_BYTES)
        if text is None:
            raise _Unavailable(f"{path} is not in {reviewed[:7]}")
        reason = _unshowable(path, text)
        if reason:
            raise _Unavailable(reason)
        out["snapshot"] = {"sha": reviewed, "lines": split_lines(text)}

        base = await git.fork_point(root, reviewed, task.base_branch)
        if base is None:
            unavailable["diff"] = f"Cannot find {task.base_branch} to compare with"
        elif base == reviewed:
            unavailable["diff"] = f"The task branch is already part of {task.base_branch}"
        else:
            diff = await git.file_diff(root, base, reviewed, path)
            if len(diff.encode("utf-8", errors="replace")) > MAX_BYTES:
                unavailable["diff"] = f"The change to {path} is too large to show"
            else:
                out["diff"] = {"from": base, "to": reviewed, "hunks": parse_hunks(diff)}

        current = task.current_sha or reviewed
        if current == reviewed:
            lines, hunks = out["snapshot"]["lines"], []
        else:
            text = await git.file_at(root, current, path, MAX_BYTES)
            if text is None:
                unavailable["now"] = f"{path} is not in {current[:7]}"
                return
            reason = _unshowable(path, text)
            if reason:
                unavailable["now"] = reason
                return
            lines, hunks = split_lines(text), parse_hunks(await git.file_diff(root, reviewed, current, path))
    except (GitError, ProcessError, OSError) as exc:  # a timeout, or the repository is gone
        raise _Unavailable(str(exc)) from exc
    line = map_line(hunks, finding.line) if finding.line is not None else None
    end_line = map_line(hunks, finding.end_line) if finding.end_line is not None else None
    changed = finding.line is not None and touches(hunks, finding.line, finding.end_line or finding.line)
    out["now"] = {"sha": current, "lines": lines, "line": line, "endLine": end_line, "changed": changed,
                  "hunks": hunks}
