"""Argument checks of onprem-step.sh: python3 -m unittest -v scripts/boinc/test_onprem_step.py

Every case stops before podman: --what-if, or a refusal. The host settings come from a scratch
USNM_BOINC_CONF directory.
"""

import os
import re
import subprocess
import tempfile
import unittest
from pathlib import Path

SCRIPT = Path(__file__).resolve().parent / "onprem-step.sh"
HANDOFF_SETTINGS = {
    "HANDOFF_URL": "https://example.blob.core.windows.net/boinc-handoff",
    "COSMOS_ENDPOINT": "https://example.documents.azure.com:443/",
    "CURATED_URL": "https://lake.blob.core.windows.net/curated",
    "REFERENCE_URL": "https://lake.blob.core.windows.net/reference",
    "OUT_URL": "https://example.blob.core.windows.net/ocr-out",
    "QUEUE_URL": "https://example.queue.core.windows.net",
    "IDENTITY_ENDPOINT": "http://localhost:40342/metadata/identity/oauth2/token",
    "IMDS_ENDPOINT": "http://localhost:40342",
}


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
            (("--env", "prod", "--target", "export", "--step", "collect", "--what-if"), "does not run on-prem"),
            (("--env", "prod", "--target", "handoff", "--step", "texts-export", "--what-if"), "stays in Azure"),
            (("--env", "prod", "--target", "handoff", "--step", "collect", "--what-if"), "no "),
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

    def test_publish_takes_a_local_image_id(self):
        # boinc-app-release.sh passes the build's image ID, so a moved tag can't change what is published.
        image_id = "sha256:" + "b" * 64
        done = self.run_step("--env", "prod", "--target", "publish", "--step", "publish", "--image", image_id,
                             "--what-if")
        self.assertEqual(done.returncode, 0, done.stderr)
        self.assertIn(image_id, done.stdout)

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
            (("--env", "prod", "--target", "publish", "--step", "publish", "--image", "sha256:" + "a" * 63,
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

    def test_ssh_surya_steps_take_exactly_their_own_inputs(self):
        crops = ("--env prod --step surya-stage-crops --var SURYA_CAMPAIGN=c --var QUALITY_CAMPAIGN=q "
                 "--var SURYA_ENGINE_IMAGE=sha256:a --var SURYA_ENGINE_LLAMA=sha256:b "
                 "--var SURYA_ENGINE_WEIGHTS=sha256:c --var SURYA_MAX=50 --var SURYA_TIMEOUT_SECS=600 --what-if")
        done = self.run_step(ssh=crops)
        self.assertEqual(done.returncode, 0, done.stderr)
        self.assertIn("BRIDGE_STEP=surya-stage-crops", done.stdout)
        done = self.run_step(ssh="--env prod --step surya-submit --var SURYA_CAMPAIGN=c --var SURYA_MAX=200 --what-if")
        self.assertEqual(done.returncode, 0, done.stderr)
        for command in (
            "--env prod --step surya-submit --var SURYA_CAMPAIGN=c --var SURYA_TARGET_HOST=7 --what-if",
            "--env prod --step surya-submit --var SURYA_CAMPAIGN=c --var QUALITY_CAMPAIGN=q --what-if",
            "--env prod --step surya-stage-crops --var SURYA_CAMPAIGN=c --var SURYA_SAMPLE=/x --what-if",
            "--env prod --step surya-stage-crops --var JOB_ID=" + "a" * 64 + " --what-if",
            "--env prod --step submit --var SURYA_CAMPAIGN=c --what-if",
            "--env prod --step surya-collect --var SURYA_SPOOL=/x --what-if",
        ):
            with self.subTest(command=command):
                done = self.run_step(ssh=command)
                self.assertNotEqual(done.returncode, 0)
                self.assertIn("not a controller bridge-step input", done.stderr)

    def test_ssh_gold_onboarding_takes_its_settings_and_never_a_threshold(self):
        settings = ("--var GOLD_CAMPAIGN=v1-tess550 --var GOLD_LANGUAGE=eng --var GOLD_PROFILE=eng "
                    "--var GOLD_ENGINE_IMAGE=sha256:a --var GOLD_ENGINE_BASE=base@sha256:b "
                    "--var GOLD_ENGINE_VERSION=5.5.0 --var GOLD_MAX_HOSTS=10")
        for step in ("gold-onboard", "gold-promote"):
            with self.subTest(step=step):
                done = self.run_step(ssh=f"--env prod --step {step} {settings} --what-if")
                self.assertEqual(done.returncode, 0, done.stderr)
                self.assertIn(f"BRIDGE_STEP={step}", done.stdout)
        for command in (
            "--env prod --step gold-promote --var GOLD_MAX_CER=1 --what-if",
            "--env prod --step gold-promote --var GOLD_MANIFEST_SHA256=" + "a" * 64 + " --what-if",
            "--env prod --step gold-onboard --var GOLD_TARGET_HOST=7 --what-if",
            "--env prod --step gold-promote --var GOLD_SPOOL=/x --what-if",
            "--env prod --step gold-trust --var GOLD_CAMPAIGN=c --what-if",
            "--env prod --step gold-submit --var GOLD_CAMPAIGN=c --what-if",
            "--env prod --step gold-onboard --var JOB_ID=" + "a" * 64 + " --what-if",
        ):
            with self.subTest(command=command):
                done = self.run_step(ssh=command)
                self.assertNotEqual(done.returncode, 0)
                self.assertIn("not a controller bridge-step input", done.stderr)

    def receive(self, members, stage):
        import io
        import tarfile
        buf = io.BytesIO()
        with tarfile.open(fileobj=buf, mode="w") as tar:
            for name, data in members:
                info = tarfile.TarInfo(name)
                if data is None:
                    info.type = tarfile.SYMTYPE
                    info.linkname = "/etc/passwd"
                    tar.addfile(info)
                else:
                    info.size = len(data)
                    tar.addfile(info, io.BytesIO(data))
        env = {"PATH": os.environ["PATH"], "USNM_BOINC_CONF": self.conf.name,
               "USNM_BOINC_SCAN_STAGE": stage, "USNM_BOINC_STEP_RUN": stage + "-run",
               "SSH_ORIGINAL_COMMAND": "usnm-boinc-step --env prod --target scans --step receive --wait-mins 5"}
        os.makedirs(stage + "-run", exist_ok=True)
        return subprocess.run(["bash", str(SCRIPT), "--from-ssh"], env=env, input=buf.getvalue(),
                              capture_output=True, timeout=60)

    def test_scans_receive_takes_only_staged_scans(self):
        # #1116: the controller's push sends copies of scans; only that layout lands.
        with tempfile.TemporaryDirectory() as d:
            stage = os.path.join(d, "stage")
            done = self.receive([("keep/nbu_gage/sn1/1890-05-03/1/7.jp2", b"scan"),
                                 ("keep/nbu_gage/sn1/1890-05-03/1/7.jp2.json", b"{}")], stage)
            self.assertEqual(done.returncode, 0, done.stderr)
            self.assertIn(b"received 2 files", done.stdout)
            self.assertEqual(Path(stage, "keep/nbu_gage/sn1/1890-05-03/1/7.jp2").read_bytes(), b"scan")
            old = Path(stage, "keep/nbu_gage/sn1/1890-01-01/1/1.jpg")
            old.parent.mkdir(parents=True)
            old.write_bytes(b"old")
            os.utime(old, (0, 0))
            for members in ([("../x/sn1/1890-05-03/1/7.jp2", b"x")],
                            [("keep/nbu_gage/sn1/1890-05-03/1/7.png", b"x")],
                            [("keep/nbu_gage/sn1/1890-05-03/1/8.jp2", None)],
                            [("keep/nbu_gage/sn1/1890-05-03/1/9.jp2.json", b"x" * 70000)]):
                with self.subTest(members=members[0][0]):
                    done = self.receive(members, stage)
                    self.assertNotEqual(done.returncode, 0)
            self.assertFalse(Path(stage, "keep/nbu_gage/sn1/1890-05-03/1/8.jp2").exists())
            done = self.receive([("keep/nbu_gage/sn1/1890-05-03/1/2.jpg", b"s")], stage)
            self.assertIn(b"pruned 1", done.stdout)
            self.assertFalse(old.exists())

    def test_scans_receive_takes_no_vars(self):
        done = self.run_step(ssh="usnm-boinc-step --env prod --target scans --step receive --var X=1 --wait-mins 5")
        self.assertNotEqual(done.returncode, 0)
        self.assertIn("no --image or --var", done.stderr)

    def write_handoff_env(self, **changes):
        settings = dict(HANDOFF_SETTINGS, **changes)
        Path(self.conf.name, "handoff.env").write_text(
            "".join(f"{k}={v}\n" for k, v in settings.items() if v is not None))

    def test_handoff_runs_the_job_command_with_the_hosts_settings(self):
        self.write_handoff_env()
        done = self.run_step(ssh="usnm-boinc-step --env prod --target handoff --step claim "
                                 "--var HANDOFF_MANIFEST=pages-v1/manifest.parquet --var CLAIM_NONCE=abcdefgh12 "
                                 "--var CLAIM_MAX=5 --var CLAIM_QUEUES=ocr-bulk,ocr-priority "
                                 "--var CLAIM_LEASE_HOURS=36 --var CLAIM_LANGUAGES=eng --wait-mins 30 --what-if")
        self.assertEqual(done.returncode, 0, done.stderr)
        self.assertIn("localhost/usnm-boinc-bridge:abc's handoff-job.sh", done.stdout)
        self.assertIn("handoff.env", done.stdout)
        self.assertIn("HANDOFF_STEP=claim", done.stdout)
        self.assertNotIn("BRIDGE_STEP", done.stdout)
        for command in ("--env prod --target handoff --step collect --var JOB_IDS=" + "a" * 64 + " --what-if",
                        "--env prod --target handoff --step collect --var JOB_ID=" + "a" * 64 +
                        " --var HOST_ID=7 --what-if",
                        "--env prod --target handoff --step release --var HANDOFF_DOCS=a_b,c_d --what-if",
                        "--env prod --target handoff --step export --var HANDOFF_PAGES=sn1/1890-05-03/ed-1/seq-7"
                        " --var HANDOFF_BATCH=nbu_gage --var HANDOFF_PLAN=queue-feed-1"
                        " --var HANDOFF_MANIFEST=pages-v1/manifest.parquet --var ENGINE_IMAGE=sha256:" + "c" * 64 +
                        " --var HANDOFF_SCANS=keep/nbu_gage/sn1/1890-05-03/1/7.jp2 --var HANDOFF_JOBS=" + "d" * 64 +
                        " --var HANDOFF_CLAIMS=1 --what-if"):
            with self.subTest(command=command):
                done = self.run_step(ssh=command)
                self.assertEqual(done.returncode, 0, done.stderr)

    def test_handoff_takes_only_its_steps_inputs(self):
        self.write_handoff_env()
        for command, message in (
            ("--env prod --target handoff --step claim --var QUEUE_URL=https://evil --what-if", "set by the host"),
            ("--env prod --target handoff --step collect --var HANDOFF_URL=https://evil --what-if", "set by the host"),
            ("--env prod --target handoff --step collect --var HANDOFF_STEP=export --what-if", "set by the host"),
            ("--env prod --target handoff --step collect --var IDENTITY_ENDPOINT=http://x --what-if", "set by the host"),
            ("--env prod --target handoff --step collect --var AZURE_CLIENT_ID=x --what-if", "steer the shell"),
            ("--env prod --target handoff --step collect --var HANDOFF_SPOOL=/project/x --what-if", "takes only"),
            ("--env prod --target handoff --step claim --var JOB_ID=" + "a" * 64 + " --what-if", "takes only"),
            ("--env prod --target handoff --step release --var CLAIM_NONCE=abcdefgh12 --what-if", "takes only"),
            ("--env prod --target handoff --step export --var JOB_ID=" + "a" * 64 + " --what-if", "takes only"),
            ("--env prod --target handoff --step texts-export --what-if", "stays in Azure"),
        ):
            with self.subTest(command=command):
                done = self.run_step(ssh=command)
                self.assertNotEqual(done.returncode, 0)
                self.assertIn(message, done.stderr)
        # As root on the host too: a hand-off step never takes another input.
        done = self.run_step("--env", "prod", "--target", "handoff", "--step", "collect",
                             "--var", "KEY_VAULT_NAME=kv2", "--what-if")
        self.assertNotEqual(done.returncode, 0)
        self.assertIn("takes only", done.stderr)

    def test_handoff_env_must_hold_exactly_the_jobs_settings(self):
        for changes, message in (
            ({"QUEUE_URL": None}, "doesn't set QUEUE_URL"),
            ({"IMDS_ENDPOINT": None}, "doesn't set IMDS_ENDPOINT"),
            ({"KEY_VAULT_NAME": "kv"}, "it takes only"),
            ({"COSMOS_ENDPOINT": ""}, "empty COSMOS_ENDPOINT"),
        ):
            with self.subTest(changes=changes):
                self.write_handoff_env(**changes)
                done = self.run_step("--env", "prod", "--target", "handoff", "--step", "collect",
                                     "--var", "JOB_ID=" + "a" * 64, "--what-if")
                self.assertNotEqual(done.returncode, 0)
                self.assertIn(message, done.stderr)

    def run_for_real(self, *args):
        # A fake podman copies the env file it is given; flock always gets the lock.
        bin_dir = Path(self.conf.name, "bin")
        bin_dir.mkdir(exist_ok=True)
        run_dir = Path(self.conf.name, "run")
        run_dir.mkdir(exist_ok=True)
        captured = Path(self.conf.name, "envfile")
        (bin_dir / "podman").write_text(
            "#!/usr/bin/env bash\n"
            "while [ $# -gt 0 ]; do [ \"$1\" = --env-file ] && cp \"$2\" " + str(captured) + "; shift; done\n")
        (bin_dir / "flock").write_text("#!/usr/bin/env bash\nexit 0\n")
        for tool in ("podman", "flock"):
            (bin_dir / tool).chmod(0o755)
        env = {"PATH": f"{bin_dir}:{os.environ['PATH']}", "USNM_BOINC_CONF": self.conf.name,
               "USNM_BOINC_STEP_RUN": str(run_dir)}
        done = subprocess.run(["bash", str(SCRIPT), *args], env=env, capture_output=True, text=True, timeout=30)
        return done, captured.read_text() if captured.exists() else None

    def test_handoff_env_without_a_final_newline(self):
        # The last setting still counts, and it doesn't run into HANDOFF_STEP.
        Path(self.conf.name, "handoff.env").write_text(
            "\n".join(f"{k}={v}" for k, v in HANDOFF_SETTINGS.items()))
        done, envfile = self.run_for_real("--env", "prod", "--target", "handoff", "--step", "collect",
                                          "--var", "JOB_ID=" + "a" * 64, "--wait-mins", "1")
        self.assertEqual(done.returncode, 0, done.stderr)
        lines = envfile.splitlines()
        self.assertEqual(lines, [f"{k}={v}" for k, v in HANDOFF_SETTINGS.items()] +
                         ["HANDOFF_STEP=collect", "JOB_ID=" + "a" * 64])

    def test_bridge_env_without_a_final_newline(self):
        Path(self.conf.name, "bridge.env").write_text("# comment\nKEY_VAULT_NAME=kv\nDB_HOST=usnm-boinc-mysql")
        done, envfile = self.run_for_real("--env", "prod", "--step", "submit", "--var", "JOB_ID=x",
                                          "--wait-mins", "1")
        self.assertEqual(done.returncode, 0, done.stderr)
        self.assertEqual(envfile.splitlines(),
                         ["KEY_VAULT_NAME=kv", "DB_HOST=usnm-boinc-mysql", "BRIDGE_STEP=submit", "JOB_ID=x"])

    def test_handoff_settings_match_the_azure_job(self):
        # The Azure job's env, less Container Apps' AZURE_CLIENT_ID, plus the Arc endpoints.
        bicep = (SCRIPT.parents[2] / "infra/modules/boinc-handoff-job.bicep").read_text()
        job = set(re.findall(r"\{ name: '([A-Z_]+)', value:", bicep)) - {"AZURE_CLIENT_ID"}
        script = re.search(r'^HANDOFF_HOST_VARS=" ([A-Z_ ]+) "$', SCRIPT.read_text(), re.M).group(1).split()
        self.assertEqual(set(script), job | {"IDENTITY_ENDPOINT", "IMDS_ENDPOINT"})
        self.assertEqual(set(HANDOFF_SETTINGS), set(script))

    def test_ssh_arguments_are_split_without_a_shell(self):
        # A value with shell syntax stays one literal argument; nothing is evaluated.
        done = self.run_step(ssh="--env prod --step submit --var 'JOB_ID=$(touch /tmp/usnm-pwned); x' --what-if")
        self.assertEqual(done.returncode, 0, done.stderr)
        self.assertIn("JOB_ID=$(touch /tmp/usnm-pwned); x", done.stdout)
        self.assertFalse(Path("/tmp/usnm-pwned").exists())


if __name__ == "__main__":
    unittest.main()
