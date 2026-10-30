"""Offline catalog contract, replay, provenance, CLI and HTTP tests (stdlib only)."""

import copy
import json
from pathlib import Path
import re
import shutil
import sqlite3
import subprocess
import sys
import threading
import unittest
from unittest import mock
import urllib.error
import urllib.parse
import urllib.request
import uuid

from scripts.corpus import data_catalog as c


ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "scripts/corpus/data_catalog.py"
T0 = "2026-01-01T00:00:00Z"
T1 = "2026-01-02T00:00:00Z"
PAGE = "loc:sn123_1900-01-01_ed-1_seq-1"
HASH = "ab" * 32


def run_record():
    # Every field is the real RunRecord v1; schema drift is checked against Rust below.
    record = {key: "metadata" for key in c.RUN_STR}
    record.update({key: None for key in c.RUN_OPT_STR | c.RUN_OPT_UINT | c.RUN_OPT_FLOAT})
    record.update({key: 0 for key in c.RUN_UINT})
    record.update({key: False for key in c.RUN_BOOL})
    record.update(schema=1, run_id="node.20260101T000000Z", doc_id=PAGE.split(":", 1)[1],
                  contract=1, engine_id="tesseract-5.5", engine_version="5.5.3",
                  family="tesseract", profile="eng", route_id="baseline", language="eng",
                  image_digest="sha256:" + HASH, models_digest="sha256:" + HASH,
                  image_sha256=HASH, text_sha256=HASH, models_json="{}",
                  options_json='{"psm":3}', engine_stats_json="{}", status="ok",
                  selected=True, scale=1.0, seconds_wall=2.5, node_finished_at=T0,
                  queued_at=None, image_ref="local")
    return record


def event(event_id="e1", kind="artifact", when=T0, **kwargs):
    payload = {"artifact_id": "scan", "sha256": HASH, "version": "v1",
               "location": "archive", "reference": "scans/issue/page.jp2", "state": "verified"}
    if kind == "run":
        payload = {"run_record": run_record()}
    elif kind == "page_state":
        payload = {"stage": "accepted", "state": "present", "run_id": None}
    elif kind == "stage":
        payload = {"stage_id": "boinc-ready", "label": "BOINC ready", "count": 19850,
                   "state": "observed", "detail": "Public tasks; underlying cache age unknown.",
                   "coverage": "BOINC task totals"}
    value = {"schema_version": 1, "batch_id": "b1", "event_id": event_id, "kind": kind,
             "source": "inventory", "observed_at": when,
             "page_id": None if kind == "stage" else PAGE, "payload": payload}
    value.update(kwargs)
    return value


class CatalogTests(unittest.TestCase):
    def setUp(self):
        # Explicit project-local scratch, never the OS temporary directory.
        self.work = ROOT / (".catalog-test-" + uuid.uuid4().hex)
        self.work.mkdir(mode=0o700)
        self.db = self.work / "catalog.sqlite"
        self.batch_counter = 0

    def tearDown(self):
        shutil.rmtree(self.work)

    def batch(self, events, batch_id=None):
        self.batch_counter += 1
        batch_id = batch_id or "batch-" + str(self.batch_counter)
        values = copy.deepcopy(events)
        for value in values:
            value["batch_id"] = batch_id
        path = self.work / (batch_id + ".jsonl")
        path.write_text("".join(c.canonical(value) + "\n" for value in values))
        return path

    def ingest(self, *events):
        return c.ingest(self.db, self.batch(events))

    def cli(self, *args, code=0):
        result = subprocess.run([sys.executable, str(SCRIPT), "--db", str(self.db), *args],
                                cwd=ROOT, capture_output=True, text=True, timeout=10)
        self.assertEqual(result.returncode, code, result.stderr)
        return json.loads(result.stdout if code == 0 else result.stderr)

    def test_actual_rust_schema_not_replacement(self):
        rust = (ROOT / "crates/usnm-ocr-contract/src/runs.rs").read_text()
        struct = rust.split("pub struct RunRecord {", 1)[1].split("\n}", 1)[0]
        fields = set(re.findall(r"pub (\w+):", struct))
        self.assertEqual(set(run_record()) - {"schema"}, fields)
        projection = c.adapt_run(run_record(), PAGE)
        self.assertEqual(projection["status"], "ok")
        self.assertEqual(projection["config_sha256"], c.adapt_run(run_record(), PAGE)["config_sha256"])
        self.assertNotIn("text_sha256", projection)

    def test_existing_local_engine_digest_sentinels(self):
        record = run_record()
        record.update(image_digest="local", models_digest="local", models_json="{}")
        self.ingest(event("local-run", "run", payload={"run_record": record}))
        data = c.query(self.db, "run", {})["items"][0]["data"]
        self.assertEqual(data["image_digest"], "local")
        self.assertEqual(data["models_digest"], "local")
        self.assertEqual(data["image_sha256"], HASH)
        for key in ("image_digest", "models_digest", "routing_digest"):
            malformed = copy.deepcopy(record)
            malformed[key] = "unknown"
            with self.subTest(key=key), self.assertRaises(c.CatalogError):
                c.adapt_run(malformed, PAGE)
        malformed = copy.deepcopy(record)
        malformed["routing_digest"] = "local"
        with self.assertRaises(c.CatalogError):
            c.adapt_run(malformed, PAGE)

    def test_real_cached_scan_index_adapter_presence_not_verification(self):
        doc = "2017270200_1885-08-20_ed-1_seq-4"
        fetched = "2026-10-11T22:32:31.635966037Z"
        scan_root = self.work / "scans"
        scan = scan_root / "keep/nbu_irisvirginica/2017270200/1885-08-20/1/4.jpg"
        scan.parent.mkdir(parents=True)
        scan.write_bytes(b"test-image")
        index = self.work / "index-full.jsonl"
        row = {"doc_id": doc, "batch": "nbu_irisvirginica", "batch_version": 1,
               "loc_url": "https://tile.loc.gov/example.jpg", "resolution": "full",
               "fetched_at": fetched, "bytes": scan.stat().st_size,
               "sha256": HASH, "download_ms": 2050}
        index.write_text(c.canonical(row) + "\n")
        evidence = self.work / "cached-evidence.jsonl"
        self.cli("ingest-cached-scans", "--index", str(index), "--scan-root", str(scan_root), "--cache-set", "keep",
                 "--evidence-out", str(evidence), "--batch-id", "cached-1",
                 "--source", "pve-big-stat")
        item = c.query(self.db, "artifact", {})["items"][0]
        self.assertEqual(item["page_id"], "loc:" + doc)
        self.assertEqual(item["data"]["state"], "present")
        self.assertEqual(item["data"]["condition"], "size_match")
        self.assertEqual(item["data"]["fetched_at"], fetched)
        self.assertEqual(c.snapshot(self.db)["artifacts"]["verified"], 0)
        self.assertNotIn("https://", evidence.read_text())
        self.assertNotIn("loc_url", evidence.read_text())
        self.assertEqual(c.timestamp(fetched), fetched)
        scan.unlink()
        c.ingest_cached_scans(self.db, index, scan_root,
                              self.work / "cached-missing.jsonl", "cached-2", "pve-big-stat", "keep")
        self.assertEqual(c.query(self.db, "artifact", {})["items"][0]["data"]["state"], "missing")
        self.assertEqual(c.query(self.db, "artifact", {})["items"][0]["data"]["condition"], "absent")
        # Wrong-size files are not considered present either.
        scan.write_bytes(b"x")
        c.ingest_cached_scans(self.db, index, scan_root,
                              self.work / "cached-size.jsonl", "cached-3", "pve-big-stat", "keep")
        self.assertEqual(c.query(self.db, "artifact", {})["items"][0]["data"]["state"], "missing")
        self.assertEqual(c.query(self.db, "artifact", {})["items"][0]["data"]["condition"], "size_mismatch")
        self.assertIn("artifact_size_mismatch", {i["code"] for i in c.snapshot(self.db)["issues"]})
        with self.assertRaises(c.CatalogError):
            c.ingest_cached_scans(self.db, index, scan_root, evidence, "cached-4", "pve-big-stat", "keep")
        self.assertEqual(c.ingest(self.db, evidence)["inserted"], 0)
        # Untrusted cache coordinates cannot traverse outside the provided root.
        row["batch"] = "../escape"
        index.write_text(c.canonical(row) + "\n")
        with self.assertRaises(c.CatalogError):
            c.ingest_cached_scans(self.db, index, scan_root,
                                  self.work / "bad.jsonl", "bad", "pve-big-stat", "keep")
        self.assertFalse((self.work / "bad.jsonl").exists())

    def test_actual_worker_capture_success_failures_roundtrip(self):
        # Unmodified JSON emitted by Worker::process tests at worker commit 0e5b097.
        # Controlled FakeEngine/ColdInstanceEngine + fake scan transport, real ALTO parse,
        # measured capture times/hashes/publication. These are NOT live OCR measurements.
        fixtures = ROOT / "scripts/corpus/fixtures/data_catalog"
        for name in ("success", "upload-failure", "timeout", "fetch-failure"):
            source = fixtures / ("worker-" + name + ".json")
            result = self.cli("ingest-worker", "--input", str(source),
                              "--evidence-out", str(self.work / (name + ".jsonl")),
                              "--batch-id", "worker-" + name, "--source", "worker-test-capture")
            self.assertEqual(result["inserted"], 2 if name in ("success", "upload-failure") else 1)
        executions = c.query(self.db, "execution", {})["items"]
        self.assertEqual(len(executions), 4)
        self.assertEqual(len(c.query(self.db, "run", {})["items"]), 2)
        self.assertEqual(c.snapshot(self.db)["runs"], {"total": 4, "pages": 2})
        upload = next(i["data"] for i in executions if i["data"]["error_kind"] == "output")
        self.assertEqual(upload["status"], "failed")
        self.assertEqual(upload["observations"][0]["status"], "ok")
        self.assertTrue(upload["has_run_record"])
        engine = c.query(self.db, "run", {"run_id": upload["run_id"]})["items"][0]
        self.assertEqual(engine["data"]["status"], "ok")
        timeout = next(i["data"] for i in executions
                       if i["data"]["observations"] and i["data"]["observations"][0]["status"] == "timeout")
        self.assertEqual(timeout["observations"][0]["info_source"], "instance")
        self.assertFalse(timeout["has_run_record"])
        fetch = next(i["data"] for i in executions if i["data"]["error_kind"] == "fetch")
        self.assertEqual(fetch["observations"], [])
        self.assertFalse(fetch["has_run_record"])
        snapshot = c.snapshot(self.db)
        for stage in snapshot["stages"]:
            if stage["id"] in ("accepted", "imported", "indexed"):
                self.assertEqual(stage["state"], "unknown")
                self.assertIsNone(stage["count"])
        response = json.dumps(c.query(self.db, "execution", {}))
        self.assertNotIn("ocr_engine", response)
        self.assertNotIn("image_ref", response)
        evidence = self.work / "success.jsonl"
        self.assertTrue(c.ingest(self.db, evidence)["replayed"])
        self.assertEqual(c.ingest_worker(
            self.db, fixtures / "worker-success.json", self.work / "overlap.jsonl",
            "worker-overlap", "worker-test-capture")["inserted"], 0)
        with c.make_server(self.db) as server:
            thread = threading.Thread(target=server.serve_forever)
            thread.start()
            try:
                url = ("http://127.0.0.1:" + str(server.server_address[1]) +
                       "/executions?status=failed&engine=tesseract-5.5&limit=1")
                with urllib.request.urlopen(url, timeout=5) as response:
                    actual = json.load(response)["items"][0]["data"]
                self.assertEqual(actual["run_id"], timeout["run_id"])
            finally:
                server.shutdown()
                thread.join(timeout=5)

    def test_producer_sidecars_format_and_explicit_cache_set(self):
        row = {"doc_id": "2017270200_1885-08-20_ed-1_seq-4", "batch": "nbu_irisvirginica",
               "batch_version": 1, "loc_url": "https://tile.loc.gov/scan",
               "iiif_base": "https://tile.loc.gov/image-service", "resolution": "full",
               "fetched_at": "2026-10-11T22:32:31.635966037Z", "bytes": 4,
               "sha256": HASH, "download_ms": 2050, "user_agent": "test-sidecar-agent",
               "format": "jp2", "md5": None}
        root = self.work / "scans"
        path = root / "buffer/nbu_irisvirginica/2017270200/1885-08-20/1/4.jp2"
        path.parent.mkdir(parents=True)
        path.write_bytes(b"jp2!")
        index = self.work / "sidecar-index.jsonl"
        index.write_text(c.canonical(row) + "\n")
        evidence = self.work / "buffer-evidence.jsonl"
        self.cli("ingest-cached-scans", "--index", str(index), "--scan-root", str(root),
                 "--cache-set", "buffer", "--evidence-out", str(evidence),
                 "--batch-id", "buffer-jp2", "--source", "cache-stat")
        item = c.query(self.db, "artifact", {})["items"][0]["data"]
        self.assertEqual(item["state"], "present")
        self.assertEqual(item["reference"], str(path.relative_to(root)))
        self.assertEqual(item["location"], "cached-scans-buffer")
        self.assertNotIn("tile.loc.gov", evidence.read_text())
        self.assertNotIn("test-sidecar-agent", evidence.read_text())
        row["format"] = "jpeg"
        row["md5"] = "ff" * 16
        path = root / "keep/nbu_irisvirginica/2017270200/1885-08-20/1/4.jpg"
        path.parent.mkdir(parents=True)
        path.write_bytes(b"jpg!")
        index.write_text(c.canonical(row) + "\n")
        c.ingest_cached_scans(self.db, index, root, self.work / "keep-evidence.jsonl",
                              "keep-jpeg", "cache-stat", "keep")
        self.assertEqual(len(c.query(self.db, "artifact", {})["items"]), 2)
        # Missing set coordinates fail at argparse rather than inventing absent evidence.
        process = subprocess.run([sys.executable, str(SCRIPT), "--db", str(self.db),
                                  "ingest-cached-scans", "--index", str(index), "--scan-root", str(root),
                                  "--evidence-out", str(self.work / "no-set.jsonl"),
                                  "--batch-id", "no-set", "--source", "cache-stat"],
                                 capture_output=True, text=True, timeout=5)
        self.assertEqual(process.returncode, 2)
        self.assertIn("--cache-set", process.stderr)
        self.assertFalse((self.work / "no-set.jsonl").exists())
        for invalid in (None, 123):
            row["batch"] = invalid
            index.write_text(c.canonical(row) + "\n")
            error = self.cli("ingest-cached-scans", "--index", str(index), "--scan-root", str(root),
                             "--cache-set", "keep", "--evidence-out", str(self.work / "bad-batch.jsonl"),
                             "--batch-id", "bad-batch", "--source", "cache-stat", code=2)
            self.assertIn("error", error)
            self.assertFalse((self.work / "bad-batch.jsonl").exists())

    def test_snapshot_counts_source_run_page_and_long_stage_identities(self):
        first = event("first", "run")
        second = event("second", "run", page_id="other:" + PAGE.split(":", 1)[1])
        self.ingest(first, second)
        self.assertEqual(c.snapshot(self.db)["runs"], {"total": 2, "pages": 2})
        events = []
        for stage_id in ("a" * 255 + "x", "a" * 255 + "y"):
            for source in ("source1", "source2"):
                value = event(source + stage_id[-1], "stage", source=source)
                value["payload"]["stage_id"] = stage_id
                events.append(value)
        self.ingest(*events)
        stages = c.snapshot(self.db)["stages"]
        self.assertEqual(len(stages), len({stage["id"] for stage in stages}))
        self.assertTrue(all(len(stage["id"]) <= 256 for stage in stages))

    def test_worker_adapter_rejects_malformed_unknown_and_oversized(self):
        fixture = json.loads((ROOT / "scripts/corpus/fixtures/data_catalog/worker-timeout.json").read_text())
        page_id = "loc:" + fixture["doc_id"]
        for key, value in (("metadata_schema", 2), ("status", "running"),
                           ("finished_at", None), ("observations", [fixture["observations"][0]] * 3),
                           ("seconds_wall", -1), ("dpi", True)):
            malformed = copy.deepcopy(fixture)
            malformed[key] = value
            with self.subTest(key=key), self.assertRaises(c.CatalogError):
                c.adapt_worker(malformed, page_id)
        fixture["ocr_text"] = "not metadata"
        with self.assertRaises(c.CatalogError):
            c.adapt_worker(fixture, page_id)
        oversized = self.work / "oversized-worker.json"
        oversized.write_text("x" * (c.MAX_LINE + 1))
        with self.assertRaises(c.CatalogError):
            c.ingest_worker(self.db, oversized, self.work / "out.jsonl", "b", "worker")
        self.assertFalse((self.work / "out.jsonl").exists())

    def test_worker_engine_index_updates_and_measurements_do_not_change_config(self):
        metadata = json.loads((ROOT / "scripts/corpus/fixtures/data_catalog/worker-timeout.json").read_text())
        page_id = "loc:" + metadata["doc_id"]
        original = c.adapt_worker(metadata, page_id)["observations"][0]["config_sha256"]
        measurements = copy.deepcopy(metadata)
        measurements["observations"][0]["info"].update(
            cold_start=False, model_load_seconds=9, engine_stats_json='{"seconds":12}')
        self.assertEqual(c.adapt_worker(measurements, page_id)["observations"][0]["config_sha256"],
                         original)
        measurements["observations"][0]["info"]["options_json"] = '{"psm":6}'
        self.assertNotEqual(c.adapt_worker(measurements, page_id)["observations"][0]["config_sha256"],
                            original)
        missing_info = copy.deepcopy(metadata)
        missing_info["observations"][0]["info"] = None
        older = event("execution-old", "execution", T0, page_id=page_id,
                      payload={"worker_metadata": missing_info})
        newer = event("execution-new", "execution", T1, page_id=page_id,
                      payload={"worker_metadata": metadata})
        self.ingest(older)
        self.assertEqual(c.query(self.db, "execution", {"engine": "tesseract-5.5"})["items"], [])
        self.ingest(newer)
        self.assertEqual(len(c.query(self.db, "execution", {"engine": "tesseract-5.5"})["items"]), 1)
        changed_engine = copy.deepcopy(metadata)
        changed_engine["observations"][0]["info"]["engine_id"] = "tesseract-corrected"
        self.ingest(event("execution-corrected", "execution", "2026-01-03T00:00:00Z",
                          page_id=page_id, payload={"worker_metadata": changed_engine}))
        self.assertEqual(c.query(self.db, "execution", {"engine": "tesseract-5.5"})["items"], [])
        self.assertEqual(len(c.query(self.db, "execution", {"engine": "tesseract-corrected"})["items"]), 1)

    def test_read_queries_do_not_create_or_modify_database_sidecars(self):
        self.ingest(event("run", "run"))
        before_files = {file.name: file.read_bytes() for file in self.work.iterdir() if file.is_file()}
        c.snapshot(self.db)
        c.page(self.db, PAGE)
        c.query(self.db, "run", {})
        with c.make_server(self.db) as server:
            thread = threading.Thread(target=server.serve_forever)
            thread.start()
            try:
                with urllib.request.urlopen("http://127.0.0.1:" + str(server.server_address[1]) +
                                            "/health", timeout=5) as response:
                    self.assertEqual(json.load(response)["catalog"], "readable")
            finally:
                server.shutdown()
                thread.join(timeout=5)
        self.assertEqual(before_files, {file.name: file.read_bytes() for file in self.work.iterdir()
                                        if file.is_file()})
        self.assertFalse(Path(str(self.db) + "-wal").exists())
        self.assertFalse(Path(str(self.db) + "-shm").exists())

    def test_both_evidence_exporters_reject_catalog_sidecar_outputs(self):
        self.ingest(event())
        before = self.db.read_bytes()
        worker = ROOT / "scripts/corpus/fixtures/data_catalog/worker-success.json"
        root = self.work / "scans"
        root.mkdir()
        index = self.work / "index.jsonl"
        index.write_text(c.canonical({
            "doc_id": "2017270200_1885-08-20_ed-1_seq-4", "batch": "nbu_irisvirginica",
            "batch_version": 1, "loc_url": "https://tile.loc.gov/scan", "resolution": "full",
            "fetched_at": T0, "bytes": 4, "sha256": HASH, "download_ms": 1}) + "\n")
        for suffix in ("", "-journal", "-wal", "-shm"):
            output = Path(str(self.db) + suffix)
            for exporter in ("worker", "cached"):
                with self.subTest(suffix=suffix, exporter=exporter), self.assertRaises(c.CatalogError):
                    if exporter == "worker":
                        c.ingest_worker(self.db, worker, output, "worker-batch", "worker")
                    else:
                        c.ingest_cached_scans(self.db, index, root, output,
                                              "cached-batch", "cache-stat", "keep")
                self.assertEqual(self.db.read_bytes(), before)
                if suffix:
                    self.assertFalse(output.exists())

    def test_cli_end_to_end_and_snapshot_freshness(self):
        batch = self.batch([event(), event("r1", "run"), event("a1", "page_state"),
                            event("s1", "stage", T1)])
        self.assertEqual(self.cli("ingest", "--batch", str(batch))["inserted"], 4)
        result = self.cli("query", "page", "--page-id", PAGE)
        self.assertEqual(len(result["items"]), 3)
        self.assertTrue(all(i["source"] == "inventory" for i in result["items"]))
        self.assertTrue(all(i["batch_id"] == "batch-1" for i in result["items"]))
        self.assertEqual({i["line_number"] for i in result["items"]}, {1, 2, 3})
        self.assertTrue(all(len(i["event_sha256"]) == 64 for i in result["items"]))
        runs = self.cli("query", "run", "--engine", "tesseract-5.5", "--status", "ok")
        self.assertEqual(len(runs["items"]), 1)
        snapshot = self.cli("snapshot")
        self.assertEqual(snapshot["schema_version"], 1)
        self.assertEqual(snapshot["producer"], "corpus-catalog")
        self.assertEqual(snapshot["observed_at"], c.timestamp(T1))
        self.assertNotEqual(snapshot["generated_at"], snapshot["observed_at"])
        self.assertEqual(snapshot["runs"], {"total": 1, "pages": 1})
        self.assertEqual(snapshot["artifacts"], {"locations": 1, "verified": 1, "missing": 0})
        stages = {x["id"]: x for x in snapshot["stages"]}
        self.assertEqual(stages["accepted"]["count"], 1)
        self.assertEqual(stages["accepted"]["observed_at"], c.timestamp(T0))
        self.assertIsNone(stages["indexed"]["count"])
        self.assertEqual(stages["indexed"]["state"], "unknown")
        self.assertEqual(stages["boinc-ready"]["count"], 19850)
        self.assertEqual(stages["boinc-ready"]["observed_at"], c.timestamp(T1))
        self.assertIn("source=inventory", stages["boinc-ready"]["detail"])
        self.assertLessEqual(len(json.dumps(snapshot).encode()), c.MAX_RESULT)
        out = self.work / "snapshot.json"
        # Atomic file mode emits nothing on stdout.
        result = subprocess.run([sys.executable, str(SCRIPT), "--db", str(self.db),
                                 "snapshot", "--out", str(out)], capture_output=True, timeout=10)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout, b"")
        self.assertEqual(json.loads(out.read_text())["observed_at"], c.timestamp(T1))
        self.assertFalse(list(self.work.glob("*.pending")))

    def test_empty_catalog_unknown_not_success_shaped(self):
        self.cli("init")
        snapshot = self.cli("snapshot")
        self.assertIsNone(snapshot["observed_at"])
        self.assertTrue(snapshot["generated_at"])
        self.assertEqual(snapshot["coverage"],
                         [x + ":unknown" for x in ("runs", "artifacts") + c.STAGES])
        self.assertTrue(all(x["state"] == "unknown" and x["count"] is None for x in snapshot["stages"]))
        self.assertEqual(len(snapshot["issues"]), 8)
        self.assertFalse(any("ready" in x["id"] for x in snapshot["stages"]))
        self.cli("query", "run", "--limit", "101", code=2)

    def test_duplicate_and_byte_exact_batch_replay(self):
        batch = self.batch([event(), event()])
        self.assertEqual(c.ingest(self.db, batch)["inserted"], 1)
        self.assertTrue(c.ingest(self.db, batch)["replayed"])
        # Same observation may occur in overlapping export batches.
        self.assertEqual(self.ingest(event())["inserted"], 0)
        before = batch.read_bytes()
        batch.write_bytes(before + b"\n")
        with self.assertRaises(c.CatalogError):
            c.ingest(self.db, batch)
        self.assertEqual(len(c.query(self.db, "artifact", {})["items"]), 1)

    def test_event_replay_conflict_and_transaction_rollback(self):
        self.ingest(event())
        conflict = event()
        conflict["payload"]["state"] = "missing"
        new = event("new")
        new["payload"]["version"] = "v2"
        with self.assertRaisesRegex(c.CatalogError, "replay conflict"):
            self.ingest(new, conflict)
        self.assertEqual(len(c.query(self.db, "artifact", {})["items"]), 1)
        conn = sqlite3.connect(self.db)
        try:
            self.assertEqual(conn.execute("SELECT count(*) FROM events").fetchone()[0], 1)
            self.assertEqual(conn.execute("SELECT count(*) FROM batches").fetchone()[0], 1)
        finally:
            conn.close()

    def test_malformed_later_line_rolls_back_without_db_creation(self):
        path = self.batch([event()])
        with path.open("a") as stream:
            stream.write('{"malformed":\n')
        with self.assertRaises(c.CatalogError):
            c.ingest(self.db, path)
        self.assertFalse(self.db.exists())
        self.ingest(event())
        path = self.batch([event("valid-new")])
        with path.open("a") as stream:
            stream.write("{}\n")
        with self.assertRaises(c.CatalogError):
            c.ingest(self.db, path)
        conn = sqlite3.connect(self.db)
        try:
            self.assertEqual(conn.execute("SELECT count(*) FROM events").fetchone()[0], 1)
        finally:
            conn.close()

    def test_newest_wins_and_deterministic_event_tie(self):
        newer = event("z", when=T1)
        newer["payload"]["state"] = "missing"
        self.ingest(newer)
        self.ingest(event("a", when=T0))
        self.ingest(event("y", when=T1))
        item = c.query(self.db, "artifact", {})["items"][0]
        self.assertEqual((item["event_id"], item["data"]["state"]), ("z", "missing"))
        self.assertEqual(c.snapshot(self.db)["observed_at"], c.timestamp(T1))
        newest = event("zz", when="2026-01-02T01:00:00+01:00")
        self.ingest(newest)
        self.assertEqual(c.query(self.db, "artifact", {})["items"][0]["event_id"], "zz")

    def test_nanosecond_event_order_and_explicit_verification(self):
        earlier = event("z", when="2026-01-01T00:00:00.000000001Z")
        earlier["payload"]["state"] = "missing"
        later = event("a", when="2026-01-01T00:00:00.000000002Z")
        later["payload"]["observed_sha256"] = HASH
        self.ingest(later)
        self.ingest(earlier)
        item = c.query(self.db, "artifact", {})["items"][0]
        self.assertEqual(item["event_id"], "a")
        self.assertEqual(item["observed_at"], later["observed_at"])
        self.assertEqual(item["data"]["observed_sha256"], item["data"]["sha256"])
        mismatch = event("mismatch")
        mismatch["payload"]["observed_sha256"] = "cd" * 32
        with self.assertRaises(c.CatalogError):
            self.ingest(mismatch)
        mismatch["payload"]["observed_sha256"] = HASH
        mismatch["payload"]["state"] = "present"
        with self.assertRaises(c.CatalogError):
            self.ingest(mismatch)

    def test_independent_sources_and_stages_never_inferred(self):
        self.ingest(event("r", "run"))
        snapshot = c.snapshot(self.db)
        stages = {x["id"]: x for x in snapshot["stages"]}
        self.assertEqual(stages["runs"]["count"], 1)
        for stage in c.STAGES:
            self.assertEqual(stages[stage]["state"], "unknown")
            self.assertIsNone(stages[stage]["count"])
        missing = event("m")
        missing["payload"]["state"] = "missing"
        other = event("m", source="other-archive")
        self.ingest(missing, other)
        artifacts = c.query(self.db, "artifact", {})["items"]
        self.assertEqual(len(artifacts), 2)
        self.assertEqual({i["data"]["state"] for i in artifacts}, {"missing", "verified"})
        self.assertEqual(len(c.plan_missing(self.db, PAGE)["items"]), 1)
        self.assertTrue(c.plan_missing(self.db, PAGE)["read_only"])
        self.assertEqual(len(c.query(self.db, "artifact", {})["items"]), 2)

    def test_artifact_and_run_identity_hash_conflicts(self):
        self.ingest(event(), event("r1", "run"))
        for field, value in (("sha256", "cd" * 32), ("reference", "other/page.jp2")):
            conflict = event("conflict-" + field, when=T1)
            conflict["payload"][field] = value
            with self.subTest(field=field), self.assertRaisesRegex(c.CatalogError, "identity conflict"):
                self.ingest(conflict)
        conflict = event("r2", "run", T1)
        conflict["payload"]["run_record"]["options_json"] = '{"psm":6}'
        with self.assertRaisesRegex(c.CatalogError, "identity conflict"):
            self.ingest(conflict)
        changed = event("v2", when=T1)
        changed["payload"].update(version="v2", sha256="cd" * 32)
        self.ingest(changed)
        self.assertEqual(len(c.query(self.db, "artifact", {})["items"]), 2)

    def test_no_text_or_free_form_secrets_persisted(self):
        record = run_record()
        # Real contract free-form fields may contain arbitrary metadata; only hashes survive.
        record.update(models_json='{"password":"never-store-model-secret"}',
                      options_json='{"text":"never-store-OCR-text"}',
                      engine_stats_json='{"text":"never-store-engine-text"}',
                      image_ref="https://registry.invalid/image?" + urllib.parse.urlencode({"sig": "never-store-SAS"}),
                      adapter="never-store-adapter", node="never-store-node",
                      ocr_engine="never-store-engine-tag")
        self.ingest(event("r", "run", payload={"run_record": record}))
        rendered = json.dumps(c.page(self.db, PAGE)) + json.dumps(c.snapshot(self.db))
        self.assertNotIn("never-store", rendered)
        for file in self.work.glob("catalog.sqlite*"):
            self.assertNotIn(b"never-store", file.read_bytes())
        self.assertFalse(set(record) & {"ocr_text", "text"})

    def test_schema_types_hashes_and_uri_validation(self):
        cases = []
        for field, value in (("schema_version", 2), ("schema_version", True),
                             ("page_id", "unqualified"), ("observed_at", "2026-01-01")):
            cases.append(event(**{field: value}))
        for field, value in (("sha256", "short"), ("state", "ready"),
                             ("reference", "https://blob/scan?" + urllib.parse.urlencode({"sig": "secret"})),
                             ("reference", "../private"), ("reference", "/absolute"),
                             ("artifact_id", "password")):
            value_event = event()
            value_event["payload"][field] = value
            cases.append(value_event)
        for field, value in (("schema", 2), ("schema", True), ("seconds_wall", -1),
                             ("selected", 1), ("image_sha256", "12"),
                             ("mean_conf", 1.1), ("contract", False), ("doc_id", "other"),
                             ("options_json", "[]"), ("engine_id", "bearer/abc"),
                             ("scale", 0), ("dpi", 4294967296)):
            value_event = event(kind="run")
            value_event["payload"]["run_record"][field] = value
            cases.append(value_event)
        extra = event()
        extra["payload"]["ocr_text"] = "must reject"
        cases.append(extra)
        extra = event(kind="run")
        extra["payload"]["run_record"]["ocr_text"] = "must reject"
        cases.append(extra)
        missing = event(kind="run")
        del missing["payload"]["run_record"]["node"]
        cases.append(missing)
        for case in cases:
            with self.subTest(case=case), self.assertRaises(c.CatalogError):
                self.ingest(case)
        self.assertFalse(self.db.exists())
        for raw in ('{"a":1,"a":2}', '{"a":NaN}', '{"a":Infinity}', '{"a":1e999}'):
            with self.assertRaises(c.CatalogError):
                c.load_json(raw)

    def test_bounded_inputs_outputs_pagination_and_deadline(self):
        events = []
        for n in range(6):
            value = event("e" + str(n), kind="run")
            value["payload"]["run_record"]["run_id"] = "run-" + str(n)
            events.append(value)
        self.ingest(*events)
        seen, cursor = [], ""
        while True:
            result = c.query(self.db, "run", {}, 2, cursor)
            seen.extend(i["data"]["run_id"] for i in result["items"])
            if not result["next_cursor"]:
                break
            cursor = result["next_cursor"]
        self.assertEqual(len(seen), len(set(seen)))
        self.assertEqual(len(seen), 6)
        for limit in (0, 101, -1, True):
            with self.assertRaises(c.CatalogError):
                c.query(self.db, "run", {}, limit)
        with self.assertRaises(c.CatalogError):
            c.query(self.db, "run", {}, after="';DROP TABLE latest")
        with self.assertRaises(c.CatalogError):
            c.query(self.db, "run", {"sql": "SELECT 1"})
        with self.assertRaises(sqlite3.OperationalError):
            c.bounded_query(self.db, lambda conn: conn.execute("DELETE FROM latest"))
        original = c.QUERY_SECONDS
        try:
            c.QUERY_SECONDS = -1
            with self.assertRaises(sqlite3.OperationalError):
                c.bounded_query(self.db, lambda conn: conn.execute("""WITH RECURSIVE
                  n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<100000)
                  SELECT sum(x) FROM n""").fetchone()[0])
        finally:
            c.QUERY_SECONDS = original
        overlong = event("long")
        overlong["source"] = "a" * 257
        with self.assertRaises(c.CatalogError):
            self.ingest(overlong)
        path = self.work / "oversize.jsonl"
        with path.open("wb") as stream:
            stream.truncate(c.MAX_BATCH + 1)
        with self.assertRaises(c.CatalogError):
            c.ingest(self.db, path)
        path.write_text(" " * (c.MAX_LINE + 1))
        with self.assertRaises(c.CatalogError):
            c.ingest(self.db, path)

    def test_aggregates_bounded_independent_not_inventory(self):
        self.ingest(event("s", "stage"))
        result = c.snapshot(self.db)
        self.assertEqual(result["runs"], {"total": 0, "pages": 0})
        self.assertEqual(result["artifacts"], {"locations": 0, "verified": 0, "missing": 0})
        self.assertEqual(c.page(self.db, PAGE)["items"], [])
        unknown = event("newer", "stage", T1)
        unknown["payload"].update(state="unknown", count=None)
        self.ingest(unknown)
        self.ingest(event("stale", "stage"))
        observed = next(s for s in c.snapshot(self.db)["stages"] if s["id"] == "boinc-ready")
        self.assertIsNone(observed["count"])
        self.assertEqual(observed["state"], "unknown")
        other = event("s", "stage", source="other-source")
        self.ingest(other)
        stages = [s for s in c.snapshot(self.db)["stages"] if s["id"].startswith("boinc-ready")]
        self.assertEqual(len(stages), 2)
        self.assertEqual(len({s["id"] for s in stages}), 2)
        for field, value in (("count", -1), ("count", True), ("count", 1000000000001),
                             ("label", "a" * 129), ("stage_id", "accepted"),
                             ("detail", "secret=value"), ("state", "ready")):
            bad = event("bad", "stage")
            bad["payload"][field] = value
            with self.subTest(field=field), self.assertRaises(c.CatalogError):
                self.ingest(bad)
        values = []
        for i in range(c.MAX_AGGREGATES):
            value = event("agg-" + str(i), "stage")
            value["payload"]["stage_id"] = "aggregate-" + str(i)
            values.append(value)
        with self.assertRaisesRegex(c.CatalogError, "stage limit"):
            self.ingest(*values)
        self.assertEqual(len(c.query(self.db, "stage", {})["items"]), 2)
        snap = c.snapshot(self.db)
        self.assertLessEqual(len(snap["coverage"]), 32)
        for stage in snap["stages"]:
            self.assertLessEqual(len(stage["id"]), 256)
            self.assertLessEqual(len(stage["detail"]), 512)

    def test_atomic_snapshot_safe_paths_and_existing_schema(self):
        self.ingest(event())
        before = c.snapshot(self.db)
        for out in (self.db, Path(str(self.db) + "-wal"), Path(str(self.db) + "-shm"),
                    Path(str(self.db) + "-journal")):
            with self.assertRaises(c.CatalogError):
                c.atomic_snapshot(out, before, self.db)
        target = self.work / "target"
        target.write_text("unchanged")
        link = self.work / "snapshot-link"
        link.symlink_to(target)
        with self.assertRaises(c.CatalogError):
            c.atomic_snapshot(link, before, self.db)
        self.assertEqual(target.read_text(), "unchanged")
        oversized = {"long": "x" * c.MAX_RESULT}
        with self.assertRaises(c.CatalogError):
            c.atomic_snapshot(target, oversized, self.db)
        self.assertEqual(target.read_text(), "unchanged")
        conn = sqlite3.connect(self.db)
        conn.execute("UPDATE meta SET version=99")
        conn.commit()
        conn.close()
        with self.assertRaises(c.CatalogError):
            c.snapshot(self.db)

    def test_http_filtering_pagination_errors_and_read_only(self):
        records = []
        for n in range(3):
            value = event("r" + str(n), "run")
            value["payload"]["run_record"]["run_id"] = "run-" + str(n)
            if n == 2:
                value["payload"]["run_record"].update(status="bad_input", selected=False)
            records.append(value)
        self.ingest(*records)
        with c.make_server(self.db) as server:
            thread = threading.Thread(target=server.serve_forever)
            thread.start()
            base = "http://127.0.0.1:" + str(server.server_address[1])
            def get(path):
                with urllib.request.urlopen(base + path, timeout=5) as response:
                    self.assertEqual(response.headers["Cache-Control"], "no-store")
                    return json.load(response)
            try:
                health = get("/health")
                self.assertEqual(health["catalog"], "readable")
                self.assertEqual(health["observed_at"], c.timestamp(T0))
                first = get("/runs?engine=tesseract-5.5&status=ok&limit=1")
                self.assertEqual(len(first["items"]), 1)
                second = get("/runs?engine=tesseract-5.5&status=ok&limit=1&after=" + first["next_cursor"])
                self.assertNotEqual(first["items"][0]["key"], second["items"][0]["key"])
                self.assertIsNone(second["next_cursor"])
                self.assertEqual(get("/runs?status=bad_input")["items"][0]["data"]["status"], "bad_input")
                detail = get("/page?" + urllib.parse.urlencode({"page_id": PAGE, "limit": 2}))
                self.assertEqual(len(detail["items"]), 2)
                self.assertIsNotNone(detail["next_cursor"])
                for path, code in (("/sql?query=delete", 404), ("/page", 400),
                                   ("/runs?limit=101", 400), ("/runs?limit=x", 400),
                                   ("/runs?status=ok&status=bad", 400), ("/runs?text=abc", 400),
                                   ("/runs?after=not-a-cursor", 400)):
                    with self.subTest(path=path), self.assertRaises(urllib.error.HTTPError) as error:
                        get(path)
                    self.assertEqual(error.exception.code, code)
                    error.exception.close()
                request = urllib.request.Request(base + "/runs", data=b"{}", method="POST")
                with self.assertRaises(urllib.error.HTTPError) as error:
                    urllib.request.urlopen(request, timeout=5)
                self.assertEqual(error.exception.code, 501)
                error.exception.close()
            finally:
                server.shutdown()
                thread.join(timeout=5)
        self.assertEqual(len(c.query(self.db, "run", {})["items"]), 3)
        with self.assertRaises(c.CatalogError):
            c.make_server(self.db, "0.0.0.0")

    def test_cli_serve_starts_and_responds(self):
        self.ingest(event("r", "run"))
        process = subprocess.Popen([sys.executable, str(SCRIPT), "--db", str(self.db),
                                    "serve", "--port", "0"], stdout=subprocess.PIPE,
                                   stderr=subprocess.PIPE, text=True, cwd=ROOT)
        try:
            # The child emits one flushed bound-address message before serving.
            import select
            self.assertTrue(select.select([process.stdout], [], [], 5)[0], "server did not start")
            address = json.loads(process.stdout.readline())["listening"]
            with urllib.request.urlopen("http://127.0.0.1:" + str(address[1]) + "/runs",
                                        timeout=5) as response:
                self.assertEqual(len(json.load(response)["items"]), 1)
            self.assertIsNone(process.poll())
        finally:
            process.terminate()
            process.communicate(timeout=5)

    def test_help_describes_missing_adapters_and_production_bootstrap(self):
        result = subprocess.run([sys.executable, str(SCRIPT), "--help"],
                                capture_output=True, text=True, timeout=5)
        self.assertEqual(result.returncode, 0)
        for phrase in ("RunRecord", "NOT provided", "Parquet", "BOINC", "Bootstrap",
                       "page_id", "batch_id", "stage_id", "underlying cache age unknown"):
            self.assertIn(phrase, result.stdout)

    def test_mere_worker_success_or_imported_never_implies_accepted_or_published(self):
        sync_rs = (ROOT / "crates/usnm-ocr-local/src/sync.rs").read_text()
        import_struct = sync_rs.split("pub struct ImportRecord {", 1)[1].split("\n}", 1)[0]
        self.assertEqual(set(re.findall(r"pub (\w+):", import_struct)),
                         {"run", "at", "index_version", "sources", "pages", "outcomes"})
        consumed_struct = sync_rs.split("pub struct Consumed {", 1)[1].split("\n}", 1)[0]
        self.assertEqual(set(re.findall(r"pub (\w+):", consumed_struct)),
                         {"path", "etag", "outcome", "reason"})

        worker_file = ROOT / "scripts/corpus/fixtures/data_catalog/worker-success.json"
        worker_meta = json.loads(worker_file.read_text())
        doc_id = worker_meta["doc_id"]
        page_id = "loc:" + doc_id
        run_id = worker_meta["run_id"]
        self.cli("ingest-worker", "--input", str(worker_file),
                 "--evidence-out", str(self.work / "worker.jsonl"),
                 "--batch-id", "worker-ok", "--source", "worker-capture")
        after_worker = c.trace(self.db, page_id)
        self.assertEqual(after_worker["execution"]["state"], "observed")
        self.assertEqual(after_worker["imported"]["state"], "unknown")
        self.assertFalse(after_worker["imported"]["imported"])
        self.assertEqual(after_worker["release_decision"]["state"], "unknown")
        self.assertEqual(after_worker["release_decision"]["decision"], "unknown")
        self.assertFalse(after_worker["accepted"])
        self.assertEqual(after_worker["search_provenance"]["state"], "unknown")
        self.assertEqual(after_worker["search_provenance"]["decision"], "unknown")
        self.assertFalse(after_worker["published"])
        self.assertFalse(after_worker["publication_proof_complete"])

        import_fixture = ROOT / "scripts/corpus/fixtures/data_catalog/ocr-sync-import.json"
        import_record = json.loads(import_fixture.read_text())
        import_id = import_record["run"]
        committed_at = import_record["at"]
        import_evidence = self.work / "import-evidence.jsonl"
        res = self.cli("ingest-import", "--input", str(import_fixture),
                       "--evidence-out", str(import_evidence),
                       "--batch-id", "import-1", "--source", "ocr-sync")
        self.assertEqual(res["inserted"], 1)
        self.assertNotIn("0x8DD000000000001", import_evidence.read_text())
        self.assertNotIn(b"0x8DD000000000001", self.db.read_bytes())
        self.assertNotIn("20261016T201100000000Z", import_evidence.read_text())

        # Reject synthetic/non-producer import payloads and mismatched ImportRecord companions.
        for bad_import in (
            {"run": import_id, "committed_at": committed_at, "part": "ocr-local/pages/x.parquet",
             "rows": 1, "imported": [{"doc_id": doc_id, "run_id": run_id, "node": "node1"}]},
            dict(import_record, sources=[import_record["sources"][0]]),
            dict(import_record, pages=[]),
            dict(import_record, outcomes=None),
        ):
            bad_path = self.work / "bad-import.json"
            bad_path.write_text(c.canonical(bad_import) + "\n")
            with self.subTest(bad_import=bad_import), self.assertRaises(c.CatalogError):
                c.ingest_import(self.db, bad_path, self.work / ("bad-" + uuid.uuid4().hex + ".jsonl"),
                                "bad-imp", "ocr-sync")

        after_import = c.trace(self.db, page_id)
        self.assertEqual(after_import["run_id"], run_id)
        self.assertEqual(after_import["imported"]["state"], "present")
        self.assertTrue(after_import["imported"]["imported"])
        self.assertEqual(after_import["imported"]["import_id"], import_id)
        self.assertEqual(after_import["imported"]["committed_at"], committed_at)
        self.assertEqual(after_import["release_decision"]["state"], "unknown")
        self.assertEqual(after_import["release_decision"]["decision"], "unknown")
        self.assertFalse(after_import["release_decision"]["accepted"])
        self.assertFalse(after_import["accepted"])
        self.assertEqual(after_import["search_provenance"]["state"], "unknown")
        self.assertEqual(after_import["search_provenance"]["decision"], "unknown")
        self.assertFalse(after_import["search_provenance"]["published"])
        self.assertFalse(after_import["published"])
        self.assertFalse(after_import["publication_proof_complete"])

        stages = {s["id"]: s for s in c.snapshot(self.db)["stages"]}
        self.assertEqual(stages["imported"]["state"], "observed")
        self.assertEqual(stages["imported"]["count"], 1)
        self.assertEqual(stages["accepted"]["state"], "unknown")
        self.assertIsNone(stages["accepted"]["count"])
        self.assertEqual(stages["indexed"]["state"], "unknown")
        self.assertIsNone(stages["indexed"]["count"])

    def test_release_decisions_accepted_rejected_revoked_and_published_provenance_roundtrip(self):
        hits_rs = (ROOT / "crates/usnm-api/src/routes/hits.rs").read_text()
        hits_struct = hits_rs.split("struct HitsResponse {", 1)[1].split("\n}", 1)[0]
        self.assertEqual(set(re.findall(r"^\s*(\w+):", hits_struct, re.M)), c.HITS_RESPONSE_FIELDS)

        doc_a = "sn83045462_1890-05-03_ed-1_seq-7"
        page_a = "loc:" + doc_a
        run_a = "node1.20261016T201020618734Z"
        doc_b = "sn83045462_1890-05-03_ed-1_seq-8"
        page_b = "loc:" + doc_b
        run_b = "node1.20261016T201500000000Z"
        import_id = "20261017T181722229390Z-0ce09259-0000"
        committed_at = "2026-10-17T18:17:37.870842308Z"
        decided_at = "2026-10-18T01:00:00.000000000Z"
        published_at = "2026-10-18T02:00:00.000000000Z"
        verified_at = "2026-10-18T02:05:00.000000000Z"

        release_file = self.work / "release-v1.json"
        release_file.write_text(c.canonical({
            "schema": 1,
            "release_id": "v20261018-1",
            "decided_at": decided_at,
            "decisions": [
                {
                    "doc_id": doc_a,
                    "run_id": run_a,
                    "import_id": import_id,
                    "committed_at": committed_at,
                    "decision": "accepted",
                    "decision_reason": "rule_v1",
                    "selected_source": "usnm-tesseract",
                    "selected_engine": "tesseract/5.5.1 deu_latf+deu",
                },
                {
                    "doc_id": doc_b,
                    "run_id": run_b,
                    "import_id": import_id,
                    "committed_at": committed_at,
                    "decision": "rejected",
                    "decision_reason": "margin",
                    "selected_source": "loc",
                    "selected_engine": None,
                },
            ],
        }) + "\n")
        res = self.cli("ingest-release", "--input", str(release_file),
                       "--evidence-out", str(self.work / "release-v1.jsonl"),
                       "--batch-id", "release-v1", "--source", "release-builder")
        self.assertEqual(res["inserted"], 2)

        trace_a = c.trace(self.db, page_a)
        self.assertTrue(trace_a["accepted"])
        self.assertFalse(trace_a["published"])
        self.assertFalse(trace_a["publication_proof_complete"])
        self.assertEqual(trace_a["release_decision"]["decision"], "accepted")
        self.assertEqual(trace_a["release_decision"]["decision_reason"], "rule_v1")
        self.assertEqual(trace_a["release_decision"]["release_id"], "v20261018-1")
        self.assertEqual(trace_a["release_decision"]["run_id"], run_a)
        self.assertEqual(trace_a["release_decision"]["import_id"], import_id)
        self.assertEqual(trace_a["release_decision"]["selected_source"], "usnm-tesseract")
        self.assertEqual(trace_a["release_decision"]["selected_engine"], "tesseract/5.5.1 deu_latf+deu")
        self.assertEqual(trace_a["release_decision"]["decided_at"], decided_at)
        self.assertEqual(trace_a["release_decision"]["committed_at"], committed_at)

        trace_b = c.trace(self.db, page_b)
        self.assertFalse(trace_b["accepted"])
        self.assertFalse(trace_b["published"])
        self.assertFalse(trace_b["publication_proof_complete"])
        self.assertEqual(trace_b["release_decision"]["state"], "missing")
        self.assertEqual(trace_b["release_decision"]["decision"], "rejected")
        self.assertEqual(trace_b["release_decision"]["decision_reason"], "margin")
        self.assertEqual(trace_b["release_decision"]["selected_source"], "loc")
        self.assertIsNone(trace_b["release_decision"]["selected_engine"])

        stages = {s["id"]: s for s in c.snapshot(self.db)["stages"]}
        self.assertEqual(stages["accepted"]["state"], "observed")
        self.assertEqual(stages["accepted"]["count"], 1)
        self.assertEqual(len(c.query(self.db, "page_state", {"status": "accepted"})["items"]), 1)
        self.assertEqual(len(c.query(self.db, "page_state", {"status": "rejected"})["items"]), 1)

        # Filtered/paginated /v1/hits responses omitting the target page, using `hits`,
        # or marked `synthetic=true` fail closed without writing evidence or mutating indexed state.
        for bad_hits in (
            {"index_version": "pages-v20261018-1", "hits": [{"doc_id": doc_a}]},
            {"index_version": "pages-v20261018-1", "synthetic": False, "place": None,
             "title": None, "total": 0, "items": [], "next_cursor": None},
            {"index_version": "pages-v20261018-1", "synthetic": False, "place": None,
             "title": None, "total": 1, "items": [{"doc_id": doc_b}], "next_cursor": "eyJvIjoxfQ"},
            {"index_version": "pages-v20261018-1", "synthetic": True, "place": None,
             "title": None, "total": 1, "items": [{
                 "doc_id": doc_a,
                 "ocr": {"source": "usnm-tesseract", "engine": "tesseract/5.5.1 deu_latf+deu"},
             }], "next_cursor": None},
        ):
            bad_hits_file = self.work / "bad-hits.json"
            bad_hits_file.write_text(c.canonical(bad_hits) + "\n")
            bad_ev = self.work / ("bad-s-" + uuid.uuid4().hex + ".jsonl")
            with self.subTest(bad_hits=bad_hits), self.assertRaises(c.CatalogError):
                c.ingest_search_provenance(
                    self.db, bad_hits_file, bad_ev,
                    "bad-search", "public-hits-verifier", page_a, verified_at, run_id=run_a)
            self.assertFalse(bad_ev.exists())
        self.assertEqual(c.trace(self.db, page_a)["search_provenance"]["state"], "unknown")

        hits_file = self.work / "hits-v20261018-1.json"
        hits_raw = c.canonical({
            "index_version": "pages-v20261018-1",
            "synthetic": False,
            "place": None,
            "title": {"lccn": "sn83045462", "name": "never-persist-newspaper-title"},
            "total": 1,
            "days": 1,
            "items": [{
                "doc_id": doc_a,
                "date": "1890-05-03",
                "lccn": "sn83045462",
                "title": "never-persist-newspaper-title",
                "place_id": "us-ne-gage",
                "edition": 1,
                "seq": 7,
                "front_page": False,
                "snippets": ["never-persist-ocr-snippet-text"],
                "ocr": {
                    "source": "usnm-tesseract",
                    "engine": "tesseract/5.5.1 deu_latf+deu",
                },
                "links": {"viewer": None},
            }],
            "next_cursor": None,
        }) + "\n"
        hits_file.write_text(hits_raw)
        search_ev = self.work / "search-v1.jsonl"
        self.cli("ingest-search", "--input", str(hits_file),
                 "--evidence-out", str(search_ev),
                 "--batch-id", "search-v1", "--source", "public-hits-verifier",
                 "--page-id", page_a, "--verified-at", verified_at,
                 "--run-id", run_a, "--release-id", "v20261018-1",
                 "--import-id", import_id, "--published-at", published_at)
        self.assertNotIn("never-persist", search_ev.read_text())
        self.assertNotIn(b"never-persist", self.db.read_bytes())

        trace_a = c.trace(self.db, page_a)
        self.assertTrue(trace_a["accepted"])
        self.assertTrue(trace_a["published"])
        # Still false because artifact, worker execution/run, and import are not yet recorded for page_a.
        self.assertFalse(trace_a["publication_proof_complete"])
        sp = trace_a["search_provenance"]
        self.assertEqual(sp["state"], "present")
        self.assertEqual(sp["decision"], "accepted")
        self.assertEqual(sp["release_id"], "v20261018-1")
        self.assertEqual(sp["index_id"], "pages-v20261018-1")
        self.assertEqual(sp["run_id"], run_a)
        self.assertEqual(sp["import_id"], import_id)
        self.assertEqual(sp["selected_source"], "usnm-tesseract")
        self.assertEqual(sp["selected_engine"], "tesseract/5.5.1 deu_latf+deu")
        self.assertEqual(sp["published_at"], published_at)
        self.assertEqual(sp["verified_at"], verified_at)
        self.assertEqual(sp["search_receipt"], {
            "index_version": "pages-v20261018-1",
            "hit_present": True,
            "ocr_source": "usnm-tesseract",
            "ocr_engine": "tesseract/5.5.1 deu_latf+deu",
            "response_sha256": c.hashlib.sha256(hits_raw.encode()).hexdigest(),
            "verified_at": verified_at,
        })
        self.assertEqual({s["id"]: s for s in c.snapshot(self.db)["stages"]}["indexed"]["count"], 1)

        # A newer baseline verification without --run-id from the same source supersedes
        # the earlier run-scoped replacement row in `latest` (and snapshot) while keeping history in `events`.
        baseline_verified_at = "2026-10-18T02:30:00.000000000Z"
        baseline_hits_file = self.work / "hits-baseline-rollback.json"
        baseline_hits_raw = c.canonical({
            "index_version": "pages-v20261010-2",
            "synthetic": False,
            "place": None,
            "title": None,
            "total": 1,
            "items": [{
                "doc_id": doc_a,
                "date": "1890-05-03",
                "lccn": "sn83045462",
                "title": None,
                "place_id": "us-ne-gage",
                "edition": 1,
                "seq": 7,
                "front_page": False,
                "snippets": [],
                "links": {"viewer": None},
            }],
            "next_cursor": None,
        }) + "\n"
        baseline_hits_file.write_text(baseline_hits_raw)
        self.cli("ingest-search", "--input", str(baseline_hits_file),
                 "--evidence-out", str(self.work / "search-baseline.jsonl"),
                 "--batch-id", "search-baseline", "--source", "public-hits-verifier",
                 "--page-id", page_a, "--verified-at", baseline_verified_at)
        trace_after_baseline = c.trace(self.db, page_a)
        self.assertFalse(trace_after_baseline["published"])
        self.assertEqual(trace_after_baseline["search_provenance"]["decision"], "unchanged_baseline")
        self.assertIsNone(trace_after_baseline["search_provenance"]["run_id"])
        self.assertFalse(c.trace(self.db, page_a, run_id=run_a)["published"])
        stages = {s["id"]: s for s in c.snapshot(self.db)["stages"]}
        self.assertEqual(stages["indexed"]["state"], "observed")
        self.assertEqual(stages["indexed"]["count"], 0)
        conn = sqlite3.connect(self.db)
        try:
            self.assertEqual(conn.execute(
                "SELECT count(*) FROM events WHERE source='public-hits-verifier'").fetchone()[0], 2)
            self.assertEqual(conn.execute(
                "SELECT count(*) FROM latest WHERE kind='page_state' AND stage='indexed' AND page_id=?",
                (page_a,)).fetchone()[0], 1)
        finally:
            conn.close()

        # Upgrade regression: existing schema-v1 databases (user_version=0) with legacy run-scoped indexed keys
        # collapse and migrate on writable open (including batch replay), persist the migration marker,
        # skip enumeration on normal opens, and are superseded by newer baseline rows.
        conn = sqlite3.connect(self.db)
        try:
            cur_row = conn.execute(
                "SELECT * FROM latest WHERE kind='page_state' AND stage='indexed' AND page_id=?",
                (page_a,),
            ).fetchone()
            col_names = [d[0] for d in conn.execute("SELECT * FROM latest LIMIT 0").description]
            row_dict = dict(zip(col_names, cur_row))
            conn.execute("DELETE FROM latest WHERE key=?", (row_dict["key"],))
            legacy_key_1 = c.digest(["public-hits-verifier", "page_state", [page_a, "indexed", "old-run"]])
            legacy_key_2 = c.digest(["public-hits-verifier", "page_state", [page_a, "indexed", run_a]])
            older_data = json.loads(row_dict["data"])
            older_data.update(state="present", run_id="old-run", decision="accepted")
            newer_data = json.loads(row_dict["data"])
            newer_data.update(state="present", run_id=run_a, decision="accepted")
            conn.execute(
                "INSERT INTO latest VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?)",
                (legacy_key_1, "", "page_state", "public-hits-verifier", page_a,
                 "legacy-ev-1", "2026-10-18T02:01:00.000000000Z", c.canonical(older_data),
                 None, "accepted", "old-run", "indexed", "present"),
            )
            conn.execute(
                "INSERT INTO latest VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?)",
                (legacy_key_2, "", "page_state", "public-hits-verifier", page_a,
                 row_dict["event_id"], "2026-10-18T02:05:00.000000000Z", c.canonical(newer_data),
                 None, "accepted", run_a, "indexed", "present"),
            )
            conn.execute("PRAGMA user_version=0")
            conn.commit()
        finally:
            conn.close()
        # Read-only open refuses unmigrated legacy indexed keys rather than miscounting snapshot.
        with self.assertRaises(c.CatalogError):
            c.snapshot(self.db)
        # Replaying a known batch into the existing database triggers migration, collapses the keys,
        # and persists user_version=2 so subsequent opens skip scanning latest.
        self.assertTrue(c.ingest(self.db, search_ev)["replayed"])
        conn = sqlite3.connect(self.db)
        try:
            self.assertEqual(conn.execute("PRAGMA user_version").fetchone()[0], c.INDEXED_KEY_SCOPE_VERSION)
        finally:
            conn.close()
        with mock.patch.object(c, "migrate_indexed_keys", side_effect=AssertionError("unexpected scan")):
            self.assertEqual({s["id"]: s for s in c.snapshot(self.db)["stages"]}["indexed"]["count"], 1)
        # Ingesting a newer baseline from the same source now replaces the migrated key in latest & snapshot.
        baseline_hits_2 = self.work / "hits-baseline-rollback-2.json"
        baseline_hits_2.write_text(baseline_hits_raw)
        self.cli("ingest-search", "--input", str(baseline_hits_2),
                 "--evidence-out", str(self.work / "search-baseline-2.jsonl"),
                 "--batch-id", "search-baseline-2", "--source", "public-hits-verifier",
                 "--page-id", page_a, "--verified-at", "2026-10-18T02:40:00.000000000Z")
        self.assertFalse(c.trace(self.db, page_a)["published"])
        self.assertEqual({s["id"]: s for s in c.snapshot(self.db)["stages"]}["indexed"]["count"], 0)

        # Revocation without run_id is rejected so it cannot leave run-scoped acceptance intact.
        bad_revoke_file = self.work / "release-revoke-null.json"
        bad_revoke_file.write_text(c.canonical({
            "schema": 1,
            "release_id": "v20261018-2",
            "decided_at": "2026-10-18T02:45:00.000000000Z",
            "decisions": [{
                "doc_id": doc_a,
                "run_id": None,
                "decision": "revoked",
                "decision_reason": "revoked",
                "selected_source": "loc",
                "selected_engine": None,
            }],
        }) + "\n")
        with self.assertRaises(c.CatalogError):
            c.ingest_release(self.db, bad_revoke_file, self.work / "bad-revoke.jsonl",
                             "bad-revoke", "release-builder")
        self.assertTrue(c.trace(self.db, page_a, run_id=run_a)["accepted"])

        # Explicit revocation with target run_id supersedes prior acceptance in latest while preserving event history.
        revoked_at = "2026-10-18T03:00:00.000000000Z"
        revoke_file = self.work / "release-revoke.json"
        revoke_file.write_text(c.canonical({
            "schema": 1,
            "release_id": "v20261018-2",
            "decided_at": revoked_at,
            "decisions": [{
                "doc_id": doc_a,
                "run_id": run_a,
                "import_id": import_id,
                "committed_at": committed_at,
                "decision": "revoked",
                "decision_reason": "revoked",
                "selected_source": "loc",
                "selected_engine": None,
            }],
        }) + "\n")
        self.cli("ingest-release", "--input", str(revoke_file),
                 "--evidence-out", str(self.work / "release-revoke.jsonl"),
                 "--batch-id", "release-revoke", "--source", "release-builder")
        trace_after_revoke = c.trace(self.db, page_a)
        self.assertFalse(trace_after_revoke["accepted"])
        self.assertEqual(trace_after_revoke["release_decision"]["state"], "blocked")
        self.assertEqual(trace_after_revoke["release_decision"]["decision"], "revoked")
        self.assertEqual(trace_after_revoke["release_decision"]["decision_reason"], "revoked")
        self.assertEqual(trace_after_revoke["release_decision"]["release_id"], "v20261018-2")
        self.assertEqual(trace_after_revoke["release_decision"]["selected_source"], "loc")
        self.assertIsNone(trace_after_revoke["release_decision"]["selected_engine"])
        self.assertEqual(trace_after_revoke["release_decision"]["decided_at"], revoked_at)
        stages = {s["id"]: s for s in c.snapshot(self.db)["stages"]}
        self.assertEqual(stages["accepted"]["state"], "blocked")
        self.assertEqual(stages["accepted"]["count"], 0)

        # Invalid decision/state combinations fail closed.
        for bad_payload in (
            {"stage": "accepted", "state": "missing", "run_id": run_a, "decision": "accepted"},
            {"stage": "accepted", "state": "present", "run_id": None, "decision": "accepted"},
            {"stage": "accepted", "state": "blocked", "run_id": None, "decision": "revoked"},
            {"stage": "accepted", "state": "present", "run_id": run_a, "decision": "rejected"},
            {"stage": "accepted", "state": "present", "run_id": run_a, "decision": "revoked"},
            {"stage": "accepted", "state": "present", "run_id": run_a, "decision": "unchanged_baseline"},
            {"stage": "imported", "state": "present", "run_id": run_a, "decision": "accepted"},
            {"stage": "indexed", "state": "missing", "run_id": None, "decision": "unchanged_baseline",
             "search_receipt": {"index_version": "pages-v1", "hit_present": False, "ocr_source": None,
                                "ocr_engine": None, "response_sha256": HASH, "verified_at": verified_at}},
        ):
            with self.subTest(payload=bad_payload), self.assertRaises(c.CatalogError):
                c.validate_event(event("bad-ps", "page_state", payload=bad_payload))

    def test_issue_780_page_trace_committed_import_and_baseline_search_provenance(self):
        doc_id = "sn83045582_1884-06-01_ed-1_seq-6"
        issue_id = "sn83045582_1884-06-01_ed-1"
        page_id = "loc:" + doc_id
        run_id = "usnm-ocr-node3-arcs-prod.20261017T171358276982Z"
        import_id = "20261017T181722229390Z-0ce09259-0000"
        committed_at = "2026-10-17T18:17:37.870842308Z"
        verified_at = "2026-10-18T12:00:00.000000000Z"

        # 1. Verified scan artifact
        self.ingest(event("scan-780", "artifact", when="2026-10-17T17:00:00Z",
                          page_id=page_id, source="scan-inventory",
                          payload={"artifact_id": "scan", "sha256": HASH, "version": "v1",
                                   "location": "cached-scans",
                                   "reference": "keep/nbu_1884/sn83045582/1884-06-01/1/6.jpg",
                                   "state": "verified", "observed_sha256": HASH}))

        # 2. Worker execution & RunRecord for sn83045582_1884-06-01_ed-1_seq-6
        worker_meta = json.loads((ROOT / "scripts/corpus/fixtures/data_catalog/worker-success.json").read_text())
        worker_meta.update(run_id=run_id, doc_id=doc_id, language="deu",
                           damage_before=0.268639,
                           started_at="2026-10-17T17:13:58.276982Z",
                           finished_at="2026-10-17T17:14:01.100000Z")
        worker_meta["run_record"].update(run_id=run_id, doc_id=doc_id, language="deu",
                                         damage_before=0.268639, damage_after=0.099350,
                                         words=970, node_finished_at="2026-10-17T17:14:01.050000Z")
        worker_path = self.work / "worker-780.json"
        worker_path.write_text(c.canonical(worker_meta) + "\n")
        self.cli("ingest-worker", "--input", str(worker_path),
                 "--evidence-out", str(self.work / "worker-780.jsonl"),
                 "--batch-id", "worker-780", "--source", "worker-capture")

        # 3. Committed structural import 20261017T181722229390Z-0ce09259-0000 (ImportRecord schema)
        import_path = self.work / "import-780.json"
        import_path.write_text(c.canonical({
            "run": import_id,
            "at": committed_at,
            "index_version": "pages-v20261010-2",
            "sources": [
                {
                    "path": f"pages/{issue_id}.{run_id}.parquet",
                    "etag": "\"0x8DD181722229390\"",
                    "outcome": "imported",
                },
                {
                    "path": f"alto/{doc_id}.{run_id}.xml",
                    "etag": "\"0x8DD181722229391\"",
                    "outcome": "imported",
                },
                {
                    "path": f"runs/{issue_id}.{run_id}.parquet",
                    "etag": "\"0x8DD181722229392\"",
                    "outcome": "imported",
                },
            ],
            "pages": [f"ocr-local/pages/sn83045582/{import_id}.parquet"],
            "outcomes": f"ocr-local/outcomes/2026-10-17/{import_id}.parquet",
        }) + "\n")
        self.cli("ingest-import", "--input", str(import_path),
                 "--evidence-out", str(self.work / "import-780.jsonl"),
                 "--batch-id", "import-780", "--source", "ocr-sync-import")

        # 4. Read-only public /v1/hits response in pages-v20261010-2 without replacement ocr field
        hits_path = self.work / "hits-780.json"
        hits_raw = c.canonical({
            "index_version": "pages-v20261010-2",
            "synthetic": False,
            "place": None,
            "title": {"lccn": "sn83045582", "name": "Freie Presse"},
            "total": 1,
            "items": [{
                "doc_id": doc_id,
                "date": "1884-06-01",
                "lccn": "sn83045582",
                "title": "Freie Presse",
                "place_id": "us-il-cook-chicago",
                "edition": 1,
                "seq": 6,
                "front_page": False,
                "snippets": [],
                "links": {"viewer": None},
            }],
            "next_cursor": None,
        }) + "\n"
        hits_path.write_text(hits_raw)
        self.cli("ingest-search", "--input", str(hits_path),
                 "--evidence-out", str(self.work / "search-780.jsonl"),
                 "--batch-id", "search-780", "--source", "public-hits-check",
                 "--page-id", page_id, "--verified-at", verified_at,
                 "--run-id", run_id, "--import-id", import_id)

        traced = self.cli("trace", "--page-id", page_id)
        self.assertEqual(traced, self.cli("query", "trace", "--page-id", page_id))
        with c.make_server(self.db) as server:
            thread = threading.Thread(target=server.serve_forever)
            thread.start()
            try:
                url = ("http://127.0.0.1:" + str(server.server_address[1]) +
                       "/trace?" + urllib.parse.urlencode({"page_id": page_id}))
                with urllib.request.urlopen(url, timeout=5) as response:
                    self.assertEqual(json.load(response), traced)
            finally:
                server.shutdown()
                thread.join(timeout=5)

        self.assertEqual(traced["page_id"], page_id)
        self.assertEqual(traced["run_id"], run_id)
        self.assertFalse(traced["accepted"])
        self.assertFalse(traced["published"])
        self.assertFalse(traced["publication_proof_complete"])
        self.assertEqual(traced["artifact"]["state"], "verified")
        self.assertEqual(traced["execution"]["state"], "observed")
        self.assertEqual(traced["execution"]["runs"][0]["data"]["damage_before"], 0.268639)
        self.assertEqual(traced["execution"]["runs"][0]["data"]["damage_after"], 0.099350)
        self.assertEqual(traced["imported"]["state"], "present")
        self.assertTrue(traced["imported"]["imported"])
        self.assertEqual(traced["imported"]["import_id"], import_id)
        self.assertEqual(traced["imported"]["run_id"], run_id)
        self.assertEqual(traced["imported"]["committed_at"], committed_at)
        self.assertEqual(traced["release_decision"]["state"], "unknown")
        self.assertEqual(traced["release_decision"]["decision"], "unknown")
        self.assertFalse(traced["release_decision"]["accepted"])
        self.assertEqual(traced["search_provenance"]["state"], "missing")
        self.assertEqual(traced["search_provenance"]["decision"], "unchanged_baseline")
        self.assertEqual(traced["search_provenance"]["decision_reason"], "unchanged_baseline")
        self.assertFalse(traced["search_provenance"]["published"])
        self.assertEqual(traced["search_provenance"]["index_id"], "pages-v20261010-2")
        self.assertEqual(traced["search_provenance"]["selected_source"], "loc")
        self.assertIsNone(traced["search_provenance"]["selected_engine"])
        self.assertEqual(traced["search_provenance"]["search_receipt"], {
            "index_version": "pages-v20261010-2",
            "hit_present": True,
            "ocr_source": None,
            "ocr_engine": None,
            "response_sha256": c.hashlib.sha256(hits_raw.encode()).hexdigest(),
            "verified_at": verified_at,
        })
        stages = {s["id"]: s for s in c.snapshot(self.db)["stages"]}
        self.assertEqual(stages["imported"]["count"], 1)
        self.assertIsNone(stages["accepted"]["count"])
        self.assertEqual(stages["indexed"]["state"], "observed")
        self.assertEqual(stages["indexed"]["count"], 0)

    def test_acceptance_report_keeps_missing_pages_in_denominator(self):
        self.cli("init")
        cohort = [{"page_id": PAGE, "run_id": "unobserved-run"},
                  {"page_id": "loc:sn124_1900-01-01_ed-1_seq-1", "run_id": "other-run"}]
        path = self.work / "cohort.json"
        path.write_text(json.dumps(cohort))
        result = self.cli("acceptance-report", "--cohort", str(path),
                          "--index-version", "pages-v1", "--as-of", T1,
                          "--max-receipt-age-seconds", "3600")
        self.assertEqual(result["pages_expected"], 2)
        self.assertEqual(result["delivery_incomplete"], 2)
        self.assertEqual(result["delivery_verified"], 0)
        self.assertEqual(result["cohort_sha256"], c.digest(cohort))
        for page in result["pages"]:
            self.assertEqual(page["quality"], "unknown")
            self.assertIn("incomplete_publication_chain", page["delivery_blockers"])
            self.assertIn("missing_search_verification", page["delivery_blockers"])

    def test_acceptance_report_rejects_invalid_cohorts_and_limits(self):
        member = {"page_id": PAGE, "run_id": "r1"}
        for cohort in ([], [member, member], [member] * 101,
                       [{"page_id": PAGE}], [{"page_id": PAGE, "run_id": None}]):
            with self.subTest(cohort=cohort), self.assertRaises(c.CatalogError):
                c.acceptance_report(self.db, cohort, "pages-v1", T1, 3600)
        for age in (0, -1, True):
            with self.subTest(age=age), self.assertRaises(c.CatalogError):
                c.acceptance_report(self.db, [member], "pages-v1", T1, age)
        path = self.work / "cohort.json"
        path.write_bytes(b" " * (c.MAX_LINE + 1))
        failure = self.cli("acceptance-report", "--cohort", str(path),
                           "--index-version", "pages-v1", "--as-of", T1,
                           "--max-receipt-age-seconds", "3600", code=2)
        self.assertIn("64 KiB", failure["error"])

    def test_two_runs_trace_resolution_and_publication_proof_completeness(self):
        doc_id = "sn83045462_1890-05-03_ed-1_seq-7"
        issue_id = "sn83045462_1890-05-03_ed-1"
        page_id = "loc:" + doc_id
        run_a = "node1.20261016T201020618734Z"
        run_b = "node2.20261017T201020618734Z"
        import_a = "20261016T210000000000Z-11111111-0000"
        import_b = "20261017T210000000000Z-22222222-0000"
        committed_a = "2026-10-16T21:00:05.123456789Z"
        committed_b = "2026-10-17T21:00:05.123456789Z"

        # Two-run regression: acceptance for run A followed by import and search observations for run B
        # must never mix run B's import/publication summaries into run A's trace.
        release_a = self.work / "release-a.json"
        release_a.write_text(c.canonical({
            "schema": 1,
            "release_id": "v20261018-1",
            "decided_at": "2026-10-18T01:00:00.000000000Z",
            "decisions": [{
                "doc_id": doc_id,
                "run_id": run_a,
                "import_id": import_a,
                "committed_at": committed_a,
                "decision": "accepted",
                "decision_reason": "rule_v1",
                "selected_source": "usnm-tesseract",
                "selected_engine": "tesseract/5.5.1 deu_latf+deu",
            }],
        }) + "\n")
        c.ingest_release(self.db, release_a, self.work / "rel-a.jsonl", "rel-a", "release-builder")

        import_b_file = self.work / "import-b.json"
        import_b_file.write_text(c.canonical({
            "run": import_b,
            "at": committed_b,
            "index_version": "pages-v20261018-2",
            "sources": [
                {"path": f"pages/{issue_id}.{run_b}.parquet", "etag": "\"e1\"", "outcome": "imported"},
                {"path": f"alto/{doc_id}.{run_b}.xml", "etag": "\"e2\"", "outcome": "imported"},
            ],
            "pages": [f"ocr-local/pages/sn83045462/{import_b}.parquet"],
            "outcomes": f"ocr-local/outcomes/2026-10-17/{import_b}.parquet",
        }) + "\n")
        c.ingest_import(self.db, import_b_file, self.work / "imp-b.jsonl", "imp-b", "ocr-sync")

        hits_b_file = self.work / "hits-b.json"
        hits_b_file.write_text(c.canonical({
            "index_version": "pages-v20261018-2",
            "items": [{
                "doc_id": doc_id,
                "ocr": {"source": "usnm-tesseract", "engine": "tesseract/5.5.1 deu_latf+deu"},
            }],
        }) + "\n")
        c.ingest_search_provenance(
            self.db, hits_b_file, self.work / "search-b.jsonl", "search-b", "public-hits",
            page_id, "2026-10-18T02:00:00Z", run_id=run_b, release_id="v20261018-2", import_id=import_b)

        default_trace = c.trace(self.db, page_id)
        self.assertEqual(default_trace["run_id"], run_a)
        self.assertTrue(default_trace["accepted"])
        self.assertFalse(default_trace["imported"]["imported"])
        self.assertEqual(default_trace["imported"]["state"], "unknown")
        self.assertIsNone(default_trace["imported"]["run_id"])
        self.assertFalse(default_trace["published"])
        self.assertEqual(default_trace["search_provenance"]["state"], "unknown")
        self.assertFalse(default_trace["publication_proof_complete"])

        trace_b = c.trace(self.db, page_id, run_id=run_b)
        self.assertEqual(trace_b["run_id"], run_b)
        self.assertTrue(trace_b["imported"]["imported"])
        self.assertEqual(trace_b["imported"]["run_id"], run_b)
        self.assertFalse(trace_b["accepted"])
        self.assertTrue(trace_b["published"])
        self.assertEqual(trace_b["search_provenance"]["run_id"], run_b)
        self.assertFalse(trace_b["publication_proof_complete"])

        # Complete positive trace for run A requires artifact + worker execution/run + import + matching release & search.
        c.ingest_import(self.db, ROOT / "scripts/corpus/fixtures/data_catalog/ocr-sync-import.json",
                        self.work / "imp-a.jsonl", "imp-a", "ocr-sync")
        hits_a_file = self.work / "hits-a.json"
        hits_a_file.write_text(c.canonical({
            "index_version": "pages-v20261018-1",
            "items": [{
                "doc_id": doc_id,
                "ocr": {"source": "usnm-tesseract", "engine": "tesseract/5.5.1 deu_latf+deu"},
            }],
        }) + "\n")
        c.ingest_search_provenance(
            self.db, hits_a_file, self.work / "search-a.jsonl", "search-a", "public-hits",
            page_id, "2026-10-18T03:00:00Z", run_id=run_a, release_id="v20261018-1", import_id=import_a)
        # Missing artifact and worker execution still keep publication_proof_complete false.
        self.assertTrue(c.trace(self.db, page_id)["accepted"])
        self.assertTrue(c.trace(self.db, page_id)["published"])
        self.assertFalse(c.trace(self.db, page_id)["publication_proof_complete"])

        worker_fixture = ROOT / "scripts/corpus/fixtures/data_catalog/worker-success.json"
        worker_sha = json.loads(worker_fixture.read_text())["run_record"]["image_sha256"]
        c.ingest_worker(self.db, worker_fixture,
                        self.work / "worker-a.jsonl", "worker-a", "worker-capture")
        # Still missing verified/present scan artifact.
        self.assertFalse(c.trace(self.db, page_id)["publication_proof_complete"])

        # Mismatched scan checksum (HASH != RunRecord.image_sha256) cannot satisfy publication_proof_complete.
        self.ingest(event("scan-mismatch", "artifact", when="2026-10-16T19:59:00Z",
                          page_id=page_id, source="scan-inventory",
                          payload={"artifact_id": "scan", "sha256": HASH, "version": "v-mismatch",
                                   "location": "cached-scans-buffer",
                                   "reference": "buffer/nbu_gage/sn83045462/1890-05-03/1/7.jpg",
                                   "state": "verified", "observed_sha256": HASH}))
        self.assertEqual(c.trace(self.db, page_id)["artifact"]["state"], "verified")
        self.assertFalse(c.trace(self.db, page_id)["publication_proof_complete"])

        # Verified scan artifact matching RunRecord.image_sha256 completes the proof chain.
        self.ingest(event("scan-a", "artifact", when="2026-10-16T20:00:00Z",
                          page_id=page_id, source="scan-inventory",
                          payload={"artifact_id": "scan", "sha256": worker_sha, "version": "v1",
                                   "location": "cached-scans",
                                   "reference": "keep/nbu_gage/sn83045462/1890-05-03/1/7.jpg",
                                   "state": "verified", "observed_sha256": worker_sha}))
        complete = c.trace(self.db, page_id)
        self.assertTrue(complete["accepted"])
        self.assertTrue(complete["published"])
        self.assertTrue(complete["publication_proof_complete"])

        cohort = [{"page_id": page_id, "run_id": run_a}]
        before_report = self.db.read_bytes()
        report = c.acceptance_report(self.db, cohort, "pages-v20261018-1",
                                     "2026-10-18T03:01:00Z", 60)
        self.assertEqual(report["delivery_verified"], 1)
        self.assertEqual(report["quality_unknown"], 1)
        self.assertEqual(report["verified_better_published_searchable"], 0)
        self.assertEqual(report["pages"][0]["search_verified_at"], "2026-10-18T03:00:00.000000000Z")
        self.assertEqual(before_report, self.db.read_bytes())
        for cutoff, fresh in [
            ("2026-10-18T03:00:59.999999999Z", True),
            ("2026-10-18T03:01:00.000000000Z", True),
            ("2026-10-18T03:01:00.000000001Z", False),
            ("2026-10-18T04:01:00.000000001+01:00", False),
        ]:
            with self.subTest(freshness_cutoff=cutoff):
                boundary = c.acceptance_report(self.db, cohort, "pages-v20261018-1", cutoff, 60)
                self.assertEqual(boundary["delivery_verified"], int(fresh))
                self.assertEqual("stale_search_verification" in
                                 boundary["pages"][0]["delivery_blockers"], not fresh)
        originals = {(item["source"], item["event_id"]): item
                     for path in self.work.glob("*.jsonl")
                     for line in path.read_text().splitlines()
                     if (item := json.loads(line)).get("schema_version") == 1}
        proof_items = [
            next(item for item in complete["artifact"]["items"]
                 if item["data"]["sha256"] == worker_sha),
            complete["execution"]["executions"][0],
            complete["execution"]["runs"][0],
            complete["imported"]["items"][-1],
            complete["release_decision"]["items"][-1],
            complete["search_provenance"]["items"][-1],
        ]
        domain_paths = [
            ("fetched_at",), ("worker_metadata", "finished_at"),
            ("run_record", "node_finished_at"), ("committed_at",),
            ("decided_at",), ("published_at",),
        ]
        for item, domain_path in zip(proof_items, domain_paths):
            for change in ("observed_at", "domain_timestamp"):
                with self.subTest(kind=item["kind"], stage=item["data"].get("stage"), change=change):
                    copied_db = self.work / "cutoff.sqlite"
                    copied_db.write_bytes(before_report)
                    updated = copy.deepcopy(originals[(item["source"], item["event_id"])])
                    updated["event_id"] = "zz-cutoff-" + item["event_id"]
                    future_time = "2026-10-18T03:01:00.000000001Z"
                    if change == "observed_at":
                        updated["observed_at"] = future_time
                    else:
                        target = updated["payload"]
                        for key in domain_path[:-1]:
                            target = target[key]
                        target[domain_path[-1]] = future_time
                    c.ingest(copied_db, self.batch([updated]))
                    self.assertTrue(c.trace(copied_db, page_id, run_a)["publication_proof_complete"])
                    bounded = c.acceptance_report(copied_db, cohort, "pages-v20261018-1",
                                                  "2026-10-18T03:01:00Z", 60)
                    self.assertEqual(bounded["delivery_verified"], 0)
                    self.assertFalse(bounded["pages"][0]["publication_proof_complete"])
                    self.assertIn("evidence_after_cutoff", bounded["pages"][0]["delivery_blockers"])
        wrong_index = c.acceptance_report(self.db, cohort, "pages-new",
                                          "2026-10-18T03:01:00Z", 60)
        self.assertEqual(wrong_index["delivery_verified"], 0)
        self.assertIn("different_or_unknown_index", wrong_index["pages"][0]["delivery_blockers"])
        stale = c.acceptance_report(self.db, cohort, "pages-v20261018-1",
                                   "2026-10-18T03:01:01Z", 60)
        self.assertIn("stale_search_verification", stale["pages"][0]["delivery_blockers"])
        future = c.acceptance_report(self.db, cohort, "pages-v20261018-1",
                                    "2026-10-18T02:59:59Z", 60)
        self.assertIn("evidence_after_cutoff", future["pages"][0]["delivery_blockers"])

        # Mismatch 1: search receipt from a different index/release leaves accepted=true & published=true,
        # but publication_proof_complete=false.
        hits_wrong_index = self.work / "hits-wrong-index.json"
        hits_wrong_index.write_text(c.canonical({
            "index_version": "pages-v20261018-9",
            "items": [{
                "doc_id": doc_id,
                "ocr": {"source": "usnm-tesseract", "engine": "tesseract/5.5.1 deu_latf+deu"},
            }],
        }) + "\n")
        c.ingest_search_provenance(
            self.db, hits_wrong_index, self.work / "search-wrong-idx.jsonl", "search-wrong-idx",
            "public-hits", page_id, "2026-10-18T04:00:00Z",
            run_id=run_a, release_id="v20261018-1", import_id=import_a)
        mismatch_idx = c.trace(self.db, page_id)
        self.assertTrue(mismatch_idx["accepted"])
        self.assertTrue(mismatch_idx["published"])
        self.assertFalse(mismatch_idx["publication_proof_complete"])

        # Mismatch 2: search receipt for a different OCR engine/source on the expected index version.
        hits_wrong_engine = self.work / "hits-wrong-engine.json"
        hits_wrong_engine.write_text(c.canonical({
            "index_version": "pages-v20261018-1",
            "items": [{
                "doc_id": doc_id,
                "ocr": {"source": "usnm-tesseract", "engine": "kraken/4.3.13 deu"},
            }],
        }) + "\n")
        c.ingest_search_provenance(
            self.db, hits_wrong_engine, self.work / "search-wrong-eng.jsonl", "search-wrong-eng",
            "public-hits", page_id, "2026-10-18T05:00:00Z",
            run_id=run_a, release_id="v20261018-1", import_id=import_a)
        mismatch_eng = c.trace(self.db, page_id)
        self.assertTrue(mismatch_eng["accepted"])
        self.assertTrue(mismatch_eng["published"])
        self.assertFalse(mismatch_eng["publication_proof_complete"])

        # Mismatch 3: release and search both claim Kraken for run A, while the selected RunRecord
        # for run A is Tesseract -> accepted=true & published=true, but publication_proof_complete=false.
        release_kraken = self.work / "release-kraken.json"
        release_kraken.write_text(c.canonical({
            "schema": 1,
            "release_id": "v20261018-1",
            "decided_at": "2026-10-18T05:30:00.000000000Z",
            "decisions": [{
                "doc_id": doc_id,
                "run_id": run_a,
                "import_id": import_a,
                "committed_at": committed_a,
                "decision": "accepted",
                "decision_reason": "rule_v1",
                "selected_source": "usnm-kraken",
                "selected_engine": "kraken/4.3.13 deu",
            }],
        }) + "\n")
        c.ingest_release(self.db, release_kraken, self.work / "rel-kraken.jsonl", "rel-kraken", "release-builder")
        hits_kraken = self.work / "hits-kraken.json"
        hits_kraken.write_text(c.canonical({
            "index_version": "pages-v20261018-1",
            "items": [{
                "doc_id": doc_id,
                "ocr": {"source": "usnm-kraken", "engine": "kraken/4.3.13 deu"},
            }],
        }) + "\n")
        c.ingest_search_provenance(
            self.db, hits_kraken, self.work / "search-kraken.jsonl", "search-kraken",
            "public-hits", page_id, "2026-10-18T05:35:00Z",
            run_id=run_a, release_id="v20261018-1", import_id=import_a)
        mismatch_run_eng = c.trace(self.db, page_id)
        self.assertTrue(mismatch_run_eng["accepted"])
        self.assertTrue(mismatch_run_eng["published"])
        self.assertFalse(mismatch_run_eng["publication_proof_complete"])

        # Baseline text observation supersedes replacement in search and sets published & proof completeness false.
        hits_baseline = self.work / "hits-baseline.json"
        hits_baseline.write_text(c.canonical({
            "index_version": "pages-v20261018-1",
            "items": [{"doc_id": doc_id}],
        }) + "\n")
        c.ingest_search_provenance(
            self.db, hits_baseline, self.work / "search-base.jsonl", "search-base",
            "public-hits", page_id, "2026-10-18T06:00:00Z")
        after_base = c.trace(self.db, page_id)
        self.assertTrue(after_base["accepted"])
        self.assertFalse(after_base["published"])
        self.assertEqual(after_base["search_provenance"]["decision"], "unchanged_baseline")
        self.assertFalse(after_base["publication_proof_complete"])

        # Hyphenated engine version regression (e.g. tesseract-5.5.1-rc1/deu_latf+deu):
        # legacy ocr_engine tags preserve the hyphenated version instead of splitting at the last hyphen.
        self.assertEqual(
            c.canonical_engine_key("tesseract-5.5.1-rc1/deu_latf+deu", family="tesseract"),
            "tesseract/5.5.1-rc1 deu_latf+deu",
        )
        self.assertEqual(
            c.canonical_engine_key("tesseract-5.5.1-rc1/deu_latf+deu"),
            "tesseract/5.5.1-rc1 deu_latf+deu",
        )
        run_rc = "usnm-ocr-node3.20261018T070000000000Z"
        worker_rc_obj = json.loads(worker_fixture.read_text())
        worker_rc_obj["run_id"] = run_rc
        worker_rc_obj["started_at"] = "2026-10-18T07:00:00.000000000Z"
        worker_rc_obj["finished_at"] = "2026-10-18T07:00:05.000000000Z"
        worker_rc_obj["observations"][0]["ocr_engine"] = "tesseract-5.5.1-rc1/deu_latf+deu"
        worker_rc_obj["run_record"]["run_id"] = run_rc
        worker_rc_obj["run_record"]["ocr_engine"] = "tesseract-5.5.1-rc1/deu_latf+deu"
        worker_rc_obj["run_record"]["engine_version"] = "5.5.1-rc1"
        worker_rc_obj["run_record"]["node_finished_at"] = "2026-10-18T07:00:05.000000000Z"
        worker_rc_file = self.work / "worker-rc1.json"
        worker_rc_file.write_text(c.canonical(worker_rc_obj) + "\n")
        c.ingest_worker(self.db, worker_rc_file, self.work / "worker-rc1.jsonl",
                        "worker-rc1", "worker-capture")

        import_rc = "20261018T070500000000Z-rc1-0000"
        import_rc_file = self.work / "import-rc1.json"
        import_rc_file.write_text(c.canonical({
            "run": import_rc,
            "at": "2026-10-18T07:05:00.000000000Z",
            "index_version": "pages-v20261018-rc1",
            "sources": [
                {"path": f"pages/{issue_id}.{run_rc}.parquet", "etag": "\"rc1\"", "outcome": "imported"},
                {"path": f"alto/{doc_id}.{run_rc}.xml", "etag": "\"rc2\"", "outcome": "imported"},
            ],
            "pages": [f"ocr-local/pages/sn83045462/{import_rc}.parquet"],
            "outcomes": f"ocr-local/outcomes/2026-10-18/{import_rc}.parquet",
        }) + "\n")
        c.ingest_import(self.db, import_rc_file, self.work / "imp-rc1.jsonl", "imp-rc1", "ocr-sync")

        release_rc_file = self.work / "release-rc1.json"
        release_rc_file.write_text(c.canonical({
            "schema": 1,
            "release_id": "v20261018-rc1",
            "decided_at": "2026-10-18T07:10:00.000000000Z",
            "decisions": [{
                "doc_id": doc_id,
                "run_id": run_rc,
                "import_id": import_rc,
                "committed_at": "2026-10-18T07:05:00.000000000Z",
                "decision": "accepted",
                "decision_reason": "rule_v1",
                "selected_source": "usnm-tesseract",
                "selected_engine": "tesseract-5.5.1-rc1/deu_latf+deu",
            }],
        }) + "\n")
        c.ingest_release(self.db, release_rc_file, self.work / "rel-rc1.jsonl",
                         "rel-rc1", "release-builder")

        hits_rc_file = self.work / "hits-rc1.json"
        hits_rc_file.write_text(c.canonical({
            "index_version": "pages-v20261018-rc1",
            "items": [{
                "doc_id": doc_id,
                "ocr": {"source": "usnm-tesseract", "engine": "tesseract-5.5.1-rc1/deu_latf+deu"},
            }],
        }) + "\n")
        c.ingest_search_provenance(
            self.db, hits_rc_file, self.work / "search-rc1.jsonl", "search-rc1",
            "public-hits", page_id, "2026-10-18T07:15:00Z",
            run_id=run_rc, release_id="v20261018-rc1", import_id=import_rc)
        trace_rc = c.trace(self.db, page_id, run_id=run_rc)
        self.assertTrue(trace_rc["accepted"])
        self.assertTrue(trace_rc["published"])
        self.assertTrue(trace_rc["publication_proof_complete"])


if __name__ == "__main__":
    unittest.main()
