import json
from pathlib import Path
from types import SimpleNamespace

from agentflow.workflow import prompts, worker_prompts
from agentflow.workflow.engine import WorkflowEngine


def make_ctx(**overrides) -> prompts.PromptContext:
    defaults = dict(
        jira_key="DE-1", worktree_root=Path("D:/agentflow/worktrees/DE-1"),
        project_dir=Path("D:/agentflow/worktrees/DE-1/Project"), task_dir=Path("D:/agentflow/tasks/DE-1"),
        run_dir=Path("D:/agentflow/tasks/DE-1/runs/architect/01"), run_id=1, role="architect",
        base_branch="main", branch_name="agent/DE-1", additional_context="", iteration=1, max_iterations=5,
        flowctl_path="D:/agentflow/bin/flowctl",
    )
    defaults.update(overrides)
    return prompts.PromptContext(**defaults)


def test_flowctl_block_gives_an_absolute_path_fallback_for_a_broken_path():
    """Confirmed failure mode: something later in a long agent session (e.g. npm install)
    can rewrite PATH and drop flowctl from it, mid-session, with no warning."""
    ctx = make_ctx(flowctl_path="D:/agentflow/bin/flowctl")
    text = prompts.architect_discovery(ctx)
    assert "D:/agentflow/bin/flowctl" in text


def test_architect_discovery_falls_back_to_mcp_only_guidance_without_local_jira_data():
    ctx = make_ctx(task_dir=Path("D:/agentflow/tasks/DE-1"))  # directory doesn't exist on disk
    text = prompts.architect_discovery(ctx)
    assert "No local Jira data was pre-fetched" in text
    assert "AgentFlow synced this ticket" not in text


def test_architect_discovery_prefers_local_attachments_when_present(tmp_path):
    task_dir = tmp_path / "DE-1"
    attachments = task_dir / "context" / "attachments"
    attachments.mkdir(parents=True)
    (attachments / "9-screenshot.png").write_bytes(b"x")
    (task_dir / "context" / "jira-issue.md").write_text("# DE-1: title\n\nbody\n", encoding="utf-8")

    ctx = make_ctx(task_dir=task_dir)
    text = prompts.architect_discovery(ctx)
    assert "AgentFlow synced this ticket" in text
    assert "9-screenshot.png" in text
    assert "jira-issue.md" in text
    assert "No local Jira data was pre-fetched" not in text


def _synced_task(tmp_path, generation, last_prompted, attachments):
    task_dir = tmp_path / "DE-1"
    context = task_dir / "context"
    (context / "attachments").mkdir(parents=True)
    (context / "attachments-removed").mkdir()
    (context / "jira-issue.md").write_text("# DE-1: title\n", encoding="utf-8")
    for entry in attachments:
        folder = "attachments-removed" if entry["status"] == "removed" else "attachments"
        if entry["status"] != "skipped":
            (context / folder / entry["file"]).write_bytes(b"x")
    (context / "jira-manifest.json").write_text(json.dumps({
        "syncGeneration": generation, "lastPromptedGeneration": last_prompted, "attachments": attachments,
    }), encoding="utf-8")
    return task_dir


MANIFEST_ENTRIES = [
    {"id": "1", "file": "1-a.png", "status": "present", "firstSeenGeneration": 1, "referencedIn": ["description"]},
    {"id": "3", "file": "3-c.png", "status": "present", "firstSeenGeneration": 3, "referencedIn": ["comment:501"]},
    {"id": "7", "file": "7-repro.mp4", "status": "present", "firstSeenGeneration": 1, "referencedIn": [],
     "frames": ["frame-01-t00m00.0s.png", "frame-02-t00m04.0s.png"]},
    {"id": "8", "file": "8-huge.mov", "filename": "huge.mov", "status": "skipped", "firstSeenGeneration": 1,
     "reason": "50.0 MB is over maxAttachmentBytes (20.0 MB)"},
    {"id": "2", "file": "2-old.png", "status": "removed", "firstSeenGeneration": 1, "removedGeneration": 3},
]


def test_local_block_lists_frames_skipped_and_removed_attachments_from_the_manifest(tmp_path):
    ctx = make_ctx(task_dir=_synced_task(tmp_path, 3, 2, MANIFEST_ENTRIES))
    text = prompts.architect_discovery(ctx)
    assert "`1-a.png` (in the description)" in text
    assert "`3-c.png` (**new since your last run**; in comment 501)" in text
    assert "2 keyframes, in time order" in text and "frames/7" in text
    assert "attachment 8 `huge.mov`: 50.0 MB is over maxAttachmentBytes" in text
    assert "attachments-removed" in text and "`2-old.png`" in text
    assert "Open every image and keyframe" in text


def test_revision_and_final_verification_flag_ticket_changes(tmp_path):
    ctx = make_ctx(task_dir=_synced_task(tmp_path, 3, 2, MANIFEST_ENTRIES))
    history = {"iterations": 1, "runs": [], "findings": []}
    for text in (prompts.architect_revision(ctx, "tighten AC", 1),
                 prompts.final_architect(ctx, final_sha="a" * 40, history=history)):
        assert "## The Jira ticket changed since your last run" in text
        assert "New attachment `3-c.png` (in comment 501)" in text
        assert "Attachment `2-old.png` was removed" in text
        assert "7-repro.mp4" in text


def test_changes_stay_flagged_across_a_sync_whose_launch_was_refused(tmp_path):
    """Sync 3 found attachment 3, but that launch was refused (e.g. capacity), so the Architect
    was last shown sync 2. Sync 4 then runs; attachment 3 is still news to it."""
    entries = [{"id": "3", "file": "3-c.png", "status": "present", "firstSeenGeneration": 3, "referencedIn": []}]
    ctx = make_ctx(task_dir=_synced_task(tmp_path, 4, 2, entries))
    assert "New attachment `3-c.png`" in prompts.architect_revision(ctx, "x", 1)


def test_first_architect_run_does_not_call_anything_new(tmp_path):
    """Attachments added between task creation and discovery aren't news to an Architect that
    has never read the ticket."""
    entries = [{"id": "3", "file": "3-c.png", "status": "present", "firstSeenGeneration": 2, "referencedIn": []}]
    ctx = make_ctx(task_dir=_synced_task(tmp_path, 2, None, entries))
    assert "new since your last run" not in prompts.architect_discovery(ctx)
    assert "changed since your last run" not in prompts.architect_revision(ctx, "x", 1)


def test_architect_discovery_pipes_the_result_into_flowctl_with_no_file_write():
    """A short result is piped in. The agent never writes result.json: the engine writes that
    file itself once it accepts a result."""
    ctx = make_ctx()
    text = prompts.architect_discovery(ctx)
    assert "flowctl complete --file - <<'EOF'" in text
    assert "result.json" not in text
    assert "result-" not in text


def test_every_role_can_submit_a_long_result_through_its_run_dir_submission_file():
    """A harness passes the whole shell command, heredoc included, as one process argument, so
    a long result hits Windows' ~32K command-line limit (ENAMETOOLONG). AF-14419's orchestrator
    failed three times that way with a 7-subtask plan and then stalled."""
    ctx = make_ctx()
    submission = "D:/agentflow/tasks/DE-1/runs/architect/01/submission.json"
    item = SimpleNamespace(plan_key="api", worktree_path="D:/w", branch_name="b", base_sha="a" * 40,
                           brief="Do it", owned_paths=["api"], acceptance_criteria=["works"])
    for text in (
        prompts.architect_discovery(ctx),
        prompts.implementer(ctx, findings=None, requirements={}, previous_sha=None),
        prompts.code_review(ctx, sha="a" * 40, implementation_summary="", requirements={}),
        prompts.final_architect(ctx, final_sha="a" * 40, history={"iterations": 1, "runs": [], "findings": []}),
        worker_prompts.orchestrator(ctx),
        worker_prompts.worker(ctx, item, []),
        worker_prompts.integration(ctx, [], "Merge them"),
    ):
        assert f"flowctl complete --file {submission}" in text
        assert "ENAMETOOLONG" in text


def test_implementer_and_reviews_and_final_architect_all_pipe_into_flowctl():
    ctx = make_ctx()
    for text in (
        prompts.implementer(ctx, findings=None, requirements={}, previous_sha=None),
        prompts.code_review(ctx, sha="a" * 40, implementation_summary="", requirements={}),
        prompts.security_review(ctx, sha="a" * 40, implementation_summary="", requirements={}),
        prompts.final_architect(ctx, final_sha="a" * 40, history={"iterations": 1, "runs": [], "findings": []}),
    ):
        assert "flowctl complete --file - <<'EOF'" in text
        assert "result.json" not in text


def test_followup_review_starts_from_previous_checkpoint():
    previous, current = "a" * 40, "b" * 40
    ctx = make_ctx(previous_review_sha=previous)
    text = prompts.security_review(ctx, sha=current, implementation_summary="Fix", requirements={})
    assert f"git diff {previous} {current} --" in text
    assert f"git diff main...{current}" not in text


def test_followup_review_verifies_prior_findings_and_limits_new_blockers_to_the_delta():
    previous, current = "a" * 40, "b" * 40
    ctx = make_ctx(previous_review_sha=previous)
    prior = [{"id": "CODE-002", "title": "Null deref", "blocking": True, "severity": "high",
              "dbId": 7, "createdAt": "2024-01-01T00:00:00Z", "status": "superseded"}]
    text = prompts.code_review(ctx, sha=current, implementation_summary="Fix", requirements={}, previous_findings=prior)
    assert "verify + delta only" in text
    assert "CODE-002" in text and "Null deref" in text
    assert "dbId" not in text and "createdAt" not in text
    assert "report it again" in text
    assert "Never re-raise prior non-blocking findings" in text
    assert "Expand to the full change" not in text


def test_first_review_has_no_followup_block():
    text = prompts.code_review(make_ctx(), sha="a" * 40, implementation_summary="", requirements={})
    assert "Follow-up review" not in text


def test_reviews_restrict_blocking_and_keep_findings_short():
    ctx = make_ctx()
    for text in (prompts.code_review(ctx, sha="a" * 40, implementation_summary="", requirements={}),
                 prompts.security_review(ctx, sha="a" * 40, implementation_summary="", requirements={})):
        assert "What may block" in text
        assert "Always set `blocking` explicitly" in text
        assert "at most 3 non-blocking findings" in text
        assert "at most 2 sentences" in text


def test_fix_round_makes_non_blocking_findings_optional():
    text = prompts.implementer(make_ctx(), findings=[{"title": "Rename var", "blocking": False}],
                               requirements={}, previous_sha="a" * 40)
    assert "Non-blocking ones are optional" in text
    assert "knownIssues" in text


def test_implementer_references_approved_documents_without_duplicating_contents():
    ctx = make_ctx()
    text = prompts.implementer(ctx, findings=None, requirements={"requirements": "Do this thing"}, previous_sha=None)
    assert "requirements.md" in text
    assert "acceptance-criteria.md" in text
    assert "Read the approved documents" in text
    assert "Do this thing" not in text
    large = prompts.implementer(ctx, findings=None, requirements={"requirements": "scope " * 10000}, previous_sha=None)
    assert len(large) == len(text)


def test_fix_round_directs_targeted_reading_without_weakening_findings():
    text = prompts.implementer(make_ctx(), findings=[{"title": "Fix injection", "blocking": True}],
                               requirements={}, previous_sha="a" * 40)
    assert "focused fix round" in text
    assert "Fix injection" in text
    assert "acceptance criteria" in text
    assert "blocking ones are mandatory" in text


def test_implementer_fix_block_projects_findings_to_lean_keys():
    ctx = make_ctx()
    findings = [
        {
            "id": "CODE-001",
            "title": "Fix this",
            "description": "It's broken",
            "recommendation": "Do this",
            "severity": "high",
            "blocking": True,
            "file": "src/main.py",
            "line": 42,
            "dbId": 999,
            "createdAt": "2024-01-01T00:00:00Z",
            "reviewedSha": "abc123",
            "type": "code-review",
            "iteration": 1,
            "status": "open",
        }
    ]
    text = prompts.implementer(ctx, findings=findings, requirements={}, previous_sha="abc123")
    # The findings JSON should be compact and only contain lean keys
    assert "CODE-001" in text
    assert "Fix this" in text
    assert "It's broken" in text
    assert "dbId" not in text
    assert "createdAt" not in text
    assert "reviewedSha" not in text
    # Verify compact JSON (no indent)
    assert '{\n' not in text.split("```json")[1].split("```")[0] or text.split("```json")[1].split("```")[0].count('\n') < 3


def test_final_architect_mentions_superseded_findings():
    ctx = make_ctx()
    text = prompts.final_architect(ctx, final_sha="a" * 40, history={"iterations": 1, "runs": [], "findings": []})
    assert "Findings from earlier iterations that a later review superseded are listed as one-liners" in text


def test_history_finding_with_superseded_status():
    """Superseded findings should be compact one-liners in history."""
    finding = SimpleNamespace(
        finding_id="CODE-001", type="code-review", iteration=1, reviewed_sha="abc123",
        severity="high", blocking=True, file="src/main.py", line=42, title="Fix this bug",
        description="It's broken", recommendation="Do this", status="superseded",
    )
    result = WorkflowEngine._history_finding(finding)
    assert set(result.keys()) == {"id", "iteration", "file", "line", "title", "status"}
    assert result["id"] == "CODE-001"
    assert result["status"] == "superseded"
    assert "dbId" not in result
    assert "createdAt" not in result
    assert "type" not in result


def test_history_finding_with_open_status():
    """Open findings should have full details in history."""
    finding = SimpleNamespace(
        finding_id="SEC-002", type="security-review", iteration=2, reviewed_sha="def456",
        severity="critical", blocking=True, file="src/auth.py", line=100, title="Auth bypass",
        description="Missing validation", recommendation="Add check", status="open",
    )
    result = WorkflowEngine._history_finding(finding)
    assert set(result.keys()) == {"id", "type", "iteration", "severity", "blocking", "file", "line", "title", "description", "recommendation", "status"}
    assert result["id"] == "SEC-002"
    assert result["type"] == "security-review"
    assert result["severity"] == "critical"
    assert result["status"] == "open"
    assert "dbId" not in result
    assert "createdAt" not in result
    assert "reviewedSha" not in result
