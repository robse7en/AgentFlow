"""Public Jira routes and launch context against a localhost HTTPS fixture."""
import json
import shutil
import ssl
import subprocess
import threading
from contextlib import contextmanager
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

from support import new_key


@contextmanager
def jira_server(tmp_path):
    openssl = Path(shutil.which('openssl') or 'C:/Program Files/Git/usr/bin/openssl.exe')
    def run(*args):
        subprocess.run([str(openssl), *args], cwd=tmp_path, check=True, capture_output=True)
    run('req', '-x509', '-newkey', 'rsa:2048', '-nodes', '-keyout', 'ca.key',
        '-out', 'ca.pem', '-days', '1', '-subj', '/CN=AgentFlow Local Fixture CA',
        '-addext', 'keyUsage=critical,keyCertSign,cRLSign')
    run('req', '-newkey', 'rsa:2048', '-nodes', '-keyout', 'server.key',
        '-out', 'server.csr', '-subj', '/CN=localhost')
    (tmp_path / 'extensions').write_text(
        'subjectAltName=DNS:localhost,IP:127.0.0.1\nbasicConstraints=CA:FALSE\n'
        'keyUsage=digitalSignature,keyEncipherment\nextendedKeyUsage=serverAuth\n'
        'subjectKeyIdentifier=hash\nauthorityKeyIdentifier=keyid,issuer\n')
    run('x509', '-req', '-in', 'server.csr', '-CA', 'ca.pem', '-CAkey', 'ca.key',
        '-CAcreateserial', '-out', 'server.pem', '-days', '1', '-extfile', 'extensions')
    requests = []
    class Handler(BaseHTTPRequestHandler):
        def log_message(self, *args):
            pass
        def do_GET(self):
            requests.append((self.path, self.headers.get('Authorization'), None))
            if '/attachment/content/' in self.path:
                payload = b'abc'
            elif '/issue/picker' in self.path:
                payload = {'sections': [{'label': 'Current Search', 'issues': [
                    {'key': 'DE-1', 'summaryText': 'Fixture issue', 'img': '/icon'}]}]}
            elif '/comment' in self.path:
                payload = {'comments': [], 'total': 0}
            elif '/transitions' in self.path:
                payload = {'transitions': [{'id': '31', 'to': {'name': 'In Progress'}}]}
            else:
                payload = {'fields': {'summary': 'Fixture issue', 'status': {'name': 'To Do'},
                    'attachment': [{'id': '9', 'filename': '../../evil.png', 'size': 3,
                    'content': base + '/rest/api/3/attachment/content/9'}]},
                    'renderedFields': {'description': '<p>Local fixture context</p>'}}
            body = payload if isinstance(payload, bytes) else json.dumps(payload).encode()
            self.send_response(200)
            self.send_header('Content-Length', str(len(body)))
            self.end_headers()
            self.wfile.write(body)
        def do_POST(self):
            body = json.loads(self.rfile.read(int(self.headers.get('Content-Length', '0'))))
            requests.append((self.path, self.headers.get('Authorization'), body))
            self.send_response(204)
            self.end_headers()
    server = ThreadingHTTPServer(('127.0.0.1', 0), Handler)
    ctx = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    ctx.load_cert_chain(tmp_path / 'server.pem', tmp_path / 'server.key')
    server.socket = ctx.wrap_socket(server.socket, server_side=True)
    base = f'https://localhost:{server.server_port}'
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    try:
        yield base, requests, tmp_path / 'ca.pem'
    finally:
        server.shutdown()
        server.server_close()
        thread.join()


def test_jira_picker_creation_and_architect_resync(launch, repo, tmp_path, monkeypatch):
    with jira_server(tmp_path) as (base, requests, ca):
        monkeypatch.setenv('SSL_CERT_FILE', str(ca))
        with launch(jira={'enabled': True, 'baseUrl': base, 'email': 'fixture@example.test',
                         'apiToken': 'fixture-token', 'maxFramesPerVideo': 0,
                         'autoTransitionStatus': 'In Progress'}, STUB_MANUAL='1') as backend:
            human = backend.human()
            results = human.ok('GET', '/api/jira/search?query=DE')
            assert results == [{'key': 'DE-1', 'summary': 'Fixture issue',
                               'iconUrl': base + '/icon', 'section': 'Current Search'}]
            key = new_key('JIRA')
            snap = human.create_task(key, repo)
            context = Path(snap['taskDir']) / 'context'
            assert (context / 'attachments/9-evil.png').read_bytes() == b'abc'
            assert 'Local fixture context' in (context / 'jira-issue.md').read_text()
            first = json.loads((context / 'jira-manifest.json').read_text())
            assert first['syncGeneration'] == 1
            human.run(key, 'architect')
            human.wait_node(key, 'architect', 'RUNNING')
            second = json.loads((context / 'jira-manifest.json').read_text())
            assert second['syncGeneration'] == 2 and second['lastPromptedGeneration'] == 2
            assert sum('/attachment/content/' in path for path, _, _ in requests) == 1
            assert any(body == {'transition': {'id': '31'}} for _, _, body in requests)
            assert all(auth and auth.startswith('Basic ') for _, auth, _ in requests)
            human.ok('POST', f'/api/tasks/{key}/cancel')
