import json
import sys
from argparse import Namespace
from datetime import datetime, timedelta, timezone
from pathlib import Path

import httpx
import pytest
from sqlalchemy import select

from agentflow.services.git_workspace import CreatedWorktree
from agentflow.cli.flowctl import _headers
from agentflow.config import AgentConfig, HarnessConfig, JiraConfig
from agentflow.database.models import RequirementsApproval
from agentflow.harnesses.claude import ClaudeAdapter
from agentflow.harnesses.codex import CodexAdapter
from agentflow.harnesses.copilot import CopilotAdapter
from agentflow.harnesses.base import LaunchSpec, SessionHandle
from agentflow.services.git_workspace import GitError
from agentflow.services.jira_client import JiraService, JiraError
from agentflow.services.process import clean_env
from agentflow.workflow import assurance, default_workflow as wf
from agentflow.workflow.engine import WorkflowError
from test_workflow_safety import engine, SHA, add_run, requirements_digest, task_state
from test_workflow_safety import client, add_session
from test_workers import parallel_engine
from agentflow.workflow.usage import collect_usage, UsageReport
from agentflow.workflow.models import CreateTaskRequest, AgentOverride
from agentflow.database.models import NodeRun
from types import SimpleNamespace
from unittest.mock import AsyncMock


def jira_service(attachment, seen, **limits):
    def handler(request):
        seen.append(request)
        if request.url.path.endswith('/issue/TEST-1'):
            return httpx.Response(200, json={'fields': {'attachment': [attachment]}})
        if request.url.host == 'jira.invalid':
            return httpx.Response(303, headers={'location': 'https://api.media.atlassian.com/file'})
        return httpx.Response(200, content=b'123456')
    return JiraService(JiraConfig(enabled=True, baseUrl='https://jira.invalid', fetchComments=False,
        email='test@example.invalid', apiToken='synthetic', **limits), httpx.MockTransport(handler))


# A bad attachment is skipped (and recorded), never fatal to the rest of the ticket sync, but
# the security properties are unchanged: no credentials leave for another host, no partial
# file survives, and nothing is written outside the attachments directory.
@pytest.mark.asyncio
@pytest.mark.parametrize('url', ['http://jira.invalid/file', 'https://evil.invalid/file', 'https://jira.invalid@evil.invalid/file'])
async def test_untrusted_attachment_never_receives_credentials(tmp_path, url):
    seen = []
    service = jira_service({'id': '1', 'filename': 'file', 'content': url}, seen)
    result = await service.sync_ticket('TEST-1', tmp_path)
    assert [s.id for s in result.skipped] == ['1'] and 'Untrusted' in result.skipped[0].reason
    assert len(seen) == 1
    assert not list((tmp_path / 'attachments').iterdir())


@pytest.mark.asyncio
async def test_attachment_redirect_strips_auth_and_size_limit_removes_partial(tmp_path):
    seen = []
    service = jira_service({'id': '1', 'filename': 'file', 'content': 'https://jira.invalid/file'}, seen, maxAttachmentBytes=3)
    result = await service.sync_ticket('TEST-1', tmp_path)
    assert 'byte limit' in result.skipped[0].reason
    assert not seen[-1].headers.get('authorization')
    assert not list((tmp_path / 'attachments').iterdir())


@pytest.mark.asyncio
async def test_attachment_id_cannot_escape(tmp_path):
    seen = []
    service = jira_service({'id': '../escaped', 'filename': 'file', 'content': 'https://jira.invalid/file'}, seen)
    result = await service.sync_ticket('TEST-1', tmp_path)
    assert 'ID' in result.skipped[0].reason
    assert len(seen) == 1
    assert not (tmp_path / 'escaped-file').exists()
    assert not list((tmp_path / 'attachments').iterdir())


def test_inherited_secrets_are_removed(monkeypatch):
    monkeypatch.setenv('UNRELATED_API_TOKEN', 'synthetic')
    monkeypatch.setenv('PATH', 'retained')
    assert 'UNRELATED_API_TOKEN' not in clean_env()
    assert clean_env()['PATH'] == 'retained'
    assert clean_env({'EXPLICIT_TOKEN': 'synthetic'})['EXPLICIT_TOKEN'] == 'synthetic'


def test_scoped_flowctl_does_not_scan_other_credentials(tmp_path, monkeypatch):
    path = tmp_path / 'credential.json'
    path.write_text(json.dumps({'runId': 1, 'token': 'synthetic'}))
    monkeypatch.setenv('AGENTFLOW_CREDENTIAL_FILE', str(path))
    assert _headers(Namespace(run=1)) == {'Authorization': 'Bearer synthetic'}
    with pytest.raises(SystemExit, match='no credential'):
        _headers(Namespace(run=2))


def test_claude_does_not_implicitly_trust_workspace(tmp_path, monkeypatch):
    monkeypatch.setattr('agentflow.harnesses.claude.trust_workspace', lambda _: pytest.fail('Implicit trust'))
    ClaudeAdapter('claude', HarnessConfig(command='claude')).prepare_workspace(tmp_path)


@pytest.mark.parametrize('adapter_type', [CodexAdapter, CopilotAdapter])
def test_resume_requires_exact_session(adapter_type, tmp_path):
    adapter = adapter_type('test', HarnessConfig(command='test'))
    handle = SessionHandle('implementer', 'session', 'tab', None, tmp_path)
    assert adapter.build_resume(handle, model=None, reasoning=None, initial_prompt='brief') is None


def _write_session_meta(path, *, session_id, cwd, timestamp, source='cli'):
    path.write_text(json.dumps({'type': 'session_meta', 'payload': {
        'session_id': session_id, 'cwd': str(cwd), 'source': source, 'timestamp': timestamp.isoformat(),
    }}) + '\n', encoding='utf-8')


@pytest.mark.asyncio
async def test_codex_capture_session_id_matches_cwd_and_launch_time(tmp_path, monkeypatch):
    monkeypatch.setattr(Path, 'home', lambda: tmp_path)
    cwd = tmp_path / 'work'
    cwd.mkdir()
    sessions_dir = tmp_path / '.codex' / 'sessions' / '2026' / '01' / '01'
    sessions_dir.mkdir(parents=True)
    since = datetime.now(timezone.utc)
    # A pre-existing rollout for the same cwd (an earlier, unrelated run) must be ignored.
    _write_session_meta(sessions_dir / 'rollout-old.jsonl', session_id='old-id', cwd=cwd,
                        timestamp=since - timedelta(minutes=5))
    # The file this exact launch just created.
    _write_session_meta(sessions_dir / 'rollout-new.jsonl', session_id='new-id', cwd=cwd,
                        timestamp=since + timedelta(seconds=1))
    adapter = CodexAdapter('codex', HarnessConfig(command='codex'))
    assert await adapter.capture_session_id(cwd, since) == 'new-id'


@pytest.mark.asyncio
async def test_codex_capture_session_id_gives_up_on_concurrent_sibling(tmp_path, monkeypatch):
    # Code review and security review share the same task cwd and can launch close together;
    # an ambiguous match must not risk wiring resume to the wrong role's conversation.
    monkeypatch.setattr(Path, 'home', lambda: tmp_path)
    cwd = tmp_path / 'work'
    cwd.mkdir()
    sessions_dir = tmp_path / '.codex' / 'sessions' / '2026' / '01' / '01'
    sessions_dir.mkdir(parents=True)
    since = datetime.now(timezone.utc)
    _write_session_meta(sessions_dir / 'rollout-a.jsonl', session_id='id-a', cwd=cwd,
                        timestamp=since + timedelta(seconds=1))
    _write_session_meta(sessions_dir / 'rollout-b.jsonl', session_id='id-b', cwd=cwd,
                        timestamp=since + timedelta(seconds=2))
    adapter = CodexAdapter('codex', HarnessConfig(command='codex'))
    assert await adapter.capture_session_id(cwd, since) is None


def test_copilot_build_launch_sets_session_id_for_resume_and_usage_lookup(tmp_path):
    # build_launch assigns --session-id both so a usage collector can find this run's
    # session log and so a later run can resume the same conversation with --resume; the
    # usage collector (scripts/usage_copilot.py) does delta accounting against a cursor file
    # rather than assuming the whole log belongs to one run.
    adapter = CopilotAdapter('copilot', HarnessConfig(command='copilot'))
    launch = adapter.build_launch(model=None, reasoning=None, initial_prompt='brief')
    assert launch.harness_session_id
    assert '--session-id' in launch.argv
    handle = SessionHandle('implementer', 'session', 'tab', None, tmp_path, harness_session_id=launch.harness_session_id)
    resumed = adapter.build_resume(handle, model=None, reasoning=None, initial_prompt='brief')
    assert resumed is not None
    assert '--resume' in resumed.argv
    assert launch.harness_session_id in resumed.argv
    assert resumed.harness_session_id == launch.harness_session_id


@pytest.mark.asyncio
async def test_requirements_mutation_blocks_launch_and_final_override(engine):
    with engine.db.session() as session:
        task = engine._load_task(session, 'TEST-1')
        (engine.workspace(task).requirements_dir / 'acceptance-criteria.md').write_text('Changed scope')
        engine._node(task, wf.IMPLEMENTER).state = 'READY'
        engine._node(task, wf.READY_FOR_PR).state = 'WAITING_FOR_APPROVAL'
    with pytest.raises(WorkflowError, match='requirements changed'):
        await engine.run_node('TEST-1', wf.IMPLEMENTER)
    with pytest.raises(WorkflowError, match='requirements changed'):
        await engine.approve('TEST-1', wf.READY_FOR_PR, reason='override', expected_sha=SHA)


def test_approval_requires_acceptance_criteria(engine):
    with engine.db.session() as session:
        task = engine._load_task(session, 'TEST-1')
        workspace = engine.workspace(task)
        (workspace.requirements_dir / 'acceptance-criteria.md').write_text(' ')
        with pytest.raises(ValueError, match='acceptanceCriteria'):
            assurance.approve_requirements(session, task, workspace)


@pytest.mark.asyncio
async def test_verification_collects_exit_code_sha_and_bounded_output(tmp_path):
    evidence = await assurance.verify_commands([[sys.executable, '-c', "print('x'*20000); raise SystemExit(3)"]], tmp_path, SHA)
    assert evidence['sha'] == SHA
    assert evidence['passed'] is False
    assert evidence['commands'][0]['exitCode'] == 3
    assert len(evidence['commands'][0]['output']) == 16000


@pytest.mark.asyncio
async def test_verification_keeps_the_end_of_long_output_where_the_test_summary_is(tmp_path):
    script = "print('restore warning ' * 5000); print('Passed!  - Failed: 0, Passed: 27')"
    evidence = await assurance.verify_commands([[sys.executable, '-c', script]], tmp_path, SHA)
    output = evidence['commands'][0]['output']
    assert output.startswith('restore warning')
    assert 'output truncated' in output
    assert output.rstrip().endswith('Passed!  - Failed: 0, Passed: 27')
    assert len(output.encode()) <= 16000
    short = await assurance.verify_commands([[sys.executable, '-c', "print('all good')"]], tmp_path, SHA)
    assert short['commands'][0]['output'].strip() == 'all good'


@pytest.mark.asyncio
async def test_usage_does_not_block_launch_when_cost_controls_are_disabled(engine):
    """Existing installations retain their launch behaviour until controls are enabled."""
    adapter = SimpleNamespace(
        prompt_reference=lambda path: f"Read {path}",
        build_launch=lambda **kwargs: LaunchSpec(["stub-agent"]),
        build_resume=lambda *args, **kwargs: None,
        prepare_workspace=lambda cwd, **kwargs: None,
        capture_session_id=AsyncMock(return_value=None),
    )
    engine.harnesses.get = lambda key: adapter
    engine.zellij.ensure_session = AsyncMock()
    engine.zellij.new_tab = AsyncMock(return_value=(7, "terminal_7"))
    engine.zellij.shell_command = lambda: ["pwsh"]
    add_run(engine, wf.ARCHITECT, state='COMPLETED')
    with engine.db.session() as session:
        task = engine._load_task(session, 'TEST-1')
        Path(task.worktree_path).mkdir(exist_ok=True)
        engine._node(task, wf.IMPLEMENTER).state = 'READY'
    before = engine.snapshot('TEST-1')
    assert (before['totalUsage']['runs'], before['totalUsage']['runsWithUsage']) == (1, 0)
    snapshot = await engine.run_node('TEST-1', wf.IMPLEMENTER)
    node = next(n for n in snapshot['nodes'] if n['key'] == wf.IMPLEMENTER)
    assert node['lastRun'] is not None
    assert (snapshot['totalUsage']['runs'], snapshot['totalUsage']['runsWithUsage']) == (2, 0)


@pytest.mark.asyncio
async def test_global_capacity_pauses_launch(engine):
    add_run(engine, wf.CODE_REVIEW)
    engine.config.workflow.max_concurrent_agents = 1
    with engine.db.session() as session:
        engine._node(engine._load_task(session, 'TEST-1'), wf.IMPLEMENTER).state = 'READY'
    snapshot = await engine.run_node('TEST-1', wf.IMPLEMENTER)
    node = next(n for n in snapshot['nodes'] if n['key'] == wf.IMPLEMENTER)
    assert node['lastRun'] is None
    assert 'Global agent capacity' in node['message']


@pytest.mark.asyncio
async def test_usage_collector_collects_provider_report_and_model(tmp_path):
    payload = {'inputTokens': 100, 'outputTokens': 20, 'cachedInputTokens': 30,
               'model': 'actual-model', 'source': 'synthetic provider report'}
    report = await collect_usage([sys.executable, '-c', f'print({json.dumps(payload)!r})'], tmp_path, 1, None)
    assert report.record('collector')['totalTokens'] == 120
    assert report.model == 'actual-model'
    assert report.cost is None
    with pytest.raises(ValueError):
        UsageReport.model_validate({**payload, 'cachedInputTokens': 101})


@pytest.mark.asyncio
async def test_usage_collector_rejects_oversized_report(tmp_path):
    with pytest.raises(ValueError, match='16 KB'):
        await collect_usage([sys.executable, '-c', "print('x'*20000)"], tmp_path, 1, None)


@pytest.mark.asyncio
async def test_repository_validation_only_fetches_for_unknown_branches(engine, tmp_path):
    """The New Task form validates on every keystroke pause. A branch already known locally
    (local head or remote-tracking ref) must validate without a network round trip; only an
    unknown name falls back to a fetch so a branch new on origin still validates."""
    engine.git.inspect_repository = AsyncMock(return_value=SimpleNamespace(
        root=str(tmp_path), path=str(tmp_path), current_branch='main', branches=['develop', 'main']))
    engine.git.resolve_base_ref = AsyncMock(side_effect=GitError("Base branch 'dev' not found locally or on origin"))
    engine.git.create_worktree = AsyncMock(return_value=CreatedWorktree('NEW-1'))
    engine.zellij.ensure_session = AsyncMock(return_value=False)

    result = await engine.validate_repository(str(tmp_path), 'develop')
    assert result['ok'] and any(c['message'] == 'Base branch: develop' for c in result['checks'])
    engine.git.resolve_base_ref.assert_not_awaited()

    result = await engine.validate_repository(str(tmp_path), 'dev')
    assert not result['ok'] and any("'dev' not found" in c['message'] for c in result['checks'])
    engine.git.resolve_base_ref.assert_awaited_once_with(str(tmp_path), 'dev')

    # Creation delegates the fetch to create_worktree (proven against real git in
    # test_git_worktrees) instead of resolving the base ref a second time here.
    engine.git.resolve_base_ref.reset_mock()
    await engine.create_task(CreateTaskRequest(jiraKey='NEW-1', repositoryPath=str(tmp_path), baseBranch='develop'))
    engine.git.create_worktree.assert_awaited_once()
    engine.git.resolve_base_ref.assert_not_awaited()


@pytest.mark.asyncio
@pytest.mark.parametrize('mode,has_orchestrator', [('direct', False), ('parallel', True)])
async def test_creation_routes_keep_all_quality_gates(parallel_engine, tmp_path, mode, has_orchestrator):
    engine = parallel_engine
    engine.git.inspect_repository = AsyncMock(return_value=SimpleNamespace(root=str(tmp_path), path=str(tmp_path)))
    engine.git.create_worktree = AsyncMock(return_value=CreatedWorktree('ROUTE-1'))
    snapshot = await engine.create_task(CreateTaskRequest(jiraKey='ROUTE-1', repositoryPath=str(tmp_path),
        baseBranch='main', implementationMode=mode))
    keys = {node['key'] for node in snapshot['nodes']}
    assert (wf.ORCHESTRATOR in keys) is has_orchestrator
    assert {wf.REQUIREMENTS_GATE, wf.IMPLEMENTER, wf.CODE_REVIEW, wf.SECURITY_REVIEW,
            wf.FINAL_ARCHITECT, wf.READY_FOR_PR} <= keys


@pytest.mark.asyncio
@pytest.mark.parametrize('configured,override,role,label', [
    (False, {}, 'architect', 'Architect Planning'),
    (False, {'orchestrator': {'harness': '', 'model': ''}}, 'architect', 'Architect Planning'),
    (True, {}, 'orchestrator', 'Orchestrator'),
    (False, {'orchestrator': {'model': 'planner-model'}}, 'orchestrator', 'Orchestrator'),
])
async def test_parallel_planning_goes_to_the_architect_unless_an_orchestrator_is_configured(
        parallel_engine, tmp_path, configured, override, role, label):
    engine = parallel_engine
    if configured:
        engine.config.agents.orchestrator = AgentConfig(harness='claude')
    engine.git.inspect_repository = AsyncMock(return_value=SimpleNamespace(root=str(tmp_path), path=str(tmp_path)))
    engine.git.create_worktree = AsyncMock(return_value=CreatedWorktree('ROUTE-2'))
    snapshot = await engine.create_task(CreateTaskRequest(jiraKey='ROUTE-2', repositoryPath=str(tmp_path),
        baseBranch='main', implementationMode='parallel', agentOverrides=override))
    planner = next(node for node in snapshot['nodes'] if node['key'] == wf.ORCHESTRATOR)
    assert (planner['role'], planner['label']) == (role, label)


@pytest.mark.asyncio
async def test_new_configuration_gets_a_new_process(parallel_engine):
    engine = parallel_engine
    with engine.db.session() as session:
        engine._node(engine._load_task(session, 'TEST-1'), wf.IMPLEMENTER).state = 'READY'
    first = await engine.run_node('TEST-1', wf.IMPLEMENTER)
    run_id = next(n for n in first['nodes'] if n['key'] == wf.IMPLEMENTER)['lastRun']['id']
    with engine.db.session() as session:
        session.get(NodeRun, run_id).state = 'COMPLETED'
        engine._node(engine._load_task(session, 'TEST-1'), wf.IMPLEMENTER).state = 'READY'
    await engine.run_node('TEST-1', wf.IMPLEMENTER, AgentOverride(model='changed-model', reasoning='low'))
    assert engine.zellij.new_tab.await_count == 2
    with engine.db.session() as session:
        runs = session.scalars(select(NodeRun).order_by(NodeRun.id)).all()
        assert runs[-1].model == 'changed-model'
        assert runs[-1].reasoning == 'low'
    assert engine.zellij.write_line.await_count == 0


def test_agent_cannot_import_usage(client, engine):
    run_id = add_run(engine, wf.IMPLEMENTER)
    add_session(engine, run_id)
    response = client.put(f'/api/tasks/TEST-1/runs/{run_id}/usage',
        headers={'Authorization': 'Bearer test-token'}, json={})
    assert response.status_code == 403


@pytest.mark.asyncio
async def test_stale_requirements_approval_is_rejected(engine):
    with engine.db.session() as session:
        task = engine._load_task(session, 'TEST-1')
        session.delete(session.scalar(select(RequirementsApproval).where(RequirementsApproval.task_id == task.id)))
        engine._node(task, wf.REQUIREMENTS_GATE).state = 'WAITING_FOR_APPROVAL'
    with pytest.raises(WorkflowError, match='changed since'):
        await engine.approve('TEST-1', wf.REQUIREMENTS_GATE, expected_requirements_digest='0' * 64)


@pytest.mark.asyncio
async def test_engine_requires_requirements_digest_for_every_caller(engine):
    """In-process callers (e.g. a chat integration) bypass the HTTP route, so the engine
    itself must refuse a requirements approval that doesn't prove what the human saw."""
    with engine.db.session() as session:
        task = engine._load_task(session, 'TEST-1')
        session.delete(session.scalar(select(RequirementsApproval).where(RequirementsApproval.task_id == task.id)))
        engine._node(task, wf.REQUIREMENTS_GATE).state = 'WAITING_FOR_APPROVAL'
    for missing in (None, ''):
        with pytest.raises(WorkflowError, match='Refresh and review'):
            await engine.approve('TEST-1', wf.REQUIREMENTS_GATE, expected_requirements_digest=missing)
    assert task_state(engine, wf.REQUIREMENTS_GATE) == 'WAITING_FOR_APPROVAL'
    await engine.approve('TEST-1', wf.REQUIREMENTS_GATE, expected_requirements_digest=requirements_digest(engine))
    assert task_state(engine, wf.REQUIREMENTS_GATE) == 'COMPLETED'


def test_requirements_approval_route_still_requires_digest(client, engine):
    with engine.db.session() as session:
        task = engine._load_task(session, 'TEST-1')
        engine._node(task, wf.REQUIREMENTS_GATE).state = 'WAITING_FOR_APPROVAL'
    headers = {'Origin': 'http://127.0.0.1'}
    client.post('/api/auth/pair', json={'code': client.app.state.human.pairing_code}, headers=headers).raise_for_status()
    response = client.post('/api/tasks/TEST-1/nodes/requirements_gate/approve', json={}, headers=headers)
    assert response.status_code == 409
    assert 'Refresh and review' in response.json()['detail']


@pytest.mark.asyncio
async def test_legacy_snapshot_approval_does_not_restart_stages(engine):
    with engine.db.session() as session:
        task = engine._load_task(session, 'TEST-1')
        session.delete(session.scalar(select(RequirementsApproval).where(RequirementsApproval.task_id == task.id)))
        engine._node(task, wf.REQUIREMENTS_GATE).state = 'COMPLETED'
        engine._node(task, wf.IMPLEMENTER).state = 'COMPLETED'
    snapshot = await engine.approve('TEST-1', wf.REQUIREMENTS_GATE,
                                    expected_requirements_digest=requirements_digest(engine))
    assert snapshot['requirementsDigest']
    assert next(n for n in snapshot['nodes'] if n['key'] == wf.IMPLEMENTER)['state'] == 'COMPLETED'


def test_shell_unsafe_environment_names_rejected():
    with pytest.raises(ValueError, match='shell-safe'):
        HarnessConfig(command='test', env={'BAD;COMMAND': 'value'})


@pytest.mark.asyncio
async def test_collector_error_leaves_usage_unknown(engine, tmp_path):
    run_id = add_run(engine, wf.IMPLEMENTER, state='COMPLETED')
    with engine.db.session() as session:
        run = session.get(NodeRun, run_id)
        run.harness = 'codex'
        engine.config.harnesses['codex'].usage_command = [sys.executable, '-c', 'print("invalid json")']
        task = engine._load_task(session, 'TEST-1')
        Path(task.worktree_path).mkdir(parents=True, exist_ok=True)
        await engine._collect_usage(session, task, run)
    row = engine.snapshot('TEST-1')['executionEvidence'][0]
    assert row['usage'] is None
    assert row['confirmedModel'] is None


@pytest.mark.asyncio
async def test_verifier_can_be_cancelled_without_waiting_for_timeout(tmp_path):
    import asyncio
    marker = tmp_path / 'started'
    task = asyncio.create_task(assurance.verify_commands([[sys.executable, '-c',
        "from pathlib import Path; import time; Path('started').write_text('yes'); time.sleep(60)"]], tmp_path, SHA))
    for _ in range(100):
        if marker.exists():
            break
        await asyncio.sleep(0.05)
    assert marker.exists()
    task.cancel()
    with pytest.raises(asyncio.CancelledError):
        await asyncio.wait_for(task, timeout=15)
