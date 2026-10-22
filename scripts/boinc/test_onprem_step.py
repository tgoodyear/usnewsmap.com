"""Argument checks of onprem-step.sh: python3 -m unittest -v scripts/boinc/test_onprem_step.py

Every case stops before podman: --what-if, or a refusal. The host settings come from a scratch
USNM_BOINC_CONF directory.
"""

import os
import subprocess
import tempfile
import unittest
from pathlib import Path

SCRIPT = Path(__file__).resolve().parent / "onprem-step.sh"


class OnpremStep(unittest.TestCase):
    def setUp(self):
        self.conf = tempfile.TemporaryDirectory()
        Path(self.conf.name, "server.env").write_text("SERVER_IMAGE=localhost/usnm-boinc-bridge:abc\n")
        Path(self.conf.name, "bridge.env").write_text("KEY_VAULT_NAME=kv\nDB_HOST=usnm-boinc-mysql\n")

    def tearDown(self):
        self.conf.cleanup()

    def run_step(self, *args, ssh=None):
        env = {"PATH": os.environ["PATH"], "USNM_BOINC_CONF": self.conf.name}
        argv = ["bash", str(SCRIPT)]
        if ssh is not None:
            env["SSH_ORIGINAL_COMMAND"] = ssh
            argv.append("--from-ssh")
        argv += list(args)
        return subprocess.run(argv, env=env, capture_output=True, text=True, timeout=30)

    def test_what_if_prints_the_run(self):
        done = self.run_step("--env", "prod", "--step", "submit", "--var", "JOB_ID=" + "a" * 64, "--what-if")
        self.assertEqual(done.returncode, 0, done.stderr)
        self.assertIn("localhost/usnm-boinc-bridge:abc", done.stdout)
        self.assertIn("BRIDGE_STEP=submit", done.stdout)

    def test_refusals(self):
        cases = [
            (("--env", "prod", "--target", "handoff", "--step", "collect", "--what-if"), "does not run on-prem"),
            (("--env", "prod", "--step", "submit", "--var", "DB_HOST=x", "--what-if"), "set by the host"),
            (("--env", "prod", "--step", "submit", "--var", "BRIDGE_STEP=x", "--what-if"), "set by the host"),
            (("--env", "prod", "--step", "submit", "--var", "BASH_ENV=/tmp/x", "--what-if"), "steer the shell"),
            (("--env", "prod", "--step", "submit", "--var", "SHELLOPTS=xtrace", "--what-if"), "steer the shell"),
            (("--env", "prod", "--step", "submit", "--var", "LD_PRELOAD=/x.so", "--what-if"), "steer the shell"),
            (("--env", "prod", "--step", "submit", "--var", "lower=x", "--what-if"), "upper-case NAME"),
            (("--env", "prod", "--step", "Submit;id", "--what-if"), "bridge step name"),
            (("--env", "prod", "--step", "submit"), "--wait-mins is required"),
        ]
        for args, message in cases:
            with self.subTest(args=args):
                done = self.run_step(*args)
                self.assertNotEqual(done.returncode, 0)
                self.assertIn(message, done.stderr)

    def test_ssh_takes_only_controller_inputs(self):
        ok = self.run_step(ssh="usnm-boinc-step --env prod --target bridge --step spool-out "
                               "--var JOB_ID=" + "b" * 64 + " --var HANDOFF_SPOOL=/project/x --what-if")
        self.assertEqual(ok.returncode, 0, ok.stderr)
        self.assertIn("BRIDGE_STEP=spool-out", ok.stdout)
        for command, message in (
            ("--env prod --step gold-prepare --var GOLD_CAMPAIGN=v1 --what-if", "not a controller bridge-step input"),
            ("--env prod --step submit --var 'JOB_ID=$(id)' --var BASH_ENV=/tmp/x --what-if", "steer the shell"),
            ("--from-ssh --env prod --step submit --what-if", "--from-ssh once only"),
        ):
            with self.subTest(command=command):
                done = self.run_step(ssh=command)
                self.assertNotEqual(done.returncode, 0)
                self.assertIn(message, done.stderr)

    def test_publish_runs_an_appver_image_with_reuse_on(self):
        img = "localhost/usnm-boinc-appver:usnm_ocr-1.6"
        done = self.run_step("--env", "prod", "--target", "publish", "--step", "publish",
                             "--image", img, "--what-if")
        self.assertEqual(done.returncode, 0, done.stderr)
        self.assertIn(f"would run {img}'s publish-appver.sh", done.stdout)
        self.assertIn("APPVER_REUSE_PROJECT=1", done.stdout)
        done = self.run_step("--env", "prod", "--target", "publish", "--step", "publish", "--image", img,
                             "--var", "APPVER_REUSE_PROJECT=0", "--what-if")
        self.assertIn("APPVER_REUSE_PROJECT=0", done.stdout)

    def test_publish_refusals(self):
        img = "localhost/usnm-boinc-appver:usnm_ocr-1.6"
        cases = [
            (("--env", "prod", "--target", "publish", "--step", "publish", "--what-if"), "needs --image"),
            (("--env", "prod", "--target", "publish", "--step", "submit", "--image", img, "--what-if"),
             "takes --step publish"),
            (("--env", "prod", "--target", "publish", "--step", "publish", "--image", img,
              "--var", "JOB_ID=x", "--what-if"), "takes only --var APPVER_REUSE_PROJECT"),
            (("--env", "prod", "--target", "publish", "--step", "publish", "--image", "bad image;x",
              "--what-if"), "needs --image"),
            (("--env", "prod", "--step", "submit", "--image", img, "--what-if"), "--image is for --target publish"),
        ]
        for args, message in cases:
            with self.subTest(args=args):
                done = self.run_step(*args)
                self.assertNotEqual(done.returncode, 0)
                self.assertIn(message, done.stderr)
        done = self.run_step(ssh=f"--env prod --target publish --step publish --image {img} --what-if")
        self.assertNotEqual(done.returncode, 0)
        self.assertIn("isn't allowed over ssh", done.stderr)

    def test_ssh_arguments_are_split_without_a_shell(self):
        # A value with shell syntax stays one literal argument; nothing is evaluated.
        done = self.run_step(ssh="--env prod --step submit --var 'JOB_ID=$(touch /tmp/usnm-pwned); x' --what-if")
        self.assertEqual(done.returncode, 0, done.stderr)
        self.assertIn("JOB_ID=$(touch /tmp/usnm-pwned); x", done.stdout)
        self.assertFalse(Path("/tmp/usnm-pwned").exists())


if __name__ == "__main__":
    unittest.main()
