#!/usr/bin/env python
"""Read exact Codex session totals; AgentFlow owns immutable per-run baselines.

Codex reports tokens but no price (a ChatGPT login meters by quota), so cost is a
list-price estimate.
"""

from __future__ import annotations

import itertools
import json
import os
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import usage_pricing  # noqa: E402


MAX_DAY_DIRECTORIES = 30  # bound the backward scan; a just-finished run is always recent


def _subdirs_desc(directory: Path) -> list[Path]:
    if not directory.is_dir():
        return []
    return sorted((p for p in directory.iterdir() if p.is_dir()), key=lambda p: p.name, reverse=True)


def _day_directories_desc(sessions_root: Path):
    for year_dir in _subdirs_desc(sessions_root):
        for month_dir in _subdirs_desc(year_dir):
            yield from _subdirs_desc(month_dir)


def _matches(path: Path, session_id: str) -> bool:
    try:
        with path.open("r", encoding="utf-8") as f:
            first_line = f.readline().strip()
    except OSError:
        return False
    if not first_line:
        return False
    try:
        entry = json.loads(first_line)
    except json.JSONDecodeError:
        return False
    if entry.get("type") != "session_meta":
        return False
    payload = entry.get("payload") or {}
    # Only top-level CLI sessions - excludes Codex's own internal guardian/subagent
    # review sessions (source is a dict like {"subagent": {...}}) and other front ends
    # (vscode, exec), which can log under the same cwd.
    if payload.get("source") != "cli":
        return False
    return (payload.get("session_id") or payload.get("id")) == session_id


def find_session_file(session_id: str) -> Path | None:
    sessions_root = Path.home() / ".codex" / "sessions"
    if not session_id:
        return None

    for day_dir in itertools.islice(_day_directories_desc(sessions_root), MAX_DAY_DIRECTORIES):
        files = sorted(day_dir.glob("*.jsonl"), key=lambda p: p.stat().st_mtime, reverse=True)
        for path in files:
            if _matches(path, session_id):
                return path
    return None


def collect(session_file: Path) -> dict | None:
    model: str | None = None
    last_usage: dict | None = None

    with session_file.open("r", encoding="utf-8") as f:
        for line in f:
            line = line.strip()
            if not line:
                continue
            try:
                entry = json.loads(line)
            except json.JSONDecodeError:
                continue

            if entry.get("type") == "turn_context":
                candidate = (entry.get("payload") or {}).get("model")
                if candidate:
                    model = candidate
                continue

            if entry.get("type") != "event_msg":
                continue
            payload = entry.get("payload") or {}
            payload_type = payload.get("type")
            if payload_type == "thread_settings_applied":
                candidate = (payload.get("thread_settings") or {}).get("model")
                if candidate:
                    model = candidate
            elif payload_type == "token_count":
                usage = (payload.get("info") or {}).get("total_token_usage")
                if usage:
                    last_usage = usage

    if last_usage is None or model is None:
        return None

    report = {
        "inputTokens": int(last_usage.get("input_tokens") or 0),
        # reasoning_output_tokens is a subset breakdown of output_tokens, not additional -
        # input_tokens + output_tokens already equals total_tokens in these logs.
        "outputTokens": int(last_usage.get("output_tokens") or 0),
        "cachedInputTokens": int(last_usage.get("cached_input_tokens") or 0),
        "model": model,
        "source": "codex-cli-rollout",
    }
    rates = usage_pricing.rates_for(usage_pricing.load_catalog(), model)
    if rates is not None:
        # input_tokens includes cached and cache-write tokens. Totals span many requests, so
        # the base (not long-context) rate applies; Codex's context window sits below that tier.
        cache_write = int(last_usage.get("cache_write_input_tokens") or 0)
        report.update(costKind="estimated", costUsd=usage_pricing.price(
            rates, uncached=report["inputTokens"] - report["cachedInputTokens"] - cache_write,
            cache_read=report["cachedInputTokens"], cache_write=cache_write,
            output=report["outputTokens"]))
    return report


def main() -> int:
    session_file = find_session_file(os.environ.get("AGENTFLOW_SESSION_ID", ""))
    if session_file is None:
        return 1
    totals = collect(session_file)
    if totals is None:
        return 1
    print(json.dumps({**totals, "scope": "session"}))
    return 0


if __name__ == "__main__":
    sys.exit(main())