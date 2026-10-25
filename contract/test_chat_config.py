"""Offline Slack boundary contracts: startup/pairing, validation, restart requirements.

Chat stays disabled or lacks an operator ID, so neither target can contact Slack.
The Socket Mode/action delivery boundary is covered by the named Rust localhost tests.
"""
import json

import pytest


@pytest.fixture(scope='module')
def backend(launch):
    with launch(slack={'enabled': False}) as backend:
        yield backend


@pytest.fixture(scope='module')
def human(backend):
    return backend.human()


@pytest.mark.parametrize('field,value', [
    ('userId', '@rob'),
    ('digestAt', '9:00'),
    ('digestAt', '24:00'),
    ('digestAt', '09:60'),
])
def test_invalid_chat_settings_are_rejected_without_changing_the_running_app(backend, human, field, value):
    original = backend.config_file.read_bytes()
    try:
        raw = json.loads(original)
        raw['slack'][field] = value
        backend.config_file.write_text(json.dumps(raw), encoding='utf-8')
        response = human.post('/api/config/reload')
        assert response.status_code == 400
        assert 'slack' in response.text and field in response.text
        assert human.ok('GET', '/api/tasks') == []
    finally:
        backend.config_file.write_bytes(original)
    assert human.post('/api/config/reload').status_code == 200


def test_chat_changes_require_a_restart(backend, human):
    original = backend.config_file.read_bytes()
    try:
        raw = json.loads(original)
        raw['slack']['details'] = ['cost']
        backend.config_file.write_text(json.dumps(raw), encoding='utf-8')
        response = human.post('/api/config/reload')
        assert response.status_code == 409
        assert response.json() == {'detail': 'Restart AgentFlow to change Slack configuration'}
    finally:
        backend.config_file.write_bytes(original)
    assert human.post('/api/config/reload').status_code == 200


def test_unconfigured_chat_leaves_startup_and_pairing_available(launch):
    # Missing operator ID guarantees transport cannot start, even if token env vars exist.
    with launch(slack={'enabled': True, 'userId': None}) as backend:
        human = backend.human()
        assert human.ok('GET', '/api/tasks') == []
        assert human.ok('GET', '/api/config')['configPath'] == str(backend.config_file)
        assert human.post('/api/config/reload').status_code == 200
