# Developing AgentFlow

Setting up a source checkout is covered in [Installing AgentFlow](installation.md#install-from-source).
This page covers what contributors need on top of that.

## Frontend development

For frontend development run the Vite dev server as well (it proxies `/api` and the WebSocket to the backend):

```bash
cd frontend && npm run dev        # http://127.0.0.1:5173
```

`scripts/dev.ps1` / `scripts/dev.sh` start both.

## Building the Rust code

Only needed to build from source: the desktop app, the Rust `flowctl`, or `cargo test` in `app/`. The
desktop installer ships prebuilt binaries, so installing AgentFlow never compiles anything.

Install Rust with [rustup](https://rustup.rs). On Windows, Rust links with Microsoft's linker and the
Windows SDK, and some dependencies (the bundled SQLite, `ring`) compile C code, so you also need the
**MSVC C++ build tools**, either of:

- Visual Studio (Community is enough): in the Visual Studio Installer choose *Modify* and tick the
  **Desktop development with C++** workload.
- The standalone Build Tools, from a terminal:
  `winget install Microsoft.VisualStudio.2022.BuildTools --override "--passive --wait --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended"`

Without them `cargo build` stops at the link step, with `linker 'link.exe' not found` or, when Git for
Windows puts its own `link` first on `PATH`, `link: missing operand`. `cargo build` in `app/` checks the
setup.

## Smoke test

A stub harness (`scripts/stub_agent.py`) behaves like an interactive agent and lets you exercise the entire engine
without real AI sessions or a Jira MCP:

```bash
backend/.venv/Scripts/python.exe scripts/smoke_test.py     # Windows
backend/.venv/bin/python scripts/smoke_test.py             # POSIX
```

It creates a throw-away repo and isolated `AGENTFLOW_HOME`, starts a backend on port 8799 and walks a task through:
question → answer, approval, implementation, checkpoint, parallel reviews, fix loop, clean reviews, persistent-Architect
final verification, `READY_FOR_PR`; then a second task that hits the iteration cap (`NEEDS_HUMAN_REVIEW`) and the
review-SHA mismatch guard (the mismatch assertion is best-effort: on a slow machine the stub reviewer may report first, in which case that check is skipped and printed as such). Requires git and zellij.

## Repository layout

```
backend/agentflow/
  main.py            FastAPI app, static UI serving, WebSocket; `agentflow setup` / `doctor` dispatch
  config.py          Pydantic configuration model + loader, Jira startup check
  setup_wizard.py    `agentflow setup` (interactive config.json writer, Jira MCP step)
  doctor.py          `agentflow doctor` (read-only checks, shared with the wizard)
  installers.py      install-with-consent routes for missing tools (winget / brew / Linux)
  api/               tasks, agent (flowctl) endpoints, config/fs, harnesses, jira (search proxy)
  workflow/          engine.py (state machine), default_workflow.py, states.py, models.py (contracts), prompts.py,
                     pr_sync.py (GitHub PR poller)
  harnesses/         base.py adapter contract, claude.py, codex.py, copilot.py, generic_cli.py, registry.py
  services/          git_workspace.py, zellij.py, workspace.py, process.py, events.py, jira_client.py,
                     github.py (read-only gh CLI lookups)
  database/          SQLAlchemy models + SQLite session factory
  cli/flowctl.py     agent-side CLI
frontend/src/        Vue 3 + TypeScript + Pinia + Vue Flow (views/, components/, stores/, api/, workflow/)
scripts/             stub_agent.py, smoke_test.py, dev.ps1 / dev.sh, usage_*.py (usage collectors)
setup.ps1 / setup.sh guided setup (Python + backend, then `agentflow setup`)
start.sh             one-click launcher (runs setup first when Jira isn't configured)
config.example.json
```
