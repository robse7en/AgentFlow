#!/usr/bin/env python
"""Usage collector for the `claude` harness (see docs/security-and-efficiency.md).

Configure as harnesses.claude.usageCommand, e.g.:
    ["python", "D:/Dev/AgentFlow/scripts/usage_claude.py"]

Reads the exact session transcript. Totals cover the entire conversation; AgentFlow
subtracts the baseline captured before each run. While the latest turn is still inside a tool
call (the agent's own `flowctl complete` is one), the report so far is printed with exit code
PENDING, so AgentFlow collects again once the turn has ended. When Claude Code runs through the
Copilot proxy (copilot-api), the proxy's own ledger supplies the cost GitHub billed
in AI credits; otherwise the transcript is priced at list rates as an estimate.
"""

from __future__ import annotations

import json
import os
import re
import sqlite3
import sys
from glob import glob
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import usage_pricing  # noqa: E402

PENDING = 75  # EX_TEMPFAIL: the latest turn has not ended yet
# An assistant message ending in these continues the turn after the tool runs.
OPEN_TURN_STOP_REASONS = ("tool_use", "pause_turn")


def find_transcript(session_id: str) -> Path | None:
    if not re.fullmatch(r"[A-Za-z0-9_-]{1,128}", session_id):
        return None
    matches = glob(str(Path.home() / ".claude" / "projects" / "*" / f"{session_id}.jsonl"))
    return Path(matches[0]) if matches else None


def proxy_billed_cost(session_id: str) -> float | None:
    """Session total from copilot-api's ledger; None unless every request carries a USD cost."""
    database = usage_pricing.copilot_api_home() / "copilot-api.sqlite"
    if not database.is_file():
        return None
    try:
        connection = sqlite3.connect(f"{database.as_uri()}?mode=ro", uri=True, timeout=5)
        try:
            requests, costed, nanos = connection.execute(
                "SELECT COUNT(*), COUNT(total_cost_nanos), COALESCE(SUM(total_cost_nanos), 0) "
                "FROM token_usage_events WHERE session_id = ? AND cost_currency IS 'USD'",
                (session_id,)).fetchone()
            other = connection.execute(
                "SELECT COUNT(*) FROM token_usage_events WHERE session_id = ? "
                "AND cost_currency IS NOT 'USD'", (session_id,)).fetchone()[0]
        finally:
            connection.close()
    except sqlite3.Error:
        return None
    if not requests or costed < requests or other:
        return None
    return nanos / 1_000_000_000


def collect(transcript: Path) -> tuple[dict | None, bool]:
    """The session totals so far, and whether its latest turn is still open."""
    seen_message_ids: set[str] = set()
    input_tokens = output_tokens = cache_creation = cache_read = 0
    model: str | None = None
    turns = 0
    turn_open = False
    catalog = usage_pricing.load_catalog()
    estimate: float | None = 0.0

    with transcript.open("r", encoding="utf-8") as f:
        for line in f:
            line = line.strip()
            if not line:
                continue
            try:
                entry = json.loads(line)
            except json.JSONDecodeError:
                continue
            if entry.get("type") != "assistant":
                continue
            message = entry.get("message") or {}
            turn_open = message.get("stop_reason") in OPEN_TURN_STOP_REASONS
            usage = message.get("usage")
            entry_model = message.get("model")
            if not usage or not entry_model or entry_model == "<synthetic>":
                continue
            # A single API turn can appear as several JSONL lines (one per content block),
            # all sharing the same message id and the same usage totals for that turn.
            message_id = message.get("id")
            if message_id and message_id in seen_message_ids:
                continue
            if message_id:
                seen_message_ids.add(message_id)

            input_tokens += int(usage.get("input_tokens") or 0)
            output_tokens += int(usage.get("output_tokens") or 0)
            cache_creation += int(usage.get("cache_creation_input_tokens") or 0)
            cache_read += int(usage.get("cache_read_input_tokens") or 0)
            model = entry_model
            turns += 1
            rates = usage_pricing.rates_for(catalog, entry_model)
            if rates is None:
                estimate = None
            elif estimate is not None:
                one_hour = int((usage.get("cache_creation") or {}).get("ephemeral_1h_input_tokens") or 0)
                written = int(usage.get("cache_creation_input_tokens") or 0)
                estimate += usage_pricing.price(
                    rates, uncached=int(usage.get("input_tokens") or 0),
                    cache_read=int(usage.get("cache_read_input_tokens") or 0),
                    cache_write=max(written - one_hour, 0), cache_write_1h=min(one_hour, written),
                    output=int(usage.get("output_tokens") or 0), per_request=True)

    if turns == 0 or model is None:
        return None, False

    report = {
        "inputTokens": input_tokens + cache_creation + cache_read,
        "outputTokens": output_tokens,
        "cachedInputTokens": cache_read,
        "model": model,
        "source": "claude-code-transcript",
        "scope": "session",
    }
    if estimate is not None:
        report.update(costUsd=estimate, costKind="estimated")
    return report, turn_open


def main() -> int:
    session_id = os.environ.get("AGENTFLOW_SESSION_ID")
    if not session_id:
        return 1
    transcript = find_transcript(session_id)
    if transcript is None:
        return 1
    report, turn_open = collect(transcript)
    if report is None:
        return 1
    billed = proxy_billed_cost(session_id)
    if billed is not None:
        report.update(costUsd=billed, costKind="billed", source="claude-code-transcript+copilot-api")
    print(json.dumps(report))
    return PENDING if turn_open else 0


if __name__ == "__main__":
    sys.exit(main())
