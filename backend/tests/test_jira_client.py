import json
import re
from types import SimpleNamespace
from unittest.mock import AsyncMock

import httpx
import pytest

from agentflow.config import JiraConfig
from agentflow.harnesses.base import LaunchSpec
from agentflow.services.jira_client import JiraError, JiraService, build_service
from agentflow.workflow import default_workflow as wf
from test_workflow_safety import engine  # noqa: F401 (pytest fixture)


def test_build_service_is_none_when_not_fully_configured():
    assert build_service(JiraConfig()) is None
    assert build_service(JiraConfig(enabled=True)) is None
    assert build_service(JiraConfig(enabled=True, baseUrl="https://x.atlassian.net")) is None


def test_build_service_returns_a_client_when_fully_configured():
    config = JiraConfig(enabled=True, baseUrl="https://x.atlassian.net", email="a@b.com", apiToken="t")
    assert isinstance(build_service(config), JiraService)


def _picker_transport(sections):
    def handler(request: httpx.Request) -> httpx.Response:
        assert request.url.path == "/rest/api/3/issue/picker"
        assert request.url.params["currentJQL"] == "order by updated DESC"
        return httpx.Response(200, json={"sections": sections})
    return httpx.MockTransport(handler)


@pytest.mark.asyncio
async def test_search_issues_parses_sections_and_stops_at_max_results():
    sections = [{"issues": [
        {"key": "DE-1", "summaryText": "First"},
        {"key": "DE-2", "summaryText": "Second"},
        {"key": "DE-3", "summaryText": "Third"},
    ]}]
    service = JiraService(
        JiraConfig(enabled=True, baseUrl="https://x.atlassian.net", email="a@b.com", apiToken="t"),
        transport=_picker_transport(sections),
    )
    results = await service.search_issues("de", max_results=2)
    assert [r.key for r in results] == ["DE-1", "DE-2"]


@pytest.mark.asyncio
async def test_search_issues_carries_section_label_and_resolves_relative_icon():
    sections = [{"label": "Current Search", "issues": [
        {"key": "DE-1", "summaryText": "First", "img": "/secure/viewavatar?avatarType=issuetype"},
    ]}]
    service = JiraService(
        JiraConfig(enabled=True, baseUrl="https://x.atlassian.net", email="a@b.com", apiToken="t"),
        transport=_picker_transport(sections),
    )
    [result] = await service.search_issues("de")
    assert result.section == "Current Search"
    assert result.icon_url == "https://x.atlassian.net/secure/viewavatar?avatarType=issuetype"


@pytest.mark.asyncio
async def test_search_issues_lists_a_ticket_in_both_sections_once():
    sections = [
        {"label": "History Search", "issues": [{"key": "DE-1", "summaryText": "First"}]},
        {"label": "Current Search", "issues": [
            {"key": "DE-1", "summaryText": "First"},
            {"key": "DE-2", "summaryText": "Second"},
        ]},
    ]
    service = JiraService(
        JiraConfig(enabled=True, baseUrl="https://x.atlassian.net", email="a@b.com", apiToken="t"),
        transport=_picker_transport(sections),
    )
    results = await service.search_issues("de")
    assert [(r.key, r.section) for r in results] == [("DE-1", "History Search"), ("DE-2", "Current Search")]


@pytest.mark.asyncio
async def test_search_issues_raises_on_bad_credentials():
    def handler(request: httpx.Request) -> httpx.Response:
        return httpx.Response(401, json={})
    service = JiraService(
        JiraConfig(enabled=True, baseUrl="https://x.atlassian.net", email="a@b.com", apiToken="bad"),
        transport=httpx.MockTransport(handler),
    )
    with pytest.raises(JiraError):
        await service.search_issues("de")


@pytest.mark.asyncio
async def test_sync_ticket_sanitizes_filenames_and_writes_issue_markdown(tmp_path):
    def handler(request: httpx.Request) -> httpx.Response:
        if request.url.path == "/rest/api/3/issue/DE-1/comment":
            return httpx.Response(200, json={"comments": [], "total": 0})
        if request.url.path == "/rest/api/3/issue/DE-1":
            return httpx.Response(200, json={
                "fields": {
                    "summary": "Fix ../../etc/passwd handling",
                    "attachment": [
                        {"id": "9", "filename": "../../evil.png", "content": "https://x.atlassian.net/rest/api/3/attachment/content/9"},
                    ],
                },
                "renderedFields": {"description": "&lt;p&gt;It broke &amp;amp; needs a fix&lt;/p&gt;"},
            })
        if request.url.path == "/rest/api/3/attachment/content/9":
            # Real Jira Cloud behavior: attachment content 303-redirects to the media API
            # with its own pre-signed token, on a different host.
            return httpx.Response(303, headers={"Location": "https://api.media.atlassian.com/file/abc/binary?token=t"})
        if request.url.host == "api.media.atlassian.com":
            return httpx.Response(200, content=b"binary-bytes")
        raise AssertionError(f"unexpected request to {request.url}")

    service = JiraService(
        JiraConfig(enabled=True, baseUrl="https://x.atlassian.net", email="a@b.com", apiToken="t"),
        transport=httpx.MockTransport(handler),
    )
    context = tmp_path / "context"
    result = await service.sync_ticket("DE-1", context)

    assert result.added == ["9-evil.png"]
    saved = list((context / "attachments").iterdir())
    assert len(saved) == 1
    # id-prefixed, no path traversal from the uploaded filename
    assert saved[0].name == "9-evil.png"
    assert saved[0].read_bytes() == b"binary-bytes"

    issue_doc = (context / "jira-issue.md").read_text(encoding="utf-8")
    assert "DE-1" in issue_doc
    assert "It broke & needs a fix" in issue_doc


BASE = "https://x.atlassian.net"


def _att(attachment_id, filename, size=None, mime="image/png"):
    return {"id": attachment_id, "filename": filename, "mimeType": mime,
            "size": size if size is not None else len(f"bytes-{attachment_id}"),
            "content": f"{BASE}/rest/api/3/attachment/content/{attachment_id}"}


class FakeJira:
    """A mutable ticket, so tests can edit it between syncs like a human editing Jira."""

    def __init__(self, attachments, description="", comments=None, comment_page=100):
        self.attachments = attachments
        self.description = description
        self.comments = comments or []
        self.comment_page = comment_page
        self.comment_status = 200
        self.content_requests: list[str] = []

    def handler(self, request: httpx.Request) -> httpx.Response:
        path = request.url.path
        if path == "/rest/api/3/issue/DE-1":
            return httpx.Response(200, json={"fields": {"summary": "Title", "attachment": self.attachments},
                                             "renderedFields": {"description": self.description}})
        if path == "/rest/api/3/issue/DE-1/comment":
            if self.comment_status != 200:
                return httpx.Response(self.comment_status, json={})
            start = int(request.url.params["startAt"])
            page = self.comments[start:start + self.comment_page]
            return httpx.Response(200, json={"comments": page, "total": len(self.comments)})
        if match := re.fullmatch(r"/rest/api/3/attachment/content/([0-9]+)", path):
            self.content_requests.append(match.group(1))
            return httpx.Response(200, content=f"bytes-{match.group(1)}".encode())
        raise AssertionError(f"unexpected request to {request.url}")

    def service(self, **config):
        return JiraService(JiraConfig(enabled=True, baseUrl=BASE, email="a@b.com", apiToken="t", **config),
                           transport=httpx.MockTransport(self.handler))


def _manifest(context):
    data = json.loads((context / "jira-manifest.json").read_text(encoding="utf-8"))
    return {e["id"]: e for e in data["attachments"]}, data


@pytest.mark.asyncio
async def test_sync_skips_an_oversize_attachment_and_keeps_going(tmp_path):
    jira = FakeJira([_att("1", "a.png"), _att("2", "huge.mp4", size=50_000_000, mime="video/mp4"), _att("3", "c.png")])
    result = await jira.service().sync_ticket("DE-1", tmp_path, at_task_creation=True)

    assert result.added == ["1-a.png", "3-c.png"]
    assert [(s.id, s.filename) for s in result.skipped] == [("2", "huge.mp4")]
    assert "maxAttachmentBytes" in result.skipped[0].reason
    assert jira.content_requests == ["1", "3"]  # the oversize one is never even requested
    entries, _ = _manifest(tmp_path)
    assert entries["2"]["status"] == "skipped"
    assert (tmp_path / "jira-issue.md").is_file()


@pytest.mark.asyncio
async def test_sync_skips_attachments_over_the_count_limit_instead_of_failing(tmp_path):
    jira = FakeJira([_att("1", "a.png"), _att("2", "b.png"), _att("3", "c.png")])
    result = await jira.service(maxAttachments=2).sync_ticket("DE-1", tmp_path)
    assert result.added == ["1-a.png", "2-b.png"]
    assert [s.id for s in result.skipped] == ["3"]
    assert "maxAttachments" in result.skipped[0].reason


@pytest.mark.asyncio
async def test_sync_is_incremental_and_moves_removed_attachments_aside(tmp_path):
    jira = FakeJira([_att("1", "old.png"), _att("2", "kept.png")])
    await jira.service().sync_ticket("DE-1", tmp_path, at_task_creation=True)
    jira.content_requests.clear()

    # The ticket is edited: one screenshot replaced by another.
    jira.attachments = [_att("2", "kept.png"), _att("3", "new.png")]
    result = await jira.service().sync_ticket("DE-1", tmp_path)

    assert jira.content_requests == ["3"]  # the unchanged file is not downloaded again
    assert result.added == ["3-new.png"] and result.kept == ["2-kept.png"] and result.removed == ["1-old.png"]
    assert (tmp_path / "attachments-removed" / "1-old.png").is_file()
    assert sorted(p.name for p in (tmp_path / "attachments").iterdir()) == ["2-kept.png", "3-new.png"]
    entries, manifest = _manifest(tmp_path)
    assert manifest["syncGeneration"] == 2 and manifest["lastPromptedGeneration"] is None
    assert entries["1"]["status"] == "removed" and entries["1"]["removedGeneration"] == 2
    assert entries["3"]["firstSeenGeneration"] == 2 and entries["2"]["firstSeenGeneration"] == 1


@pytest.mark.asyncio
async def test_first_sync_of_a_pre_manifest_task_reports_what_changed(tmp_path):
    """AF-14811: downloaded once at task creation (no manifest back then), then its screenshots
    were replaced in Jira. The next Architect run must be told, not silently handed new files."""
    attachments = tmp_path / "attachments"
    attachments.mkdir(parents=True)
    (attachments / "1-old.png").write_bytes(b"bytes-1")
    (attachments / "2-kept.png").write_bytes(b"bytes-2")
    jira = FakeJira([_att("2", "kept.png"), _att("3", "new.png")])
    await jira.service().sync_ticket("DE-1", tmp_path)

    entries, manifest = _manifest(tmp_path)
    assert manifest["lastPromptedGeneration"] == 0 and manifest["syncGeneration"] == 1
    assert entries["2"]["firstSeenGeneration"] == 0  # already seen by the earlier Architect run
    assert entries["3"]["firstSeenGeneration"] == 1
    assert entries["1"]["status"] == "removed" and entries["1"]["removedGeneration"] == 1


@pytest.mark.asyncio
async def test_sync_redownloads_a_truncated_file_and_clears_leftover_parts(tmp_path):
    jira = FakeJira([_att("1", "a.png")])
    attachments = tmp_path / "attachments"
    attachments.mkdir(parents=True)
    (attachments / "1-a.png").write_bytes(b"trunc")  # a crash mid-download left a short file
    (attachments / "2-b.png.part").write_bytes(b"x")
    result = await jira.service().sync_ticket("DE-1", tmp_path)
    assert result.added == ["1-a.png"]
    assert (attachments / "1-a.png").read_bytes() == b"bytes-1"
    assert not (attachments / "2-b.png.part").exists()


@pytest.mark.asyncio
async def test_sync_marks_embedded_images_inline_in_description_and_comments(tmp_path):
    description = (f'<p>Step 1</p><p><img src="{BASE}/rest/api/3/attachment/content/1" alt="a.png" /></p>'
                   f'<p>See <a href="{BASE}/secure/attachment/2/log.txt">log.txt</a></p>'
                   f'<p><img src="{BASE}/rest/api/3/attachment/content/99" /></p>'
                   '<p><img src="https://elsewhere.example/x.png" /></p>'
                   f'<p><img src="{BASE}/rest/api/3/attachment/content/4" /></p>')
    comments = [
        {"id": "501", "author": {"displayName": "Tester"}, "created": "2026-09-28T10:02:57.909+1300",
         "renderedBody": f'<p>Still broken:</p><img src="{BASE}/rest/api/3/attachment/content/3" alt="c.png">'},
        {"id": "502", "author": {"displayName": "Dev"}, "created": "2026-09-28T11:00:00.000+1300",
         "renderedBody": "<p>Looking</p>"},
    ]
    jira = FakeJira([_att("1", "a.png"), _att("2", "log.txt", mime="text/plain"),
                     _att("3", "c.png"), _att("4", "big.png", size=50_000_000)],
                    description=description, comments=comments, comment_page=1)  # forces paging
    await jira.service().sync_ticket("DE-1", tmp_path)

    doc = (tmp_path / "jira-issue.md").read_text(encoding="utf-8")
    description_part, comments_part = doc.split("## Comments (2)")
    assert "Step 1\n[image: attachments/1-a.png]" in description_part
    assert "See [attachment: attachments/2-log.txt]" in description_part
    assert "[attachment 99: not an attachment of this ticket]" in description_part
    assert "[image not attached to the ticket: https://elsewhere.example/x.png]" in description_part
    assert "[attachment 4 (big.png): not downloaded - " in description_part
    assert "### Comment 501 - Tester, 2026-09-28 10:02" in comments_part
    assert "Still broken:\n[image: attachments/3-c.png]" in comments_part
    assert "Looking" in comments_part
    entries, _ = _manifest(tmp_path)
    assert entries["1"]["referencedIn"] == ["description"]
    assert entries["3"]["referencedIn"] == ["comment:501"]


@pytest.mark.asyncio
async def test_sync_still_writes_the_issue_when_comments_cannot_be_fetched(tmp_path):
    jira = FakeJira([_att("1", "a.png")])
    jira.comment_status = 403
    result = await jira.service().sync_ticket("DE-1", tmp_path)
    assert result.comments_error and result.added == ["1-a.png"]
    doc = (tmp_path / "jira-issue.md").read_text(encoding="utf-8")
    assert "Not fetched" in doc and "Jira MCP" in doc


@pytest.mark.asyncio
async def test_sync_extracts_video_keyframes_only_on_architect_runs(tmp_path, monkeypatch):
    calls = []

    async def fake_extract(video, dest, **kwargs):
        calls.append(video.name)
        dest.mkdir(parents=True)
        (dest / "frame-01-t00m00.0s.png").write_bytes(b"png")
        return ["frame-01-t00m00.0s.png"]

    monkeypatch.setattr("agentflow.services.media_frames.extract_frames", fake_extract)
    jira = FakeJira([_att("7", "repro.mp4", mime="video/mp4")],
                    description=f'<a href="{BASE}/rest/api/3/attachment/content/7">repro.mp4</a>')
    await jira.service().sync_ticket("DE-1", tmp_path, at_task_creation=True)
    assert calls == []  # never at task creation

    result = await jira.service().sync_ticket("DE-1", tmp_path)
    assert calls == ["7-repro.mp4"] and result.frames == {"7": 1}
    entries, _ = _manifest(tmp_path)
    assert entries["7"]["frames"] == ["frame-01-t00m00.0s.png"]
    doc = (tmp_path / "jira-issue.md").read_text(encoding="utf-8")
    assert "[video: attachments/7-repro.mp4 - keyframes in frames/7/]" in doc

    await jira.service().sync_ticket("DE-1", tmp_path)
    assert calls == ["7-repro.mp4"]  # already extracted


@pytest.mark.asyncio
async def test_keyframes_share_one_time_budget_per_sync(tmp_path, monkeypatch):
    clock = [1000.0]
    monkeypatch.setattr("agentflow.services.jira_client.time.monotonic", lambda: clock[0])
    timeouts = []

    async def slow_extract(video, dest, *, timeout, **kwargs):
        timeouts.append(timeout)
        clock[0] += 119.5  # this video uses almost all of the 120 s budget
        dest.mkdir(parents=True)
        (dest / "frame-01.png").write_bytes(b"png")
        return ["frame-01.png"]

    monkeypatch.setattr("agentflow.services.media_frames.extract_frames", slow_extract)
    jira = FakeJira([_att("7", "a.mp4", mime="video/mp4"), _att("8", "b.mp4", mime="video/mp4")])
    result = await jira.service().sync_ticket("DE-1", tmp_path)

    assert timeouts == [120]
    assert result.frames == {"7": 1} and "deferred to the next sync" in result.frame_errors["8"]
    entries, _ = _manifest(tmp_path)
    assert not entries["8"].get("framesChecked")  # picked up again next sync

    await jira.service().sync_ticket("DE-1", tmp_path)
    assert len(timeouts) == 2  # the deferred video, with a fresh budget


@pytest.mark.asyncio
async def test_mark_prompted_records_what_the_architect_was_shown(tmp_path):
    from agentflow.services.jira_client import manifest_generation, mark_prompted

    jira = FakeJira([_att("1", "a.png")])
    await jira.service().sync_ticket("DE-1", tmp_path, at_task_creation=True)
    await jira.service().sync_ticket("DE-1", tmp_path)
    assert manifest_generation(tmp_path) == 2
    mark_prompted(tmp_path, 2)
    await jira.service().sync_ticket("DE-1", tmp_path)  # carried forward by later syncs
    _, manifest = _manifest(tmp_path)
    assert manifest["lastPromptedGeneration"] == 2 and manifest["syncGeneration"] == 3


@pytest.mark.asyncio
async def test_sync_records_a_frame_failure_without_failing(tmp_path, monkeypatch):
    from agentflow.services import media_frames

    async def broken(*args, **kwargs):
        raise media_frames.FrameExtractionError("ffmpeg exited with code 1")

    monkeypatch.setattr("agentflow.services.media_frames.extract_frames", broken)
    jira = FakeJira([_att("7", "repro.mov", mime="video/quicktime")])
    result = await jira.service().sync_ticket("DE-1", tmp_path)
    assert result.added == ["7-repro.mov"] and result.frame_errors == {"7": "ffmpeg exited with code 1"}
    entries, _ = _manifest(tmp_path)
    assert entries["7"]["framesChecked"] is True  # a bad video isn't retried on every run


def _transition_service(status_name, transitions, *, posted):
    def handler(request: httpx.Request) -> httpx.Response:
        if request.url.path == "/rest/api/3/issue/DE-1" and request.method == "GET":
            return httpx.Response(200, json={"fields": {"status": {"name": status_name}}})
        if request.url.path == "/rest/api/3/issue/DE-1/transitions" and request.method == "GET":
            return httpx.Response(200, json={"transitions": transitions})
        if request.url.path == "/rest/api/3/issue/DE-1/transitions" and request.method == "POST":
            posted.append(request)
            return httpx.Response(204)
        raise AssertionError(f"unexpected {request.method} {request.url}")
    return JiraService(
        JiraConfig(enabled=True, baseUrl="https://x.atlassian.net", email="a@b.com", apiToken="t"),
        transport=httpx.MockTransport(handler),
    )


@pytest.mark.asyncio
async def test_transition_to_skips_when_current_status_not_allowed():
    posted = []
    service = _transition_service("Blocked", [], posted=posted)
    moved = await service.transition_to("DE-1", "In Progress", ["To Do"])
    assert moved is False
    assert posted == []


@pytest.mark.asyncio
async def test_transition_to_matches_destination_status_case_insensitively_and_posts():
    posted = []
    transitions = [
        {"id": "11", "name": "Start Progress", "to": {"name": "in progress"}},
        {"id": "21", "name": "Close", "to": {"name": "Done"}},
    ]
    service = _transition_service("To Do", transitions, posted=posted)
    moved = await service.transition_to("DE-1", "In Progress", ["To Do"])
    assert moved is True
    assert len(posted) == 1
    assert json.loads(posted[0].content) == {"transition": {"id": "11"}}


@pytest.mark.asyncio
async def test_transition_to_returns_false_when_no_matching_transition_available():
    posted = []
    transitions = [{"id": "21", "name": "Close", "to": {"name": "Done"}}]
    service = _transition_service("To Do", transitions, posted=posted)
    moved = await service.transition_to("DE-1", "In Progress", ["To Do"])
    assert moved is False
    assert posted == []


@pytest.mark.asyncio
async def test_transition_to_raises_on_bad_status_fetch():
    def handler(request: httpx.Request) -> httpx.Response:
        return httpx.Response(401, json={})
    service = JiraService(
        JiraConfig(enabled=True, baseUrl="https://x.atlassian.net", email="a@b.com", apiToken="t"),
        transport=httpx.MockTransport(handler),
    )
    with pytest.raises(JiraError):
        await service.transition_to("DE-1", "In Progress", ["To Do"])


def _stub_harness_adapter():
    return SimpleNamespace(
        prompt_reference=lambda path: f"Read {path}",
        build_launch=lambda **kwargs: LaunchSpec(["stub-agent"]),
        build_resume=lambda *args, **kwargs: None,
        prepare_workspace=lambda cwd, **kwargs: None,
        capture_session_id=AsyncMock(return_value=None),
    )


@pytest.mark.asyncio
async def test_run_node_auto_transitions_jira_and_still_starts_when_it_fails(engine, monkeypatch):
    (engine.config.database_file.parent / "tree").mkdir(exist_ok=True)
    engine.config.jira.auto_transition_status = "In Progress"
    engine.config.jira.auto_transition_from_statuses = ["To Do"]

    lock_held_during_sync = []

    async def failing_sync(*args, **kwargs):
        # The launch lock is global: a slow Jira sync inside it would stall every task's launches.
        lock_held_during_sync.append(engine._launch_lock.locked())
        # Not a JiraError: an unexpected payload must not stop the Architect starting either.
        raise ValueError("Expecting value: line 1 column 1")

    fake_service = SimpleNamespace(transition_to=AsyncMock(side_effect=JiraError("Jira is down")),
                                   sync_ticket=failing_sync)
    monkeypatch.setattr("agentflow.workflow.engine.build_jira_service", lambda cfg: fake_service)

    engine.harnesses.get = lambda key: _stub_harness_adapter()
    engine.zellij.ensure_session = AsyncMock()
    engine.zellij.new_tab = AsyncMock(return_value=(1, "terminal_1"))
    engine.zellij.shell_command = lambda: ["pwsh"]
    engine.zellij.pane_state.return_value = SimpleNamespace(exited=False)

    with engine.db.session() as session:
        task = engine._load_task(session, "TEST-1")
        engine._node(task, wf.ARCHITECT).state = "READY"

    snap = await engine.run_node("TEST-1", wf.ARCHITECT)

    fake_service.transition_to.assert_awaited_once_with("TEST-1", "In Progress", ["To Do"])
    architect = next(n for n in snap["nodes"] if n["key"] == wf.ARCHITECT)
    assert architect["state"] == "RUNNING"  # the failed Jira calls did not block or fail the run
    assert any("Could not update Jira status" in e["message"] for e in snap["events"])
    assert any("Could not sync Jira ticket context: ValueError" in e["message"] for e in snap["events"])
    assert lock_held_during_sync == [False]


@pytest.mark.asyncio
async def test_architect_launch_records_the_jira_generation_it_was_shown(engine, monkeypatch):
    from agentflow.services.jira_client import TicketSync

    (engine.config.database_file.parent / "tree").mkdir(exist_ok=True)

    context = engine.workspace(engine_task(engine)).context_dir
    context.mkdir(parents=True, exist_ok=True)
    (context / "jira-manifest.json").write_text(json.dumps({"syncGeneration": 5, "attachments": []}), encoding="utf-8")
    fake_service = SimpleNamespace(sync_ticket=AsyncMock(return_value=TicketSync()),
                                   transition_to=AsyncMock(return_value=False))
    monkeypatch.setattr("agentflow.workflow.engine.build_jira_service", lambda cfg: fake_service)
    engine.harnesses.get = lambda key: _stub_harness_adapter()
    engine.zellij.ensure_session = AsyncMock()
    engine.zellij.new_tab = AsyncMock(return_value=(1, "terminal_1"))
    engine.zellij.shell_command = lambda: ["pwsh"]
    engine.zellij.pane_state.return_value = SimpleNamespace(exited=False)
    with engine.db.session() as session:
        engine._node(engine._load_task(session, "TEST-1"), wf.ARCHITECT).state = "READY"

    await engine.run_node("TEST-1", wf.ARCHITECT)

    fake_service.sync_ticket.assert_awaited_once()
    manifest = json.loads((context / "jira-manifest.json").read_text(encoding="utf-8"))
    assert manifest["lastPromptedGeneration"] == 5


def engine_task(engine):
    with engine.db.session() as session:
        task = engine._load_task(session, "TEST-1")
        session.expunge(task)
        return task
