import contextlib
import copy
import importlib.util
import io
import json
import subprocess
import unittest
import uuid
from pathlib import Path
from unittest.mock import patch


SPEC = importlib.util.spec_from_file_location("batch_scheduler", Path(__file__).with_name("batch_scheduler.py"))
batch = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(batch)
NOW = 1700000000


def config():
    return {
        "schema_version": 1, "env": "dev", "account": "exchange123", "plan": "batch-test",
        "batch": "test_batch", "manifest": "v1/manifest.parquet", "image": "sha256:" + "a" * 64,
        "push_command": ["ssh", "-o", "BatchMode=yes", "root@pve.example", "pct", "exec", "211",
                         "--", "/opt/usnm/scripts/ocr-node/push-boinc-scan.sh"],
        "scans_root": "/tank/ocr/scans", "manifests_root": "/tank/ocr/manifests",
        "batch_size": 2, "max_pending": 4, "pages_per_host": 2,
        "pages": [{"page": f"sn12345678/1900-01-01/ed-1/seq-{number}",
                   "scan": f"/tank/ocr/scans/keep/test_batch/sn12345678/1900-01-01/ed-1/seq-{number}.jp2"}
                  for number in range(1, 6)],
    }


def record(page, number, stage="reserved"):
    return {"stage": stage, "plan": "batch-test",
            "job": {"id": f"{number:064x}", "message": {"page": page, "batch": "test_batch"},
                    "engine": {"image": "sha256:" + "a" * 64}}}


class Fake:
    def __init__(self):
        self.rows = []
        self.jobs = {}
        self.actions = []
        self.hosts = 2
        self.other = 0
        self.assimilated = set()
        self.extra_pending = set()
        self.extra_existing = set()
        self.crash_export = False
        self.crash_submit = False
        self.hold = False
        self.terminal = False

    def inventory(self):
        pending = [job_id for job_id, job in self.jobs.items()
                   if job["stage"] == "submitted" and not job["spool"] and job_id not in self.assimilated]
        pending = sorted(set(pending) | self.extra_pending)
        return {"schema_version": 1, "nonce": "a" * 32, "created_at": NOW,
                "active_hosts": self.hosts,
                "pending_workunits": self.other + len(pending), "pending_jobs": pending,
                "existing_jobs": sorted(set(pending) | self.extra_existing |
                                        {job_id for job_id, job in self.jobs.items()
                                         if job["stage"] == "submitted"}),
                "jobs": copy.deepcopy(self.jobs)}

    def records(self):
        return copy.deepcopy(self.rows)

    def export(self, pages, job_ids=()):
        self.actions.append(("export", [page["page"] for page in pages]))
        for page in pages:
            if not any(row["job"]["message"]["page"] == page["page"] for row in self.rows):
                self.rows.append(record(page["page"], len(self.rows) + 1))
        if self.crash_export:
            self.crash_export = False
            raise RuntimeError("interrupted export")

    def submit(self, job_id):
        self.actions.append(("submit", job_id))
        self.jobs[job_id] = {"stage": "submitted", "spool": None}
        if self.crash_submit:
            self.crash_submit = False
            self.jobs[job_id]["stage"] = "submitting"
            raise RuntimeError("uncertain create_work")

    def collect(self, job_id, spool):
        self.actions.append(("collect", job_id))
        if self.hold:
            raise RuntimeError("trust gate refused publication")
        row = next(row for row in self.rows if row["job"]["id"] == job_id)
        row["stage"] = "failed" if self.terminal else "published"


class SchedulerTests(unittest.TestCase):
    def setUp(self):
        self.config = config()
        self.state = {}
        self.fake = Fake()
        self.saved = []

    def tick(self, *, drain=False):
        return batch.tick(self.config, self.state, self.fake,
                          lambda: self.saved.append(copy.deepcopy(self.state)), NOW, drain=drain)

    def test_drain_does_not_allocate_or_dispatch(self):
        event = self.tick(drain=True)
        self.assertTrue(event["draining"])
        self.assertTrue(event["backpressure"])
        self.assertEqual(event["selected"], 0)
        self.assertEqual(self.fake.actions, [])

    def test_drain_collects_and_can_resume_same_allocation(self):
        self.tick()
        for job in self.fake.jobs.values():
            job["spool"] = "/project/usnmocr/results/batch1"
        self.fake.actions.clear()
        event = self.tick(drain=True)
        self.assertEqual(event["published"], 2)
        self.assertEqual(event["selected"], 2)
        self.assertEqual([action[0] for action in self.fake.actions], ["collect", "collect"])
        self.assertFalse(self.tick()["draining"])
        self.assertEqual(len(self.state["selected"]), 4)

    def test_drain_preserves_unexported_and_reserved_allocations(self):
        self.state["selected"] = {self.config["pages"][0]["page"]: NOW}
        self.fake.rows = [record(self.config["pages"][1]["page"], 1)]
        self.tick(drain=True)
        self.assertEqual(len(self.state["selected"]), 2)
        self.assertEqual(self.fake.actions, [])
        self.assertEqual(self.fake.rows[0]["stage"], "reserved")

    def test_drain_still_reports_uncertain_submissions(self):
        self.fake.rows = [record(self.config["pages"][0]["page"], 1)]
        self.fake.jobs["0" * 63 + "1"] = {"stage": "submitting", "spool": None}
        with self.assertRaisesRegex(ValueError, "uncertain"):
            self.tick(drain=True)
        self.assertEqual(self.fake.actions, [])

    def test_drain_does_not_bypass_trust_failure(self):
        self.tick()
        for job in self.fake.jobs.values():
            job["spool"] = "/project/usnmocr/results/batch1"
        self.fake.hold = True
        with self.assertRaisesRegex(RuntimeError, "trust"):
            self.tick(drain=True)

    def test_full_feed_collect_refill_and_replay(self):
        first = self.tick()
        self.assertEqual(first["selected"], 2)
        self.assertEqual(len(self.fake.jobs), 2)
        second = self.tick()
        self.assertEqual(second["selected"], 4)
        actions = len(self.fake.actions)
        self.assertTrue(self.tick()["backpressure"])
        self.assertEqual(len(self.fake.actions), actions)
        for job in self.fake.jobs.values():
            job["spool"] = "/project/usnmocr/results/batch1"
        third = self.tick()
        self.assertEqual(third["newly_published"], 2)
        self.assertEqual(third["selected"], 4)
        self.assertEqual(third["collection_backlog"], 2)
        fourth = self.tick()
        self.assertEqual(fourth["newly_published"], 2)
        self.assertEqual(self.tick()["published"], 4)
        self.assertEqual(len([action for action in self.fake.actions if action[0] == "submit"]), 5)

    def test_interrupted_export_recovers_authoritative_records(self):
        self.fake.crash_export = True
        with self.assertRaisesRegex(RuntimeError, "interrupted"):
            self.tick()
        self.assertEqual(len(self.saved[-1]["selected"]), 2)
        self.tick()
        self.assertEqual(len(self.fake.jobs), 2)
        self.tick()
        self.assertEqual(len(self.fake.jobs), 4)
        self.assertEqual(len({row["job"]["id"] for row in self.fake.rows}), 4)

    def test_uncertain_submission_never_resubmits_or_allocates(self):
        self.fake.crash_submit = True
        with self.assertRaises(RuntimeError):
            self.tick()
        actions = len(self.fake.actions)
        with self.assertRaisesRegex(ValueError, "uncertain"):
            self.tick()
        self.assertEqual(len(self.fake.actions), actions)
        self.assertEqual(len(self.state["selected"]), 2)

    def test_interrupted_pull_reserved_server_record_can_resume(self):
        self.fake.rows = [record(self.config["pages"][0]["page"], 1)]
        self.fake.jobs["0" * 63 + "1"] = {"stage": "reserved", "spool": None}
        self.tick()
        self.assertIn(("submit", "0" * 63 + "1"), self.fake.actions)

    def test_recovered_reservations_cannot_bypass_new_global_backlog(self):
        self.fake.crash_export = True
        with self.assertRaises(RuntimeError):
            self.tick()
        self.fake.other = 4
        self.tick()
        self.assertEqual(self.fake.jobs, {})
        self.fake.other = 3
        self.tick()
        self.assertEqual(len(self.fake.jobs), 1)
        self.assertEqual(len(self.state["selected"]), 2)

    def test_other_apps_backpressure_and_no_hosts(self):
        self.fake.other = 4
        self.assertTrue(self.tick()["backpressure"])
        self.assertEqual(self.fake.actions, [])
        self.fake.other = 0
        self.fake.hosts = 0
        self.assertEqual(self.tick()["target_pending"], 0)
        self.assertEqual(self.fake.actions, [])

    def test_external_reserved_work_consumes_headroom(self):
        external = record("different/page", 9)
        external["plan"] = "external"
        self.fake.rows = [external]
        self.config["max_pending"] = 1
        self.tick()
        self.assertEqual(self.fake.actions, [])

    def test_assimilated_but_incomplete_output_still_consumes_capacity(self):
        self.tick()
        self.tick()
        self.fake.assimilated = set(self.fake.jobs)
        actions = len(self.fake.actions)
        event = self.tick()
        self.assertEqual(event["observed_pending"], 4)
        self.assertTrue(event["backpressure"])
        self.assertEqual(len(self.fake.actions), actions)
        self.assertEqual(len(self.state["selected"]), 4)

    def test_collection_continues_without_hosts(self):
        self.tick()
        self.fake.hosts = 0
        for job in self.fake.jobs.values():
            job["spool"] = "/project/usnmocr/results/batch1"
        event = self.tick()
        self.assertEqual(event["published"], 2)
        self.assertEqual(len(self.state["selected"]), 2)

    def test_trust_hold_and_terminal_failures_stop_feed(self):
        self.tick()
        for job in self.fake.jobs.values():
            job["spool"] = "/project/usnmocr/results/batch1"
        self.fake.hold = True
        with self.assertRaisesRegex(RuntimeError, "trust"):
            self.tick()
        self.assertEqual(len(self.state["selected"]), 2)
        self.fake.hold = False
        self.fake.terminal = True
        with self.assertRaisesRegex(ValueError, "terminal"):
            self.tick()
        self.assertEqual(len(self.state["selected"]), 2)

    def test_allocation_or_pin_collision_is_operator_gate(self):
        self.fake.rows = [record(self.config["pages"][0]["page"], 1)]
        self.fake.rows[0]["job"]["engine"]["image"] = "sha256:" + "b" * 64
        with self.assertRaisesRegex(ValueError, "migration"):
            self.tick()
        self.assertEqual(self.fake.actions, [])

    def test_other_plan_same_page_cannot_overwrite_original_journal(self):
        original = record(self.config["pages"][0]["page"], 1, "submitted")
        original.update(plan="original-owner", stamp="original-timestamp")
        self.fake.rows = [copy.deepcopy(original)]
        with self.assertRaisesRegex(ValueError, "another plan"):
            self.tick()
        self.assertEqual(self.fake.rows, [original])
        self.assertEqual(self.fake.actions, [])
        self.assertNotIn("selected", self.state)

    def test_sql_workunit_with_missing_or_stale_bridge_journal_is_not_resubmitted(self):
        job_id = "0" * 63 + "1"
        self.fake.rows = [record(self.config["pages"][0]["page"], 1)]
        self.fake.extra_pending = {job_id}
        for remote in (None, {"stage": "reserved", "spool": None}):
            with self.subTest(remote=remote):
                self.fake.jobs = {} if remote is None else {job_id: remote}
                with self.assertRaisesRegex(ValueError, "SQL/journal mismatch"):
                    self.tick()
                self.assertEqual(self.fake.actions, [])
                self.assertNotIn("selected", self.state)

    def test_sql_orphan_discovered_after_export_is_not_submitted(self):
        self.fake.extra_pending = {"0" * 63 + "1"}
        with self.assertRaisesRegex(ValueError, "SQL/journal mismatch"):
            self.tick()
        self.assertEqual(self.fake.jobs, {})
        self.assertFalse(any(action[0] == "submit" for action in self.fake.actions))

    def test_completed_sql_work_with_missing_or_stale_journal_is_not_replayed(self):
        job_id = "0" * 63 + "1"
        self.fake.rows = [record(self.config["pages"][0]["page"], 1)]
        self.fake.extra_existing = {job_id}
        for remote in (None, {"stage": "reserved", "spool": None}):
            with self.subTest(remote=remote):
                self.fake.jobs = {} if remote is None else {job_id: remote}
                with self.assertRaisesRegex(ValueError, "SQL/journal mismatch"):
                    self.tick()
                self.assertEqual(self.fake.actions, [])
                self.assertNotIn("selected", self.state)

    def test_foreign_assimilated_backlog_blocks_recovered_submissions(self):
        own = [record(self.config["pages"][0]["page"], 1), record(self.config["pages"][1]["page"], 2)]
        foreign = [record("foreign/page-1", 10), record("foreign/page-2", 11)]
        for row in foreign:
            row["plan"] = "foreign-owner"
            self.fake.jobs[row["job"]["id"]] = {"stage": "submitted", "spool": None}
            self.fake.assimilated.add(row["job"]["id"])
        self.fake.rows = own + foreign
        self.fake.other = 2
        event = self.tick()
        self.assertTrue(event["backpressure"])
        self.assertEqual(self.fake.actions, [])
        self.assertFalse(any(job_id in self.fake.jobs for job_id in ("0" * 63 + "1", "0" * 63 + "2")))

    def test_journal_inventory_batches_metadata_and_discards_stale_cache(self):
        directory = Path(__file__).parent.resolve() / (".journal-test-" + uuid.uuid4().hex)
        directory.mkdir()
        try:
            transport = batch.Transport(self.config, directory)
            rows = [record("page-" + str(number), number) for number in range(1, 129)]
            names = [f'dispatch/jobs/{row["job"]["id"]}/record.json' for row in rows]
            stale = directory / "journal-cache" / "stale.json"
            stale.parent.mkdir()
            stale.write_text("{}")

            def download(argv, label):
                self.assertIn("download-batch", argv)
                for name, row in zip(names, rows):
                    path = directory / "journal-cache" / name
                    path.parent.mkdir(parents=True, exist_ok=True)
                    path.write_text(json.dumps(row))
                return ""

            with patch.object(transport, "az", return_value=json.dumps(names)), \
                    patch.object(transport, "run", side_effect=download) as run, \
                    patch.object(transport, "blob", side_effect=AssertionError("per-record CLI")):
                self.assertEqual(transport.records(), rows)
                self.assertEqual(run.call_count, 1)
                self.assertFalse(stale.exists())
            with patch.object(transport, "az", return_value=json.dumps(names)), \
                    patch.object(transport, "run", return_value=""), \
                    self.assertRaisesRegex(ValueError, "changed during download"):
                transport.records()
        finally:
            import shutil
            shutil.rmtree(directory)

    def test_stale_inventory(self):
        with self.assertRaisesRegex(ValueError, "fresh"):
            batch.tick(self.config, self.state, self.fake, lambda: None, NOW + 4000)

    def test_validate_offline_and_config_immutability(self):
        # Project-local scratch, never a system temporary directory.
        directory = Path(__file__).parent / (".batch-test-" + uuid.uuid4().hex)
        directory.mkdir()
        try:
            path = directory / "config.json"
            path.write_text(json.dumps(self.config))
            with patch.object(batch.subprocess, "run", side_effect=AssertionError("offline")), \
                    contextlib.redirect_stdout(io.StringIO()):
                self.assertEqual(batch.main(["--config", str(path), "--state-dir", str(directory)]), 0)
            output = io.StringIO()
            with patch.object(batch.subprocess, "run", side_effect=AssertionError("offline")), \
                    contextlib.redirect_stdout(output):
                self.assertEqual(batch.main(["--config", str(path), "--state-dir", str(directory),
                                             "--drain"]), 0)
            self.assertTrue(json.loads(output.getvalue())["draining"])
            self.assertFalse((directory / "state.json").exists())
            with patch.object(batch.Transport, "lock", return_value=contextlib.nullcontext()), \
                    patch.object(batch, "tick", return_value={"draining": True}) as tick, \
                    contextlib.redirect_stdout(io.StringIO()):
                self.assertEqual(batch.main(["--config", str(path), "--state-dir", str(directory),
                                             "--apply", "--drain"]), 0)
            self.assertTrue(tick.call_args.kwargs["drain"])
            batch.save(directory / "state.json", {"config_sha256": "wrong"})
            with contextlib.redirect_stderr(io.StringIO()):
                self.assertEqual(batch.main(["--config", str(path), "--state-dir", str(directory),
                                             "--apply"]), 1)
            self.config["pages"][0]["scan"] = "/tank/ocr/scans/keep/../../escape.jp2"
            path.write_text(json.dumps(self.config))
            with self.assertRaises(ValueError):
                batch.load_config(path)
        finally:
            for file in directory.iterdir():
                file.unlink()
            directory.rmdir()

    def test_case_sensitive_loc_batch_names_are_preserved_through_export(self):
        directory = Path(__file__).parent.resolve() / (".case-test-" + uuid.uuid4().hex)
        directory.mkdir()
        try:
            path = directory / "config.json"
            for name in ("curiv_joshuaTree", "lu_Cypher"):
                with self.subTest(batch=name):
                    config = copy.deepcopy(self.config)
                    config["batch"] = name
                    for page in config["pages"]:
                        page["scan"] = page["scan"].replace("test_batch", name)
                    path.write_text(json.dumps(config))
                    loaded = batch.load_config(path)
                    self.assertEqual(loaded["batch"], name)
                    transport = batch.Transport(loaded, directory)
                    with patch.object(transport, "run", return_value="") as run:
                        transport.export(loaded["pages"][:1])
                    self.assertIn("HANDOFF_BATCH=" + name, run.call_args.args[0])
                    self.assertIn("/keep/" + name + "/", run.call_args_list[0].args[0][-1])
        finally:
            (directory / "config.json").unlink()
            directory.rmdir()

    def test_batch_validation_matches_native_base_batch_shape_and_bound(self):
        directory = Path(__file__).parent.resolve() / (".batch-name-test-" + uuid.uuid4().hex)
        directory.mkdir()
        try:
            path = directory / "config.json"
            for name in ("LU_Cypher", "lu_Cypher_ver01", "missingawardee", "lu_" + "a" * 126):
                with self.subTest(batch=name):
                    config = {**self.config, "batch": name}
                    path.write_text(json.dumps(config))
                    with self.assertRaises(ValueError):
                        batch.load_config(path)
        finally:
            (directory / "config.json").unlink()
            directory.rmdir()

    def test_transport_uses_real_steps_and_waits(self):
        transport = batch.Transport(self.config, Path("."))
        with patch.object(transport, "run", return_value="") as run:
            transport.export(self.config["pages"][:2])
            self.assertIn("--scan", run.call_args_list[0].args[0])
            self.assertIn("--wait-mins", run.call_args_list[1].args[0])
            self.assertIn("HANDOFF_PLAN=batch-test", run.call_args_list[1].args[0])
            self.assertTrue(any(arg.startswith("HANDOFF_SCANS=keep/") for arg in
                                run.call_args_list[1].args[0]))
            transport.submit("a" * 64)
            self.assertIn("submit-remote", run.call_args.args[0])
            transport.collect("a" * 64, "/project/usnmocr/results/1")
            self.assertIn("spool-out", run.call_args_list[-2].args[0])
            self.assertIn("collect", run.call_args.args[0])

    def test_distributed_lock_lease_released_on_failure(self):
        transport = batch.Transport(self.config, Path("."))
        with patch.object(transport, "az", return_value="true") as az:
            with self.assertRaises(RuntimeError):
                with transport.lock():
                    raise RuntimeError("tick failure")
            self.assertEqual(az.call_args_list[1].args[:2], ("lease", "acquire"))
            self.assertEqual(az.call_args_list[-1].args[:2], ("lease", "release"))

    def test_handoff_bounds_export_and_collect_to_exact_objects(self):
        directory = Path(__file__).parent.resolve() / (".handoff-test-" + uuid.uuid4().hex)
        directory.mkdir()
        try:
            executable = directory / "usnm-boinc"
            executable.write_text("""#!/usr/bin/env python3
import json, os, sys
from pathlib import Path
args = sys.argv[1:]
with Path(os.environ["CALL_LOG"]).open("a") as out:
    out.write(json.dumps(args) + "\\n")
def value(flag):
    return args[args.index(flag) + 1]
if args[:2] == ["handoff", "get-file"]:
    path = Path(value("--to"))
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text("{}")
elif args[:2] == ["handoff", "get-spool"]:
    Path(value("--trust-out")).write_text("{}")
    print("42")
elif args[0] == "export":
    path = Path(value("--root")) / "jobs" / ("a" * 64) / "record.json"
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text("{}")
""")
            executable.chmod(0o700)
            import os
            env = {**os.environ, "PATH": str(directory) + os.pathsep + os.environ["PATH"],
                   "TMPDIR": str(directory), "CALL_LOG": str(directory / "calls.jsonl"),
                   "HANDOFF_URL": "https://exchange123.blob.core.windows.net/boinc-handoff",
                   "COSMOS_ENDPOINT": "synthetic", "CURATED_URL": "synthetic",
                   "REFERENCE_URL": "synthetic", "OUT_URL": "synthetic"}
            script = batch.REPO / "contrib-compute/images/bridge/handoff-job.sh"
            subprocess.run(["bash", str(script)], env={
                **env, "HANDOFF_STEP": "export", "HANDOFF_PAGES": self.config["pages"][0]["page"],
                "HANDOFF_PLAN": "batch-test", "HANDOFF_BATCH": "test_batch",
                "ENGINE_IMAGE": self.config["image"], "HANDOFF_MANIFEST": self.config["manifest"],
                "HANDOFF_SCANS": "keep/test_batch/selected.jp2", "HANDOFF_JOBS": "a" * 64,
            }, text=True, capture_output=True, check=True)
            calls = [json.loads(line) for line in (directory / "calls.jsonl").read_text().splitlines()]
            objects = [call[call.index("--path") + 1] for call in calls
                       if call[:2] == ["handoff", "get-file"]]
            self.assertEqual(objects, ["scans/keep/test_batch/selected.jp2",
                                       "scans/keep/test_batch/selected.jp2.json",
                                       "dispatch/jobs/" + "a" * 64 + "/record.json",
                                       "manifests/v1/manifest.parquet"])
            self.assertFalse(any(call[:2] == ["handoff", "get"] for call in calls))
            (directory / "calls.jsonl").write_text("")
            subprocess.run(["bash", str(script)], env={
                **env, "HANDOFF_STEP": "collect", "JOB_ID": "a" * 64,
            }, text=True, capture_output=True, check=True)
            calls = [json.loads(line) for line in (directory / "calls.jsonl").read_text().splitlines()]
            self.assertEqual(sum(call[:2] == ["handoff", "get-file"] for call in calls), 1)
            prefixes = [call[call.index("--prefix") + 1] for call in calls
                        if call[:2] == ["handoff", "get"]]
            self.assertEqual(prefixes, ["dispatch/jobs/" + "a" * 64 + "/accepted"])
            collect = next(call for call in calls if call[0] == "collect")
            self.assertIn("--trust", collect)
            self.assertEqual(collect[collect.index("--host-id") + 1], "42")
        finally:
            import shutil
            shutil.rmtree(directory)


def queue_config():
    value = config()
    del value["batch"]
    value.update(source="queue", claim_queues=["ocr-bulk"], lease_hours=96, pages=[])
    return value


def message(number, batch="test_batch"):
    page = f"sn12345678/1900-01-01/ed-1/seq-{number}"
    return {"page": page, "doc_id": page.replace("/", "_"), "batch": batch, "queue": "ocr-bulk",
            "scan": f"keep/{batch}/sn12345678/1900-01-01/1/{number}",
            "lease_until": "2026-10-21T00:00:00Z", "message_sha256": f"{number:064x}"}


class QueueFake(Fake):
    """The work queue behind the claim step: messages in line, and the scan server's cache."""

    def __init__(self, waiting):
        super().__init__()
        self.waiting = list(waiting)
        self.uncached = set()
        self.claimed = []
        self.released = []
        self.pushes = []

    def claim(self, count):
        self.actions.append(("claim", count))
        taken, self.waiting = self.waiting[:count], self.waiting[count:]
        self.claimed += [item["page"] for item in taken]
        return copy.deepcopy(taken)

    def push_pages(self, bases):
        self.pushes.append(list(bases))
        found = {base: f"{base}.jp2" for base in bases if base not in self.uncached}
        return found, [base for base in bases if base in self.uncached]

    def release(self, doc_ids):
        self.actions.append(("release", sorted(doc_ids)))
        self.released += doc_ids

    def export(self, pages, job_ids=()):
        by_batch = {}
        for page in pages:
            by_batch.setdefault(page["batch"], []).append(page)
        for name, group in sorted(by_batch.items()):
            self.actions.append(("export", name, [page["page"] for page in group]))
            for page in group:
                self.assertions.append(page["scan"])
                if not any(row["job"]["message"]["page"] == page["page"] for row in self.rows):
                    row = record(page["page"], len(self.rows) + 1)
                    row["job"]["message"]["batch"] = name
                    self.rows.append(row)

    assertions = []


class QueueModeTests(unittest.TestCase):
    def setUp(self):
        self.config = queue_config()
        self.state = {}
        self.fake = QueueFake([message(number) for number in range(1, 8)])
        self.fake.assertions = []
        self.saved = []

    def tick(self):
        return batch.tick(self.config, self.state, self.fake,
                          lambda: self.saved.append(copy.deepcopy(self.state)), NOW)

    def test_claims_only_what_it_can_start_then_pushes_exports_and_submits(self):
        first = self.tick()
        self.assertEqual(self.fake.actions[0], ("claim", 2))
        self.assertEqual(first["selected"], 2)
        self.assertEqual(len(self.fake.jobs), 2)
        # The claim is on disk before its scan is pushed or anything else is done for it.
        claimed = next(state for state in self.saved if state.get("claims"))["claims"]
        self.assertEqual(len(claimed), 2)
        self.assertTrue(all(c["scan"] is None for c in claimed.values()))
        # The scan path is the one the push reported, under the configured scans root.
        self.assertEqual(self.fake.assertions,
                         [f"/tank/ocr/scans/keep/test_batch/sn12345678/1900-01-01/1/{n}.jp2" for n in (1, 2)])
        self.tick()
        self.assertEqual(len(self.fake.jobs), 4)
        # At the cap (max_pending 4): no more claims, so no more messages are hidden.
        claims = [a for a in self.fake.actions if a[0] == "claim"]
        self.assertTrue(self.tick()["backpressure"])
        self.assertEqual([a for a in self.fake.actions if a[0] == "claim"], claims)
        self.assertEqual(len(self.fake.waiting), 3)

    def test_other_work_in_line_shrinks_the_claim(self):
        self.fake.other = 3
        self.tick()
        self.assertEqual(self.fake.actions[0], ("claim", 1))
        self.assertEqual(len(self.fake.claimed), 1)

    def test_no_hosts_claims_nothing(self):
        self.fake.hosts = 0
        self.tick()
        self.assertEqual(self.fake.actions, [])
        self.assertEqual(len(self.fake.waiting), 7)

    def test_a_page_with_no_cached_scan_goes_back_to_the_queue(self):
        self.fake.uncached = {"keep/test_batch/sn12345678/1900-01-01/1/1"}
        self.tick()
        self.assertEqual(self.fake.released, ["sn12345678_1900-01-01_ed-1_seq-1"])
        self.assertNotIn("sn12345678/1900-01-01/ed-1/seq-1", self.state["claims"])
        exported = [a for a in self.fake.actions if a[0] == "export"]
        self.assertEqual(exported, [("export", "test_batch", ["sn12345678/1900-01-01/ed-1/seq-2"])])

    def test_a_failed_release_is_retried_before_anything_else(self):
        self.fake.uncached = {"keep/test_batch/sn12345678/1900-01-01/1/1"}
        original = self.fake.release
        self.fake.release = lambda ids: (_ for _ in ()).throw(RuntimeError("release failed"))
        with self.assertRaisesRegex(RuntimeError, "release failed"):
            self.tick()
        self.assertTrue(self.state["claims"]["sn12345678/1900-01-01/ed-1/seq-1"]["release"])
        self.fake.release = original
        self.fake.uncached = set()
        self.tick()
        self.assertEqual(self.fake.released, ["sn12345678_1900-01-01_ed-1_seq-1"])
        self.assertNotIn("sn12345678/1900-01-01/ed-1/seq-1", self.state["claims"])

    def test_restart_resumes_claimed_pages_without_claiming_again(self):
        # A crash between claiming and pushing: the claims are in the state, not yet pushed.
        self.fake.push_pages = lambda bases: (_ for _ in ()).throw(RuntimeError("push interrupted"))
        with self.assertRaisesRegex(RuntimeError, "push interrupted"):
            self.tick()
        self.assertEqual(len(self.state["claims"]), 2)
        del self.fake.push_pages
        self.tick()
        self.assertEqual(len(self.fake.claimed), 2)  # nothing more taken off the queue
        self.assertEqual(len(self.fake.jobs), 2)

    def test_pages_from_several_batches_export_one_batch_at_a_time(self):
        self.fake.waiting = [message(1, "aa_one"), message(2, "bb_two")]
        self.tick()
        exported = [a for a in self.fake.actions if a[0] == "export"]
        self.assertEqual(exported, [("export", "aa_one", ["sn12345678/1900-01-01/ed-1/seq-1"]),
                                    ("export", "bb_two", ["sn12345678/1900-01-01/ed-1/seq-2"])])
        self.tick()
        self.assertEqual(len(self.fake.jobs), 2)

    def test_a_journal_for_a_page_the_plan_never_claimed_stops_the_feed(self):
        stray = record("sn99999999/1900-01-01/ed-1/seq-1", 9)
        self.fake.rows = [stray]
        with self.assertRaisesRegex(ValueError, "different allocation"):
            self.tick()
        self.assertEqual(self.fake.actions, [])

    def test_a_journal_in_another_batch_than_the_claim_stops_the_feed(self):
        self.tick()
        self.fake.rows[0]["job"]["message"]["batch"] = "zz_other"
        with self.assertRaisesRegex(ValueError, "different allocation"):
            self.tick()

    def test_collection_goes_on_and_published_pages_stay_in_the_plan(self):
        self.tick()
        for job in self.fake.jobs.values():
            job["spool"] = "/project/usnmocr/results/batch1"
        event = self.tick()
        self.assertEqual(event["newly_published"], 2)
        # Published pages stay in the plan's state (their journals belong to it) beside the
        # pages claimed to refill the queue.
        self.assertEqual(len(self.state["claims"]), 4)
        self.assertEqual(sorted(self.state["claims"])[:2],
                         ["sn12345678/1900-01-01/ed-1/seq-1", "sn12345678/1900-01-01/ed-1/seq-2"])

    def test_a_claim_naming_another_pages_scan_is_refused(self):
        bad = message(1)
        bad["scan"] = "keep/test_batch/sn12345678/1900-01-01/1/2"
        self.fake.waiting = [bad]
        with self.assertRaisesRegex(ValueError, "different page"):
            self.tick()
        for field, value in (("batch", "Bad_Batch_ver01"), ("page", "../x"), ("doc_id", "other")):
            broken = message(1)
            broken[field] = value
            self.fake.waiting = [broken]
            with self.assertRaises(ValueError):
                self.tick()

    def test_the_same_message_again_after_its_lease_is_the_same_claim(self):
        self.tick()
        before = copy.deepcopy(self.state["claims"])
        jobs = len(self.fake.jobs)
        # The lease on page 1 ran out while its job was pending; the message came round again.
        self.fake.waiting = [message(1)] + self.fake.waiting
        self.fake.other = 0
        for job in self.fake.jobs.values():
            job["spool"] = "/project/usnmocr/results/batch1"
        self.tick()
        self.assertEqual(self.fake.claimed.count("sn12345678/1900-01-01/ed-1/seq-1"), 2)
        self.assertEqual(self.state["claims"]["sn12345678/1900-01-01/ed-1/seq-1"],
                         before["sn12345678/1900-01-01/ed-1/seq-1"])
        self.assertGreaterEqual(len(self.fake.jobs), jobs)
        self.assertEqual(len([row for row in self.fake.rows
                              if row["job"]["message"]["page"] == "sn12345678/1900-01-01/ed-1/seq-1"]), 1)

    def test_a_different_message_for_a_claimed_page_stops_the_feed(self):
        self.tick()
        other = message(1)
        other["message_sha256"] = "f" * 64
        self.fake.waiting = [other]
        for job in self.fake.jobs.values():
            job["spool"] = "/project/usnmocr/results/batch1"
        with self.assertRaisesRegex(ValueError, "different message"):
            self.tick()

    def test_claims_made_before_a_failed_claim_step_are_kept(self):
        def failing(count):
            raise batch.ClaimFailed("claim step failed", [message(1)])

        self.fake.claim = failing
        with self.assertRaisesRegex(RuntimeError, "claim step failed"):
            self.tick()
        self.assertEqual(list(self.state["claims"]), ["sn12345678/1900-01-01/ed-1/seq-1"])
        self.assertEqual(self.saved[-1]["claims"], self.state["claims"])

    def test_more_claims_than_asked_for_is_refused(self):
        self.fake.claim = lambda count: [message(n) for n in range(1, 10)]
        with self.assertRaisesRegex(ValueError, "more pages than asked"):
            self.tick()

    def test_queue_config_validation(self):
        directory = Path(__file__).parent.resolve() / (".queue-config-" + uuid.uuid4().hex)
        directory.mkdir()
        try:
            path = directory / "config.json"
            good = queue_config()
            del good["pages"]
            path.write_text(json.dumps(good))
            loaded = batch.load_config(path)
            self.assertEqual((loaded["source"], loaded["pages"]), ("queue", []))
            for change in ({"claim_queues": ["ocr-deadletter"]}, {"claim_queues": []},
                           {"claim_queues": ["ocr-bulk", "ocr-bulk"]}, {"lease_hours": 169},
                           {"lease_hours": 0}, {"batch": "test_batch"}, {"pages": [message(1)]},
                           {"source": "other"}):
                with self.subTest(change=change):
                    path.write_text(json.dumps({**good, **change}))
                    with self.assertRaises((ValueError, KeyError)):
                        batch.load_config(path)
            missing = {k: v for k, v in good.items() if k != "lease_hours"}
            path.write_text(json.dumps(missing))
            with self.assertRaises(ValueError):
                batch.load_config(path)
            # Offline: the plan names its source and takes nothing from the queue.
            path.write_text(json.dumps(good))
            out = io.StringIO()
            with patch.object(batch.subprocess, "run", side_effect=AssertionError("offline")), \
                    contextlib.redirect_stdout(out):
                self.assertEqual(batch.main(["--config", str(path), "--state-dir", str(directory)]), 0)
            event = json.loads(out.getvalue())
            self.assertEqual((event["source"], event["allocated_pages"]), ("queue", 0))
        finally:
            for file in directory.iterdir():
                file.unlink()
            directory.rmdir()


class QueueTransportTests(unittest.TestCase):
    def setUp(self):
        self.config = queue_config()
        self.transport = batch.Transport(self.config, Path("."))

    def test_claim_step_variables_and_nonce(self):
        sent = {}

        def step(target, name, **variables):
            sent.update(target=target, name=name, **variables)

        summary = {"schema_version": 1, "nonce": None, "claims": [message(1)]}

        def blob(name):
            self.assertEqual(name, "scheduler/claims.json")
            return {**summary, "nonce": sent["CLAIM_NONCE"]}

        with patch.object(self.transport, "step", side_effect=step), \
                patch.object(self.transport, "blob", side_effect=blob):
            self.assertEqual(self.transport.claim(3), [message(1)])
        self.assertEqual((sent["target"], sent["name"]), ("handoff", "claim"))
        self.assertEqual((sent["CLAIM_MAX"], sent["CLAIM_QUEUES"], sent["CLAIM_LEASE_HOURS"]),
                         (3, "ocr-bulk", 96))
        self.assertEqual(sent["HANDOFF_MANIFEST"], "v1/manifest.parquet")
        with patch.object(self.transport, "step"), \
                patch.object(self.transport, "blob", return_value={**summary, "nonce": "other"}), \
                self.assertRaisesRegex(ValueError, "stale/concurrent"):
            self.transport.claim(1)

    def test_a_failed_claim_step_still_returns_the_claims_it_made(self):
        def step(*args, **kwargs):
            raise RuntimeError("handoff/claim failed")

        def blob(name):
            return {"schema_version": 1, "nonce": nonce["value"], "claims": [message(1)]}

        nonce = {}
        original = batch.uuid.uuid4
        with patch.object(self.transport, "step", side_effect=step), \
                patch.object(self.transport, "blob", side_effect=blob):
            with patch.object(batch.uuid, "uuid4") as make:
                make.return_value = original()
                nonce["value"] = make.return_value.hex
                with self.assertRaises(batch.ClaimFailed) as caught:
                    self.transport.claim(2)
        self.assertEqual(caught.exception.claims, [message(1)])
        # No summary: the plain failure, nothing to keep.
        with patch.object(self.transport, "step", side_effect=step), \
                patch.object(self.transport, "blob", side_effect=RuntimeError("no blob")), \
                self.assertRaisesRegex(RuntimeError, "claim failed"):
            self.transport.claim(2)

    def test_push_pages_reads_what_the_script_reports(self):
        base = "keep/test_batch/sn12345678/1900-01-01/1/"
        # The script takes a page without "keep/", prints the pushed path with it, and names a
        # missing page without it.
        output = f"pushed {base}1.jp2\nmissing {base[5:]}2\n"
        with patch.object(self.transport, "run", return_value=output) as run:
            found, missing = self.transport.push_pages([base + "1", base + "2"])
        self.assertEqual((found, missing), ({base + "1": base + "1.jp2"}, [base + "2"]))
        argv = run.call_args.args[0]
        pages = [argv[i + 1] for i, arg in enumerate(argv) if arg == "--keep-page"]
        self.assertEqual(pages, [base[5:] + "1", base[5:] + "2"])
        self.assertEqual(run.call_args.kwargs["ok"], (0, 3))
        # A page the script said nothing about is an error, not a quiet skip.
        with patch.object(self.transport, "run", return_value=f"pushed {base}1.jp2\n"), \
                self.assertRaisesRegex(ValueError, "account for every"):
            self.transport.push_pages([base + "1", base + "2"])

    def test_push_pages_against_the_real_push_script(self):
        directory = Path(__file__).parent.resolve() / (".push-test-" + uuid.uuid4().hex)
        directory.mkdir()
        try:
            scans, manifests = directory / "scans", directory / "manifests"
            for number, ext in ((1, "jp2"), (3, "jpg")):
                scan = scans / "keep/test_batch/sn12345678/1900-01-01/1" / f"{number}.{ext}"
                scan.parent.mkdir(parents=True, exist_ok=True)
                scan.write_bytes(b"scan")
                Path(f"{scan}.json").write_text("{}")
            (manifests / "v1").mkdir(parents=True)
            (manifests / "v1/manifest.parquet").write_bytes(b"manifest")
            bin_dir = directory / "bin"
            bin_dir.mkdir()
            az = bin_dir / "az"
            az.write_text('#!/usr/bin/env bash\n[ "$3" = show ] && exit 1\nexit 0\n')
            az.chmod(0o755)
            config = {**self.config, "scans_root": str(scans), "manifests_root": str(manifests),
                      "push_command": [str(batch.REPO / "scripts/ocr-node/push-boinc-scan.sh")]}
            transport = batch.Transport(config, directory)
            base = "keep/test_batch/sn12345678/1900-01-01/1/"
            with patch.dict(batch.os.environ, {"PATH": f"{bin_dir}:{batch.os.environ['PATH']}"}):
                found, missing = transport.push_pages([base + "1", base + "2", base + "3"])
            self.assertEqual(found, {base + "1": base + "1.jp2", base + "3": base + "3.jpg"})
            self.assertEqual(missing, [base + "2"])
        finally:
            import shutil
            shutil.rmtree(directory)

    def test_queue_export_passes_claims_and_one_batch_per_step(self):
        pages = [{"page": "sn12345678/1900-01-01/ed-1/seq-1", "batch": "aa_one",
                  "scan": "/tank/ocr/scans/keep/aa_one/sn12345678/1900-01-01/1/1.jp2"},
                 {"page": "sn12345678/1900-01-01/ed-1/seq-2", "batch": "bb_two",
                  "scan": "/tank/ocr/scans/keep/bb_two/sn12345678/1900-01-01/1/2.jp2"}]
        with patch.object(self.transport, "run", return_value="") as run:
            self.transport.export(pages, ["a" * 64])
        argvs = [call.args[0] for call in run.call_args_list]
        self.assertEqual(len(argvs), 2)  # no --scan push here: the scans went up when claimed
        self.assertTrue(all("HANDOFF_CLAIMS=1" in argv for argv in argvs))
        self.assertIn("HANDOFF_BATCH=aa_one", argvs[0])
        self.assertIn("HANDOFF_BATCH=bb_two", argvs[1])
        self.assertTrue(all("--scan" not in argv for argv in argvs))

    def test_release_step(self):
        with patch.object(self.transport, "run", return_value="") as run:
            self.transport.release(["a_b", "c_d"])
        argv = run.call_args.args[0]
        self.assertIn("release", argv)
        self.assertIn("HANDOFF_DOCS=a_b,c_d", argv)


if __name__ == "__main__":
    unittest.main()
