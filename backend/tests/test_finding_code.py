"""The code a review finding points at: diff parsing, line mapping and the views over a real repository."""
import asyncio
import subprocess
from types import SimpleNamespace

import pytest

from agentflow.services.git_workspace import GitService
from agentflow.workflow.finding_code import finding_code, map_line, parse_hunks, repository_path, split_lines, touches

DIFF = ("diff --git a/f b/f\nindex 1..2 100644\n--- a/f\n+++ b/f\n"
        "@@ -2,4 +2,5 @@ fn main\n a\n-b\n+B1\n+B2\n c\n d\n"
        "@@ -20,0 +22,2 @@\n+x\n+y\n\\ No newline at end of file\n")


def test_paths_stay_inside_the_repository():
    assert repository_path(" ./src\\a.rs ") == "src/a.rs"
    for bad in ["", "/etc/passwd", "C:/x", "a/../../b", ".."]:
        assert repository_path(bad) is None, bad
    roots = ("D:\\work\\tree\\", "/repo")
    assert repository_path("d:/Work/tree/src/a.rs", roots) == "src/a.rs"
    assert repository_path("/repo/b.py", roots) == "b.py"
    assert repository_path("/repository/b.py", roots) is None


def test_lines_drop_their_endings():
    assert split_lines("a\r\nb\n") == ["a", "b"]
    assert split_lines("a\n\nb") == ["a", "", "b"]
    assert split_lines("") == []


def test_hunks_number_both_sides():
    hunks = parse_hunks(DIFF)
    assert len(hunks) == 2
    assert hunks[0]["header"] == "@@ -2,4 +2,5 @@ fn main"
    assert [(r["kind"], r["old"], r["new"]) for r in hunks[0]["rows"]] == [
        ("context", 2, 2), ("del", 3, None), ("add", None, 3), ("add", None, 4), ("context", 4, 5), ("context", 5, 6)]
    assert len(hunks[1]["rows"]) == 2
    assert parse_hunks("@@ \n@@ -x +1 @@\n+a\n") == []


def test_lines_map_through_the_change():
    hunks = parse_hunks(DIFF)
    assert [map_line(hunks, n) for n in (1, 2, 3, 4, 10, 20, 21)] == [1, 2, 3, 5, 11, 21, 24]
    assert touches(hunks, 3, 3) and touches(hunks, 1, 5) and not touches(hunks, 4, 19)
    # x and y are added after old line 20: inside 20..21, not at either end of a range.
    assert touches(hunks, 20, 21) and not touches(hunks, 21, 30) and not touches(hunks, 10, 20)


def git(repo, *args):
    result = subprocess.run(["git", "-C", str(repo), *args], capture_output=True, text=True, check=True)
    return result.stdout.strip()


def commit(repo, text, message):
    (repo / "src").mkdir(exist_ok=True)
    (repo / "src" / "app.txt").write_text(text, newline="\n")
    git(repo, "add", "-A")
    git(repo, "commit", "-qm", message)
    return git(repo, "rev-parse", "HEAD")


@pytest.fixture
def repo(tmp_path):
    repo = tmp_path / "repo"
    repo.mkdir()
    git(repo, "init", "-q", "-b", "main")
    for key, value in {"commit.gpgSign": "false", "user.name": "Test", "user.email": "test@localhost",
                       "core.autocrlf": "false", "core.hooksPath": str(tmp_path / "hooks")}.items():
        git(repo, "config", key, value)
    lines = [f"line {n}" for n in range(1, 11)]
    commit(repo, "\n".join(lines) + "\n", "base")
    git(repo, "switch", "-qc", "task")
    lines[4:5] = ["changed 5a", "changed 5b"]
    reviewed = commit(repo, "\n".join(lines) + "\n", "task change")
    lines[0:0] = ["header"]
    lines[5:7] = ["fixed 5"]
    fixed = commit(repo, "\n".join(lines) + "\n", "fix")
    return SimpleNamespace(path=repo, reviewed=reviewed, fixed=fixed)


def views(repo, current, **finding):
    task = SimpleNamespace(repository_root=str(repo.path), worktree_path="", base_branch="main",
                           current_sha=current)
    fields = {"file": "src/app.txt", "line": 5, "end_line": 6, "reviewed_sha": repo.reviewed, **finding}
    return asyncio.run(finding_code(GitService(), task, SimpleNamespace(**fields)))


def test_views_show_the_reviewed_code_the_task_change_and_where_it_is_now(repo):
    data = views(repo, repo.fixed)
    assert data["unavailable"] == {}
    assert data["snapshot"]["lines"][4:6] == ["changed 5a", "changed 5b"]
    assert [(r["kind"], r["text"]) for r in data["diff"]["hunks"][0]["rows"] if r["kind"] != "context"] == [
        ("del", "line 5"), ("add", "changed 5a"), ("add", "changed 5b")]
    now = data["now"]
    assert (now["sha"], now["line"], now["endLine"], now["changed"]) == (repo.fixed, 6, 6, True)
    assert now["lines"][5] == "fixed 5"
    assert now["hunks"]


def test_unchanged_since_review_reuses_the_snapshot(repo):
    now = views(repo, repo.reviewed, line=2, end_line=None)["now"]
    assert (now["line"], now["endLine"], now["changed"], now["hunks"]) == (2, None, False, [])


def test_a_fast_forwarded_branch_has_no_task_diff(repo):
    git(repo.path, "switch", "-q", "main")
    git(repo.path, "merge", "-q", "--ff-only", "task")
    data = views(repo, repo.fixed)
    assert data["diff"] is None and data["snapshot"] and data["now"]
    assert data["unavailable"] == {"diff": "The task branch is already part of main"}


def test_a_branch_merged_with_a_merge_commit_still_shows_the_task_diff(repo):
    git(repo.path, "switch", "-q", "main")
    commit(repo.path, "\n".join(f"line {n}" for n in range(1, 11)) + "\nmain only\n", "main moves on")
    git(repo.path, "merge", "-q", "--no-ff", "-m", "merge task", "task")
    data = views(repo, repo.fixed)
    assert data["unavailable"] == {}
    assert data["diff"]["from"] == git(repo.path, "rev-parse", "main~1~1")
    assert [r["text"] for r in data["diff"]["hunks"][0]["rows"] if r["kind"] == "add"] == ["changed 5a", "changed 5b"]


def test_missing_or_unsafe_files_explain_why(repo):
    message = f"src/gone.txt is not in {repo.reviewed[:7]}"
    assert views(repo, repo.fixed, file="src/gone.txt")["unavailable"] == {
        "snapshot": message, "diff": message, "now": message}
    assert views(repo, repo.fixed, file="../x")["unavailable"]["snapshot"] == "../x is outside the repository"
    assert views(repo, repo.fixed, file=None)["unavailable"]["now"] == "The finding names no file"


def test_findings_take_an_end_line_by_either_name():
    from agentflow.workflow.models import Finding
    dumped = Finding.model_validate({"title": "t", "line": 3, "end_line": "5"}).model_dump(by_alias=True, exclude_none=True)
    assert dumped == {"severity": "medium", "line": 3, "endLine": 5, "title": "t"}
    assert Finding.model_validate({"endLine": 7}).end_line == 7


def test_lines_added_inside_the_flagged_range_count_as_a_change(repo):
    # Reviewed lines 7..9 are "line 6".."line 8"; the fix commit only changed lines 5-6 above them.
    assert views(repo, repo.fixed, line=7, end_line=9)["now"]["changed"] is False
    lines = (repo.path / "src" / "app.txt").read_text().splitlines()
    lines[8:8] = ["inserted a", "inserted b"]  # between "line 7" and "line 8"
    current = commit(repo.path, "\n".join(lines) + "\n", "insert")
    assert views(repo, current, line=7, end_line=9)["now"]["changed"] is True
    # Added right after the range's last line is outside it.
    assert views(repo, current, line=7, end_line=8)["now"]["changed"] is False


def test_too_large_globbed_or_missing_repositories_are_unavailable_not_errors(repo, monkeypatch, tmp_path):
    import agentflow.workflow.finding_code as module
    monkeypatch.setattr(module, "MAX_BYTES", 10)
    assert views(repo, repo.fixed)["unavailable"]["snapshot"] == "src/app.txt is too large to show"
    monkeypatch.undo()
    message = f"src/*.txt is not in {repo.reviewed[:7]}"
    assert views(repo, repo.fixed, file="src/*.txt")["unavailable"]["snapshot"] == message
    task = SimpleNamespace(repository_root=str(tmp_path / "gone"), worktree_path="", base_branch="main",
                           current_sha=repo.fixed)
    finding = SimpleNamespace(file="src/app.txt", line=5, end_line=None, reviewed_sha=repo.reviewed)
    data = asyncio.run(finding_code(GitService(), task, finding))
    assert data["snapshot"] is None and set(data["unavailable"]) == {"snapshot", "diff", "now"}
