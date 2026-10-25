from agentflow.config import Config


def test_branch_prefix_defaults_to_empty_and_can_be_configured():
    assert Config().workspace.branch_prefix == ""
    empty_prefix = Config.model_validate({"workspace": {"branchPrefix": ""}})
    custom_prefix = Config.model_validate({"workspace": {"branchPrefix": "agent/"}})
    assert empty_prefix.workspace.branch_prefix == ""
    assert custom_prefix.workspace.branch_prefix == "agent/"


def test_default_repository_directory_defaults_to_none_and_can_be_configured():
    assert Config().workspace.default_repository_directory is None
    configured = Config.model_validate({"workspace": {"defaultRepositoryDirectory": "D:/Repos/Acme"}})
    assert configured.workspace.default_repository_directory == "D:/Repos/Acme"


def test_example_config_is_valid_portable_and_matches_the_built_in_harnesses():
    from pathlib import Path

    import sys

    from agentflow.config import default_harnesses, read_config
    from agentflow.doctor import ROLES
    from agentflow.workflow.usage import resolve_usage_command

    example = Path(__file__).resolve().parents[2] / "config.example.json"
    config = read_config(example)  # parses, validates and resolves every role's harness
    for role in ROLES:
        assert config.harnesses[config.agents.for_role(role).harness].enabled, role
    # Only the token is left to fill in; setup (or the user) supplies it.
    assert config.jira.missing() == ["jira.apiToken"]
    for key, built_in in default_harnesses().items():
        harness = config.harnesses[key]
        # Portable: `python` and the relative script resolve against this install at run time.
        assert harness.usage_command == ["python", f"scripts/usage_{harness.type}.py"]
        resolved = resolve_usage_command(harness.usage_command)
        assert resolved[0] == sys.executable and Path(resolved[1]).is_file()
        assert (harness.default_model, harness.available_models) == (built_in.default_model, built_in.available_models)
        assert harness.args == built_in.args, key


def test_run_as_administrator_defaults_off_and_can_be_configured():
    assert Config().server.run_as_administrator is False
    assert Config.model_validate({"server": {"runAsAdministrator": True}}).server.run_as_administrator is True


def test_worktree_setup_is_accepted_and_validated_like_the_rust_core():
    import pytest
    from pydantic import ValidationError

    assert Config().workspace.worktree_setup == {}
    setup = {"copy": [".env", "apps/web/.env.local"], "commands": [["npm", "ci"]], "timeoutSeconds": 600}
    configured = Config.model_validate({"workspace": {"worktreeSetup": {"D:/Dev/repo": setup}}})
    assert configured.workspace.worktree_setup["D:/Dev/repo"].copy_paths == [".env", "apps/web/.env.local"]
    assert configured.model_dump(by_alias=True)["workspace"]["worktreeSetup"]["D:/Dev/repo"] == setup
    for bad in [{"copy": ["../x"]}, {"copy": ["C:/x"]}, {"copy": ["/x"]}, {"copy": [".git/hooks"]},
                {"commands": [[]]}, {"timeoutSeconds": 0}, {"hooks": []}]:
        with pytest.raises(ValidationError):
            Config.model_validate({"workspace": {"worktreeSetup": {"D:/r": bad}}})
