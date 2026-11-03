"""Reviewed lessons: agents propose (`flowctl lesson`), a human approves them into the role's
repository guidance in the original checkout, uncommitted. Nothing reaches a briefing before that."""

from pathlib import Path
from unittest.mock import AsyncMock

import pytest

from agentflow.workflow import default_workflow as wf
from agentflow.workflow import instructions, lessons
from agentflow.workflow.engine import WorkflowError
from test_workflow_safety import add_run, engine  # noqa: F401 - fixture


def test_a_lesson_is_one_trimmed_line():
    assert lessons.normalize("  Run the build\n\n  # first\t please ") == "Run the build # first please"
    with pytest.raises(lessons.LessonError, match="needs text"):
        lessons.validate(" \n\t ")
    with pytest.raises(lessons.LessonError, match="at most 300 characters"):
        lessons.validate("x" * 301)
    assert lessons.validate("é" * 300) == "é" * 300
    with pytest.raises(lessons.LessonError, match="NUL"):
        lessons.validate("a\x00b")


def test_append_adds_a_heading_once_and_keeps_line_endings():
    assert lessons.append_lesson(None, "First") == "## Lessons from past tasks\n\n- First\n"
    assert lessons.append_lesson("", "First") == "## Lessons from past tasks\n\n- First\n"
    once = lessons.append_lesson("# Implementer\n\nUse tabs.", "First")
    assert once == "# Implementer\n\nUse tabs.\n\n## Lessons from past tasks\n\n- First\n"
    assert lessons.append_lesson(once, "Second") == once + "- Second\n"
    crlf = "# Implementer\r\n\r\nUse tabs.\r\n"
    assert lessons.append_lesson(crlf, "First") == crlf + "\r\n## Lessons from past tasks\r\n\r\n- First\r\n"
    # A later section after the lessons heading gets a new lessons heading, so the bullet is under it.
    later = "## Lessons from past tasks\n\n- Old\n\n## Testing\n\nRun pytest.\n"
    assert lessons.append_lesson(later, "New") == later + "\n## Lessons from past tasks\n\n- New\n"


async def propose(engine, text, key=wf.IMPLEMENTER):
    run_id = add_run(engine, key)
    return run_id, await engine.agent_lesson("TEST-1", "implementer" if key == wf.IMPLEMENTER else "code_reviewer",
                                             run_id, text)


def guidance(engine, name="implementer.md") -> Path:
    return engine.config.database_file.parent / ".agentflow-instructions" / name


@pytest.mark.asyncio
async def test_an_agent_proposes_and_the_snapshot_shows_it(engine):
    run_id, result = await propose(engine, "Run   the build\nfirst.")
    assert result["ok"] and result["count"] == 1
    [lesson] = engine.snapshot("TEST-1")["lessons"]
    assert (lesson["role"], lesson["nodeKey"], lesson["runId"], lesson["text"], lesson["status"]) == (
        "implementer", wf.IMPLEMENTER, run_id, "Run the build first.", "proposed")
    assert lesson["decidedAt"] is None and lesson["path"] is None
    assert not guidance(engine).exists()  # a proposal writes nothing to the repository


@pytest.mark.asyncio
async def test_proposals_need_an_active_run_and_are_capped(engine):
    run_id = add_run(engine, wf.IMPLEMENTER, "COMPLETED")
    with pytest.raises(WorkflowError, match="active run") as error:
        await engine.agent_lesson("TEST-1", "implementer", run_id, "Too late")
    assert error.value.status_code == 409
    with pytest.raises(WorkflowError) as error:
        await engine.agent_lesson("TEST-1", "implementer", add_run(engine, wf.IMPLEMENTER), " ")
    assert error.value.status_code == 422
    run_id = add_run(engine, wf.IMPLEMENTER)
    for n in range(lessons.MAX_PER_RUN):
        await engine.agent_lesson("TEST-1", "implementer", run_id, f"Lesson {n}")
    with pytest.raises(WorkflowError, match="already proposed 5 lessons"):
        await engine.agent_lesson("TEST-1", "implementer", run_id, "One too many")


@pytest.mark.asyncio
async def test_approving_appends_the_edited_text_to_the_original_checkout(engine):
    engine.git.dirty_paths = AsyncMock(return_value=["?? .agentflow-instructions/"])
    _, proposed = await propose(engine, "Run the build first")
    result = await engine.decide_lesson("TEST-1", proposed["uid"], True, "Run `npm run build`\nbefore the tests")
    path = guidance(engine)
    assert path.read_bytes() == b"## Lessons from past tasks\n\n- Run `npm run build` before the tests\n"
    assert result["dirtyPaths"] == ["?? .agentflow-instructions/"]
    lesson = result["lesson"]
    assert (lesson["status"], lesson["approvedText"], lesson["text"]) == (
        "approved", "Run `npm run build` before the tests", "Run the build first")
    assert Path(lesson["path"]) == path and lesson["decidedAt"]
    assert not (Path(engine.snapshot("TEST-1")["worktreePath"]) / ".agentflow-instructions").exists()

    with pytest.raises(WorkflowError, match="already approved") as error:
        await engine.decide_lesson("TEST-1", proposed["uid"], True)
    assert error.value.status_code == 409
    assert path.read_bytes().count(b"- Run") == 1


@pytest.mark.asyncio
async def test_approving_keeps_existing_guidance_and_its_line_endings(engine):
    path = guidance(engine, "code-reviewer.md")
    path.parent.mkdir(parents=True)
    path.write_bytes(b"# Reviewer\r\n\r\nBe brief.")
    _, proposed = await propose(engine, "The lint warning in gen/ is expected", key=wf.CODE_REVIEW)
    await engine.decide_lesson("TEST-1", proposed["uid"], True)
    assert path.read_bytes() == (b"# Reviewer\r\n\r\nBe brief.\r\n\r\n## Lessons from past tasks\r\n\r\n"
                                 b"- The lint warning in gen/ is expected\r\n")


@pytest.mark.asyncio
async def test_dismissing_writes_nothing_and_unknown_lessons_are_404(engine):
    _, proposed = await propose(engine, "Not useful")
    result = await engine.decide_lesson("TEST-1", proposed["uid"], False)
    assert result["lesson"]["status"] == "dismissed" and result["dirtyPaths"] == []
    assert not guidance(engine).exists()
    with pytest.raises(WorkflowError, match="already dismissed"):
        await engine.decide_lesson("TEST-1", proposed["uid"], True)
    with pytest.raises(WorkflowError) as error:
        await engine.decide_lesson("TEST-1", "00000000-0000-0000-0000-000000000000", True)
    assert error.value.status_code == 404


@pytest.mark.asyncio
async def test_guidance_that_cannot_take_the_lesson_is_refused(engine):
    path = guidance(engine)
    path.parent.mkdir(parents=True)
    path.write_text("x" * (instructions.MAX_FILE_BYTES - 10), encoding="utf-8")
    _, proposed = await propose(engine, "This would make the file too large")
    with pytest.raises(WorkflowError) as error:
        await engine.decide_lesson("TEST-1", proposed["uid"], True)
    assert error.value.status_code == 409
    assert engine.snapshot("TEST-1")["lessons"][0]["status"] == "proposed"


@pytest.mark.asyncio
async def test_an_unreadable_lessons_file_fails_closed(engine):
    _, proposed = await propose(engine, "First")
    store = lessons.path(Path(engine.snapshot("TEST-1")["taskDir"]))
    store.write_text("{not json", encoding="utf-8")
    with pytest.raises(WorkflowError, match="unreadable") as error:
        await engine.agent_lesson("TEST-1", "implementer", add_run(engine, wf.IMPLEMENTER), "Second")
    assert error.value.status_code == 409
    assert store.read_text(encoding="utf-8") == "{not json"  # never replaced
    assert engine.snapshot("TEST-1")["lessons"] == []


def test_every_briefing_names_the_lesson_command_with_its_run(tmp_path):
    from agentflow.workflow import prompts
    ctx = prompts.PromptContext(
        jira_key="TEST-1", worktree_root=tmp_path, project_dir=None, task_dir=tmp_path, run_dir=tmp_path / "run",
        run_id=7, role="implementer", base_branch="main", branch_name="agent/TEST-1", additional_context="",
        iteration=1, max_iterations=3, flowctl_path="flowctl")
    block = prompts._flowctl_block(ctx)
    assert 'flowctl lesson --message "' in block and "It has no effect on this run" in block
    snapshot = {"files": {"implementer": {"content": None}}}
    assert "flowctl lesson --run 7 --message" in instructions.compose(block, snapshot, "implementer", 7)
