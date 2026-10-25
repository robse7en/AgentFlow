import json
from pathlib import Path
from unittest.mock import Mock, patch

from agentflow import diagnostics
from test_workflow_safety import client, engine


def test_log_is_structured_rotating_and_omits_exception_secrets(tmp_path):
    path = tmp_path / 'logs' / 'agentflow.jsonl'
    handler = diagnostics.open_log(path)
    token = diagnostics.request_id.set('test-reference')
    try:
        try:
            raise ValueError('SECRET_TOKEN')
        except ValueError:
            diagnostics.log.exception('test.failed')
        entry = json.loads(path.read_text())
        assert entry['requestId'] == 'test-reference'
        assert entry['exceptionType'] == 'ValueError'
        assert entry['frames']
        assert 'SECRET_TOKEN' not in path.read_text()
        handler.maxBytes = 1
        diagnostics.event('test.next')
        assert path.with_name('agentflow.jsonl.1').is_file()
        assert handler.backupCount == 4
    finally:
        diagnostics.request_id.reset(token)
        diagnostics.log.removeHandler(handler)
        handler.close()


def test_diagnostics_require_pairing_and_reject_agent_credentials(client):
    client.post('/api/auth/pair', json={'code': client.app.state.human.pairing_code},
                headers={'Origin': 'http://127.0.0.1'}).raise_for_status()
    response = client.get('/api/diagnostics')
    assert response.status_code == 200
    assert response.headers['x-request-id']
    assert 'application.starting' in response.text
    assert client.get('/api/diagnostics', headers={'Authorization': 'Bearer secret'}).status_code == 403
    client.cookies.clear()
    assert client.get('/api/diagnostics').status_code == 401


def test_diagnostics_content_length_matches_body_despite_concurrent_growth(client):
    """Regression for D1: the `diagnostic_requests` middleware appends a `request.completed`
    line to the same NDJSON file *this very request* reads, after the route handler builds
    its response but before it's sent. A plain FileResponse stamps Content-Length from
    stat() up front, so uvicorn then finds the streamed body longer than promised and
    truncates it. Simulate the race by growing the file the instant it's read into memory;
    the header must still match what's actually returned."""
    client.post('/api/auth/pair', json={'code': client.app.state.human.pairing_code},
                headers={'Origin': 'http://127.0.0.1'}).raise_for_status()
    path = client.app.state.diagnostic_path

    original_read_bytes = Path.read_bytes

    def read_then_grow(self, *args, **kwargs):
        data = original_read_bytes(self, *args, **kwargs)
        if self == path:
            with open(path, 'a', encoding='utf-8') as handle:
                handle.write('{"event": "grew-after-read"}\n')
        return data

    with patch.object(Path, 'read_bytes', read_then_grow):
        response = client.get('/api/diagnostics')
    assert response.status_code == 200
    assert int(response.headers['content-length']) == len(response.content)


def test_internal_error_has_correlated_safe_diagnostic(client, engine):
    client.post('/api/auth/pair', json={'code': client.app.state.human.pairing_code},
                headers={'Origin': 'http://127.0.0.1'}).raise_for_status()
    engine.snapshot = Mock(side_effect=ValueError('PRIVATE_PROMPT'))
    response = client.get('/api/tasks/TEST-1?secret=QUERY_SECRET')
    assert response.status_code == 500
    reference = response.headers['x-request-id']
    assert reference in response.json()['detail']
    logs = client.get('/api/diagnostics').text
    assert reference in logs
    assert 'request.failed' in logs
    assert 'PRIVATE_PROMPT' not in logs
    assert 'QUERY_SECRET' not in logs

