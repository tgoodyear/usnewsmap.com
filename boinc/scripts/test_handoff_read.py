"""python3 -m unittest -v boinc/scripts/test_handoff_read.py

The ssh read steps over the hand-off root (#1367): the host helper, onprem-step.sh's routing, and
the controller's --ssh-reads mode.
"""

import hashlib
import json
import os
import subprocess
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

import batch_scheduler as batch
import handoff_read as read

SCRIPT = Path(__file__).resolve().parent / "onprem-step.sh"
JOB = "a" * 64


def journal(root, job=JOB, body=None):
    directory = root / "dispatch/jobs" / job
    directory.mkdir(parents=True, exist_ok=True)
    (directory / "record.json").write_text(json.dumps(body or {"job": {"id": job}}))


class HelperTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.root = Path(self.tmp.name).resolve()

    def tearDown(self):
        self.tmp.cleanup()

    def test_read_object_returns_json_and_refuses_unsafe_paths(self):
        (self.root / "scheduler").mkdir()
        (self.root / "scheduler/inventory.json").write_text('{"nonce": "n"}')
        self.assertEqual(json.loads(read.read_object(self.root, "scheduler/inventory.json")), {"nonce": "n"})
        for name in ("../x", "/etc/passwd", "a/../b", "scheduler//inventory.json", ".hidden"):
            with self.subTest(name=name), self.assertRaises(read.ReadError):
                read.read_object(self.root, name)
        outside = self.root.parent / (self.root.name + "-out.json")
        outside.write_text("{}")
        self.addCleanup(outside.unlink)
        (self.root / "escape.json").symlink_to(outside)
        with self.assertRaises(read.ReadError):
            read.read_object(self.root, "escape.json")

    def test_missing_object_is_null_only_when_optional(self):
        self.assertEqual(read.read_object(self.root, "nope.json", optional=True), "null")
        with self.assertRaises(read.ReadError):
            read.read_object(self.root, "nope.json")

    def test_read_journals_carries_digests_and_checks_identity(self):
        journal(self.root)
        journal(self.root, "b" * 64)
        out = json.loads(read.read_journals(self.root))["journals"]
        name = f"dispatch/jobs/{JOB}/record.json"
        self.assertEqual(out[name]["sha256"], hashlib.sha256((self.root / name).read_bytes()).hexdigest())
        self.assertEqual(sorted(out), sorted([name, f"dispatch/jobs/{'b' * 64}/record.json"]))
        journal(self.root, "c" * 64, {"job": {"id": "d" * 64}})
        with self.assertRaisesRegex(read.ReadError, "identity mismatch"):
            read.read_journals(self.root)

    def test_missing_jobs_directory_fails_closed(self):
        with self.assertRaisesRegex(read.ReadError, "missing or unsafe"):
            read.read_journals(self.root)


class StepTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.root = Path(self.tmp.name).resolve()
        self.state = self.root / "state"
        self.state.mkdir(mode=0o700)
        (self.root / "server.env").write_text("SERVER_IMAGE=localhost/x:abc\n")
        (self.root / "bridge.env").write_text("KEY_VAULT_NAME=kv\n")

    def tearDown(self):
        self.tmp.cleanup()

    def step(self, step, *variables):
        env = {"PATH": os.environ["PATH"], "USNM_BOINC_CONF": str(self.root),
               "USNM_BOINC_STEP_RUN": str(self.root), "USNM_BOINC_HANDOFF_ROOT": str(self.state)}
        argv = ["bash", str(SCRIPT), "--env", "prod", "--target", "handoff", "--step", step, "--wait-mins", "1"]
        for variable in variables:
            argv += ["--var", variable]
        return subprocess.run(argv, env=env, capture_output=True, text=True, timeout=30)

    def test_read_steps_run_on_the_host_without_a_container(self):
        (self.state / "scheduler").mkdir()
        (self.state / "scheduler/inventory.json").write_text('{"nonce": "n"}')
        done = self.step("read-object", "OBJECT=scheduler/inventory.json")
        self.assertEqual((done.returncode, json.loads(done.stdout)), (0, {"nonce": "n"}), done.stderr)
        absent = self.step("read-object", "OBJECT=nope.json", "OBJECT_OPTIONAL=1")
        self.assertEqual(absent.stdout.strip(), "null")
        journal(self.state)
        self.assertEqual(len(json.loads(self.step("read-journals").stdout)["journals"]), 1)

    def test_read_steps_take_no_other_variable(self):
        done = self.step("read-journals", "JOB_ID=" + "a" * 64)
        self.assertNotEqual(done.returncode, 0)
        self.assertIn("takes only", done.stderr)
        self.assertNotEqual(self.step("read-object", "OBJECT=../x").returncode, 0)


class ControllerTests(unittest.TestCase):
    def transport(self, **kwargs):
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        config = {"env": "prod", "account": "exchange123", "plan": "q",
                  "step_command": ["ssh", "root@boinc.example", "--"]}
        return batch.Transport(config, Path(tmp.name), onprem_handoff=("claim", "collect", "export"),
                               local_lock=True, ssh_reads=True, **kwargs)

    def test_ssh_reads_needs_the_rest_of_the_on_prem_mode(self):
        config = {"env": "prod", "account": "a", "step_command": ["ssh", "h", "--"]}
        with self.assertRaisesRegex(ValueError, "ssh-reads needs"):
            batch.Transport(config, Path("."), ssh_reads=True)
        with self.assertRaisesRegex(ValueError, "ssh-reads needs"):
            batch.Transport(config, Path("."), ssh_reads=True, local_lock=True,
                            onprem_handoff=("claim",))

    def test_reads_go_over_the_runner_and_never_to_azure(self):
        transport = self.transport()
        name = f"dispatch/jobs/{JOB}/record.json"
        record = {"job": {"id": JOB}}
        calls = []

        def run(argv, label, **kwargs):
            calls.append(argv)
            step = argv[argv.index("--step") + 1]
            if step == "read-journals":
                return json.dumps({"journals": {name: {"sha256": "f" * 64, "record": record}}})
            return json.dumps({"nonce": "n"}) if "OBJECT=scheduler/inventory.json" in argv else "null"

        with patch.object(transport, "run", side_effect=run):
            self.assertEqual(transport.blob("scheduler/inventory.json"), {"nonce": "n"})
            self.assertEqual(transport.records(), [record])
            self.assertIsNone(transport.deadline_retry(JOB, "e" * 64))
            with self.assertRaisesRegex(ValueError, "forbidden"):
                transport.az("list")
            with self.assertRaises(ValueError):
                transport.blob("../escape")
        self.assertTrue(all(argv[:3] == ["ssh", "root@boinc.example", "--"] for argv in calls))
        self.assertIn("OBJECT_OPTIONAL=1", calls[-1])
        self.assertFalse(any(v.startswith("EXPECTED_HANDOFF_URL") for argv in calls for v in argv))
        self.assertEqual(transport.handoff_url, "/handoff-state")


if __name__ == "__main__":
    unittest.main()
