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

    def tick(self):
        return batch.tick(self.config, self.state, self.fake,
                          lambda: self.saved.append(copy.deepcopy(self.state)), NOW)

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
            self.assertFalse((directory / "state.json").exists())
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


if __name__ == "__main__":
    unittest.main()
