"""OpenAI Codex CLI adapter."""

from __future__ import annotations

import asyncio
import itertools
import json
import os
from datetime import datetime
from pathlib import Path

from .base import HarnessAdapter, LaunchSpec, ScreenPatterns, SessionHandle


def _normalize(path: str) -> str:
    return os.path.normcase(os.path.normpath(path))


def _subdirs_desc(directory: Path) -> list[Path]:
    if not directory.is_dir():
        return []
    return sorted((p for p in directory.iterdir() if p.is_dir()), key=lambda p: p.name, reverse=True)


def _day_directories_desc(sessions_root: Path):
    for year_dir in _subdirs_desc(sessions_root):
        for month_dir in _subdirs_desc(year_dir):
            yield from _subdirs_desc(month_dir)


def _read_session_meta(path: Path) -> dict | None:
    try:
        with path.open("r", encoding="utf-8") as f:
            first_line = f.readline().strip()
    except OSError:
        return None
    if not first_line:
        return None
    try:
        entry = json.loads(first_line)
    except json.JSONDecodeError:
        return None
    if entry.get("type") != "session_meta":
        return None
    return entry.get("payload") or {}


def _parse_timestamp(value: object) -> datetime | None:
    if not isinstance(value, str):
        return None
    try:
        return datetime.fromisoformat(value.replace("Z", "+00:00"))
    except ValueError:
        return None


class CodexAdapter(HarnessAdapter):
    type_name = "codex"
    supports_resume = True
    # Codex CLI's own command-approval prompts and its "Working"/"esc to interrupt"
    # activity indicator while a turn is in progress.
    screen_patterns = ScreenPatterns(
        blocked=(r"allow command", r"approve this (command|action)", r"do you want to allow"),
        working=(r"\bworking\b", r"esc to interrupt", r"\bthinking\b"),
        idle=(r"send a message", r"^\s*›\s*$"),
    )

    def model_flag(self) -> str | None:
        return self.config.model_argument or "--model"

    def reasoning_flag(self) -> str | None:
        # Codex expresses reasoning effort through a config override.
        return self.config.reasoning_argument or "-c"

    def option_args(self, model, reasoning) -> list[str]:
        args: list[str] = []
        if model:
            args += [self.model_flag(), model]
        mapped = self.map_reasoning(reasoning)
        if mapped:
            flag = self.reasoning_flag()
            if flag == "-c":
                args += ["-c", f'model_reasoning_effort="{mapped}"']
            else:
                args += [flag, mapped]
        return args

    def build_resume(self, session: SessionHandle, *, model, reasoning, initial_prompt) -> LaunchSpec | None:
        # session.harness_session_id comes from capture_session_id below - Codex assigns its
        # own id, so there's nothing to resume until a previous launch's id was discovered.
        if not session.harness_session_id:
            return None
        argv = [*self.base_argv(), "resume", session.harness_session_id, *self.option_args(model, reasoning), initial_prompt]
        return LaunchSpec(argv=argv, env=dict(self.config.env), harness_session_id=session.harness_session_id)

    async def capture_session_id(self, cwd: Path, since: datetime) -> str | None:
        """Codex has no launch-time flag to assign its own session id (unlike Claude's
        --session-id), so this polls ~/.codex/sessions for the rollout file this exact
        launch just created: matched by cwd and a session_meta timestamp no older than
        `since`. AgentFlow reuses the same cwd for every non-worker role on a task (code
        review and security review can even run concurrently in it), so cwd alone is not
        a safe match - an ambiguous window (more than one candidate) gives up rather than
        risk wiring a future resume to the wrong role's conversation.
        """
        target = _normalize(str(cwd))
        sessions_root = Path.home() / ".codex" / "sessions"
        for _ in range(40):
            await asyncio.sleep(0.25)
            candidates: list[str] = []
            for day_dir in itertools.islice(_day_directories_desc(sessions_root), 2):
                for path in day_dir.glob("*.jsonl"):
                    meta = _read_session_meta(path)
                    if meta is None or meta.get("source") != "cli":
                        continue
                    if _normalize(str(meta.get("cwd") or "")) != target:
                        continue
                    started = _parse_timestamp(meta.get("timestamp"))
                    if started is None or started < since:
                        continue
                    session_id = meta.get("session_id") or meta.get("id")
                    if session_id and session_id not in candidates:
                        candidates.append(session_id)
            if len(candidates) == 1:
                return candidates[0]
            if len(candidates) > 1:
                return None
        return None
