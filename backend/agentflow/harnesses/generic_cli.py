"""Config-driven adapter for any CLI harness. No Python changes required."""

from __future__ import annotations

from .base import HarnessAdapter


class GenericCliAdapter(HarnessAdapter):
    type_name = "generic-cli"
