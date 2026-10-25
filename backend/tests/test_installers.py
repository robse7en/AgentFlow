import hashlib
import io
import os
import subprocess
import sys
import tarfile
from pathlib import Path

import pytest

from agentflow import doctor, installers, setup_wizard
from agentflow.config import Config

# Reuse the scripted prompts from the wizard tests.
from test_setup_wizard import Scripted


@pytest.fixture
def on(monkeypatch):
    """Pretend to be on `system` with `manager` (never the real machine)."""
    def pretend(system, manager, root=False):
        monkeypatch.setattr(installers, "system", lambda: system)
        monkeypatch.setattr(installers, "package_manager", lambda: manager)
        monkeypatch.setattr(os, "geteuid", lambda: 0 if root else 1000, raising=False)
    return pretend


class Calls(list):
    pass


@pytest.fixture
def ran(monkeypatch):
    """Records the commands run; `.code` is the exit code they return."""
    calls = Calls()
    calls.code = 0

    def run(argv, **kwargs):
        calls.append(argv)
        return subprocess.CompletedProcess(argv, calls.code)
    monkeypatch.setattr(installers.subprocess, "run", run)
    monkeypatch.setattr(installers, "refresh_path", lambda io: [])
    return calls


# which command, where -------------------------------------------------------------------
@pytest.mark.parametrize("tool, package", [("git", "Git.Git"), ("zellij", "Zellij.Zellij"),
                                           ("codex", "OpenAI.Codex"), ("claude", "Anthropic.ClaudeCode")])
def test_windows_uses_winget_with_the_exact_id(on, tool, package):
    on("windows", "winget")
    assert installers.install_commands(tool) == [["winget", "install", "--id", package, "--exact", "--source", "winget"]]


def test_macos_uses_brew_formulas_and_casks(on):
    on("macos", "brew")
    assert installers.install_commands("zellij") == [["brew", "install", "zellij"]]
    assert installers.install_commands("zellij", upgrade=True) == [["brew", "upgrade", "zellij"]]
    assert installers.install_commands("claude") == [["brew", "install", "--cask", "claude-code"]]
    assert installers.install_commands("copilot") == [["brew", "install", "--cask", "copilot-cli"]]


def test_linux_apt_updates_first_and_uses_sudo_unless_root(on):
    on("linux", "apt")
    assert installers.install_commands("git") == [["sudo", "apt-get", "update"],
                                                  ["sudo", "apt-get", "install", "-y", "git"]]
    on("linux", "apt", root=True)
    assert installers.install_commands("ffmpeg")[1] == ["apt-get", "install", "-y", "ffmpeg"]


def test_linux_harnesses_use_the_vendor_scripts(on):
    on("linux", "dnf")
    assert installers.install_commands("claude") == [["bash", "-c", "curl -fsSL https://claude.ai/install.sh | bash"]]


@pytest.mark.parametrize("tool, manager", [("node", "apt"), ("gh", "apt"), ("ffmpeg", "dnf"), ("git", None)])
def test_linux_without_a_dependable_package_is_manual(on, tool, manager):
    on("linux", manager)
    assert installers.install_commands(tool) is None


def test_windows_without_winget_is_manual(on):
    on("windows", None)
    assert installers.install_commands("git") is None


# offering -------------------------------------------------------------------------------
def test_declining_installs_nothing(on, ran):
    on("windows", "winget")
    prompts = Scripted("")  # Enter = the default, no
    assert installers.offer_install(prompts, "git", "Git") is False
    assert ran == [] and "winget install --id Git.Git" in prompts.output


def test_accepting_runs_every_command_in_order(on, ran):
    on("linux", "apt")
    assert installers.offer_install(Scripted("y"), "git", "Git") is True
    assert ran == [["sudo", "apt-get", "update"], ["sudo", "apt-get", "install", "-y", "git"]]


def test_a_failed_install_stops_and_points_to_the_manual_route(on, ran):
    on("linux", "apt")
    ran.code = 100
    prompts = Scripted("y")
    assert installers.offer_install(prompts, "git", "Git") is False
    assert len(ran) == 1 and "exit 100" in prompts.output and "git-scm.com" in prompts.output


def test_manual_tools_get_a_link_and_no_prompt(on, ran):
    on("linux", "apt")
    prompts = Scripted()
    assert installers.offer_install(prompts, "node", "Node.js") is False
    assert ran == [] and "nodejs.org" in prompts.output


# PATH -----------------------------------------------------------------------------------
def test_refresh_path_adds_new_registry_entries_on_windows(monkeypatch, tmp_path):
    new = tmp_path / "Git" / "cmd"
    new.mkdir(parents=True)
    monkeypatch.setattr(installers, "system", lambda: "windows")
    monkeypatch.setattr(installers, "_registry_path", lambda: [str(new), str(tmp_path / "missing")])
    monkeypatch.setenv("PATH", str(tmp_path))
    assert installers.refresh_path(Scripted()) == [str(new)]
    assert os.environ["PATH"].split(os.pathsep)[-1] == str(new)
    assert installers.refresh_path(Scripted()) == []  # already there


def test_refresh_path_adds_local_bin_elsewhere_and_says_so(monkeypatch, tmp_path):
    (tmp_path / ".local" / "bin").mkdir(parents=True)
    monkeypatch.setattr(installers, "system", lambda: "linux")
    monkeypatch.setattr(installers.Path, "home", lambda: tmp_path)
    monkeypatch.setenv("PATH", "/usr/bin")
    prompts = Scripted()
    assert installers.refresh_path(prompts) == [str(tmp_path / ".local" / "bin")]
    assert "shell profile" in prompts.output


# zellij on Linux ------------------------------------------------------------------------
def _release(binary=b"\x7fELF zellij", name="zellij"):
    buffer = io.BytesIO()
    with tarfile.open(fileobj=buffer, mode="w:gz") as tar:
        info = tarfile.TarInfo(name)
        info.size = len(binary)
        tar.addfile(info, io.BytesIO(binary))
    checksum = f"{hashlib.sha256(binary).hexdigest()}  target/x86_64-unknown-linux-musl/release/zellij\n"
    return buffer.getvalue(), checksum


def test_zellij_binary_is_checked_against_its_own_hash():
    archive, checksum = _release()
    assert installers.extract_zellij(archive, checksum) == b"\x7fELF zellij"
    with pytest.raises(ValueError, match="checksum"):
        installers.extract_zellij(archive, "0" * 64 + "  zellij")
    with pytest.raises(ValueError, match="no zellij binary"):
        installers.extract_zellij(_release(name="README")[0], checksum)


def test_zellij_binary_installs_into_local_bin(monkeypatch, tmp_path):
    archive, checksum = _release()
    monkeypatch.setattr(installers, "system", lambda: "linux")
    monkeypatch.setattr(installers, "zellij_arch", lambda: "x86_64")
    monkeypatch.setattr(installers.Path, "home", lambda: tmp_path)
    fetched = []

    def download(url):
        fetched.append(url)
        return archive if url.endswith(".tar.gz") else checksum.encode()
    monkeypatch.setattr(installers, "download", download)
    monkeypatch.setattr(installers, "refresh_path", lambda io: [])
    assert installers.offer_install(Scripted("y"), "zellij", "Zellij") is True
    target = tmp_path / ".local" / "bin" / "zellij"
    assert target.read_bytes() == b"\x7fELF zellij"
    assert fetched[0] == ("https://github.com/zellij-org/zellij/releases/latest/download/"
                          "zellij-x86_64-unknown-linux-musl.tar.gz")
    if os.name != "nt":
        assert target.stat().st_mode & 0o777 == 0o755


def test_declining_the_zellij_download_fetches_nothing(monkeypatch):
    monkeypatch.setattr(installers, "system", lambda: "linux")
    monkeypatch.setattr(installers, "zellij_arch", lambda: "aarch64")
    monkeypatch.setattr(installers, "download", lambda url: pytest.fail("downloaded without consent"))
    assert installers.offer_install(Scripted(""), "zellij", "Zellij") is False


# Python extras --------------------------------------------------------------------------
def test_extras_install_with_uv_because_uv_venvs_have_no_pip(monkeypatch):
    monkeypatch.setattr(installers, "resolve_executable", lambda command: "/bin/uv" if command == "uv" else None)
    command = installers.extra_install_command("slack")
    assert command[:5] == ["/bin/uv", "pip", "install", "--python", sys.executable]
    assert command[-1].endswith("backend[slack]")


def test_no_uv_and_no_pip_means_no_command(monkeypatch):
    monkeypatch.setattr(installers, "resolve_executable", lambda command: None)
    monkeypatch.setitem(sys.modules, "pip", None)  # import pip now raises ImportError
    assert installers.extra_install_command("slack") is None


# wizard ---------------------------------------------------------------------------------
@pytest.mark.asyncio
async def test_wizard_offers_to_install_a_missing_tool_once_then_rechecks(monkeypatch):
    results = iter([[doctor.Check("Git", "fail", "'git' not found on PATH"), doctor.Check("UI", "ok", "built")],
                    [doctor.Check("Git", "ok", "git 2"), doctor.Check("UI", "ok", "built")]])

    async def tools(_config):
        return next(results)
    offered = []

    def offer(io, tool, name, upgrade=False):
        offered.append((tool, upgrade))
        return True
    monkeypatch.setattr(doctor, "_tools", tools)
    monkeypatch.setattr(installers, "offer_install", offer)
    await setup_wizard.prerequisites(Scripted(), Config().model_dump(by_alias=True))
    assert offered == [("git", False)]


@pytest.mark.asyncio
async def test_too_old_zellij_is_offered_as_an_upgrade(monkeypatch):
    checks = [doctor.Check("Zellij", "fail", "zellij 0.43.0 is too old; needs 0.44 or newer"),
              doctor.Check("UI", "ok", "built")]

    async def tools(_config):
        return checks
    offered = []
    monkeypatch.setattr(doctor, "_tools", tools)
    monkeypatch.setattr(installers, "offer_install", lambda io, tool, name, upgrade=False: offered.append(upgrade) or False)
    await setup_wizard.prerequisites(Scripted("skip"), Config().model_dump(by_alias=True))
    assert offered == [True]  # and not offered again after the user declined


def test_an_older_zellij_first_on_path_is_called_out(monkeypatch, tmp_path):
    archive, checksum = _release()
    monkeypatch.setattr(installers, "system", lambda: "linux")
    monkeypatch.setattr(installers, "zellij_arch", lambda: "x86_64")
    monkeypatch.setattr(installers.Path, "home", lambda: tmp_path)
    monkeypatch.setattr(installers, "download", lambda url: archive if url.endswith(".tar.gz") else checksum.encode())
    monkeypatch.setattr(installers, "resolve_executable", lambda command: "/usr/bin/zellij")
    monkeypatch.setenv("PATH", os.pathsep.join(["/usr/bin", str(tmp_path / ".local" / "bin")]))
    prompts = Scripted("y")
    assert installers.offer_install(prompts, "zellij", "Zellij") is True
    assert "/usr/bin/zellij comes first on your PATH" in prompts.output
    # This setup run now finds the new binary first, and only once.
    entries = os.environ["PATH"].split(os.pathsep)
    assert entries[0] == str(tmp_path / ".local" / "bin") and entries.count(entries[0]) == 1


def test_codex_on_linux_is_manual(on):
    on("linux", "apt")
    assert installers.install_commands("codex") is None


def test_slack_extra_install_asks_first(monkeypatch):
    monkeypatch.setattr(installers, "extra_install_command", lambda extra: ["/bin/uv", "pip", "install", "x[slack]"])
    ran = []
    monkeypatch.setattr(installers, "run_foreground", lambda io, argv: ran.append(argv) or True)
    setup_wizard.install_slack_extra(Scripted(""))
    assert ran == []
    prompts = Scripted("y")
    setup_wizard.install_slack_extra(prompts)
    assert ran == [["/bin/uv", "pip", "install", "x[slack]"]] and "Installed" in prompts.output


def test_slack_extra_without_uv_or_pip_explains(monkeypatch):
    monkeypatch.setattr(installers, "extra_install_command", lambda extra: None)
    prompts = Scripted()
    setup_wizard.install_slack_extra(prompts)
    assert "neither uv nor pip" in prompts.output
