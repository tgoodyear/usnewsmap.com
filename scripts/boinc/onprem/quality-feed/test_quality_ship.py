"""Exercise the shipper's Qwen admission forwarding without network or live state."""
import json
import os
from pathlib import Path
import shutil
import subprocess
import unittest
import uuid

HERE = Path(__file__).resolve().parent


class ShipperTest(unittest.TestCase):
    def setUp(self):
        self.root = HERE / (".ship-test-" + uuid.uuid4().hex)
        self.root.mkdir()
        self.addCleanup(shutil.rmtree, self.root)
        self.bin = self.root / "bin"
        self.bin.mkdir()
        self.env = {**os.environ, "PATH": str(self.bin) + os.pathsep + os.environ["PATH"],
                    "SHIP_TEST_ROOT": str(self.root)}
        self.stub("curl", """#!/bin/sh
cat <<EOF
<server_status><database_file_states>
<results_ready_to_send>${SHIP_TEST_READY:-0}</results_ready_to_send>
<tasks_by_app><app><name>usnm_quality</name><unsent>${SHIP_TEST_QUALITY_READY:-0}</unsent></app>
<app><name>usnm_qwen</name><unsent>${SHIP_TEST_READY:-0}</unsent></app>
</tasks_by_app></database_file_states></server_status>
EOF
""")
        self.stub("tar", "#!/bin/sh\nexit 0\n")
        self.stub("mktemp", "#!/bin/sh\necho \"$SHIP_TEST_ROOT/step.err\"\n")
        self.stub("ssh", """#!/usr/bin/env python3
import json, os, shlex, sys
from pathlib import Path
root = Path(os.environ["SHIP_TEST_ROOT"])
args = sys.argv[1:]
remote = next(i for i, arg in enumerate(args) if arg.startswith("root@")) + 1
# OpenSSH joins remote arguments before invoking a shell: do not accidentally
# validate a local argv list that production will never receive.
args = args[:remote] + shlex.split(" ".join(args[remote:]))
with (root / "calls.jsonl").open("a") as f:
    f.write(json.dumps(args) + "\\n")
locked = os.environ.get("SHIP_TEST_LOCKED_STEP")
if locked and locked in args:
    print(os.environ.get("SHIP_TEST_LOCKED_MESSAGE",
                         "error: another bridge step is running on this host"), file=sys.stderr)
    sys.exit(1)
if "df" in args:
    print("Avail\\n100")
    if "/srv/usnm-boinc/project/quality" in args:
        print(os.environ.get("SHIP_TEST_BULK_FREE", "100"))
    if "/srv/usnm-boinc/project/usnmocr/download" in args:
        print(os.environ.get("SHIP_TEST_BULK_FREE", "100"))
elif "lvs" in args:
    print(json.dumps({"report": [{"lv": [{"lv_size": os.environ.get(
        "SHIP_TEST_POOL_FREE_BYTES", str(100 * 1024**3)), "data_percent": "0"}]}]}))
elif "/project/ops/qwen-feed/qwen_submit.py" in args:
    preflight = "--preflight" in args
    phase = "preflight" if preflight else "locked_recheck"
    code = int(os.environ.get("SHIP_TEST_PREFLIGHT_EXIT" if preflight
                              else "SHIP_TEST_PUBLISH_EXIT", "0"))
    print(json.dumps({"phase": phase, "status": "deferred" if code == 75 else "admitted",
                      "reason": "fresh_original_watermark" if code == 75 else None}))
    sys.exit(code)
elif "python3" in args and "-" in args:
    sys.stdin.read()
    if "--after-submit" in args:
        print("freed 0 bytes")
    else:
        print("/project/quality/test/incoming/c00001/sample.jsonl\\trus\\t1")
""")

    def stub(self, name, body):
        path = self.bin / name
        path.write_text(body)
        path.chmod(0o755)

    def run_ship(self, extra, native=True):
        chunk = self.root / "outbox/test/c00001"
        chunk.mkdir(parents=True, exist_ok=True)
        (chunk / "READY").write_text("1\n")
        return subprocess.run(
            ["bash", str(HERE / "quality-ship.sh"), "--campaign", "test",
             "--engine-image", "sha256:" + "a" * 64, "--outbox", str(self.root / "outbox"),
             *(["--qwen-engine-plan", "/project/plan.json", "--qwen-models", "/project/models"]
               if native else []),
             *extra], env=self.env, capture_output=True, text=True, timeout=30)

    def test_qwen_watermark_default_and_override_reach_submit(self):
        for extra, expected in (([], "1000"), (["--qwen-max-ready", "20000"], "20000")):
            with self.subTest(expected=expected):
                result = self.run_ship(extra)
                self.assertEqual(result.returncode, 0, result.stderr)
                calls = [json.loads(line) for line in (self.root / "calls.jsonl").read_text().splitlines()]
                submit = next(c for c in reversed(calls) if "/project/ops/qwen-feed/qwen_submit.py" in c)
                self.assertEqual(submit[submit.index("--max-ready") + 1], expected)
                self.assertEqual(submit[submit.index("--max-pages") + 1], "1")
                script = submit[submit.index("-c") + 1]
                probe = subprocess.run(
                    ["bash", "-c", script, "sh", "printf", "locked argv survives"],
                    capture_output=True, text=True, timeout=5)
                self.assertEqual(probe.returncode, 0, probe.stderr)
                self.assertEqual(probe.stdout, "locked argv survives")

    def test_stage_and_submit_do_not_wait_for_the_step_window(self):
        # The stage has its own lock on the host, and only native publication reads the clock.
        self.stub("date", "#!/bin/bash\nexit 9\n")
        self.stub("sleep", "#!/bin/bash\necho \"$@\" >> " + str(self.root / "slept") + "\n")
        result = self.run_ship(["--step-window-minutes", "15"], native=False)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertNotIn("waiting for the next step window", result.stderr)
        self.assertFalse((self.root / "slept").exists())
        self.assertIn("shipped 1 chunk(s)", result.stderr)

    def test_a_step_that_stays_locked_defers_and_retains_ready(self):
        # quality-submit never gets the lock: 60 tries 30 s apart, then exit 75, no acknowledgement.
        self.env["SHIP_TEST_LOCKED_STEP"] = "quality-submit"
        self.stub("sleep", "#!/bin/bash\necho \"$@\" >> " + str(self.root / "slept") + "\n")
        result = self.run_ship(["--step-window-minutes", "15"], native=False)
        self.assertEqual(result.returncode, 75, result.stderr)
        self.assertIn("quality-submit: the lock stayed held for 30 minutes; READY retained", result.stderr)
        self.assertEqual(len((self.root / "slept").read_text().split()), 60)
        self.assertTrue((self.root / "outbox/test/c00001/READY").exists())
        calls = [json.loads(line) for line in (self.root / "calls.jsonl").read_text().splitlines()]
        self.assertFalse(any("--after-submit" in c for c in calls))

    def test_a_step_that_fails_for_another_reason_is_not_deferred(self):
        self.env["SHIP_TEST_LOCKED_STEP"] = "quality-stage"
        self.env["SHIP_TEST_LOCKED_MESSAGE"] = "error: the step's container ended with status 1"
        result = self.run_ship([], native=False)
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertIn("ended with status 1", result.stderr)
        self.assertTrue((self.root / "outbox/test/c00001/READY").exists())

    def test_native_submit_waits_for_the_step_window_too(self):
        minutes = self.root / "minutes"
        minutes.write_text("40:00\n14:30\n")
        self.stub("date", f"""#!/bin/bash
head -1 {minutes}
sed -i.bak 1d {minutes}
""")
        self.stub("sleep", "#!/bin/bash\nexit 0\n")
        result = self.run_ship(["--step-window-minutes", "15"])
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stderr.count("waiting for the next step window"), 1)
        # At 14:30 of a 15-minute window, the lock wait ends with the window.
        calls = [json.loads(line) for line in (self.root / "calls.jsonl").read_text().splitlines()]
        submit = next(c for c in reversed(calls) if "/project/ops/qwen-feed/qwen_submit.py" in c)
        self.assertEqual(submit[submit.index("-w") + 1], "30")

    def test_step_window_is_off_by_default_and_checked(self):
        self.stub("date", "#!/bin/bash\nexit 9\n")
        self.assertEqual(self.run_ship([], native=False).returncode, 0)
        result = self.run_ship(["--step-window-minutes", "60"], native=False)
        self.assertEqual(result.returncode, 1)
        self.assertIn("--step-window-minutes", result.stderr)

    def test_full_watermark_defers_repeated_ticks_before_any_transfer_or_stage(self):
        self.env["SHIP_TEST_PREFLIGHT_EXIT"] = "75"
        for _ in range(2):
            result = self.run_ship(["--qwen-max-ready", "200"])
            self.assertEqual(result.returncode, 75, result.stderr)
            self.assertEqual(json.loads(result.stdout)["status"], "deferred")
            self.assertIn("transferred 0 pages; READY retained", result.stderr)
            self.assertNotIn("submitted,", result.stderr)
            self.assertEqual((self.root / "outbox/test/c00001/READY").read_text(), "1\n")
        calls = [json.loads(line) for line in (self.root / "calls.jsonl").read_text().splitlines()]
        preflights = [c for c in calls if "--preflight" in c]
        self.assertEqual(len(preflights), 2)
        self.assertTrue(all(c[c.index("--max-pages") + 1] == "1"
                            and c[c.index("--max-ready") + 1] == "200" for c in preflights))
        self.assertFalse(any("tar" in c or "--step" in c or "flock" in c
                             or "--after-submit" in c for c in calls))

    def test_capacity_change_under_project_lock_retains_ready_without_acknowledgement(self):
        self.env["SHIP_TEST_PUBLISH_EXIT"] = "75"
        result = self.run_ship([])
        self.assertEqual(result.returncode, 75, result.stderr)
        self.assertIn("native publication deferred; READY retained", result.stderr)
        self.assertTrue((self.root / "outbox/test/c00001/READY").exists())
        calls = [json.loads(line) for line in (self.root / "calls.jsonl").read_text().splitlines()]
        self.assertTrue(any("tar" in c for c in calls))
        self.assertTrue(any("--preflight" in c for c in calls))
        self.assertTrue(any("flock" in c and "/run/usnm-boinc-step.lock" in c for c in calls))
        self.assertFalse(any("quality-submit" in c or "--after-submit" in c for c in calls))
        self.env["SHIP_TEST_PUBLISH_EXIT"] = "0"
        result = self.run_ship([])
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertFalse((self.root / "outbox/test/c00001/READY").exists())

    def test_uncertainty_is_not_misclassified_as_normal_preflight_backpressure(self):
        self.env["SHIP_TEST_PREFLIGHT_EXIT"] = "1"
        result = self.run_ship([])
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertNotIn("preflight deferred", result.stderr)
        self.assertTrue((self.root / "outbox/test/c00001/READY").exists())
        calls = [json.loads(line) for line in (self.root / "calls.jsonl").read_text().splitlines()]
        self.assertFalse(any("tar" in c or "--step" in c for c in calls))

    def test_qwen_watermark_rejects_nonpositive_noninteger_and_missing_values(self):
        for value in ("0", "-1", "1.5", "oops", "", None):
            with self.subTest(value=value):
                result = self.run_ship(["--qwen-max-ready"] + ([value] if value is not None else []))
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("--qwen-max-ready: a positive integer", result.stderr)
                self.assertFalse((self.root / "calls.jsonl").exists())

    def test_bulk_mount_floor_blocks_even_when_project_root_has_space(self):
        self.env["SHIP_TEST_BULK_FREE"] = "39"
        result = self.run_ship(["--min-free-gb", "50"])
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("39 GB free (under 50)", result.stderr)
        self.assertTrue((self.root / "outbox/test/c00001/READY").exists())
        calls = [json.loads(line) for line in (self.root / "calls.jsonl").read_text().splitlines()]
        self.assertFalse(any("tar" in call or "/project/ops/qwen-feed/qwen_submit.py" in call
                             for call in calls))

    def test_thin_pool_reserves_inputs_above_floor_before_copy(self):
        self.env["SHIP_TEST_POOL_FREE_BYTES"] = str(50 * 1024**3 + 1024**2)
        result = self.run_ship(["--min-free-gb", "50"])
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("including per-page reservation", result.stderr)
        self.assertTrue((self.root / "outbox/test/c00001/READY").exists())
        calls = [json.loads(line) for line in (self.root / "calls.jsonl").read_text().splitlines()]
        self.assertFalse(any("tar" in call or "/project/ops/qwen-feed/qwen_submit.py" in call
                             for call in calls))

    def test_native_shipping_does_not_wait_for_unrelated_global_replicas(self):
        self.env["SHIP_TEST_READY"] = "5000"
        self.stub("curl", "#!/bin/sh\nexit 1\n")
        result = self.run_ship([])
        self.assertEqual(result.returncode, 0, result.stderr)
        calls = [json.loads(line) for line in (self.root / "calls.jsonl").read_text().splitlines()]
        self.assertTrue(any("/project/ops/qwen-feed/qwen_submit.py" in call for call in calls))

    def test_quality_shipping_does_not_wait_for_unrelated_gpu_work(self):
        self.env["SHIP_TEST_READY"] = "5000"
        result = self.run_ship([], native=False)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("shipped 1 chunk(s)", result.stderr)
        calls = [json.loads(line) for line in (self.root / "calls.jsonl").read_text().splitlines()]
        self.assertTrue(any("quality-submit" in call for call in calls))

    def test_quality_queue_retains_its_own_unsent_backpressure(self):
        self.env["SHIP_TEST_QUALITY_READY"] = "301"
        result = self.run_ship([], native=False)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("301 results unsent (over 300)", result.stderr)
        self.assertTrue((self.root / "outbox/test/c00001/READY").exists())
        self.assertFalse((self.root / "calls.jsonl").exists())

    def test_quality_status_failure_never_admits_or_removes_ready_chunk(self):
        for body in (
            "<server_status/>",
            "<server_status><database_file_states><tasks_by_app>"
            "<app><name>usnm_qwen</name><unsent>0</unsent></app>"
            "</tasks_by_app></database_file_states></server_status>",
            "<server_status><database_file_states><tasks_by_app>"
            "<app><name>usnm_quality</name><unsent>bad</unsent></app>"
            "</tasks_by_app></database_file_states></server_status>",
            "not XML",
        ):
            with self.subTest(body=body):
                self.stub("curl", "#!/bin/sh\nprintf '%s' '" + body + "'\n")
                result = self.run_ship([], native=False)
                self.assertNotEqual(result.returncode, 0)
                self.assertTrue((self.root / "outbox/test/c00001/READY").exists())
                self.assertFalse((self.root / "calls.jsonl").exists())

    @unittest.skipUnless(shutil.which("flock"), "Linux shipper uses util-linux flock")
    def test_inline_and_independent_native_shippers_cannot_clean_same_chunk(self):
        import fcntl
        outbox = self.root / "outbox"
        outbox.mkdir()
        with (outbox / ".qwen-ship-test.lock").open("w") as lock:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
            result = self.run_ship([])
        self.assertEqual(result.returncode, 75, result.stderr)
        self.assertTrue((outbox / "test/c00001/READY").exists())
        self.assertFalse((self.root / "calls.jsonl").exists())
