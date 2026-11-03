"""Offline regression tests for study output recovery and SSH status handling."""

import json
import os
from pathlib import Path
import shlex
import shutil
import subprocess
import textwrap
import unittest


ROOT = Path(__file__).resolve().parents[2]
STUDY = ROOT / "docs/reports/2026-11-gpu-studies/scripts"
WORK = ROOT / f".gpu-study-transport-tests-{os.getpid()}"
RESULT = b"synthetic completed result\n"


def executable(path, source):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(textwrap.dedent(source), encoding="utf-8")
    path.chmod(0o755)


class GPUStudyTransportTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        WORK.mkdir()
        cls.addClassCleanup(shutil.rmtree, WORK)

    def setUp(self):
        self.work = WORK / self._testMethodName
        self.work.mkdir()
        self.bin = self.work / "bin"
        self.bin.mkdir()
        self.env = {
            "HOME": str(self.work),
            "LC_ALL": "C",
            "PATH": str(self.bin) + os.pathsep + os.environ["PATH"],
            "PYTHONDONTWRITEBYTECODE": "1",
            "TMPDIR": str(self.work),
        }

    def run_script(self, script, env, *args):
        return subprocess.run(
            ["bash", str(script), *args],
            cwd=self.work,
            env=env,
            capture_output=True,
            text=True,
            timeout=30,
            check=False,
        )

    @staticmethod
    def events(path):
        if not path.exists():
            return []
        return [json.loads(line) for line in path.read_text(encoding="utf-8").splitlines()]

    # These stand-ins consume existing child-command arguments; none contact PVE or a GPU.
    def local_fixture(self, case_name, name="synthetic-run"):
        case = self.work / case_name
        scratch_root = case / "remote/scratch"
        scratch = scratch_root / name
        push = case / "input files"
        job = case / "synthetic job.sh"
        out = case / "copied results"
        study_root = case / "study root"
        logs = case / "logs"
        for directory in (scratch_root, push, study_root, logs):
            directory.mkdir(parents=True)
        (scratch_root / "untouched.txt").write_text("outside the job scratch\n", encoding="utf-8")
        (push / "input.txt").write_text("synthetic input\n", encoding="utf-8")
        job.write_text("# synthetic job; never executed\n", encoding="utf-8")
        executable(
            self.bin / "pct",
            """\
            #!/usr/bin/env python3
            import json, os, pathlib, shutil, subprocess, sys
            args = sys.argv[1:]
            if args[:3] != ["exec", os.environ["MOCK_CT"], "--"]:
                sys.exit(97)
            cmd = args[3:]
            with open(os.environ["PCT_TRACE"], "a", encoding="utf-8") as trace:
                trace.write(json.dumps(cmd) + "\\n")
            root = pathlib.Path(os.environ["MOCK_SCRATCH_ROOT"])
            scratch = pathlib.Path(os.environ["MOCK_SCRATCH"])
            if not cmd:
                sys.exit(97)
            if cmd[0] == "nvidia-smi":
                print("12000" if "--query-gpu=memory.free" in cmd else "0,0")
                sys.exit(0)
            if cmd[0] == "mkdir":
                paths = [pathlib.Path(arg) for arg in cmd[1:] if not arg.startswith("-")]
                if any(path not in (root, scratch, scratch / "in", scratch / "out") for path in paths):
                    sys.exit(98)
                try:
                    for path in paths:
                        path.mkdir(parents="-p" in cmd, exist_ok="-p" in cmd)
                except OSError:
                    sys.exit(31)
                sys.exit(0)
            if cmd[0] == "tar":
                if cmd[1:] == ["-C", str(scratch / "in"), "-xf", "-"]:
                    if os.environ.get("MOCK_INPUT_FAIL") == "1":
                        sys.stdin.buffer.read()
                        sys.exit(41)
                    os.execv(os.environ["REAL_TAR"], [os.environ["REAL_TAR"], *cmd[1:]])
                if cmd[1:] == ["-C", str(scratch / "out"), "-cf", "-", "."]:
                    status = subprocess.run([os.environ["REAL_TAR"], *cmd[1:]], check=False).returncode
                    if status:
                        sys.exit(status)
                    sys.exit(42 if os.environ.get("MOCK_PRODUCER_FAIL") == "1" else 0)
                sys.exit(98)
            if cmd[:2] == ["sh", "-c"] and cmd[2:] == ["cat > " + str(scratch / "job.sh")]:
                (scratch / "job.sh").write_bytes(sys.stdin.buffer.read())
                sys.exit(0)
            if cmd[0] == "podman":
                if "-v" not in cmd or cmd[cmd.index("-v") + 1] != str(scratch) + ":/work":
                    sys.exit(98)
                (scratch / "out").mkdir(parents=True, exist_ok=True)
                (scratch / "out/result.txt").write_bytes(b"synthetic completed result\\n")
                print("synthetic job log")
                sys.exit(int(os.environ.get("MOCK_JOB_STATUS", "0")))
            if cmd[:2] == ["rm", "-rf"]:
                paths = [arg for arg in cmd[2:] if arg != "--"]
                if paths != [str(scratch)] or scratch.parent != root or scratch.is_symlink():
                    sys.exit(98)
                if os.environ.get("MOCK_CLEANUP_FAIL") == "1":
                    sys.exit(43)
                shutil.rmtree(scratch)
                sys.exit(0)
            sys.exit(98)
            """,
        )
        executable(
            self.bin / "tar",
            """\
            #!/usr/bin/env python3
            import json, os, sys
            args = sys.argv[1:]
            with open(os.environ["TAR_TRACE"], "a", encoding="utf-8") as trace:
                trace.write(json.dumps(args) + "\\n")
            if (args[:2] == ["-C", os.environ["MOCK_OUT"]] and "-xf" in args
                    and os.environ.get("MOCK_CONSUMER_FAIL") == "1"):
                sys.stdin.buffer.read()
                sys.exit(44)
            os.execv(os.environ["REAL_TAR"], [os.environ["REAL_TAR"], *args])
            """,
        )
        executable(self.bin / "sleep", "#!/bin/sh\nexit 1\n")
        executable(
            self.bin / "date",
            """\
            #!/bin/sh
            case "$1" in
              +%s) echo 1700000000;;
              -Is) echo 2026-11-03T00:00:00Z;;
              *) exit 99;;
            esac
            """,
        )

        source = (STUDY / "local-gpu.sh").read_text(encoding="utf-8")
        for old, new in (
            ("SCRATCH=/tmp/gpu-studies", "SCRATCH=" + shlex.quote(str(scratch_root))),
            ('"/tmp/$NAME.gpu"', '"' + str(logs) + '/$NAME.gpu"'),
            ('"/tmp/$NAME.log"', '"' + str(logs) + '/$NAME.log"'),
            ("-e HOME=/tmp", "-e HOME=" + shlex.quote(str(case / "home"))),
        ):
            self.assertIn(old, source)
            source = source.replace(old, new)
        self.assertNotIn("/tmp/", source)
        script = case / "local-gpu.sh"
        script.write_text(source, encoding="utf-8")
        env = self.env.copy()
        env.update(
            GPU_STUDY_ROOT=str(study_root),
            MOCK_CT="123",
            MOCK_OUT=str(out),
            MOCK_SCRATCH=str(scratch),
            MOCK_SCRATCH_ROOT=str(scratch_root),
            PCT_TRACE=str(case / "pct-trace"),
            REAL_TAR=shutil.which("tar"),
            TAR_TRACE=str(case / "tar-trace"),
        )
        args = (
            "--ct", "123", "--name", name, "--image", "synthetic-image",
            "--push-dir", str(push), "--job", str(job), "--out", str(out),
        )
        return script, args, scratch, out, case, env

    def assert_retained(self, result, scratch, case, env):
        self.assertTrue(scratch.is_dir(), "scratch was removed after a failed transfer")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn(str(scratch), result.stderr)
        self.assertIn("123", result.stderr)
        self.assertEqual((scratch / "out/result.txt").read_bytes(), RESULT)
        self.assertFalse(any(cmd[:2] == ["rm", "-rf"] for cmd in self.events(case / "pct-trace")))
        self.assertEqual((scratch.parent / "untouched.txt").read_text(encoding="utf-8"),
                         "outside the job scratch\n")
        self.assertFalse((Path(env["GPU_STUDY_ROOT"]) / "local-gpu.csv").exists())

    def test_pct_producer_failure_retains_complete_scratch(self):
        script, args, scratch, out, case, env = self.local_fixture("producer")
        env["MOCK_PRODUCER_FAIL"] = "1"
        result = self.run_script(script, env, *args)
        self.assert_retained(result, scratch, case, env)
        self.assertEqual((out / "result.txt").read_bytes(), RESULT)
        self.assertIn("transfer", result.stderr.lower())

    def test_local_tar_failure_retains_complete_scratch(self):
        script, args, scratch, out, case, env = self.local_fixture("consumer")
        env["MOCK_CONSUMER_FAIL"] = "1"
        result = self.run_script(script, env, *args)
        self.assert_retained(result, scratch, case, env)
        self.assertTrue(out.is_dir())
        self.assertIn("transfer", result.stderr.lower())

    def test_successful_output_transfer_cleans_only_its_scratch(self):
        script, args, scratch, out, case, env = self.local_fixture("success")
        result = self.run_script(script, env, *args)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual((out / "result.txt").read_bytes(), RESULT)
        self.assertEqual((out / "container.log").read_text(encoding="utf-8"), "synthetic job log\n")
        self.assertFalse(scratch.exists())
        self.assertEqual((scratch.parent / "untouched.txt").read_text(encoding="utf-8"),
                         "outside the job scratch\n")
        deletions = [cmd for cmd in self.events(case / "pct-trace") if cmd[:2] == ["rm", "-rf"]]
        self.assertEqual(len(deletions), 1)
        self.assertEqual(deletions[0][-1], str(scratch))
        ledger = Path(env["GPU_STUDY_ROOT"]) / "local-gpu.csv"
        self.assertIn("synthetic-run,123,", ledger.read_text(encoding="utf-8"))

    def test_failed_job_is_recorded_after_successful_transfer(self):
        script, args, scratch, out, _, env = self.local_fixture("failed-job")
        env["MOCK_JOB_STATUS"] = "17"
        result = self.run_script(script, env, *args)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual((out / "result.txt").read_bytes(), RESULT)
        self.assertFalse(scratch.exists())
        ledger = Path(env["GPU_STUDY_ROOT"]) / "local-gpu.csv"
        self.assertEqual(ledger.read_text(encoding="utf-8").strip().split(",")[-1], "17")

    def test_safe_hidden_scratch_name_remains_supported(self):
        script, args, scratch, out, _, env = self.local_fixture("hidden", name=".study_run-1")
        result = self.run_script(script, env, *args)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual((out / "result.txt").read_bytes(), RESULT)
        self.assertFalse(scratch.exists())

    def test_cannot_reuse_retained_scratch_or_broaden_cleanup(self):
        script, args, scratch, _, case, env = self.local_fixture("collision")
        (scratch / "out").mkdir(parents=True)
        (scratch / "out/result.txt").write_bytes(b"previous retained result\n")
        result = self.run_script(script, env, *args)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn(str(scratch), result.stderr)
        self.assertEqual((scratch / "out/result.txt").read_bytes(), b"previous retained result\n")
        commands = self.events(case / "pct-trace")
        self.assertFalse(any(cmd[0] in ("podman", "rm") for cmd in commands))

        script, args, scratch, _, case, env = self.local_fixture("symlink-collision")
        target = case / "outside"
        target.mkdir()
        (target / "result.txt").write_bytes(b"untouched result\n")
        scratch.symlink_to(target, target_is_directory=True)
        result = self.run_script(script, env, *args)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn(str(scratch), result.stderr)
        self.assertTrue(scratch.is_symlink())
        self.assertEqual((target / "result.txt").read_bytes(), b"untouched result\n")
        self.assertFalse(any(cmd[0] in ("podman", "rm") for cmd in self.events(case / "pct-trace")))

        for index, name in enumerate(("", ".", "..", "../outside", "bad/name", "bad;name")):
            with self.subTest(name=name):
                script, args, _, _, case, env = self.local_fixture(f"invalid-name-{index}")
                args = list(args)
                args[args.index("--name") + 1] = name
                result = self.run_script(script, env, *args)
                self.assertNotEqual(result.returncode, 0)
                self.assertEqual(self.events(case / "pct-trace"), [])
                self.assertEqual((Path(env["MOCK_SCRATCH_ROOT"]) / "untouched.txt").read_text(),
                                 "outside the job scratch\n")

    def test_setup_and_output_directory_failures_retain_scratch(self):
        script, args, scratch, _, case, env = self.local_fixture("input-failure")
        env["MOCK_INPUT_FAIL"] = "1"
        result = self.run_script(script, env, *args)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn(str(scratch), result.stderr)
        self.assertTrue(scratch.is_dir())
        self.assertFalse(any(cmd[0] in ("podman", "rm") for cmd in self.events(case / "pct-trace")))

        script, args, scratch, _, case, env = self.local_fixture("missing-job")
        args = list(args)
        args[args.index("--job") + 1] = str(case / "missing-job.sh")
        result = self.run_script(script, env, *args)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn(str(scratch), result.stderr)
        self.assertTrue(scratch.is_dir())
        self.assertFalse(any(cmd[0] in ("podman", "rm") for cmd in self.events(case / "pct-trace")))

        script, args, scratch, _, case, env = self.local_fixture("bad-output")
        blocker = case / "not-a-directory"
        blocker.write_text("fixture\n", encoding="utf-8")
        out = blocker / "results"
        args = list(args)
        args[args.index("--out") + 1] = str(out)
        env["MOCK_OUT"] = str(out)
        result = self.run_script(script, env, *args)
        self.assert_retained(result, scratch, case, env)

    def test_cleanup_failure_is_reported_after_successful_copy(self):
        script, args, scratch, out, case, env = self.local_fixture("cleanup-failure")
        env["MOCK_CLEANUP_FAIL"] = "1"
        result = self.run_script(script, env, *args)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn(str(scratch), result.stderr)
        self.assertEqual((scratch / "out/result.txt").read_bytes(), RESULT)
        self.assertEqual((out / "result.txt").read_bytes(), RESULT)
        self.assertTrue(any(cmd[:2] == ["rm", "-rf"] for cmd in self.events(case / "pct-trace")))

    def watcher_fixture(self, case_name):
        case = self.work / case_name
        root = case / "study root"
        (root / "scripts").mkdir(parents=True)
        listing = case / "listing.jsonl"
        listing.write_text(json.dumps({
            "label": "usnm-gpu-s4surya3090", "public_ipaddr": "203.0.113.8",
            "ports": {"22/tcp": [{"HostPort": "2222"}]},
        }) + "\n", encoding="utf-8")
        executable(
            root / "scripts/vast.sh",
            """\
            #!/usr/bin/env python3
            import os, pathlib
            counter = pathlib.Path(os.environ["VAST_CALLS"])
            count = int(counter.read_text()) + 1 if counter.exists() else 1
            counter.write_text(str(count))
            if count <= 2:
                print(pathlib.Path(os.environ["VAST_LISTING"]).read_text(), end="")
            """,
        )
        executable(
            self.bin / "ssh",
            """\
            #!/usr/bin/env python3
            import json, os, sys
            args = sys.argv[1:]
            with open(os.environ["SSH_TRACE"], "a", encoding="utf-8") as trace:
                trace.write(json.dumps(args) + "\\n")
            if args[-1] == "wc -l < /work/out/surya-pages.jsonl":
                command = "count"
                print("20")
            elif args[-1] == "pkill -f surya_read.py":
                command = "stop"
                print("stopped")
            else:
                sys.exit(97)
            sys.exit(57 if os.environ.get("SSH_FAIL_ON") == command else 0)
            """,
        )
        executable(
            self.bin / "tail",
            """\
            #!/usr/bin/env python3
            import os, pathlib, sys
            if sys.argv[1:] != ["-1"]:
                sys.exit(97)
            lines = sys.stdin.read().splitlines()
            if lines:
                print(lines[-1])
            counter = pathlib.Path(os.environ["TAIL_CALLS"])
            count = int(counter.read_text()) + 1 if counter.exists() else 1
            counter.write_text(str(count))
            sys.exit(71 if os.environ.get("TAIL_FAIL_AT") == str(count) else 0)
            """,
        )
        executable(self.bin / "date", "#!/bin/sh\necho 2026-11-03T00:00:00Z\n")
        executable(self.bin / "sleep", "#!/bin/sh\nexit 1\n")
        source = (STUDY / "s4-stop-at-20.sh").read_text(encoding="utf-8")
        source = source.replace("/tmp/kh-w", str(case / "known-hosts-"))
        self.assertNotIn("/tmp/", source)
        script = case / "s4-stop-at-20.sh"
        script.write_text(source, encoding="utf-8")
        env = self.env.copy()
        env.update(
            GPU_STUDY_ROOT=str(root),
            GPU_STUDY_SECRETS=str(case / "synthetic key directory"),
            SSH_TRACE=str(case / "ssh-trace"),
            TAIL_CALLS=str(case / "tail-calls"),
            VAST_CALLS=str(case / "vast-calls"),
            VAST_LISTING=str(listing),
        )
        return script, root, case, env

    def test_watcher_ssh_failure_does_not_accept_partial_output(self):
        for command, expected_calls in (("count", 1), ("stop", 2)):
            with self.subTest(command=command):
                script, root, case, env = self.watcher_fixture("ssh-" + command)
                env["SSH_FAIL_ON"] = command
                result = self.run_script(script, env)
                self.assertEqual(result.returncode, 57, result.stderr)
                self.assertIn("fail", result.stderr.lower())
                self.assertNotIn("stopped", result.stdout)
                self.assertEqual(len(self.events(case / "ssh-trace")), expected_calls)
                log = root / "s4watch.log"
                self.assertNotIn("S4-WATCH-DONE", log.read_text() if log.exists() else "")

    def test_watcher_tail_failure_is_propagated(self):
        for failure, expected_calls in (("1", 1), ("2", 2)):
            with self.subTest(failure=failure):
                script, root, case, env = self.watcher_fixture("tail-" + failure)
                env["TAIL_FAIL_AT"] = failure
                result = self.run_script(script, env)
                self.assertEqual(result.returncode, 71, result.stderr)
                self.assertIn("fail", result.stderr.lower())
                self.assertNotIn("stopped", result.stdout)
                self.assertEqual(len(self.events(case / "ssh-trace")), expected_calls)
                log = root / "s4watch.log"
                self.assertNotIn("S4-WATCH-DONE", log.read_text() if log.exists() else "")

    def test_watcher_success_preserves_commands_and_output(self):
        script, root, case, env = self.watcher_fixture("success-watch")
        result = self.run_script(script, env)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("stopped", result.stdout)
        calls = self.events(case / "ssh-trace")
        self.assertEqual([cmd[-1] for cmd in calls], [
            "wc -l < /work/out/surya-pages.jsonl", "pkill -f surya_read.py",
        ])
        self.assertIn("S4-WATCH-DONE", (root / "s4watch.log").read_text(encoding="utf-8"))


if __name__ == "__main__":
    unittest.main()
