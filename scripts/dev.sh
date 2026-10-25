#!/bin/sh
# Start backend (with auto-reload) and the Vite dev server; Ctrl-C stops both.
root="$(cd "$(dirname "$0")/.." && pwd)"
(cd "$root/backend" && .venv/bin/python -m agentflow.main --reload) &
(cd "$root/frontend" && npm run dev) &
wait
