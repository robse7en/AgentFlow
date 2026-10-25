"""GitHub Copilot CLI adapter."""

from __future__ import annotations

from .base import HarnessAdapter, LaunchSpec, ScreenPatterns, SessionHandle, new_session_id


class CopilotAdapter(HarnessAdapter):
    type_name = "copilot"
    supports_resume = True
    # GitHub Copilot CLI's own action-approval prompts and selection dialogs (folder trust,
    # hook permission), its ask_user question box ("enter accept · ctrl+d decline"), its
    # "Working"/thinking indicator while a turn is in progress, and the "Loading: ..." status
    # while it starts. At rest (1.0.89) the footer lists "/ commands".
    screen_patterns = ScreenPatterns(
        blocked=(r"allow this action", r"do you want to allow", r"\(y/n\)", r"enter to select",
                 r"ctrl\+d decline"),
        working=(r"\bworking\b", r"\bthinking\b", r"\bloading:"),
        idle=(r"^\s*>\s*$", r"/ commands\b.*\? help"),
    )
    # Copilot writes a session's final credits at shutdown (scripts/usage_copilot.py).
    exit_command = "/exit"

    def model_flag(self) -> str | None:
        return self.config.model_argument or "--model"

    def reasoning_flag(self) -> str | None:
        return self.config.reasoning_argument or "--effort"

    def prompt_args(self, prompt: str) -> list[str]:
        # -i starts interactive mode and executes the prompt (-p would be non-interactive).
        return [self.config.prompt_argument or "-i", prompt]

    def build_launch(self, *, model, reasoning, initial_prompt) -> LaunchSpec:
        # This id also lets build_resume continue the same conversation later, and lets a
        # usage collector find this run's own session log.
        session_id = new_session_id()
        argv = [
            *self.base_argv(),
            "--session-id", session_id,
            *self.option_args(model, reasoning),
            *self.prompt_args(initial_prompt),
        ]
        return LaunchSpec(argv=argv, env=dict(self.config.env), harness_session_id=session_id)

    def build_resume(self, session: SessionHandle, *, model, reasoning, initial_prompt) -> LaunchSpec | None:
        # Copilot's own event log is cumulative per session id (see scripts/usage_copilot.py),
        # so resuming reuses the same id and the usage collector does delta accounting against
        # a cursor file rather than reporting the whole log as this run's own usage.
        if not session.harness_session_id:
            return None
        argv = [
            *self.base_argv(),
            "--resume", session.harness_session_id,
            *self.option_args(model, reasoning),
            *self.prompt_args(initial_prompt),
        ]
        return LaunchSpec(argv=argv, env=dict(self.config.env), harness_session_id=session.harness_session_id)
