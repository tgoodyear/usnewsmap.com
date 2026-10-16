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
import urllib.error
import urllib.parse
import urllib.request
import uuid

from scripts import data_catalog as c


ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "scripts/data_catalog.py"
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
        self.cli("ingest-cached-scans", str(index), "--scan-root", str(scan_root),
                 "--evidence-out", str(evidence), "--batch-id", "cached-1",
                 "--source", "pve-big-stat")
        item = c.query(self.db, "artifact", {})["items"][0]
        self.assertEqual(item["page_id"], "loc:" + doc)
        self.assertEqual(item["data"]["state"], "present")
        self.assertEqual(item["data"]["fetched_at"], fetched)
        self.assertEqual(c.snapshot(self.db)["artifacts"]["verified"], 0)
        self.assertNotIn("https://", evidence.read_text())
        self.assertNotIn("loc_url", evidence.read_text())
        self.assertEqual(c.timestamp(fetched), "2026-10-11T22:32:31.635966Z")
        scan.unlink()
        c.ingest_cached_scans(self.db, index, scan_root,
                              self.work / "cached-missing.jsonl", "cached-2", "pve-big-stat")
        self.assertEqual(c.query(self.db, "artifact", {})["items"][0]["data"]["state"], "missing")
        # Wrong-size files are not considered present either.
        scan.write_bytes(b"x")
        c.ingest_cached_scans(self.db, index, scan_root,
                              self.work / "cached-size.jsonl", "cached-3", "pve-big-stat")
        self.assertEqual(c.query(self.db, "artifact", {})["items"][0]["data"]["state"], "missing")
        with self.assertRaises(c.CatalogError):
            c.ingest_cached_scans(self.db, index, scan_root, evidence, "cached-4", "pve-big-stat")
        self.assertEqual(c.ingest(self.db, evidence)["inserted"], 0)
        # Untrusted cache coordinates cannot traverse outside the provided root.
        row["batch"] = "../escape"
        index.write_text(c.canonical(row) + "\n")
        with self.assertRaises(c.CatalogError):
            c.ingest_cached_scans(self.db, index, scan_root,
                                  self.work / "bad.jsonl", "bad", "pve-big-stat")
        self.assertFalse((self.work / "bad.jsonl").exists())

    def test_cli_end_to_end_and_snapshot_freshness(self):
        batch = self.batch([event(), event("r1", "run"), event("a1", "page_state"),
                            event("s1", "stage", T1)])
        self.assertEqual(self.cli("ingest", str(batch))["inserted"], 4)
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
                      image_ref="https://registry.invalid/image?sig=never-store-SAS",
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
                             ("reference", "https://blob/scan?sig=secret"),
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
        for raw in ('{"a":1,"a":2}', '{"a":NaN}', '{"a":Infinity}'):
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
        for out in (self.db, Path(str(self.db) + "-wal"), Path(str(self.db) + "-shm")):
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


if __name__ == "__main__":
    unittest.main()
