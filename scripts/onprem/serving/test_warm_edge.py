"""warm-edge.py against a small local server standing in for the site."""

import gzip
import http.server
import importlib.util
import json
from pathlib import Path
import threading
import unittest
import urllib.parse

SPEC = importlib.util.spec_from_file_location("warm_edge", Path(__file__).with_name("warm-edge.py"))
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)


class Site(http.server.BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass

    def send_body(self, status, body, cache="MISS"):
        data = gzip.compress(json.dumps(body).encode())
        self.send_response(status)
        self.send_header("Content-Encoding", "gzip")
        self.send_header("cf-cache-status", cache)
        self.send_header("Retry-After", "0")
        self.end_headers()
        self.wfile.write(data)

    def do_GET(self):
        url = urllib.parse.urlsplit(self.path)
        self.server.seen.append(self.path)
        if url.path == "/v1/meta":
            self.send_body(200, {"index_version": "v1", "edge_generation": "f8"})
        elif url.path == "/edge-warm.json":
            self.send_body(200, self.server.queries)
        elif url.path.endswith("/coverage"):
            self.send_body(200, {})
        elif "q=busy" in url.query and self.server.seen.count(self.path) == 1:
            self.send_body(202, {})
        elif "q=bad" in url.query:
            self.send_body(400, {})
        elif "q=cached" in url.query:
            self.send_body(200, {"cube": {"baseline_ref": "/api/v1/coverage?v=v1"}}, "HIT")
        else:
            self.send_body(200, {"cube": {"baseline_ref": "/api/v1/coverage?v=v1"}})


class WarmEdge(unittest.TestCase):
    def setUp(self):
        self.server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Site)
        self.server.seen = []
        self.server.queries = []
        threading.Thread(target=self.server.serve_forever, daemon=True).start()
        self.addCleanup(self.server.server_close)
        self.addCleanup(self.server.shutdown)
        self.base = f"http://127.0.0.1:{self.server.server_address[1]}"

    def run_warm(self):
        return MODULE.run(self.base, 0, sleep=lambda _: None)

    def test_requests_the_apps_url_then_the_coverage_it_links_to(self):
        self.server.queries = ["q=gold&from=1896-01-01"]
        report = self.run_warm()
        self.assertEqual(report["cache"], {"MISS": 1})
        self.assertIn("/v1/aggregate?q=gold&from=1896-01-01&v=v1&g=f8", self.server.seen)
        self.assertIn("/v1/coverage?v=v1", self.server.seen)

    def test_counts_hits_and_failures_and_waits_out_a_computing_search(self):
        self.server.queries = ["q=gold", "q=cached", "q=busy", "q=bad"]
        report = self.run_warm()
        self.assertEqual(report["cache"], {"MISS": 2, "HIT": 1, "FAILED": 1})
        self.assertEqual(self.server.seen.count("/v1/aggregate?q=busy&v=v1&g=f8"), 2)

    def test_a_computing_search_is_polled_to_the_time_limit_not_a_try_limit(self):
        self.server.seen = []
        url = self.base + "/v1/aggregate?q=slow"
        calls = {"n": 0}

        class Response:
            status = 202
            headers = {}

            def __enter__(self):
                calls["n"] += 1
                return self

            def __exit__(self, *_):
                return False

            def read(self):
                return b""

        original = MODULE.urllib.request.urlopen
        MODULE.urllib.request.urlopen = lambda *a, **k: Response()
        self.addCleanup(setattr, MODULE.urllib.request, "urlopen", original)
        now = iter(range(0, 10_000, 30))
        status, _, _ = MODULE.fetch(url, sleep=lambda _: None, clock=lambda: next(now))
        self.assertEqual(status, 202)
        self.assertEqual(calls["n"], MODULE.COMPUTING_LIMIT_SECS // 30)

    def test_a_site_without_a_generation_is_an_error(self):
        original = MODULE.fetch
        MODULE.fetch = lambda *_: (200, b'{"index_version": "v1"}', "NONE")
        self.addCleanup(setattr, MODULE, "fetch", original)
        with self.assertRaisesRegex(RuntimeError, "edge_generation"):
            self.run_warm()


if __name__ == "__main__":
    unittest.main()
