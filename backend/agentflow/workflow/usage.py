"""Usage collection from an explicitly configured provider-report command."""

import asyncio
import json
import sys
from pathlib import Path
from typing import Literal

from pydantic import BaseModel, ConfigDict, Field, model_validator

from ..services.process import clean_env, process_group_options, terminate_process_tree

SCRIPTS_DIR = Path(__file__).resolve().parents[3] / "scripts"
BUNDLED_COLLECTORS = {"claude": "usage_claude.py", "codex": "usage_codex.py", "copilot": "usage_copilot.py"}


def resolve_usage_command(argv: list[str]) -> list[str]:
    """A leading `python` runs the backend's own interpreter, and a relative argument naming a file
    in the AgentFlow install resolves to it, so the example config's commands work anywhere."""
    root = SCRIPTS_DIR.parent
    head = sys.executable if argv[0] == "python" else argv[0]
    rest = [(root / arg).as_posix() if not Path(arg).is_absolute() and (root / arg).is_file() else arg
            for arg in argv[1:]]
    return [head, *rest]


def bundled_usage_command(harness_type: str) -> list[str] | None:
    """The bundled usage collector for a harness type, run by this Python; None if there is none."""
    script = BUNDLED_COLLECTORS.get(harness_type)
    return [Path(sys.executable).as_posix(), (SCRIPTS_DIR / script).as_posix()] if script else None


class UsageReport(BaseModel):
    model_config = ConfigDict(extra="forbid", populate_by_name=True)
    # None when the provider reports cost only (Copilot's usage checkpoints carry no tokens).
    input_tokens: int | None = Field(None, alias="inputTokens", ge=0, strict=True)
    output_tokens: int | None = Field(None, alias="outputTokens", ge=0, strict=True)
    cached_input_tokens: int | None = Field(None, alias="cachedInputTokens", ge=0, strict=True)
    model: str = Field(min_length=1, max_length=128)
    source: str = Field(min_length=1, max_length=200)
    cost: float | None = Field(None, alias="costUsd", ge=0, allow_inf_nan=False)
    # "billed": the provider metered this amount (e.g. Copilot AI credits). "estimated": tokens
    # priced at public list rates. Reports predating this field carry provider-reported costs.
    cost_kind: Literal["billed", "estimated"] | None = Field(None, alias="costKind")
    scope: Literal["run", "session"] = "run"

    @model_validator(mode="after")
    def counts(self):
        if (self.input_tokens is None) != (self.output_tokens is None):
            raise ValueError("Report both inputTokens and outputTokens, or neither")
        if self.input_tokens is None:
            if self.cached_input_tokens is not None:
                raise ValueError("cachedInputTokens requires inputTokens")
            if self.cost is None:
                raise ValueError("A usage report needs token counts or a cost")
        elif self.cached_input_tokens is None:
            self.cached_input_tokens = 0
        elif self.cached_input_tokens > self.input_tokens:
            raise ValueError("Cached input tokens must be included in inputTokens")
        if self.cost is None:
            self.cost_kind = None
        elif self.cost_kind is None:
            self.cost_kind = "billed"
        return self

    def record(self, provenance: str) -> dict:
        total = None if self.input_tokens is None else self.input_tokens + self.output_tokens
        return {**self.model_dump(by_alias=True), "totalTokens": total, "provenance": provenance}


class UsagePending(ValueError):
    """The provider has not written all of this run's usage yet; collecting again later can
    succeed. `report` is the usage so far when the collector printed one (Claude mid-turn)."""

    def __init__(self, message: str, report: "UsageReport | None" = None) -> None:
        super().__init__(message)
        self.report = report


PENDING_EXIT = 75  # EX_TEMPFAIL, as returned by scripts/usage_copilot.py


def cost_kind(record: dict | None) -> str | None:
    """Stored records predating costKind carry provider-reported costs."""
    if not record or record.get("costUsd") is None:
        return None
    return record.get("costKind") or "billed"


async def collect_usage(argv: list[str], cwd: Path, run_id: int, session_id: str | None) -> UsageReport:
    """Legacy collectors report runs; bundled collectors explicitly report session totals."""
    process = await asyncio.create_subprocess_exec(
        *resolve_usage_command(argv), cwd=str(cwd), env=clean_env({"AGENTFLOW_RUN": str(run_id),
            "AGENTFLOW_SESSION_ID": session_id or ""}),
        stdin=asyncio.subprocess.DEVNULL, stdout=asyncio.subprocess.PIPE,
        stderr=asyncio.subprocess.DEVNULL,
        **process_group_options(),
    )

    async def read_report():
        data = bytearray()
        while chunk := await process.stdout.read(4096):
            data.extend(chunk)
            if len(data) > 16384:
                raise ValueError("Usage report exceeds 16 KB")
        code = await process.wait()
        if code == PENDING_EXIT:
            raise UsagePending("Usage is not all written yet",
                               UsageReport.model_validate(json.loads(data)) if data.strip() else None)
        if code != 0:
            raise ValueError("Usage collector failed")
        return UsageReport.model_validate(json.loads(data))

    try:
        return await asyncio.wait_for(read_report(), timeout=15)
    finally:
        if process.returncode is None:
            await terminate_process_tree(process)


def run_usage(report: UsageReport, baseline: dict | None) -> UsageReport:
    """Subtract an immutable launch baseline, never a mutable last-collection cursor."""
    if report.scope == "run":
        return report
    if baseline is None:
        raise ValueError("Session usage has no verified launch baseline")
    data = report.model_dump(by_alias=True)
    token_keys = ("inputTokens", "outputTokens", "cachedInputTokens")
    if report.input_tokens is not None and any(baseline.get(key, 0) is None for key in token_keys):
        # A cost-only baseline cannot be subtracted from token counters.
        data.update(dict.fromkeys(token_keys))
    elif report.input_tokens is not None:
        for key in token_keys:
            data[key] -= baseline.get(key, 0)
            if data[key] < 0:
                raise ValueError("Session usage counter decreased")
        data["cachedInputTokens"] = min(data["cachedInputTokens"], data["inputTokens"])
    previous_cost = baseline.get("costUsd", 0 if not baseline else None)
    if baseline and baseline.get("costKind") != report.cost_kind:
        previous_cost = None  # billed and estimated counters cannot be subtracted from each other
    data["costUsd"] = (report.cost - previous_cost
                       if report.cost is not None and previous_cost is not None else None)
    if data["costUsd"] is not None and data["costUsd"] < 0:
        raise ValueError("Session cost counter decreased")
    data["scope"] = "run"
    return UsageReport.model_validate(data)


async def prepare_baseline(argv: list[str] | None, cwd: Path, run_id: int,
                           session_id: str | None, resumed: bool) -> dict | None:
    """Failure to measure a resumed session leaves usage unknown without blocking work."""
    if not resumed:
        return {}
    if not argv or not session_id:
        return None
    try:
        report = await collect_usage(argv, cwd, run_id, session_id)
    except UsagePending as exc:
        # The previous process is gone, so what it wrote is final even if its turn was cut off.
        report = exc.report
    except (OSError, ValueError, asyncio.TimeoutError):
        return None
    return report.model_dump(by_alias=True) if report is not None else None
