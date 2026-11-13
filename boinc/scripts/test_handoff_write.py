"""python3 -m unittest -v boinc/scripts/test_handoff_write.py

The ssh write path (#1367): held-pages parts written on the BOINC host, and spool-out, collect and
export sent over the step runner with the on-disk spool layout left to the hand-off job.
"""

import io
import json
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
lock = load("handoff_lock")
write = load("handoff_write")

SCRIPT = Path(__file__).resolve().parent / "onprem-step.sh"
PART = "scheduler/held-pages/q.0000.json"
GOOD = json.dumps({"schema_version": 1, "pages": {"doc1": ["a" * 64]}})


class HelperTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.root = Path(self.tmp.name).resolve()

    def tearDown(self):
        self.tmp.cleanup()

    def test_parts_are_written_and_only_this_plans_stale_parts_go(self):
        directory = self.root / "scheduler/held-pages"
        directory.mkdir(parents=True)
        (directory / "q.0001.json").write_text("{}")
        (directory / "other.0000.json").write_text("{}")
        result = write.held_pages(self.root, "q", {PART: GOOD})
        self.assertEqual(result, {"removed": 1, "written": 1})
        self.assertEqual((self.root / PART).read_text(), GOOD)
        self.assertFalse((directory / "q.0001.json").exists())
        self.assertTrue((directory / "other.0000.json").exists())
        self.assertEqual(write.held_pages(self.root, "q", {})["removed"], 1)

    def test_other_plans_names_and_bad_json_are_refused(self):
        for parts in ({"scheduler/held-pages/other.0000.json": "{}"}, {"scheduler/../x.json": "{}"},
                      {PART: 1}):
            with self.subTest(parts=parts), self.assertRaises(write.WriteError):
                write.held_pages(self.root, "q", parts)
        with self.assertRaises(write.WriteError):
            write.held_pages(self.root, "q", {PART: "not json"})
        with self.assertRaises(write.WriteError):
            write.held_pages(self.root, "../q", {})

    def test_parts_the_reader_would_refuse_are_refused_before_anything_changes(self):
        bad = ["{}", "null", "[]", json.dumps({"schema_version": 2, "pages": {}}),
               json.dumps({"schema_version": 1, "pages": {}, "extra": 1}),
               json.dumps({"schema_version": 1, "pages": {"d": []}}),
               json.dumps({"schema_version": 1, "pages": {"d": ["A" * 64]}}),
               json.dumps({"schema_version": 1, "pages": {"d": ["a" * 63]}}),
               json.dumps({"schema_version": 1, "pages": {"../d": ["a" * 64]}}),
               json.dumps({"schema_version": 1, "pages": []})]
        for body in bad:
            with self.subTest(body=body[:40]), self.assertRaises(write.WriteError):
                write.held_pages(self.root, "q", {PART: body})
        self.assertFalse((self.root / "scheduler").exists())
        with patch.object(write, "MAX_BYTES", 10), self.assertRaises(write.WriteError):
            write.held_pages(self.root, "q", {PART: GOOD})

    def test_the_readers_list_count_bound_counts_other_plans(self):
        directory = self.root / "scheduler/held-pages"
        directory.mkdir(parents=True)
        (directory / "other.0000.json").write_text(GOOD)
        with patch.object(write, "MAX_LISTS", 1), self.assertRaisesRegex(write.WriteError, "more than"):
            write.held_pages(self.root, "q", {PART: GOOD})

    def test_a_boolean_or_float_schema_version_is_refused(self):
        for version in (True, 1.0):
            body = json.dumps({"schema_version": version, "pages": {}})
            with self.subTest(version=version), self.assertRaises(write.WriteError):
                write.held_pages(self.root, "q", {PART: body})

    def test_scheduler_valid_plan_names_are_accepted(self):
        part = "scheduler/held-pages/p_-q.0000.json"
        write.held_pages(self.root, "-q", {part: GOOD})
        self.assertTrue((self.root / part).exists())
        self.assertEqual(batch.held_pages_stem("-q"), write.stem("-q"))
        self.assertEqual(batch.held_pages_stem("queue-a"), "queue-a")
        # The name starts with a letter, so the store lists it, and it is not another plan's.
        self.assertTrue(batch.held_pages_stem("-q")[0].isalnum())
        with self.assertRaises(write.WriteError):
            write.held_pages(self.root, "-q", {"scheduler/held-pages/-q.0000.json": GOOD})

    def test_main_reads_stdin(self):
        body = json.dumps({"parts": {PART: GOOD}})
        self.assertEqual(write.main(["--root", str(self.root), "held-pages", "--plan", "q"],
                                    stdin=io.StringIO(body)), 0)
        self.assertTrue((self.root / PART).exists())
        self.assertEqual(write.main(["--root", str(self.root), "held-pages", "--plan", "q"],
                                    stdin=io.StringIO("nope")), 1)


class StepTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.root = Path(self.tmp.name).resolve()
        self.state = self.root / "state"
        self.state.mkdir(mode=0o700)
        (self.root / "server.env").write_text("SERVER_IMAGE=localhost/x:abc\n")
        (self.root / "bridge.env").write_text("KEY_VAULT_NAME=kv\n")
        (self.root / "handoff.env").write_text("".join(f"{k}={v}\n" for k, v in {
            "HANDOFF_URL": "/handoff-state", "COSMOS_ENDPOINT": "https://c.documents.azure.com:443/",
            "CURATED_URL": "https://l.blob.core.windows.net/curated",
            "REFERENCE_URL": "https://l.blob.core.windows.net/reference",
            "OUT_URL": "https://e.blob.core.windows.net/ocr-out", "QUEUE_URL": "https://e.queue.core.windows.net",
            "IDENTITY_ENDPOINT": "http://localhost:1/t", "IMDS_ENDPOINT": "http://localhost:1"}.items()))
        self.bin = self.root / "bin"
        self.bin.mkdir()
        (self.bin / "flock").write_text("#!/bin/sh\nexit 0\n")
        (self.bin / "flock").chmod(0o755)
        (self.bin / "timeout").write_text('#!/bin/sh\nshift\nexec "$@"\n')
        (self.bin / "timeout").chmod(0o755)

    def tearDown(self):
        self.tmp.cleanup()

    def step(self, target, step, *variables, stdin=""):
        env = {"PATH": f"{self.bin}:{os.environ['PATH']}", "USNM_BOINC_CONF": str(self.root),
               "USNM_BOINC_STEP_RUN": str(self.root), "USNM_BOINC_HANDOFF_ROOT": str(self.state)}
        argv = ["bash", str(SCRIPT), "--env", "prod", "--target", target, "--step", step, "--wait-mins", "1"]
        for variable in variables:
            argv += ["--var", variable]
        return subprocess.run(argv, env=env, input=stdin, capture_output=True, text=True, timeout=30)

    def test_write_held_pages_step_is_fenced_and_writes(self):
        lock.acquire(self.state, "a-1", "h", 1)
        body = json.dumps({"parts": {PART: GOOD}})
        refused = self.step("handoff", "write-held-pages", "HELD_PLAN=q", "EXPECTED_HANDOFF_URL=/handoff-state", "LOCK_HOLDER=zz",
                            "LOCK_GENERATION=1", stdin=body)
        self.assertIn("not this controller's", refused.stderr)
        self.assertFalse((self.state / PART).exists())
        done = self.step("handoff", "write-held-pages", "HELD_PLAN=q", "EXPECTED_HANDOFF_URL=/handoff-state", "LOCK_HOLDER=a-1",
                         "LOCK_GENERATION=1", stdin=body)
        self.assertEqual(done.returncode, 0, done.stderr)
        self.assertTrue((self.state / PART).exists())
        self.assertNotEqual(self.step("handoff", "write-held-pages", "JOB_ID=x", stdin=body).returncode, 0)
        dash = json.dumps({"parts": {"scheduler/held-pages/p_-q.0000.json": GOOD}})
        again = self.step("handoff", "write-held-pages", "HELD_PLAN=-q", "EXPECTED_HANDOFF_URL=/handoff-state", "LOCK_HOLDER=a-1",
                          "LOCK_GENERATION=1", stdin=dash)
        self.assertEqual(again.returncode, 0, again.stderr)
        self.assertTrue((self.state / "scheduler/held-pages/p_-q.0000.json").exists())

    def test_write_held_pages_checks_the_fence_again_after_the_step_lock(self):
        lock.acquire(self.state, "a-1", "h", 1)
        binaries = self.bin
        record = self.state / "scheduler/controller.lock.json"
        (binaries / "flock").write_text(
            f'#!/bin/sh\npython3 -c "import json;p=\'{record}\';r=json.load(open(p));'
            f'r[\'holder\']=\'b-2\';r[\'generation\']=2;json.dump(r,open(p,\'w\'))"\nexit 0\n')
        (binaries / "flock").chmod(0o755)
        env = {"PATH": f"{binaries}:{os.environ['PATH']}", "USNM_BOINC_CONF": str(self.root),
               "USNM_BOINC_STEP_RUN": str(self.root), "USNM_BOINC_HANDOFF_ROOT": str(self.state)}
        argv = ["bash", str(SCRIPT), "--env", "prod", "--target", "handoff", "--step", "write-held-pages",
                "--wait-mins", "1", "--var", "HELD_PLAN=q", "--var", "EXPECTED_HANDOFF_URL=/handoff-state",
                "--var", "LOCK_HOLDER=a-1",
                "--var", "LOCK_GENERATION=1"]
        done = subprocess.run(argv, env=env, input=json.dumps({"parts": {PART: GOOD}}),
                              capture_output=True, text=True, timeout=30)
        self.assertNotEqual(done.returncode, 0)
        self.assertIn("expired while waiting", done.stderr)
        self.assertFalse((self.state / PART).exists())

    def test_bridge_spool_out_is_fenced_when_given_a_holder(self):
        lock.acquire(self.state, "a-1", "h", 1)
        refused = self.step("bridge", "spool-out", "JOB_ID=" + "a" * 64, "LOCK_HOLDER=zz", "LOCK_GENERATION=1")
        self.assertIn("not this controller's", refused.stderr)
        half = self.step("bridge", "spool-out", "JOB_ID=x", "LOCK_HOLDER=a-1")
        self.assertIn("go together", half.stderr)


class ControllerTests(unittest.TestCase):
    def transport(self):
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        config = {"env": "prod", "account": "exchange123", "plan": "q",
                  "step_command": ["ssh", "root@boinc.example", "--"]}
        return batch.Transport(config, Path(tmp.name), onprem_handoff=("claim", "collect", "export"),
                               local_lock=True, ssh_reads=True)

    def test_spool_out_collect_and_export_are_fenced_and_stay_on_the_runner(self):
        transport = self.transport()
        calls = []

        def run(argv, label, **kwargs):
            calls.append(argv)
            return json.dumps({"generation": 4}) if label.endswith("lock-acquire") else ""

        with patch.object(transport, "run", side_effect=run), patch.object(batch, "LOCK_HEARTBEAT_SECS", 3600):
            with transport.lock():
                transport.collect("a" * 64, "/project/results/x/y")
                transport.step("handoff", "export", HANDOFF_BATCH="b")
        by_step = {argv[argv.index("--step") + 1]: argv for argv in calls}
        for name in ("spool-out", "collect", "export"):
            self.assertIn("LOCK_GENERATION=4", by_step[name], name)
            self.assertTrue(by_step[name][:3] == ["ssh", "root@boinc.example", "--"], name)
        self.assertIn("HANDOFF_URL=/handoff-state", by_step["spool-out"])
        self.assertIn("HANDOFF_SPOOL=/project/results/x/y", by_step["spool-out"])
        self.assertFalse(any("az" == argv[0] for argv in calls))

    def test_spool_out_is_unfenced_without_the_lock(self):
        config = {"env": "prod", "account": "a", "step_command": ["ssh", "h", "--"]}
        transport = batch.Transport(config, Path(tempfile.mkdtemp()))
        with patch.object(transport, "run") as run:
            transport.step("bridge", "spool-out", JOB_ID="a" * 64)
        self.assertFalse(any(v.startswith("LOCK_") for v in run.call_args.args[0]))

    def test_held_pages_go_over_the_runner_on_stdin(self):
        transport = self.transport()
        with patch.object(transport, "run", return_value="{}") as run:
            self.assertTrue(transport.publish_held({"doc1": ["a" * 64]}))
        argv, kwargs = run.call_args.args[0], run.call_args.kwargs
        self.assertIn("HELD_PLAN=q", argv)
        self.assertIn(PART, json.loads(kwargs["stdin"])["parts"])

    def test_held_pages_step_carries_the_expected_url_and_the_marker_names_the_host(self):
        transport = self.transport()
        with patch.object(transport, "run", return_value="{}") as run:
            transport.publish_held({"doc1": ["a" * 64]})
        self.assertIn("EXPECTED_HANDOFF_URL=/handoff-state", run.call_args.args[0])
        marker = (transport.directory / "held-pages.sha256").read_text()
        other = self.transport()
        other.config["step_command"] = ["ssh", "root@other.example", "--"]
        other.directory = transport.directory
        with patch.object(other, "run", return_value="{}") as again:
            self.assertTrue(other.publish_held({"doc1": ["a" * 64]}))
        self.assertTrue(again.called)
        self.assertNotEqual(marker, (transport.directory / "held-pages.sha256").read_text())

    def test_a_marker_from_another_destination_does_not_suppress_the_first_upload(self):
        ssh = self.transport()
        with patch.object(ssh, "run", return_value="{}"):
            ssh.publish_held({"doc1": ["a" * 64]})
        azure = batch.Transport({"env": "prod", "account": "exchange123", "plan": "q"}, ssh.directory)
        with patch.object(azure, "az", return_value="[]") as az:
            self.assertTrue(azure.publish_held({"doc1": ["a" * 64]}))
        self.assertTrue(az.called)


if __name__ == "__main__":
    unittest.main()
