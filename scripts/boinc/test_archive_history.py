"""Tests for archive_history.py (#1483), with synthetic stores only: a container in memory, a
controller state directory, a bridge tree and a SQLite stand-in for the BOINC database."""

import base64
import contextlib
import email.utils
import fcntl
import hashlib
import importlib.util
import io
import json
import sqlite3
import tempfile
import threading
import time
import unittest
from pathlib import Path
from unittest import mock


def load(name):
    spec = importlib.util.spec_from_file_location(name, Path(__file__).with_name(f"{name}.py"))
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


archive = load("archive_history")
scheduler = load("batch_scheduler")
IMAGE = "sha256:" + "a" * 64
DAY = 86400


def page_key(number):
    return f"sn12345678/1900-01-01/ed-1/seq-{number}"


def job_id(number):
    return f"{number:064x}"


def config():
    return {
        "schema_version": 1, "env": "dev", "account": "exchange123", "plan": "batch-test",
        "manifest": "v1/manifest.parquet", "image": IMAGE,
        "push_command": ["/opt/usnm/scripts/ocr-node/push-boinc-scan.sh"],
        "scans_root": "/tank/ocr/scans", "manifests_root": "/tank/ocr/manifests",
        "batch_size": 2, "max_pending": 4, "pages_per_host": 2,
        "source": "queue", "claim_queues": ["ocr-bulk"], "lease_hours": 96,
    }


def journal(number, page, stage, *, plan="batch-test", parent=None):
    job = {"id": job_id(number), "message": {"page": page, "batch": "test_batch", "language": "deu",
                                            "reason": "bulk"}, "engine": {"image": IMAGE}}
    record = {"stage": stage, "plan": plan, "stamp": f"20261020T0000{number:02d}000000Z", "job": job}
    if parent is not None:
        job.update(v=2, retry_of=job_id(parent), attempt=1)
        record["retry"] = {"previous_job": job_id(parent), "previous_attempt": 0, "app_version_num": 3}
    return record


def claim(number):
    page = page_key(number)
    return {"page": page, "doc_id": page.replace("/", "_"), "batch": "test_batch",
            "scan_base": f"keep/test_batch/sn12345678/1900-01-01/1/{number}",
            "scan": f"/tank/ocr/scans/keep/test_batch/sn12345678/1900-01-01/1/{number}.jp2",
            "message": f"{number:064x}", "pushed_to": "container"}


class FakeContainer:
    """The hand-off container: blobs with ETags, Content-MD5 and Last-Modified, and the lease."""

    def __init__(self):
        self.blobs = {}
        self.lease = None
        self.lease_blob_state = "available"
        self.fail_deletes_after = None
        self.deletes = 0
        self.acquired = 0
        self.md5 = True
        self.lock = threading.Lock()

    def put(self, name, data, modified):
        etag = '"0x' + hashlib.sha256(data + name.encode()).hexdigest()[:16].upper() + '"'
        self.blobs[name] = {"data": data, "etag": etag, "modified": email.utils.formatdate(modified, usegmt=True)}

    def list(self, prefix):
        with self.lock:
            return [{"name": name, "etag": blob["etag"], "size": len(blob["data"]),
                     "md5": base64.b64encode(hashlib.md5(blob["data"]).digest()).decode() if self.md5 else None,
                     "modified": blob["modified"]}
                    for name, blob in sorted(self.blobs.items()) if name.startswith(prefix)]

    def get(self, name):
        with self.lock:
            blob = self.blobs[name]
            return blob["data"], blob["etag"]

    def lease_state(self, name):
        return self.lease_blob_state

    def acquire(self, name):
        archive.require(self.lease_blob_state in archive.FREE_LEASE_STATES, "leased")
        self.acquired += 1
        self.lease_blob_state, self.lease = "leased", "lease-id"
        return self.lease

    def release(self, name, lease):
        assert lease == self.lease
        self.lease_blob_state, self.lease = "available", None

    def delete(self, name, etag):
        with self.lock:
            if self.fail_deletes_after is not None and self.deletes >= self.fail_deletes_after:
                raise RuntimeError("hand-off storage DELETE failed: injected")
            if name not in self.blobs:
                return False
            archive.require(self.blobs[name]["etag"] == etag, f"{name} changed after it was archived")
            del self.blobs[name]
            self.deletes += 1
            return True

    def snapshot(self):
        return {name: (blob["etag"], blob["data"]) for name, blob in self.blobs.items()}, self.lease_blob_state


def tree(root):
    root = Path(root)
    if not root.exists():
        return {}
    return {str(path.relative_to(root)): (hashlib.sha256(path.read_bytes()).hexdigest() if path.is_file() else "dir")
            for path in sorted(root.rglob("*"))}


def run(argv):
    out, err = io.StringIO(), io.StringIO()
    with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
        code = archive.main(argv)
    lines = [json.loads(line) for line in out.getvalue().splitlines() if line.strip()]
    try:
        errors = [json.loads(line) for line in err.getvalue().splitlines() if line.strip()]
    except ValueError:
        raise AssertionError(err.getvalue())
    return code, (lines[-1] if lines else None), errors


class DispatchTests(unittest.TestCase):
    """The controller host's step: the controller state, then the hand-off blobs."""

    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        root = Path(self.tmp.name)
        self.config_path = root / "config.json"
        self.config_path.write_text(json.dumps(config()))
        self.state_dir = root / "state"
        self.state_dir.mkdir()
        self.archive_dir = root / "archive"
        now = time.time()
        self.container = FakeContainer()
        # Pages 1 to 7 are this plan's; page 20 is an older plan's, no longer claimed here.
        layout = [
            (1, [journal(1, page_key(1), "published")], now - 10 * DAY),
            (2, [journal(2, page_key(2), "published")], now - 9 * DAY),
            # A failed first attempt and its reviewed retry, which was published.
            (3, [journal(3, page_key(3), "failed"), journal(30, page_key(3), "published", parent=3)], now - 8 * DAY),
            (4, [journal(4, page_key(4), "submitted")], now - 12 * DAY),
            (5, [journal(5, page_key(5), "published")], now - 3600),
            (6, [journal(6, page_key(6), "reserved")], now - 11 * DAY),
            (20, [journal(20, page_key(20), "published", plan="older-plan")], now - 20 * DAY),
        ]
        for _, records, modified in layout:
            for record in records:
                job = record["job"]["id"]
                self.container.put(f"dispatch/jobs/{job}/record.json", json.dumps(record).encode(), modified)
                self.container.put(f"dispatch/jobs/{job}/request.json", b'{"synthetic": true}', modified)
                self.container.put(f"dispatch/jobs/{job}/page", b"synthetic page " + job.encode(), modified)
                if record["stage"] == "published":
                    self.container.put(f"dispatch/jobs/{job}/accepted/ledger.json", b"{}", modified)
                    self.container.put(f"spool/{job}/usnm_{job}__file_result.json", b"{}", modified)
        self.container.put(f"dispatch/quarantine/{job_id(3)}/reason.json", b'{"why": "synthetic"}', now - 8 * DAY)
        self.container.put("claims/sn12345678_1900-01-01_ed-1_seq-1.json", b'{"ended": "finished"}', now - 10 * DAY)
        self.container.put("scheduler/controller.lock", b'{"schema_version":1}\n', now - 30 * DAY)
        loaded = scheduler.load_config(self.config_path)
        self.state = {
            "config_sha256": scheduler.digest(loaded),
            "claims": {page_key(n): claim(n) for n in (1, 2, 3, 4, 5, 6, 7)},
            "selected": {page_key(n): int(now - 12 * DAY) for n in (1, 2, 3, 4, 5, 6, 7)},
            "held_since": {job_id(4): int(now - 3600)},
            "dead_lettered": {job_id(3): int(now - 9 * DAY)},
            "published": 4, "started_at": int(now - 30 * DAY),
        }
        (self.state_dir / "state.json").write_text(json.dumps(self.state, sort_keys=True) + "\n")
        self.units = {"timer": "inactive", "service": "inactive"}
        self.patches = [
            mock.patch.object(archive, "RestContainer", lambda account: self.container),
            mock.patch.object(archive, "unit_state", lambda unit: self.units[unit.rsplit(".", 1)[1]]),
        ]
        for patch in self.patches:
            patch.start()

    def tearDown(self):
        for patch in self.patches:
            patch.stop()
        self.tmp.cleanup()

    def argv(self, *extra, pages=10):
        return ["dispatch", "--config", str(self.config_path), "--state-dir", str(self.state_dir),
                "--archive-dir", str(self.archive_dir), "--pages", str(pages), *extra]

    def apply(self, *extra, pages=10):
        return run(self.argv("--apply", "--confirm-controller-paused", *extra, pages=pages))

    def snapshot(self):
        return self.container.snapshot(), tree(self.state_dir), tree(self.archive_dir)

    def state_now(self):
        return json.loads((self.state_dir / "state.json").read_text())

    def jobs_in_container(self):
        return {match[1] for match in map(archive.JOB_BLOB.fullmatch, self.container.blobs) if match}

    def test_dry_run_reads_only_and_picks_the_oldest_published_pages(self):
        before = self.snapshot()
        code, summary, _ = run(self.argv(pages=3))
        self.assertEqual(code, 0)
        self.assertEqual(before, self.snapshot())
        self.assertEqual(self.container.acquired, 0)
        self.assertFalse(self.archive_dir.exists())
        self.assertFalse(summary["apply"])
        self.assertEqual(summary["journals"], 8)
        self.assertEqual((summary["pages"]["eligible"], summary["pages"]["eligible_this_plan"]), (4, 3))
        self.assertEqual(summary["pages"]["skipped"], {
            "current_journal_reserved": 1, "current_journal_submitted": 1, "published_too_recently": 1})
        # Oldest published first: the older plan's page, then pages 1 and 2; page 3 waits.
        self.assertEqual(summary["batch"]["pages"], 3)
        self.assertEqual(summary["batch"]["this_plan_pages"], 2)
        self.assertEqual(summary["batch"]["other_plan_pages"], 1)
        self.assertEqual(summary["after_batch"]["journals"], 5)
        self.assertEqual(summary["after_batch"]["state_claims"], 5)
        self.assertEqual(summary["outstanding_pages"], {"before": 1, "after": 1, "unchanged": True, "error": None})
        self.assertTrue(summary["controller"]["paused"])

    def test_apply_refuses_unless_the_controller_is_paused_and_idle(self):
        before = self.snapshot()
        code, _, errors = run(self.argv("--apply"))
        self.assertEqual(code, 1)
        self.assertIn("--confirm-controller-paused", errors[-1]["error"])
        self.units["timer"] = "active"
        code, _, errors = self.apply()
        self.assertEqual(code, 1)
        self.assertIn("not paused", errors[-1]["error"])
        self.units.update(timer="inactive", service="activating")
        self.assertEqual(self.apply()[0], 1)
        self.units["service"] = "failed"
        self.container.lease_blob_state = "leased"
        code, _, errors = self.apply()
        self.assertEqual(code, 1)
        self.assertIn("lease is leased", errors[-1]["error"])
        self.container.lease_blob_state = "available"
        with open(self.state_dir / "controller.lock", "a") as held:
            fcntl.flock(held, fcntl.LOCK_EX | fcntl.LOCK_NB)
            before = self.snapshot()
            code, _, errors = self.apply()
        self.assertEqual(code, 1)
        self.assertIn("locked", errors[-1]["error"])
        self.assertEqual(before, self.snapshot())
        self.assertEqual(self.container.acquired, 0)

    def test_apply_archives_every_store_in_lockstep(self):
        originals = self.container.snapshot()[0]
        code, receipt, _ = self.apply()
        self.assertEqual(code, 0, receipt)
        archived = {job_id(n) for n in (1, 2, 3, 30, 20)}
        self.assertTrue(receipt["complete"])
        self.assertEqual((receipt["pages"], receipt["jobs"]), (4, 5))
        self.assertEqual((receipt["before"]["journals"], receipt["after"]["journals"]), (8, 3))
        # The hand-off: every blob of the archived jobs is gone, and held byte for byte.
        self.assertFalse(self.jobs_in_container() & archived)
        self.assertEqual(self.jobs_in_container(), {job_id(n) for n in (4, 5, 6)})
        batch = self.archive_dir / "batches/0001"
        copies = {name: data for name, (_, data) in originals.items()
                  if archive.JOB_BLOB.fullmatch(name) and archive.JOB_BLOB.fullmatch(name)[1] in archived}
        self.assertEqual(len(copies), receipt["handoff"]["blobs"])
        for name, data in copies.items():
            self.assertEqual((batch / "handoff" / name).read_bytes(), data)
        # Claim files and the lease blob are not job history; they stay.
        self.assertIn("claims/sn12345678_1900-01-01_ed-1_seq-1.json", self.container.blobs)
        self.assertEqual(self.container.lease_blob_state, "available")
        # The controller: the pages' claims and selections, and the jobs' entries, moved out.
        state = self.state_now()
        self.assertEqual(sorted(state["claims"]), [page_key(n) for n in (4, 5, 6, 7)])
        self.assertEqual(sorted(state["selected"]), [page_key(n) for n in (4, 5, 6, 7)])
        self.assertEqual(state["dead_lettered"], {})
        self.assertEqual(state["held_since"], self.state["held_since"])
        self.assertEqual(state["published"], 1)
        saved = json.loads((batch / "controller/state-entries.json").read_text())
        self.assertEqual(saved["claims"], {page_key(n): claim(n) for n in (1, 2, 3)})
        self.assertEqual(saved["dead_lettered"], self.state["dead_lettered"])
        # What a tick would now see: owned_records accepts what is left, and the only page that
        # needs work is the one that needed it before.
        left = [json.loads(blob["data"]) for name, blob in self.container.blobs.items()
                if name.endswith("/record.json")]
        loaded = {**scheduler.load_config(self.config_path), "pages": archive.claimed_pages(state)}
        owned = scheduler.owned_records(loaded, left)
        self.assertEqual(sorted(owned), [page_key(n) for n in (4, 5, 6)])
        self.assertEqual(archive.outstanding(scheduler, loaded, state, left), {page_key(7)})
        # Nothing else is old and published enough: no second batch is made.
        code, summary, _ = self.apply()
        self.assertEqual(code, 0)
        self.assertIsNone(summary["batch"])
        self.assertEqual(archive.batch_dirs(self.archive_dir), [batch])

    def test_a_batch_stopped_half_way_resumes_where_it_stopped(self):
        originals = self.container.snapshot()[0]
        self.container.fail_deletes_after = 4
        code, _, errors = self.apply()
        self.assertEqual(code, 1)
        self.assertIn("injected", errors[-1]["error"])
        self.assertEqual(self.container.lease_blob_state, "available")
        batch = self.archive_dir / "batches/0001"
        self.assertFalse((batch / "receipts/dispatch.json").exists())
        # The controller step went first and is complete; the hand-off is part done.
        self.assertNotIn(page_key(1), self.state_now()["claims"])
        self.assertEqual(self.container.deletes, 4)
        code, summary, _ = run(self.argv())
        self.assertEqual(code, 0)
        self.assertEqual(summary["resume"], {"batch": "0001", "pages": 4, "jobs": 5})
        self.container.fail_deletes_after = None
        code, receipt, _ = self.apply()
        self.assertEqual(code, 0)
        self.assertEqual(receipt["batch"], "0001")
        self.assertTrue(receipt["controller"]["resumed"])
        self.assertEqual(self.jobs_in_container(), {job_id(n) for n in (4, 5, 6)})
        archived = {job_id(n) for n in (1, 2, 3, 30, 20)}
        for name, (_, data) in originals.items():
            if archive.JOB_BLOB.fullmatch(name) and archive.JOB_BLOB.fullmatch(name)[1] in archived:
                self.assertEqual((batch / "handoff" / name).read_bytes(), data)
        self.assertEqual(archive.batch_dirs(self.archive_dir), [batch])

    def test_a_resume_checks_archived_copies_by_their_sha256(self):
        self.container.md5 = False
        originals = self.container.snapshot()[0]
        self.container.fail_deletes_after = 4
        self.assertEqual(self.apply()[0], 1)
        handoff = self.archive_dir / "batches/0001/handoff"
        copied = [name for name in sorted(originals) if (handoff / name).is_file()]
        live = [name for name in copied if name in self.container.blobs]
        gone = [name for name in copied if name not in self.container.blobs]
        self.assertTrue(live and gone)

        def swap(name):
            # Same size, other bytes: without Content-MD5 only the sha256 tells them apart.
            data = originals[name][1]
            (handoff / name).write_bytes(bytes([data[0] ^ 1]) + data[1:])

        swap(gone[0])
        swap(live[0])
        self.container.fail_deletes_after = None
        code, _, errors = self.apply()
        self.assertEqual(code, 1)
        self.assertIn("archived copy is missing or wrong", errors[-1]["error"])
        (handoff / gone[0]).write_bytes(originals[gone[0]][1])
        code, receipt, _ = self.apply()
        self.assertEqual(code, 0, receipt)
        # The live blob was copied again rather than deleted against the wrong copy.
        for name in copied:
            self.assertEqual((handoff / name).read_bytes(), originals[name][1])

    def test_a_blob_changed_since_the_plan_stops_the_resume(self):
        self.container.fail_deletes_after = 1
        self.assertEqual(self.apply()[0], 1)
        name = next(name for name in sorted(self.container.blobs) if name.endswith("/record.json")
                    and job_id(20) in name)
        self.container.put(name, b'{"changed": true}', time.time())
        self.container.fail_deletes_after = None
        left = set(self.container.blobs)
        code, _, errors = self.apply()
        self.assertEqual(code, 1)
        self.assertIn("changed since the batch was planned", errors[-1]["error"])
        self.assertEqual(set(self.container.blobs), left)

    def test_a_fixed_page_list_is_never_archived(self):
        fixed = config()
        for key in ("source", "claim_queues", "lease_hours"):
            del fixed[key]
        fixed["batch"] = "test_batch"
        fixed["pages"] = [{"page": page_key(n), "scan": f"/tank/ocr/scans/keep/test_batch/{n}.jp2"} for n in range(1, 8)]
        self.config_path.write_text(json.dumps(fixed))
        self.state["config_sha256"] = scheduler.digest(scheduler.load_config(self.config_path))
        (self.state_dir / "state.json").write_text(json.dumps(self.state))
        code, summary, _ = run(self.argv())
        self.assertEqual(code, 0)
        self.assertEqual(summary["pages"]["skipped"]["fixed_page_allocation"], 3)
        self.assertEqual(summary["batch"]["this_plan_pages"], 0)
        self.assertEqual(summary["outstanding_pages"], {"before": 1, "after": 1, "unchanged": True, "error": None})
        # A configured page whose only journal is an older plan's stays: the tick refuses it
        # today, and archiving the journal would let the tick export it again.
        fixed["pages"].append({"page": page_key(20), "scan": "/tank/ocr/scans/keep/test_batch/20.jp2"})
        self.config_path.write_text(json.dumps(fixed))
        self.state["config_sha256"] = scheduler.digest(scheduler.load_config(self.config_path))
        (self.state_dir / "state.json").write_text(json.dumps(self.state))
        code, summary, _ = run(self.argv())
        self.assertEqual(code, 0)
        self.assertEqual(summary["pages"]["skipped"]["allocated_to_current_plan"], 1)
        self.assertEqual(summary["batch"]["pages"], 0)
        self.assertIn("another plan", summary["outstanding_pages"]["error"])
        before = self.snapshot()
        code, _, errors = self.apply()
        self.assertEqual(code, 1)
        self.assertIn("don't agree already", errors[-1]["error"])
        self.assertEqual(before[0], self.snapshot()[0])


class RestContainerTests(unittest.TestCase):
    def test_a_listing_and_a_read_name_the_same_etag(self):
        # A listing gives an ETag bare and a read's header quotes it; If-Match takes the quoted form.
        container = archive.RestContainer("exchange123")
        listing = (b"<EnumerationResults><Blobs><Blob><Name>dispatch/jobs/x/record.json</Name><Properties>"
                   b"<Etag>0x8DD1</Etag><Content-Length>2</Content-Length></Properties></Blob></Blobs>"
                   b"</EnumerationResults>")
        calls = []

        def request(method, name="", query=None, headers=None, absent=(404,)):
            calls.append((method, headers))
            if method == "DELETE":
                return 202, {}, b""
            return 200, {"ETag": '"0x8DD1"'}, listing if query else b"{}"

        container.request = request
        [entry] = container.list("dispatch/jobs/")
        self.assertEqual(container.get(entry["name"])[1], entry["etag"])
        self.assertTrue(container.delete(entry["name"], entry["etag"]))
        self.assertEqual(calls[-1], ("DELETE", {"If-Match": '"0x8DD1"'}))


def write_batch(batch, jobs):
    manifest = {"schema_version": 1, "kind": "usnm-boinc-archive-batch", "jobs": sorted(jobs)}
    archive.write_json(batch / "manifest.json", manifest)
    archive.write_json(batch / "receipts/dispatch.json", {
        "step": "dispatch", "complete": True, "manifest_sha256": archive.sha256_file(batch / "manifest.json")})


class BridgeTests(unittest.TestCase):
    """The BOINC host's journals and spool outputs."""

    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        root = Path(self.tmp.name)
        self.handoff, self.results = root / "project/handoff", root / "project/usnmocr/results"
        self.batch = root / "archive/0001"
        (self.handoff / "jobs").mkdir(parents=True)
        (self.results / "0").mkdir(parents=True)
        self.locks = [root / "step.lock"]
        bundle_x, bundle_y = "b" * 64, "c" * 64
        # Jobs 1 and 2 share bundle X and are both archived; job 3 is in bundle Y with job 4,
        # which stays; job 5 ran alone; job 6 isn't in the batch.
        layout = {1: bundle_x, 2: bundle_x, 3: bundle_y, 4: bundle_y, 5: None, 6: None}
        for number, bundle in layout.items():
            job = job_id(number)
            record = journal(number, page_key(number), "submitted")
            if bundle:
                record["bundle"] = bundle
            directory = self.handoff / "jobs" / job
            directory.mkdir()
            (directory / "record.json").write_text(json.dumps(record))
            (directory / "request.json").write_text("{}")
            (directory / "page").write_bytes(b"synthetic page")
        for bundle in (bundle_x, bundle_y):
            for name in ("__file_results.json", "__file_texts", "__file_layouts"):
                (self.results / "0" / f"usnm_ocrb_{bundle}{name}").write_text(name)
        for number in (5, 6):
            for name in ("__file_result.json", "__file_text.txt", "__file_layout.xml"):
                (self.results / "0" / f"usnm_{job_id(number)}{name}").write_text(name)
        (self.results / "0" / "usnm_quality_other").write_text("another app's result")
        self.bundle_x, self.bundle_y = bundle_x, bundle_y
        write_batch(self.batch, [job_id(n) for n in (1, 2, 3, 5)])

    def tearDown(self):
        self.tmp.cleanup()

    def argv(self, *extra):
        return ["bridge", "--batch-dir", str(self.batch), "--handoff-dir", str(self.handoff),
                "--results", str(self.results), "--lock-file", str(self.locks[0]), *extra]

    def snapshot(self):
        return tree(self.handoff), tree(self.results), tree(self.batch)

    def test_dry_run_reads_only(self):
        before = self.snapshot()
        code, summary, _ = run(self.argv())
        self.assertEqual(code, 0)
        self.assertEqual(before, self.snapshot())
        self.assertEqual((summary["jobs"], summary["journal_files"], summary["spool_files"]), (4, 12, 6))
        self.assertEqual((summary["drained_bundles"], summary["kept_bundles"]), (1, 1))
        self.assertEqual(summary["before"]["server_jobs"], 6)

    def test_apply_moves_journals_and_spool_and_resumes_after_a_crash(self):
        originals = {**{("jobs", k): v for k, v in tree(self.handoff / "jobs").items()},
                     **{("spool", k): v for k, v in tree(self.results).items()}}
        self.assertEqual(run(self.argv("--apply"))[0], 1)  # no --confirm-controller-paused
        real_unlink, calls = Path.unlink, []

        def failing_unlink(path, *args, **kwargs):
            calls.append(path)
            if len(calls) == 5:
                raise OSError("injected")
            return real_unlink(path, *args, **kwargs)

        with mock.patch.object(Path, "unlink", failing_unlink):
            code, _, _ = run(self.argv("--apply", "--confirm-controller-paused"))
        self.assertEqual(code, 1)
        self.assertTrue((self.batch / "bridge-plan.json").exists())
        self.assertFalse((self.batch / "receipts/bridge.json").exists())
        code, receipt, _ = run(self.argv("--apply", "--confirm-controller-paused"))
        self.assertEqual(code, 0, receipt)
        self.assertEqual(sorted(path.name for path in (self.handoff / "jobs").iterdir()), [job_id(4), job_id(6)])
        left = sorted(path.name for path in (self.results / "0").iterdir())
        self.assertEqual(left, sorted([f"usnm_ocrb_{self.bundle_y}{name}" for name in
                                       ("__file_results.json", "__file_texts", "__file_layouts")]
                                      + [f"usnm_{job_id(6)}{name}" for name in
                                         ("__file_result.json", "__file_text.txt", "__file_layout.xml")]
                                      + ["usnm_quality_other"]))
        for (kind, path), digest in originals.items():
            source = self.batch / ("bridge/jobs" if kind == "jobs" else "spool") / path
            if source.exists() and source.is_file():
                self.assertEqual(hashlib.sha256(source.read_bytes()).hexdigest(), digest)
        receipt_file = json.loads((self.batch / "receipts/bridge.json").read_text())
        self.assertEqual(receipt_file["workunits"], sorted(
            [f"usnm_{job_id(n)}" for n in (1, 2, 3, 5)] + [f"usnm_ocrb_{self.bundle_x}"]))
        self.assertEqual((receipt_file["before"]["server_jobs"], receipt_file["after"]["server_jobs"]), (6, 2))
        self.assertEqual((receipt_file["journals"]["files"], receipt_file["spool"]["files"]), (12, 6))

    def test_a_job_the_server_has_not_finished_stops_the_step(self):
        path = self.handoff / "jobs" / job_id(5) / "record.json"
        path.write_text(json.dumps(journal(5, page_key(5), "submitting")))
        before = self.snapshot()
        code, _, errors = run(self.argv("--apply", "--confirm-controller-paused"))
        self.assertEqual(code, 1)
        self.assertIn("not drained", errors[-1]["error"])
        after = self.snapshot()
        after[0].pop(".lock")  # the bridge's own lock file, taken as its steps take it
        self.assertEqual(before[:2], after[:2])

    def test_a_stale_partial_link_never_writes_through_to_the_source(self):
        source = self.handoff / "jobs" / job_id(1) / "page"
        target = self.batch / "bridge/jobs" / job_id(1) / "page"
        target.parent.mkdir(parents=True)
        # What a crash between os.link and os.replace leaves behind.
        target.with_name("page.partial").hardlink_to(source)
        archive.write_atomic(target, b"other bytes")
        self.assertEqual(source.read_bytes(), b"synthetic page")
        target.unlink()
        target.with_name("page.partial").hardlink_to(source)
        with mock.patch.object(archive.os, "link", side_effect=OSError("cross-device")):
            code, receipt, _ = run(self.argv("--apply", "--confirm-controller-paused"))
        self.assertEqual(code, 0, receipt)
        self.assertEqual(target.read_bytes(), b"synthetic page")

    def test_needs_the_controller_hosts_unchanged_manifest_and_receipt(self):
        (self.batch / "manifest.json").write_text('{"jobs": []}')
        code, _, errors = run(self.argv())
        self.assertEqual(code, 1)
        self.assertIn("dispatch step must be complete", errors[-1]["error"])


class FakeMysql:
    """The mysql client over SQLite, with BOINC's two tables as far as the step reads them."""

    db = None
    fail_before = None
    fail_after = None

    def __init__(self, args):
        pass

    def run(self, sql, *, header=False):
        cursor = FakeMysql.db.execute(sql)
        rows = cursor.fetchall()
        lines = ["\t".join(column[0] for column in cursor.description)] if header else []
        lines += ["\t".join("NULL" if value is None else str(value) for value in row) for row in rows]
        return "".join(line + "\n" for line in lines)

    def session(self):
        return FakeSession()


class FakeSession:
    """A session as the client gives it: a failed statement ends it, and ending a session
    without COMMIT rolls back."""

    def query(self, sql):
        if FakeMysql.fail_before and sql.startswith(FakeMysql.fail_before):
            FakeMysql.fail_before = None
            raise archive.Refused("a database statement failed; nothing after it ran")
        lite = sql.replace(" FOR UPDATE", "").replace("ROW_COUNT()", "changes()")
        for statement in lite.split("; "):
            cursor = FakeMysql.db.execute("BEGIN" if statement == "START TRANSACTION" else statement)
        rows = cursor.fetchall() if cursor.description else []
        if FakeMysql.fail_after and sql.startswith(FakeMysql.fail_after):
            FakeMysql.fail_after = None
            raise archive.Refused("the client stopped")
        if not rows:
            return [], []
        return ([column[0] for column in cursor.description],
                [["NULL" if value is None else str(value) for value in row] for row in rows])

    def close(self):
        if FakeMysql.db.in_transaction:
            FakeMysql.db.execute("ROLLBACK")


class DbTests(unittest.TestCase):
    """The BOINC database's rows, after the bridge step."""

    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.batch = Path(self.tmp.name) / "0001"
        db = sqlite3.connect(":memory:", isolation_level=None)
        db.create_function("REGEXP", 2, lambda pattern, value: __import__("re").search(pattern, value) is not None)
        db.executescript(
            "CREATE TABLE workunit (id INTEGER PRIMARY KEY, name TEXT, assimilate_state INT, file_delete_state INT);"
            "CREATE TABLE result (id INTEGER PRIMARY KEY, workunitid INT, server_state INT, file_delete_state INT,"
            " hostid INT);")
        rows = [
            (1, f"usnm_{job_id(1)}", 2, 2), (2, f"usnm_{job_id(2)}", 2, 1),   # 2: input files not deleted yet
            (3, f"usnm_ocrb_{'b' * 64}", 2, 2), (4, f"usnm_{job_id(9)}", 2, 2),  # 9 is not in the batch
            (6, f"usnm_{job_id(7)}", 2, 2),  # 7: a result's output files not deleted yet
        ]
        db.executemany("INSERT INTO workunit VALUES (?, ?, ?, ?)", rows)
        db.executemany("INSERT INTO result VALUES (?, ?, ?, ?, ?)", [
            (10, 1, 5, 2, 7), (11, 1, 5, 2, 8), (12, 2, 5, 2, 7), (13, 3, 5, 2, 7), (14, 4, 5, 2, 7),
            (15, 6, 5, 1, 7)])
        FakeMysql.db, FakeMysql.fail_before, FakeMysql.fail_after = db, None, None
        write_batch(self.batch, [job_id(n) for n in (1, 2, 5, 7)])
        archive.write_json(self.batch / "receipts/bridge.json", {
            "step": "bridge", "complete": True, "manifest_sha256": archive.sha256_file(self.batch / "manifest.json"),
            "workunits": [f"usnm_{job_id(1)}", f"usnm_{job_id(2)}", f"usnm_{job_id(5)}", f"usnm_{job_id(7)}",
                          f"usnm_ocrb_{'b' * 64}"]})
        self.patch = mock.patch.object(archive, "MysqlClient", FakeMysql)
        self.patch.start()

    def tearDown(self):
        self.patch.stop()
        FakeMysql.db.close()
        self.tmp.cleanup()

    def argv(self, *extra):
        return ["db", "--batch-dir", str(self.batch), "--mysql-defaults-file", "/run/fake.cnf",
                "--database", "usnmocr", *extra]

    def apply(self):
        return run(self.argv("--apply", "--confirm-controller-paused"))

    def rows(self):
        return (FakeMysql.db.execute("SELECT * FROM workunit ORDER BY id").fetchall(),
                FakeMysql.db.execute("SELECT * FROM result ORDER BY id").fetchall())

    def test_dry_run_reads_only(self):
        before = self.rows(), tree(self.batch)
        code, summary, _ = run(self.argv())
        self.assertEqual(code, 0)
        self.assertEqual(before, (self.rows(), tree(self.batch)))
        self.assertEqual((summary["present"], summary["absent"], summary["ready"], summary["deferred"]), (4, 1, 2, 2))
        self.assertEqual(summary["existing_workunits"], 5)

    def test_apply_exports_the_rows_before_deleting_them_and_resumes(self):
        # The second DELETE fails: the first is rolled back with it.
        FakeMysql.fail_before = "DELETE FROM workunit"
        before = self.rows()
        self.assertEqual(self.apply()[0], 1)
        self.assertEqual(before, self.rows())
        exported = (self.batch / "db/chunk-0001-attempt-1/result.tsv").read_text().splitlines()
        self.assertEqual(exported, ["id\tworkunitid\tserver_state\tfile_delete_state\thostid",
                                    "10\t1\t5\t2\t7", "11\t1\t5\t2\t8", "13\t3\t5\t2\t7"])
        code, receipt, _ = self.apply()
        self.assertEqual(code, 0, receipt)
        workunits, results = self.rows()
        # Unfinished file cleanup, of a workunit or of a result, stays for db_purge, as does
        # another job's workunit.
        self.assertEqual([row[0] for row in workunits], [2, 4, 6])
        self.assertEqual([row[0] for row in results], [12, 14, 15])
        self.assertEqual((receipt["deleted_workunits"], receipt["deferred"], receipt["absent"], receipt["vanished"]),
                         (2, 2, 1, 0))
        self.assertEqual((receipt["before"]["existing_workunits"], receipt["after"]["existing_workunits"]), (5, 3))
        # Both attempts' exports are kept; the second is what was deleted.
        self.assertEqual(sorted(receipt["exports"]), [f"chunk-0001-attempt-{n}/{table}.tsv"
                                                      for n in (1, 2) for table in ("result", "workunit")])
        self.assertEqual(archive.exported_ids(self.batch / "db/chunk-0001-attempt-2/workunit.tsv"), {1, 3})

    def test_a_stop_after_commit_resumes_from_its_export(self):
        FakeMysql.fail_after = "COMMIT"
        self.assertEqual(self.apply()[0], 1)
        self.assertEqual([row[0] for row in self.rows()[0]], [2, 4, 6])
        self.assertFalse((self.batch / "db/chunk-0001.json").exists())
        code, receipt, _ = self.apply()
        self.assertEqual(code, 0, receipt)
        self.assertEqual((receipt["deleted_workunits"], receipt["vanished"]), (2, 0))
        self.assertEqual(sorted(receipt["exports"]), ["chunk-0001-attempt-1/result.tsv",
                                                      "chunk-0001-attempt-1/workunit.tsv"])

    def test_rows_are_checked_again_under_their_lock(self):
        FakeMysql.fail_before = "START TRANSACTION"
        self.assertEqual(self.apply()[0], 1)
        self.assertTrue((self.batch / "db-plan.json").exists())
        # BOINC changed a planned row before the step resumed: a result is open again.
        FakeMysql.db.execute("UPDATE result SET server_state = 4 WHERE id = 10")
        code, receipt, _ = self.apply()
        self.assertEqual(code, 0, receipt)
        self.assertEqual([row[0] for row in self.rows()[0]], [1, 2, 4, 6])
        self.assertEqual((receipt["deleted_workunits"], receipt["deferred"]), (1, 3))

    def test_needs_this_batchs_bridge_step_first(self):
        receipt = json.loads((self.batch / "receipts/bridge.json").read_text())
        receipt["manifest_sha256"] = "0" * 64  # another batch's
        archive.write_json(self.batch / "receipts/bridge.json", receipt)
        before = self.rows()
        code, _, errors = self.apply()
        self.assertEqual(code, 1)
        self.assertIn("this batch's manifest", errors[-1]["error"])
        (self.batch / "receipts/bridge.json").unlink()
        code, _, errors = run(self.argv())
        self.assertEqual(code, 1)
        self.assertIn("bridge.json", errors[-1]["error"])
        self.assertEqual(before, self.rows())


if __name__ == "__main__":
    unittest.main()
