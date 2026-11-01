"""Claude Code adapter."""

from __future__ import annotations

import json
from pathlib import Path

from .base import HarnessAdapter, LaunchSpec, ScreenPatterns, SessionHandle, new_session_id


def _claude_config_path() -> Path:
    return Path.home() / ".claude.json"


def trust_workspace(cwd: Path) -> None:
    """Pre-accept Claude Code's one-time-per-directory workspace trust dialog for `cwd`.

    Claude Code asks "Do you trust the files in this folder?" the first time it's
    launched in a directory it hasn't seen before, tracked as `hasTrustDialogAccepted`
    per project path in ~/.claude.json. Every AgentFlow task runs in a freshly created
    git worktree, so without this the dialog fires on first launch and, with no one
    attached to answer it, the process just sits there - indistinguishable from a
    normal running session. This writes exactly what answering "yes" once would.

    ~/.claude.json is the same file the user's own interactive Claude Code sessions
    read and write, so this reads fresh, changes only this one project's flag, and
    writes back atomically (temp file + replace) to keep the read-modify-write window
    as small as possible. Best-effort: any failure here just means the dialog may
    still appear, not that the launch itself should be blocked.
    """
    path = _claude_config_path()
    if not path.is_file():
        return
    try:
        with path.open("r", encoding="utf-8") as f:
            data = json.load(f)
    except (OSError, json.JSONDecodeError):
        return
    if not isinstance(data, dict):
        return
    projects = data.setdefault("projects", {})
    if not isinstance(projects, dict):
        return
    key = cwd.as_posix()
    entry = projects.get(key)
    if isinstance(entry, dict) and entry.get("hasTrustDialogAccepted") is True:
        return
    if not isinstance(entry, dict):
        entry = {}
    entry["hasTrustDialogAccepted"] = True
    projects[key] = entry
    try:
        tmp = path.with_name(path.name + ".agentflow-tmp")
        with tmp.open("w", encoding="utf-8") as f:
            json.dump(data, f)
        tmp.replace(path)
    except OSError:
        pass


class ClaudeAdapter(HarnessAdapter):
    type_name = "claude"
    supports_resume = True
    # Claude Code's own permission/trust dialogs and its "esc to interrupt" spinner
    # while it's working. `prepare_workspace` below pre-clears the trust dialog for
    # trusted repos, but a first-run prompt in an untrusted one, or a Bash/edit
    # permission prompt mid-run, still needs this fallback to be seen at all.
    #
    # The blocked patterns below were tuned against a real captured dialog (a
    # workspace-trust prompt AgentFlow itself didn't pre-clear, on an unattended
    # scratch task):
    #   "Quick safety check: Is this a project you created or one you trust? ..."
    #   "❯ No, exit" / "  Yes, I trust this folder" / "Enter to confirm · Esc to cancel"
    # "❯" is Claude Code's own cursor for a selectable option in these dialogs; it's
    # generic across dialog wording, so it's kept even though the specific phrases above
    # are guesses for permission prompts other than this trust one.
    screen_patterns = ScreenPatterns(
        blocked=(r"do you want to proceed", r"quick safety check", r"trust this folder",
                 r"enter to confirm", r"❯"),
        working=(r"esc to interrupt", r"\bthinking\b", r"\bgenerating\b"),
        idle=(r"\?\s*for shortcuts", r"^\s*>\s*$"),
    )

    def prepare_workspace(self, cwd: Path, *, repository_root: Path | None = None) -> None:
        trusted_root = repository_root or cwd
        if trusted_root.resolve() in {Path(path).expanduser().resolve() for path in self.config.trusted_repositories}:
            trust_workspace(cwd)

    def model_flag(self) -> str | None:
        return self.config.model_argument or "--model"

    def reasoning_flag(self) -> str | None:
        return self.config.reasoning_argument or "--effort"

    def build_launch(self, *, model, reasoning, initial_prompt) -> LaunchSpec:
        session_id = new_session_id()
        argv = [
            *self.base_argv(),
            "--session-id", session_id,
            *self.option_args(model, reasoning),
            *self.prompt_args(initial_prompt),
        ]
        return LaunchSpec(argv=argv, env=dict(self.config.env), harness_session_id=session_id)

    def build_resume(self, session: SessionHandle, *, model, reasoning, initial_prompt) -> LaunchSpec | None:
        if not session.harness_session_id:
            return None
        argv = [
            *self.base_argv(),
            "--resume", session.harness_session_id,
            *self.option_args(model, reasoning),
            *self.prompt_args(initial_prompt),
        ]
        return LaunchSpec(argv=argv, env=dict(self.config.env), harness_session_id=session.harness_session_id)
