"""Optional chat integration (Slack DMs). Off unless `slack.enabled` is set in config.json."""

from __future__ import annotations

import logging
from pathlib import Path

from ... import diagnostics
from ...config import Config
from ...workflow.engine import WorkflowEngine
from .service import ChatService

log = logging.getLogger(__name__)


def start_chat(config: Config, engine: WorkflowEngine, config_file: Path | None = None) -> ChatService | None:
    """Start Slack notifications in the background, or explain why they are off.

    Never raises and never waits on Slack: the backend starts (and prints its pairing code)
    whether or not Slack is reachable or correctly configured."""
    settings = config.slack
    if not settings.enabled:
        # Logged so a Slack setup in a config file the backend didn't load isn't silently ignored.
        log.info("Slack is off: slack.enabled is not true in %s", config_file or "the loaded config")
        return None
    problems = settings.problems()
    if problems:
        diagnostics.event("chat.misconfigured", platform="slack", problems=len(problems))
        log.error("Slack is enabled but not started: %s", "; ".join(problems))
        return None
    try:
        from .slack import SlackAdapter
        adapter = SlackAdapter(settings.resolved_bot_token(), settings.resolved_app_token(), settings.command)
    except ImportError:
        log.error('Slack is enabled but its libraries are missing: pip install -e "backend[slack]"')
        return None
    service = ChatService(engine, adapter, settings)
    service.start()
    return service
