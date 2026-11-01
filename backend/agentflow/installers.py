"""Installing missing tools for `agentflow setup`, only ever after the user says yes.

Every command comes from the tool's own documentation, and each winget package was checked to download
from the vendor's own releases. Installs run in the
foreground so the package manager can prompt (sudo, UAC, licence terms). Where there is no dependable
automated route (Node.js, gh and dnf's ffmpeg on Linux) the wizard gives the download page instead.
"""

from __future__ import annotations

import hashlib
import io as _io
import os
import platform
import subprocess
import sys
import tarfile
import urllib.request
from pathlib import Path

from .services.process import resolve_executable

ROOT = Path(__file__).resolve().parents[2]

WINGET = {"git": "Git.Git", "node": "OpenJS.NodeJS.LTS", "zellij": "Zellij.Zellij", "gh": "GitHub.cli",
          "ffmpeg": "Gyan.FFmpeg", "claude": "Anthropic.ClaudeCode", "codex": "OpenAI.Codex",
          "copilot": "GitHub.Copilot"}
BREW = {"git": ["git"], "node": ["node"], "zellij": ["zellij"], "gh": ["gh"], "ffmpeg": ["ffmpeg"],
        "claude": ["--cask", "claude-code"], "codex": ["--cask", "codex"], "copilot": ["--cask", "copilot-cli"]}
LINUX_PACKAGES = {"git": {"apt": "git", "dnf": "git", "pacman": "git"}, "ffmpeg": {"apt": "ffmpeg", "pacman": "ffmpeg"}}
# The vendors' own install scripts, as their install docs give them; each installs into ~/.local/bin
# for a non-root user. Codex on Linux stays manual: its README documents npm and brew, and no script
# URL could be confirmed from a primary source.
LINUX_SCRIPTS = {"claude": "curl -fsSL https://claude.ai/install.sh | bash",
                 "copilot": "curl -fsSL https://gh.io/copilot-install | bash"}
MANUAL = {
    "git": "https://git-scm.com/downloads",
    "node": "https://nodejs.org/en/download (version 22 or newer)",
    "zellij": "https://zellij.dev/documentation/installation.html (version 0.44 or newer)",
    "gh": "https://github.com/cli/cli#installation",
    "ffmpeg": "https://ffmpeg.org/download.html",
    "claude": "https://code.claude.com/docs/en/setup",
    "codex": "https://github.com/openai/codex#installation",
    "copilot": "https://docs.github.com/en/copilot/how-tos/copilot-cli/set-up-copilot-cli/install-copilot-cli",
}
ZELLIJ_RELEASE = "https://github.com/zellij-org/zellij/releases/latest/download/zellij-{arch}-unknown-linux-musl"


def system() -> str:
    if sys.platform == "win32":
        return "windows"
    return "macos" if sys.platform == "darwin" else "linux"


def package_manager() -> str | None:
    candidates = {"windows": [("winget", "winget")], "macos": [("brew", "brew")],
                  "linux": [("apt", "apt-get"), ("dnf", "dnf"), ("pacman", "pacman")]}[system()]
    return next((name for name, command in candidates if resolve_executable(command)), None)


def _root(argv: list[str]) -> list[str]:
    return argv if not hasattr(os, "geteuid") or os.geteuid() == 0 else ["sudo", *argv]


def install_commands(tool: str, *, upgrade: bool = False) -> list[list[str]] | None:
    """The commands that install (or, for a too-old tool, upgrade) `tool` here; None when there is no
    automated route and the user should follow MANUAL[tool]."""
    manager, where = package_manager(), system()
    if manager == "winget" and tool in WINGET:
        # winget install upgrades a package that is already installed.
        return [["winget", "install", "--id", WINGET[tool], "--exact", "--source", "winget"]]
    if manager == "brew" and tool in BREW:
        return [["brew", "upgrade" if upgrade else "install", *BREW[tool]]]
    if where != "linux":
        return None
    if tool in LINUX_SCRIPTS:
        return [["bash", "-c", LINUX_SCRIPTS[tool]]]
    package = LINUX_PACKAGES.get(tool, {}).get(manager or "")
    if package is None:
        return None
    if manager == "apt":
        return [_root(["apt-get", "update"]), _root(["apt-get", "install", "-y", package])]
    if manager == "dnf":
        return [_root(["dnf", "install", "-y", package])]
    return [_root(["pacman", "-S", "--needed", "--noconfirm", package])]


def offer_install(io, tool: str, name: str, *, upgrade: bool = False) -> bool:
    """Show how `tool` would be installed and do it if the user agrees. True once it installed."""
    if tool == "zellij" and system() == "linux":
        # Distro packages are often older than 0.44 (§1), so use the official release binary.
        return offer_zellij_binary(io)
    commands = install_commands(tool, upgrade=upgrade)
    if not commands:
        io.say(f"   Install {name} by hand: {MANUAL[tool]}")
        return False
    io.say(f"   AgentFlow can {'upgrade' if upgrade else 'install'} {name} with:")
    for argv in commands:
        io.say(f"     {' '.join(argv)}")
    if not io.confirm(f"   {'Upgrade' if upgrade else 'Install'} {name} now?", False):
        return False
    for argv in commands:
        if not run_foreground(io, argv):
            io.say(f"   Other ways to install it: {MANUAL[tool]}")
            return False
    refresh_path(io)
    return True


def run_foreground(io, argv: list[str]) -> bool:
    try:
        code = subprocess.run(argv).returncode
    except KeyboardInterrupt:
        io.say("\n   Interrupted")
        return False
    except OSError as exc:
        io.say(f"   [warn] {exc}")
        return False
    if code != 0:
        io.say(f"   [warn] `{' '.join(argv)}` failed (exit {code})")
    return code == 0


# PATH -----------------------------------------------------------------------------------
def refresh_path(io) -> list[str]:
    """Put directories a fresh install added on this process's PATH, so the re-check finds the tool.
    Windows installers write the registry, which a running process never re-reads; the Linux and
    macOS installers used here put binaries in ~/.local/bin."""
    wanted = _registry_path() if system() == "windows" else [str(Path.home() / ".local" / "bin")]
    current = os.environ.get("PATH", "").split(os.pathsep)
    known = {os.path.normcase(os.path.normpath(entry)) for entry in current if entry}
    added = [entry for entry in wanted if entry and Path(entry).is_dir()
             and os.path.normcase(os.path.normpath(entry)) not in known]
    if added:
        os.environ["PATH"] = os.pathsep.join([*current, *added])
        if system() != "windows":
            io.say(f"   Added {', '.join(added)} to PATH for this setup. Add it to your shell profile too "
                   f"(e.g. export PATH=\"$HOME/.local/bin:$PATH\" in ~/.bashrc), or AgentFlow won't find it later")
    return added


def _registry_path() -> list[str]:
    import winreg

    entries = []
    for root, key in ((winreg.HKEY_LOCAL_MACHINE, r"SYSTEM\CurrentControlSet\Control\Session Manager\Environment"),
                      (winreg.HKEY_CURRENT_USER, "Environment")):
        try:
            with winreg.OpenKey(root, key) as handle:
                value, _ = winreg.QueryValueEx(handle, "Path")
        except OSError:
            continue
        entries += [winreg.ExpandEnvironmentStrings(part) for part in str(value).split(";") if part]
    return entries


# zellij on Linux ------------------------------------------------------------------------
def zellij_arch() -> str | None:
    return {"x86_64": "x86_64", "amd64": "x86_64", "aarch64": "aarch64", "arm64": "aarch64"}.get(
        platform.machine().lower())


def offer_zellij_binary(io) -> bool:
    arch = zellij_arch()
    if not arch:
        io.say(f"   No zellij release binary for {platform.machine()}; install it by hand: {MANUAL['zellij']}")
        return False
    url = ZELLIJ_RELEASE.format(arch=arch)
    target = Path.home() / ".local" / "bin" / "zellij"
    io.say(f"   AgentFlow can download the latest zellij release:\n     {url}.tar.gz\n"
           f"   check it against {url}.sha256sum, and put it in {target}")
    if not io.confirm("   Install zellij now?", False):
        return False
    try:
        binary = extract_zellij(download(f"{url}.tar.gz"), download(f"{url}.sha256sum").decode())
    except (OSError, ValueError, tarfile.TarError) as exc:
        io.say(f"   [warn] could not install zellij: {exc}")
        return False
    older = resolve_executable("zellij")  # the one the user's PATH picks today, e.g. a distro's 0.43
    target.parent.mkdir(parents=True, exist_ok=True)
    temporary = target.with_name("zellij.download")
    temporary.write_bytes(binary)
    temporary.chmod(0o755)
    os.replace(temporary, target)
    io.say(f"   Installed {target}")
    # First on PATH for the rest of setup, so the re-check sees this binary and not an older one.
    entries = [e for e in os.environ.get("PATH", "").split(os.pathsep) if e and Path(e) != target.parent]
    os.environ["PATH"] = os.pathsep.join([str(target.parent), *entries])
    if older and Path(older).resolve() != target.resolve():
        io.say(f"   [warn] {older} comes first on your PATH, so AgentFlow would still run that one. Remove it, "
               f"or put {target.parent} first in your shell profile, or set zellij.command to {target}")
    else:
        io.say(f"   Make sure {target.parent} is on PATH in your shell profile "
               f"(e.g. export PATH=\"$HOME/.local/bin:$PATH\" in ~/.bashrc)")
    return True


def download(url: str) -> bytes:
    with urllib.request.urlopen(url, timeout=120) as response:
        return response.read()


def extract_zellij(archive: bytes, checksum_file: str) -> bytes:
    """The zellij binary from a release tarball. Its .sha256sum is the hash of the binary itself
    ("<hex>  target/<triple>/release/zellij"), not of the tarball."""
    expected = checksum_file.split()[0].lower() if checksum_file.split() else ""
    with tarfile.open(fileobj=_io.BytesIO(archive), mode="r:gz") as tar:
        member = next((m for m in tar.getmembers() if m.isfile() and Path(m.name).name == "zellij"), None)
        if member is None:
            raise ValueError("the release archive has no zellij binary")
        binary = tar.extractfile(member).read()
    if hashlib.sha256(binary).hexdigest() != expected:
        raise ValueError("the checksum does not match the download")
    return binary


# Python extras --------------------------------------------------------------------------
def extra_install_command(extra: str) -> list[str] | None:
    """Install an optional dependency group into the running backend venv. Venvs made by `uv venv`
    (start.sh, the README, setup.sh) have no pip, so prefer uv whenever it exists."""
    target = f"{ROOT / 'backend'}[{extra}]"
    if uv := resolve_executable("uv"):
        return [uv, "pip", "install", "--python", sys.executable, "-e", target]
    try:
        import pip  # noqa: F401
    except ImportError:
        return None
    return [sys.executable, "-m", "pip", "install", "-e", target]
