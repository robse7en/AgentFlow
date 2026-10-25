"""Configuration, system status and filesystem browsing endpoints."""

from __future__ import annotations

import contextlib
import json
import string
import sys
from pathlib import Path
from types import SimpleNamespace

from fastapi import APIRouter, Depends, HTTPException, Query
from pydantic import BaseModel, Field, ValidationError

from ..config import Config, ConfigError, CostControls, jira_startup_problem, load_config
from .deps import AppState, get_state
from ..workflow.budgets import budget_status

router = APIRouter(prefix="/api", tags=["config"])


def system_info(state: AppState) -> dict:
    return {
        "configPath": str(state.config_file),
        "dataDirectory": str(state.config.data_dir),
        "databasePath": str(state.config.database_file),
        "worktreeDirectory": str(state.config.worktree_dir),
        "taskDirectory": str(state.config.task_dir),
        "defaultRepositoryDirectory": state.config.workspace.default_repository_directory,
        "jiraEnabled": bool(state.config.jira.enabled and state.config.jira.base_url and state.config.jira.api_token),
        "jiraBaseUrl": state.config.jira.base_url,
        "github": state.config.github.model_dump(by_alias=True),
        "platform": sys.platform,
        "python": sys.version.split()[0],
        "system": state.system,
        "zellij": state.config.zellij.model_dump(by_alias=True),
        "workflow": state.config.workflow.model_dump(by_alias=True),
        "agents": {**state.config.agents.model_dump(by_alias=True),
                   "orchestrator": state.config.agents.for_role("orchestrator").model_dump(by_alias=True)},
        "harnesses": [
            {
                "key": caps.key, "displayName": caps.display_name, "type": caps.type, "enabled": caps.enabled,
                "available": caps.available, "command": caps.command, "resolvedPath": caps.resolved_path,
                "version": caps.version, "message": caps.message, "supportsModel": caps.supports_model,
                "supportsReasoning": caps.supports_reasoning, "supportsResume": caps.supports_resume,
                "defaultModel": (state.config.harnesses.get(caps.key).default_model if state.config.harnesses.get(caps.key) else None),
                "automaticTrustWorkspaces": state.config.harnesses[caps.key].trusted_repositories,
                "billingMode": state.config.harnesses[caps.key].billing_mode,
                "inheritedEnvironment": "allowlist; additional credentials require explicit harness env configuration",
                "availableModels": (state.config.harnesses.get(caps.key).available_models if state.config.harnesses.get(caps.key) else []),
            }
            for caps in state.harnesses.capabilities.values()
        ],
    }


@router.get("/config")
def get_config(state: AppState = Depends(get_state)):
    return system_info(state)


@router.get("/costs")
def cost_overview(state: AppState = Depends(get_state)):
    with state.db.session() as session:
        status = budget_status(session, SimpleNamespace(id=None, api_budget_usd=None),
                               state.config.workflow.cost_controls)
        return {"daily": status["daily"], "dayUtc": status["dayUtc"]}


@router.post("/config/reload")
async def reload_config(state: AppState = Depends(get_state)):
    from ..main import build_state  # local import to avoid a cycle

    # Held from reading the file to the swap, so a save cannot land in between and be undone.
    async with state.config_lock:
        try:
            new_config = load_config(state.config_file)
        except ConfigError as exc:
            raise HTTPException(status_code=400, detail=str(exc))
        if problem := jira_startup_problem(new_config):
            raise HTTPException(status_code=400, detail=problem)
        if (new_config.database_file != state.config.database_file
                or new_config.data_dir != state.config.data_dir
                or new_config.server != state.config.server):
            raise HTTPException(409, "Restart AgentFlow to change database, data directory, host or port")
        if new_config.zellij != state.config.zellij:
            raise HTTPException(409, "Restart AgentFlow to change Zellij configuration")
        if new_config.slack != state.config.slack:
            raise HTTPException(409, "Restart AgentFlow to change Slack configuration")
        new_state = await build_state(new_config, state.config_file, state.broadcaster, existing_db=state.db)
        # Harness detection above runs outside the launch lock; a launch sees the old or the
        # new configuration, never a mix.
        async with state.engine._launch_lock:
            # swap in place so routers keep working
            state.config = new_state.config
            state.git = new_state.git
            state.zellij = new_state.zellij
            state.harnesses = new_state.harnesses
            # Retain the engine and its locks: requests may still be finishing on it.
            state.engine.config = new_state.config
            state.engine.git = new_state.git
            state.engine.harnesses = new_state.harnesses
            state.engine.zellij = new_state.zellij
            state.system = new_state.system
        return system_info(state)


def _read_raw(state: AppState) -> dict:
    return json.loads(state.config_file.read_text(encoding="utf-8"))


def _save_raw(state: AppState, raw: dict) -> None:
    """Replace config_file with `raw` through a temporary file beside it, so the file is always
    the old or the new configuration. Callers hold state.config_lock and change live state only
    after this returns. The lock is per process: another process editing the file (such as
    `agentflow setup`) is not serialized with these saves."""
    temporary = state.config_file.with_name(state.config_file.name + ".tmp")
    try:
        temporary.write_text(json.dumps(raw, indent=2) + "\n", encoding="utf-8")
        temporary.replace(state.config_file)
    except OSError as exc:
        with contextlib.suppress(OSError):
            temporary.unlink(missing_ok=True)
        raise HTTPException(500, f"Could not save {state.config_file.name}: {exc.strerror or exc}") from exc


class SetDefaultRepositoryRequest(BaseModel):
    path: str | None = None


@router.put("/config/default-repository")
async def set_default_repository(payload: SetDefaultRepositoryRequest, state: AppState = Depends(get_state)):
    """Persist the repository directory the New Task form should prefill."""
    path = payload.path.strip() if payload.path else None
    async with state.config_lock:
        raw = _read_raw(state)
        raw.setdefault("workspace", {})["defaultRepositoryDirectory"] = path
        try:
            Config.model_validate(raw)
        except ValidationError as exc:
            raise HTTPException(status_code=400, detail=str(exc))
        _save_raw(state, raw)
        state.config.workspace.default_repository_directory = path
        return system_info(state)


class SetExchangeRateRequest(BaseModel):
    usd_to_nzd_rate: float = Field(alias="usdToNzdRate", gt=0)


@router.put("/config/exchange-rate")
async def set_exchange_rate(payload: SetExchangeRateRequest, state: AppState = Depends(get_state)):
    """Persist the fixed USD→NZD rate used to display run costs (not a live FX lookup)."""
    async with state.config_lock:
        raw = _read_raw(state)
        raw.setdefault("workflow", {})["usdToNzdRate"] = payload.usd_to_nzd_rate
        try:
            Config.model_validate(raw)
        except ValidationError as exc:
            raise HTTPException(status_code=400, detail=str(exc))
        _save_raw(state, raw)
        state.config.workflow.usd_to_nzd_rate = payload.usd_to_nzd_rate
        return system_info(state)


# --------------------------------------------------------------------------------------
# filesystem browsing for the directory pickers
# --------------------------------------------------------------------------------------
class CostSettingsRequest(BaseModel):
    controls: CostControls
    billing: dict[str, str]


@router.put("/config/cost-controls")
async def set_cost_controls(payload: CostSettingsRequest, state: AppState = Depends(get_state)):
    """Human-only configuration; serialized with launches so policy changes are atomic."""
    async with state.config_lock, state.engine._launch_lock:
        raw = _read_raw(state)
        raw.setdefault("workflow", {})["costControls"] = payload.controls.model_dump(by_alias=True)
        if payload.billing and "harnesses" not in raw:
            # The file relies on the default harnesses. Write them all, not just the one being
            # changed: a harnesses section replaces the defaults, and the agents still name them.
            raw["harnesses"] = {key: harness.model_dump(by_alias=True) for key, harness in state.config.harnesses.items()}
        for key, mode in payload.billing.items():
            if key not in state.config.harnesses:
                raise HTTPException(422, "Unknown harness")
            raw.setdefault("harnesses", {}).setdefault(key, state.config.harnesses[key].model_dump(by_alias=True))["billingMode"] = mode
        try:
            updated = Config.model_validate(raw)
        except ValidationError:
            raise HTTPException(422, "Invalid cost controls or billing mode")
        _save_raw(state, raw)
        state.config.workflow.cost_controls = updated.workflow.cost_controls
        for key in payload.billing:
            state.config.harnesses[key].billing_mode = updated.harnesses[key].billing_mode
        return system_info(state)


def _roots() -> list[str]:
    if sys.platform == "win32":
        return [f"{letter}:\\" for letter in string.ascii_uppercase if Path(f"{letter}:\\").exists()]
    return ["/"]


def _children(target: Path) -> list[dict]:
    entries = []
    try:
        for child in sorted(target.iterdir(), key=lambda p: p.name.lower()):
            try:
                if child.is_dir() and not child.name.startswith(".") and child.name.lower() != "node_modules":
                    entries.append({"name": child.name, "path": str(child)})
            except OSError:
                continue
    except PermissionError:
        raise HTTPException(status_code=403, detail=f"Permission denied: {target}")
    return entries


@router.get("/fs/list")
def list_directory(
    path: str | None = Query(None),
    root: str | None = Query(None, description="Constrain browsing to this directory (project picker)"),
):
    """List sub-directories of `path`. When `root` is given the result never leaves it."""
    if root:
        root_path = Path(root).expanduser().resolve()
        if not root_path.is_dir():
            raise HTTPException(status_code=400, detail=f"Root does not exist: {root}")
        target = (root_path / path).resolve() if path else root_path
        if target != root_path and root_path not in target.parents:
            target = root_path
        if not target.is_dir():
            target = root_path
        relative = target.relative_to(root_path).as_posix() if target != root_path else ""
        return {
            "path": str(target), "relative": relative, "root": str(root_path),
            "parent": str(target.parent) if target != root_path else None,
            "entries": _children(target),
        }
    if not path:
        return {"path": None, "parent": None, "roots": _roots(), "home": str(Path.home()),
                "entries": [{"name": r, "path": r} for r in _roots()]}
    target = Path(path).expanduser()
    if not target.is_dir():
        raise HTTPException(status_code=400, detail=f"Directory does not exist: {path}")
    target = target.resolve()
    parent = str(target.parent) if target.parent != target else None
    return {"path": str(target), "parent": parent, "roots": _roots(), "home": str(Path.home()),
            "entries": _children(target), "isGitRepo": (target / ".git").exists()}
