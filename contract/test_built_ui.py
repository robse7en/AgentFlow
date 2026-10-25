"""Actual Vue build served by either backend, with pairing still protecting the API."""
import re

import httpx
import pytest

from support import ROOT


def test_built_vue_ui_and_assets_share_the_api_origin(launch):
    index = ROOT / 'frontend/dist/index.html'
    if not index.is_file():
        pytest.skip('Run npm run build in frontend first')
    with launch() as backend, httpx.Client(base_url=backend.url, timeout=20) as client:
        for route in ['/', '/tasks/UI-42', '/settings', '/projects', '/decisions']:
            response = client.get(route)
            assert response.status_code == 200
            assert response.headers['content-type'].startswith('text/html')
            assert response.content == index.read_bytes()
        assets = re.findall(r'(?:src|href)="(/assets/[^"<>]+)"', response.text)
        assert assets, 'Built index must reference its bundled assets'
        for asset in assets:
            response = client.get(asset)
            assert response.status_code == 200
            assert response.content == (ROOT / 'frontend/dist' / asset.lstrip('/')).read_bytes()
            assert 'text/html' not in response.headers['content-type']
            head = client.head(asset)
            assert head.status_code == 200 and not head.content
            assert int(head.headers['content-length']) == len(response.content)
        assert client.get('/assets/missing.js').status_code == 404
        assert client.get('/api/tasks').status_code == 401
        human = backend.human()
        assert human.get('/api/tasks').status_code == 200
        assert client.get('/', headers={'Host': 'evil.example'}).status_code == 400
