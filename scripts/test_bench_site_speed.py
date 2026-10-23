"""scripts/bench-site-speed.py: the URL set it builds and the timings it reports, against a local server."""

import contextlib
import gzip
import importlib.util
import io
import json
import threading
import unittest
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

HERE = Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location("bench", HERE / "bench-site-speed.py")
bench = importlib.util.module_from_spec(spec)
spec.loader.exec_module(bench)

HTML = b'<html><head><link rel="stylesheet" href="/assets/index-AAA.css"></head><body><script src="/assets/index-BBB.js"></script></body></html>'
SEEN = []


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def do_GET(self):
        SEEN.append(self.path)
        if self.path == "/":
            body = gzip.compress(HTML)
            headers = {"Content-Type": "text/html", "Content-Encoding": "gzip", "CF-Cache-Status": "DYNAMIC"}
        elif self.path == "/v1/meta":
            body, headers = json.dumps({"index_version": "pages-v1"}).encode(), {"Content-Type": "application/json"}
        else:
            body, headers = b"x" * 100, {"Content-Type": "text/plain"}
        self.send_response(200)
        for k, v in headers.items():
            self.send_header(k, v)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *args):
        pass


class BenchSiteSpeed(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        cls.base = f"http://127.0.0.1:{cls.server.server_address[1]}"
        threading.Thread(target=cls.server.serve_forever, daemon=True).start()

    @classmethod
    def tearDownClass(cls):
        cls.server.shutdown()
        cls.server.server_close()

    def test_asset_paths_keeps_assets_in_order_without_repeats(self):
        html = '<link href="/assets/a.css"><script src="/assets/b.js"></script><img src="/assets/a.css"><a href="/datasets">'
        self.assertEqual(bench.asset_paths(html), ["/assets/a.css", "/assets/b.js"])

    def test_search_path_pins_the_index_version(self):
        path = bench.search_path("pages-v1")
        self.assertTrue(path.startswith("/v1/aggregate?q=whigs&"))
        self.assertTrue(path.endswith("&v=pages-v1"))

    def test_p95_and_median(self):
        self.assertEqual(bench.med([3, 1, 2]), 2)
        self.assertEqual(bench.p95(list(range(1, 101))), 95)
        self.assertEqual(bench.p95([7]), 7)

    def test_sample_times_a_cold_and_a_warm_request(self):
        s = bench.sample("127.0.0.1", self.server.server_address[1], "/", tls=False)
        self.assertEqual(s["status"], 200)
        self.assertEqual(s["tls"], 0.0)
        self.assertEqual(s["cache"], "DYNAMIC")
        self.assertEqual(s["encoding"], "gzip")
        self.assertEqual(s["body"], HTML)
        self.assertGreaterEqual(s["wait"], 0)
        self.assertGreaterEqual(s["warm_wait"], 0)

    def test_run_direct_covers_pages_search_and_assets(self):
        SEEN.clear()
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            result = bench.run_direct(self.base, 2)
        names = [r["name"] for r in result["results"]]
        self.assertEqual(
            names,
            ["home", "search-page", "datasets", "meta", "search", "asset:index-AAA.css", "asset:index-BBB.js"],
        )
        self.assertTrue(all(r["status"] == [200] and len(r["runs"]) == 2 for r in result["results"]))
        self.assertTrue(any(p.startswith("/v1/aggregate?") and p.endswith("&v=pages-v1") for p in SEEN))
        self.assertIn("assets together", out.getvalue())
        self.assertEqual(set(result["assets_together"]), {"median_ms", "max_ms"})


if __name__ == "__main__":
    unittest.main()
