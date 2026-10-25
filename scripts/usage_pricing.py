"""List-price estimates for the bundled usage collectors.

Prices come from the public models.dev catalog: the copy the Copilot proxy
(copilot-api) already caches, else our own copy refreshed weekly. Estimates are
reported with "costKind": "estimated" so AgentFlow never adds them to billed
amounts or counts them against API budgets.
"""

from __future__ import annotations

import json
import os
import re
import time
import urllib.request
from pathlib import Path

CATALOG_URL = "https://models.dev/api.json"
MAX_CATALOG_AGE_SECONDS = 7 * 24 * 3600
DOWNLOAD_DEADLINE_SECONDS = 6
PREFERRED_PROVIDERS = ("anthropic", "openai", "google", "xai")


def copilot_api_home() -> Path:
    configured = os.environ.get("COPILOT_API_HOME")
    return Path(configured) if configured else Path.home() / ".local" / "share" / "copilot-api"


def _read_json(path: Path) -> dict | None:
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, ValueError):
        return None
    return value if isinstance(value, dict) else None


def _download(target: Path) -> dict | None:
    # A hard overall deadline: the collector gets 15 s in total, and a slow download must
    # cost only the estimate, never the token report.
    deadline = time.monotonic() + DOWNLOAD_DEADLINE_SECONDS
    try:
        data = bytearray()
        # models.dev rejects urllib's default User-Agent.
        request = urllib.request.Request(CATALOG_URL, headers={"User-Agent": "agentflow-usage-collector"})
        with urllib.request.urlopen(request, timeout=3) as response:
            while chunk := response.read(256 * 1024):
                data.extend(chunk)
                if time.monotonic() > deadline or len(data) > 64 * 1024 * 1024:
                    return None
        catalog = json.loads(data)
        if not isinstance(catalog, dict):
            return None
        target.parent.mkdir(parents=True, exist_ok=True)
        partial = target.with_suffix(".tmp")
        partial.write_bytes(data)
        partial.replace(target)
        return catalog
    except (OSError, ValueError):
        return None


def load_catalog() -> dict | None:
    catalog = _read_json(copilot_api_home() / "models-dev-api.json")
    if catalog:
        return catalog
    own = Path.home() / ".cache" / "agentflow" / "models-dev-api.json"
    try:
        fresh = time.time() - own.stat().st_mtime < MAX_CATALOG_AGE_SECONDS
    except OSError:
        fresh = False
    return (_read_json(own) if fresh else None) or _download(own) or _read_json(own)


def _candidates(model: str) -> list[str]:
    name = re.sub(r"\[[^\]]*\]$", "", model.strip().lower()).split("/")[-1]
    return list(dict.fromkeys([name, name.replace(".", "-")]))


def rates_for(catalog: dict | None, model: str) -> dict | None:
    """The first catalog entry with token prices, preferring the model's own vendor."""
    if not catalog or not model:
        return None
    providers = [p for p in PREFERRED_PROVIDERS if p in catalog]
    providers += [p for p in catalog if p not in PREFERRED_PROVIDERS]
    for name in _candidates(model):
        for provider in providers:
            models = (catalog.get(provider) or {}).get("models") or {}
            cost = (models.get(name) or {}).get("cost")
            if isinstance(cost, dict) and (cost.get("input") or cost.get("output")):
                return cost
    return None


def _tier(rates: dict, context_tokens: int | None) -> dict:
    selected, selected_size = rates, 0
    if context_tokens is None:
        return selected
    for tier in rates.get("tiers") or []:
        size = (tier.get("tier") or {}).get("size") or 0
        if (tier.get("tier") or {}).get("type") == "context" and selected_size < size < context_tokens:
            selected, selected_size = tier, size
    return selected


def price(rates: dict, *, uncached: int, cache_read: int = 0, cache_write: int = 0,
          cache_write_1h: int = 0, output: int = 0, per_request: bool = False) -> float:
    """USD at per-million list prices. Only a single request's context selects a long-context tier."""
    context = uncached + cache_read + cache_write + cache_write_1h if per_request else None
    rate = _tier(rates, context)
    input_rate = float(rate.get("input") or 0)
    total = (max(uncached, 0) * input_rate
             + cache_read * float(rate.get("cache_read", input_rate) or 0)
             + cache_write * float(rate.get("cache_write", input_rate) or 0)
             # Anthropic bills one-hour cache writes at twice the input price.
             + cache_write_1h * input_rate * 2
             + output * float(rate.get("output") or 0))
    return total / 1_000_000
