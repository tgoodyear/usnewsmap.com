"""Canonical collection pipeline: ordering, fail-closed phases and fresh snapshot transfer."""
import importlib.util
import fcntl
import json
import io
import os
from pathlib import Path
import shutil
import subprocess
import shlex
import sys
import time
import threading
import types
import unittest
import uuid
from unittest import mock

HERE = Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location("quality_results", HERE / "quality_results.py")
results = importlib.util.module_from_spec(spec)
spec.loader.exec_module(results)


class ResultsTest(unittest.TestCase):
    def setUp(self):
        fixed = mock.patch.object(results.uuid, "uuid4", return_value=types.SimpleNamespace(hex="a" * 32))
        fixed.start()
        self.addCleanup(fixed.stop)
        transfer = mock.patch.object(results, "transfer",
                                     side_effect=lambda command: results.subprocess.run(
                                         command, check=True, timeout=300))
        transfer.start()
        self.addCleanup(transfer.stop)

    def args(self):
        return types.SimpleNamespace(campaign="v1-test", engine_image="sha256:" + "a" * 64,
                                     server_host="server.example", server_ct=230, ctid=216,
                                     qwen_campaign=None, incremental=False, max_dirty_units=8,
                                     collector="usnm-boinc", step_lock_wait_secs=0)

    def test_invalid_lock_wait_is_rejected_before_remote_work(self):
        for bound in ("-1", "61"):
            with self.subTest(bound=bound), mock.patch.object(results, "run") as run:
                with self.assertRaises(SystemExit):
                    results.main(["--campaign", "test", "--engine-image", "sha256:" + "a" * 64,
                                  "--step-lock-wait-secs", bound])
                run.assert_not_called()

    @unittest.skipUnless(shutil.which("flock"), "real util-linux flock requires Linux")
    def test_real_bounded_admission_busy_then_free_timeout_and_collect_failure(self):
        root = HERE / (".results-admission-" + uuid.uuid4().hex)
        root.mkdir()
        self.addCleanup(shutil.rmtree, root)
        lock_path, ran = root / "step.lock", root / "collected"
        actual_run = subprocess.run
        args = self.args()
        args.step_lock_wait_secs = 1
        for outcome in ("released", "busy", "error", "error75"):
            with self.subTest(outcome=outcome), lock_path.open("a") as lock:
                if ran.exists():
                    ran.unlink()
                fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
                timer = None
                if outcome != "busy":
                    timer = threading.Timer(0.1, lambda: fcntl.flock(lock, fcntl.LOCK_UN))
                    timer.start()

                def transport(command, **kwargs):
                    if command[0] != "ssh":
                        return subprocess.CompletedProcess(command, 0)
                    remote = shlex.split(command[-1])[4:]
                    remote[remote.index("/run/usnm-boinc-step.lock")] = str(lock_path)
                    start = remote.index("podman")
                    code = 75 if outcome == "error75" else 7 if outcome == "error" else 0
                    remote[start:] = [
                        sys.executable, "-c",
                        f"from pathlib import Path;Path({str(ran)!r}).touch();raise SystemExit({code})",
                    ]
                    self.assertLessEqual(kwargs["timeout"], 1320)
                    return actual_run(remote, **kwargs)

                try:
                    with mock.patch.object(results.subprocess, "run", side_effect=transport) as run, \
                            mock.patch.object(results.sys, "stderr", new_callable=io.StringIO) as log:
                        if outcome == "released":
                            self.assertEqual(results.run(args)["campaign"], args.campaign)
                            self.assertEqual(run.call_count, 4)
                        else:
                            with self.assertRaises(subprocess.CalledProcessError) as error:
                                results.run(args)
                            self.assertEqual(error.exception.returncode,
                                             75 if outcome in ["busy", "error75"] else 7)
                            self.assertEqual(run.call_count, 1)
                        self.assertEqual(ran.exists(), outcome != "busy")
                        self.assertEqual('"phase": "step_lock_timeout"' in log.getvalue(),
                                         outcome == "busy")
                        self.assertEqual('"phase": "collection_failed"' in log.getvalue(),
                                         outcome in ["error", "error75"])
                finally:
                    if timer:
                        timer.join()

    def test_missing_admission_receipt_never_transfers_even_after_remote_success(self):
        args = self.args()
        args.step_lock_wait_secs = 1
        with mock.patch.object(results.subprocess, "run",
                               return_value=subprocess.CompletedProcess(["ssh"], 0, stderr="")) as run:
            with self.assertRaises(RuntimeError):
                results.run(args)
            self.assertEqual(run.call_count, 1)

    def test_bounded_admission_transport_timeout_replays_receipt_and_never_loads(self):
        args = self.args()
        args.step_lock_wait_secs = 60
        receipt = results.LOCK_ACQUIRED + " 2026-10-28T05:00:00Z\ncollector diagnostic\n"
        for stderr in (receipt, receipt.encode(), b"remote diagnostic\n", None):
            with self.subTest(stderr=stderr):
                error = subprocess.TimeoutExpired(["ssh"], 1320, stderr=stderr)
                with mock.patch.object(results.subprocess, "run", side_effect=error) as run, \
                        mock.patch.object(results.sys, "stderr", new_callable=io.StringIO) as log:
                    with self.assertRaises(subprocess.TimeoutExpired) as raised:
                        results.run(args)
                    self.assertIs(raised.exception, error)
                    self.assertEqual(run.call_count, 1)
                    text = log.getvalue()
                    event = json.loads(text.splitlines()[-1])
                    self.assertEqual(event["phase"], "collection_transport_timeout")
                    self.assertEqual(event["step_lock_acquired"], stderr in (receipt, receipt.encode()))
                    self.assertEqual(event["budget_seconds"], 1320)
                    if stderr:
                        self.assertIn(stderr.decode() if isinstance(stderr, bytes) else stderr, text)

    def test_incremental_forwarding_and_incomplete_warming_never_pulls_stale_snapshot(self):
        args = self.args()
        args.incremental = True
        args.max_dirty_units = 3
        args.collector = "/project/ops/quality-results/usnm-boinc"
        with mock.patch.object(results.subprocess, "run",
                               side_effect=subprocess.CalledProcessError(1, ["warming"])) as run:
            with self.assertRaises(subprocess.CalledProcessError):
                results.run(args)
            self.assertEqual(run.call_count, 1)
            command = run.call_args.args[0][-1]
            self.assertIn("--cache /project/quality/v1-test/collected-parts", command)
            self.assertIn("--max-dirty-units 3", command)
            self.assertIn("/project/ops/quality-results/usnm-boinc quality collect", command)

    def test_nonpositive_dirty_bound_is_rejected_before_lock_or_remote_work(self):
        for bound in ("0", "-1"):
            with self.subTest(bound=bound), mock.patch.object(results, "run") as run:
                with self.assertRaises(SystemExit) as error:
                    results.main(["--campaign", "test", "--engine-image", "sha256:" + "a" * 64,
                                  "--max-dirty-units", bound])
                self.assertEqual(error.exception.code, 2)
                run.assert_not_called()

    def test_collection_pull_then_certificate_load_with_bounded_locks_and_retry(self):
        with mock.patch.object(results.subprocess, "run") as run:
            for _ in range(2):
                self.assertEqual(results.run(self.args())["campaign"], "v1-test")
            calls = run.call_args_list
            self.assertEqual(len(calls), 8)
            self.assertEqual(calls[:4], calls[4:])
            collect, pull, load, cleanup = [c.args[0] for c in calls[:4]]
            self.assertIn("flock -n /run/usnm-boinc-step.lock", collect[-1])
            self.assertIn("podman exec usnm-boinc-server timeout --signal=TERM --kill-after=30s 20m",
                          collect[-1])
            self.assertIn("--spool /project/usnmocr/results/0", collect[-1])
            self.assertIn("--snapshot-only", pull)
            self.assertEqual(pull[pull.index("--snapshot-id") + 1], "a" * 32)
            self.assertEqual(pull[pull.index("--ctid") + 1], "216")
            self.assertEqual(load[:4], ["pct", "exec", "216", "--"])
            self.assertIn('$(cat /etc/usnm/quality-db/dsn)', load[-1])
            self.assertIn("/results/v1-test/snapshots/" + "a" * 32 + "/out/pages.parquet", load[-1])
            self.assertEqual(cleanup[-1], "/var/lib/usnm-quality-feed/results/v1-test/snapshots/" + "a" * 32)
            for call in calls:
                self.assertTrue(call.kwargs["check"])
                self.assertGreater(call.kwargs["timeout"], 0)

    def test_failed_or_timed_out_phase_never_advances(self):
        for phase in range(4):
            for error in (subprocess.CalledProcessError(1, ["phase"]),
                          subprocess.TimeoutExpired(["phase"], 1)):
                with self.subTest(phase=phase, error=type(error).__name__):
                    with mock.patch.object(results.subprocess, "run",
                                           side_effect=[None] * phase + [error]) as run:
                        with self.assertRaises(type(error)):
                            results.run(self.args())
                        self.assertEqual(run.call_count, phase + 1)

    def test_native_qwen_joins_matching_campaign_and_batch8_without_submission(self):
        args = self.args()
        args.campaign = args.qwen_campaign = "qwen-nonenglish-v1"
        with mock.patch.object(results.subprocess, "run") as run:
            report = results.run(args)
            command = run.call_args_list[0].args[0][-1]
            self.assertIn("--root /project/quality/qwen-nonenglish-v1", command)
            self.assertIn("--qwen-root /project/qwen/qwen-nonenglish-v1", command)
            self.assertIn("--qwen-spool /project/usnmocr/results/8", command)
            self.assertIn("quality collect", command)
            self.assertNotIn("submit", command)
            self.assertEqual(report["qwen_campaign"], args.campaign)
            load = run.call_args_list[2].args[0][-1]
            self.assertIn("--campaign qwen-nonenglish-v1", load)
            self.assertIn("/results/qwen-nonenglish-v1/snapshots/", load)


class PullTest(unittest.TestCase):
    def setUp(self):
        self.root = HERE / (".results-test-" + uuid.uuid4().hex)
        self.root.mkdir()
        self.addCleanup(shutil.rmtree, self.root)
        self.bin = self.root / "bin"
        self.bin.mkdir()
        for name, body in {
            "pct": '#!/bin/sh\nexit 0\n',
            "ssh": """#!/usr/bin/env python3
import json, os, sys
from pathlib import Path
Path(os.environ["CALL"]).write_text(json.dumps(sys.argv[1:]))
raise SystemExit(int(os.environ.get("SSH_EXIT", "0")))
""",
        }.items():
            path = self.bin / name
            path.write_text(body)
            path.chmod(0o755)
        self.env = {**os.environ, "PATH": str(self.bin) + os.pathsep + os.environ["PATH"],
                    "CALL": str(self.root / "call.json")}

    def test_snapshot_only_requires_fresh_file_and_propagates_remote_failure(self):
        for code in (0, 1):
            with self.subTest(code=code):
                result = subprocess.run(
                    ["bash", str(HERE / "quality-results-pull.sh"), "--campaign", "v1-test",
                     "--snapshot-only"], env={**self.env, "SSH_EXIT": str(code)},
                    capture_output=True, timeout=10)
                self.assertEqual(result.returncode, code)
                remote = json.loads((self.root / "call.json").read_text())[-1]
                self.assertIn("set -euo pipefail", remote)
                self.assertIn("test -f out/pages.parquet", remote)
                self.assertNotIn("collected/", remote)
                command = shlex.split(remote)[-1].replace(
                    "/srv/usnm-boinc/project/quality/v1-test", str(self.root))
                (self.root / "out").mkdir(exist_ok=True)
                page = self.root / "out/pages.parquet"
                if page.exists():
                    page.unlink()
                missing = subprocess.run(["bash", "-c", command], capture_output=True, timeout=10)
                self.assertNotEqual(missing.returncode, 0)
                page.write_bytes(b"fresh-snapshot")
                fresh = subprocess.run(["bash", "-c", command], capture_output=True, timeout=10)
                self.assertEqual(fresh.returncode, 0, fresh.stderr)
                self.assertIn(b"fresh-snapshot", fresh.stdout)

    def test_second_pipeline_cannot_run_while_host_lock_is_held(self):
        path = self.root / "results.lock"
        with path.open("a") as lock:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
            with mock.patch.object(results, "run") as run:
                with self.assertRaises(BlockingIOError):
                    results.main(["--campaign", "v1-test", "--engine-image",
                                  "sha256:" + "a" * 64, "--lock", str(path)])
                run.assert_not_called()

    def test_invalid_native_campaign_never_reaches_a_host(self):
        with mock.patch.object(results, "run") as run:
            with self.assertRaises(SystemExit):
                results.main(["--campaign", "v1-test", "--engine-image", "sha256:" + "a" * 64,
                              "--qwen-campaign", "../wrong-campaign"])
            run.assert_not_called()

    def test_timed_out_transfer_kills_children_even_when_parent_exits_on_term(self):
        late = self.root / "late-write"
        child = ("import signal,time;from pathlib import Path;"
                 "signal.signal(signal.SIGTERM,signal.SIG_IGN);time.sleep(1);"
                 f"Path({str(late)!r}).write_text('orphan')")
        parent = ("import subprocess,sys,time;"
                  f"subprocess.Popen([sys.executable,'-c',{child!r}]);time.sleep(20)")
        with self.assertRaises(subprocess.TimeoutExpired):
            results.transfer([sys.executable, "-c", parent], timeout=0.4)
        time.sleep(1)
        self.assertFalse(late.exists())
