"""Same operator/source/snapshot contract against either backend, using only local stubs."""
import hashlib
import json
from pathlib import Path

import httpx
import pytest

from support import agent_run, git, new_key, node


@pytest.fixture(scope='module')
def backend(launch):
    with launch(STUB_MANUAL='1') as backend:
        yield backend


@pytest.fixture(scope='module')
def human(backend):
    return backend.human()


def test_project_saves_are_not_committed_or_carried_into_new_worktrees(human, repo):
    project = human.ok('POST', '/api/projects', json={
        'name': new_key('Instructions'), 'defaultRepositoryPath': str(repo),
    })
    base = f'/api/projects/{project["id"]}/instructions'
    initial = human.ok('GET', base)
    assert initial['files']['architect']['content'] is None
    content = '# Architect\nUnicode café\nflowctl complete\n<script>plain text</script>\n'
    saved = human.ok('PUT', base + '/architect', json={'content': content, 'expectedHash': None})
    assert saved['hash'] == hashlib.sha256(content.encode('utf-8')).hexdigest()
    assert (repo / saved['path']).read_bytes() == content.encode('utf-8')
    assert human.put(base + '/architect', json={'content': 'lost', 'expectedHash': None}).status_code == 409
    assert human.put(base + '/architect', json={'content': 'bad'}).status_code == 422
    key = new_key()
    task = human.create_task(key, repo)
    preview = human.ok('GET', f'/api/tasks/{key}/instructions')
    assert preview['files']['architect']['content'] is None
    assert not (Path(task['worktreePath']) / saved['path']).exists()
    assert preview['snapshot'] is None


def test_launch_update_history_and_agent_authorization(backend, human, repo):
    directory = repo / '.agentflow-instructions'
    directory.mkdir()
    original = '# Shared Architect\nflowctl complete\nLiteral {{template}} and `commands`.\n'
    (directory / 'architect.md').write_bytes(original.encode('utf-8'))
    git('add', '.agentflow-instructions', cwd=repo)
    git('-c', 'user.name=contract', '-c', 'user.email=contract@local', 'commit', '-m', 'role guidance', cwd=repo)
    key = new_key()
    task = human.create_task(key, repo)
    # Git's checkout line-ending policy may change LF to CRLF. Capture exact worktree bytes.
    original = (Path(task['worktreePath']) / '.agentflow-instructions/architect.md').read_bytes().decode('utf-8')
    base = f'/api/tasks/{key}/instructions'
    before = human.ok('GET', base)
    assert before['snapshot'] is None and before['briefing'].endswith(original)
    human.run(key, 'architect')
    agent = agent_run(backend, task, 'architect')
    initial = human.ok('GET', base)
    version = initial['snapshot']['version']
    metadata = json.loads((agent.run_dir / 'instructions.json').read_text())
    assert metadata['version'] == version
    assert Path(metadata['repository']) == Path(task['worktreePath'])
    assert metadata['briefingHash'] == hashlib.sha256((agent.run_dir / 'prompt.md').read_bytes()).hexdigest()
    assert (agent.run_dir / 'prompt.md').read_bytes().decode('utf-8').endswith(original)
    assert initial['snapshot']['revision'] == git('rev-parse', 'HEAD', cwd=Path(task['worktreePath']))
    old = initial['files']['architect']
    human.ok('PUT', base + '/architect', json={'content': 'Updated Architect guidance', 'expectedHash': old['hash']})
    edited = human.ok('GET', base)
    assert edited['differs'] is True
    assert edited['snapshot']['version'] == version
    assert edited['briefing'].endswith(original)
    assert human.ok('GET', base + '?preview=source')['briefing'].endswith('Updated Architect guidance')
    request = {'expectedVersion': version,
               'expectedSourceHashes': {r: f['hash'] for r, f in edited['files'].items()},
               'freshConversations': True}
    updated = human.ok('POST', base + '/apply', json=request)
    assert updated['version'] != version
    assert human.post(base + '/apply', json=request).status_code == 409
    assert human.ok('GET', base)['briefing'].endswith('Updated Architect guidance')
    historical = human.ok('GET', base + f'?runId={agent.run_id}')
    assert historical['metadata']['version'] == version
    assert historical['briefing'].endswith(original)
    assert node(human.task(key), 'architect')['state'] == 'RUNNING'
    for method, suffix, body in [('GET', '', None), ('PUT', '/architect', {'content': 'bad', 'expectedHash': None}),
                                  ('POST', '/apply', request)]:
        response = httpx.request(method, backend.url + base + suffix, json=body,
                                 headers={'Authorization': f'Bearer {agent.token}'})
        assert response.status_code == 403
    assert httpx.get(backend.url + base).status_code == 401
