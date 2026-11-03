"""Synthetic regression tests; run from the repository root with Python's unittest."""

import json
import os
from pathlib import Path
import runpy
import shutil
import signal
import subprocess
import sys
import time
import types
import unittest
from unittest import mock


HERE = Path(__file__).resolve().parent
WORK = Path.cwd() / f".study8-test-{os.getpid()}"


def executable(path, text):
    path.write_text(text, encoding="utf-8")
    path.chmod(0o755)


class RunnerTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        WORK.mkdir()

    @classmethod
    def tearDownClass(cls):
        shutil.rmtree(WORK)

    def setUp(self):
        self.work = WORK / self._testMethodName
        self.work.mkdir()
        self.bin = self.work / "bin"
        self.bin.mkdir()
        self.trace = self.work / "trace"
        self.state = self.work / "paused"
        self.env = os.environ.copy()
        self.env.update(
            PATH=str(self.bin) + os.pathsep + os.environ["PATH"],
            TRACE=str(self.trace),
            STATE=str(self.state),
        )
        executable(
            self.bin / "date",
            """#!/usr/bin/env python3
import os, sys, time
if sys.argv[1:] == ['-Is']:
    print('2026-01-01T00:00:00Z')
elif sys.argv[1:] == ['+%s%N']:
    print(time.time_ns())
else:
    os.execv('/bin/date', ['/bin/date'] + sys.argv[1:])
""",
        )
        executable(
            self.bin / "boinccmd",
            """#!/usr/bin/env python3
import os, pathlib, sys
args = sys.argv[1:]
with open(os.environ['TRACE'], 'a') as f:
    f.write('BOINC ' + ' '.join(args) + '\\n')
if args == ['--get_tasks']:
    if os.environ.get('FAIL_STATUS') == '1' or (os.environ.get('FAIL_AFTER') == '1' and pathlib.Path(os.environ['STATE']).exists()):
        sys.exit(7)
elif args[:2] == ['--set_gpu_mode', 'never']:
    if os.environ.get('FAIL_PAUSE') == '1':
        sys.exit(8)
    pathlib.Path(os.environ['STATE']).touch()
""",
        )
        executable(
            self.bin / "nvidia-smi",
            """#!/usr/bin/env bash
if [ "${BAD_GPU_STATUS:-0}" = 1 ]; then echo unknown; else echo 0; fi
""",
        )
        executable(self.bin / "sleep", "#!/usr/bin/env bash\nexit 0\n")

    def run_script(self, *args, env=None):
        return subprocess.run(
            args,
            cwd=self.work,
            env=env or self.env,
            text=True,
            capture_output=True,
            timeout=15,
            check=False,
        )

    def trace_lines(self):
        return self.trace.read_text(encoding="utf-8").splitlines() if self.trace.exists() else []

    def window_fixture(self, body='printf "RUN %s\\n" "$*" >> "$TRACE"\nexit "${RUN_RC:-0}"\n'):
        scripts = self.work / "scripts" / "yiddish"
        scripts.mkdir(parents=True)
        shutil.copy2(HERE / "gpu-window.sh", scripts)
        executable(scripts / "qwen-native-units.sh", "#!/usr/bin/env bash\n" + body)
        units = self.work / "units"
        units.mkdir()
        log = self.work / "window.log"
        args = (
            "bash", str(scripts / "gpu-window.sh"), "--seconds", "65", "--unit-seconds", "1",
            "--wait-max", "0", "--units", str(units), "--log", str(log),
        )
        return args, log

    def test_window_aborts_on_failed_pause_or_status(self):
        args, _ = self.window_fixture()
        for setting in ("FAIL_PAUSE", "FAIL_STATUS", "BAD_GPU_STATUS"):
            with self.subTest(setting=setting):
                self.trace.unlink(missing_ok=True)
                self.state.unlink(missing_ok=True)
                env = self.env.copy()
                env[setting] = "1"
                result = self.run_script(*args, env=env)
                self.assertNotEqual(result.returncode, 0)
                trace = self.trace_lines()
                self.assertFalse(any(line.startswith("RUN ") for line in trace))
                self.assertNotIn("BOINC --set_gpu_mode auto", trace)

    def test_window_restores_after_runner_or_post_pause_failure(self):
        args, _ = self.window_fixture()
        env = self.env.copy()
        env["RUN_RC"] = "23"
        result = self.run_script(*args, env=env)
        self.assertEqual(result.returncode, 23, result.stderr)
        trace = self.trace_lines()
        self.assertIn("BOINC --set_gpu_mode never 65", trace)
        run = next(i for i, line in enumerate(trace) if line.startswith("RUN "))
        self.assertIn("--boinc-until", trace[run])
        self.assertGreater(trace.index("BOINC --set_gpu_mode auto"), run)

        self.trace.unlink()
        self.state.unlink()
        env = self.env.copy()
        env["FAIL_AFTER"] = "1"
        result = self.run_script(*args, env=env)
        self.assertNotEqual(result.returncode, 0)
        trace = self.trace_lines()
        self.assertFalse(any(line.startswith("RUN ") for line in trace))
        self.assertEqual(trace[-1], "BOINC --set_gpu_mode auto")

    def test_window_waits_for_interrupted_runner_before_restoring(self):
        args, _ = self.window_fixture(
            """trap 'echo CLEANING >> "$TRACE"; /bin/sleep 0.3; echo STOPPED >> "$TRACE"; exit 143' TERM
echo RUN >> "$TRACE"
while :; do /bin/sleep 0.2; done
"""
        )
        process = subprocess.Popen(args, cwd=self.work, env=self.env, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        try:
            limit = time.monotonic() + 5
            while "RUN" not in self.trace_lines() and time.monotonic() < limit:
                time.sleep(0.05)
            self.assertIn("RUN", self.trace_lines())
            process.send_signal(signal.SIGTERM)
            process.communicate(timeout=8)
            self.assertEqual(process.returncode, 143)
            trace = self.trace_lines()
            self.assertLess(trace.index("STOPPED"), trace.index("BOINC --set_gpu_mode auto"))
        finally:
            if process.poll() is None:
                process.terminate()
                process.communicate(timeout=8)

    def test_window_driver_calls_sibling_and_stops_on_failure(self):
        scripts = self.work / "scripts" / "yiddish"
        scripts.mkdir(parents=True)
        shutil.copy2(HERE / "gpu-windows.sh", scripts)
        executable(scripts / "gpu-window.sh", '#!/usr/bin/env bash\necho "WINDOW $*" >> "$TRACE"\nexit 42\n')
        units = self.work / "units with spaces"
        (units / "b001").mkdir(parents=True)
        result = self.run_script(
            "bash", str(scripts / "gpu-windows.sh"), "--gap", "0", "--max-windows", "3",
            "--units", str(units), "--log", str(self.work / "driver.log"),
        )
        self.assertEqual(result.returncode, 42, result.stderr)
        trace = self.trace_lines()
        self.assertEqual(len(trace), 1)
        self.assertIn(str(units), trace[0])

    def test_native_runner_never_caches_a_timed_out_unit(self):
        scripts = self.work / "scripts" / "yiddish"
        scripts.mkdir(parents=True)
        shutil.copy2(HERE / "qwen-native-units.sh", scripts)
        (scripts / "run-deadline.py").write_text(
            """import os, pathlib, sys
with open(os.environ['TRACE'], 'a') as f: f.write('SUPERVISOR ' + ' '.join(sys.argv[1:]) + '\\n')
pathlib.Path('result.json').write_text('synthetic result')
pathlib.Path('qwen_checkpoint.json').write_text('synthetic checkpoint')
sys.exit(int(os.environ.get('RUN_RC', '0')))
""",
            encoding="utf-8",
        )
        units = self.work / "units"
        unit = units / "b001"
        unit.mkdir(parents=True)
        (unit / "qwen.json").write_text("synthetic", encoding="utf-8")
        (unit / "page.jpg").write_bytes(b"synthetic")
        appdir = self.work / "app"
        appdir.mkdir()
        (appdir / "dummy").touch()
        (appdir / "dummy_helper").touch()
        for model in ("Qwen3VL-8B-Instruct-Q4_K_M.gguf", "mmproj-Qwen3VL-8B-Instruct-Q8_0.gguf"):
            (appdir / f"usnm_qwen_test_{model}").touch()
        args = (
            "bash", str(scripts / "qwen-native-units.sh"), "--units", str(units),
            "--app-dir", str(appdir), "--app", "dummy", "--plan-class", "synthetic",
            "--scratch", str(self.work / "scratch"), "--until", str(int(time.time()) + 20),
            "--boinc-until", str(int(time.time()) + 80), "--unit-seconds", "1",
            "--gpu-log", str(self.work / "gpu.log"),
        )
        env = self.env.copy()
        env["RUN_RC"] = "124"
        failed = self.run_script(*args, env=env)
        self.assertEqual(failed.returncode, 124, failed.stderr)
        self.assertFalse((unit / "result.json").exists())
        self.assertFalse((unit / "qwen_checkpoint.final.json").exists())
        self.assertIn("--boinc-until", self.trace_lines()[0])

        succeeded = self.run_script(*args)
        self.assertEqual(succeeded.returncode, 0, succeeded.stderr)
        self.assertTrue((unit / "result.json").exists())
        self.assertTrue((unit / "qwen_checkpoint.final.json").exists())

    def tesseract_fixture(self):
        executable(
            self.bin / "podman",
            """#!/usr/bin/env bash
for arg in "$@"; do case "$arg" in /work/*) input=${arg#/work/};; esac; done
printf '%s\\n' "$input" >> "$TRACE"
echo synthetic
if [ "$input" = bad.png ] && [ "${FAIL_BAD:-0}" = 1 ]; then exit 42; fi
""",
        )
        images = self.work / "images"
        images.mkdir()
        names = ["ok.png", "bad.png", "evil;touch INJECTED.png"]
        for name in names:
            (images / name).touch()
        listing = self.work / "images.list"
        listing.write_text("\n".join(names) + "\n", encoding="utf-8")
        out = self.work / "results"
        args = (
            "bash", str(HERE / "tesseract-pages.sh"), "--dir", str(images), "--list", str(listing),
            "--image", "synthetic", "--out", str(out), "--jobs", "2",
        )
        return args, out

    def read_rows(self, out):
        result = self.run_script(
            sys.executable, "-I", str(HERE / "to-jsonl.py"), "--kind", "tesseract",
            "--dir", str(out), "--out", str(self.work / "out.jsonl"),
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        return [json.loads(line) for line in (self.work / "out.jsonl").read_text().splitlines()]

    def test_tesseract_failure_is_missing_retryable_and_filenames_are_literal(self):
        args, out = self.tesseract_fixture()
        out.mkdir()
        (out / "done").write_text("stale", encoding="utf-8")
        env = self.env.copy()
        env["FAIL_BAD"] = "1"
        result = self.run_script(*args, env=env)
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse((out / "done").exists())
        self.assertTrue((out / "txt" / "ok.txt").is_file())
        self.assertFalse((out / "txt" / "bad.txt").exists())
        self.assertFalse((out / "txt" / "bad.tmp").exists())
        self.assertFalse((self.work / "INJECTED.png").exists())
        (out / "txt" / "bad.txt").write_text("stale synthetic output", encoding="utf-8")
        (out / "txt" / "orphan.txt").write_text("unverified synthetic output", encoding="utf-8")
        self.assertEqual({row["page"] for row in self.read_rows(out)}, {"ok", "evil;touch INJECTED"})

        result = self.run_script(*args)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual({row["page"] for row in self.read_rows(out)}, {"ok", "bad", "evil;touch INJECTED"})
        self.assertEqual(self.trace_lines().count("ok.png"), 1)
        self.assertEqual(self.trace_lines().count("bad.png"), 2)

        with (out / "timing.tsv").open("a", encoding="utf-8") as timing:
            timing.write("ok\t1\t0\textra\n")
        self.assertNotIn("ok", {row["page"] for row in self.read_rows(out)})
        result = self.run_script(*args)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.trace_lines().count("ok.png"), 2)

    def test_import_uses_latest_receipt_and_requires_one(self):
        out = self.work / "results"
        (out / "txt").mkdir(parents=True)
        for name in ("failed", "success", "unverified"):
            (out / "txt" / f"{name}.txt").write_text("synthetic", encoding="utf-8")
        (out / "timing.tsv").write_text("failed\t1\t0\nfailed\t2\t9\nsuccess\t1\t0\n", encoding="utf-8")
        self.assertEqual([row["page"] for row in self.read_rows(out)], ["success"])

    def test_cleanup_waits_when_a_process_snapshot_misses_an_adopted_child(self):
        supervisor = runpy.run_path(str(HERE / "run-deadline.py"))
        cleanup = supervisor["cleanup"]
        for process in (None, mock.Mock(returncode=0)):
            with self.subTest(process=process):
                if process is not None:
                    process.poll.return_value = 0
                adopted = mock.Mock(side_effect=[True, False])
                snapshot = mock.Mock(return_value=set())
                clock = types.SimpleNamespace(monotonic=lambda: 0, sleep=lambda _delay: None)
                with mock.patch.dict(cleanup.__globals__, reap_adopted=adopted, descendants=snapshot, time=clock):
                    cleanup(process, 0)
                self.assertEqual(adopted.call_count, 2)

    @staticmethod
    def descendant_workload(marker, orphan):
        server = (
            "import signal,time; signal.signal(signal.SIGTERM,signal.SIG_IGN); "
            "print('ready',flush=True); time.sleep(30)"
        )
        ending = "os._exit(0)" if orphan else "time.sleep(30)"
        return f"""
import json, os, subprocess, sys, time
server = subprocess.Popen([sys.executable, '-c', {server!r}], stdout=subprocess.PIPE, start_new_session=True)
assert server.stdout.readline() == b'ready\\n'
with open({str(marker)!r}, 'w') as f: json.dump([os.getpid(), server.pid], f)
{ending}
"""

    def assert_gone(self, marker):
        for pid in json.loads(marker.read_text(encoding="utf-8")):
            with self.assertRaises(ProcessLookupError, msg=f"process {pid} survived cleanup"):
                os.kill(pid, 0)

    @unittest.skipUnless(sys.platform == "linux", "the native supervisor requires Linux")
    def test_deadline_kills_detached_descendants_and_renews_a_near_expiry(self):
        marker = self.work / "pids.json"
        result = self.run_script(
            sys.executable, "-I", str(HERE / "run-deadline.py"),
            "--until", str(time.time() + 1.5), "--boinc-until", str(time.time() + 2),
            "--", sys.executable, "-c", self.descendant_workload(marker, orphan=False),
        )
        self.assertEqual(result.returncode, 124, result.stderr)
        self.assert_gone(marker)
        self.assertIn("BOINC --set_gpu_mode never 120", self.trace_lines())

    @unittest.skipUnless(sys.platform == "linux", "the native supervisor requires Linux")
    def test_successful_parent_cannot_leave_detached_orphans(self):
        marker = self.work / "pids.json"
        result = self.run_script(
            sys.executable, "-I", str(HERE / "run-deadline.py"),
            "--until", str(time.time() + 10), "--", sys.executable, "-c",
            self.descendant_workload(marker, orphan=True),
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assert_gone(marker)

    @unittest.skipUnless(sys.platform == "linux", "the native supervisor requires Linux")
    def test_interrupted_supervisor_cleans_up_before_returning(self):
        marker = self.work / "pids.json"
        args = (
            sys.executable, "-I", str(HERE / "run-deadline.py"),
            "--until", str(time.time() + 20), "--", sys.executable, "-c",
            self.descendant_workload(marker, orphan=False),
        )
        process = subprocess.Popen(args, cwd=self.work, env=self.env, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        try:
            limit = time.monotonic() + 5
            while not marker.exists() and time.monotonic() < limit:
                time.sleep(0.05)
            self.assertTrue(marker.exists())
            process.send_signal(signal.SIGTERM)
            _, stderr = process.communicate(timeout=8)
            self.assertEqual(process.returncode, 143, stderr.decode())
            self.assert_gone(marker)
        finally:
            if process.poll() is None:
                process.terminate()
                process.communicate(timeout=8)


if __name__ == "__main__":
    unittest.main()
