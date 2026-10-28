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
        self.stub("curl", "#!/bin/sh\necho \"<results_ready_to_send>${SHIP_TEST_READY:-0}</results_ready_to_send>\"\n")
        self.stub("tar", "#!/bin/sh\nexit 0\n")
        self.stub("mktemp", "#!/bin/sh\necho \"$SHIP_TEST_ROOT/step.err\"\n")
        self.stub("ssh", """#!/usr/bin/env python3
import json, os, sys
from pathlib import Path
root = Path(os.environ["SHIP_TEST_ROOT"])
args = sys.argv[1:]
with (root / "calls.jsonl").open("a") as f:
    f.write(json.dumps(args) + "\\n")
if "df" in args:
    print("Avail\\n100")
    if "/srv/usnm-boinc/project/quality" in args:
        print(os.environ.get("SHIP_TEST_BULK_FREE", "100"))
    if "/srv/usnm-boinc/project/usnmocr/download" in args:
        print(os.environ.get("SHIP_TEST_BULK_FREE", "100"))
elif "lvs" in args:
    print(json.dumps({"report": [{"lv": [{"lv_size": str(100 * 1024**3), "data_percent": "0"}]}]}))
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

    def test_native_shipping_does_not_wait_for_unrelated_global_replicas(self):
        self.env["SHIP_TEST_READY"] = "5000"
        result = self.run_ship([])
        self.assertEqual(result.returncode, 0, result.stderr)
        calls = [json.loads(line) for line in (self.root / "calls.jsonl").read_text().splitlines()]
        self.assertTrue(any("/project/ops/qwen-feed/qwen_submit.py" in call for call in calls))

    def test_other_campaigns_retain_global_unsent_backpressure(self):
        self.env["SHIP_TEST_READY"] = "5000"
        result = self.run_ship([], native=False)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("over 300", result.stderr)
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
