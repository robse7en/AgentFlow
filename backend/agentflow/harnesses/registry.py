"""Maps configured harnesses to adapter classes and caches detection results."""

from __future__ import annotations

from ..config import Config, HarnessConfig
from .base import HarnessAdapter, HarnessCapabilities
from .claude import ClaudeAdapter
from .codex import CodexAdapter
from .copilot import CopilotAdapter
from .generic_cli import GenericCliAdapter

ADAPTERS: dict[str, type[HarnessAdapter]] = {
    "claude": ClaudeAdapter,
    "codex": CodexAdapter,
    "copilot": CopilotAdapter,
    "generic-cli": GenericCliAdapter,
}


def make_adapter(key: str, config: HarnessConfig) -> HarnessAdapter:
    cls = ADAPTERS.get(config.type, GenericCliAdapter)
    return cls(key, config)


class HarnessRegistry:
    def __init__(self, config: Config) -> None:
        self.adapters: dict[str, HarnessAdapter] = {
            key: make_adapter(key, cfg) for key, cfg in config.harnesses.items()
        }
        self.capabilities: dict[str, HarnessCapabilities] = {}

    async def detect_all(self) -> dict[str, HarnessCapabilities]:
        self.capabilities = {key: await adapter.detect() for key, adapter in self.adapters.items()}
        return self.capabilities

    def get(self, key: str) -> HarnessAdapter:
        if key not in self.adapters:
            raise KeyError(f"Unknown harness '{key}'")
        return self.adapters[key]

    def is_usable(self, key: str) -> bool:
        caps = self.capabilities.get(key)
        return bool(caps and caps.enabled and caps.available)
