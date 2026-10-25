#!/usr/bin/env bash
# Guided setup for AgentFlow on macOS and Linux (Git Bash on Windows works too, but setup.ps1 can
# also install uv there).
#
# Gets Python ready and installs the backend, then hands over to `agentflow setup`, which checks
# and offers to install everything else and writes config.json. Nothing is installed without a yes.
# Arguments go to `agentflow setup` (e.g. ./setup.sh --config ~/agentflow.json).
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")"
ROOT="$(pwd)"

if [[ "${OS:-}" == "Windows_NT" ]]; then
  PY="$ROOT/backend/.venv/Scripts/python.exe"
else
  PY="$ROOT/backend/.venv/bin/python"
fi

step() { printf '\n\033[1;36m== %s ==\033[0m\n' "$*"; }
confirm() {
  local reply
  read -r -p "$1 [y/N] " reply || true
  [[ "$reply" =~ ^[Yy] ]]
}

# The first Python 3.12+ on PATH, if any.
find_python() {
  local candidate
  for candidate in python3.13 python3.12 python3 python; do
    if command -v "$candidate" >/dev/null 2>&1 &&
       "$candidate" -c 'import sys; sys.exit(0 if sys.version_info >= (3, 12) else 1)' 2>/dev/null; then
      echo "$candidate"
      return 0
    fi
  done
  return 1
}

# --- Python and the backend venv -----------------------------------------------
if [[ ! -x "$PY" ]]; then
  if ! command -v uv >/dev/null 2>&1 && ! PYTHON="$(find_python)"; then
    if [[ "${OS:-}" == "Windows_NT" ]]; then
      echo "AgentFlow needs Python 3.12 or newer, or uv. Run setup.ps1 from PowerShell, which can install uv:" >&2
      echo "  powershell -ExecutionPolicy Bypass -File setup.ps1" >&2
      exit 1
    fi
    echo "AgentFlow needs Python 3.12 or newer. uv (https://docs.astral.sh/uv/) can provide it, installed with:"
    echo "  curl -LsSf https://astral.sh/uv/install.sh | sh"
    confirm "Install uv now?" || { echo "Install Python 3.12+ or uv, then run setup.sh again." >&2; exit 1; }
    curl -LsSf https://astral.sh/uv/install.sh | sh
    # uv's installer puts it here; this shell's PATH predates it.
    export PATH="${XDG_BIN_HOME:-$HOME/.local/bin}:$PATH"
  fi
  step "Creating the Python venv (backend/.venv)"
  if command -v uv >/dev/null 2>&1; then
    uv venv --python 3.13 "$ROOT/backend/.venv"  # downloads Python 3.13 if needed
  else
    "$PYTHON" -m venv "$ROOT/backend/.venv"
  fi
fi

if ! "$PY" -c "import agentflow" 2>/dev/null; then
  step "Installing the backend"
  if command -v uv >/dev/null 2>&1; then
    uv pip install --python "$PY" -e "$ROOT/backend"
  else
    "$PY" -m pip install -e "$ROOT/backend"  # a venv made by `python -m venv` has pip
  fi
fi

# --- the wizard ----------------------------------------------------------------
exec "$PY" -m agentflow.main setup "$@"
