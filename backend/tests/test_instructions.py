"""Instruction safety and launch contracts. All harnesses are local stubs, never providers."""
import json
import subprocess
import sys
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import AsyncMock, Mock

import pytest

from agentflow.api.instructions import ApplyUpdate, SourceSave, apply_update, task_instructions
from agentflow.config import HarnessConfig
from agentflow.harnesses.base import SessionHandle, LaunchSpec
from agentflow.harnesses.claude import ClaudeAdapter
from agentflow.harnesses.copilot import CopilotAdapter
from agentflow.workflow import instructions as guidance, default_workflow as wf
from agentflow.workflow.engine import WorkflowError
from test_workers import parallel_engine, submit_plan, finish, plan
from test_workflow_safety import engine, client, add_run, add_session


def sources(root):
    for role in guidance.ROLES:
        guidance.save_source(root, role, f'Unique {role}\nflowctl complete\n<script>text</script>\n', None)


def test_optional_defaults_exact_composition_and_role_separation(tmp_path):
    snapshot = guidance.capture(tmp_path / 'task', tmp_path, 'a' * 40)
    original = 'workflow flowctl complete\n'
    for role in guidance.ROLES:
        assert guidance.compose(original, snapshot, role, 42) == original.replace(
            'flowctl complete', 'flowctl complete --run 42') + guidance.RUN_NOTICE
    sources(tmp_path)
    new = guidance.capture(tmp_path / 'task', tmp_path, 'b' * 40)
    for role in guidance.ROLES:
        composed = guidance.compose(original, new, role, 42)
        assert composed.endswith(new['files'][role]['content'])
        assert composed.count('flowctl complete --run 42') == 1
        assert composed.count('flowctl complete\n') == 1
        for other in guidance.ROLES:
            assert (f'Unique {other}\n' in composed) == (other == role)


@pytest.mark.parametrize('content', ['\ufeffBOM', 'NUL\x00', 'x' * 65537, '\ud800'],
                         ids=['bom', 'nul', 'oversized', 'surrogate'])
def test_malformed_and_oversized_save_fails_without_file(tmp_path, content):
    with pytest.raises(guidance.InstructionError):
        guidance.save_source(tmp_path, 'architect', content, None)
    assert not (tmp_path / '.agentflow-instructions').exists()


def test_invalid_utf8_total_size_concurrent_changes_and_unsupported_paths(tmp_path):
    folder = tmp_path / '.agentflow-instructions'
    folder.mkdir()
    (folder / 'architect.md').write_bytes(b'\xff')
    with pytest.raises(guidance.InstructionError):
        guidance.read_set(tmp_path)
    (folder / 'architect.md').unlink()
    first = guidance.save_source(tmp_path, 'architect', 'one', None)
    guidance.save_source(tmp_path, 'architect', 'external', first['hash'])
    with pytest.raises(guidance.InstructionError, match='changed'):
        guidance.save_source(tmp_path, 'architect', 'lost update', first['hash'])
    for role in ('../escape', 'C:/absolute', '\\\\server\\share', 'architect.md', '\\\\?\\C:\\x'):
        with pytest.raises(guidance.InstructionError):
            guidance.save_source(tmp_path, role, 'bad', None)
    for role in guidance.ROLES[:4]:
        old = guidance.read_source(tmp_path, role)
        guidance.save_source(tmp_path, role, 'x' * 65536, old['hash'])
    with pytest.raises(guidance.InstructionError, match='262144'):
        guidance.save_source(tmp_path, guidance.ROLES[4], 'x', None)
    assert guidance.read_source(tmp_path, guidance.ROLES[4])['content'] is None


def test_remote_and_device_roots_are_rejected_before_any_filesystem_access(monkeypatch):
    def unexpected_stat(*args, **kwargs):
        raise AssertionError('Remote/device path reached filesystem')
    monkeypatch.setattr(Path, 'is_dir', unexpected_stat)
    for root in ('\\\\server\\share', '\\\\?\\UNC\\server\\share', '\\\\.\\C:\\repo', '\\??\\C:\\repo'):
        with pytest.raises(guidance.InstructionError, match='UNC/device'):
            guidance.checked_path(Path(root), 'architect')


def test_snapshot_survives_source_removal_and_detects_required_artifact_loss(tmp_path):
    sources(tmp_path)
    task = tmp_path / 'task'
    captured = guidance.capture(task, tmp_path, 'revision')
    (tmp_path / guidance.role_path('architect')).unlink()
    assert guidance.active_snapshot(task) == captured
    version_file = task / 'instructions' / f'{captured["version"]}.json'
    version_file.write_text('{}', encoding='utf-8')
    with pytest.raises(guidance.InstructionError):
        guidance.active_snapshot(task)
    version_file.unlink()
    (task / 'instructions' / 'active.json').unlink()
    (task / 'instructions').rmdir()
    with pytest.raises(guidance.InstructionError, match='required|Required'):
        guidance.active_snapshot(task)


def test_directory_links_are_rejected(tmp_path):
    outside = tmp_path / 'other'
    outside.mkdir()
    path = tmp_path / '.agentflow-instructions'
    if sys.platform == 'win32':
        # Junction creation needs no symlink privilege. Both paths stay in this test workspace.
        result = subprocess.run(['cmd', '/c', 'mklink', '/J', str(path), str(outside)], capture_output=True)
        assert result.returncode == 0, result.stderr
    else:
        path.symlink_to(outside, target_is_directory=True)
    with pytest.raises(guidance.InstructionError, match='links|reparse'):
        guidance.read_set(tmp_path)


@pytest.mark.parametrize('key,iteration', [
    (wf.ARCHITECT, 0), (wf.ARCHITECT, 1), (wf.ORCHESTRATOR, 0),
    (wf.IMPLEMENTER, 1), (wf.IMPLEMENTER, 2),
    (wf.CODE_REVIEW, 1), (wf.SECURITY_REVIEW, 1), (wf.FINAL_ARCHITECT, 1),
])
async def test_every_launch_branch_receives_only_its_role(parallel_engine, key, iteration):
    engine = parallel_engine
    with engine.db.session() as session:
        task = engine._load_task(session, 'TEST-1')
        root = Path(task.worktree_path)
        sources(root)
        task.review_iteration = max(1, iteration)
        node = engine._node(task, key)
        node.iteration = iteration if key == wf.ARCHITECT else 1
        session.commit()
        preview = await task_instructions('TEST-1', key, None, 'snapshot', SimpleNamespace(engine=engine))
        run = await engine._start_agent_run_unlocked(session, task, node)
        snapshot = guidance.active_snapshot(engine.workspace(task).root)
        text = Path(run.prompt_path).read_text(encoding='utf-8')
        assert f'Unique {node.role}\n' in text
        expected = preview['briefing'].replace('--run 0', f'--run {run.id}')
        expected = expected.replace('run id (`0`)', f'run id (`{run.id}`)')
        assert expected == text
        assert text.count('Unique ') == 1
        metadata = json.loads((Path(run.run_dir) / 'instructions.json').read_text())
        assert metadata['version'] == snapshot['version']
        assert metadata['briefingHash'] == guidance.content_hash(text)
        assert metadata['source'] == snapshot['files'][node.role]
        if key == wf.ORCHESTRATOR:
            assert 'Unique architect' not in text
            assert engine.resolve_agent_config(task, node)['harness'] == engine.config.agents.architect.harness


async def test_parallel_workers_and_integration_reuse_snapshot_despite_edits(parallel_engine):
    engine = parallel_engine
    with engine.db.session() as session:
        task = engine._load_task(session, 'TEST-1')
        root = Path(task.worktree_path)
    sources(root)
    nodes = await submit_plan(engine)
    first = json.loads((Path(nodes[0]['lastRun']['runDir']) / 'instructions.json').read_text())
    old = guidance.read_source(root, 'implementer')
    guidance.save_source(root, 'implementer', 'NEW instructions', old['hash'])
    state = SimpleNamespace(engine=engine, db=engine.db)
    snapshot = guidance.active_snapshot(engine.config.task_dir / 'TEST-1')
    req = ApplyUpdate(expectedVersion=snapshot['version'],
                      expectedSourceHashes={r: f['hash'] for r, f in guidance.read_set(root).items()},
                      freshConversations=True)
    with pytest.raises(Exception) as error:
        await apply_update('TEST-1', req, state)
    assert 'parallel batch' in str(error.value)
    await finish(engine, nodes[0])
    nodes = [n for n in engine.snapshot('TEST-1')['nodes'] if n.get('worker')]
    for node in nodes[1:]:
        metadata = json.loads((Path(node['lastRun']['runDir']) / 'instructions.json').read_text())
        assert metadata['version'] == first['version']
        assert metadata['source'] == first['source']
        await finish(engine, node)
    result = await engine.run_node('TEST-1', wf.IMPLEMENTER)
    run = next(n['lastRun'] for n in result['nodes'] if n['key'] == wf.IMPLEMENTER)
    assert 'Unique implementer' in Path(run['promptPath']).read_text()
    assert 'NEW instructions' not in Path(run['promptPath']).read_text()
    integrator = next(n for n in result['nodes'] if n['key'] == wf.IMPLEMENTER)
    await finish(engine, integrator)
    updated = await apply_update('TEST-1', req, state)
    assert updated['version'] != first['version']


async def test_preview_explicit_update_restart_and_history(parallel_engine):
    engine = parallel_engine
    state = SimpleNamespace(engine=engine, db=engine.db)
    with engine.db.session() as session:
        task = engine._load_task(session, 'TEST-1')
        sources(Path(task.worktree_path))
        node = engine._node(task, wf.ARCHITECT)
        run = await engine._start_agent_run_unlocked(session, task, node)
        run_id, run_dir, role = run.id, Path(run.run_dir), node.role
        directory, root = engine.workspace(task).root, Path(task.worktree_path)
    old = guidance.active_snapshot(directory)
    guidance.save_source(root, role, 'Updated guidance', old['files'][role]['hash'])
    preview = await task_instructions('TEST-1', wf.ARCHITECT, None, 'snapshot', state)
    assert preview['differs'] is True
    assert 'Updated guidance' not in preview['briefing']
    source_preview = await task_instructions('TEST-1', wf.ARCHITECT, None, 'source', state)
    assert source_preview['briefing'].endswith('Updated guidance')
    req = ApplyUpdate(expectedVersion=old['version'], expectedSourceHashes={r: f['hash'] for r, f in preview['files'].items()}, freshConversations=True)
    new = await apply_update('TEST-1', req, state)
    assert new['version'] != old['version']
    assert guidance.active_snapshot(directory) == new
    # Reopen the database in a new engine, as after an application restart.
    reopened_db = engine.db.__class__(engine.config.database_file)
    restarted = engine.__class__(engine.config, reopened_db, engine.git, engine.zellij,
                                 engine.harnesses, engine.broadcaster, engine.api_base_url)
    try:
        launches_before_recovery = engine.zellij.new_tab.await_count
        await restarted.recover()
        await restarted.recover_node('TEST-1', wf.ARCHITECT, retry=False)
        assert engine.zellij.new_tab.await_count == launches_before_recovery
        assert json.loads((run_dir / 'instructions.json').read_text())['version'] == old['version']
        restarted_preview = await task_instructions(
            'TEST-1', wf.ARCHITECT, None, 'snapshot', SimpleNamespace(engine=restarted))
        assert restarted_preview['snapshot']['version'] == new['version']
        assert restarted_preview['briefing'].endswith('Updated guidance')
    finally:
        reopened_db.engine.dispose()
    assert guidance.needs_fresh(new, str(run_dir))
    assert not guidance.needs_fresh(old, str(run_dir))
    assert (directory / 'instructions' / f'{old["version"]}.json').exists()
    history = await task_instructions('TEST-1', wf.ARCHITECT, run_id, 'snapshot', state)
    assert history['metadata']['version'] == old['version']
    assert 'Updated guidance' not in history['briefing']
    with pytest.raises(Exception):
        await apply_update('TEST-1', req, state)


def test_operator_only_instruction_apis(client, engine):
    base = '/api/tasks/TEST-1/instructions'
    assert client.get(base).status_code == 401
    for method, path, payload in [
        ('GET', base, None), ('PUT', base + '/architect', {'content': 'bad', 'expectedHash': None}),
        ('POST', base + '/apply', {}), ('GET', '/api/projects/1/instructions', None),
    ]:
        response = client.request(method, path, json=payload, headers={'Authorization': 'Bearer test-token'})
        assert response.status_code == 403


def test_failed_atomic_save_preserves_previous_source(tmp_path, monkeypatch):
    original = guidance.save_source(tmp_path, 'architect', 'original', None)
    monkeypatch.setattr(guidance.os, 'replace', Mock(side_effect=OSError('simulated replace failure')))
    with pytest.raises(guidance.InstructionError):
        guidance.save_source(tmp_path, 'architect', 'replacement', original['hash'])
    assert guidance.read_source(tmp_path, 'architect') == original


async def test_failed_run_evidence_prevents_launch_and_records_failure(parallel_engine, monkeypatch):
    engine = parallel_engine
    with engine.db.session() as session:
        task = engine._load_task(session, 'TEST-1')
        engine._node(task, wf.IMPLEMENTER).state = 'READY'
    monkeypatch.setattr(guidance, 'record_run', Mock(side_effect=guidance.InstructionError('evidence unavailable')))
    with pytest.raises(WorkflowError, match='evidence unavailable'):
        await engine.run_node('TEST-1', wf.IMPLEMENTER)
    node = next(n for n in engine.snapshot('TEST-1')['nodes'] if n['key'] == wf.IMPLEMENTER)
    assert node['state'] == 'FAILED'
    assert node['lastRun']['state'] == 'FAILED'
    engine.zellij.new_tab.assert_not_awaited()


async def test_update_chooses_resume_or_fresh_without_touching_running_agent(parallel_engine, monkeypatch):
    engine = parallel_engine
    adapter = engine.harnesses.get('claude')
    adapter.build_launch = Mock(return_value=LaunchSpec(['stub-agent'], harness_session_id='recorded-session'))
    adapter.build_resume = Mock(return_value=LaunchSpec(['stub-agent', '--resume'], harness_session_id='recorded-session'))
    monkeypatch.setattr(engine, '_live_session', AsyncMock(return_value=None))
    with engine.db.session() as session:
        task = engine._load_task(session, 'TEST-1')
        root, directory = Path(task.worktree_path), engine.workspace(task).root
        sources(root)
        await engine._start_agent_run_unlocked(session, task, engine._node(task, wf.IMPLEMENTER))
    for fresh in (False, True):
        previous = guidance.active_snapshot(directory)
        guidance.save_source(root, 'implementer', f'New instructions fresh={fresh}', previous['files']['implementer']['hash'])
        req = ApplyUpdate(expectedVersion=previous['version'],
                          expectedSourceHashes={r: f['hash'] for r, f in guidance.read_set(root).items()},
                          freshConversations=fresh)
        launches_before = engine.zellij.new_tab.await_count
        await apply_update('TEST-1', req, SimpleNamespace(engine=engine, db=engine.db))
        assert engine.zellij.new_tab.await_count == launches_before
        assert engine.zellij.close_tab.await_count == 0
        resumes_before = adapter.build_resume.call_count
        with engine.db.session() as session:
            task = engine._load_task(session, 'TEST-1')
            run = await engine._start_agent_run_unlocked(session, task, engine._node(task, wf.IMPLEMENTER))
            assert f'fresh={fresh}' in Path(run.prompt_path).read_text()
        assert adapter.build_resume.call_count == resumes_before + (0 if fresh else 1)


@pytest.mark.parametrize('adapter_type', [ClaudeAdapter, CopilotAdapter])
def test_two_local_adapters_deliver_exact_briefing_on_launch_and_resume(tmp_path, adapter_type):
    sources(tmp_path)
    snapshot = guidance.capture(tmp_path / 'task', tmp_path, 'revision')
    briefing = guidance.compose('flowctl complete', snapshot, 'implementer', 73)
    prompt = tmp_path / 'prompt.md'
    prompt.write_text(briefing, encoding='utf-8')
    stub = tmp_path / 'stub.py'
    stub.write_text('import sys, re\nfrom pathlib import Path\n'
                    'reference = next(a for a in sys.argv if "Read the file " in a)\n'
                    'path = re.search(r"Read the file (.+?prompt\\.md) and follow", reference)[1]\n'
                    'sys.stdout.buffer.write(Path(path).read_bytes())\n', encoding='utf-8')
    config = HarnessConfig(command=sys.executable, args=[str(stub)])
    adapter = adapter_type('stub', config)
    reference = adapter.prompt_reference(prompt)
    launch = adapter.build_launch(model=None, reasoning=None, initial_prompt=reference)
    handle = SessionHandle(role='implementer', zellij_session='stub', tab_name='stub',
                           pane_id=None, cwd=tmp_path, harness_session_id=launch.harness_session_id)
    resume = adapter.build_resume(handle, model=None, reasoning=None, initial_prompt=reference)
    for spec in (launch, resume):
        assert spec is not None
        completed = subprocess.run(spec.argv, capture_output=True, check=True, cwd=tmp_path)
        assert completed.stdout == prompt.read_bytes()


@pytest.mark.parametrize('failure_stage', ['snapshot', 'pointer', 'established'])
def test_interrupted_initial_capture_recovers_exact_content(tmp_path, monkeypatch, failure_stage):
    sources(tmp_path)
    task = tmp_path / 'task'
    original = guidance.atomic_json
    def fail(path, value):
        stage = ('pointer' if path.name == 'active.json' else
                 'established' if value == {'schema': 2, 'state': 'established'} else
                 'snapshot' if path.parent.name == 'instructions' else 'pending')
        if stage == failure_stage:
            raise guidance.InstructionError('Injected persistence failure')
        original(path, value)
    with monkeypatch.context() as patch:
        patch.setattr(guidance, 'atomic_json', fail)
        with pytest.raises(guidance.InstructionError):
            guidance.capture(task, tmp_path, 'original')
    pending = json.loads((task / 'instructions-required.json').read_text())['snapshot']
    source = guidance.read_source(tmp_path, 'architect')
    guidance.save_source(tmp_path, 'architect', 'Changed after interruption', source['hash'])
    assert guidance.active_snapshot(task) == pending
    assert guidance.active_snapshot(task) == pending
    assert json.loads((task / 'instructions-required.json').read_text()) == {
        'schema': 2, 'state': 'established'}
    (task / 'instructions' / 'active.json').unlink()
    with pytest.raises(guidance.InstructionError):
        guidance.active_snapshot(task)


def test_pending_capture_with_invalid_hash_fails_closed(tmp_path):
    task = tmp_path / 'task'
    snapshot = guidance.capture(task, tmp_path, 'original')
    snapshot['files']['architect']['hash'] = 'invalid'
    guidance.atomic_json(task / 'instructions-required.json',
                         {'schema': 2, 'state': 'pending', 'snapshot': snapshot})
    with pytest.raises(guidance.InstructionError):
        guidance.active_snapshot(task)
