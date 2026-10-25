#!/usr/bin/env bash
set -euo pipefail

usage() {
    echo "Usage: bash scripts/rebuild-desktop.sh [--release]"
    echo "Build the Windows desktop app using Git Bash (debug by default)."
}

profile=debug
desktop_args=()
case "${1:-}" in
    --help|-h) usage; exit 0 ;;
    --release) profile=release; desktop_args+=(--release); shift ;;
    "") ;;
    *) usage >&2; exit 2 ;;
esac
if (( $# > 0 )); then
    usage >&2
    exit 2
fi

case "$(uname -s)" in
    MINGW*|MSYS*|CYGWIN*) ;;
    *)
        echo "This Windows desktop build requires Git Bash, not WSL." >&2
        echo 'From PowerShell, run .\scripts\rebuild-desktop.ps1 (add -Release for release).' >&2
        exit 1
        ;;
esac

# Git Bash may omit Rustup's user-level bin directory from its login PATH.
if [[ -n "${USERPROFILE:-}" ]]; then
    cargo_bin="$(cygpath -u "$USERPROFILE")/.cargo/bin"
    if [[ -x "$cargo_bin/cargo.exe" ]]; then
        export PATH="$cargo_bin:$PATH"
    fi
fi

repo_root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
for tool in node npm cargo; do
    if ! command -v "$tool" >/dev/null 2>&1; then
        echo "Missing $tool on PATH. Install the build prerequisites and reopen Git Bash." >&2
        exit 1
    fi
done
if [[ ! -d "$repo_root/frontend/node_modules" ]]; then
    echo "Install frontend dependencies first: cd \"$repo_root/frontend\" && npm ci" >&2
    exit 1
fi

cd -- "$repo_root"
echo "[1/3] Building frontend..."
npm --prefix "$repo_root/frontend" run build

# The desktop resource map expects release flowctl in app/target/release.
# Explicit target directories keep that true even with CARGO_TARGET_DIR set.
echo "[2/3] Building Rust backend and release flowctl..."
cargo build --locked --release --workspace \
    --manifest-path "$repo_root/app/Cargo.toml" --target-dir "$repo_root/app/target"

echo "[3/3] Building desktop ($profile)..."
cargo build --locked "${desktop_args[@]}" \
    --manifest-path "$repo_root/app/src-tauri/Cargo.toml" \
    --target-dir "$repo_root/app/src-tauri/target"

echo "Desktop built: $repo_root/app/src-tauri/target/$profile/agentflow-desktop.exe"
