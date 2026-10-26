"""scripts/bench-site-speed.py: the URL set it builds and the timings it reports, against a local server."""

import contextlib
import gzip
import importlib.util
import io
import json
import socket
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
DNT = []
HOSTS = []


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def do_GET(self):
        SEEN.append(self.path)
        DNT.append(self.headers.get("DNT"))
        HOSTS.append(self.headers.get("Host"))
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
        if self.path == "/close":
            self.send_header("Connection", "close")
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

    def test_sample_asks_not_to_be_tracked(self):
        DNT.clear()
        bench.sample("127.0.0.1", self.server.server_address[1], "/v1/meta", tls=False)
        self.assertEqual(DNT, ["1", "1"])

    def test_sample_keeps_the_port_in_the_host_header(self):
        HOSTS.clear()
        bench.sample("127.0.0.1", self.server.server_address[1], "/", tls=False)
        self.assertEqual(HOSTS, [f"127.0.0.1:{self.server.server_address[1]}"] * 2)

    def test_sample_has_no_warm_time_when_the_server_closes_the_connection(self):
        SEEN.clear()
        s = bench.sample("127.0.0.1", self.server.server_address[1], "/close", tls=False)
        self.assertIsNone(s["warm_wait"])
        self.assertEqual(SEEN, ["/close"])
        self.assertEqual(bench.warm_cell([s]), "-")

    def test_warm_cell_is_the_median_of_the_runs_that_had_one(self):
        self.assertEqual(bench.warm_cell([{"warm_wait": 0.010}, {"warm_wait": None}, {"warm_wait": 0.030}]), 20.0)

    def test_tls_context_is_built_once_with_a_floor(self):
        self.assertIs(bench.tls_context(), bench.tls_context())
        self.assertGreaterEqual(bench.tls_context().minimum_version, bench.ssl.TLSVersion.TLSv1_2)

    def test_sample_without_warm_makes_one_request(self):
        SEEN.clear()
        s = bench.sample("127.0.0.1", self.server.server_address[1], "/x", tls=False, warm=False)
        self.assertNotIn("warm_wait", s)
        self.assertEqual(SEEN, ["/x"])

    def test_connect_falls_back_to_the_next_address(self):
        live = self.server.server_address[1]
        infos = [
            (socket.AF_INET, socket.SOCK_STREAM, 6, "", ("127.0.0.1", 1)),  # nothing listens here
            (socket.AF_INET, socket.SOCK_STREAM, 6, "", ("127.0.0.1", live)),
        ]
        sock, seconds = bench.connect(infos)
        try:
            self.assertEqual(sock.getpeername()[1], live)
            self.assertGreaterEqual(seconds, 0)
        finally:
            sock.close()

    def test_connect_raises_the_last_error_when_none_answers(self):
        infos = [(socket.AF_INET, socket.SOCK_STREAM, 6, "", ("127.0.0.1", 1))]
        with self.assertRaises(OSError):
            bench.connect(infos)

    def test_globalping_row_counts_missing_phases_as_zero(self):
        probe = {"continent": "EU", "country": "DE", "city": "Falkenstein", "network": "x"}
        result = {
            "status": "finished",
            "statusCode": 200,
            "timings": {"dns": None, "tcp": 20, "tls": None, "firstByte": 30, "download": 1, "total": 51},
            "headers": {},
        }
        row = bench.globalping_row({"probe": probe, "result": result})
        self.assertEqual((row["dns"], row["tls"], row["ttfb"], row["dl"], row["total"]), (0, 0, 50, 1, 51))
        self.assertFalse(row["truncated"])

    def test_globalping_row_drops_download_and_total_of_a_truncated_body(self):
        probe = {"country": "IN"}
        result = {
            "status": "finished",
            "statusCode": 200,
            "truncated": True,
            "timings": {"dns": 5, "tcp": 100, "tls": 100, "firstByte": 100, "download": 1, "total": 306},
        }
        row = bench.globalping_row({"probe": probe, "result": result})
        self.assertTrue(row["truncated"])
        self.assertIsNone(row["dl"])
        self.assertIsNone(row["total"])
        self.assertEqual(row["ttfb"], 305)

    def test_globalping_row_reports_a_probe_that_failed(self):
        row = bench.globalping_row({"probe": {"country": "ZA"}, "result": {"status": "failed", "rawOutput": "timeout"}})
        self.assertEqual(row["error"], "timeout")

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
        # Each asset: a cold and a warm request per run in the table, one cold request per run in the
        # assets-together time. Two downloads in that time would make it 8, not 6.
        self.assertEqual(SEEN.count("/assets/index-AAA.css"), 2 * 2 + 2)
        self.assertEqual(set(result["assets_together"]), {"median_ms", "max_ms"})


if __name__ == "__main__":
    unittest.main()
