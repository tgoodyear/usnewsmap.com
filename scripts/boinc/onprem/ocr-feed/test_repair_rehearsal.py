"""Disposable process/HTTP rehearsal; see docs/boinc-quality.md for the opt-in command.

No patched production functions: the real producer CLI publishes to serve-scans,
the Rust prefetch entry point downloads from a loopback stand-in, and the dashboard
reads the resulting queue/index. This does not certify deployed historical binaries.
"""
import contextlib
import datetime as dt
import hashlib
import http.client
import http.server
import json
import os
from pathlib import Path
import selectors
import shutil
import subprocess
import sys
import tempfile
import threading
import time
import unittest
import urllib.parse
import urllib.request

sys.path.insert(0, str(Path(__file__).resolve().parent))
import ocr_manifest as manifest

sys.path.insert(0, str(Path(__file__).resolve().parent.parent / "quality-feed"))
import scan_quarantine as quarantine


def sha(body):
    return hashlib.sha256(body).hexdigest()


def md5(body):
    return hashlib.md5(body, usedforsecurity=False).hexdigest()


class DecodeError(Exception):
    original_type = "OSError"


class QuietHandler(http.server.BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass

    def reply(self, body, status=200, headers=()):
        self.send_response(status)
        self.send_header("Content-Length", str(len(body)))
        for key, value in headers:
            self.send_header(key, value)
        self.end_headers()
        with contextlib.suppress(BrokenPipeError, ConnectionResetError):
            self.wfile.write(body)


class Source:
    """Synthetic bag, METS and images; all bytes are generated locally."""
    def __init__(self):
        self.files = {}
        self.hits = []
        self.wait_path = None
        self.started = threading.Event()
        self.release = threading.Event()
        self.retry_once = None
        batch = "/storage-services/service/ndnp/xx/batch_xx_test_ver01/"
        issue = "data/sn1/1/1900010201/"
        dmd, files, divs = [], [], []
        for seq in range(1, 21):
            frame = f"{seq:04}"
            dmd.append(f'<dmdSec ID="p{seq}"><mdWrap MDTYPE="MODS"><xmlData><MODS:mods>'
                       f'<MODS:part><MODS:extent unit="pages"><MODS:start>{seq}</MODS:start>'
                       '</MODS:extent></MODS:part></MODS:mods></xmlData></mdWrap></dmdSec>')
            files.append(f'<fileGrp><file ID="f{seq}" USE="service">'
                         f'<FLocat xlink:href="./{frame}.jp2"/></file></fileGrp>')
            divs.append(f'<div TYPE="np:page" DMDID="p{seq}"><fptr FILEID="f{seq}"/></div>')
            self.files[batch + issue + frame + ".jp2"] = self.body(seq)
        mets = ('<mets xmlns:MODS="http://www.loc.gov/mods/v3" '
                'xmlns:xlink="http://www.w3.org/1999/xlink" xmlns="http://www.loc.gov/METS/">'
                + "".join(dmd) + "<fileSec>" + "".join(files) + "</fileSec>"
                '<structMap><div TYPE="np:issue">' + "".join(divs) + "</div></structMap></mets>").encode()
        self.files[batch + issue + "1900010201.xml"] = mets
        self.files[batch + "manifest-md5.txt"] = "".join(
            f"{md5(body)}  {path.removeprefix(batch)}\n"
            for path, body in self.files.items()).encode()
        source = self

        class Handler(QuietHandler):
            def do_GET(self):
                path = urllib.parse.urlsplit(self.path).path
                source.hits.append((time.monotonic(), path, self.headers.get("User-Agent")))
                if path == source.retry_once:
                    source.retry_once = None
                    self.reply(b"", 429, (("Retry-After", "1"),))
                    return
                if path == source.wait_path:
                    source.started.set()
                    if not source.release.wait(90):
                        self.reply(b"", 503)
                        return
                if path.startswith("/item/"):
                    pages = [[{"mimetype": "image/jp2", "url":
                               f"https://tile.loc.gov/storage-services/service/ndnp/x/"
                               f"batch_x/data/sn1/1/1900010201/{seq:04}.jp2"}]
                             for seq in range(1, 21)]
                    self.reply(json.dumps({"resources": [{"files": pages}]}).encode())
                elif path.startswith("/iiif/"):
                    frame = path.split("/")[2].rsplit(":", 1)[-1]
                    self.reply(source.body(int(frame)))
                else:
                    self.reply(source.files.get(path, b""), 200 if path in source.files else 404)

        self.server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.thread = threading.Thread(target=self.server.serve_forever)
        self.thread.start()
        self.url = f"http://127.0.0.1:{self.server.server_port}"

    @staticmethod
    def body(seq):
        return f"synthetic repair scan {seq}\n".encode()

    @staticmethod
    def image_path(seq):
        return f"/storage-services/service/ndnp/xx/batch_xx_test_ver01/data/sn1/1/1900010201/{seq:04}.jp2"

    def close(self):
        self.release.set()
        self.server.shutdown()
        self.server.server_close()
        self.thread.join()


@unittest.skipUnless(os.environ.get("USNM_REPAIR_HARNESS"),
                     "opt in with the compiled local-only repair_rehearsal example")
class RepairRehearsal(unittest.TestCase):
    def setUp(self):
        self.binary = str(Path(os.environ["USNM_REPAIR_HARNESS"]).resolve(strict=True))
        work = Path(os.environ["USNM_REPAIR_WORK"]).resolve(strict=True)
        # Never let tempfile fall back to a system temporary directory.
        self.folder = tempfile.TemporaryDirectory(prefix="repair-", dir=work)
        self.addCleanup(self.folder.cleanup)
        self.root = Path(self.folder.name)
        self.initialize(self.root)
        self.quarantines = self.root / "quarantines"
        self.quarantines.mkdir()
        self.state = self.quarantines / "qwen-test"
        self.quality = self.root / "quality"
        self.quality.mkdir()
        # A genuinely outstanding ordinary manifest makes build-quality return
        # after repair reconciliation, before its unrelated PostgreSQL selector.
        self.ordinary = self.put_initial_manifest(20, "queue-quality-qwen-rehearsal")
        (self.quality / "last-quality.json").write_text(json.dumps({
            "name": self.ordinary.parent.name, "at": dt.datetime.now(dt.timezone.utc).isoformat(),
            "pairs": [["xx_test", self.page(20)["doc_id"]]]}))
        self.start_scan_server()
        self.source = Source()
        self.addCleanup(self.source.close)

    def start_scan_server(self):
        self.serve = self.spawn("serve")
        selector = selectors.DefaultSelector()
        selector.register(self.serve.stdout, selectors.EVENT_READ)
        self.assertTrue(selector.select(30), "scan server did not start")
        selector.close()
        self.url = self.serve.stdout.readline().strip()
        self.assertTrue(self.url.startswith("http://127.0.0.1:"), self.url)
        with urllib.request.urlopen(self.url + "/index/full.jsonl", timeout=5) as response:
            self.assertEqual(response.status, 200)

    @staticmethod
    def initialize(root):
        root.mkdir(exist_ok=True)
        (root / ".repair-rehearsal").touch()
        (root / "scans").mkdir()
        (root / "manifests").mkdir()
        (root / "scans/index-full.jsonl").touch()

    def stop(self, process):
        if process.poll() is None:
            process.terminate()
            try:
                process.wait(timeout=10)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait(timeout=10)
        for stream in (process.stdout, process.stderr):
            if stream:
                stream.close()

    def command(self, action, *, root=None, manifest=None, source=None):
        args = [self.binary, "--root", str(root or self.root), action]
        for flag, value in (("--manifest", manifest), ("--source", source)):
            if value is not None:
                args.extend((flag, str(value)))
        return args

    def spawn(self, action, *, root=None, manifest=None, source=None):
        process = subprocess.Popen(self.command(action, root=root, manifest=manifest, source=source),
                                   stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        self.addCleanup(self.stop, process)
        return process

    def driver(self, action, *, root=None, manifest=None, source=None, ok=True):
        result = subprocess.run(self.command(action, root=root, manifest=manifest, source=source),
                                capture_output=True, text=True, timeout=120)
        if ok:
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        else:
            self.assertNotEqual(result.returncode, 0, result.stdout)
        return result

    def produce(self, flag="on", url=None, ok=True):
        result = subprocess.run([
            sys.executable, str(Path(manifest.__file__)), "build-quality",
            "--state", str(self.quality), "--dsn", "unused-rehearsal-must-not-reach-postgres",
            "--non-english", "--quarantine-root", str(self.quarantines),
            "--scan-server", url or self.url, "--queue-decode-repairs", flag,
        ], capture_output=True, text=True, timeout=120)
        self.assertEqual(result.returncode == 0, ok, result.stdout + result.stderr)
        return result

    @staticmethod
    def page(seq, *, body=None, batch="xx_test"):
        body = Source.body(seq) if body is None else body
        return {"doc_id": f"sn1_1900-01-02_ed-1_seq-{seq}", "batch": batch,
                "batch_version": 1, "bytes": len(body), "sha256": sha(body),
                "scan": f"buffer/{batch}/sn1/1900-01-02/1/{seq}.jp2",
                "lang": "rus", "damage_rate": 0.8}

    def seed(self, seq, *, manual=False, campaign=None, body=None, batch="xx_test"):
        page = self.page(seq, body=body, batch=batch)
        state = self.quarantines / campaign if campaign else self.state
        for _ in range(3):
            quarantine.record_failure(state, page, DecodeError("synthetic decoder failure"),
                                      {"decoder": "rehearsal"}, automatic_repair=not manual)
        original = self.root / "scans" / page["scan"]
        original.parent.mkdir(parents=True, exist_ok=True)
        original.write_bytes(Source.body(seq) if body is None else body)
        original.with_suffix(".jp2.done").write_bytes(b"preserve consumption marker")
        return page

    def records(self):
        return {r["source"]["doc_id"]: r for r in quarantine.quarantined(self.state)}

    def repair(self, seq):
        return self.root / "manifests" / self.records()[self.page(seq)["doc_id"]]["manifest_name"] / "manifest.parquet"

    def put_initial_manifest(self, seq, name):
        page = self.page(seq)
        body = manifest.manifest_bytes([manifest.row_of({**page, "language": "rus"}, {})])
        path = self.root / "manifests" / name / "manifest.parquet"
        path.parent.mkdir()
        path.write_bytes(body)
        return path

    def fetch(self, path, *, root=None):
        return json.loads(self.driver("fetch", manifest=path, source=self.source.url, root=root).stdout)

    def assert_occupied_noncompletion(self, status):
        persisted = json.loads((self.root / "scans/status-full.json").read_bytes())
        fields = ("state", "failed_transient", "deferred_repair_occupied", "deferred_needs_loc")
        self.assertEqual([persisted[key] for key in fields], [status[key] for key in fields])
        self.assertEqual([persisted[key] for key in fields], ["done", 1, 1, 0])
        self.assertEqual((persisted["fetched"], persisted["bytes_fetched"]), (0, 0))
        # The historical supervisor's documented fetched-marker predicate.
        # Its actual script/one-day cooling upgrade is rehearsed separately.
        legacy_fetched = (persisted["state"] == "done" and persisted["failed_transient"] == 0
                          and persisted["deferred_needs_loc"] == 0)
        self.assertFalse(legacy_fetched, "an occupied repair must not be retired as fetched")

    def dashboard(self):
        value = json.loads(self.driver("dashboard").stdout)
        self.assertIsNone(value["queue_error"], value)
        self.assertIsNone(value["index_error"], value)
        return value

    def retained(self, seq, ext="jp2", batch="xx_test"):
        return self.root / f"scans/keep/{batch}/sn1/1900-01-02/1/{seq}.{ext}"

    @staticmethod
    def snapshot(root):
        return {str(p.relative_to(root)): p.read_bytes() for p in root.rglob("*") if p.is_file()}

    def test_upgrade_fetch_reconcile_and_disabled_rollback(self):
        self.seed(1)
        self.seed(2, body=b"old broken fingerprint")
        self.seed(3, manual=True, campaign="cpu-manual")
        self.seed(4, manual=True, campaign="qwen-manual")
        before = self.snapshot(self.quarantines)
        originals = self.snapshot(self.root / "scans/buffer")
        self.produce("off")
        self.assertEqual(self.snapshot(self.quarantines), before)
        self.assertEqual(len(self.dashboard()["queue"]), 1)
        self.produce()
        repairs = [self.repair(seq) for seq in (1, 2)]
        for path in repairs:
            rejected = self.driver("ordinary-decode", manifest=path, ok=False)
            self.assertIn("not a prefetch manifest", rejected.stderr)
        self.assertEqual(self.driver("ordinary-decode", manifest=self.ordinary).stdout.strip(), "1")
        self.assertEqual(len(self.dashboard()["queue"]), 3)
        self.assertEqual({q["kind"] for q in self.dashboard()["queue"]}, {"quality"})
        first = self.fetch(repairs[0])
        self.assertEqual((first["fetched"], first["fetched_jp2"]), (1, 1))
        self.assertEqual(first["failed_transient"], 0)
        self.source.retry_once = Source.image_path(2)
        second = self.fetch(repairs[1])
        self.assertEqual(second["fetched"], 1)
        self.assertEqual(second["failed_transient"], 0)
        self.produce()
        self.assertEqual([self.records()[self.page(seq)["doc_id"]]["refetch_state"] for seq in (1, 2)],
                         ["observed_same", "observed_new"])
        for campaign in ("cpu-manual", "qwen-manual"):
            self.assertEqual(quarantine.quarantined(self.quarantines / campaign)[0]["refetch_state"],
                             "manual_review")
        self.assertEqual(self.snapshot(self.root / "scans/buffer"), originals)
        dash = self.dashboard()
        self.assertEqual([q["fetched"] for q in dash["queue"] if "zdecode" in q["name"]], [1, 1])
        self.assertEqual(dash["status"]["fetched"], 1)
        self.assertIsNotNone(dash["pacer"]["slowed"])
        times = [entry[0] for entry in self.source.hits]
        self.assertTrue(all(b - a >= 2.85 for a, b in zip(times, times[1:])), self.source.hits)
        retries = [t for t, path, _ in self.source.hits if path == Source.image_path(2)]
        self.assertEqual(len(retries), 2)
        self.assertGreaterEqual(retries[1] - retries[0], 4.85)
        self.assertTrue(all(agent.startswith("usnewsmap-ocr-worker/") for _, _, agent in self.source.hits))
        saved = (self.snapshot(self.root / "manifests"), self.snapshot(self.quarantines),
                 self.snapshot(self.root / "scans/keep"))
        self.produce("off")
        self.stop(self.serve)
        self.start_scan_server()
        self.produce("off")
        self.assertEqual((self.snapshot(self.root / "manifests"), self.snapshot(self.quarantines),
                          self.snapshot(self.root / "scans/keep")), saved)
        self.assertEqual(len(self.dashboard()["queue"]), 3)
        print("PASS lifecycle: real CLI/HTTP/JP2/dashboard; same/new; 3s pacer and 429 slowdown; "
              "manual quarantines and all queued bytes survive disabled-producer restart")

    def test_lost_ack_capacity_and_corrupt_retained_bytes(self):
        for seq in range(1, 12):
            self.seed(seq)
        destination = urllib.parse.urlsplit(self.url)
        puts = []

        class LostAck(QuietHandler):
            def forward(self):
                body = self.rfile.read(int(self.headers.get("Content-Length", "0")))
                connection = http.client.HTTPConnection(destination.hostname, destination.port, timeout=30)
                try:
                    connection.request(self.command, self.path, body,
                                       {"Content-Type": "application/octet-stream"})
                    response = connection.getresponse()
                    data = response.read()
                    if self.command == "PUT":
                        puts.append((self.path, body, response.status))
                        # The actual serve-scans PUT completed, but the client gets EOF.
                        self.close_connection = True
                        return
                    self.reply(data, response.status)
                finally:
                    connection.close()

            do_GET = forward
            do_PUT = forward

        proxy = http.server.ThreadingHTTPServer(("127.0.0.1", 0), LostAck)
        thread = threading.Thread(target=proxy.serve_forever)
        thread.start()
        try:
            self.produce(url=f"http://127.0.0.1:{proxy.server_port}", ok=False)
        finally:
            proxy.shutdown()
            proxy.server_close()
            thread.join()
        self.assertEqual(len(puts), 1)
        record = next(r for r in self.records().values() if r["refetch_state"] == "prepared")
        published = self.root / "manifests" / record["manifest_name"] / "manifest.parquet"
        self.assertEqual(published.read_bytes(), record["manifest_body"])
        self.assertEqual(puts[0][1], record["manifest_body"])
        self.assertIn(puts[0][2], (200, 201))
        self.produce()
        self.assertEqual(published.read_bytes(), record["manifest_body"])
        self.assertEqual(sum(r["refetch_state"] == "queued" for r in self.records().values()), 10)
        self.assertEqual(sum(r["refetch_state"] == "pending" for r in self.records().values()), 1)
        for seq in range(1, 11):
            path = self.retained(seq)
            path.parent.mkdir(parents=True, exist_ok=True)
            body = Source.body(seq)
            path.write_bytes(body[:-1] if seq % 2 else b"x" * len(body))
            side = {**self.page(seq), "loc_url": self.source.url + Source.image_path(seq),
                    "iiif_base": self.source.url + "/iiif/test", "resolution": "full",
                    "fetched_at": "2026-11-03T00:00:00Z", "download_ms": 1, "user_agent": "rehearsal",
                    "format": "jp2", "md5": md5(body)}
            for key in ("scan", "lang", "damage_rate"):
                del side[key]
            Path(str(path) + ".json").write_text(json.dumps(side))
        before = self.snapshot(self.root / "scans/keep")
        self.produce()
        self.assertEqual(sum(r["refetch_state"] == "queued" for r in self.records().values()), 10)
        self.assertEqual(self.records()[self.page(11)["doc_id"]]["refetch_state"], "pending")
        result = self.fetch(self.repair(1))
        self.assertEqual((result["fetched"], result["deferred_repair_occupied"]), (0, 1))
        self.assert_occupied_noncompletion(result)
        self.assertEqual(self.source.hits, [])
        self.assertEqual(self.snapshot(self.root / "scans/keep"), before)
        print("PASS lost ACK: durable prepared bytes replay identically; cap=10; valid sidecars "
              "with truncated/wrong bytes retain queued slots; occupied status blocks legacy "
              "fetched markers without HTTP fetch")

    def test_concurrent_acquisition_and_killed_fetcher_restart(self):
        self.seed(1)
        self.seed(2)
        self.produce()
        self.source.wait_path = Source.image_path(1)
        running = self.spawn("fetch", manifest=self.repair(1), source=self.source.url)
        self.assertTrue(self.source.started.wait(60), "fetch did not reach synthetic image")
        self.assertIsNone(running.poll())
        blocked = self.driver("fetch", manifest=self.repair(2), source=self.source.url, ok=False)
        self.assertIn("one fetching process at a time", blocked.stderr)
        path = self.retained(1)
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(b"concurrent independent retained scan")
        self.source.release.set()
        stdout, stderr = running.communicate(timeout=60)
        self.assertEqual(running.returncode, 0, stderr)
        self.assert_occupied_noncompletion(json.loads(stdout))
        self.assertEqual(path.read_bytes(), b"concurrent independent retained scan")
        self.assertFalse(Path(str(path) + ".json").exists())
        self.assertEqual((self.root / "scans/index-full.jsonl").read_bytes(), b"")
        self.source.started.clear()
        self.source.release.clear()
        self.source.wait_path = Source.image_path(2)
        killed = self.spawn("fetch", manifest=self.repair(2), source=self.source.url)
        self.assertTrue(self.source.started.wait(60))
        abandoned = Path(str(self.retained(2)) + ".repair-abandoned.part")
        abandoned.write_bytes(b"preserve prior crash evidence")
        killed.kill()
        killed.wait(timeout=10)
        self.assertNotEqual(killed.returncode, 0)
        self.assertFalse(self.retained(2).exists())
        self.source.release.set()
        self.source.wait_path = None
        restarted = self.fetch(self.repair(2))
        self.assertEqual(restarted["fetched"], 1)
        self.assertEqual(abandoned.read_bytes(), b"preserve prior crash evidence")
        self.assertEqual(self.retained(2).read_bytes(), Source.body(2))
        self.produce()
        self.assertEqual(self.records()[self.page(1)["doc_id"]]["refetch_state"], "queued")
        self.assertEqual(self.records()[self.page(2)["doc_id"]]["refetch_state"], "observed_same")
        # Restore an independent crash-boundary fixture from actual published bytes:
        # both files durable, index append absent. Never remove the original index.
        replay = self.root / "restart-after-publication"
        self.initialize(replay)
        target = replay / self.retained(2).relative_to(self.root)
        target.parent.mkdir(parents=True)
        shutil.copyfile(self.retained(2), target)
        shutil.copyfile(str(self.retained(2)) + ".json", str(target) + ".json")
        copied_manifest = replay / self.repair(2).relative_to(self.root)
        copied_manifest.parent.mkdir(parents=True)
        shutil.copyfile(self.repair(2), copied_manifest)
        hits = len(self.source.hits)
        original_index = (self.root / "scans/index-full.jsonl").read_bytes()
        for _ in range(2):
            result = self.fetch(copied_manifest, root=replay)
            self.assertEqual((result["fetched"], result["already_there"]), (0, 1))
            lines = (replay / "scans/index-full.jsonl").read_text().splitlines()
            self.assertEqual(len(lines), 1)
            self.assertEqual(json.loads(lines[0]), json.loads(Path(str(target) + ".json").read_bytes()))
        self.assertEqual(len(self.source.hits), hits)
        self.assertEqual((self.root / "scans/index-full.jsonl").read_bytes(), original_index)
        self.assertEqual(path.read_bytes(), b"concurrent independent retained scan")
        print("PASS concurrency/crash: second fetch process excluded; late retained file wins; "
              "occupied status cannot be marked fetched by the legacy predicate; "
              "SIGKILL/restart releases lock and preserves partial files; copied post-publication "
              "crash boundary restores exactly one index entry without refetch")

    def test_jpeg_fallback_and_ordinary_http_manifest_coexist(self):
        page = self.seed(1, batch="yy_jpeg")
        self.produce()
        record = self.records()[page["doc_id"]]
        path = self.root / "manifests" / record["manifest_name"] / "manifest.parquet"
        ordinary_body = self.ordinary.read_bytes()
        request = urllib.request.Request(
            self.url + "/manifests/queue-ocr-rehearsal/manifest.parquet",
            data=ordinary_body, method="PUT")
        with urllib.request.urlopen(request, timeout=30) as response:
            self.assertIn(response.status, (200, 201))
        queued = self.dashboard()["queue"]
        self.assertEqual(queued[0]["kind"], "ocr")
        self.assertEqual(len(queued), 3)
        result = self.fetch(path)
        self.assertEqual((result["fetched"], result["fetched_item_api"]), (1, 1))
        self.assertEqual(self.retained(1, "jpg", "yy_jpeg").read_bytes(), Source.body(1))
        self.produce()
        self.assertEqual(self.records()[page["doc_id"]]["refetch_state"], "observed_same")
        self.assertEqual(self.ordinary.read_bytes(), ordinary_body)
        print("PASS JPEG fallback: ordinary HTTP PUT and repair coexist; dashboard OCR-first "
              "ordering; actual item/IIIF fallback fetch reconciles same bytes")


if __name__ == "__main__":
    unittest.main(verbosity=2)
