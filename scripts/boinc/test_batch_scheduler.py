import contextlib
import copy
import importlib.util
import io
import json
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
        self.crash_export = False
        self.crash_submit = False
        self.hold = False
        self.terminal = False

    def inventory(self):
        return {"schema_version": 1, "nonce": "a" * 32, "created_at": NOW,
                "active_hosts": self.hosts,
                "pending_workunits": self.other + sum(job["stage"] == "submitted" and not job["spool"]
                                                       for job in self.jobs.values()),
                "jobs": copy.deepcopy(self.jobs)}

    def records(self):
        return copy.deepcopy(self.rows)

    def export(self, pages):
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

    def test_transport_uses_real_steps_and_waits(self):
        transport = batch.Transport(self.config, Path("."))
        with patch.object(transport, "run", return_value="") as run:
            transport.export(self.config["pages"][:2])
            self.assertIn("--scan", run.call_args_list[0].args[0])
            self.assertIn("--wait-mins", run.call_args_list[1].args[0])
            self.assertIn("HANDOFF_PLAN=batch-test", run.call_args_list[1].args[0])
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


if __name__ == "__main__":
    unittest.main()
