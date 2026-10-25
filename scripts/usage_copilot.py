#!/usr/bin/env python
"""Read the AI credits GitHub billed for a Copilot session.

Copilot records the session's cumulative credits in a usage checkpoint after each turn and again
at shutdown. Neither carries token counts, so this reports cost only. A turn's credits are written
after the turn ends, which is after the agent's own `flowctl complete`: until then the collector
exits with PENDING so AgentFlow collects again later instead of recording the previous snapshot.
"""

from __future__ import annotations

import json
import os
import re
import sys
from pathlib import Path

NANO_AIU_PER_USD = 100_000_000_000
PENDING = 75  # EX_TEMPFAIL: this run's usage is not written yet
SNAPSHOTS = ("session.usage_checkpoint", "session.shutdown")
# Events that happen inside a turn, so its credits are not in the latest snapshot yet.
ACTIVITY = ("user.", "assistant.", "model.", "tool.", "subagent.")


def find_events_file(session_id: str) -> Path | None:
    if not re.fullmatch(r"[A-Za-z0-9_-]{1,128}", session_id):
        return None
    path = Path.home() / ".copilot" / "session-state" / session_id / "events.jsonl"
    return path if path.is_file() else None


def _credits(value) -> int | None:
    return value if isinstance(value, int) and not isinstance(value, bool) and value >= 0 else None


def snapshot_credits(data: dict) -> int | None:
    """The snapshot's session total in nano-AIU, else the sum of its per-model totals."""
    total = _credits(data.get("totalNanoAiu"))
    if total is not None:
        return total
    per_model = [_credits((metrics or {}).get("totalNanoAiu")) for metrics in (data.get("modelMetrics") or {}).values()]
    return sum(per_model) if per_model and None not in per_model else None


def collect_cost(events_file: Path) -> dict | int | None:
    """The session's cumulative billed cost, PENDING while a turn is unaccounted, or None."""
    nano_aiu: int | None = None
    pending = False
    models: list[str] = []
    selected: str | None = None

    with events_file.open("r", encoding="utf-8") as f:
        for line in f:
            line = line.strip()
            if not line:
                continue
            try:
                entry = json.loads(line)
            except json.JSONDecodeError:
                continue
            kind = entry.get("type") or ""
            data = entry.get("data") or {}
            if kind in SNAPSHOTS:
                # A snapshot without credits (older Copilot releases) leaves the total unknown.
                nano_aiu = snapshot_credits(data)
                pending = False
            elif kind.startswith(ACTIVITY):
                pending = True
            if kind == "assistant.message" and isinstance(data.get("model"), str) and data["model"] not in models:
                models.append(data["model"])
            elif kind == "session.start" and isinstance(data.get("selectedModel"), str):
                selected = data["selectedModel"]
            elif kind == "session.model_change" and isinstance(data.get("newModel"), str):
                selected = data["newModel"]

    if pending:
        return PENDING
    if nano_aiu is None:
        return None
    return {
        "model": "+".join(sorted(models)) or selected or "copilot",
        # GitHub AI credits: 1 credit = 1e9 nano-AIU = US$0.01.
        "costUsd": nano_aiu / NANO_AIU_PER_USD,
        "costKind": "billed",
    }


def main() -> int:
    session_id = os.environ.get("AGENTFLOW_SESSION_ID")
    if not session_id:
        return 1
    events_file = find_events_file(session_id)
    if events_file is None:
        return 1
    report = collect_cost(events_file)
    if not isinstance(report, dict):
        return report or 1
    print(json.dumps({**report, "scope": "session", "source": "copilot-cli-events"}))
    return 0


if __name__ == "__main__":
    sys.exit(main())
