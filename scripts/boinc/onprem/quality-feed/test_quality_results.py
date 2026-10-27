"""Canonical collection pipeline: ordering, fail-closed phases and fresh snapshot transfer."""
import importlib.util
import fcntl
import json
import os
from pathlib import Path
import shutil
import subprocess
import shlex
import types
import unittest
import uuid
from unittest import mock

HERE = Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location("quality_results", HERE / "quality_results.py")
results = importlib.util.module_from_spec(spec)
spec.loader.exec_module(results)


class ResultsTest(unittest.TestCase):
    def args(self):
        return types.SimpleNamespace(campaign="v1-test", engine_image="sha256:" + "a" * 64,
                                     server_host="server.example", server_ct=230, ctid=215)

    def test_collection_pull_then_certificate_load_with_bounded_locks_and_retry(self):
        with mock.patch.object(results.subprocess, "run") as run:
            for _ in range(2):
                self.assertEqual(results.run(self.args())["campaign"], "v1-test")
            calls = run.call_args_list
            self.assertEqual(len(calls), 6)
            self.assertEqual(calls[:3], calls[3:])
            collect, pull, load = [c.args[0] for c in calls[:3]]
            self.assertIn("flock -n /run/usnm-boinc-step.lock", collect[-1])
            self.assertIn("podman exec usnm-boinc-server timeout --signal=TERM --kill-after=30s 20m",
                          collect[-1])
            self.assertIn("--spool /project/usnmocr/results/0", collect[-1])
            self.assertIn("--snapshot-only", pull)
            self.assertEqual(load[:4], ["pct", "exec", "215", "--"])
            self.assertIn('$(cat /etc/usnm/quality-db/dsn)', load[-1])
            self.assertIn("/results/v1-test/out/pages.parquet", load[-1])
            for call in calls:
                self.assertTrue(call.kwargs["check"])
                self.assertGreater(call.kwargs["timeout"], 0)

    def test_failed_or_timed_out_phase_never_advances(self):
        for phase in range(3):
            for error in (subprocess.CalledProcessError(1, ["phase"]),
                          subprocess.TimeoutExpired(["phase"], 1)):
                with self.subTest(phase=phase, error=type(error).__name__):
                    with mock.patch.object(results.subprocess, "run",
                                           side_effect=[None] * phase + [error]) as run:
                        with self.assertRaises(type(error)):
                            results.run(self.args())
                        self.assertEqual(run.call_count, phase + 1)


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
