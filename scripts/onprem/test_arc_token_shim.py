"""Tests for scripts/onprem/arc-token-shim.py: python3 -m unittest -v scripts/onprem/test_arc_token_shim.py.
Needs curl. The Arc agent is a server on the loopback interface; nothing reaches Azure."""

import http.server
import json
import os
import subprocess
import sys
import tempfile
import threading
import time
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
SHIM = os.path.join(HERE, "arc-token-shim.py")
KEY = "arc-challenge-secret"
SECRET = "0123456789abcdef0123"


class Arc(http.server.BaseHTTPRequestHandler):
    realm = ""
    requests = None

    def do_GET(self):
        self.requests.append(self.path)
        if self.headers.get("Authorization") == "Basic " + KEY:
            body = json.dumps({"access_token": "tok", "expires_on": 1900000000, "token_type": "Bearer"}).encode()
            self.send_response(200)
        else:
            body = b""
            self.send_response(401)
            self.send_header("WWW-Authenticate", "Basic realm=" + self.realm)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *args):
        pass


class Shim(unittest.TestCase):
    def start(self, realm_name="agent.key", quoted=False):
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        tokens = os.path.join(tmp.name, "tokens")
        os.makedirs(tokens)
        with open(os.path.join(tokens, "agent.key"), "w") as f:
            f.write(KEY)
        with open(os.path.join(tmp.name, "elsewhere.key"), "w") as f:
            f.write(KEY)
        self.requests = []
        realm = os.path.join(tokens if realm_name == "agent.key" else tmp.name, realm_name)
        if quoted:
            realm = '"%s"' % realm
        handler = type("A", (Arc,), {"realm": realm, "requests": self.requests})
        arc = http.server.ThreadingHTTPServer(("127.0.0.1", 0), handler)
        threading.Thread(target=arc.serve_forever, daemon=True).start()
        self.addCleanup(arc.server_close)
        self.addCleanup(arc.shutdown)
        port_file = os.path.join(tmp.name, "port")
        env = dict(os.environ, USNM_ARC_SHIM_HEADER=SECRET, USNM_ARC_TOKEN_DIR=tokens,
                   USNM_ARC_ENDPOINT="http://127.0.0.1:%d/metadata/identity/oauth2/token" % arc.server_address[1])  # fmt: skip
        proc = subprocess.Popen([sys.executable, SHIM, "--port-file", port_file], env=env, stderr=subprocess.DEVNULL)
        self.addCleanup(proc.wait)
        self.addCleanup(proc.kill)
        for _ in range(100):
            if os.path.exists(port_file):
                break
            time.sleep(0.05)
        with open(port_file) as f:
            self.port = int(f.read())

    def get(self, header=SECRET, resource="https%3A%2F%2Fstorage.azure.com"):
        out = subprocess.run(
            ["curl", "-s", "-w", "\n%{http_code}", "-H", "X-IDENTITY-HEADER: " + header,
             "http://127.0.0.1:%d/token?api-version=2019-08-01&resource=%s" % (self.port, resource)],
            capture_output=True, text=True, check=True,
        ).stdout  # fmt: skip
        body, code = out.rsplit("\n", 1)
        return int(code), json.loads(body)

    def test_it_answers_with_the_arc_token_as_azure_identity_reads_it(self):
        self.start()
        code, body = self.get()
        self.assertEqual(code, 200, body)
        self.assertEqual(body, {"access_token": "tok", "expires_on": "1900000000",
                                "resource": "https://storage.azure.com", "token_type": "Bearer"})  # fmt: skip
        self.assertIn("resource=https%3A%2F%2Fstorage.azure.com", self.requests[0])

    def test_it_takes_a_quoted_realm(self):
        self.start(quoted=True)
        self.assertEqual(self.get()[0], 200)

    def test_it_talks_to_the_agent_only_over_http_on_loopback(self):
        for endpoint in ("https://localhost:40342/x", "http://example.com:40342/x", "http://192.0.2.1:40342/x"):
            env = dict(os.environ, USNM_ARC_SHIM_HEADER=SECRET, USNM_ARC_ENDPOINT=endpoint)
            p = subprocess.run([sys.executable, SHIM, "--port-file", "/nonexistent/x"],
                               env=env, capture_output=True, text=True, check=False)  # fmt: skip
            self.assertNotEqual(p.returncode, 0, endpoint)
            self.assertIn("http on loopback", p.stderr, endpoint)

    def test_it_refuses_without_the_secret(self):
        self.start()
        self.assertEqual(self.get(header="wrong-secret-value")[0], 401)
        self.assertEqual(self.requests, [])

    def test_it_hands_out_only_storage_tokens(self):
        self.start()
        self.assertEqual(self.get(resource="https%3A%2F%2Fmanagement.azure.com%2F")[0], 400)
        self.assertEqual(self.requests, [])

    def test_it_reads_only_the_agents_challenge_files(self):
        self.start(realm_name="elsewhere.key")
        code, body = self.get()
        self.assertEqual(code, 502)
        self.assertIn("isn't one of the agent's", body["error"])

    def test_it_needs_a_secret(self):
        env = dict(os.environ, USNM_ARC_SHIM_HEADER="short")
        p = subprocess.run([sys.executable, SHIM, "--port-file", "/nonexistent/x"], env=env, capture_output=True, text=True, check=False)
        self.assertNotEqual(p.returncode, 0)
        self.assertIn("at least 16", p.stderr)


if __name__ == "__main__":
    unittest.main()
