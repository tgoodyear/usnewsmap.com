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
        self.stub("curl", "#!/bin/sh\necho '<results_ready_to_send>0</results_ready_to_send>'\n")
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

    def run_ship(self, extra):
        chunk = self.root / "outbox/test/c00001"
        chunk.mkdir(parents=True, exist_ok=True)
        (chunk / "READY").write_text("1\n")
        return subprocess.run(
            ["bash", str(HERE / "quality-ship.sh"), "--campaign", "test",
             "--engine-image", "sha256:" + "a" * 64, "--outbox", str(self.root / "outbox"),
             "--qwen-engine-plan", "/project/plan.json", "--qwen-models", "/project/models",
             *extra], env=self.env, capture_output=True, text=True, timeout=30)

    def test_qwen_watermark_default_and_override_reach_submit(self):
        for extra, expected in (([], "1000"), (["--qwen-max-ready", "20000"], "20000")):
            with self.subTest(expected=expected):
                result = self.run_ship(extra)
                self.assertEqual(result.returncode, 0, result.stderr)
                calls = [json.loads(line) for line in (self.root / "calls.jsonl").read_text().splitlines()]
                submit = next(c for c in reversed(calls) if "/project/ops/qwen-feed/qwen_submit.py" in c)
                self.assertEqual(submit[submit.index("--max-ready") + 1], expected)

    def test_qwen_watermark_rejects_nonpositive_noninteger_and_missing_values(self):
        for value in ("0", "-1", "1.5", "oops", "", None):
            with self.subTest(value=value):
                result = self.run_ship(["--qwen-max-ready"] + ([value] if value is not None else []))
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("--qwen-max-ready: a positive integer", result.stderr)
                self.assertFalse((self.root / "calls.jsonl").exists())
