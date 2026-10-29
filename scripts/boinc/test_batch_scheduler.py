import contextlib
import copy
import importlib.util
import io
import json
import subprocess
import sys
import tempfile
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

    def test_reviewed_retry_lineage_is_owned_but_never_automatically_submitted(self):
        page = self.config["pages"][0]["page"]
        self.config["pages"] = self.config["pages"][:1]
        parent = record(page, 1, "failed")
        child = record(page, 2)
        child["job"].update(v=2, attempt=1, retry_of=parent["job"]["id"])
        child["job"]["engine"]["image"] = "sha256:" + "b" * 64
        child["retry"] = {"previous_job": parent["job"]["id"], "previous_attempt": 0,
                          "app_version_num": 27}
        self.fake.rows = [child, parent]
        self.assertEqual(batch.owned_records(self.config, self.fake.rows), {page: child})
        self.tick()
        self.assertEqual(self.fake.actions, [])
        self.fake.jobs[child["job"]["id"]] = {
            "stage": "submitted", "spool": "/project/usnmocr/results/one"}
        child["stage"] = "submitted"
        self.tick()
        self.assertEqual(self.fake.actions, [("collect", child["job"]["id"])])
        self.assertEqual(parent["stage"], "failed")
        branch = copy.deepcopy(child)
        branch["job"]["id"] = "f" * 64
        with self.assertRaisesRegex(ValueError, "branching"):
            batch.owned_records(self.config, [parent, child, branch])

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
            listing = [{"name": name, "etag": '"version-1"'} for name in names]
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

            with patch.object(transport, "az", side_effect=lambda *args: json.dumps(listing)) as az, \
                    patch.object(transport, "run", side_effect=download) as run, \
                    patch.object(transport, "blob", side_effect=AssertionError("per-record CLI")):
                self.assertEqual(transport.records(), rows)
                returned = transport.records()
                returned[0]["stage"] = "must-not-poison-cache"
                self.assertEqual(transport.records(), rows)
                self.assertEqual(run.call_count, 1)
                self.assertEqual(az.call_count, 3)
                self.assertFalse(stale.exists())
                listing[0]["etag"] = '"version-2"'
                rows[0]["stage"] = "published"
                self.assertEqual(transport.records(), rows)
                self.assertEqual(run.call_count, 2)
                listing.pop()
                names.pop()
                rows.pop()
                self.assertEqual(transport.records(), rows)
                self.assertEqual(run.call_count, 3)
            listing[0]["etag"] = '"version-3"'
            with patch.object(transport, "az", return_value=json.dumps(listing)), \
                    patch.object(transport, "run", return_value=""), \
                    self.assertRaisesRegex(ValueError, "changed during download"):
                transport.records()
            with patch.object(transport, "az", return_value=json.dumps(listing)), \
                    patch.object(transport, "run", side_effect=download) as run:
                self.assertEqual(transport.records(), rows)
                self.assertEqual(run.call_count, 1)
            listing[0]["etag"] = None
            with patch.object(transport, "az", return_value=json.dumps(listing)), \
                    patch.object(transport, "run", side_effect=AssertionError("invalid inventory")), \
                    self.assertRaisesRegex(ValueError, "missing journal ETag"):
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


GOLD = {"campaign": "v1-tess550", "language": "eng", "profile": "eng",
        "engine_base": "docker.io/tesseract@sha256:" + "c" * 64}


class GoldFake(Fake):
    """A feed whose trust gate holds results until gold-promote admits their host."""

    def __init__(self):
        super().__init__()
        self.now = NOW
        self.steps = []
        self.fail_steps = set()
        self.admits = True

    def inventory(self):
        return {**super().inventory(), "created_at": self.now}

    def step(self, target, step, **variables):
        # As bridge-job.sh: GOLD_MAX_HOSTS is 1 to 100.
        assert 1 <= int(variables["GOLD_MAX_HOSTS"]) <= 100, variables
        self.steps.append((target, step, variables))
        self.actions.append(("step", step))
        if step in self.fail_steps:
            raise RuntimeError(f"{target}/{step} failed; inspect the execution privately before retrying")
        if step == "gold-promote" and self.admits:
            self.hold = False


class GoldOnboardingTests(unittest.TestCase):
    """#901: calibrate new hosts before their results stall collection."""

    def setUp(self):
        self.config = {**config(), "gold_onboarding": [dict(GOLD)], "max_global_pending": 1000}
        self.state = {}
        self.fake = GoldFake()

    def tick(self, at=0, drain=False):
        self.fake.now = NOW + at
        # Per-app counts, as live: gold units then weigh on the global cap, not the OCR cohort.
        return batch.tick(self.config, self.state, self.fake, lambda: None, NOW + at, drain=drain,
                          server_status=status_xml(qwen_unsent=0, quality_unsent=0,
                                                   quality_in_progress=0, train_unsent=0))

    def ran(self):
        return [variables for _, _, variables in self.fake.steps]

    def test_steps_and_their_settings(self):
        event = self.tick()
        self.assertEqual([(t, s) for t, s, _ in self.fake.steps],
                         [("bridge", "gold-onboard"), ("bridge", "gold-promote")])
        self.assertEqual(self.fake.steps[0][2], {
            "GOLD_CAMPAIGN": "v1-tess550", "GOLD_LANGUAGE": "eng", "GOLD_PROFILE": "eng",
            "GOLD_ENGINE_IMAGE": self.config["image"], "GOLD_ENGINE_BASE": GOLD["engine_base"],
            "GOLD_ENGINE_VERSION": "5.5.0", "GOLD_MAX_HOSTS": 10})
        self.assertEqual(event["gold_onboarding"][0]["scope"], "v1-tess550/eng/eng")
        self.assertEqual(event["gold_onboarding"][0]["steps"],
                         {"gold-onboard": "ok", "gold-promote": "ok"})
        # Gold runs before any feed work in the tick.
        self.assertEqual(self.fake.actions[:2], [("step", "gold-onboard"), ("step", "gold-promote")])

    def test_runs_on_its_interval_and_sooner_while_results_are_held(self):
        self.tick()
        self.tick(at=30 * 60)
        self.assertEqual(len(self.fake.steps), 2)
        self.tick(at=60 * 60)
        self.assertEqual(len(self.fake.steps), 4)
        # A held result shortens the wait to ten minutes.
        for job in self.fake.jobs.values():
            job["spool"] = "/project/usnmocr/results/batch1"
        self.fake.hold = True
        self.fake.admits = False
        with self.assertRaisesRegex(RuntimeError, "trust"):
            self.tick(at=65 * 60)
        self.assertEqual(len(self.fake.steps), 4)
        with self.assertRaisesRegex(RuntimeError, "trust"):
            self.tick(at=70 * 60)
        self.assertEqual(len(self.fake.steps), 6)
        self.assertTrue(self.state["gold_onboarding"]["v1-tess550/eng/eng"]["held"])

    def test_an_admitted_host_publishes_its_held_result_in_the_same_tick(self):
        self.tick()
        for job in self.fake.jobs.values():
            job["spool"] = "/project/usnmocr/results/batch1"
        self.fake.hold = True
        event = self.tick(at=60 * 60)
        self.assertEqual(event["newly_published"], 2)
        steps = [a for a in self.fake.actions if a[0] in ("step", "collect")]
        self.assertEqual(steps[-4:], [("step", "gold-onboard"), ("step", "gold-promote"),
                                      ("collect", self.fake.rows[0]["job"]["id"]),
                                      ("collect", self.fake.rows[1]["job"]["id"])])
        self.assertEqual(event["collection_held"], 0)

    def test_a_failed_step_is_reported_and_not_retried_every_tick(self):
        self.fake.fail_steps = {"gold-onboard"}
        event = self.tick()
        self.assertEqual(event["gold_onboarding"][0]["steps"]["gold-promote"], "ok")
        self.assertIn("bridge/gold-onboard failed", event["gold_onboarding"][0]["steps"]["gold-onboard"])
        self.assertIn({"code": "gold_onboarding_failed", "scope": "v1-tess550/eng/eng",
                       "step": "gold-onboard",
                       "error": event["gold_onboarding"][0]["steps"]["gold-onboard"]}, event["alerts"])
        # The feed went on as before.
        self.assertEqual(len(self.fake.jobs), 2)
        self.tick(at=5 * 60)
        self.assertEqual(len(self.fake.steps), 2)

    def test_the_run_is_on_record_before_its_steps(self):
        saved = []
        self.fake.fail_steps = {"gold-onboard", "gold-promote"}
        batch.tick(self.config, self.state, self.fake,
                   lambda: saved.append(copy.deepcopy(self.state.get("gold_onboarding"))), NOW)
        first = next(s for s in saved if s)
        self.assertEqual(first["v1-tess550/eng/eng"],
                         {"at": NOW, "held": False, "max_hosts": 10, "steps": {}})

    def test_gold_units_count_against_the_global_cap(self):
        # 997 of 1000 in flight: three gold units at most, and the refill sees them.
        self.fake.other = 997
        event = self.tick()
        onboard = self.fake.steps[0][2]
        self.assertEqual(onboard["GOLD_MAX_HOSTS"], 3)
        self.assertEqual(event["gold_onboarding"][0]["max_hosts"], 3)
        self.assertEqual(event["global_pending"], 1000)
        self.assertEqual(len(self.fake.jobs), 0)
        # Two scopes share what is left.
        self.config["gold_onboarding"] = [dict(GOLD), {**GOLD, "language": "deu", "profile": "frak2021"}]
        self.state = {}
        self.fake.steps.clear()
        self.tick(at=60)
        self.assertEqual([v["GOLD_MAX_HOSTS"] for _, s, v in self.fake.steps if s == "gold-onboard"], [3])
        self.assertEqual([(s, v["GOLD_MAX_HOSTS"]) for _, s, v in self.fake.steps],
                         [("gold-onboard", 3), ("gold-promote", 10), ("gold-promote", 10)])

    def test_the_cap_is_the_status_aware_one_without_capacity_overrides(self):
        # No watermarks, cohorts or max_global_pending: with per-app status the cap is the
        # cohort-mode default (1000), not the OCR high target (4 here).
        del self.config["max_global_pending"]
        self.fake.other = 990
        event = self.tick()
        self.assertEqual(self.fake.steps[0][1], "gold-onboard")
        self.assertEqual(self.fake.steps[0][2]["GOLD_MAX_HOSTS"], 10)
        self.assertEqual(event["max_global_pending"], 1000)

    def other_plan(self, number, stage):
        row = record(f"sn87654321/1900-01-01/ed-1/seq-{number}", 1000 + number, stage)
        row["plan"] = "another-plan"
        self.fake.rows.append(row)

    def test_room_leaves_out_recovered_reservations_and_other_plans_uncollected_work(self):
        self.fake.other = 990
        for number in range(1, 4):
            self.other_plan(number, "reserved")  # recovered reservations, not on BOINC
        for number in range(4, 6):
            self.other_plan(number, "submitted")  # returned, not yet collected
        self.tick()
        self.assertEqual(self.fake.steps[0][2]["GOLD_MAX_HOSTS"], 5)
        # Five more: no room for gold units; promotion still runs, with its own limit.
        for number in range(6, 11):
            self.other_plan(number, "reserved")
        self.state = {}
        self.fake.steps.clear()
        event = self.tick(at=60)
        self.assertEqual([(s, v["GOLD_MAX_HOSTS"]) for _, s, v in self.fake.steps],
                         [("gold-promote", 10)])
        self.assertEqual(event["gold_onboarding"][0]["steps"]["gold-onboard"], batch.GOLD_SKIPPED_AT_CAP)

    def test_without_status_no_gold_units_are_issued(self):
        def broken():
            raise RuntimeError("server status fetch failed; inspect the endpoint privately")

        self.fake.now = NOW
        with self.assertRaisesRegex(RuntimeError, "server status"):
            batch.tick(self.config, self.state, self.fake, lambda: None, NOW, server_status=broken)
        self.assertEqual([(s, v["GOLD_MAX_HOSTS"]) for _, s, v in self.fake.steps], [("gold-promote", 10)])

    def test_at_the_cap_only_promotion_runs(self):
        self.fake.other = 1000
        event = self.tick()
        self.assertEqual([s for _, s, _ in self.fake.steps], ["gold-promote"])
        # Promotion keeps the configured limit: the bridge refuses GOLD_MAX_HOSTS=0.
        self.assertEqual(self.fake.steps[0][2]["GOLD_MAX_HOSTS"], 10)
        self.assertEqual(event["gold_onboarding"][0]["steps"],
                         {"gold-onboard": batch.GOLD_SKIPPED_AT_CAP, "gold-promote": "ok"})
        self.assertFalse([a for a in event["alerts"] if a["code"] == "gold_onboarding_failed"])

    def test_drain_and_plans_without_it_run_no_gold_steps(self):
        self.tick(drain=True)
        self.assertEqual(self.fake.steps, [])
        self.config = config()
        event = self.tick()
        self.assertEqual((self.fake.steps, event["gold_onboarding"]), ([], []))

    def test_held_results_report_their_age(self):
        self.tick()
        for job in self.fake.jobs.values():
            job["spool"] = "/project/usnmocr/results/batch1"
        self.fake.hold = True
        self.fake.admits = False
        for at in (60 * 60, 75 * 60):
            with self.assertRaisesRegex(RuntimeError, "trust"):
                self.tick(at=at)
        self.assertEqual(batch.held_results(self.state, set(self.state["held_since"]), NOW + 80 * 60),
                         (2, 20 * 60))
        self.fake.hold = False
        event = self.tick(at=80 * 60)
        self.assertEqual((event["collection_held"], event["oldest_held_seconds"]), (0, 0))
        self.assertEqual(self.state["held_since"], {})

    def test_failure_line_carries_held_age_and_gold_runs(self):
        directory = Path(tempfile.mkdtemp())
        try:
            (directory / "state.json").write_text(json.dumps({
                "config_sha256": "x", "held_since": {"a" * 64: NOW - 600},
                "gold_onboarding": {"v1-tess550/eng/eng": {"at": NOW, "held": True, "steps": {}}}}))
            fake = GoldFake()
            fake.lock = lambda: contextlib.nullcontext()

            def fail(*a, **k):
                raise RuntimeError("handoff/collect failed; inspect the execution privately before retrying")

            fake.inventory = fail
            err = io.StringIO()
            with patch.object(batch, "load_config", return_value=self.config), \
                    patch.object(batch, "digest", return_value="x"), \
                    patch.object(batch, "Transport", return_value=fake), \
                    patch.object(batch.time, "time", return_value=NOW), \
                    contextlib.redirect_stderr(err):
                self.assertEqual(batch.main(["--config", "c.json", "--state-dir", str(directory),
                                             "--apply"]), 1)
            failure = json.loads(err.getvalue())
            self.assertEqual((failure["collection_held"], failure["oldest_held_seconds"]), (1, 600))
            self.assertTrue(failure["gold_onboarding"]["v1-tess550/eng/eng"]["held"])
        finally:
            import shutil
            shutil.rmtree(directory)

    def test_config_validation_and_digest(self):
        directory = Path(tempfile.mkdtemp())
        try:
            path = directory / "config.json"
            plain = config()
            path.write_text(json.dumps(plain))
            before = batch.digest(batch.load_config(path))
            full = {**GOLD, "engine_version": "5.5.0", "max_hosts": 5, "interval_minutes": 30}
            path.write_text(json.dumps({**plain, "gold_onboarding": [full]}))
            loaded = batch.load_config(path)
            # Turning gold onboarding on is not an allocation change.
            self.assertEqual(batch.digest(loaded), before)
            # The bare digest the gold reports and bridge-job use is accepted too.
            path.write_text(json.dumps({**plain, "gold_onboarding": [{**GOLD, "engine_base": "sha256:" + "e" * 64}]}))
            self.assertEqual(batch.load_config(path)["gold_onboarding"][0]["engine_base"], "sha256:" + "e" * 64)
            for bad in (
                [], {}, [dict(GOLD)] * 2, [dict(GOLD)] * 5,
                [{**GOLD, "campaign": "../x"}], [{**GOLD, "language": "en"}],
                [{**GOLD, "engine_base": "docker.io/tesseract:latest"}],
                [{**GOLD, "engine_base": "sha256:" + "c" * 63}],
                [{**GOLD, "engine_base": "sha256:" + "C" * 64}],
                [{**GOLD, "engine_version": "5.5.0;x"}],
                [{**GOLD, "interval_minutes": 5}], [{**GOLD, "max_hosts": 0}],
                [{**GOLD, "max_hosts": True}],
                [{**GOLD, "max_cer": 0.1}], [{**GOLD, "manifest_sha256": "a" * 64}],
                [{**GOLD, "target_host": 7}],
                [{key: value for key, value in GOLD.items() if key != "engine_base"}],
            ):
                with self.subTest(bad=bad):
                    path.write_text(json.dumps({**plain, "gold_onboarding": bad}))
                    with self.assertRaises(ValueError):
                        batch.load_config(path)
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

    def dead_letter(self, doc_ids):
        self.actions.append(("dead-letter", sorted(doc_ids)))

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

    def test_a_page_that_failed_for_good_stops_the_tick_or_is_dead_lettered(self):
        # #1154: without --dead-letter-failed the tick stops and names the page.
        self.tick()
        for job in self.fake.jobs.values():
            job["spool"] = "/project/usnmocr/results/batch1"
        self.fake.terminal = True
        with self.assertRaisesRegex(ValueError, r"terminal BOINC failure: sn12345678/1900-01-01/ed-1/seq-1 .*--dead-letter-failed"):
            self.tick()
        self.assertFalse([a for a in self.fake.actions if a[0] == "dead-letter"])
        # With it, both failed pages go to ocr-deadletter once, and the feed goes on.
        self.fake.terminal = False
        before = len(self.fake.actions)
        event = batch.tick(self.config, self.state, self.fake, lambda: None, NOW, dead_letter_failed=True)
        # Not held: the feed claims and exports pages in their place.
        after = self.fake.actions[before:]
        self.assertTrue([a for a in after if a[0] == "claim"], after)
        self.assertTrue([a for a in after if a[0] == "export"], after)
        letters = [a for a in self.fake.actions if a[0] == "dead-letter"]
        failed_jobs = sorted(row["job"]["id"] for row in self.fake.rows if row["stage"] == "failed")
        self.assertEqual(len(failed_jobs), 2)
        self.assertEqual(letters, [("dead-letter", failed_jobs)])
        self.assertEqual(len(self.state["dead_lettered"]), 2)
        # The dead-lettered pages are not pending.
        self.assertNotIn("sn12345678/1900-01-01/ed-1/seq-1",
                         [page for page, record in batch.owned_records(self.config, self.fake.records()).items()
                          if record["stage"] not in ("published", "failed")])
        self.assertIn("selected", event)
        batch.tick(self.config, self.state, self.fake, lambda: None, NOW, dead_letter_failed=True)
        self.assertEqual(len([a for a in self.fake.actions if a[0] == "dead-letter"]), 1)

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
                           {"source": "other"}, {"claim_languages": []},
                           {"claim_languages": ["en"]}, {"claim_languages": ["eng", "eng"]},
                           {"claim_languages": "eng"}):
                with self.subTest(change=change):
                    path.write_text(json.dumps({**good, **change}))
                    with self.assertRaises((ValueError, KeyError)):
                        batch.load_config(path)
            path.write_text(json.dumps({**good, "claim_languages": ["eng"]}))
            self.assertEqual(batch.load_config(path)["claim_languages"], ["eng"])
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


class BundleTests(unittest.TestCase):
    """#1059: several pages of one queue in one usnm_ocr workunit, when a plan asks for it."""

    def ids(self, n):
        return [f"{i:064x}" for i in range(1, n + 1)]

    def records(self, reasons):
        out = []
        for i, reason in enumerate(reasons, 1):
            r = record(f"sn12345678/1900-01-01/ed-1/seq-{i}", i)
            r["job"]["message"]["reason"] = reason
            out.append(r)
        return out

    def test_config_knob_is_optional_bounded_and_needs_an_app_version(self):
        directory = Path(tempfile.mkdtemp())
        try:
            path = directory / "config.json"
            base = queue_config()
            del base["pages"]
            path.write_text(json.dumps(base))
            plain = batch.load_config(path)
            self.assertNotIn("pages_per_unit", plain)
            good = dict(base, pages_per_unit={"ocr-bulk": 10, "ocr-priority": 1},
                        bundle_app_version=106)
            path.write_text(json.dumps(good))
            loaded = batch.load_config(path)
            self.assertNotEqual(batch.digest(loaded), batch.digest(plain))
            for bad in ({"ocr-bulk": 33}, {"ocr-bulk": 0}, {"elsewhere": 2}, {}, [10]):
                path.write_text(json.dumps(dict(good, pages_per_unit=bad)))
                with self.assertRaises(ValueError):
                    batch.load_config(path)
            missing = dict(good)
            del missing["bundle_app_version"]
            path.write_text(json.dumps(missing))
            with self.assertRaisesRegex(ValueError, "bundle_app_version"):
                batch.load_config(path)
            # All ones: nothing is bundled, so no app version is needed.
            path.write_text(json.dumps(dict(base, pages_per_unit={"ocr-bulk": 1})))
            batch.load_config(path)
        finally:
            import shutil
            shutil.rmtree(directory)

    def test_units_split_by_queue_into_groups_of_at_most_the_size(self):
        records = self.records(["bulk", "user", "bulk", "bulk", "user", "bulk"])
        ids = [r["job"]["id"] for r in records]
        units = batch.units_of({"pages_per_unit": {"ocr-bulk": 3}}, records)
        self.assertEqual(units, [[ids[0], ids[2], ids[3]], [ids[5]], [ids[1]], [ids[4]]])
        self.assertEqual(batch.units_of({}, records), [[i] for i in [ids[0], ids[2], ids[3], ids[5], ids[1], ids[4]]])

    def test_units_keep_one_page_deadline_each(self):
        # #1159: resumed 600 s jobs and new 1800 s jobs never share a bundle.
        records = self.records(["bulk", "bulk", "bulk", "bulk"])
        for record, deadline in zip(records, (600, 1800, 600, 1800)):
            record["job"].setdefault("engine", {})["timeout_secs"] = deadline
        ids = [r["job"]["id"] for r in records]
        units = batch.units_of({"pages_per_unit": {"ocr-bulk": 10}}, records)
        self.assertEqual(units, [[ids[0], ids[2]], [ids[1], ids[3]]])

    def test_bundled_units_go_as_job_units(self):
        cfg = dict(queue_config(), pages_per_unit={"ocr-bulk": 2}, bundle_app_version=106)
        a, b, c = self.ids(3)
        for flag, expected in ((False, [f"{a}+{b}", c]), (True, [f"{a}+{b},{c}"])):
            t = batch.Transport(cfg, Path("."), batch_submit=flag)
            sent = []
            with patch.object(t, "step", side_effect=lambda tg, n, **v: sent.append((tg, n, v))):
                t.submit_batch([a, b, c], [[a, b], [c]])
            self.assertEqual([x[2]["JOB_UNITS"] for x in sent], expected)
            self.assertTrue(all(x[:2] == ("bridge", "submit-remote") and
                                x[2]["BUNDLE_APP_VERSION"] == 106 and "JOB_IDS" not in x[2]
                                for x in sent))
        # Units of one page each: exactly today's path.
        t = batch.Transport(cfg, Path("."))
        sent = []
        with patch.object(t, "step", side_effect=lambda tg, n, **v: sent.append(v)):
            t.submit_batch([a, b], [[a], [b]])
        self.assertEqual([v["JOB_ID"] for v in sent], [a, b])

    def test_a_tick_hands_its_submissions_over_as_units(self):
        class Bundling(QueueFake):
            def submit_batch(self, job_ids, units=None):
                self.units = units
                for job_id in job_ids:
                    self.submit(job_id)

        cfg = dict(queue_config(), pages_per_unit={"ocr-bulk": 2}, bundle_app_version=106,
                   batch_size=4, max_pending=8, pages_per_host=4)
        fake = Bundling([message(n) for n in range(1, 8)])
        fake.assertions = []
        batch.tick(cfg, {}, fake, lambda: None, NOW)
        self.assertEqual([len(u) for u in fake.units], [2, 2])
        self.assertEqual(len(fake.jobs), 4)
        # Without the knob the transport gets no units.
        plain = Bundling([message(n) for n in range(1, 4)])
        plain.assertions = []
        plain.units = "unset"
        batch.tick(queue_config(), {}, plain, lambda: None, NOW)
        self.assertEqual(plain.units, None)


class BatchedStepsTests(unittest.TestCase):
    """#932: a tick's submissions, spool-outs and collects in one execution each, when on."""

    J1, J2 = "a" * 64, "b" * 64
    S1, S2 = "/project/usnmocr/results/1", "/project/usnmocr/results/2"

    def steps(self, transport, call):
        sent = []
        with patch.object(transport, "step", side_effect=lambda t, n, **v: sent.append((t, n, v))):
            call()
        return sent

    def test_off_by_default_one_execution_per_job(self):
        t = batch.Transport(queue_config(), Path("."))
        sent = self.steps(t, lambda: t.submit_batch([self.J1, self.J2]))
        self.assertEqual([(x[0], x[1], x[2]["JOB_ID"]) for x in sent],
                         [("bridge", "submit-remote", self.J1), ("bridge", "submit-remote", self.J2)])
        sent = self.steps(t, lambda: t.collect_batch([(self.J1, self.S1), (self.J2, self.S2)]))
        # Off: the per-job flow, spool-out then collect for each job in turn.
        self.assertEqual([(x[0], x[1], x[2]["JOB_ID"]) for x in sent], [
            ("bridge", "spool-out", self.J1), ("handoff", "collect", self.J1),
            ("bridge", "spool-out", self.J2), ("handoff", "collect", self.J2)])

    def test_off_a_failure_stops_the_per_job_flow_as_before(self):
        t = batch.Transport(queue_config(), Path("."))
        sent = []

        def step(target, name, **v):
            sent.append((target, name, v.get("JOB_ID")))
            if (name, v.get("JOB_ID")) == ("spool-out", self.J1):
                raise RuntimeError("bridge/spool-out failed")

        with patch.object(t, "step", side_effect=step), self.assertRaises(RuntimeError):
            t.collect_batch([(self.J1, self.S1), (self.J2, self.S2)])
        self.assertEqual(sent, [("bridge", "spool-out", self.J1)])

    def test_batched_spool_out_failure_still_collects_every_job_then_fails(self):
        # A failed batch spool-out (B's) must not keep A unpublished: collection runs for all,
        # and the spool-out error is raised afterwards.
        for collect_flag in (True, False):
            with self.subTest(batch_collect=collect_flag):
                t = batch.Transport(queue_config(), Path("."), batch_spool_out=True,
                                    batch_collect=collect_flag)
                sent = []

                def step(target, name, **v):
                    sent.append((target, name, v.get("JOB_ID") or v.get("JOB_IDS")))
                    if name == "spool-out":
                        raise RuntimeError("bridge/spool-out failed")

                with patch.object(t, "step", side_effect=step), \
                        self.assertRaisesRegex(RuntimeError, "spool-out"):
                    t.collect_batch([(self.J1, self.S1), (self.J2, self.S2)])
                collects = [x for x in sent if x[1] == "collect"]
                if collect_flag:
                    self.assertEqual(collects, [("handoff", "collect", f"{self.J1},{self.J2}")])
                else:
                    self.assertEqual(collects, [("handoff", "collect", self.J1), ("handoff", "collect", self.J2)])

    def test_per_job_collect_goes_on_past_a_failure_when_spool_out_is_batched(self):
        t = batch.Transport(queue_config(), Path("."), batch_spool_out=True)
        sent = []

        def step(target, name, **v):
            sent.append((name, v.get("JOB_ID")))
            if (name, v.get("JOB_ID")) == ("collect", self.J1):
                raise RuntimeError("handoff/collect failed")

        with patch.object(t, "step", side_effect=step), self.assertRaisesRegex(RuntimeError, "collect"):
            t.collect_batch([(self.J1, self.S1), (self.J2, self.S2)])
        self.assertEqual([x for x in sent if x[0] == "collect"], [("collect", self.J1), ("collect", self.J2)])

    def test_batched_one_execution_per_step(self):
        t = batch.Transport(queue_config(), Path("."), batch_submit=True, batch_spool_out=True,
                            batch_collect=True)
        [(target, name, v)] = self.steps(t, lambda: t.submit_batch([self.J1, self.J2]))
        self.assertEqual((target, name, v["JOB_IDS"]), ("bridge", "submit-remote", f"{self.J1},{self.J2}"))
        self.assertNotIn("JOB_ID", v)
        sent = self.steps(t, lambda: t.collect_batch([(self.J1, self.S1), (self.J2, self.S2)]))
        self.assertEqual([(x[0], x[1]) for x in sent], [("bridge", "spool-out"), ("handoff", "collect")])
        self.assertEqual(sent[0][2]["HANDOFF_SPOOLS"], f"{self.J1}={self.S1},{self.J2}={self.S2}")
        self.assertEqual(sent[1][2]["JOB_IDS"], f"{self.J1},{self.J2}")

    def test_spool_out_and_collect_switch_separately(self):
        # The bridge and hand-off images deploy on different stacks: either can go first.
        t = batch.Transport(queue_config(), Path("."), batch_spool_out=True)
        sent = self.steps(t, lambda: t.collect_batch([(self.J1, self.S1), (self.J2, self.S2)]))
        self.assertEqual([(x[0], x[1]) for x in sent],
                         [("bridge", "spool-out"), ("handoff", "collect"), ("handoff", "collect")])

    def test_empty_batches_start_nothing(self):
        t = batch.Transport(queue_config(), Path("."), batch_submit=True, batch_spool_out=True,
                            batch_collect=True)
        self.assertEqual(self.steps(t, lambda: (t.submit_batch([]), t.collect_batch([]))), [])

    def test_flags_reach_the_transport(self):
        seen = {}

        def capture(*a, **k):
            seen.update(k)
            raise SystemExit(0)

        with patch.object(batch, "load_config", return_value=queue_config()), \
                patch.object(batch, "Transport", side_effect=capture), \
                patch.object(batch, "digest", return_value="x"), \
                tempfile.TemporaryDirectory() as d:
            (Path(d) / "state.json").write_text(json.dumps({"config_sha256": "x"}))
            with self.assertRaises(SystemExit):
                batch.main(["--config", "c.json", "--state-dir", d, "--apply",
                            "--batch-submit", "--batch-collect"])
        self.assertEqual((seen["batch_submit"], seen["batch_spool_out"], seen["batch_collect"]),
                         (True, False, True))

    def test_batch_export_sends_every_batch_in_one_execution(self):
        pages = [{"page": "sn1/1890-05-03/ed-1/seq-1", "batch": "dlc_bbb", "scan": "/tank/ocr/scans/keep/dlc_bbb/sn1/1890-05-03/1/1.jp2"},
                 {"page": "sn2/1890-05-03/ed-1/seq-1", "batch": "dlc_aaa", "scan": "/tank/ocr/scans/keep/dlc_aaa/sn2/1890-05-03/1/1.jp2"},
                 {"page": "sn1/1890-05-03/ed-1/seq-2", "batch": "dlc_bbb", "scan": "/tank/ocr/scans/keep/dlc_bbb/sn1/1890-05-03/1/2.jp2"}]
        for flag, calls in ((False, 2), (True, 1)):
            with self.subTest(batch_export=flag):
                cfg = queue_config()
                cfg["scans_root"] = "/tank/ocr/scans"
                t = batch.Transport(cfg, Path("."), batch_export=flag)
                with patch.object(t, "step") as step:
                    t.export(pages, ["a" * 64])
                self.assertEqual(step.call_count, calls)
                if flag:
                    sent = step.call_args.kwargs
                    self.assertNotIn("HANDOFF_BATCH", sent)
                    self.assertEqual(sent["HANDOFF_BATCHES"], "dlc_aaa,dlc_bbb,dlc_bbb")
                    self.assertEqual(sent["HANDOFF_PAGES"].split(",")[0], "sn2/1890-05-03/ed-1/seq-1")
                    self.assertEqual(sent["HANDOFF_SCANS"].split(",")[0], "keep/dlc_aaa/sn2/1890-05-03/1/1.jp2")
                    self.assertEqual(sent["HANDOFF_CLAIMS"], "1")
                else:
                    self.assertEqual([c.kwargs["HANDOFF_BATCH"] for c in step.call_args_list], ["dlc_aaa", "dlc_bbb"])
        # More batches than one execution takes go in several.
        many = [{"page": f"sn1/1890-05-03/ed-1/seq-{i}", "batch": f"dlc_b{i:03d}",
                 "scan": f"/tank/ocr/scans/keep/dlc_b{i:03d}/sn1/1890-05-03/1/{i}.jp2"}
                for i in range(1, batch.EXPORT_BATCHES * 2 + 2)]
        cfg = queue_config()
        cfg["scans_root"] = "/tank/ocr/scans"
        t = batch.Transport(cfg, Path("."), batch_export=True)
        with patch.object(t, "step") as step:
            t.export(many, [])
        self.assertEqual(step.call_count, 3)
        self.assertEqual(sum(len(c.kwargs["HANDOFF_BATCHES"].split(",")) for c in step.call_args_list), len(many))
        # One batch needs no HANDOFF_BATCHES, so an older hand-off image still takes it.
        t = batch.Transport(queue_config(), Path("."), batch_export=True)
        with patch.object(t, "step") as step:
            t.export(pages[:1], [])
        self.assertEqual(step.call_args.kwargs["HANDOFF_BATCH"], "dlc_bbb")


class Ticker:
    """A clock and a CPU counter for Timings that move only when told to."""

    def __init__(self):
        self.now = 0.0
        self.used = 0.0

    def clock(self):
        return self.now

    def cpu(self):
        return self.used

    def timings(self, report=None):
        return batch.Timings(report=report, clock=self.clock, cpu=self.cpu)


class TimingsTests(unittest.TestCase):
    """#902: a tick's line says where its wall time and CPU went."""

    J1, J2 = "a" * 64, "b" * 64
    S1, S2 = "/project/usnmocr/results/1", "/project/usnmocr/results/2"

    def test_execution_routes_follow_actual_launcher_and_retain_failures(self):
        directory = Path(self.enterContext(tempfile.TemporaryDirectory()))
        cfg = queue_config()
        cfg["step_command"] = ["ssh", "boinc.example"]
        transport = batch.Transport(cfg, directory, onprem_handoff=("claim", "export"))
        with patch.object(batch.subprocess, "run",
                          return_value=subprocess.CompletedProcess([], 0, "", "")):
            transport.step("handoff", "claim")
            transport.step("handoff", "collect")
            transport.step("bridge", "batch-inventory")
        with patch.object(batch.subprocess, "run", side_effect=OSError("cannot start")), \
                self.assertRaises(RuntimeError):
            transport.step("handoff", "export")
        routes = transport.timings.summary()["execution_routes"]
        self.assertEqual(set(routes), {
            "onprem/handoff/claim", "azure/handoff/collect",
            "onprem/bridge/batch-inventory", "onprem/handoff/export",
        })
        self.assertEqual(routes["onprem/handoff/export"]["failed"], 1)
        routes["onprem/handoff/claim"]["count"] = 99
        self.assertEqual(transport.timings.summary()["execution_routes"]
                         ["onprem/handoff/claim"]["count"], 1)
        # A bridge without a custom launcher still uses Azure.
        del cfg["step_command"]
        transport = batch.Transport(cfg, directory)
        with patch.object(batch.subprocess, "run",
                          return_value=subprocess.CompletedProcess([], 0, "", "")):
            transport.step("bridge", "batch-inventory")
        self.assertIn("azure/bridge/batch-inventory",
                      transport.timings.summary()["execution_routes"])

    def test_scan_route_records_the_actual_queue_destination(self):
        for choices, route in (((), "azure/scans"), (("export",), "onprem/scans")):
            with self.subTest(choices=choices):
                cfg = queue_config()
                cfg["step_command"] = ["ssh", "boinc.example"]
                transport = batch.Transport(cfg, Path("."), onprem_handoff=choices)
                base = "keep/batch/sn1/1900-01-01/1/1"
                done = subprocess.CompletedProcess([], 0, f"pushed {base}.jpg\n", "")
                with patch.object(batch.subprocess, "run", return_value=done):
                    self.assertEqual(transport.push_pages([base]), ({base: base + ".jpg"}, []))
                self.assertEqual(set(transport.timings.summary()["execution_routes"]), {route})
                self.assertEqual(transport.timings.summary()["execution_routes"][route]["count"], 1)

    def test_phases_add_up_and_report_as_they_end(self):
        ticker, lines = Ticker(), []
        timings = ticker.timings(report=lines.append)
        for seconds in (30, 40):
            with timings.phase("collect"):
                ticker.now += seconds
                ticker.used += 2
                timings.command("handoff/collect", seconds, 2, True)
        with self.assertRaises(RuntimeError), timings.phase("export"):
            ticker.now += 5
            timings.command("handoff/export", 5, 1, False)
            raise RuntimeError("handoff/export failed")
        summary = timings.summary()
        self.assertEqual(summary["tick_seconds"], 75)
        self.assertEqual(summary["cpu_seconds"], 4)
        self.assertEqual(summary["phases"]["collect"],
                         {"count": 2, "failed": 0, "seconds": 70, "cpu_seconds": 4})
        self.assertEqual(summary["phases"]["export"]["failed"], 1)
        self.assertEqual(summary["commands"]["handoff/collect"]["count"], 2)
        self.assertEqual(summary["commands"]["handoff/export"]["failed"], 1)
        self.assertEqual([(line["event"], line["phase"], line["ok"], line["seconds"], line["commands"])
                          for line in lines],
                         [("boinc_batch_phase", "collect", True, 30, 1),
                          ("boinc_batch_phase", "collect", True, 40, 1),
                          ("boinc_batch_phase", "export", False, 5, 1)])
        # The summary is a copy: a later phase doesn't change a line already printed.
        summary["phases"]["collect"]["count"] = 99
        self.assertEqual(timings.summary()["phases"]["collect"]["count"], 2)

    def test_transport_counts_each_command_and_the_outer_phase_once(self):
        ticker = Ticker()
        directory = Path(self.enterContext(tempfile.TemporaryDirectory()))
        t = batch.Transport(queue_config(), directory, timings=ticker.timings())
        calls = []

        def run(argv, **kwargs):
            calls.append(argv)
            ticker.now += 70
            ticker.used += 3
            failed = "handoff" in argv and f"JOB_ID={self.J2}" in argv
            return subprocess.CompletedProcess(argv, 1 if failed else 0, "", "")

        with patch.object(batch.subprocess, "run", side_effect=run), \
                self.assertRaisesRegex(RuntimeError, "handoff/collect"):
            t.collect_batch([(self.J1, self.S1), (self.J2, self.S2)])
        summary = t.timings.summary()
        # Off: spool-out then collect per job, stopping at J2's failed collect. The nested
        # per-job collect() calls belong to the one collect phase.
        self.assertEqual(summary["phases"], {"collect": {"count": 1, "failed": 1, "seconds": 280,
                                                         "cpu_seconds": 12}})
        self.assertEqual(summary["commands"], {
            "bridge/spool-out": {"count": 2, "failed": 0, "seconds": 140, "cpu_seconds": 6},
            "handoff/collect": {"count": 2, "failed": 1, "seconds": 140, "cpu_seconds": 6},
        })

    def test_storage_calls_go_by_their_az_verb(self):
        ticker = Ticker()
        t = batch.Transport(queue_config(), Path("."), timings=ticker.timings())
        with patch.object(batch.subprocess, "run",
                          side_effect=lambda argv, **k: subprocess.CompletedProcess(argv, 0, "true", "")):
            with t.lock():
                pass
        summary = t.timings.summary()
        self.assertEqual({kind: entry["count"] for kind, entry in summary["commands"].items()},
                         {"storage exists": 1, "storage lease": 2})
        self.assertEqual(set(summary["phases"]), {"lock", "unlock"})

    def test_a_command_that_cannot_start_is_counted_as_failed(self):
        directory = Path(self.enterContext(tempfile.TemporaryDirectory()))
        t = batch.Transport(queue_config(), directory, timings=Ticker().timings())
        with patch.object(batch.subprocess, "run", side_effect=OSError("no az")), \
                self.assertRaisesRegex(RuntimeError, "hand-off storage"):
            t.az("exists", "--name", "x")
        self.assertEqual(t.timings.summary()["commands"]["storage exists"]["failed"], 1)

    def test_command_failure_receipt_is_private_bounded_and_not_in_public_error(self):
        directory = Path(self.enterContext(tempfile.TemporaryDirectory()))
        t = batch.Transport(queue_config(), directory)
        secret = "private-command-output"
        failed = subprocess.CompletedProcess(["secret-argv"], 17, "x" * 20000, secret)
        with patch.object(batch.subprocess, "run", return_value=failed), \
                self.assertRaises(RuntimeError) as caught:
            t.run(["secret-argv"], "handoff/collect")
        path = directory / "last-command-failure.json"
        receipt = json.loads(path.read_text())
        self.assertEqual(path.stat().st_mode & 0o777, 0o600)
        self.assertEqual(receipt["returncode"], 17)
        self.assertEqual(receipt["stderr_tail"], secret)
        self.assertEqual(len(receipt["stdout_tail"]), batch.MAX_FAILURE_OUTPUT_CHARS)
        self.assertNotIn(secret, str(caught.exception))
        self.assertNotIn("secret-argv", path.read_text())
        with patch.object(batch.subprocess, "run", return_value=subprocess.CompletedProcess([], 0, "ok", "")):
            self.assertEqual(t.run([], "success"), "ok")
        self.assertEqual(json.loads(path.read_text()), receipt)

    def test_timeout_keeps_partial_bytes_and_atomically_replaces_previous_receipt(self):
        directory = Path(self.enterContext(tempfile.TemporaryDirectory()))
        path = directory / "last-command-failure.json"
        path.write_text("previous")
        path.chmod(0o644)
        t = batch.Transport(queue_config(), directory)
        failure = subprocess.TimeoutExpired(["private-argv"], 30, output=b"partial", stderr=b"timeout\xff")
        with patch.object(batch.subprocess, "run", side_effect=failure), \
                self.assertRaisesRegex(RuntimeError, "bridge/batch-inventory failed"):
            t.run(["private-argv"], "bridge/batch-inventory")
        receipt = json.loads(path.read_text())
        self.assertEqual(receipt["exception"], "TimeoutExpired")
        self.assertEqual(receipt["stdout_tail"], "partial")
        self.assertEqual(receipt["stderr_tail"], "timeout\ufffd")
        self.assertEqual(path.stat().st_mode & 0o777, 0o600)
        self.assertEqual(list(directory.glob(".command-failure-*")), [])

    def test_nonzero_child_with_invalid_utf8_retains_output(self):
        directory = Path(self.enterContext(tempfile.TemporaryDirectory()))
        t = batch.Transport(queue_config(), directory)
        with self.assertRaisesRegex(RuntimeError, "handoff/collect failed"):
            t.run([sys.executable, "-c",
                   "import os; os.write(1,b'out\\xff'); os.write(2,b'err\\xfe'); raise SystemExit(7)"],
                  "handoff/collect")
        receipt = json.loads((directory / "last-command-failure.json").read_text())
        self.assertEqual(receipt["returncode"], 7)
        self.assertEqual(receipt["stdout_tail"], "out\ufffd")
        self.assertEqual(receipt["stderr_tail"], "err\ufffd")

    def test_batch_keeps_collecting_and_reports_later_receipt_write_failure(self):
        directory = Path(self.enterContext(tempfile.TemporaryDirectory()))
        t = batch.Transport(queue_config(), directory, batch_spool_out=True)
        calls = []

        def run(argv, **kwargs):
            calls.append(argv)
            return subprocess.CompletedProcess(argv, 1, "", "private-output")

        replace = batch.os.replace
        replacements = []

        def replace_receipt(source, target):
            replacements.append(target)
            if len(replacements) > 1:
                raise OSError("private-path")
            return replace(source, target)

        with patch.object(batch.subprocess, "run", side_effect=run), \
                patch.object(batch.os, "replace", side_effect=replace_receipt), \
                self.assertRaises(batch.FailureReceiptError) as caught:
            t.collect_batch([(self.J1, self.S1), (self.J2, self.S2)])
        self.assertEqual(len(calls), 3)
        self.assertIn("bridge/spool-out failed", str(caught.exception))
        self.assertIn("handoff/collect failed; private command failure receipt could not be saved",
                      str(caught.exception))
        self.assertNotIn("private-output", str(caught.exception))
        receipt = json.loads((directory / "last-command-failure.json").read_text())
        self.assertEqual(receipt["label"], "bridge/spool-out")
        self.assertEqual(list(directory.glob(".command-failure-*")), [])

    def test_receipt_write_failure_is_explicit_and_does_not_expose_output(self):
        directory = Path(self.enterContext(tempfile.TemporaryDirectory()))
        t = batch.Transport(queue_config(), directory)
        failed = subprocess.CompletedProcess([], 1, "", "private-output")
        with patch.object(batch.subprocess, "run", return_value=failed), \
                patch.object(batch.os, "replace", side_effect=OSError("private-path")), \
                self.assertRaisesRegex(RuntimeError, "receipt could not be saved") as caught:
            t.run([], "handoff/collect")
        self.assertNotIn("private", str(caught.exception).replace("private command", ""))
        self.assertEqual(list(directory.glob(".command-failure-*")), [])

    def test_receipt_cleanup_failure_still_allows_remaining_batch_collection(self):
        directory = Path(self.enterContext(tempfile.TemporaryDirectory()))
        t = batch.Transport(queue_config(), directory, batch_spool_out=True)
        failed = subprocess.CompletedProcess([], 1, "", "private-output")
        unlink = Path.unlink
        leftover = []

        def refuse_cleanup(path, **kwargs):
            leftover.append(path)
            raise OSError("private-path")

        with patch.object(batch.subprocess, "run", return_value=failed) as run, \
                patch.object(batch.os, "replace", side_effect=OSError("private-path")), \
                patch.object(Path, "unlink", refuse_cleanup), \
                self.assertRaises(batch.FailureReceiptError) as caught:
            t.collect_batch([(self.J1, self.S1), (self.J2, self.S2)])
        self.assertEqual(run.call_count, 3)
        self.assertIn("receipt cleanup failed", str(caught.exception))
        self.assertNotIn("private-path", str(caught.exception))
        self.assertNotIn("private-output", str(caught.exception))
        for path in leftover:
            unlink(path)

    def test_failed_lease_release_preserves_collection_receipt_loss_warning(self):
        directory = Path(self.enterContext(tempfile.TemporaryDirectory()))
        t = batch.Transport(queue_config(), directory)
        replacements = []
        replace = batch.os.replace

        def replace_receipt(source, target):
            replacements.append(target)
            if len(replacements) == 1:
                raise OSError("private-path")
            return replace(source, target)

        with patch.object(t, "_acquire", return_value="lease"), \
                patch.object(batch.subprocess, "run",
                             return_value=subprocess.CompletedProcess([], 1, "", "private-output")) as run, \
                patch.object(batch.os, "replace", side_effect=replace_receipt), \
                self.assertRaises(batch.FailureReceiptError) as caught:
            with t.lock():
                t.run([], "handoff/collect")
        self.assertEqual(run.call_count, 2)
        self.assertIn("handoff/collect failed; private command failure receipt could not be saved",
                      str(caught.exception))
        self.assertIn("hand-off storage failed", str(caught.exception))
        self.assertNotIn("private-output", str(caught.exception))
        receipt = json.loads((directory / "last-command-failure.json").read_text())
        self.assertEqual(receipt["label"], "hand-off storage")

    def test_the_tick_line_carries_the_timings(self):
        fake = Fake()
        self.assertNotIn("timings", batch.tick(config(), {}, fake, lambda: None, NOW))
        fake = Fake()
        fake.timings = Ticker().timings()
        fake.timings.command("handoff/export", 50, 1, True)
        event = batch.tick(config(), {}, fake, lambda: None, NOW)
        self.assertEqual(event["timings"]["commands"]["handoff/export"]["count"], 1)

    def test_a_broken_log_stream_never_strands_the_lease(self):
        def broken(line):
            raise BrokenPipeError()

        t = batch.Transport(queue_config(), Path("."), timings=Ticker().timings(report=broken))
        with patch.object(t, "az", return_value="true") as az:
            with t.lock():
                pass
        self.assertEqual(az.call_args.args[:2], ("lease", "release"))
        # A failing tick still releases.
        with patch.object(t, "_acquire", return_value="lease-id"), \
                patch.object(t, "az", return_value="") as az, self.assertRaises(RuntimeError):
            with t.lock():
                raise RuntimeError("tick failed")
        self.assertEqual(az.call_args.args[:2], ("lease", "release"))

    def test_a_successful_line_counts_the_lease_release(self):
        out, err = io.StringIO(), io.StringIO()
        with patch.object(batch, "load_config", return_value=queue_config()), \
                patch.object(batch, "digest", return_value="x"), \
                patch.object(batch.Transport, "az", return_value="true"), \
                patch.object(batch, "tick", return_value={"event": "boinc_batch_tick"}), \
                tempfile.TemporaryDirectory() as d:
            (Path(d) / "state.json").write_text(json.dumps({"config_sha256": "x"}))
            with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
                self.assertEqual(batch.main(["--config", "c.json", "--state-dir", d, "--apply"]), 0)
        timings = json.loads(out.getvalue())["timings"]
        self.assertEqual(set(timings["phases"]), {"lock", "unlock"})
        phases = [json.loads(line)["phase"] for line in err.getvalue().splitlines()]
        self.assertEqual(phases, ["lock", "unlock"])

    def test_main_prints_phases_and_a_failed_tick_says_how_far_it_got(self):
        class Failing:
            def __init__(self, timings):
                self.timings = timings

            @contextlib.contextmanager
            def lock(self):
                with self.timings.phase("collect"):
                    self.timings.command("handoff/collect", 70, 3, False)
                    raise RuntimeError("handoff/collect failed; inspect the execution privately before retrying")
                yield

        with patch.object(batch, "load_config", return_value=queue_config()), \
                patch.object(batch, "Transport", side_effect=lambda *a, **k: Failing(k["timings"])), \
                patch.object(batch, "digest", return_value="x"), \
                tempfile.TemporaryDirectory() as d:
            (Path(d) / "state.json").write_text(json.dumps({"config_sha256": "x"}))
            err = io.StringIO()
            with contextlib.redirect_stderr(err):
                self.assertEqual(batch.main(["--config", "c.json", "--state-dir", d, "--apply"]), 1)
        phase, failure = [json.loads(line) for line in err.getvalue().splitlines()]
        self.assertEqual((phase["event"], phase["phase"], phase["ok"], phase["commands"]),
                         ("boinc_batch_phase", "collect", False, 1))
        self.assertEqual(failure["event"], "boinc_batch_failure")
        self.assertIn("handoff/collect failed", failure["error"])
        self.assertEqual(failure["timings"]["commands"]["handoff/collect"]["failed"], 1)


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
        self.assertNotIn("CLAIM_LANGUAGES", sent)
        with patch.object(self.transport, "step"), \
                patch.object(self.transport, "blob", return_value={**summary, "nonce": "other"}), \
                self.assertRaisesRegex(ValueError, "stale/concurrent"):
            self.transport.claim(1)

    def test_claim_recovery_preserves_later_receipt_write_warning(self):
        directory = Path(self.enterContext(tempfile.TemporaryDirectory()))
        transport = batch.Transport(self.config, directory)
        failed = subprocess.CompletedProcess([], 1, "", "private-output")
        replace = batch.os.replace
        replacements = []

        def replace_receipt(source, target):
            replacements.append(target)
            if len(replacements) > 1:
                raise OSError("private-path")
            return replace(source, target)

        with patch.object(batch.subprocess, "run", return_value=failed) as run, \
                patch.object(batch.os, "replace", side_effect=replace_receipt), \
                self.assertRaises(batch.FailureReceiptError) as caught:
            transport.claim(1)
        self.assertEqual(run.call_count, 2)
        self.assertIn("handoff/claim failed", str(caught.exception))
        self.assertIn("receipt could not be saved", str(caught.exception))
        self.assertNotIn("private-output", str(caught.exception))
        receipt = json.loads((directory / "last-command-failure.json").read_text())
        self.assertEqual(receipt["label"], "handoff/claim")

    def test_claim_languages_reach_the_claim_step(self):
        sent = {}
        self.transport.config["claim_languages"] = ["eng", "fra"]

        def step(target, name, **variables):
            sent.update(variables)

        def blob(name):
            return {"schema_version": 1, "nonce": sent["CLAIM_NONCE"], "claims": []}

        with patch.object(self.transport, "step", side_effect=step), \
                patch.object(self.transport, "blob", side_effect=blob):
            self.assertEqual(self.transport.claim(2), [])
        self.assertEqual(sent["CLAIM_LANGUAGES"], "eng,fra")

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

    def test_steps_use_the_azure_launcher_by_default(self):
        with patch.object(self.transport, "run", return_value="") as run:
            self.transport.step("bridge", "submit", JOB_ID="a" * 64)
        argv = run.call_args.args[0]
        self.assertEqual(argv[:2], ["bash", str(batch.REPO / "scripts/boinc/start-boinc-bridge.sh")])
        self.assertEqual(argv[2:], ["--env", "dev", "--target", "bridge", "--step", "submit",
                                    "--wait-mins", "30", "--var", "JOB_ID=" + "a" * 64])

    def test_step_command_replaces_the_launcher(self):
        cfg = copy.deepcopy(self.config)
        cfg["step_command"] = ["ssh", "-o", "BatchMode=yes", "usnm-boinc-step@boinc.example"]
        transport = batch.Transport(cfg, Path("."))
        with patch.object(transport, "run", return_value="") as run:
            transport.step("bridge", "submit", JOB_ID="a" * 64)
        argv = run.call_args.args[0]
        self.assertEqual(argv[:4], cfg["step_command"])
        self.assertEqual(argv[4:8], ["--env", "dev", "--target", "bridge"])

    def test_handoff_steps_keep_the_azure_launcher_with_step_command(self):
        cfg = copy.deepcopy(self.config)
        cfg["step_command"] = ["ssh", "-o", "BatchMode=yes", "usnm-boinc-step@boinc.example"]
        transport = batch.Transport(cfg, Path("."))
        with patch.object(transport, "run", return_value="") as run:
            transport.step("handoff", "collect", JOB_IDS="a" * 64)
            transport.release(["a_b"])
        for call in run.call_args_list:
            argv = call.args[0]
            self.assertEqual(argv[:2], ["bash", str(batch.REPO / "scripts/boinc/start-boinc-bridge.sh")])
            self.assertIn("handoff", argv)

    def test_onprem_handoff_sends_the_chosen_steps_through_step_command(self):
        cfg = copy.deepcopy(self.config)
        cfg["step_command"] = ["ssh", "-o", "BatchMode=yes", "usnm-boinc-step@boinc.example"]
        launcher = ["bash", str(batch.REPO / "scripts/boinc/start-boinc-bridge.sh")]
        for choice, onprem in (((), set()), (("claim",), {"claim", "release"}), (("collect",), {"collect"}),
                               (("claim", "collect"), {"claim", "release", "collect"}),
                               (("export",), {"export"}),
                               (("claim", "collect", "export"), {"claim", "release", "collect", "export"})):
            with self.subTest(choice=choice):
                transport = batch.Transport(cfg, Path("."), onprem_handoff=choice)
                with patch.object(transport, "run", return_value="") as run:
                    for step in ("claim", "release", "collect", "export"):
                        transport.step("handoff", step, JOB_ID="a" * 64)
                for call, step in zip(run.call_args_list, ("claim", "release", "collect", "export")):
                    argv = call.args[0]
                    expected = cfg["step_command"] if step in onprem else launcher
                    self.assertEqual(argv[:len(expected)], expected)
                    self.assertEqual(argv[len(expected):len(expected) + 6],
                                     ["--env", "dev", "--target", "handoff", "--step", step])

    def test_onprem_export_pushes_scans_to_the_boinc_host(self):
        cfg = queue_config()
        cfg["step_command"] = ["ssh", "-i", "/root/.ssh/k", "root@boinc.example", "--"]
        plain = batch.Transport(cfg, Path("."), onprem_handoff=("claim", "collect")).push_args()
        self.assertNotIn("--to-onprem", plain)
        argv = batch.Transport(cfg, Path("."), onprem_handoff=("claim", "collect", "export")).push_args()
        self.assertEqual(argv[:len(plain)], plain)
        rest = argv[len(plain):]
        self.assertEqual(rest[:3], ["--to-onprem", "--onprem-env", cfg["env"]])
        self.assertEqual(rest[3::2], ["--onprem-step-arg"] * len(cfg["step_command"]))
        self.assertEqual(rest[4::2], cfg["step_command"])

    def test_export_restages_scans_pushed_to_another_destination(self):
        # A page pushed to the container before export moved on-prem is pushed again first.
        cfg = queue_config()
        cfg["step_command"] = ["ssh", "root@boinc.example", "--"]
        cfg["scans_root"] = "/tank/ocr/scans"
        t = batch.Transport(cfg, Path("."), onprem_handoff=("export",))
        pages = [{"page": "sn1/1890-05-03/ed-1/seq-1", "batch": "dlc_aaa", "pushed_to": "container",
                  "scan_base": "keep/dlc_aaa/sn1/1890-05-03/1/1",
                  "scan": "/tank/ocr/scans/keep/dlc_aaa/sn1/1890-05-03/1/1.jp2"},
                 {"page": "sn1/1890-05-03/ed-1/seq-2", "batch": "dlc_aaa", "pushed_to": "onprem",
                  "scan_base": "keep/dlc_aaa/sn1/1890-05-03/1/2",
                  "scan": "/tank/ocr/scans/keep/dlc_aaa/sn1/1890-05-03/1/2.jp2"}]
        found = {"keep/dlc_aaa/sn1/1890-05-03/1/1": "keep/dlc_aaa/sn1/1890-05-03/1/1.jpg"}
        with patch.object(t, "push_pages", return_value=(found, [])) as push, patch.object(t, "step") as step:
            t.export(pages, [])
        push.assert_called_once_with(["keep/dlc_aaa/sn1/1890-05-03/1/1"])
        # The export asks for the format the push found now.
        self.assertEqual(step.call_args.kwargs["HANDOFF_SCANS"],
                         "keep/dlc_aaa/sn1/1890-05-03/1/1.jpg,keep/dlc_aaa/sn1/1890-05-03/1/2.jp2")
        with patch.object(t, "push_pages", return_value=({}, ["keep/dlc_aaa/sn1/1890-05-03/1/1"])), \
                patch.object(t, "step") as step:
            with self.assertRaisesRegex(ValueError, "gone from the scan server"):
                t.export(pages, [])
            step.assert_not_called()

    def test_onprem_handoff_needs_step_command_and_known_steps(self):
        with self.assertRaisesRegex(ValueError, "step_command"):
            batch.Transport(copy.deepcopy(self.config), Path("."), onprem_handoff=("claim",))
        cfg = copy.deepcopy(self.config)
        cfg["step_command"] = ["ssh", "usnm-boinc-step@boinc.example"]
        with self.assertRaisesRegex(ValueError, "claim, collect and/or export"):
            batch.Transport(cfg, Path("."), onprem_handoff=("texts-export",))

    def test_main_onprem_handoff_is_off_by_default_and_checked(self):
        cfg = config()
        with tempfile.NamedTemporaryFile("w", suffix=".json") as f, \
                tempfile.TemporaryDirectory() as state:
            json.dump(cfg, f); f.flush()
            out = io.StringIO()
            with contextlib.redirect_stdout(out):
                self.assertEqual(batch.main(["--config", f.name, "--state-dir", state]), 0)
            self.assertEqual(json.loads(out.getvalue())["onprem_handoff"], [])
            err = io.StringIO()
            with contextlib.redirect_stderr(err):
                self.assertEqual(batch.main(["--config", f.name, "--state-dir", state,
                                             "--onprem-handoff", "claim"]), 1)
            self.assertIn("step_command", err.getvalue())
        cfg["step_command"] = ["ssh", "usnm-boinc-step@boinc.example"]
        with tempfile.NamedTemporaryFile("w", suffix=".json") as f, \
                tempfile.TemporaryDirectory() as state:
            json.dump(cfg, f); f.flush()
            out = io.StringIO()
            with contextlib.redirect_stdout(out):
                self.assertEqual(batch.main(["--config", f.name, "--state-dir", state,
                                             "--onprem-handoff", "collect", "--onprem-handoff", "claim"]), 0)
            self.assertEqual(json.loads(out.getvalue())["onprem_handoff"], ["claim", "collect"])

    def test_step_command_must_be_literal(self):
        for bad in ([], "ssh host", ["ssh", "host; rm -rf /"], ["ssh", "$(id)"]):
            cfg = config()
            cfg["step_command"] = bad
            with tempfile.NamedTemporaryFile("w", suffix=".json") as f:
                json.dump(cfg, f); f.flush()
                with self.assertRaisesRegex(ValueError, "step_command"):
                    batch.load_config(Path(f.name))

    def test_release_step(self):
        with patch.object(self.transport, "run", return_value="") as run:
            self.transport.release(["a_b", "c_d"])
        argv = run.call_args.args[0]
        self.assertIn("release", argv)
        self.assertIn("HANDOFF_DOCS=a_b,c_d", argv)


def status_xml(ocr_unsent=0, ocr_in_progress=0, qwen_unsent=456, quality_unsent=24,
               quality_in_progress=31, train_unsent=2, zero_work=None):
    ready = ocr_unsent + qwen_unsent + quality_unsent + train_unsent
    in_prog = ocr_in_progress + quality_in_progress
    extra = f"<zero_work_requests>{zero_work}</zero_work_requests>" if zero_work is not None else ""
    return f"""<server_status>
  <database_file_states>
    <results_ready_to_send>{ready}</results_ready_to_send>
    <results_in_progress>{in_prog}</results_in_progress>
    {extra}
    <tasks_by_app>
      <app><name>usnm_iris</name><unsent>0</unsent><in_progress>0</in_progress></app>
      <app><name>usnm_ocr</name><unsent>{ocr_unsent}</unsent><in_progress>{ocr_in_progress}</in_progress></app>
      <app><name>usnm_assess</name><unsent>0</unsent><in_progress>0</in_progress></app>
      <app><name>usnm_surya</name><unsent>0</unsent><in_progress>0</in_progress></app>
      <app><name>usnm_ndlocr</name><unsent>0</unsent><in_progress>0</in_progress></app>
      <app><name>usnm_qwen</name><unsent>{qwen_unsent}</unsent><in_progress>0</in_progress></app>
      <app><name>usnm_quality</name><unsent>{quality_unsent}</unsent><in_progress>{quality_in_progress}</in_progress></app>
      <app><name>usnm_qwen_train</name><unsent>{train_unsent}</unsent><in_progress>0</in_progress></app>
    </tasks_by_app>
  </database_file_states>
</server_status>"""


class CohortWatermarksAndStarvationTests(unittest.TestCase):
    def setUp(self):
        self.config = config()
        self.config["pages"] = [
            {
                "page": f"sn12345678/1900-01-01/ed-1/seq-{number}",
                "scan": f"/tank/ocr/scans/keep/test_batch/sn12345678/1900-01-01/ed-1/seq-{number}.jp2",
            }
            for number in range(1, 9)
        ]
        self.state = {}
        self.fake = Fake()
        self.saved = []

    def tick(self, **kwargs):
        return batch.tick(
            self.config, self.state, self.fake,
            lambda: self.saved.append(copy.deepcopy(self.state)), NOW, **kwargs,
        )

    def test_bundled_pages_count_as_pages_for_ocr_and_as_workunits_globally(self):
        # #1059: four 10-page bundles and 20 other-app workunits are 24 workunits on the
        # server (under a global cap of 30) and 40 OCR pages in flight.
        self.fake.other = 20
        self.fake.extra_pending = {f"{n:064x}" for n in range(1000, 1040)}
        base = self.fake.inventory

        def inventory():
            snap = base()
            snap["pending_workunits"] = 24
            snap["pending_ocr_workunits"] = 4
            return snap

        self.fake.inventory = inventory
        xml = status_xml(ocr_unsent=4, qwen_unsent=20, quality_unsent=0,
                         quality_in_progress=0, train_unsent=0)
        event = self.tick(low_watermark=1, high_watermark=100, max_global_pending=30,
                          server_status=xml)
        self.assertEqual(event["global_pending"], 24)
        self.assertEqual(event["other_app_pending"], 20)
        self.assertEqual(event["cohort_pending"], 40)
        self.assertNotEqual(event["backpressure_reason"], "global_safety_cap")
        # An inventory from before bundles (no pending_ocr_workunits) counts as before.
        self.fake.inventory = base
        self.fake.extra_pending = set()
        self.assertEqual(batch.ocr_workunit_counts({}, {"a", "b"}), (2, 0))
        with self.assertRaises(ValueError):
            batch.ocr_workunit_counts({"pending_ocr_workunits": 3}, {"a", "b"})

    def test_456_unsent_qwen_tasks_do_not_block_ocr_refill_below_low_watermark(self):
        # Reproduce the 2026-10-17 live starvation state: 456 unsent usnm_qwen tasks (plus
        # quality/training) in aggregate pending_workunits, while usnm_ocr has 0 unsent / 0 in_progress.
        self.fake.other = 513
        xml = status_xml(ocr_unsent=0, ocr_in_progress=0, qwen_unsent=456)
        event = self.tick(low_watermark=1, high_watermark=4, max_global_pending=1000,
                          server_status=xml)
        self.assertFalse(event["backpressure"])
        self.assertIsNone(event["backpressure_reason"])
        self.assertEqual(event["cohort_pending"], 0)
        self.assertEqual(event["other_app_pending"], 513)
        self.assertEqual(event["global_pending"], 513)
        self.assertEqual(event["max_global_pending"], 1000)
        self.assertEqual(event["selected"], 2)
        self.assertEqual(len(self.fake.jobs), 2)
        self.assertEqual(
            [action[0] for action in self.fake.actions],
            ["export", "submit", "submit"],
        )

    def test_456_unsent_qwen_tasks_do_not_block_queue_fed_ocr_claim_and_refill(self):
        qcfg = queue_config()
        qfake = QueueFake([message(number) for number in range(1, 6)])
        qfake.assertions = []
        qfake.other = 456
        state = {}
        xml = status_xml(ocr_unsent=0, ocr_in_progress=0, qwen_unsent=456,
                         quality_unsent=0, quality_in_progress=0, train_unsent=0)
        event = batch.tick(qcfg, state, qfake, lambda: None, NOW,
                           low_watermark=1, high_watermark=4, max_global_pending=1000,
                           server_status=xml)
        self.assertFalse(event["backpressure"])
        self.assertEqual(qfake.actions[0], ("claim", 2))
        self.assertEqual(len(qfake.jobs), 2)

    def test_global_safety_cap_halts_refill_with_explicit_diagnostic_reason(self):
        # 456 unsent usnm_qwen tasks exceed max_global_pending=400 even though usnm_ocr is 0/0.
        self.fake.other = 456
        xml = status_xml(ocr_unsent=0, ocr_in_progress=0, qwen_unsent=456,
                         quality_unsent=0, quality_in_progress=0, train_unsent=0)
        blocked = self.tick(low_watermark=1, high_watermark=4, max_global_pending=400,
                            server_status=xml)
        self.assertTrue(blocked["backpressure"])
        self.assertEqual(blocked["backpressure_reason"], "global_safety_cap")
        self.assertEqual(blocked["selected"], 0)
        self.assertEqual(self.fake.actions, [])
        self.assertTrue(blocked["starvation_alert"])
        self.assertEqual(blocked["alerts"][0]["blocked_by"], "global_safety_cap")
        self.assertEqual(blocked["alerts"][0]["refilled"], 0)

        # Raising max_global_pending above 456 allows usnm_ocr to refill up to its cohort high watermark,
        # after which backpressure_reason explicitly reports cohort_high_watermark rather than global_safety_cap.
        first = self.tick(low_watermark=1, high_watermark=4, max_global_pending=600,
                          server_status=xml)
        self.assertFalse(first["backpressure"])
        self.assertIsNone(first["backpressure_reason"])
        second = self.tick(low_watermark=1, high_watermark=4, max_global_pending=600,
                           server_status=xml)
        self.assertFalse(second["backpressure"])
        self.assertEqual(len(self.fake.jobs), 4)
        at_high = self.tick(low_watermark=1, high_watermark=4, max_global_pending=600,
                            server_status=xml)
        self.assertTrue(at_high["backpressure"])
        self.assertEqual(at_high["backpressure_reason"], "cohort_high_watermark")
        self.assertNotEqual(at_high["backpressure_reason"], blocked["backpressure_reason"])

    def test_low_high_watermark_hysteresis_works_cleanly_across_ticks(self):
        self.fake.other = 456
        xml = status_xml(ocr_unsent=0, ocr_in_progress=0, qwen_unsent=456,
                         quality_unsent=0, quality_in_progress=0, train_unsent=0)
        # Tick 1: at 0 (<= low_watermark 1), enters refill mode and dispatches batch_size=2.
        t1 = self.tick(low_watermark=1, high_watermark=4, max_global_pending=1000,
                       server_status=xml)
        self.assertFalse(t1["backpressure"])
        self.assertTrue(t1["cohort"]["refilling"])
        self.assertEqual(len(self.fake.jobs), 2)

        # Tick 2: at 2 (> low_watermark 1, < high_watermark 4), continues in-progress refill wave up to 4.
        t2 = self.tick(low_watermark=1, high_watermark=4, max_global_pending=1000,
                       server_status=xml)
        self.assertFalse(t2["backpressure"])
        self.assertFalse(t2["cohort"]["refilling"])
        self.assertEqual(len(self.fake.jobs), 4)

        # Tick 3: at high_watermark (4), stops refilling with cohort_high_watermark.
        t3 = self.tick(low_watermark=1, high_watermark=4, max_global_pending=1000,
                       server_status=xml)
        self.assertTrue(t3["backpressure"])
        self.assertEqual(t3["backpressure_reason"], "cohort_high_watermark")
        self.assertEqual(t3["selected"], 4)

        # Tick 4: 2 of the 4 jobs finish and are collected; 2 remain in flight (> low_watermark 1).
        # Hysteresis holds back refill while draining between high_watermark (4) and low_watermark (1).
        job_ids = sorted(self.fake.jobs)
        for job_id in job_ids[:2]:
            self.fake.jobs[job_id]["spool"] = "/project/usnmocr/results/batch1"
        t4 = self.tick(low_watermark=1, high_watermark=4, max_global_pending=1000,
                       server_status=xml)
        self.assertEqual(t4["newly_published"], 2)
        self.assertEqual(t4["cohort_pending"], 2)
        self.assertTrue(t4["backpressure"])
        self.assertEqual(t4["backpressure_reason"], "cohort_hysteresis")
        self.assertEqual(t4["selected"], 4)
        self.assertEqual(len(self.fake.jobs), 4)

        # Tick 5: 1 more job finishes and is collected; now only 1 job remains in flight (<= low_watermark 1).
        # Hysteresis triggers refill back toward high_watermark (4).
        self.fake.jobs[job_ids[2]]["spool"] = "/project/usnmocr/results/batch1"
        t5 = self.tick(low_watermark=1, high_watermark=4, max_global_pending=1000,
                       server_status=xml)
        self.assertEqual(t5["newly_published"], 1)
        self.assertEqual(t5["cohort_pending"], 1)
        self.assertFalse(t5["backpressure"])
        self.assertIsNone(t5["backpressure_reason"])
        self.assertEqual(t5["selected"], 6)
        self.assertEqual(len(self.fake.jobs), 6)

        # Tick 6: completes the refill wave from 3 up to high_watermark (4).
        t6 = self.tick(low_watermark=1, high_watermark=4, max_global_pending=1000,
                       server_status=xml)
        self.assertFalse(t6["backpressure"])
        self.assertEqual(t6["selected"], 7)
        self.assertEqual(len(self.fake.jobs), 7)
        self.assertFalse(t6["cohort"]["refilling"])

    def test_starvation_alert_triggers_when_active_ocr_hosts_face_zero_unsent_work(self):
        self.fake.other = 456
        xml_starved = status_xml(ocr_unsent=0, ocr_in_progress=0, qwen_unsent=456)
        event = self.tick(low_watermark=1, high_watermark=4, max_global_pending=1000,
                          server_status=xml_starved)
        self.assertTrue(event["starvation_alert"])
        self.assertEqual(len(event["alerts"]), 1)
        alert = event["alerts"][0]
        self.assertEqual(alert["code"], "cohort_starvation")
        self.assertEqual(alert["app"], "usnm_ocr")
        self.assertEqual(alert["active_hosts"], 2)
        self.assertEqual(alert["unsent"], 0)
        self.assertEqual(alert["other_app_pending"], 456)
        self.assertEqual(alert["other_app_pending_results"], 513)
        self.assertEqual(alert["refilled"], 2)

        # Once usnm_ocr has > low_watermark runnable work and > 0 unsent work, starvation alert clears.
        xml_healthy = status_xml(ocr_unsent=3, ocr_in_progress=1, qwen_unsent=456)
        healthy = self.tick(low_watermark=1, high_watermark=4, max_global_pending=1000,
                            server_status=xml_healthy)
        self.assertFalse(healthy["starvation_alert"])
        self.assertEqual(healthy["alerts"], [])

        # Repeated zero-work ('No tasks sent') scheduler replies trigger zero_work_starvation.
        self.fake.scheduler_log = (
            "2026-10-17 19:09:01 [PID=12] Sending reply to [HOST#8]: 0 results\n"
            "2026-10-17 19:09:01 [PID=12] No tasks sent\n"
            "2026-10-17 19:09:15 [PID=14] Got 0 new tasks\n"
        )
        zero_reply = self.tick(low_watermark=1, high_watermark=4, max_global_pending=1000,
                               server_status=status_xml(ocr_unsent=0, ocr_in_progress=1, qwen_unsent=456))
        self.assertTrue(zero_reply["starvation_alert"])
        self.assertEqual(zero_reply["zero_work_requests"], 3)
        self.assertEqual(zero_reply["alerts"][0]["code"], "zero_work_starvation")

        # With 0 active hosts and 0 zero-work requests, no starvation alert is emitted.
        self.fake.hosts = 0
        self.fake.scheduler_log = None
        idle = self.tick(low_watermark=1, high_watermark=4, max_global_pending=1000,
                         server_status=xml_starved)
        self.assertFalse(idle["starvation_alert"])
        self.assertEqual(idle["alerts"], [])

    def test_parse_server_status_and_config_cohort_validation(self):
        parsed = batch.parse_server_status(status_xml(ocr_unsent=2, ocr_in_progress=3, qwen_unsent=456).encode())
        self.assertEqual(parsed["apps"]["usnm_ocr"], {"unsent": 2, "in_progress": 3, "runnable": 5})
        self.assertEqual(parsed["apps"]["usnm_qwen"], {"unsent": 456, "in_progress": 0, "runnable": 456})

        from_dict = batch.parse_server_status({
            "tasks_by_app": [{"name": "usnm_ocr", "unsent": 1, "in_progress": 2}],
            "zero_work_hosts": ["host8", "host9"],
        })
        self.assertEqual(from_dict["apps"]["usnm_ocr"]["runnable"], 3)
        self.assertEqual(from_dict["zero_work_requests"], 2)

        for bad in (
            b"<!DOCTYPE html><server_status/>",
            b"<server_status><database_file_states><tasks_by_app><app><name>usnm_ocr</name><unsent>-1</unsent><in_progress>0</in_progress></app></tasks_by_app></database_file_states></server_status>",
            b"<not_status/>",
            b"x" * (batch.MAX_STATUS_BYTES + 1),
        ):
            with self.subTest(bad=bad[:24]), self.assertRaises(ValueError):
                batch.parse_server_status(bad)

        directory = Path(__file__).parent.resolve() / (".watermark-config-" + uuid.uuid4().hex)
        directory.mkdir()
        try:
            cfg = {
                **self.config,
                "low_watermark": 1,
                "high_watermark": 4,
                "max_global_pending": 1000,
                "cohorts": {
                    "usnm_ocr:cpu": {
                        "app": "usnm_ocr", "platform": "all", "resource": "cpu",
                        "low_watermark": 1, "high_watermark": 4, "pages_per_host": 2,
                    }
                },
            }
            cfg_path = directory / "config.json"
            cfg_path.write_text(json.dumps(cfg))
            loaded = batch.load_config(cfg_path)
            self.assertEqual(loaded["max_global_pending"], 1000)

            status_path = directory / "server_status.xml"
            status_path.write_text(status_xml(ocr_unsent=0, ocr_in_progress=0, qwen_unsent=456))
            out = io.StringIO()
            with patch.object(batch.subprocess, "run", side_effect=AssertionError("offline")), \
                    contextlib.redirect_stdout(out):
                self.assertEqual(batch.main([
                    "--config", str(cfg_path), "--state-dir", str(directory),
                    "--low-watermark", "1", "--high-watermark", "4", "--max-global-pending", "800",
                ]), 0)
            plan = json.loads(out.getvalue())
            self.assertEqual((plan["low_watermark"], plan["high_watermark"], plan["max_global_pending"]),
                             (1, 4, 800))

            with patch.object(batch.Transport, "lock", return_value=contextlib.nullcontext()), \
                    patch.object(batch, "tick", return_value={"event": "boinc_batch_tick"}) as mock_tick, \
                    contextlib.redirect_stdout(io.StringIO()):
                self.assertEqual(batch.main([
                    "--config", str(cfg_path), "--state-dir", str(directory), "--apply",
                    "--low-watermark", "1", "--high-watermark", "4", "--max-global-pending", "800",
                    "--server-status-file", str(status_path),
                ]), 0)
            self.assertEqual(mock_tick.call_args.kwargs["max_global_pending"], 800)
            transport_arg = mock_tick.call_args.args[2]
            with contextlib.redirect_stderr(io.StringIO()):  # its timings report the status phase
                self.assertEqual(transport_arg.server_status()["apps"]["usnm_qwen"]["unsent"], 456)
        finally:
            for item in directory.iterdir():
                item.unlink()
            directory.rmdir()

    def test_interrupted_export_recovers_at_global_safety_cap(self):
        # Regression for comment 4308880789: with batch_size=2, high_watermark=2, and
        # max_global_pending=2, an interruption after persisting two selections leaves
        # global_pending=2 (via missing) and global_free=0 on retry; existing allocations
        # must still export and submit so work reaches BOINC.
        xml = status_xml(ocr_unsent=0, ocr_in_progress=0, qwen_unsent=0,
                         quality_unsent=0, quality_in_progress=0, train_unsent=0)
        self.fake.crash_export = True
        with self.assertRaisesRegex(RuntimeError, "interrupted"):
            self.tick(low_watermark=1, high_watermark=2, max_global_pending=2, server_status=xml)
        self.assertEqual(len(self.state["selected"]), 2)
        self.assertNotIn("refilling", self.state)
        self.assertEqual(self.fake.jobs, {})

        recovered = self.tick(low_watermark=1, high_watermark=2, max_global_pending=2,
                              server_status=xml)
        self.assertEqual(recovered["selected"], 2)
        self.assertEqual(len(self.fake.jobs), 2)
        self.assertEqual(
            [action[0] for action in self.fake.actions],
            ["export", "submit", "submit"],
        )

    def test_interrupted_first_refill_resumes_under_hysteresis(self):
        # Regression for comment 4308880809: with low=1, high=4 and batch_size=2, interruption
        # after persisting selections but before completing export leaves two allocated pages
        # and no saved refilling flag; retry enters hysteresis (pending=2) for fresh allocation
        # but must still export and submit the recovered reservations.
        xml = status_xml(ocr_unsent=0, ocr_in_progress=0, qwen_unsent=0,
                         quality_unsent=0, quality_in_progress=0, train_unsent=0)
        self.fake.crash_export = True
        with self.assertRaisesRegex(RuntimeError, "interrupted"):
            self.tick(low_watermark=1, high_watermark=4, max_global_pending=10, server_status=xml)
        self.assertEqual(len(self.state["selected"]), 2)
        self.assertNotIn("refilling", self.state)
        self.assertEqual(self.fake.jobs, {})

        recovered = self.tick(low_watermark=1, high_watermark=4, max_global_pending=10,
                              server_status=xml)
        self.assertEqual(recovered["selected"], 2)
        self.assertEqual(len(self.fake.jobs), 2)
        self.assertEqual(
            [action[0] for action in self.fake.actions],
            ["export", "submit", "submit"],
        )

    def test_non_canonical_ocr_workunits_are_not_double_counted_in_global_pending(self):
        # Regression for comment 4308880830: batch-inventory counts all unassimilated workunits
        # in pending_workunits, but pending_jobs only lists usnm_[0-9a-f]{64}. Three OCR workunits
        # with other names (pending_workunits=3, pending_jobs=[], ocr_unsent=3) must report
        # global_pending=3 (not 6) and allow 1 page to refill under a cap of 4.
        self.fake.other = 3
        xml = status_xml(ocr_unsent=3, ocr_in_progress=0, qwen_unsent=0,
                         quality_unsent=0, quality_in_progress=0, train_unsent=0)
        event = self.tick(low_watermark=3, high_watermark=4, max_global_pending=4,
                          server_status=xml)
        self.assertEqual(event["cohort_pending"], 3)
        self.assertEqual(event["other_app_pending"], 0)
        self.assertEqual(event["global_pending"], 3)
        self.assertFalse(event["backpressure"])
        self.assertEqual(event["selected"], 1)
        self.assertEqual(len(self.fake.jobs), 1)

    def test_228_qwen_workunits_with_456_unsent_replicas_do_not_block_cap_of_400(self):
        # Regression for comment 4308954039: Qwen creates 2 result replicas per workunit,
        # so 228 Qwen workunits in SQL (pending_workunits=228) have 456 unsent replicas in
        # server_status. Workunit-level global_pending must be 228 (not 456) and must not
        # block an empty usnm_ocr cohort under max_global_pending=400, while retaining
        # replica counts separately for diagnostics.
        self.fake.other = 228
        xml = status_xml(ocr_unsent=0, ocr_in_progress=0, qwen_unsent=456,
                         quality_unsent=0, quality_in_progress=0, train_unsent=0)
        event = self.tick(low_watermark=1, high_watermark=4, max_global_pending=400,
                          server_status=xml)
        self.assertFalse(event["backpressure"])
        self.assertIsNone(event["backpressure_reason"])
        self.assertEqual(event["cohort_pending"], 0)
        self.assertEqual(event["other_app_pending"], 228)
        self.assertEqual(event["global_pending"], 228)
        self.assertEqual(event["cohort_pending_results"], 0)
        self.assertEqual(event["other_app_pending_results"], 456)
        self.assertEqual(event["global_pending_results"], 456)
        self.assertEqual(event["selected"], 2)
        self.assertEqual(len(self.fake.jobs), 2)

    def test_non_ocr_cohort_app_selectors_are_rejected(self):
        # Regression for comment 4308954020 and Round-4 selector validation:
        # Transport.submit() and BOINC status/inventory accounting only support
        # application-wide CPU usnm_ocr workunits (app="usnm_ocr", platform="all", resource="cpu").
        for bad_app in ("usnm_qwen", "usnm_surya", "usnm_quality", "other"):
            with self.subTest(app=bad_app):
                with self.assertRaisesRegex(batch.ConfigError, "only dispatches usnm_ocr"):
                    batch.normalize_cohort_apps(bad_app)
                with self.assertRaisesRegex(batch.ConfigError, "only dispatches usnm_ocr"):
                    batch._validate_cohort_spec({"app": bad_app})
                with self.assertRaisesRegex(batch.ConfigError, "only dispatches usnm_ocr"):
                    batch.resolve_watermarks({**self.config, "app": bad_app},
                                             self.fake.inventory())
        for bad_platform in ("x86_64-pc-linux-gnu", "arm64-apple-darwin", "linux"):
            with self.subTest(platform=bad_platform):
                with self.assertRaisesRegex(batch.ConfigError, "application-scoped across all platforms"):
                    batch.normalize_cohort_platform(bad_platform)
                with self.assertRaisesRegex(batch.ConfigError, "application-scoped across all platforms"):
                    batch._validate_cohort_spec({"platform": bad_platform})
                with self.assertRaisesRegex(batch.ConfigError, "application-scoped across all platforms"):
                    batch.resolve_watermarks({**self.config, "platform": bad_platform},
                                             self.fake.inventory())
        for bad_resource in ("gpu", "cuda", "mps"):
            with self.subTest(resource=bad_resource):
                with self.assertRaisesRegex(batch.ConfigError, "CPU-only"):
                    batch.normalize_cohort_resource(bad_resource)
                with self.assertRaisesRegex(batch.ConfigError, "CPU-only"):
                    batch._validate_cohort_spec({"resource": bad_resource})
                with self.assertRaisesRegex(batch.ConfigError, "CPU-only"):
                    batch.resolve_watermarks({**self.config, "resource": bad_resource},
                                             self.fake.inventory())

    def test_empty_matched_cohort_retains_inherited_defaults(self):
        # An explicitly matched empty cohort entry ("small": {}) must retain inherited
        # defaults rather than falling through to another cohort ("large": {"high_watermark": 100}).
        cfg = {
            **self.config,
            "cohort": "small",
            "cohorts": {
                "large": {"high_watermark": 100},
                "small": {},
            },
        }
        resolved = batch.resolve_watermarks(cfg, self.fake.inventory())
        self.assertEqual(resolved["cohort"], "small")
        self.assertEqual(resolved["high_watermark"], 4)
        self.assertEqual(resolved["low_watermark"], 2)

    def test_incomplete_structured_status_counters_are_rejected(self):
        # Structured status entries must provide both unsent and in_progress counters
        # rather than defaulting missing counters to 0 and bypassing the SQL fallback.
        for bad_status in (
            {"apps": {"usnm_ocr": {}}},
            {"apps": {"usnm_ocr": {"unsent": 0}}},
            {"apps": {"usnm_ocr": {"in_progress": 0}}},
            {"tasks_by_app": [{"name": "usnm_ocr"}]},
            {"tasks_by_app": [{"name": "usnm_ocr", "unsent": 1}]},
        ):
            with self.subTest(status=bad_status):
                with self.assertRaisesRegex(ValueError, "missing BOINC app queue counters"):
                    batch.parse_server_status(bad_status)
                with self.assertRaisesRegex(ValueError, "missing BOINC app queue counters"):
                    self.tick(low_watermark=1, high_watermark=4, max_global_pending=10,
                              server_status=bad_status)

    def test_refill_calculation_includes_existing_uncollected_backlog(self):
        # With low=1, high=4, batch_size=3, and 1 foreign reservation in backlog,
        # refilling 3 jobs reaches 4 total pending workunits and must clear refilling=False.
        self.config["batch_size"] = 3
        foreign = record("sn99999999/1900-01-01/ed-1/seq-1", 99)
        foreign["plan"] = "other-plan"
        self.fake.rows.append(foreign)
        xml = status_xml(ocr_unsent=0, ocr_in_progress=0, qwen_unsent=0,
                         quality_unsent=0, quality_in_progress=0, train_unsent=0)
        event = self.tick(low_watermark=1, high_watermark=4, max_global_pending=10,
                          server_status=xml)
        self.assertEqual(event["selected"], 3)
        self.assertEqual(len(self.fake.jobs), 3)
        self.assertFalse(event["cohort"]["refilling"])
        self.assertFalse(self.state["refilling"])

    def test_missing_or_aggregate_only_status_conservatively_uses_sql_backlog(self):
        # Regression for comment 4309021102: when explicit watermarks enable cohort_mode without
        # per-app status (or with aggregate-only status / status omitting usnm_ocr), unattributed
        # SQL workunits in pending_workunits must be counted conservatively toward cohort_pending.
        aggregate_only_xml = (
            "<server_status><database_file_states>"
            "<results_ready_to_send>3</results_ready_to_send>"
            "<results_in_progress>0</results_in_progress>"
            "</database_file_states></server_status>"
        )
        omitting_ocr_xml = (
            "<server_status><database_file_states>"
            "<results_ready_to_send>3</results_ready_to_send>"
            "<results_in_progress>0</results_in_progress>"
            "<tasks_by_app><app><name>usnm_qwen</name><unsent>0</unsent><in_progress>0</in_progress></app></tasks_by_app>"
            "</database_file_states></server_status>"
        )
        for label, status_input in (
            ("missing_status", None),
            ("aggregate_only_xml", aggregate_only_xml),
            ("omitting_ocr_xml", omitting_ocr_xml),
        ):
            with self.subTest(case=label):
                self.setUp()
                self.fake.other = 3
                # With low=1, high=4 and 3 unattributed workunits in SQL, cohort_pending=3 enters
                # hysteresis and admits 0 pages (not 2).
                held_by_hysteresis = self.tick(
                    low_watermark=1, high_watermark=4, max_global_pending=10,
                    server_status=status_input,
                )
                self.assertEqual(held_by_hysteresis["cohort_pending"], 3)
                self.assertEqual(held_by_hysteresis["other_app_pending"], 0)
                self.assertEqual(held_by_hysteresis["selected"], 0)
                self.assertEqual(len(self.fake.jobs), 0)

                # With low=3, high=4 and 3 unattributed workunits in SQL, cohort_pending=3 admits
                # only 1 page (not batch_size=2) to reach high=4.
                refilled_one = self.tick(
                    low_watermark=3, high_watermark=4, max_global_pending=10,
                    server_status=status_input,
                )
                self.assertEqual(refilled_one["cohort_pending"], 3)
                self.assertEqual(refilled_one["selected"], 1)
                self.assertEqual(len(self.fake.jobs), 1)

    def test_healthy_ocr_queue_with_zero_work_replies_does_not_alert(self):
        # Regression for comment 4309021122: when usnm_ocr has a healthy queue (unsent=3,
        # in_progress=1, low_watermark=1), zero-work scheduler replies (e.g. from earlier or
        # another app/incompatible host) must not trigger zero_work_starvation.
        self.fake.other = 228
        self.fake.scheduler_log = (
            "2026-10-17 19:09:01 [PID=12] Sending reply to [HOST#8]: 0 results\n"
            "2026-10-17 19:09:01 [PID=12] No tasks sent\n"
            "2026-10-17 19:09:15 [PID=14] Got 0 new tasks\n"
        )
        healthy_xml = status_xml(ocr_unsent=3, ocr_in_progress=1, qwen_unsent=456)
        event = self.tick(low_watermark=1, high_watermark=4, max_global_pending=1000,
                          server_status=healthy_xml)
        self.assertEqual(event["zero_work_requests"], 3)
        self.assertFalse(event["starvation_alert"])
        self.assertEqual(event["alerts"], [])

    def test_status_failure_does_not_prevent_drain_collection(self):
        # Regression for comment 4309021131: --apply --drain with an unavailable or malformed
        # server status file/endpoint must still collect available results and succeed.
        self.tick(low_watermark=1, high_watermark=4, max_global_pending=10)
        for job in self.fake.jobs.values():
            job["spool"] = "/project/usnmocr/results/batch1"

        directory = Path(__file__).parent.resolve() / (".drain-status-" + uuid.uuid4().hex)
        directory.mkdir()
        try:
            cfg_path = directory / "config.json"
            cfg_path.write_text(json.dumps(self.config))
            bad_status = directory / "corrupt_status.xml"
            bad_status.write_text("<!DOCTYPE html><broken")
            batch.save(directory / "state.json", {
                "config_sha256": batch.digest(self.config),
                **self.state,
            })

            real_transport = batch.Transport

            def fake_transport(cfg, state_dir, *, status_url=None, status_file=None, **batching):
                self.fake.lock = lambda: contextlib.nullcontext()
                self.fake.server_status = lambda: real_transport(
                    cfg, state_dir, status_url=status_url, status_file=status_file,
                ).server_status()
                return self.fake

            out = io.StringIO()
            with patch.object(batch, "Transport", side_effect=fake_transport), \
                    patch.object(batch.time, "time", return_value=NOW), \
                    contextlib.redirect_stdout(out):
                rc = batch.main([
                    "--config", str(cfg_path), "--state-dir", str(directory),
                    "--apply", "--drain", "--server-status-file", str(bad_status),
                ])
            self.assertEqual(rc, 0)
            drained = json.loads(out.getvalue())
            self.assertTrue(drained["draining"])
            self.assertEqual(drained["published"], 2)
            self.assertEqual(drained["newly_published"], 2)
        finally:
            for item in directory.iterdir():
                item.unlink()
            directory.rmdir()


def surya_config():
    return {
        "schema_version": 1, "env": "dev", "plan": "surya-wave", "app": "usnm_surya",
        "low_watermark": 100, "high_watermark": 400,
        "step_command": ["ssh", "-o", "BatchMode=yes", "usnm-boinc-step@boinc.example"],
        "surya": {"campaign": "surya-wave1", "quality_campaign": "quality-2026",
                  "engine_image": "sha256:" + "1" * 64, "engine_llama": "sha256:" + "2" * 64,
                  "engine_weights": "sha256:" + "3" * 64},
    }


def surya_status(unsent):
    return status_xml().replace("<app><name>usnm_surya</name><unsent>0</unsent>",
                                f"<app><name>usnm_surya</name><unsent>{unsent}</unsent>")


class SuryaFake:
    """Records the bridge steps a Surya tick runs; no inventory, journal or hand-off exists."""

    def __init__(self):
        self.steps = []
        self.fail = None

    def step(self, target, step, **variables):
        self.steps.append((target, step, variables))
        if step == self.fail:
            raise RuntimeError(f"{target}/{step} failed; inspect the execution privately before retrying")


class SuryaPlanTests(unittest.TestCase):
    def setUp(self):
        self.config = surya_config()
        self.state = {}
        self.fake = SuryaFake()
        self.saved = []

    def tick(self, unsent, **kwargs):
        return batch.tick(self.config, self.state, self.fake,
                          lambda: self.saved.append(copy.deepcopy(self.state)), NOW,
                          server_status=surya_status(unsent), **kwargs)

    def load(self, value):
        directory = Path(__file__).parent.resolve() / (".surya-config-" + uuid.uuid4().hex)
        directory.mkdir()
        try:
            path = directory / "config.json"
            path.write_text(json.dumps(value))
            return batch.load_config(path)
        finally:
            for file in directory.iterdir():
                file.unlink()
            directory.rmdir()

    def test_config_validation(self):
        loaded = self.load(self.config)
        self.assertEqual(loaded["surya"]["campaign"], "surya-wave1")
        full = copy.deepcopy(self.config)
        full["surya"].update(timeout_secs=14400, stage_pages=500, submit_units=2000)
        full["server_status_url"] = "https://boinc.example/server_status.php?xml=1"
        self.load(full)
        for label, change in (
            ("no surya", lambda c: c.pop("surya")),
            ("no low", lambda c: c.pop("low_watermark")),
            ("no high", lambda c: c.pop("high_watermark")),
            ("low over high", lambda c: c.update(low_watermark=401)),
            ("zero high", lambda c: c.update(low_watermark=0, high_watermark=0)),
            ("pages config field", lambda c: c.update(pages=[])),
            ("manifest field", lambda c: c.update(manifest="v1/manifest.parquet")),
            ("account field", lambda c: c.update(account="exchange123")),
            ("schema", lambda c: c.update(schema_version=2)),
            ("bad env", lambda c: c.update(env="Dev")),
            ("bad step_command", lambda c: c.update(step_command=["ssh", "a b"])),
            ("http status url", lambda c: c.update(server_status_url="http://boinc.example/s")),
            ("surya not object", lambda c: c.update(surya=[])),
            ("unknown surya key", lambda c: c["surya"].update(extra=1)),
            ("no campaign", lambda c: c["surya"].pop("campaign")),
            ("no quality campaign", lambda c: c["surya"].pop("quality_campaign")),
            ("upper campaign", lambda c: c["surya"].update(campaign="Surya")),
            ("slash campaign", lambda c: c["surya"].update(quality_campaign="a/b")),
            ("long campaign", lambda c: c["surya"].update(campaign="a" * 42)),
            ("tag image", lambda c: c["surya"].update(engine_image="latest")),
            ("short llama", lambda c: c["surya"].update(engine_llama="sha256:" + "2" * 63)),
            ("upper weights", lambda c: c["surya"].update(engine_weights="sha256:" + "A" * 64)),
            ("timeout low", lambda c: c["surya"].update(timeout_secs=59)),
            ("timeout high", lambda c: c["surya"].update(timeout_secs=14401)),
            ("timeout bool", lambda c: c["surya"].update(timeout_secs=True)),
            ("stage zero", lambda c: c["surya"].update(stage_pages=0)),
            ("stage high", lambda c: c["surya"].update(stage_pages=501)),
            ("submit zero", lambda c: c["surya"].update(submit_units=0)),
            ("submit high", lambda c: c["surya"].update(submit_units=2001)),
            ("submit string", lambda c: c["surya"].update(submit_units="200")),
        ):
            with self.subTest(label):
                bad = copy.deepcopy(self.config)
                change(bad)
                with self.assertRaises(ValueError):
                    self.load(bad)

    def test_existing_ocr_configs_stay_valid_and_surya_cannot_ride_an_ocr_plan(self):
        self.assertEqual(self.load(config())["batch"], "test_batch")
        queued = queue_config()
        del queued["pages"]
        self.assertEqual(self.load(queued)["source"], "queue")
        # An OCR plan still names no other app than usnm_ocr.
        with self.assertRaises(ValueError):
            self.load({**config(), "app": "usnm_surya"})

    def test_below_the_low_watermark_stages_crops_then_submits_up_to_the_high_one(self):
        event = self.tick(40)
        crops, submit = self.fake.steps
        self.assertEqual(crops, ("bridge", "surya-stage-crops", {
            "SURYA_CAMPAIGN": "surya-wave1", "QUALITY_CAMPAIGN": "quality-2026",
            "SURYA_ENGINE_IMAGE": "sha256:" + "1" * 64, "SURYA_ENGINE_LLAMA": "sha256:" + "2" * 64,
            "SURYA_ENGINE_WEIGHTS": "sha256:" + "3" * 64, "SURYA_MAX": 50, "SURYA_TIMEOUT_SECS": 600}))
        # 400 - 40 = 360 results of room, two results per unit: 180 units, under the default 200.
        self.assertEqual(submit, ("bridge", "surya-submit", {"SURYA_CAMPAIGN": "surya-wave1", "SURYA_MAX": 180}))
        self.assertEqual((event["ran"], event["unsent"], event["skipped"]),
                         (["surya-stage-crops", "surya-submit"], 40, None))
        self.assertEqual(self.state["surya"]["last_run"]["at"], NOW)
        self.assertEqual([step["step"] for step in self.state["surya"]["last_run"]["steps"]], event["ran"])
        self.assertEqual(self.state["surya"]["unsent"], 40)

    def test_submit_is_capped_by_half_the_room_below_the_high_watermark(self):
        self.tick(100)  # at the low mark counts as low: 300 results of room, 150 units
        self.assertEqual(self.fake.steps[1][2]["SURYA_MAX"], 150)
        self.fake.steps.clear()
        self.config["high_watermark"] = 150
        self.tick(100)
        self.assertEqual(self.fake.steps[1][2]["SURYA_MAX"], 25)
        # Odd headroom rounds down: 51 results of room is 25 units, never 26.
        self.fake.steps.clear()
        self.config["high_watermark"] = 151
        self.tick(100)
        self.assertEqual(self.fake.steps[1][2]["SURYA_MAX"], 25)
        self.fake.steps.clear()
        self.config["surya"]["submit_units"] = 10
        self.tick(100)
        self.assertEqual(self.fake.steps[1][2]["SURYA_MAX"], 10)

    def test_configured_caps_reach_the_steps(self):
        self.config["surya"].update(timeout_secs=1200, stage_pages=7, submit_units=9)
        self.tick(0)
        self.assertEqual(self.fake.steps[0][2]["SURYA_MAX"], 7)
        self.assertEqual(self.fake.steps[0][2]["SURYA_TIMEOUT_SECS"], 1200)
        self.assertEqual(self.fake.steps[1][2]["SURYA_MAX"], 9)

    def test_above_the_low_watermark_runs_nothing(self):
        for unsent in (101, 250, 400, 900):
            with self.subTest(unsent=unsent):
                event = self.tick(unsent)
                self.assertEqual((self.fake.steps, event["ran"], event["skipped"]),
                                 ([], [], "above_low_watermark"))
        self.assertNotIn("last_run", self.state["surya"])
        self.assertEqual(self.state["surya"]["unsent"], 900)

    def test_a_queue_with_room_for_less_than_one_unit_runs_nothing(self):
        for high in (100, 101):
            with self.subTest(high=high):
                self.config.update(low_watermark=100, high_watermark=high)
                self.assertEqual(self.tick(100)["skipped"], "no_room")
                self.assertEqual(self.fake.steps, [])
        self.config.update(high_watermark=102)
        self.assertEqual(self.tick(100)["skipped"], None)
        self.assertEqual(self.fake.steps[1][2]["SURYA_MAX"], 1)

    def test_engine_pins_are_held_to_the_campaign(self):
        self.tick(300)  # a quiet tick still records the campaign's pins
        self.assertEqual(self.state["surya"]["engine"]["SURYA_ENGINE_WEIGHTS"], "sha256:" + "3" * 64)
        self.config["surya"]["engine_weights"] = "sha256:" + "4" * 64
        with self.assertRaisesRegex(ValueError, "engine pins changed for campaign surya-wave1"):
            self.tick(0)
        self.assertEqual(self.fake.steps, [])
        self.assertEqual(self.state["surya"]["engine"]["SURYA_ENGINE_WEIGHTS"], "sha256:" + "3" * 64)
        # The same change under a new campaign name starts a fresh record.
        self.config["surya"]["campaign"] = "surya-wave2"
        self.tick(0)
        self.assertEqual([step[1] for step in self.fake.steps], ["surya-stage-crops", "surya-submit"])
        self.assertEqual(self.fake.steps[0][2]["SURYA_ENGINE_WEIGHTS"], "sha256:" + "4" * 64)
        self.assertEqual(self.state["surya"]["campaign"], "surya-wave2")
        self.assertEqual(self.state["surya"]["engine"]["SURYA_ENGINE_WEIGHTS"], "sha256:" + "4" * 64)
        # Watermarks and caps may still change within the campaign.
        self.fake.steps.clear()
        self.config.update(high_watermark=300)
        self.config["surya"]["submit_units"] = 20
        self.tick(0)
        self.assertEqual(self.fake.steps[1][2]["SURYA_MAX"], 20)

    def test_a_previous_run_stays_on_record_through_a_quiet_tick(self):
        self.tick(0)
        self.tick(300)
        self.assertEqual(self.state["surya"]["last_run"]["unsent"], 0)
        self.assertEqual(self.state["surya"]["unsent"], 300)

    def test_interrupted_first_stage_keeps_engine_pins_across_restart(self):
        for previous_campaign in (False, True):
            with self.subTest(previous_campaign=previous_campaign):
                self.setUp()
                if previous_campaign:
                    self.tick(300)
                    self.config["surya"]["campaign"] = "surya-wave2"
                self.fake.fail = "surya-stage-crops"
                with self.assertRaises(RuntimeError):
                    self.tick(0)
                # A failed step may already have written units with the original pins.
                self.state = copy.deepcopy(self.saved[-1]) if self.saved else {}
                self.config["surya"]["engine_weights"] = "sha256:" + "4" * 64
                self.fake.fail = None
                self.fake.steps.clear()
                with self.assertRaisesRegex(ValueError, "engine pins changed"):
                    self.tick(0)
                self.assertEqual(self.fake.steps, [])

    def test_a_failed_stage_stops_before_submit_and_a_failed_submit_keeps_the_stage_on_record(self):
        self.fake.fail = "surya-stage-crops"
        with self.assertRaises(RuntimeError):
            self.tick(0)
        self.assertEqual([step[1] for step in self.fake.steps], ["surya-stage-crops"])
        self.assertNotIn("last_run", self.state["surya"])
        self.fake.steps.clear()
        self.fake.fail = "surya-submit"
        with self.assertRaises(RuntimeError):
            self.tick(0)
        self.assertEqual([step["step"] for step in self.state["surya"]["last_run"]["steps"]],
                         ["surya-stage-crops"])

    def test_missing_status_or_app_counts_run_nothing(self):
        with self.assertRaises(ValueError):
            batch.tick(self.config, self.state, self.fake, lambda: None, NOW,
                       server_status=surya_status(0).replace("usnm_surya", "usnm_other"))
        with self.assertRaises(ValueError):
            batch.tick(self.config, self.state, self.fake, lambda: None, NOW)
        self.assertEqual(self.fake.steps, [])

    def test_drain_runs_nothing_and_needs_no_status(self):
        event = batch.tick(self.config, self.state, self.fake, lambda: None, NOW, drain=True)
        self.assertEqual((self.fake.steps, event["skipped"]), ([], "draining"))

    def test_watermark_flags_override_the_config(self):
        self.tick(150, low_watermark=200, high_watermark=300)
        self.assertEqual(self.fake.steps[1][2]["SURYA_MAX"], 75)

    def test_steps_go_through_step_command_with_the_vars_in_order(self):
        transport = batch.Transport(self.config, Path("."))
        with patch.object(transport, "run", return_value="") as run:
            batch.tick(self.config, self.state, transport, lambda: None, NOW,
                       server_status=surya_status(10))
        crops = run.call_args_list[0].args[0]
        self.assertIn("surya-stage-crops", crops)
        self.assertEqual(sum(arg == "--var" for arg in crops), 7)
        submit = run.call_args_list[1].args[0]
        self.assertEqual(submit, self.config["step_command"] + [
            "--env", "dev", "--target", "bridge", "--step", "surya-submit", "--wait-mins", "30",
            "--var", "SURYA_CAMPAIGN=surya-wave1", "--var", "SURYA_MAX=195"])

    def test_main_what_if_prints_the_steps_and_runs_nothing(self):
        directory = Path(__file__).parent.resolve() / (".surya-main-" + uuid.uuid4().hex)
        directory.mkdir()
        try:
            path = directory / "config.json"
            path.write_text(json.dumps(self.config))
            out = io.StringIO()
            with patch.object(batch.subprocess, "run", side_effect=AssertionError("offline")), \
                    patch.object(batch.urllib.request, "urlopen", side_effect=AssertionError("offline")), \
                    contextlib.redirect_stdout(out):
                self.assertEqual(batch.main(["--config", str(path), "--state-dir", str(directory / "state")]), 0)
            event = json.loads(out.getvalue())
            self.assertFalse(event["apply"])
            steps = event["steps_if_at_or_below_low_watermark"]
            self.assertEqual([step["step"] for step in steps], ["surya-stage-crops", "surya-submit"])
            self.assertEqual(steps[1]["vars"]["SURYA_MAX"], 200)
            self.assertFalse((directory / "state").exists())
        finally:
            for file in directory.iterdir():
                file.unlink()
            directory.rmdir()

    def test_main_apply_ticks_under_the_host_lock_and_may_change_the_config_later(self):
        directory = Path(__file__).parent.resolve() / (".surya-apply-" + uuid.uuid4().hex)
        directory.mkdir()
        try:
            path = directory / "config.json"
            status = directory / "status.xml"
            status.write_text(surya_status(0))
            fake = SuryaFake()
            fake.lock = lambda: contextlib.nullcontext()
            fake.server_status = lambda: batch.parse_server_status(status.read_bytes())
            for high in (400, 300):
                path.write_text(json.dumps({**self.config, "high_watermark": high}))
                fake.steps.clear()
                out = io.StringIO()
                with patch.object(batch, "Transport", return_value=fake), \
                        contextlib.redirect_stdout(out):
                    self.assertEqual(batch.main(["--config", str(path), "--state-dir", str(directory),
                                                 "--apply"]), 0)
                self.assertEqual(json.loads(out.getvalue())["ran"], ["surya-stage-crops", "surya-submit"])
                self.assertEqual(fake.steps[1][2]["SURYA_MAX"], min(200, high // 2))
            saved = json.loads((directory / "state.json").read_text())
            self.assertEqual(saved["surya"]["last_run"]["steps"][1]["vars"]["SURYA_MAX"], 150)
            self.assertEqual(saved["surya"]["engine"]["SURYA_ENGINE_LLAMA"], "sha256:" + "2" * 64)
        finally:
            for file in directory.iterdir():
                file.unlink()
            directory.rmdir()

    def test_an_ocr_config_still_ticks_as_before(self):
        fake = Fake()
        event = batch.tick(config(), {}, fake, lambda: None, NOW)
        self.assertEqual(event["event"], "boinc_batch_tick")
        self.assertEqual([action[0] for action in fake.actions], ["export", "submit", "submit"])


if __name__ == "__main__":
    unittest.main()
