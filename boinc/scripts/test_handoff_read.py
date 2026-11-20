"""python3 -m unittest -v boinc/scripts/test_handoff_read.py

The ssh read steps over the hand-off root (#1367): the host helper, onprem-step.sh's routing, and
the controller's --ssh-reads mode.
"""

import hashlib
import json
import shutil
import pathlib
import os
import subprocess
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

import importlib.util


def load(name):
    spec = importlib.util.spec_from_file_location(name, Path(__file__).with_name(f"{name}.py"))
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


batch = load("batch_scheduler")
read = load("handoff_read")

SCRIPT = Path(__file__).resolve().parent / "onprem-step.sh"
JOB = "a" * 64


def journal(root, job=JOB, body=None):
    directory = root / "dispatch/jobs" / job
    directory.mkdir(parents=True, exist_ok=True)
    (directory / "record.json").write_text(json.dumps(body or {"job": {"id": job}}))


def rewrite(root, job, body):
    """A write as LocalStore makes one: a new file renamed into place."""
    path = root / "dispatch/jobs" / job / "record.json"
    partial = path.with_name("record.json.tmp")
    partial.write_text(json.dumps(body))
    os.replace(partial, path)


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

    def test_read_journals_reads_again_only_what_changed_then_gives_up(self):
        jobs = [c * 64 for c in "abc"]
        for job in jobs:
            journal(self.root, job)
        name = f"dispatch/jobs/{JOB}/record.json"
        real, reads = read.read_with_stat, []

        def flaky(path, limit):
            found = real(path, limit)
            reads.append(path.parent.name)
            if path.parent.name == JOB and reads.count(JOB) < 3:
                rewrite(self.root, JOB, {"job": {"id": JOB}, "n": reads.count(JOB)})
            return found

        sleeps = []
        with patch.object(read, "read_with_stat", flaky):
            out = json.loads(read.read_journals(self.root, sleep=sleeps.append))["journals"]
        self.assertEqual(out[name]["record"]["n"], 2)
        self.assertEqual(out[name]["sha256"], hashlib.sha256((self.root / name).read_bytes()).hexdigest())
        self.assertEqual(len(sleeps), 2)
        # Each of the other journals was read once; only the one that moved was read again.
        self.assertEqual(sorted(reads), sorted(jobs + [JOB, JOB]))

        def never_settles(path, limit):
            found = real(path, limit)
            rewrite(self.root, path.parent.name, {"job": {"id": path.parent.name}, "n": len(sleeps)})
            return found

        sleeps.clear()
        with patch.object(read, "read_with_stat", never_settles), \
                self.assertRaisesRegex(read.ReadError, "changed during read"):
            read.read_journals(self.root, attempts=3, sleep=sleeps.append)
        self.assertEqual(len(sleeps), 2)

    def test_journals_that_come_and_go_between_rounds_settle(self):
        journal(self.root)
        gone, late = "b" * 64, "c" * 64
        journal(self.root, gone)
        real, rounds = read.listing, []

        def moving(root):
            names = real(root)
            rounds.append(1)
            if len(rounds) == 1:
                journal(self.root, late)
                (self.root / f"dispatch/jobs/{gone}/record.json").unlink()
            return names

        sleeps = []
        with patch.object(read, "listing", moving):
            out = json.loads(read.read_journals(self.root, sleep=sleeps.append))["journals"]
        self.assertEqual(sorted(out), [f"dispatch/jobs/{job}/record.json" for job in (JOB, late)])
        self.assertEqual(len(sleeps), 1)

    def test_a_job_directory_removed_while_listing_is_a_change(self):
        journal(self.root)
        gone = "d" * 64
        journal(self.root, gone)
        real, calls = pathlib.Path.iterdir, []

        def removing(path):
            entries = list(real(path))
            calls.append(1)
            if len(calls) == 1 and path.name == "jobs":
                # The archive removes an emptied job directory after the listing saw it.
                shutil.rmtree(self.root / f"dispatch/jobs/{gone}")
            return iter(entries)

        sleeps = []
        with patch.object(pathlib.Path, "iterdir", removing):
            out = json.loads(read.read_journals(self.root, sleep=sleeps.append))["journals"]
        self.assertEqual(sorted(out), [f"dispatch/jobs/{JOB}/record.json"])
        self.assertEqual(len(sleeps), 1)

    def test_failures_in_a_later_round_other_than_a_vanished_journal_are_not_retried(self):
        journal(self.root)
        journal(self.root, "b" * 64)
        name = f"dispatch/jobs/{JOB}/record.json"
        real, seen = read.read_with_stat, []

        def grows(path, limit):
            seen.append(path.parent.name)
            if seen.count(JOB) == 1:
                found = real(path, limit)
                if path.parent.name == JOB:
                    rewrite(self.root, JOB, {"job": {"id": JOB}, "pad": "x" * 64})
                return found
            raise read.ReadError("object exceeds limit")

        sleeps = []
        with patch.object(read, "read_with_stat", grows), \
                self.assertRaisesRegex(read.ReadError, "invalid/oversized"):
            read.read_journals(self.root, sleep=sleeps.append)
        self.assertEqual(len(sleeps), 1)

        seen.clear()
        sleeps.clear()

        def vanishes(path, limit):
            found = real(path, limit)
            if path.parent.name == JOB:
                (self.root / name).unlink(missing_ok=True)
            return found

        with patch.object(read, "read_with_stat", vanishes), \
                self.assertRaisesRegex(read.ReadError, "changed during read"):
            read.read_journals(self.root, attempts=1, sleep=sleeps.append)
        out = json.loads(read.read_journals(self.root, sleep=sleeps.append))["journals"]
        self.assertEqual(sorted(out), [f"dispatch/jobs/{'b' * 64}/record.json"])

    def test_inventory_past_the_cap_is_refused_on_every_round(self):
        for job in ("a" * 64, "b" * 64, "c" * 64):
            journal(self.root, job)
        with patch.object(read, "MAX_JOBS", 2), \
                self.assertRaisesRegex(read.ReadError, "inventory limit"):
            read.read_journals(self.root, sleep=lambda _: self.fail("a cap is not retried"))
        real, rounds = read.listing, []

        def grows(root):
            rounds.append(1)
            if len(rounds) == 2:
                journal(self.root, "d" * 64)
            return real(root)

        # A journal that takes the inventory past the cap while it is read fails the round's
        # check listing at once, never a change to read again.
        sleeps = []
        with patch.object(read, "MAX_JOBS", 3), patch.object(read, "listing", grows), \
                self.assertRaisesRegex(read.ReadError, "inventory limit"):
            read.read_journals(self.root, sleep=sleeps.append)
        self.assertEqual((len(rounds), sleeps), (2, []))

    def test_read_journals_does_not_retry_other_failures(self):
        journal(self.root, "c" * 64, {"job": {"id": "d" * 64}})
        sleeps = []
        with self.assertRaisesRegex(read.ReadError, "identity mismatch"):
            read.read_journals(self.root, sleep=sleeps.append)
        self.assertEqual(sleeps, [])

    def test_root_must_be_runner_owned_and_mode_0700(self):
        self.root.chmod(0o700)
        read.check_root(self.root)
        self.root.chmod(0o770)
        with self.assertRaisesRegex(read.ReadError, "mode 0700"):
            read.check_root(self.root)
        self.assertEqual(read.main(["--root", str(self.root), "read-journals"]), 1)
        self.root.chmod(0o700)

    def test_reads_are_bounded_by_what_they_read_not_by_a_size_check(self):
        big = self.root / "big.json"
        big.write_bytes(b"x" * 20)
        with self.assertRaisesRegex(read.ReadError, "exceeds limit"):
            read.read_bounded(big, 19)
        self.assertEqual(read.read_bounded(big, 20), b"x" * 20)
        (self.root / "link").symlink_to(big)
        with self.assertRaises(read.ReadError):
            read.read_bounded(self.root / "link", 100)

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
        with self.assertRaisesRegex(ValueError, "every hand-off step"):
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
