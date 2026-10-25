#!/usr/bin/env bash
# One-click launcher for AgentFlow.
#   - installs the backend venv and frontend deps on first run
#   - builds the frontend (or rebuilds when sources changed)
#   - starts the backend (which serves the UI) and opens the browser
#
# Windows: double-click this file (Git Bash) or run  bash start.sh
# macOS/Linux: ./start.sh
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")"
ROOT="$(pwd)"

# Never inherit an ambient zellij session into the backend.
unset ZELLIJ ZELLIJ_SESSION_NAME ZELLIJ_PANE_ID ZELLIJ_SERVER_MODE

if [[ "${OS:-}" == "Windows_NT" ]]; then
  PY="$ROOT/backend/.venv/Scripts/python.exe"
else
  PY="$ROOT/backend/.venv/bin/python"
fi

step() { printf '\n\033[1;36m== %s ==\033[0m\n' "$*"; }
need() { command -v "$1" >/dev/null 2>&1 || { echo "ERROR: '$1' is required but not on PATH." >&2; exit 1; }; }

need git; need npm
command -v zellij >/dev/null 2>&1 || echo "WARNING: zellij not found on PATH - agents cannot be launched until it is installed."

# --- run as administrator (Windows, opt-in) -----------------------------------
# With server.runAsAdministrator set, restart this script elevated (UAC prompt) before anything
# else, so the elevated copy is the one that stops a previous instance and starts the backend.
# Reading the config needs the venv, so the very first run starts unelevated.
wants_admin() {
  [[ "${OS:-}" == "Windows_NT" && -x "$PY" ]] || return 1
  [[ "$("$PY" -c "import sys; sys.path.insert(0, 'backend'); from agentflow.config import load_config; print(load_config().server.run_as_administrator)" 2>/dev/null)" == "True" ]]
}
if wants_admin && ! net session >/dev/null 2>&1; then
  if [[ "${1:-}" == "--elevated" ]]; then
    echo "WARNING: still not running as administrator; continuing as a normal user." >&2
  else
    step "Restarting as administrator (server.runAsAdministrator)"
    bash_exe="$(cygpath -w /bin/bash.exe)"
    script="$(cygpath -w "$ROOT/start.sh")"
    if powershell.exe -NoProfile -Command "Start-Process -Verb RunAs -FilePath '$bash_exe' -ArgumentList '--login \"$script\" --elevated'"; then
      exit 0
    fi
    echo "WARNING: could not start as administrator (UAC declined?); continuing as a normal user." >&2
  fi
fi

# --- stop a previous instance ------------------------------------------------
# Closing the window a previous run was in does not reliably kill the backend
# on Windows (it can survive as an orphan holding the port), so track our own
# pid across runs and stop it explicitly instead of hoping the OS did.
PIDFILE="$ROOT/.start.pid"

wait_for_exit() {
  local pid="$1"
  for _ in $(seq 1 20); do
    kill -0 "$pid" 2>/dev/null || return 0
    sleep 0.25
  done
  kill -9 "$pid" 2>/dev/null || true
}

if [[ -f "$PIDFILE" ]]; then
  old_pid="$(cat "$PIDFILE" 2>/dev/null || true)"
  if [[ -n "$old_pid" ]] && kill -0 "$old_pid" 2>/dev/null; then
    step "Stopping previous AgentFlow instance (pid $old_pid)"
    kill "$old_pid" 2>/dev/null || true
    wait_for_exit "$old_pid"
  fi
  rm -f "$PIDFILE"
fi
trap 'rm -f "$PIDFILE"' EXIT

# --- backend -----------------------------------------------------------------
if [[ ! -x "$PY" ]]; then
  step "Creating Python venv (backend/.venv)"
  if command -v uv >/dev/null 2>&1; then
    uv venv --python 3.13 "$ROOT/backend/.venv"
    uv pip install --python "$PY" -e "$ROOT/backend"
  else
    need python3
    python3 -m venv "$ROOT/backend/.venv"
    "$PY" -m pip install --upgrade pip >/dev/null
    "$PY" -m pip install -e "$ROOT/backend"
  fi
elif ! "$PY" -c "import agentflow" 2>/dev/null; then
  step "Installing backend package"
  if command -v uv >/dev/null 2>&1; then
    uv pip install --python "$PY" -e "$ROOT/backend"  # a venv made by uv has no pip
  else
    "$PY" -m pip install -e "$ROOT/backend"
  fi
fi

# --- frontend ----------------------------------------------------------------
cd "$ROOT/frontend"
if [[ ! -d node_modules ]]; then
  step "Installing frontend dependencies"
  npm install
  npm install-scripts approve esbuild >/dev/null 2>&1 || true
  npm rebuild esbuild >/dev/null 2>&1 || true
fi
if [[ ! -f dist/index.html ]] || [[ -n "$(find src index.html package.json -newer dist/index.html -print -quit 2>/dev/null)" ]]; then
  step "Building frontend"
  npm run build
fi
cd "$ROOT"

# --- configuration -----------------------------------------------------------
# Run the setup wizard first when there is no config.json yet (load_config would quietly write a
# default one), or when Jira is enabled without its settings and the backend would refuse to start.
# Only when there is someone to answer it; without a terminal, the backend's own message explains below.
if ! "$PY" -c "import sys; sys.path.insert(0, 'backend'); from agentflow.config import config_path, read_config, jira_startup_problem; p = config_path(); sys.exit(1 if not p.exists() or jira_startup_problem(read_config(p)) else 0)" 2>/dev/null; then
  if [[ -t 0 && -t 1 ]]; then
    step "AgentFlow is not set up yet - starting the setup wizard"
    "$PY" -m agentflow.main setup || true
  fi
fi

# --- run ---------------------------------------------------------------------
PORT="$("$PY" -c "import sys; sys.path.insert(0, 'backend'); from agentflow.config import load_config; print(load_config().server.port)")"
URL="http://127.0.0.1:${PORT}"

# Belt-and-braces: if something is still on the port (e.g. a pre-existing stale
# run from before the pidfile above existed), find and stop it directly rather
# than failing to bind.
port_pid() {
  if [[ "${OS:-}" == "Windows_NT" ]]; then
    netstat -ano 2>/dev/null | grep -i "LISTENING" | grep ":${PORT} " | awk '{print $NF}' | head -n1
  else
    lsof -ti "tcp:${PORT}" 2>/dev/null | head -n1
  fi
}
existing_pid="$(port_pid || true)"
if [[ -n "${existing_pid:-}" ]]; then
  step "Port $PORT is already in use (pid $existing_pid) - stopping it"
  if [[ "${OS:-}" == "Windows_NT" ]]; then
    taskkill //PID "$existing_pid" //F >/dev/null 2>&1 || true
  else
    kill -9 "$existing_pid" 2>/dev/null || true
  fi
  sleep 1
fi

step "Starting AgentFlow on $URL  (Ctrl+C to stop)"
(
  for _ in $(seq 1 40); do
    sleep 0.5
    if "$PY" -c "import urllib.request,sys; urllib.request.urlopen('$URL/api/health', timeout=1)" 2>/dev/null; then
      case "$(uname -s)" in
        MINGW*|MSYS*|CYGWIN*) start "" "$URL" ;;
        Darwin) open "$URL" ;;
        *) xdg-open "$URL" >/dev/null 2>&1 || true ;;
      esac
      break
    fi
  done
) &

cd "$ROOT/backend"
# $$ survives exec (it replaces this process's image, not its pid), so the pidfile
# written here still names the right process for the next run to find and stop.
echo $$ > "$PIDFILE"
exec "$PY" -m agentflow.main
