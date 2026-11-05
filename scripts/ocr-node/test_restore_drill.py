"""Restore drill tests against a loopback archive; no Azure, ZFS or systemd."""
import base64
import hashlib
import http.server
import json
import os
from pathlib import Path
import socket
import subprocess
import tempfile
import threading
import time
import unittest

SCRIPT = Path(__file__).with_name("restore-drill.sh")
DATASET = "tank/usnm-restore-drill-test"
TOKEN = "drill-token-5f1c"
EDITION = "keep/b1/sn1/1900-01-01/1"


class Archive(http.server.ThreadingHTTPServer):
    """The Arc token challenge and a Cold `scans` container, on loopback."""

    def __init__(self, scans, key):
        self.scans, self.key = scans, key
        self.requests, self.corrupt = [], set()
        super().__init__(("127.0.0.1", 0), Handler)


class Handler(http.server.BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass

    def reply(self, code, headers=(), body=b""):
        self.send_response(code)
        for k, v in headers:
            self.send_header(k, v)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        if self.command == "GET":
            self.wfile.write(body)

    def blob(self):
        self.server.requests.append((self.command, self.path))
        if self.headers.get("Authorization") != f"Bearer {TOKEN}":
            return self.reply(403)
        name = self.path.split("/scans/", 1)[1]
        data = (self.server.scans / name).read_bytes()
        headers = [("Content-MD5", base64.b64encode(hashlib.md5(data).digest()).decode()),
                   ("x-ms-meta-sha256", hashlib.sha256(data).hexdigest()),
                   ("x-ms-access-tier", "Cold")]
        if name in self.server.corrupt:
            data = data[:-1] + bytes([data[-1] ^ 1])
        self.send_response(200)
        for k, v in headers:
            self.send_header(k, v)
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        if self.command == "GET":
            self.wfile.write(data)

    do_HEAD = blob

    def do_GET(self):
        if not self.path.startswith("/token"):
            return self.blob()
        self.server.requests.append(("GET", "/token"))
        if self.headers.get("Authorization") == "Basic " + self.server.key.read_text():
            return self.reply(200, body=json.dumps({"access_token": TOKEN}).encode())
        self.reply(401, [("WWW-Authenticate", f"Basic realm={self.server.key}")])


class RestoreDrillTest(unittest.TestCase):
    def setUp(self):
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        self.root = root = Path(tmp.name)
        for d in ("bin", "etc/usnm", "var/lib/usnm-scan-archive", "var/opt/azcmagent/tokens",
                  "tank/ocr/scans/keep", "tank/ocr/scans/.zfs/snapshot/snap/keep", "drill"):
            (root / d).mkdir(parents=True)
        self.scans = root / "tank/ocr/scans"
        self.snapshot = self.scans / ".zfs/snapshot/snap"
        self.manifest = root / "var/lib/usnm-scan-archive/archived.tsv"
        self.manifest.touch()
        key = root / "var/opt/azcmagent/tokens/0a1b-2c3d.key"
        key.write_text("arc-secret")
        self.archive = Archive(self.scans, key)
        threading.Thread(target=self.archive.serve_forever, daemon=True).start()
        self.addCleanup(self.archive.server_close)
        self.addCleanup(self.archive.shutdown)
        port = self.archive.server_address[1]
        (root / "etc/usnm/scan-archive.env").write_text(
            f"USNM_ARCHIVE_URL=http://127.0.0.1:{port}/scans\nUSNM_ARCHIVE_SCANS='/tank/ocr/scans'\n")
        self.stub("ionice", "exit 0\n")
        self.stub("renice", "exit 0\n")
        self.stub("findmnt", 'echo "$STUB_MOUNT"\n')
        self.env = dict(os.environ, PATH=f"{root / 'bin'}{os.pathsep}{os.environ['PATH']}",
                        USNM_OCR_TEST_ROOT=str(root), STUB_MOUNT=f"{DATASET} zfs",
                        USNM_OCR_TEST_TOKEN_URL=f"http://127.0.0.1:{port}/token?api-version=x")

    def stub(self, name, body):
        path = self.root / "bin" / name
        path.write_text("#!/usr/bin/env bash\n" + body)
        path.chmod(0o755)

    def scan(self, page, ext="jp2", md5=True, size=3000):
        """A scan with its sidecar, in the keep set, the snapshot and the manifest."""
        name = f"{EDITION}/{page}.{ext}"
        data = hashlib.sha256(name.encode()).digest() * (size // 32)
        side = dict(bytes=len(data), sha256=hashlib.sha256(data).hexdigest(),
                    format="jp2" if ext == "jp2" else "jpeg")
        if md5:
            side["md5"] = hashlib.md5(data).hexdigest()
        for base in (self.scans, self.snapshot):
            (base / name).parent.mkdir(parents=True, exist_ok=True)
            (base / name).write_bytes(data)
        (self.scans / f"{name}.json").write_text(json.dumps(side))
        with self.manifest.open("a") as f:
            f.write(f"{name}\t{len(data)}\t0\t0\t0\n")
        return name

    def run_drill(self, *extra, count=4, **env):
        return subprocess.run(
            ["bash", str(SCRIPT), "--dir", str(self.root / "drill"), "--dataset", DATASET,
             "--snapshot", "snap", "--count", str(count), *extra],
            env={**self.env, **env}, capture_output=True, text=True, timeout=60)

    def gets(self):
        return [p for m, p in self.archive.requests if m == "GET" and p != "/token"]

    def test_restores_a_mixed_sample_and_checks_every_copy(self):
        names = [self.scan(1), self.scan(2), self.scan(3, ext="jpg", md5=False), self.scan(4)]
        (self.snapshot / names[3]).unlink()  # archived after the snapshot
        r = self.run_drill()
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        self.assertIn("4 of 4 match (snapshot copies: 3; JPEGs without a sidecar md5: 1)", r.stdout)
        self.assertIn("check 3: ok (2976 bytes, not in the snapshot)", r.stdout)
        self.assertEqual(len(self.gets()), 4)
        for i, name in enumerate(names):
            self.assertEqual((self.root / f"drill/cold/{i}").read_bytes(), (self.scans / name).read_bytes())
        self.assertNotIn("keep/", r.stdout + r.stderr)
        self.assertNotIn(TOKEN, r.stdout + r.stderr)

    def test_a_jp2_sidecar_without_its_md5_fails(self):
        self.scan(1)
        self.scan(2, md5=False)
        r = self.run_drill(count=2)
        self.assertEqual(r.returncode, 1)
        self.assertIn("check 1: FAILED sidecar-md5-missing", r.stdout)

    def test_cold_bytes_that_differ_fail(self):
        self.scan(1)
        self.archive.corrupt.add(self.scan(2))
        r = self.run_drill(count=2)
        self.assertEqual(r.returncode, 1)
        self.assertIn("check 1: FAILED sidecar-sha256 sidecar-md5 blob-sha256 blob-md5 "
                      "live-sha256 live-bytes snapshot-bytes", r.stdout)
        self.assertIn("1 restored scans did not match", r.stderr)

    def test_the_budget_stops_before_the_first_scan_past_it(self):
        for page in (1, 2, 3):
            self.scan(page, size=1000)
        r = self.run_drill("--budget-bytes", "2000", count=3)
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        self.assertIn("budget: stopping before candidate 2 (1984 + 992 > 2000 bytes)", r.stdout)
        self.assertEqual(len(self.gets()), 2)

    def test_the_token_endpoint_must_be_ipv4_loopback(self):
        self.scan(1)
        port = self.archive.server_address[1]
        r = self.run_drill(count=1, USNM_OCR_TEST_TOKEN_URL=f"http://localhost:{port}/token")
        self.assertEqual(r.returncode, 1)
        self.assertIn("must be http://127.0.0.1:<port>/, never localhost", r.stderr)
        self.assertEqual(self.archive.requests, [])

    def test_an_unanswered_token_request_times_out(self):
        self.scan(1)
        with socket.socket() as silent:  # accepts the connection, never answers
            silent.bind(("127.0.0.1", 0))
            silent.listen(4)
            started = time.monotonic()
            r = self.run_drill(count=1, USNM_OCR_TEST_TOKEN_TIMEOUT="2",
                               USNM_OCR_TEST_TOKEN_URL=f"http://127.0.0.1:{silent.getsockname()[1]}/token")
        self.assertEqual(r.returncode, 1)
        self.assertIn("token challenge failed", r.stderr)
        self.assertLess(time.monotonic() - started, 20)

    def test_writes_only_into_the_empty_scratch_dataset(self):
        self.scan(1)
        with self.subTest("another mount"):
            r = self.run_drill(count=1, STUB_MOUNT="tank/ocr/scans zfs")
            self.assertEqual(r.returncode, 1)
            self.assertIn("is not the mountpoint of", r.stderr)
        with self.subTest("not empty"):
            (self.root / "drill/left").touch()
            r = self.run_drill(count=1)
            self.assertEqual(r.returncode, 1)
            self.assertIn("is not empty", r.stderr)
        self.assertEqual(self.archive.requests, [])


if __name__ == "__main__":
    unittest.main()
