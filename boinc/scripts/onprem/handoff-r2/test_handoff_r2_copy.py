"""python3 -m unittest -v boinc/scripts/onprem/handoff-r2/test_handoff_r2_copy.py

handoff-r2-copy.sh with a fake rclone: off without the token file, one-way and scoped when on,
failure reported by its own exit status and status file.
"""

import json
import os
import subprocess
import tempfile
import unittest
from pathlib import Path

SCRIPT = Path(__file__).resolve().parent / "handoff-r2-copy.sh"


class CopyTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.dir = Path(self.tmp.name).resolve()
        self.root = self.dir / "root"
        self.root.mkdir()
        self.status = self.dir / "state/status.json"
        self.log = self.dir / "calls.txt"
        self.rclone = self.dir / "rclone"
        self.set_rclone(0)
        self.env_file = self.dir / "r2.env"
        # The host has flock(1); a stand-in lets the tests run where it doesn't.
        (self.dir / "bin").mkdir()
        (self.dir / "bin/flock").write_text("#!/bin/sh\nexit 0\n")
        (self.dir / "bin/flock").chmod(0o755)

    def tearDown(self):
        self.tmp.cleanup()

    def set_rclone(self, code):
        self.rclone.write_text(
            f'#!/bin/sh\n{{ echo "$@"; echo "KEY=$RCLONE_CONFIG_R2_ACCESS_KEY_ID"; }} >> {self.log}\nexit {code}\n')
        self.rclone.chmod(0o755)

    def run_copy(self):
        argv = ["bash", str(SCRIPT), "--bucket", "copy-bucket", "--r2-env", str(self.env_file),
                "--root", str(self.root), "--rclone", str(self.rclone), "--status", str(self.status)]
        self.last = subprocess.run(argv, capture_output=True, text=True, timeout=30,
                                   env={"PATH": f"{self.dir / 'bin'}:{os.environ['PATH']}"})
        return self.last

    def token(self):
        self.env_file.write_text("R2_ACCESS_KEY_ID=scoped\nR2_SECRET_ACCESS_KEY=s\nR2_ENDPOINT=https://x.r2.example\n")

    def test_off_without_the_token_file(self):
        done = self.run_copy()
        self.assertEqual(done.returncode, 0, done.stderr)
        self.assertEqual(json.loads(done.stdout)["result"], "disabled")
        self.assertFalse(self.log.exists())

    def test_copies_one_way_and_leaves_out_the_lock(self):
        self.token()
        done = self.run_copy()
        self.assertEqual(done.returncode, 0, done.stderr)
        call = self.log.read_text()
        self.assertIn(f"copy {self.root} r2:copy-bucket/handoff", call)
        self.assertIn("scheduler/controller.lock.json", call)
        self.assertNotRegex(call, r"\b(sync|move|delete|purge)\b")
        self.assertIn("KEY=scoped", call)
        self.assertNotIn("s\n", done.stdout + done.stderr.replace("KEY", ""))
        self.assertEqual(json.loads(self.status.read_text())["result"], "ok")

    def test_failure_fails_this_unit_only_and_is_recorded(self):
        self.token()
        self.set_rclone(5)
        done = self.run_copy()
        self.assertEqual(done.returncode, 5, done.stdout + done.stderr)
        self.assertEqual(json.loads(self.status.read_text())["result"], "failed")
        self.assertEqual(list(self.root.iterdir()), [])

    def test_missing_root_and_bad_bucket_are_refused(self):
        self.token()
        self.root.rmdir()
        self.assertNotEqual(self.run_copy().returncode, 0)


if __name__ == "__main__":
    unittest.main()
