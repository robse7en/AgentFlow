"""Harness adapter contract.

Workflow logic never touches CLI syntax; it asks an adapter to build a launch and to
deliver prompts. Interactive processes live inside zellij tabs so the human can steer them.
"""

from __future__ import annotations

import os
import re
import sys
import uuid
from abc import ABC
from dataclasses import dataclass, field
from datetime import datetime
from pathlib import Path

from ..config import HarnessConfig
from ..services.process import ProcessError, resolve_executable, run

# How many trailing non-blank rows of a pane's rendered viewport classify_screen looks
# at. A permission dialog or spinner line is always near the bottom; the scrollback
# above it (past conversation) is noise for this purpose.
_SCREEN_TAIL_ROWS = 12


@dataclass(frozen=True)
class ScreenPatterns:
    """Regexes (case-insensitive) matched against the tail of a pane's rendered screen
    to classify what a harness's CLI is doing right now - AgentFlow's equivalent of
    herdr's screen manifests. Checked in priority
    order blocked > working > idle, so a permission prompt rendered under a still-active
    spinner is reported as blocked, not working.
    """

    blocked: tuple[str, ...] = ()
    working: tuple[str, ...] = ()
    idle: tuple[str, ...] = ()


def _matches_any(text: str, patterns: tuple[str, ...]) -> bool:
    return any(re.search(pattern, text, re.IGNORECASE | re.MULTILINE) for pattern in patterns)


@dataclass
class HarnessCapabilities:
    key: str
    display_name: str
    type: str
    enabled: bool
    available: bool
    command: str
    resolved_path: str | None = None
    version: str | None = None
    supports_model: bool = True
    supports_reasoning: bool = True
    supports_resume: bool = False
    message: str | None = None


@dataclass
class LaunchSpec:
    """Everything needed to start a harness process for one node run."""

    argv: list[str]
    env: dict[str, str] = field(default_factory=dict)
    harness_session_id: str | None = None


@dataclass
class SessionHandle:
    """Runtime view of an agent session as the adapter needs it."""

    role: str
    zellij_session: str
    tab_name: str
    pane_id: str | None
    cwd: Path
    harness_session_id: str | None = None


class HarnessAdapter(ABC):
    type_name = "generic-cli"
    supports_resume = False
    # No fixed CLI screen to key off by default (generic-cli is config-driven); harnesses
    # for a specific known CLI override this with their own prompt/spinner/dialog text.
    screen_patterns: ScreenPatterns = ScreenPatterns()
    # Typed into an idle agent before its tab is closed, for CLIs that only write their final
    # usage when they exit normally. None: the tab is just closed.
    exit_command: str | None = None

    def __init__(self, key: str, config: HarnessConfig) -> None:
        self.key = key
        self.config = config

    # detection ---------------------------------------------------------------------
    async def detect(self) -> HarnessCapabilities:
        path = resolve_executable(self.config.command)
        caps = HarnessCapabilities(
            key=self.key,
            display_name=self.config.display_name or self.key,
            type=self.config.type,
            enabled=self.config.enabled,
            available=path is not None,
            command=self.config.command,
            resolved_path=path,
            supports_reasoning=self.reasoning_flag() is not None,
            supports_model=self.model_flag() is not None,
            supports_resume=self.supports_resume,
        )
        if path is None:
            caps.message = f"'{self.config.command}' not found"
            return caps
        try:
            result = await run([path, *self.version_args()], timeout=20)
            caps.version = (result.stdout or result.stderr).strip().splitlines()[0][:120] if (result.stdout or result.stderr).strip() else None
        except (ProcessError, OSError) as exc:
            caps.message = f"version check failed: {exc}"
        return caps

    def version_args(self) -> list[str]:
        return ["--version"]

    # argv construction -------------------------------------------------------------
    def model_flag(self) -> str | None:
        return self.config.model_argument

    def reasoning_flag(self) -> str | None:
        return self.config.reasoning_argument

    def map_reasoning(self, reasoning: str | None) -> str | None:
        if reasoning is None:
            return None
        return self.config.reasoning_map.get(reasoning, reasoning)

    def prompt_args(self, prompt: str) -> list[str]:
        flag = self.config.prompt_argument
        if flag is None or flag == "positional":
            return [prompt]
        return [flag, prompt]

    def base_argv(self) -> list[str]:
        exe = resolve_executable(self.config.command) or self.config.command
        return [exe, *self.config.args]

    def option_args(self, model: str | None, reasoning: str | None) -> list[str]:
        args: list[str] = []
        model_flag = self.model_flag()
        if model and model_flag:
            args += [model_flag, model]
        reasoning_flag = self.reasoning_flag()
        mapped = self.map_reasoning(reasoning)
        if mapped and reasoning_flag:
            args += [reasoning_flag, mapped]
        return args

    def build_launch(self, *, model: str | None, reasoning: str | None, initial_prompt: str) -> LaunchSpec:
        argv = [*self.base_argv(), *self.option_args(model, reasoning), *self.prompt_args(initial_prompt)]
        return LaunchSpec(argv=argv, env=dict(self.config.env))

    def build_resume(self, session: SessionHandle, *, model: str | None, reasoning: str | None, initial_prompt: str) -> LaunchSpec | None:
        """Argv to resume a previous conversation; None when unsupported."""
        return None

    async def capture_session_id(self, cwd: Path, since: datetime) -> str | None:
        """Best-effort discovery of a session id the CLI assigned itself after launch,
        for harnesses with no launch-time flag to choose one upfront (unlike Claude's
        --session-id). None when unsupported, or when discovery can't be done safely.
        """
        return None

    def classify_screen(self, viewport: list[str]) -> str:
        """Best-effort live state from a pane's rendered screen: idle | working | blocked | unknown.

        Heuristic, like herdr's screen manifests - an unfamiliar CLI release can
        misclassify. Unrecognized text stays "unknown" rather than guessing idle: a
        false idle hides an agent that's actually stuck, which is worse than admitting
        the classifier doesn't know.

        Blank rows are dropped *before* taking the tail, not after: a short dialog or
        prompt near the top of a tall pane is followed by dozens of blank padding rows
        below it, and slicing the raw tail first would land entirely on that padding
        (confirmed against a real Claude Code trust dialog).
        """
        non_blank = [line for line in viewport if line.strip()]
        text = "\n".join(non_blank[-_SCREEN_TAIL_ROWS:])
        patterns = self.screen_patterns
        if _matches_any(text, patterns.blocked):
            return "blocked"
        if _matches_any(text, patterns.working):
            return "working"
        if _matches_any(text, patterns.idle):
            return "idle"
        return "unknown"

    def prepare_workspace(self, cwd: Path, *, repository_root: Path | None = None) -> None:
        """Best-effort: pre-clear any first-run-in-this-directory prompt the CLI itself would show.

        Called once before every launch/resume, before cwd even has a process in it.
        A no-op by default; harnesses that gate on a per-directory prompt (e.g. Claude
        Code's workspace trust dialog) override this, since AgentFlow always runs a
        fresh git worktree the harness has never seen - unattended, that prompt would
        otherwise stall the run forever with no one there to answer it.
        """

    # prompt delivery ---------------------------------------------------------------
    @staticmethod
    def prompt_reference(prompt_file: Path) -> str:
        """The short line typed into the harness: prompts themselves live in files (Windows argv safety)."""
        return f"Read the file {prompt_file.as_posix()} and follow the instructions in it exactly. It is your task briefing."


def new_session_id() -> str:
    return str(uuid.uuid4())


def is_windows() -> bool:
    return sys.platform == "win32" or os.name == "nt"
