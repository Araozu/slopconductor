"""Real two-daemon scoped peering, event/revocation and ambiguous delivery checks."""
import http.server
import http.client
import json
import os
from pathlib import Path
import re
import socket
import sqlite3
import subprocess
import threading
import time
import urllib.error
import urllib.request


def check_peers(daemon_binary, cli_binary, root):
    from smoke import isolated_env, start_daemon, stop_daemon
    from chat_smoke import request, wait_turn

    root.mkdir(parents=True)
    gate = threading.Event()
    held = threading.Event()
    prompts = []
    lock = threading.Lock()

    class Provider(http.server.BaseHTTPRequestHandler):
        def do_POST(self):
            payload = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
            text = next((m['content'] for m in reversed(payload['messages']) if m['role'] == 'user'), '')
            with lock:
                prompts.append(text)
            if text == 'held while gateway exits':
                held.set()
                gate.wait(30)
            body = json.dumps({'choices': [{'finish_reason': 'stop', 'message': {'content': 'owner reply: ' + str(text)}}],
                               'usage': {'prompt_tokens': 8, 'completion_tokens': 4}}).encode()
            try:
                self.send_response(200)
                self.send_header('Content-Type', 'application/json')
                self.send_header('Content-Length', str(len(body)))
                self.end_headers()
                self.wfile.write(body)
            except (BrokenPipeError, ConnectionResetError):
                pass

        def log_message(self, *_):
            pass

    provider = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Provider)
    provider_thread = threading.Thread(target=provider.serve_forever, daemon=True)
    provider_thread.start()
    a = b = alog = blog = proxy = proxy_thread = None
    followers = []
    envs = {}
    for name in ('a', 'b'):
        env = isolated_env(root)
        for key in ('OPENCODE_GO_API_KEY', 'OPENCODE_ZEN_API_KEY', 'CODEX_API_KEY', 'OPENAI_API_KEY', 'SLOP_PROVIDER_BASE_URL', 'SLOP_OPENCODE_ZEN_BASE_URL', 'SLOP_CODEX_BASE_URL'):
            env.pop(key, None)
        env.update(OPENCODE_GO_API_KEY='offline-peer-fixture', SLOP_PROVIDER_BASE_URL=f'http://127.0.0.1:{provider.server_port}/v1')
        envs[name] = env

    def call(endpoint, token, method, path, body=None, expected=None):
        if expected is None:
            expected = 202 if method == "POST" and (path.endswith("/sessions") or path.endswith("/messages") or path.endswith("/cancel")) else 200
        status, value = request(endpoint, token, method, body, path)
        assert status == expected, (method, path, status, value)
        return value

    def cli(name, *args, ok=True):
        endpoint, token = (aurl, atoken) if name == 'a' else (burl, btoken)
        process = subprocess.run([str(cli_binary), '--daemon', endpoint, '--token-file', str(token), '--json', *args],
                                 env=envs[name], capture_output=True, text=True, timeout=20)
        assert (process.returncode == 0) == ok, (args, process.stdout, process.stderr)
        if not ok:
            return process.stderr
        value = json.loads(process.stdout)
        return value["receipt"] if isinstance(value, dict) and value.get("type") == "receipt" else value

    try:
        b, burl, blog = start_daemon(daemon_binary, root / 'b', data_dir=root / 'b-data', extra=('--peer-listen', '127.0.0.1:0'), env=envs['b'])
        a, aurl, alog = start_daemon(daemon_binary, root / 'a', data_dir=root / 'a-data', env=envs['a'])
        peer_url = re.search(r'Peer API listening on (http://127\.0\.0\.1:\d+)', (root / 'b/daemon.log').read_text()).group(1)
        peer_address = peer_url.removeprefix('http://')
        atoken, btoken = root / 'a-data/credentials/local-api-token', root / 'b-data/credentials/local-api-token'
        asecret, bsecret = atoken.read_text().strip(), btoken.read_text().strip()
        bnode = cli('b', 'node')['node_id']
        read_file, control_file = root / 'read-token', root / 'control-token'
        grant = cli('b', 'peer', 'grant', '--label', 'work-read', '--token-output', str(read_file), '--command-id', 'read-grant')
        assert grant['owner_node_id'] == bnode and grant['scope'] == 'read'
        assert cli('b', 'peer', 'grant', '--label', 'work-read', '--token-output', str(read_file), '--command-id', 'read-grant') == grant
        read_secret = read_file.read_text().strip()
        call(peer_url, bsecret, 'GET', '/v1/node', expected=401)
        call(burl, read_secret, 'GET', '/v1/node', expected=401)
        connection = http.client.HTTPConnection('127.0.0.1', int(peer_url.rsplit(':', 1)[1]), timeout=5)
        connection.putrequest('GET', '/v1/node')
        connection.putheader('Authorization', 'Bearer ' + read_secret)
        connection.putheader('Authorization', 'Bearer ' + read_secret)
        connection.endheaders()
        duplicate_auth = connection.getresponse()
        assert duplicate_auth.status == 401
        duplicate_auth.read()
        connection.close()
        for path in ('/v1/providers', '/v1/filesystem', '/v1/peers', '/docs', '/api-docs/openapi.json'):
            call(peer_url, read_secret, 'GET', path, expected=404)
        cli('a', 'peer', 'add', 'home-read', '--origin', peer_url, '--owner-node-id', bnode, '--credential-file', str(read_file))
        assert cli('a', 'peer', 'probe', 'home-read')['node']['node_id'] == bnode
        assert cli('a', '--peer', 'home-read', 'node')['node_id'] == bnode
        assert 'provider-credentials' not in cli('a', '--peer', 'home-read', 'status')['capabilities']
        sid = call(burl, bsecret, 'POST', '/v1/sessions', {'command_id': 'peer-session', 'provider': 'opencode-go', 'model': 'glm-5.3-flash'})['session_id']
        initial = call(burl, bsecret, 'POST', f'/v1/sessions/{sid}/messages', {'command_id': 'initial', 'text': 'initial'})
        wait_turn(burl, bsecret, initial['turn_id'], {'completed'})
        assert cli('a', '--peer', 'home-read', 'session', 'list')['items'][0]['owner_node_id'] == bnode
        assert 'initial' in json.dumps(cli('a', '--peer', 'home-read', 'session', 'history', sid))
        assert cli('a', '--peer', 'home-read', 'turn', 'show', initial['turn_id'])['status'] == 'completed'
        denied = cli('a', '--peer', 'home-read', 'session', 'send', sid, '--text', 'must not execute', '--detach', '--command-id', 'denied', ok=False)
        assert 'peer_read_only' in denied
        for operation in ('pause', 'resume', 'cancel'):
            call(aurl, asecret, 'POST', f'/v1/peers/home-read/turns/{initial["turn_id"]}/{operation}', {'command_id': 'denied-' + operation}, expected=403)
        for method, path in (('POST', '/v1/sessions'), ('POST', f'/v1/sessions/{sid}/fork'), ('GET', '/v1/peers/home/node')):
            status, _ = request(peer_url, read_secret, method, {}, path)
            assert status in (403, 404, 405), (path, status)
        cli('b', 'peer', 'grant', '--label', 'work-control', '--scope', 'control', '--token-output', str(control_file), '--command-id', 'control-grant')
        control_secret = control_file.read_text().strip()
        wrong_owner = urllib.request.Request(peer_url + '/v1/node', headers={'Authorization': 'Bearer ' + control_secret, 'x-slop-expected-owner-node-id': 'replaced-owner'})
        try:
            urllib.request.urlopen(wrong_owner, timeout=5)
            raise AssertionError('owner precondition was ignored')
        except urllib.error.HTTPError as error:
            assert error.code == 409
        cli('a', 'peer', 'add', 'home', '--origin', peer_url, '--owner-node-id', bnode, '--credential-file', str(control_file))
        cli('a', 'peer', 'add', 'wrong-owner', '--origin', peer_url, '--owner-node-id', 'wrong-node', '--credential-file', str(control_file))
        assert 'peer_identity_mismatch' in cli('a', 'peer', 'probe', 'wrong-owner', ok=False)
        cli('a', 'peer', 'add', 'bad-http', '--origin', 'http://example.com', '--owner-node-id', bnode, '--credential-file', str(control_file), ok=False)
        for origin in (peer_url + '/v1', peer_url + '?secret=x', 'https://user:secret@example.com'):
            call(aurl, asecret, 'PUT', '/v1/peers/bad-origin', {'origin': origin, 'owner_node_id': bnode, 'token': control_secret}, expected=400)
        # The CLI's existing follow renderer reads canonical messages and can detach.
        follower = subprocess.Popen([str(cli_binary), '--daemon', aurl, '--token-file', str(atoken), '--peer', 'home', '--json', 'session', 'follow', sid],
                                    env=envs['a'], stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        followers.append(follower)
        receipt = cli('a', '--peer', 'home', 'session', 'send', sid, '--text', 'remote message', '--command-id', 'remote-once', '--detach')
        wait_turn(burl, bsecret, receipt['turn_id'], {'completed'})
        repeat = cli('a', '--peer', 'home', 'session', 'send', sid, '--text', 'remote message', '--command-id', 'remote-once', '--detach')
        assert repeat == receipt
        time.sleep(0.3)
        follower.terminate()
        stdout, stderr = follower.communicate(timeout=10)
        followers.remove(follower)
        assert 'remote message' in stdout or 'owner reply' in stdout, (stdout, stderr)
        path = f'/v1/peers/home/sessions/{sid}/messages'
        call(aurl, asecret, 'POST', path, {'command_id': 'stale', 'text': 'must not execute', 'expected_revision': 0}, expected=409)
        # Steering applies at the owner's next safe boundary during active work.
        steering = cli('a', '--peer', 'home', 'session', 'send', sid, '--text', 'held while gateway exits', '--command-id', 'steering-active', '--detach')
        assert held.wait(5)
        cli('a', '--peer', 'home', 'session', 'send', sid, '--text', 'apply this remote correction', '--delivery', 'next-boundary', '--command-id', 'active-correction', '--detach')
        gate.set()
        wait_turn(burl, bsecret, steering['turn_id'], {'completed'})
        instructions = call(aurl, asecret, 'GET', f'/v1/peers/home/sessions/{sid}/instructions')['items']
        assert any(i['text'] == 'apply this remote correction' and i['status'] == 'applied' for i in instructions), instructions
        held.clear()
        gate.clear()
        # Remote pause/resume and cancellation use B's durable receipts.
        paused = cli('a', '--peer', 'home', 'session', 'send', sid, '--text', 'held while gateway exits', '--command-id', 'held', '--detach')
        assert held.wait(5)
        result = cli('a', '--peer', 'home', 'turn', 'pause', paused['turn_id'], '--command-id', 'remote-pause')
        assert result['turn_id'] == paused['turn_id']
        # Pause applies at a safe boundary, after the current provider request.
        gate.set()
        wait_turn(burl, bsecret, paused['turn_id'], {'paused'})
        result = cli('a', '--peer', 'home', 'turn', 'resume', paused['turn_id'], '--command-id', 'remote-resume')
        assert result['turn_id'] == paused['turn_id']
        wait_turn(burl, bsecret, paused['turn_id'], {'completed'})
        gate.clear()
        held.clear()
        cancelled = cli('a', '--peer', 'home', 'session', 'send', sid, '--text', 'held while gateway exits', '--command-id', 'held-cancel', '--detach')
        assert held.wait(5)
        cli('a', '--peer', 'home', 'turn', 'cancel', cancelled['turn_id'], '--command-id', 'remote-cancel')
        gate.set()
        wait_turn(burl, bsecret, cancelled['turn_id'], {'cancelled'})
        gate.clear()
        held.clear()
        paused = cli('a', '--peer', 'home', 'session', 'send', sid, '--text', 'held while gateway exits', '--command-id', 'held-gateway-exit', '--detach')
        assert held.wait(5)
        # Kill only the gateway; the already accepted owner inference survives.
        stop_daemon(a, alog)
        a = alog = None
        gate.set()
        wait_turn(burl, bsecret, paused['turn_id'], {'completed'})
        a, aurl, alog = start_daemon(daemon_binary, root / 'a-restart', data_dir=root / 'a-data', env=envs['a'])
        assert cli('a', 'peer', 'probe', 'home')['node']['node_id'] == bnode
        with sqlite3.connect(root / 'a-data/state.sqlite3') as db:
            assert db.execute('SELECT COUNT(*) FROM sessions').fetchone()[0] == 0
        # Stream admission is bounded independently of execution/local API work.
        stream_url = aurl + f'/v1/peers/home/sessions/{sid}/events?follow=true'
        stream_headers = {'Authorization': 'Bearer ' + asecret}
        bounded_streams = []
        try:
            for _ in range(4):
                bounded_streams.append(urllib.request.urlopen(urllib.request.Request(stream_url, headers=stream_headers), timeout=8))
            call(aurl, asecret, 'GET', f'/v1/peers/home/sessions/{sid}/events?follow=true', expected=429)
            assert call(aurl, asecret, 'GET', '/v1/node')['node_id'] != bnode
        finally:
            for bounded_stream in bounded_streams:
                bounded_stream.close()
        # Following streams close on grant revocation, including idle streams.
        cursor = call(burl, bsecret, 'GET', f'/v1/sessions/{sid}')['last_event_sequence']
        stream_request = urllib.request.Request(aurl + f'/v1/peers/home-read/sessions/{sid}/events?follow=true&after={cursor}', headers={'Authorization': 'Bearer ' + asecret})
        with urllib.request.urlopen(stream_request, timeout=8) as response:
            assert response.headers['x-slop-owner-node-id'] == bnode
            cli('b', 'peer', 'revoke', 'read-grant')
            revoked_at = time.monotonic()
            assert len(response.read()) < 1000
            assert time.monotonic() - revoked_at < 3
        assert 'peer_unauthorized' in cli('a', 'peer', 'probe', 'home-read', ok=False)
        assert cli('b', 'peer', 'grant', '--label', 'work-read', '--token-output', str(read_file), '--command-id', 'read-grant')['revoked']
        # A proxy deliberately discards the owner's committed mutation response.
        drop_next = threading.Event()

        class Proxy(http.server.BaseHTTPRequestHandler):
            def forward(self):
                if self.path != '/v1/health':
                    assert self.headers.get('x-slop-expected-owner-node-id') == bnode
                body = self.rfile.read(int(self.headers.get('Content-Length', '0'))) if self.command == 'POST' else None
                upstream = urllib.request.Request(peer_url + self.path, data=body, method=self.command,
                                                  headers={'Authorization': self.headers.get('Authorization', ''), 'Content-Type': 'application/json', 'x-slop-expected-owner-node-id': self.headers.get('x-slop-expected-owner-node-id', '')})
                try:
                    response = urllib.request.urlopen(upstream, timeout=10)
                except urllib.error.HTTPError as error:
                    response = error
                except urllib.error.URLError:
                    self.send_error(502)
                    return
                with response:
                    data = response.read()
                    if self.command == 'POST' and drop_next.is_set():
                        drop_next.clear()
                        self.connection.shutdown(socket.SHUT_RDWR)
                        self.connection.close()
                        return
                    self.send_response(response.status)
                    self.send_header('Content-Type', 'application/json')
                    self.send_header('Content-Length', str(len(data)))
                    self.end_headers()
                    self.wfile.write(data)
            do_GET = do_POST = forward
            def log_message(self, *_):
                pass

        proxy = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Proxy)
        proxy_thread = threading.Thread(target=proxy.serve_forever, daemon=True)
        proxy_thread.start()
        cli('a', 'peer', 'add', 'lossy', '--origin', f'http://127.0.0.1:{proxy.server_port}', '--owner-node-id', bnode, '--credential-file', str(control_file))
        payload = {'command_id': 'lost-response', 'text': 'exactly one accepted message'}
        drop_next.set()
        error = call(aurl, asecret, 'POST', f'/v1/peers/lossy/sessions/{sid}/messages', payload, expected=502)
        assert error['code'] == 'peer_delivery_unknown', error
        recovered = call(aurl, asecret, 'POST', f'/v1/peers/lossy/sessions/{sid}/messages', payload)
        wait_turn(burl, bsecret, recovered['turn_id'], {'completed'})
        history = call(burl, bsecret, 'GET', f'/v1/sessions/{sid}/messages')['items']
        assert sum(m['role'] == 'user' and m['text'] == payload['text'] for m in history) == 1
        # Revocation and outbound removal persist independently.
        with urllib.request.urlopen(urllib.request.Request(aurl + f'/v1/peers/home/sessions/{sid}/events?follow=true', headers={'Authorization': 'Bearer ' + asecret}), timeout=8) as response:
            cli('a', 'peer', 'remove', 'home')
            # Existing catch-up bytes may already have reached the socket.
            assert len(response.read()) < 1024 * 1024
        assert 'peer_removed' in cli('a', 'peer', 'probe', 'home', ok=False)
        cli('a', 'peer', 'add', 'direct', '--origin', peer_url, '--owner-node-id', bnode, '--credential-file', str(control_file))
        stop_daemon(b, blog)
        b = blog = None
        unavailable = cli('a', 'peer', 'probe', 'direct', ok=False)
        assert 'peer_unreachable' in unavailable
        # An unreachable owner does not stop unrelated local work.
        local = call(aurl, asecret, 'POST', '/v1/sessions', {'command_id': 'local-work', 'provider': 'opencode-go', 'model': 'glm-5.3-flash'})['session_id']
        local_turn = call(aurl, asecret, 'POST', f'/v1/sessions/{local}/messages', {'command_id': 'local-turn', 'text': 'local work remains usable'})
        wait_turn(aurl, asecret, local_turn['turn_id'], {'completed'})
        b, burl, blog = start_daemon(daemon_binary, root / 'b-restart', data_dir=root / 'b-data', extra=('--peer-listen', peer_address), env=envs['b'])
        call(peer_url, read_secret, 'GET', '/v1/node', expected=401)
        assert call(peer_url, control_secret, 'GET', '/v1/node')['node_id'] == bnode
        grants = cli('b', 'peer', 'grants')
        assert next(g for g in grants if g['id'] == 'read-grant')['revoked']
        assert control_secret not in json.dumps(grants) and read_secret not in json.dumps(grants)
        assert not any(p == 'must not execute' for p in prompts)
        for name in ('a', 'b'):
            database = (root / (name + '-data') / 'state.sqlite3').read_bytes()
            assert read_secret.encode() not in database and control_secret.encode() not in database
        stop_daemon(b, blog)
        b = blog = None
        b, burl, blog = start_daemon(daemon_binary, root / 'b-replaced', data_dir=root / 'b-new-data', extra=('--peer-listen', peer_address), env=envs['b'])
        replacement_token = (root / 'b-new-data/credentials/local-api-token').read_text().strip()
        call(burl, replacement_token, 'PUT', '/v1/peer-grants/replacement', {'label': 'restored credential', 'scope': 'control', 'token': control_secret})
        assert 'peer_identity_mismatch' in cli('a', 'peer', 'probe', 'direct', ok=False)

    finally:
        gate.set()
        for follower in followers:
            follower.kill()
            follower.communicate(timeout=5)
        if a is not None:
            stop_daemon(a, alog)
        if b is not None:
            stop_daemon(b, blog)
        if proxy is not None:
            proxy.shutdown()
            proxy.server_close()
            proxy_thread.join(5)
        provider.shutdown()
        provider.server_close()
        provider_thread.join(5)
