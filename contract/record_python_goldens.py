"""Record what the Python backend does with configuration and the database, for the Rust port's
`cargo test` to reproduce (app/crates/core/tests/), and generate the Rust schema from the models
(app/crates/core/src/db/schema.rs).

Unlike the contract tests, this script imports `agentflow`: it is a recorder, not a test. Run it from
the repository root with the backend venv, then review the diff of contract/golden/{config,db}:

    backend/.venv/Scripts/python contract/record_python_goldens.py [kept-contract-run.db]

The optional database is a kept contract run (CONTRACT_KEEP=1), dumped as the populated fixture.
"""

from __future__ import annotations

import json
import sqlite3
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "backend"))

from agentflow.config import Config, ConfigError, fresh_config, read_config  # noqa: E402
from agentflow.database.database import Database  # noqa: E402

GOLDEN = ROOT / "contract" / "golden"

# (name, input) pairs. Each records Python's dump of the validated config, or that it is rejected.
CONFIG_CASES: list[tuple[str, object]] = [
    ("empty", {}),
    ("example", json.loads((ROOT / "config.example.json").read_text(encoding="utf-8"))),
    ("partial-agents", {"agents": {"architect": {"harness": "codex", "model": "gpt-5.5"}}}),
    ("null-reasoning", {"agents": {"implementer": {"harness": "codex", "reasoning": None}}}),
    ("orchestrator", {"agents": {"orchestrator": {"harness": "copilot", "reasoning": "low"}}}),
    ("snake-case-names", {"server": {"run_as_administrator": True},
                          "workflow": {"max_concurrent_agents": 8, "cost_controls": {"warning_percent": 50}}}),
    ("harness-display-name-filled", {"harnesses": {
        "claude": {"command": "claude"}, "codex": {"command": "codex", "displayName": "Codex"},
        "stub": {"command": "python", "args": ["stub.py"], "env": {"STUB_X": "1"}}}}),
    ("full-workflow", {"workflow": {
        "implementationMode": "parallel", "maxConcurrentAgents": 32, "usdToNzdRate": 1.7,
        "verificationCommands": [["pytest", "-q"], ["npm", "test"]], "maxReviewIterations": 50,
        "defaultExecutionMode": "approval_required", "commitCheckpoints": False,
        "costControls": {"taskBudgetUsd": 2.5, "dailyBudgetUsd": 10, "copilotRoles": ["implementer"],
                         "requireMeteredApproval": True}}}),
    ("slack-and-jira", {
        "slack": {"enabled": True, "userId": "U0123ABC", "command": "/agentflow-rob", "digestAt": "09:30",
                  "details": ["findings", "cost"], "notify": ["FAILED"], "allowActions": ["answer", "run"],
                  "ask": {"enabled": True, "harness": "copilot", "timeoutSeconds": 30}},
        "jira": {"enabled": True, "baseUrl": "https://example.atlassian.net", "email": "a@b.c", "apiToken": "t",
                 "autoTransitionStatus": "In Progress", "maxFramesPerVideo": 0},
        "github": {"prSync": False, "pollIntervalSeconds": 600, "hosts": ["github.com", "ghe.local"]},
        "zellij": {"shell": "pwsh", "headlessCols": 120}}),
    ("slack-without-command", {"slack": {"enabled": True, "userId": "U0123ABC", "command": None}}),
    ("unicode", {"workspace": {"branchPrefix": "agënt/", "defaultRepositoryDirectory": "D:/Dev/プロジェクト"}}),
    ("worktree-setup", {"workspace": {"worktreeSetup": {
        "D:/Dev/repo": {"copy": [".env", "apps/web/.env.local", "./tools\\local.json"],
                        "commands": [["npm", "ci"], ["dotnet", "restore"]], "timeoutSeconds": 600},
        "~/other": {}}}}),
    # Rejected by Python.
    ("reject-unknown-top-level", {"nope": 1}),
    ("reject-unknown-nested", {"server": {"port": 1, "colour": "red"}}),
    ("reject-port-type", {"server": {"port": "8765x"}}),
    ("reject-host", {"server": {"host": "0.0.0.0"}}),
    ("reject-range", {"workflow": {"maxConcurrentAgents": 0}}),
    ("reject-range-high", {"workflow": {"maxReviewIterations": 51}}),
    ("reject-budget-zero", {"workflow": {"costControls": {"taskBudgetUsd": 0}}}),
    ("reject-empty-argv", {"workflow": {"verificationCommands": [[]]}}),
    ("reject-blank-argv-part", {"workflow": {"verificationCommands": [["pytest", " "]]}}),
    ("reject-literal", {"workflow": {"implementationMode": "serial"}}),
    ("reject-reasoning", {"agents": {"architect": {"harness": "claude", "reasoning": "max"}}}),
    ("reject-slack-user", {"slack": {"userId": "rob"}}),
    ("reject-slack-command", {"slack": {"command": "/Has Spaces"}}),
    ("reject-slack-digest", {"slack": {"digestAt": "24:00"}}),
    ("reject-slack-ui-url", {"slack": {"uiBaseUrl": "ftp://x"}}),
    ("reject-env-name", {"harnesses": {"claude": {"command": "claude", "env": {"BAD-NAME": "1"}},
                                       "codex": {"command": "codex"}}}),
    ("reject-harness-without-command", {"harnesses": {"claude": {"type": "claude"}}}),
    ("reject-unknown-harness-reference", {"harnesses": {"stub": {"command": "x"}}}),
    ("reject-orchestrator-reference", {"agents": {"orchestrator": {"harness": "nope"}}}),
    ("reject-not-an-object", []),
    ("reject-setup-absolute-copy", {"workspace": {"worktreeSetup": {"D:/r": {"copy": ["C:/secrets/.env"]}}}}),
    ("reject-setup-rooted-copy", {"workspace": {"worktreeSetup": {"D:/r": {"copy": ["/etc/passwd"]}}}}),
    ("reject-setup-parent-copy", {"workspace": {"worktreeSetup": {"D:/r": {"copy": ["a/../../other/.env"]}}}}),
    ("reject-setup-git-copy", {"workspace": {"worktreeSetup": {"D:/r": {"copy": [".git/config"]}}}}),
    ("reject-setup-blank-copy", {"workspace": {"worktreeSetup": {"D:/r": {"copy": [" "]}}}}),
    ("reject-setup-empty-argv", {"workspace": {"worktreeSetup": {"D:/r": {"commands": [[]]}}}}),
    ("reject-setup-timeout", {"workspace": {"worktreeSetup": {"D:/r": {"timeoutSeconds": 0}}}}),
    ("reject-setup-unknown-field", {"workspace": {"worktreeSetup": {"D:/r": {"hooks": []}}}}),
    ("reject-setup-blank-root", {"workspace": {"worktreeSetup": {" ": {}}}}),
]


def record_config() -> None:
    cases = []
    with tempfile.TemporaryDirectory() as tmp:
        for name, raw in CONFIG_CASES:
            path = Path(tmp) / f"{name}.json"
            path.write_text(json.dumps(raw), encoding="utf-8")
            try:
                dump = read_config(path).model_dump(by_alias=True)
            except (ConfigError, AttributeError, TypeError) as exc:
                message = str(exc).replace(str(path), "<config>").splitlines()
                # Pydantic's third line names the field; keep it as a hint for the Rust error.
                cases.append({"name": name, "input": raw, "rejected": True,
                              "pythonError": " | ".join(line.strip() for line in message[:3])})
                continue
            assert not name.startswith("reject-"), f"{name} was accepted by Python"
            cases.append({"name": name, "input": raw, "dump": dump})
    out = GOLDEN / "config"
    out.mkdir(parents=True, exist_ok=True)
    (out / "cases.json").write_text(json.dumps(cases, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")
    # The exact text a first launch writes (Config.to_json), for the defaults and for a checkout.
    (out / "defaults.json").write_text(Config().to_json() + "\n", encoding="utf-8")
    (out / "fresh-checkout.json").write_text(fresh_config().to_json() + "\n", encoding="utf-8")


def normalized_schema(path: Path) -> dict:
    """Columns, indexes and foreign keys as SQLite reports them, independent of DDL formatting."""
    db = sqlite3.connect(path)
    schema = {}
    for (table,) in db.execute("SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name"):
        columns = [{"name": r[1], "type": r[2], "notnull": bool(r[3]), "default": r[4], "pk": r[5]}
                   for r in db.execute(f"PRAGMA table_info('{table}')")]
        indexes = {}
        for _, name, unique, origin, _ in db.execute(f"PRAGMA index_list('{table}')"):
            indexes[name if origin == "c" else f"<{origin}:{','.join(r[2] for r in db.execute(f'PRAGMA index_info({name!r})'))}>"] = {
                "unique": bool(unique), "origin": origin,
                "columns": [r[2] for r in db.execute(f"PRAGMA index_info('{name}')")]}
        foreign_keys = sorted([r[2], r[3], r[4]] for r in db.execute(f"PRAGMA foreign_key_list('{table}')"))
        schema[table] = {"columns": columns, "indexes": dict(sorted(indexes.items())), "foreignKeys": foreign_keys}
    db.close()
    return schema


# The tables as they were before each _upgrade_* step added columns, so the upgrade path is recorded.
OLD_SHAPE = """
CREATE TABLE tasks (id INTEGER NOT NULL PRIMARY KEY, jira_key VARCHAR(64) NOT NULL, repository_path TEXT NOT NULL,
  repository_root TEXT NOT NULL, project_relative_path TEXT, base_branch VARCHAR(255) NOT NULL,
  branch_name VARCHAR(255) NOT NULL, worktree_path TEXT NOT NULL, zellij_session_name VARCHAR(255) NOT NULL,
  task_dir TEXT NOT NULL, additional_context TEXT, status VARCHAR(32) NOT NULL, status_message TEXT,
  review_iteration INTEGER NOT NULL, max_review_iterations INTEGER NOT NULL, current_sha VARCHAR(64),
  agent_overrides JSON NOT NULL, created_at DATETIME NOT NULL, updated_at DATETIME NOT NULL);
CREATE UNIQUE INDEX ix_tasks_jira_key ON tasks (jira_key);
CREATE TABLE agent_questions (id INTEGER NOT NULL PRIMARY KEY, task_id INTEGER NOT NULL, node_run_id INTEGER,
  role VARCHAR(64) NOT NULL, message TEXT NOT NULL, answered BOOLEAN NOT NULL, created_at DATETIME NOT NULL,
  answered_at DATETIME, FOREIGN KEY(task_id) REFERENCES tasks (id));
CREATE TABLE agent_sessions (id INTEGER NOT NULL PRIMARY KEY, task_id INTEGER NOT NULL, role VARCHAR(64) NOT NULL,
  harness VARCHAR(64) NOT NULL, model VARCHAR(128), reasoning VARCHAR(32), zellij_tab_name VARCHAR(128) NOT NULL,
  zellij_tab_id INTEGER, zellij_pane_id VARCHAR(32), harness_session_id VARCHAR(128), status VARCHAR(32) NOT NULL,
  current_run_id INTEGER, started_at DATETIME NOT NULL, ended_at DATETIME, FOREIGN KEY(task_id) REFERENCES tasks (id));
CREATE TABLE review_findings (id INTEGER NOT NULL PRIMARY KEY, task_id INTEGER NOT NULL, node_run_id INTEGER,
  finding_id VARCHAR(32) NOT NULL, type VARCHAR(32) NOT NULL, iteration INTEGER NOT NULL, reviewed_sha VARCHAR(64),
  severity VARCHAR(16) NOT NULL, blocking BOOLEAN NOT NULL, file TEXT, line INTEGER, title TEXT NOT NULL,
  description TEXT, recommendation TEXT, status VARCHAR(16) NOT NULL, created_at DATETIME NOT NULL,
  resolved_at DATETIME, FOREIGN KEY(task_id) REFERENCES tasks (id));
CREATE TABLE run_evidence (uid VARCHAR(36) NOT NULL PRIMARY KEY, run_id INTEGER NOT NULL UNIQUE,
  requirements_digest VARCHAR(64), verification JSON, usage JSON, confirmed_model VARCHAR(128),
  FOREIGN KEY(run_id) REFERENCES node_runs (id));
INSERT INTO tasks VALUES (1, 'OLD-1', '/r', '/r', NULL, 'main', 'agent/OLD-1', '/w', 'agentflow-OLD-1', '/t', NULL,
  'ACTIVE', NULL, 1, 5, NULL, '{}', '2025-01-01 00:00:00.000000', '2025-01-01 00:00:00.000000');
INSERT INTO agent_questions VALUES (1, 1, NULL, 'architect', 'Old question?', 0, '2025-01-01 00:00:00.000000', NULL);
INSERT INTO agent_questions VALUES (2, 1, NULL, 'architect', 'Another?', 1, '2025-01-01 00:00:00', '2025-01-02 00:00:00');
"""


def record_db(populated: Path | None) -> None:
    out = GOLDEN / "db"
    out.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory() as tmp:
        fresh = Path(tmp) / "fresh.db"
        db = Database(fresh)
        db.create_all()
        db.engine.dispose()
        write_json(out / "schema-fresh.json", normalized_schema(fresh))

        old = Path(tmp) / "old.db"
        connection = sqlite3.connect(old)
        connection.executescript(OLD_SHAPE)
        connection.close()
        (out / "old-shape.sql").write_text(OLD_SHAPE.strip() + "\n", encoding="utf-8")
        db = Database(old)
        db.create_all()
        db.engine.dispose()
        write_json(out / "schema-upgraded.json", normalized_schema(old))
        upgraded = sqlite3.connect(old)
        questions = upgraded.execute("SELECT id, uid, choices, answer FROM agent_questions ORDER BY id").fetchall()
        upgraded.close()
        # The uids are random; record that every question got a distinct 36-character one.
        assert len({q[1] for q in questions}) == len(questions) and all(len(q[1]) == 36 for q in questions)
        write_json(out / "old-shape-questions-after-upgrade.json",
                   [{"id": q[0], "choices": q[2], "answer": q[3]} for q in questions])

    if populated:
        with tempfile.TemporaryDirectory() as tmp:
            copy = Path(tmp) / "populated.db"
            source = sqlite3.connect(f"file:{populated.as_posix()}?mode=ro", uri=True)
            target = sqlite3.connect(copy)
            source.backup(target)
            source.close()
            target.close()
            add_rows_a_stub_run_never_writes(copy)
            target = sqlite3.connect(copy)
            dump = "\n".join(target.iterdump()) + "\n"
            target.close()
        (out / "populated.sql").write_text(dump, encoding="utf-8")


def add_rows_a_stub_run_never_writes(path: Path) -> None:
    """One row in each table a stub workflow leaves empty, written through the Python models so the
    fixture has Python's own formats for them too."""
    from datetime import datetime, timezone

    from sqlalchemy import text

    from agentflow.database.models import (AgentQuestion, ChatThread, DeletedRunSpend, ImplementationWorkItem, Project,
                                           Task, WorkflowNode)

    db = Database(path)
    db.create_all()
    with db.session() as session:
        task = session.query(Task).first()
        node = session.query(WorkflowNode).filter_by(task_id=task.id, key="implementer").one()
        session.add(DeletedRunSpend(jira_key="GONE-1", run_id=99, charged_at=datetime(2026, 9, 30, 12, 0, tzinfo=timezone.utc),
                                    billing_mode="metered", cost_usd=0.42))
        session.add(DeletedRunSpend(jira_key="GONE-1", run_id=100, charged_at=datetime(2026, 9, 30, 12, 5, tzinfo=timezone.utc),
                                    billing_mode="unknown", cost_usd=None))
        # Fixed times and ids, so re-recording gives the same fixture.
        session.add(ChatThread(task_id=task.id, platform="slack", channel="D123", thread_id="1727.0001",
                               title="Ticket — ünïcode", notified={"WAITING_FOR_USER:architect": "1727.0002"},
                               updated_at=datetime(2026, 9, 30, 12, 10, 0, 123456, tzinfo=timezone.utc)))
        session.add(ImplementationWorkItem(uid="00000000-0000-4000-8000-000000000001",
                                           task_id=task.id, node_id=node.id, plan_key="api", brief="Add the endpoint",
                                           owned_paths=["src/api/"], acceptance_criteria=["200 on GET"], dependencies=[],
                                           worktree_path="/w/api", branch_name="agent/X-api", base_sha="a" * 40))
        project = Project(slug="billing-app", name="Billing App — ünïcode", color="teal", description="Invoice pages",
                          default_repository_path="/r", default_project_relative_path="src", default_base_branch="main",
                          agent_overrides={"implementer": {"harness": "codex"}},
                          created_at=datetime(2026, 10, 22, 9, 0, tzinfo=timezone.utc),
                          updated_at=datetime(2026, 10, 22, 9, 0, 0, 654321, tzinfo=timezone.utc))
        session.add(project)
        session.add(AgentQuestion(
            uid="00000000-0000-4000-8000-000000000002", task_id=task.id, node_run_id=None, role="architect",
            message="Which cleanup — ünïcode?", choices=["Soft-delete", "Hard-delete"], answered=False,
            created_at=datetime(2026, 10, 25, 9, 0, tzinfo=timezone.utc),
            decision={"context": "312 own invoices · ünïcode", "risk": "high", "choices": [
                {"label": "Soft-delete", "recommended": True, "followUp": {"message": "Purge when?", "choices": [
                    {"label": "Later", "detail": "Nightly job"},
                    {"label": "Never", "note": "Legal asked.", "source": "docs/privacy.md"}]}},
                {"label": "Hard-delete", "warning": "Breaks history"}]}))
        session.flush()
        # Plain SQL: assigning through the ORM would stamp the task's updated_at with the time now.
        session.execute(text("UPDATE tasks SET project_id = :project WHERE id = :task"),
                        {"project": project.id, "task": task.id})
    db.engine.dispose()


def write_schema_rs() -> None:
    """app/crates/core/src/db/schema.rs: the exact statements SQLAlchemy's create_all() runs for a
    fresh database, per table in its creation order."""
    from agentflow.database.models import Base

    lines = [
        "//! The schema SQLAlchemy's `create_all()` creates for a fresh database (backend/agentflow/database/models.py),",
        "//! as the exact statements it emits, in its creation order. Generated by contract/record_python_goldens.py;",
        "//! checked against contract/golden/db/schema-fresh.json by tests/db.rs.",
        "",
        "/// (table, statements creating it and its indexes). A table that already exists is skipped",
        "/// with its indexes, as SQLAlchemy does; columns added since are handled by the upgrades.",
        "#[rustfmt::skip] // generated; regenerating must not leave formatting changes",
        "pub const TABLES: &[(&str, &[&str])] = &[",
    ]
    with tempfile.TemporaryDirectory() as tmp:
        path = Path(tmp) / "fresh.db"
        db = Database(path)
        db.create_all()
        db.engine.dispose()
        connection = sqlite3.connect(path)
        for table in Base.metadata.sorted_tables:
            statements = connection.execute(
                "SELECT sql FROM sqlite_master WHERE tbl_name = ? AND sql IS NOT NULL ORDER BY type DESC, name",
                (table.name,)).fetchall()
            lines += ["    (", f'        "{table.name}",', "        &["]
            for (sql,) in statements:
                body = "\n".join(line.rstrip() for line in sql.strip().splitlines())
                lines.append(f'            r#"{body}"#,')
            lines += ["        ],", "    ),"]
        connection.close()
    lines.append("];")
    target = ROOT / "app" / "crates" / "core" / "src" / "db" / "schema.rs"
    target.write_text("\n".join(lines) + "\n", encoding="utf-8", newline="\n")


WORKSPACE_META = {"jiraKey": "WS-1", "branch": "agent/WS-1", "créé": "ünïcode", "nested": {"n": [1, 2.5, None, True]}}


def record_workspace() -> None:
    """The exact bytes TaskWorkspace writes (platform newlines included): golden/workspace/<platform>/."""
    from agentflow.services.workspace import TaskWorkspace

    out = GOLDEN / "workspace" / sys.platform
    with tempfile.TemporaryDirectory() as tmp:
        workspace = TaskWorkspace(Path(tmp), "WS-1")
        workspace.create(WORKSPACE_META, "  Some context\nwith two lines  ")
        TaskWorkspace.write_credential(workspace.root / "credential.json", {"runId": 7, "token": "t0k€n"})
        files = {"task.json": workspace.root / "task.json",
                 "additional-context.md": workspace.context_dir / "additional-context.md",
                 "credential.json": workspace.root / "credential.json"}
        out.mkdir(parents=True, exist_ok=True)
        for name, path in files.items():
            (out / name).write_bytes(path.read_bytes())


# A command that is never on PATH, so base_argv keeps it as written and argv[0] is deterministic.
NOT_INSTALLED = "agentflow-golden-not-installed"

# (name, extra HarnessConfig fields) for every adapter type, including an unknown type.
HARNESS_CONFIGS: list[tuple[str, dict]] = [
    ("plain", {}),
    ("args-and-env", {"args": ["--yolo", "two words"], "env": {"B_VAR": "2", "A_VAR": "1"}}),
    ("custom-flags", {"modelArgument": "-m", "reasoningArgument": "--think", "promptArgument": "--prompt"}),
    ("positional-prompt", {"promptArgument": "positional"}),
    ("reasoning-map", {"reasoningMap": {"high": "xhigh", "low": "minimal"}}),
]
HARNESS_TYPES = ["claude", "codex", "copilot", "generic-cli", "mystery-type"]
# (model, reasoning) pairs passed to build_launch / build_resume.
LAUNCH_OPTIONS = [(None, None), ("opus", None), (None, "high"), ("gpt-5.5", "low"), ("", "")]

# Rows of a rendered pane, as `zellij subscribe` or the embedded terminal's screen gives them.
SCREENS: list[tuple[str, list[str]]] = [
    ("empty", []),
    ("blank-rows", ["", "   ", ""]),
    ("claude-idle", ["╭───╮", "│ > │", "╰───╯", "  ? for shortcuts"]),
    ("claude-working", ["✻ Thinking… (12s · esc to interrupt)", "", ">"]),
    ("claude-trust-dialog", ["Quick safety check: Is this a project you created or one you trust?", "",
                             "❯ 1. Yes, I trust this folder", "  2. No, exit", "",
                             "Enter to confirm · Esc to cancel", *[""] * 40]),
    ("claude-permission", ["Bash command", "  rm -rf build", "Do you want to proceed?", "❯ 1. Yes", "  2. No"]),
    ("dialog-scrolled-out", ["Do you want to proceed?", *[f"line {n}" for n in range(12)]]),
    ("dialog-just-in-tail", ["Do you want to proceed?", *[f"line {n}" for n in range(11)]]),
    ("lone-prompt", ["some output", ">"]),
    ("indented-prompt", ["some output", "   >   "]),
    ("codex-idle", ["▌ Send a message", "", "›"]),
    ("codex-working", ["• Working (3s • esc to interrupt)"]),
    ("codex-approval", ["Allow command?", "  cargo test", "Approve this command [y/N]"]),
    ("copilot-idle", [" D:\\Dev\\AgentFlow [⎇ main]", "─" * 40, "❯", "─" * 40,
                      " ← open sidebar · / commands · ? help · tab next tab        GitHub Copilot • GPT-5.6 Luna"]),
    ("copilot-loading", [" ◉ Loading: 11 instructions, 9 agents, 2 plugins"]),
    ("copilot-select", ["│ ❯ 1. Yes", "│   3. No (Esc)", "│ ↑/↓ to navigate · enter to select · esc to cancel"]),
    ("copilot-yes-no", ["Run this? (y/N)"]),
    ("copilot-ask-user", ["Copilot needs information.", "Paste prompt.md or provide an in-workspace copy path.",
                          ">", "enter accept · ctrl+d decline · esc cancel"]),
    ("uppercase", ["ESC TO INTERRUPT"]),
    ("word-boundary", ["rethinking the plan", "networking"]),
    ("unrecognised", ["$ ls", "README.md  src"]),
]

ZELLIJ_VERSIONS = ["zellij 0.44.0", "zellij 0.45.1", "zellij 1.0.0", "zellij 0.43.9", "zellij", None,
                   "zellij 0.44.0-rc1", "0.44.0", "zellij v0.100.2", "zellij 0.9.99"]

TRUST_CWD = "D:/agentflow/worktrees/DE-1"
# (name, ~/.claude.json text or None for a missing file, cwd) for trust_workspace.
TRUST_CASES: list[tuple[str, str | None, str]] = [
    ("missing-file", None, TRUST_CWD),
    ("new-entry", json.dumps({"projects": {"C:/Other/Project": {"hasTrustDialogAccepted": True}}}), TRUST_CWD),
    ("keeps-other-fields", json.dumps({"projects": {TRUST_CWD: {"hasTrustDialogAccepted": False,
                                                                "allowedTools": ["Bash"]}}}), TRUST_CWD),
    ("already-trusted", json.dumps({"projects": {TRUST_CWD: {"hasTrustDialogAccepted": True}}}), TRUST_CWD),
    ("trusted-as-1", json.dumps({"projects": {TRUST_CWD: {"hasTrustDialogAccepted": 1}}}), TRUST_CWD),
    ("corrupt", "not valid json", TRUST_CWD),
    ("not-an-object", "[1, 2]", TRUST_CWD),
    ("projects-not-an-object", json.dumps({"projects": ["x"]}), TRUST_CWD),
    ("entry-not-an-object", json.dumps({"projects": {TRUST_CWD: "yes"}}), TRUST_CWD),
    ("no-projects", json.dumps({"numStartups": 3}), TRUST_CWD),
    ("backslash-cwd", "{}", "D:\\agentflow\\worktrees\\DE-2"),
    ("dot-segments", "{}", "D:/agentflow/./worktrees//DE-3/"),
    # Values Claude Code writes (Node: doubles only), formatted the way Node writes them.
    ("node-formatted", '{\n  "numStartups": 12,\n  "tipsHistory": {"a": 1e-7, "b": 1.5, "c": 1e21},\n'
                       '  "big": 123456789012345680000,\n  "name": "Ré\\u00e9 😀",\n  "dupe": 1,\n  "dupe": 2,\n'
                       '  "projects": {"C:/x": {"history": [], "n": null, "ok": false}}\n}\n', TRUST_CWD),
]


def record_harness() -> None:
    """Launch argv, screen classification, zellij version checks and trust_workspace writes."""
    import agentflow.harnesses.claude as claude_module
    from agentflow.config import HarnessConfig
    from agentflow.harnesses.base import SessionHandle
    from agentflow.harnesses.registry import make_adapter
    from agentflow.services.zellij import version_supported

    def normalized(argv: list[str], session_id: str | None) -> list[str]:
        assert argv[0] == NOT_INSTALLED, argv
        return [("<SESSION>" if session_id and arg == session_id else arg) for arg in argv[1:]]

    launches = []
    for kind in HARNESS_TYPES:
        for config_name, extra in HARNESS_CONFIGS:
            config = HarnessConfig(type=kind, command=NOT_INSTALLED, **extra)
            adapter = make_adapter("h", config)
            case = {"type": kind, "config": config_name, "extra": extra,
                    "typeName": adapter.type_name, "supportsResume": adapter.supports_resume,
                    "supportsModel": adapter.model_flag() is not None,
                    "supportsReasoning": adapter.reasoning_flag() is not None,
                    "exitCommand": adapter.exit_command, "runs": []}
            for model, reasoning in LAUNCH_OPTIONS:
                launch = adapter.build_launch(model=model, reasoning=reasoning, initial_prompt="Read the brief")
                handle = SessionHandle("implementer", "s", "t", None, Path("."), harness_session_id="sess-1")
                resume = adapter.build_resume(handle, model=model, reasoning=reasoning, initial_prompt="Read the brief")
                unknown = SessionHandle("implementer", "s", "t", None, Path("."))
                assert adapter.build_resume(unknown, model=model, reasoning=reasoning, initial_prompt="x") is None
                case["runs"].append({
                    "model": model, "reasoning": reasoning,
                    "launch": normalized(launch.argv, launch.harness_session_id),
                    "launchEnv": launch.env,
                    "launchSessionId": None if launch.harness_session_id is None else "<SESSION>",
                    "resume": None if resume is None else normalized(resume.argv, None),
                    "resumeSessionId": None if resume is None else resume.harness_session_id,
                })
            launches.append(case)

    screens = []
    for name, rows in SCREENS:
        states = {kind: make_adapter("h", HarnessConfig(type=kind, command="x")).classify_screen(rows)
                  for kind in HARNESS_TYPES}
        screens.append({"name": name, "rows": rows, "states": states})

    from agentflow.harnesses.base import HarnessAdapter
    references = {path: HarnessAdapter.prompt_reference(Path(path))
                  for path in ["C:/tasks/DE-1/runs/7/prompt.md", "C:\\tasks\\DE 1\\runs\\7\\prompt.md"]}

    trust = []
    original = claude_module._claude_config_path
    try:
        with tempfile.TemporaryDirectory() as tmp:
            for name, text, cwd in TRUST_CASES:
                path = Path(tmp) / name / ".claude.json"
                path.parent.mkdir()
                if text is not None:
                    path.write_bytes(text.encode("utf-8"))
                before = path.read_bytes() if path.exists() else None
                claude_module._claude_config_path = lambda path=path: path
                claude_module.trust_workspace(Path(cwd))
                after = path.read_bytes() if path.exists() else None
                assert not path.with_name(path.name + ".agentflow-tmp").exists()
                trust.append({"name": name, "input": text, "cwd": cwd, "written": after != before,
                              "output": None if after == before else after.decode("utf-8")})
    finally:
        claude_module._claude_config_path = original

    out = GOLDEN / "harness"
    out.mkdir(parents=True, exist_ok=True)
    write_json(out / "launches.json", launches)
    write_json(out / "screens.json", screens)
    write_json(out / "misc.json", {
        "promptReferences": references,
        "zellijVersions": [{"version": v, "supported": version_supported(v)} for v in ZELLIJ_VERSIONS],
    })
    write_json(out / "trust.json", trust)


PROMPT_CTX = dict(
    jira_key="DE-1", worktree_root="D:/agentflow/worktrees/DE-1", project_dir="D:/agentflow/worktrees/DE-1/Project",
    task_dir="D:/agentflow/tasks/DE-1", run_dir="D:/agentflow/tasks/DE-1/runs/architect/01", run_id=17,
    role="architect", base_branch="main", branch_name="agent/DE-1", additional_context="", iteration=1,
    max_iterations=5, flowctl_path="D:/agentflow/bin/flowctl", previous_review_sha=None,
)
PROMPT_MANIFEST = [
    {"id": "1", "file": "1-a.png", "status": "present", "firstSeenGeneration": 1, "referencedIn": ["description"]},
    {"id": "3", "file": "3-c.png", "status": "present", "firstSeenGeneration": 3, "referencedIn": ["comment:501"]},
    {"id": "7", "file": "7-repro.mp4", "status": "present", "firstSeenGeneration": 1, "referencedIn": [],
     "frames": ["frame-01-t00m00.0s.png", "frame-02-t00m04.0s.png"]},
    {"id": "9", "file": "9-broken.mov", "status": "present", "firstSeenGeneration": 1,
     "referencedIn": ["description", "comment:7"], "framesError": "ffmpeg exited with code 1"},
    {"id": "8", "file": "8-huge.mov", "filename": "huge.mov", "status": "skipped", "firstSeenGeneration": 1,
     "reason": "50.0 MB is over maxAttachmentBytes (20.0 MB)"},
    {"id": "2", "file": "2-old.png", "status": "removed", "firstSeenGeneration": 1, "removedGeneration": 3},
]
# (generation, lastPromptedGeneration or None, manifest entries); None: no manifest, files on disk only.
PROMPT_JIRA = {
    "none": None,
    "legacy": "legacy",
    "manifest": (3, 2, PROMPT_MANIFEST),
    "first-run": (2, None, PROMPT_MANIFEST[:2]),
    "unchanged": (3, 3, PROMPT_MANIFEST),
}
FINDINGS = [
    {"id": "CODE-001", "severity": "high", "blocking": True, "file": "src/app.py", "line": 12, "title": "Null check",
     "description": "Crashes on empty input.", "recommendation": "Guard it.", "occurrence": 2, "status": "open",
     "response": "disputed", "responseNote": "Input is validated upstream.", "dbId": 4},
    {"id": "SEC-002", "severity": "low", "blocking": False, "file": None, "line": None, "title": "Naming \u00e9",
     "description": "Minor.", "recommendation": "Rename.", "occurrence": 1},
]
NON_BLOCKING = [dict(FINDINGS[1])]
LONG_OUTPUT = "".join(f"line {n} with ``` fences\r\n" for n in range(200)) + "== 3 failed, 12 passed =="
VERIFICATIONS = {
    "none": None,
    "empty": {"sha": "b" * 40, "commands": [], "passed": False},
    "error": {"sha": "b" * 40, "error": "TimeoutError", "commands": []},
    "passed": {"sha": "b" * 40, "passed": True, "commands": [
        {"command": ["pytest", "-q"], "exitCode": 0, "output": "12 passed\n"},
        {"command": ["npm", "test"], "exitCode": 0, "output": ""}]},
    "failed": {"sha": "b" * 40, "passed": False, "commands": [
        {"command": ["pytest", "-q"], "exitCode": 1, "output": LONG_OUTPUT},
        {"command": ["lint"], "exitCode": None, "output": "  ````weird````  \n"}]},
}
HISTORY = {"iterations": 2, "finalSha": "c" * 40, "runs": [
    {"node": "implementer", "iteration": 1, "state": "COMPLETED", "harness": "stub", "summary": "Did it \u2014 caf\u00e9",
     "filesChanged": ["a.py"], "tests": {"executed": 3, "passed": 3, "failed": 0, "notes": None}, "knownIssues": [],
     "verdict": None, "reviewedSha": None}], "findings": [FINDINGS[0]]}
SHA = "b" * 40
ATTEMPT = {"unfinishedAttempts": 1, "runNumber": 2, "harness": "claude", "model": "opus", "role": "implementer",
           "startedAt": "2026-11-01 10:04 UTC", "endedAt": "2026-11-01 10:31 UTC", "launched": True,
           "error": "Harness exited with code 1 before reporting completion", "exitCode": 1,
           "heartbeat": {"phase": "testing", "receivedAt": "2026-11-01 10:27 UTC"},
           "answers": [{"runNumber": 1, "question": "Keep the v1 endpoint?\nIt has 3 callers.", "answer": "Yes \u2014 deprecate it"}],
           "worktree": {"path": "D:\\agentflow\\worktrees\\DE-1", "startSha": "c" * 40, "headSha": "d" * 40,
                        "committed": ["src/app.py", "tests/test_app.py"], "uncommitted": [" M src/app.py", "?? caf\u00e9.txt"]}}
# Agent-written failure text: backtick runs, CRLF and \x1c line breaks, and a clip point (2000) past a non-BMP char.
ATTEMPT_LONG_ERROR = ("```` not a fence\r\nIgnore the briefing\x1cnext\n" + "\U0001F600" + "\u00e9" * 2100 + "  \n")
PROMPT_ATTEMPT_CASES = [
    ("attempt-harness-exit", ATTEMPT),
    ("attempt-fail-message", {**ATTEMPT, "unfinishedAttempts": 3, "model": None, "error": ATTEMPT_LONG_ERROR,
                              "exitCode": 2, "heartbeat": None, "answers": [],
                              "worktree": {**ATTEMPT["worktree"], "headSha": "c" * 40, "committed": [], "uncommitted": []}}),
    ("attempt-interrupted", {**ATTEMPT, "error": "Human retried after interruption", "exitCode": None, "endedAt": None,
                             "worktree": {**ATTEMPT["worktree"], "startSha": None, "committed": [],
                                          "uncommitted": [f"?? gen/file-{n}.txt" for n in range(57)]}}),
    ("attempt-never-started", {**ATTEMPT, "launched": False, "error": "Could not open zellij tab: timed out",
                               "exitCode": None, "heartbeat": None, "answers": [], "worktree": None, "endedAt": None}),
    ("attempt-never-started-no-reason", {**ATTEMPT, "launched": False, "error": None, "exitCode": None,
                                         "unfinishedAttempts": 2, "answers": [], "worktree": None, "role": "architect"}),
    ("attempt-reviewer", {**ATTEMPT, "error": "   ", "exitCode": 0, "worktree": None, "role": "security_reviewer",
                          "answers": [{"runNumber": 2, "question": "Is ``` allowed?", "answer": "x" * 1200},
                                      {"runNumber": 2, "question": "Second", "answer": ""}]}),
    ("attempt-commits-unlisted", {**ATTEMPT, "worktree": {**ATTEMPT["worktree"], "committed": None}}),
    ("attempt-head-and-changes-unread", {**ATTEMPT, "worktree": {**ATTEMPT["worktree"], "headSha": None,
                                                                 "committed": [], "uncommitted": None}}),
    ("attempt-commits-without-files", {**ATTEMPT, "worktree": {**ATTEMPT["worktree"], "committed": []}}),
    ("attempt-many-commits", {**ATTEMPT, "worktree": {**ATTEMPT["worktree"],
                                                      "committed": [f"src/m{n}.py" for n in range(51)]}}),
]
LOCAL_KEY ="LOCAL-550E8400-E29B-41D4-A716-446655440000"
LOCAL_CTX = dict(jira_key=LOCAL_KEY, branch_name=f"agent/{LOCAL_KEY}",
                 additional_context="Tidy the readme\nKeep the links\n\nUse the house style.")
PROMPT_CASES: list[tuple[str, str, dict, dict, str]] = [
    # (name, function, ctx overrides, arguments, jira layout)
    ("discovery-plain", "architect_discovery", {}, {}, "none"),
    ("discovery-no-project", "architect_discovery", {"project_dir": None}, {}, "none"),
    ("discovery-context", "architect_discovery", {"additional_context": "  Use the v2 API.\n\n\u00c9t\u00e9\x1c \n"}, {}, "none"),
    ("discovery-legacy-files", "architect_discovery", {}, {}, "legacy"),
    ("discovery-manifest", "architect_discovery", {}, {}, "manifest"),
    ("discovery-first-run", "architect_discovery", {}, {}, "first-run"),
    ("revision-manifest", "architect_revision", {}, {"feedback": "  tighten AC\n", "revision": 2}, "manifest"),
    ("revision-blank-feedback", "architect_revision", {"additional_context": "ctx"}, {"feedback": "   ", "revision": 1}, "none"),
    ("revision-unchanged-ticket", "architect_revision", {}, {"feedback": "x", "revision": 3}, "unchanged"),
    ("implementer-first", "implementer", {"role": "implementer"},
     {"findings": None, "requirements": {}, "previous_sha": None}, "none"),
    ("implementer-fix-round", "implementer", {"role": "implementer", "iteration": 3, "project_dir": None},
     {"findings": FINDINGS, "requirements": {}, "previous_sha": SHA}, "none"),
    ("code-review-first", "code_review", {"role": "code_reviewer"},
     {"sha": SHA, "implementation_summary": "  Added the check.\n", "requirements": {}, "verification": "none"}, "none"),
    ("code-review-empty-verification", "code_review", {"role": "code_reviewer"},
     {"sha": SHA, "implementation_summary": "", "requirements": {}, "verification": "empty"}, "none"),
    ("code-review-error", "code_review", {"role": "code_reviewer"},
     {"sha": SHA, "implementation_summary": "x", "requirements": {}, "verification": "error"}, "none"),
    ("code-review-passed", "code_review", {"role": "code_reviewer"},
     {"sha": SHA, "implementation_summary": "x", "requirements": {}, "verification": "passed"}, "none"),
    ("code-review-failed", "code_review", {"role": "code_reviewer"},
     {"sha": SHA, "implementation_summary": "x", "requirements": {}, "verification": "failed"}, "none"),
    ("code-review-followup-blockers", "code_review", {"role": "code_reviewer", "iteration": 2, "previous_review_sha": "d" * 40},
     {"sha": SHA, "implementation_summary": "fixed", "requirements": {}, "previous_findings": FINDINGS, "verification": "passed"}, "none"),
    ("code-review-followup-no-blockers", "code_review", {"role": "code_reviewer", "iteration": 2, "previous_review_sha": "d" * 40},
     {"sha": SHA, "implementation_summary": "fixed", "requirements": {}, "previous_findings": NON_BLOCKING, "verification": "none"}, "none"),
    ("code-review-followup-empty", "code_review", {"role": "code_reviewer", "iteration": 2, "previous_review_sha": "d" * 40},
     {"sha": SHA, "implementation_summary": "fixed", "requirements": {}, "previous_findings": [], "verification": "none"}, "none"),
    ("code-review-delta-no-previous-findings", "code_review", {"role": "code_reviewer", "previous_review_sha": "d" * 40},
     {"sha": SHA, "implementation_summary": "x", "requirements": {}, "previous_findings": None, "verification": "none"}, "none"),
    ("security-review-first", "security_review", {"role": "security_reviewer"},
     {"sha": SHA, "implementation_summary": "x", "requirements": {}, "verification": "failed"}, "none"),
    ("security-review-followup", "security_review", {"role": "security_reviewer", "iteration": 2, "previous_review_sha": "d" * 40},
     {"sha": SHA, "implementation_summary": "x", "requirements": {}, "previous_findings": FINDINGS, "verification": "none"}, "none"),
    ("final-architect", "final_architect", {}, {"final_sha": "c" * 40, "history": HISTORY}, "none"),
    ("final-architect-manifest", "final_architect", {}, {"final_sha": "c" * 40, "history": HISTORY}, "manifest"),
    ("orchestrator", "orchestrator", {"role": "orchestrator"}, {}, "none"),
    ("architect-planning", "orchestrator", {"role": "architect"}, {}, "none"),
    ("worker", "worker", {"role": "implementer"}, {"item": {
        "plan_key": "api", "worktree_path": "D:/agentflow/worktrees/DE-1-worker-api", "branch_name": "agentflow-worker/DE-1/api",
        "base_sha": SHA, "brief": "Build the API \u2014 carefully.", "owned_paths": ["backend/api", "docs/api.md"],
        "acceptance_criteria": ["Returns 200", "caf\u00e9"]},
        "dependency_results": [{"key": "db", "summary": "Schema", "sha": "e" * 40}]}, "none"),
    ("integration", "integration", {"role": "implementer"}, {
        "workers": [{"key": "api", "sha": SHA, "summary": "API \u00e9"}], "summary": "Merge api then ui."}, "none"),
    # Local prompt tasks: no Jira wording, and Jira files on disk (manifest) are ignored.
    ("local-discovery", "architect_discovery", {**LOCAL_CTX}, {}, "manifest"),
    ("local-revision", "architect_revision", {**LOCAL_CTX}, {"feedback": "tighten AC", "revision": 2}, "manifest"),
    ("local-implementer", "implementer", {**LOCAL_CTX, "role": "implementer"},
     {"findings": FINDINGS, "requirements": {}, "previous_sha": SHA}, "none"),
    ("local-code-review", "code_review", {**LOCAL_CTX, "role": "code_reviewer"},
     {"sha": SHA, "implementation_summary": "x", "requirements": {}, "verification": "passed"}, "none"),
    ("local-security-review", "security_review", {**LOCAL_CTX, "role": "security_reviewer"},
     {"sha": SHA, "implementation_summary": "x", "requirements": {}, "verification": "none"}, "none"),
    ("local-final-architect", "final_architect", {**LOCAL_CTX}, {"final_sha": "c" * 40, "history": HISTORY}, "manifest"),
    ("local-orchestrator", "orchestrator", {**LOCAL_CTX, "role": "orchestrator"}, {}, "none"),
    # The section appended to a retry's briefing (it takes no context).
    *((name, "previous_attempt", {}, {"attempt": value}, "none") for name, value in PROMPT_ATTEMPT_CASES),
]


def record_prompts() -> None:
    """Every prompt function rendered from Python across PROMPT_CASES: golden/prompts-unit/cases.json.
    A case's task directory is a temp dir (spelled <TMP> in the golden) when it has Jira files."""
    from types import SimpleNamespace

    from agentflow.workflow import prompts, worker_prompts

    cases = []
    with tempfile.TemporaryDirectory() as tmp:
        root = Path(tmp).resolve()
        for name, function, overrides, arguments, jira in PROMPT_CASES:
            ctx_fields = {**PROMPT_CTX, **overrides}
            layout = PROMPT_JIRA[jira]
            if layout is not None:
                task_dir = root / name / "DE-1"
                context = task_dir / "context"
                (context / "attachments").mkdir(parents=True)
                (context / "jira-issue.md").write_text("# DE-1: title\n", encoding="utf-8")
                if layout == "legacy":
                    for file in ("9-screenshot.png", "1-log.txt"):
                        (context / "attachments" / file).write_bytes(b"x")
                else:
                    generation, last, entries = layout
                    (context / "attachments-removed").mkdir()
                    for entry in entries:
                        folder = "attachments-removed" if entry["status"] == "removed" else "attachments"
                        if entry["status"] != "skipped":
                            (context / folder / entry["file"]).write_bytes(b"x")
                    (context / "jira-manifest.json").write_text(json.dumps(
                        {"syncGeneration": generation, "lastPromptedGeneration": last, "attachments": entries}),
                        encoding="utf-8")
                ctx_fields["task_dir"] = str(task_dir)
            ctx = prompts.PromptContext(**{
                key: (Path(value) if key in ("worktree_root", "project_dir", "task_dir", "run_dir") and value is not None
                      else value) for key, value in ctx_fields.items()})
            args = dict(arguments)
            if "verification" in args:
                args["verification"] = VERIFICATIONS[args["verification"]]
            if function in ("orchestrator", "worker", "integration"):
                if function == "worker":
                    args["item"] = SimpleNamespace(**args["item"])
                text = getattr(worker_prompts, function)(ctx, **args)
            elif function in ("architect_revision",):
                text = prompts.architect_revision(ctx, args["feedback"], args["revision"])
            elif function == "previous_attempt":
                text = prompts.previous_attempt(args["attempt"])
            else:
                text = getattr(prompts, function)(ctx, **args)
            if layout is not None:
                for spelling in (str(root), root.as_posix()):
                    text = text.replace(spelling, "<TMP>")
            cases.append({"name": name, "function": function, "ctx": {**PROMPT_CTX, **overrides},
                          "arguments": arguments, "jira": jira, "text": text})
    out = GOLDEN / "prompts-unit"
    out.mkdir(parents=True, exist_ok=True)
    (out / "inputs.json").write_text(json.dumps({"manifest": PROMPT_MANIFEST, "layouts": {
        k: (v if v in (None, "legacy") else {"generation": v[0], "lastPrompted": v[1], "entries": v[2]})
        for k, v in PROMPT_JIRA.items()}, "verifications": VERIFICATIONS}, indent=2) + "\n", encoding="utf-8")
    write_json(out / "cases.json", cases)


# ---------------------------------------------------------------------------- usage collectors

USAGE_CATALOG = {
    "openai": {"models": {
        "gpt-test": {"cost": {"input": 2, "output": 10, "cache_read": 0.2, "cache_write": 2.5,
                              "tiers": [{"input": 4, "output": 15, "cache_read": 0.4, "cache_write": 5,
                                         "tier": {"type": "context", "size": 272000}},
                                        {"input": 3, "output": 12, "tier": {"type": "context", "size": 128000}},
                                        {"input": 9, "tier": {"type": "output", "size": 1000}}]}},
        "gpt-nullcache": {"cost": {"input": 1.25, "output": 7.5, "cache_read": None}},
        "gpt-free": {"cost": {"input": 0, "output": 0}},
    }},
    "zz-other": {"models": {"other-model": {"cost": {"input": 0.3, "output": 0.7, "cache_read": 0.03}}}},
    "anthropic": {"models": {"claude-test-5-5": {"cost": {"input": 4, "output": 20, "cache_read": 0.2,
                                                          "cache_write": 5}}}},
    "google": None,
    "xai": {"models": {"gpt-test": {"cost": {"input": 99, "output": 99}}}},
}
PROXY_CATALOG = ".local/share/copilot-api/models-dev-api.json"
OWN_CATALOG = ".cache/agentflow/models-dev-api.json"


def _jsonl(*entries: object, newline: str = "\n") -> str:
    return newline.join(e if isinstance(e, str) else json.dumps(e) for e in entries)


def _assistant(message_id, model, usage, stop=None):
    message = {"id": message_id, "model": model, "usage": usage}
    if stop is not None:
        message["stop_reason"] = stop
    return {"type": "assistant", "message": message}


def _claude_case(name, lines, *, session="sess", ledger=None, catalog=PROXY_CATALOG, extra=None):
    files = {f".claude/projects/proj/{session}.jsonl": lines} if lines is not None else {}
    if catalog:
        files[catalog] = json.dumps(USAGE_CATALOG)
    files.update(extra or {})
    return {"name": name, "collector": "claude", "session": session, "files": files, "ledger": ledger}


def _codex_meta(identity, source="cli", key="id"):
    return {"type": "session_meta", "payload": {key: identity, "source": source, "cwd": "same"}}


def _codex_usage(**usage):
    return {"type": "event_msg", "payload": {"type": "token_count", "info": {"total_token_usage": usage}}}


def _codex_case(name, rollouts, *, session="abc", mtimes=None):
    files = {f".codex/sessions/{rel}": text for rel, text in rollouts.items()}
    files[PROXY_CATALOG] = json.dumps(USAGE_CATALOG)
    return {"name": name, "collector": "codex", "session": session, "files": files,
            "mtimes": {f".codex/sessions/{rel}": t for rel, t in (mtimes or {}).items()}}


def _copilot_case(name, *events, session="cs", raw=None):
    text = raw if raw is not None else _jsonl(*events)
    files = {f".copilot/session-state/{session}/events.jsonl": text} if text is not None else {}
    return {"name": name, "collector": "copilot", "session": session, "files": files}


def _shutdown(nano_aiu=None, per_model=None, models=("gpt-test",)):
    metrics = {}
    for model in models:
        metrics[model] = {"usage": {"inputTokens": 100}}
        if per_model is not None:
            metrics[model]["totalNanoAiu"] = per_model
    data = {"modelMetrics": metrics}
    if nano_aiu is not None:
        data["totalNanoAiu"] = nano_aiu
    return {"type": "session.shutdown", "data": data}


TURN = ({"type": "user.message", "data": {}}, {"type": "assistant.turn_start", "data": {}},
        {"type": "assistant.message", "data": {"model": "gpt-test"}}, {"type": "assistant.turn_end", "data": {}})
U1 = {"input_tokens": 1200, "output_tokens": 300, "cache_creation_input_tokens": 500, "cache_read_input_tokens": 7000}
U2 = {"input_tokens": 10, "output_tokens": 45, "cache_read_input_tokens": 8100,
      "cache_creation_input_tokens": 260, "cache_creation": {"ephemeral_1h_input_tokens": 60}}
MONTH = "2026/09"


def usage_cases() -> list[dict]:
    claude = [
        _claude_case("claude-basic", _jsonl(
            {"type": "user", "message": {"content": "hi"}}, "", "not json", "   ",
            _assistant("m1", "gpt-test", U1, "tool_use"), _assistant("m1", "gpt-test", U1, "tool_use"),
            _assistant("m2", "gpt-test", U2, "end_turn"),
            {"type": "assistant", "message": {"id": "s", "model": "<synthetic>", "usage": U1}},
            {"type": "assistant", "message": {"model": "gpt-test"}})),
        _claude_case("claude-open-turn", _jsonl(_assistant("m1", "gpt-test", U1, "end_turn"),
                                                _assistant("m2", "gpt-test", U2, "tool_use"))),
        _claude_case("claude-pause-on-a-skipped-entry", _jsonl(
            _assistant("m1", "gpt-test", U1, "end_turn"),
            {"type": "assistant", "message": {"model": "<synthetic>", "usage": U2, "stop_reason": "pause_turn"}})),
        _claude_case("claude-closed-after-tool-use", _jsonl(
            _assistant("m1", "gpt-test", U1, "tool_use"), {"type": "user", "message": {}},
            {"type": "assistant", "message": {"id": "m1", "stop_reason": "end_turn"}})),
        _claude_case("claude-no-message-ids", _jsonl(_assistant(None, "gpt-test", U2), _assistant(None, "gpt-test", U2))),
        _claude_case("claude-long-context", _jsonl(
            _assistant("m1", "GPT-Test[1m]", {"input_tokens": 300_000, "output_tokens": 10}),
            _assistant("m2", "openai/gpt-test", {"input_tokens": 150_000, "output_tokens": 20}),
            _assistant("m3", "gpt-test", {"input_tokens": 100_000, "output_tokens": 30, "cache_read_input_tokens": 172_000}))),
        _claude_case("claude-one-hour-cache-and-dot-names", _jsonl(
            _assistant("m1", "claude-test-5.5", {"input_tokens": 0, "output_tokens": 0, "cache_creation_input_tokens": 300,
                                                 "cache_creation": {"ephemeral_1h_input_tokens": 100}}),
            _assistant("m2", " Claude-Test-5.5 ", {"input_tokens": 7, "output_tokens": 3, "cache_creation_input_tokens": 50,
                                                   "cache_creation": {"ephemeral_1h_input_tokens": 80}}))),
        _claude_case("claude-unknown-model-drops-the-estimate", _jsonl(
            _assistant("m1", "gpt-test", U1), _assistant("m2", "mystery-model", U2), _assistant("m3", "gpt-test", U2))),
        _claude_case("claude-null-and-missing-cache-rates", _jsonl(
            _assistant("m1", "gpt-nullcache", U1), _assistant("m2", "other-model", U2))),
        _claude_case("claude-zero-rates-are-not-prices", _jsonl(_assistant("m1", "gpt-free", U1))),
        _claude_case("claude-loose-numbers", _jsonl(_assistant("m1", "gpt-test", {
            "input_tokens": "12", "output_tokens": 3.9, "cache_read_input_tokens": True,
            "cache_creation_input_tokens": None, "cache_creation": {}}))),
        _claude_case("claude-crlf-lines", _jsonl(_assistant("m1", "gpt-test", U1), _assistant("m2", "gpt-test", U2),
                                                 newline="\r\n") + "\r\n"),
        _claude_case("claude-proxy-billed", _jsonl(_assistant("m1", "gpt-test", U1)),
                     ledger=[("sess", 2_000_000_000, "USD"), ("sess", 500_000_001, "USD"), ("other", 9, "USD")]),
        _claude_case("claude-proxy-null-cost", _jsonl(_assistant("m1", "gpt-test", U1)),
                     ledger=[("sess", 2_000_000_000, "USD"), ("sess", None, None)]),
        _claude_case("claude-proxy-other-currency", _jsonl(_assistant("m1", "gpt-test", U1)),
                     ledger=[("sess", 2_000_000_000, "USD"), ("sess", 5, "EUR")]),
        _claude_case("claude-proxy-no-rows", _jsonl(_assistant("m1", "gpt-test", U1)), ledger=[("other", 1, "USD")]),
        _claude_case("claude-own-catalog-cache", _jsonl(_assistant("m1", "gpt-test", U1)), catalog=OWN_CATALOG),
        _claude_case("claude-only-synthetic", _jsonl(
            {"type": "assistant", "message": {"model": "<synthetic>", "usage": U1}})),
        _claude_case("claude-no-transcript", None),
        _claude_case("claude-unsafe-session-id", _jsonl(_assistant("m1", "gpt-test", U1)), session="a.b"),
        _claude_case("claude-message-not-an-object", _jsonl(_assistant("m1", "gpt-test", U1),
                                                            {"type": "assistant", "message": "oops"})),
        _claude_case("claude-hidden-project-is-ignored", None, extra={
            ".claude/projects/.hidden/sess.jsonl": _jsonl(_assistant("m1", "gpt-test", U1))}),
    ]
    day = f"{MONTH}/22"
    codex = [
        _codex_case("codex-basic", {f"{day}/rollout-1.jsonl": _jsonl(
            _codex_meta("abc"), {"type": "turn_context", "payload": {"model": "gpt-test"}},
            _codex_usage(input_tokens=1000, cached_input_tokens=600, output_tokens=50),
            "garbage", _codex_usage(input_tokens=5000, cached_input_tokens=3000, output_tokens=400,
                                    reasoning_output_tokens=300))}),
        _codex_case("codex-thread-settings-model", {f"{day}/r.jsonl": _jsonl(
            _codex_meta("abc"), {"type": "turn_context", "payload": {"model": "mystery"}},
            {"type": "event_msg", "payload": {"type": "thread_settings_applied", "thread_settings": {"model": "gpt-test"}}},
            _codex_usage(input_tokens=400_000, cached_input_tokens=100, output_tokens=7, cache_write_input_tokens=900))}),
        _codex_case("codex-unknown-model-has-no-cost", {f"{day}/r.jsonl": _jsonl(
            _codex_meta("abc"), {"type": "turn_context", "payload": {"model": "mystery"}},
            _codex_usage(input_tokens=10, output_tokens=1))}),
        _codex_case("codex-only-cli-sessions", {
            f"{day}/vscode.jsonl": _jsonl(_codex_meta("abc", source="vscode"), {"type": "turn_context", "payload": {"model": "gpt-test"}},
                                          _codex_usage(input_tokens=1, output_tokens=1)),
            f"{MONTH}/21/cli.jsonl": _jsonl(_codex_meta("abc", key="session_id"), {"type": "turn_context", "payload": {"model": "gpt-test"}},
                                            _codex_usage(input_tokens=77, output_tokens=7))}),
        _codex_case("codex-newest-file-wins", {
            f"{day}/a.jsonl": _jsonl(_codex_meta("abc"), {"type": "turn_context", "payload": {"model": "gpt-test"}},
                                     _codex_usage(input_tokens=111, output_tokens=1)),
            f"{day}/b.jsonl": _jsonl(_codex_meta("abc"), {"type": "turn_context", "payload": {"model": "gpt-test"}},
                                     _codex_usage(input_tokens=222, output_tokens=2))},
            mtimes={f"{day}/a.jsonl": 1_790_000_200, f"{day}/b.jsonl": 1_790_000_100}),
        _codex_case("codex-no-token-count", {f"{day}/r.jsonl": _jsonl(
            _codex_meta("abc"), {"type": "turn_context", "payload": {"model": "gpt-test"}})}),
        _codex_case("codex-empty-session-id", {f"{day}/r.jsonl": _jsonl(_codex_meta(""))}, session=""),
        _codex_case("codex-scan-stops-after-30-days", {
            **{f"{MONTH}/{d:02d}/x.jsonl": _jsonl(_codex_meta("other")) for d in range(1, 31)},
            "2026/08/31/old.jsonl": _jsonl(_codex_meta("abc"), {"type": "turn_context", "payload": {"model": "gpt-test"}},
                                           _codex_usage(input_tokens=5, output_tokens=5))}),
        _codex_case("codex-thirtieth-day-is-scanned", {
            **{f"{MONTH}/{d:02d}/x.jsonl": _jsonl(_codex_meta("other")) for d in range(2, 31)},
            "2026/08/31/old.jsonl": _jsonl(_codex_meta("abc"), {"type": "turn_context", "payload": {"model": "gpt-test"}},
                                           _codex_usage(input_tokens=5, output_tokens=5))}),
    ]
    copilot = [
        _copilot_case("copilot-shutdown", {"type": "session.start"}, *TURN, _shutdown(125_000_000_000)),
        _copilot_case("copilot-per-model-sum", _shutdown(per_model=50_000_000_001, models=("gpt-test", "b"))),
        _copilot_case("copilot-per-model-missing-one", {"type": "session.start", "data": {"selectedModel": "x"}},
                      {"type": "session.shutdown", "data": {"modelMetrics": {"a": {"totalNanoAiu": 5}, "b": {}}}}),
        _copilot_case("copilot-checkpoint-after-turn", {"type": "session.start", "data": {"selectedModel": "gpt-test"}},
                      *TURN, {"type": "session.usage_checkpoint", "data": {"totalNanoAiu": 634_300_000}}, {"type": "hook.end"}),
        _copilot_case("copilot-turn-after-snapshot-is-pending", {"type": "session.start"}, *TURN,
                      {"type": "session.usage_checkpoint", "data": {"totalNanoAiu": 1}},
                      {"type": "user.message"}, {"type": "assistant.turn_start"}),
        _copilot_case("copilot-resumed-turn-is-pending", {"type": "session.start"}, _shutdown(1_000_000_000),
                      {"type": "session.resume"}, *TURN),
        _copilot_case("copilot-resumed-before-a-turn", {"type": "session.start"}, *TURN, _shutdown(2_000_000_000),
                      {"type": "session.resume"}),
        _copilot_case("copilot-snapshot-without-credits", _shutdown(2_000_000_000), {"type": "session.resume"},
                      {"type": "session.shutdown", "data": {"modelMetrics": {}}}),
        _copilot_case("copilot-models-sorted-and-joined", {"type": "session.start", "data": {"selectedModel": "x"}},
                      {"type": "assistant.message", "data": {"model": "zeta"}},
                      {"type": "assistant.message", "data": {"model": "alpha"}},
                      {"type": "assistant.message", "data": {"model": "zeta"}}, _shutdown(3)),
        _copilot_case("copilot-model-change", {"type": "session.start", "data": {"selectedModel": "first"}},
                      {"type": "session.model_change", "data": {"newModel": "second"}}, _shutdown(7)),
        _copilot_case("copilot-no-model-at-all", _shutdown(9)),
        _copilot_case("copilot-float-credits-are-not-credits", {"type": "session.shutdown", "data": {"totalNanoAiu": 1.0}}),
        _copilot_case("copilot-bool-credits-are-not-credits", {"type": "session.shutdown", "data": {"totalNanoAiu": True,
                      "modelMetrics": {"m": {"totalNanoAiu": 4}}}}),
        _copilot_case("copilot-activity-data-not-an-object", {"type": "tool.call", "data": [1]}, _shutdown(11)),
        *[_copilot_case(f"copilot-{kind.split('.')[0]}-event-after-snapshot-is-pending", _shutdown(5), {"type": kind})
          for kind in ("tool.execution_start", "subagent.started", "model.call_failure", "user.message",
                       "assistant.turn_start")],
        _copilot_case("copilot-other-events-after-snapshot-are-settled", _shutdown(5), {"type": "session.info"},
                      {"type": "hook.start"}, {"type": "toolbox"}, {"type": ""}, {"data": {}}),
        _copilot_case("copilot-no-events-file", raw=None),
        _copilot_case("copilot-unsafe-session-id", _shutdown(1), session="../x"),
        _copilot_case("copilot-type-not-a-string", {"type": 5}, _shutdown(1)),
    ]
    return claude + codex + copilot


def record_usage() -> None:
    """Every bundled usage collector run by Python on fixture homes: golden/usage/cases.json.

    Each case has a price catalog on disk, so no recording touches the network."""
    import os
    import subprocess

    cases = []
    with tempfile.TemporaryDirectory() as tmp:
        for case in usage_cases():
            home = Path(tmp) / case["name"]
            home.mkdir()
            for rel, text in case["files"].items():
                path = home / rel
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_bytes(text.encode("utf-8"))
            for rel, mtime in case.get("mtimes", {}).items():
                os.utime(home / rel, (mtime, mtime))
            if case.get("ledger") is not None:
                database = home / ".local" / "share" / "copilot-api" / "copilot-api.sqlite"
                database.parent.mkdir(parents=True, exist_ok=True)
                connection = sqlite3.connect(database)
                connection.execute("CREATE TABLE token_usage_events (session_id TEXT, total_cost_nanos INTEGER,"
                                   " cost_currency TEXT)")
                connection.executemany("INSERT INTO token_usage_events VALUES (?, ?, ?)", case["ledger"])
                connection.commit()
                connection.close()
            env = {k: v for k, v in os.environ.items() if k.upper() not in ("COPILOT_API_HOME", "HOME", "USERPROFILE")}
            env.update(USERPROFILE=str(home), HOME=str(home), AGENTFLOW_SESSION_ID=case["session"])
            script = ROOT / "scripts" / f"usage_{case['collector']}.py"
            done = subprocess.run([sys.executable, str(script)], env=env, capture_output=True, text=True,
                                  encoding="utf-8", timeout=60)
            if OWN_CATALOG not in case["files"]:
                assert not (home / OWN_CATALOG).exists(), f"{case['name']} downloaded a price catalog"
            cases.append({**case, "exit": done.returncode,
                          "report": json.loads(done.stdout) if done.stdout.strip() else None})
    (GOLDEN / "usage").mkdir(exist_ok=True)
    write_json(GOLDEN / "usage" / "cases.json", cases)


def write_json(path: Path, value: object) -> None:
    path.write_text(json.dumps(value, indent=2) + "\n", encoding="utf-8")


if __name__ == "__main__":
    record_config()
    record_db(Path(sys.argv[1]) if len(sys.argv) > 1 else None)
    record_workspace()
    record_harness()
    record_prompts()
    record_usage()
    write_schema_rs()
    print(f"recorded into {GOLDEN}")
