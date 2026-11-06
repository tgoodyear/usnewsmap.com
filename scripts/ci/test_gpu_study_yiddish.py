"""Synthetic regression tests; run from the repository root with Python's unittest."""

import json
import os
from pathlib import Path
import runpy
import shlex
import shutil
import signal
import subprocess
import sys
import time
import types
import unittest
from unittest import mock
import zipfile


HERE = Path(__file__).resolve().parents[2] / "docs/reports/2026-11-gpu-studies/scripts/yiddish"
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
import json, os, pathlib, sys
args = sys.argv[1:]
if args[:2] == ['--set_gpu_mode', 'auto'] and os.environ.get('CHECK_PIDS'):
    for pid in json.loads(pathlib.Path(os.environ['CHECK_PIDS']).read_text()):
        try:
            os.kill(pid, 0)
        except ProcessLookupError:
            continue
        with open(os.environ['TRACE'], 'a') as f: f.write('OVERLAP ' + str(pid) + '\\n')
        sys.exit(9)
    with open(os.environ['TRACE'], 'a') as f: f.write('REAPED\\n')
with open(os.environ['TRACE'], 'a') as f:
    f.write('BOINC ' + ' '.join(args) + '\\n')
if args == ['--get_tasks']:
    if os.environ.get('FAIL_STATUS') == '1' or (os.environ.get('FAIL_AFTER') == '1' and pathlib.Path(os.environ['STATE']).exists()):
        sys.exit(7)
elif args[:2] == ['--set_gpu_mode', 'never']:
    if os.environ.get('FAIL_PAUSE') == '1' or (args[2:] == ['0'] and os.environ.get('FAIL_PERMANENT') == '1'):
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
            timeout=25,
            check=False,
        )

    def trace_lines(self):
        return self.trace.read_text(encoding="utf-8").splitlines() if self.trace.exists() else []

    def window_fixture(self, body='printf "RUN %s\\n" "$*" >> "$TRACE"\nexit "${RUN_RC:-0}"\n'):
        scripts = self.work / "scripts" / "yiddish"
        scripts.mkdir(parents=True)
        shutil.copy2(HERE / "gpu-window.sh", scripts)
        executable(scripts / "qwen-native-units.sh", "#!/usr/bin/env bash\n" + body)
        (scripts / "run-deadline.py").write_text(
            """import os, pathlib, signal, subprocess, sys
args = sys.argv[1:]
if os.environ.get('FAIL_SUPERVISOR') == '1':
    sys.exit(37)
marker = pathlib.Path(args[args.index('--completion-file') + 1])
command = args[args.index('--') + 1:]
child = subprocess.Popen(command)
interrupted = 0
def stop(signum, _frame):
    global interrupted
    interrupted = 128 + signum
    child.terminate()
signal.signal(signal.SIGTERM, stop)
rc = child.wait()
marker.touch()
sys.exit(interrupted or rc)
""",
            encoding="utf-8",
        )
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

    def test_window_fails_closed_when_supervisor_cannot_confirm_cleanup(self):
        args, log = self.window_fixture()
        env = self.env.copy()
        env["FAIL_SUPERVISOR"] = "1"
        result = self.run_script(*args, env=env)
        self.assertEqual(result.returncode, 125, result.stderr)
        self.assertIn("BOINC --set_gpu_mode never 0", self.trace_lines())
        self.assertNotIn("BOINC --set_gpu_mode auto", self.trace_lines())
        self.assertIn("operator", log.read_text(encoding="utf-8"))

    def test_window_does_not_restore_if_emergency_hold_fails(self):
        args, log = self.window_fixture()
        env = self.env.copy()
        env.update(FAIL_SUPERVISOR="1", FAIL_PERMANENT="1")
        result = self.run_script(*args, env=env)
        self.assertEqual(result.returncode, 125, result.stderr)
        self.assertIn("BOINC --set_gpu_mode never 0", self.trace_lines())
        self.assertNotIn("BOINC --set_gpu_mode auto", self.trace_lines())
        self.assertIn("failed to extend", log.read_text(encoding="utf-8"))

    def test_window_restores_after_success(self):
        args, _ = self.window_fixture()
        result = self.run_script(*args)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.trace_lines()[-1], "BOINC --set_gpu_mode auto")

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

    def native_fixture(self):
        scripts = self.work / "scripts" / "yiddish"
        scripts.mkdir(parents=True)
        shutil.copy2(HERE / "qwen-native-units.sh", scripts)
        (scripts / "run-deadline.py").write_text(
            """import os, pathlib, subprocess, sys
with open(os.environ['TRACE'], 'a') as f: f.write('SUPERVISOR ' + ' '.join(sys.argv[1:]) + '\\n')
command = sys.argv[sys.argv.index('--') + 1:]
if command[0].endswith('qwen-native-units.sh'):
    os.environ['USNM_DEADLINE_PARENT_PID'] = str(os.getpid())
    sys.exit(subprocess.run(command, check=False).returncode)
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
        return list(args), unit

    def test_native_runner_never_caches_a_timed_out_unit(self):
        args, unit = self.native_fixture()
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

    def test_native_runner_does_not_cache_a_partial_result_copy(self):
        args, unit = self.native_fixture()
        executable(
            self.bin / "cp",
            """#!/usr/bin/env bash
case "$1" in
  */result.json) printf partial > "$2"; exit 42;;
  *) exec /bin/cp "$@";;
esac
""",
        )
        failed = self.run_script(*args)
        self.assertNotEqual(failed.returncode, 0)
        self.assertFalse((unit / "result.json").exists())

        (self.bin / "cp").unlink()
        succeeded = self.run_script(*args)
        self.assertEqual(succeeded.returncode, 0, succeeded.stderr)
        self.assertEqual((unit / "result.json").read_text(encoding="utf-8"), "synthetic result")

    def test_native_runner_requires_a_safe_hold_or_explicit_no_boinc(self):
        args, unit = self.native_fixture()
        until = args.index("--until") + 1
        hold = args.index("--boinc-until") + 1
        args[until] = str(int(time.time()) + 200)
        args[hold] = str(int(time.time()) + 30)
        result = self.run_script(*args)
        self.assertEqual(result.returncode, 2, result.stderr)
        self.assertFalse(unit.joinpath("result.json").exists())
        self.assertEqual(self.trace_lines(), [])

        args[hold] = str(int(args[until]) + 59)
        result = self.run_script(*args)
        self.assertEqual(result.returncode, 2, result.stderr)
        self.assertEqual(self.trace_lines(), [])

        result = self.run_script(*args, "--no-boinc")
        self.assertEqual(result.returncode, 2, result.stderr)
        self.assertEqual(self.trace_lines(), [])

        args[hold] = str(int(args[until]) + 60)
        result = self.run_script(*args, "--internal-supervised")
        self.assertEqual(result.returncode, 2, result.stderr)
        self.assertEqual(self.trace_lines(), [])

        del args[hold - 1:hold + 1]
        result = self.run_script(*args)
        self.assertEqual(result.returncode, 2, result.stderr)
        self.assertEqual(self.trace_lines(), [])

        result = self.run_script(*args, "--no-boinc")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("--no-boinc", self.trace_lines()[0])

    def test_supervisor_rejects_missing_or_unsafe_boinc_mode(self):
        marker = self.work / "unexpected-launch"
        until = time.time() + 120
        modes = (
            (),
            ("--boinc-until", str(until - 90)),
            ("--boinc-until", str(until + 59)),
            ("--boinc-until", str(until + 60), "--no-boinc"),
        )
        for mode in modes:
            with self.subTest(mode=mode):
                result = self.run_script(
                    sys.executable, "-I", str(HERE / "run-deadline.py"),
                    "--until", str(until), *mode,
                    "--", sys.executable, "-c", f"open({str(marker)!r},'w').close()",
                )
                self.assertEqual(result.returncode, 2, result.stderr)
                self.assertFalse(marker.exists())

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

    def read_rows(self, out, kind="tesseract"):
        result = self.run_script(
            sys.executable, "-I", str(HERE / "to-jsonl.py"), "--kind", kind,
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

    def test_tesseract_mounts_extra_models_read_only_only_when_asked(self):
        args, out = self.tesseract_fixture()
        executable(self.bin / "podman", '#!/usr/bin/env bash\nprintf "%s\\n" "$*" >> "$TRACE"\n')
        (self.work / "models").mkdir()
        with_models = list(args[:-2]) + ["--jobs", "1", "--tessdata", "models"]
        result = self.run_script(*with_models)
        self.assertEqual(result.returncode, 0, result.stderr)
        calls = self.trace_lines()
        self.assertEqual(len(calls), 3)
        for call in calls:
            self.assertIn(f"-v {self.work / 'models'}:/td:ro ", call)
            self.assertIn(" - --tessdata-dir /td -l yid ", call)

        self.trace.unlink()
        shutil.rmtree(out)
        result = self.run_script(*args)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(len(self.trace_lines()), 3)
        self.assertFalse(any("/td" in call for call in self.trace_lines()))

    def test_import_uses_latest_receipt_and_requires_one(self):
        out = self.work / "results"
        (out / "txt").mkdir(parents=True)
        for name in ("failed", "success", "unverified"):
            (out / "txt" / f"{name}.txt").write_text("synthetic", encoding="utf-8")
        (out / "timing.tsv").write_text("failed\t1\t0\nfailed\t2\t9\nsuccess\t1\t0\n", encoding="utf-8")
        self.assertEqual([row["page"] for row in self.read_rows(out)], ["success"])

    def jochre_fixture(self, names):
        executable(
            self.bin / "curl",
            """#!/usr/bin/env python3
import os, pathlib, sys, zipfile
args = sys.argv[1:]
out = pathlib.Path(args[args.index('-o') + 1])
image = next(a for a in args if a.startswith('image=@'))[7:].split(';type=')[0]
stem = pathlib.Path(image).stem
with open(os.environ['TRACE'], 'a') as f: f.write(stem + '\\n')
if os.environ.get('FAIL_JOCHRE') == '1':
    if stem in ('httpfail', 'timeout', 'badbody'):
        out.write_bytes(b'invalid synthetic response')
        print({'httpfail': '500', 'timeout': '000', 'badbody': '200'}[stem], end='')
        sys.exit(28 if stem == 'timeout' else 0)
    if stem == 'notext':
        with zipfile.ZipFile(out, 'w') as z: z.writestr('page.xml', b'synthetic')
        print('200', end='')
        sys.exit(0)
    if stem == 'badutf':
        with zipfile.ZipFile(out, 'w') as z: z.writestr('page.txt', b'\\xff')
        print('200', end='')
        sys.exit(0)
with zipfile.ZipFile(out, 'w') as z: z.writestr('page.txt', b'synthetic')
print('200', end='')
sys.exit(18 if os.environ.get('FAIL_JOCHRE') == '1' and stem == 'transport' else 0)
""",
        )
        images = self.work / "images"
        images.mkdir()
        for name in names:
            (images / f"{name}.png").touch()
        listing = self.work / "ids.list"
        listing.write_text("\n".join(names) + "\n", encoding="utf-8")
        out = self.work / "jochre"
        args = (
            "bash", str(HERE / "jochre-pages.sh"), "--pages", str(images),
            "--list", str(listing), "--out", str(out), "--url", "http://synthetic.invalid",
        )
        return args, out

    @staticmethod
    def write_zip(path, name="page.txt"):
        with zipfile.ZipFile(path, "w") as archive:
            archive.writestr(name, b"synthetic")

    def test_jochre_retries_failed_receipts_and_invalid_cached_archives(self):
        names = ["cached", "failed", "corrupt", "missing", "notext", "badcrc"]
        args, out = self.jochre_fixture(names)
        (out / "zip").mkdir(parents=True)
        for name in ("cached", "failed", "badcrc", "unverified"):
            self.write_zip(out / "zip" / f"{name}.zip")
        self.write_zip(out / "zip" / "notext.zip", name="page.xml")
        (out / "zip" / "corrupt.zip").write_bytes(b"not a zip")
        crc_path = out / "zip" / "badcrc.zip"
        data = bytearray(crc_path.read_bytes())
        data[data.index(b"synthetic")] ^= 1
        crc_path.write_bytes(data)
        self.write_zip(out / "zip" / "cached.zip.part")
        (out / "timing.tsv").write_text(
            "cached\t1\t200\nfailed\t1\t200\nfailed\t2\t500\n"
            "corrupt\t1\t200\nmissing\t1\t200\nnotext\t1\t200\nbadcrc\t1\t200\n",
            encoding="utf-8",
        )
        self.assertEqual([row["page"] for row in self.read_rows(out, "jochre")], ["cached"])
        result = self.run_script(*args)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(set(self.trace_lines()), set(names) - {"cached"})
        self.assertEqual({row["page"] for row in self.read_rows(out, "jochre")}, set(names))
        self.assertTrue((out / "timing.tsv.done").exists())

    def test_jochre_failed_transfers_are_missing_and_retryable(self):
        names = ["httpfail", "timeout", "badbody", "notext", "badutf", "transport"]
        args, out = self.jochre_fixture(names)
        out.mkdir()
        (out / "timing.tsv.done").write_text("stale", encoding="utf-8")
        env = self.env.copy()
        env["FAIL_JOCHRE"] = "1"
        result = self.run_script(*args, env=env)
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse((out / "timing.tsv.done").exists())
        self.assertEqual(list((out / "zip").iterdir()), [])
        self.assertEqual(self.read_rows(out, "jochre"), [])
        receipts = {parts[0]: parts[2] for line in (out / "timing.tsv").read_text().splitlines() if (parts := line.split("\t"))}
        self.assertEqual(receipts["httpfail"], "500")
        self.assertEqual(receipts["timeout"], "000")
        self.assertEqual(receipts["transport"], "000")

        result = self.run_script(*args)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual({row["page"] for row in self.read_rows(out, "jochre")}, set(names))
        for name in names:
            self.assertEqual(self.trace_lines().count(name), 2)

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

    def test_cleanup_extends_a_near_expiring_gpu_hold(self):
        supervisor = runpy.run_path(str(HERE / "run-deadline.py"))
        cleanup = supervisor["cleanup"]
        process = mock.Mock()
        process.poll.side_effect = [None, 0]
        clock = types.SimpleNamespace(monotonic=lambda: 1, time=lambda: 100, sleep=lambda _delay: None)
        renewal = mock.Mock(return_value=types.SimpleNamespace(returncode=0))
        commands = types.SimpleNamespace(run=renewal, DEVNULL=subprocess.DEVNULL, TimeoutExpired=subprocess.TimeoutExpired)
        with mock.patch.dict(
            cleanup.__globals__,
            reap_adopted=lambda: False,
            descendants=lambda: set(),
            time=clock,
            subprocess=commands,
        ):
            cleanup(process, 120)
        self.assertEqual(renewal.call_args.args[0], ["boinccmd", "--set_gpu_mode", "never", "120"])

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

    def full_window_fixture(self, kill_inner=False):
        scripts = self.work / "scripts" / "yiddish"
        scripts.mkdir(parents=True)
        appdir = self.work / "app"
        appdir.mkdir()
        app_name = "usnm_qwen_1.17_x86_64-pc-linux-gnu_nvidia_vulkan_8b"
        window = (HERE / "gpu-window.sh").read_text(encoding="utf-8")
        original = "--app-dir /var/lib/boinc-client/projects/boinc.usnewsmap.com"
        self.assertEqual(window.count(original), 1)
        window = window.replace(original, "--app-dir " + shlex.quote(str(appdir)))
        executable(scripts / "gpu-window.sh", window)
        shutil.copy2(HERE / "qwen-native-units.sh", scripts)
        shutil.copy2(HERE / "run-deadline.py", scripts)
        marker = self.work / "pids.json"
        workload = self.descendant_workload(marker, orphan=False)
        if kill_inner:
            workload = workload.replace("time.sleep(30)\n", "os.kill(os.getppid(), signal.SIGKILL)\ntime.sleep(30)\n")
            workload = "import signal\n" + workload
        executable(appdir / app_name, "#!/usr/bin/env python3\n" + workload)
        (appdir / f"{app_name}_helper").touch()
        for model in ("Qwen3VL-8B-Instruct-Q4_K_M.gguf", "mmproj-Qwen3VL-8B-Instruct-Q8_0.gguf"):
            (appdir / f"usnm_qwen_test_{model}").touch()
        executable(self.bin / "strace", '#!/usr/bin/env bash\nfor last in "$@"; do :; done\nexec "$last"\n')
        units = self.work / "units"
        unit = units / "b001"
        unit.mkdir(parents=True)
        (unit / "qwen.json").write_text("synthetic", encoding="utf-8")
        (unit / "page.jpg").write_bytes(b"synthetic")
        args = (
            "bash", str(scripts / "gpu-window.sh"), "--seconds", "68", "--unit-seconds", "1",
            "--wait-max", "0", "--units", str(units), "--log", str(self.work / "window.log"),
        )
        env = self.env.copy()
        env["CHECK_PIDS"] = str(marker)
        return args, env, marker, unit

    @unittest.skipUnless(sys.platform == "linux", "the native supervisor requires Linux")
    def test_window_reaps_detached_descendants_before_gpu_restore(self):
        args, env, marker, unit = self.full_window_fixture()
        result = self.run_script(*args, env=env)
        self.assertEqual(result.returncode, 124, result.stderr)
        self.assert_gone(marker)
        self.assertFalse((unit / "result.json").exists())
        trace = self.trace_lines()
        self.assertNotIn("OVERLAP", "\n".join(trace))
        self.assertLess(trace.index("REAPED"), trace.index("BOINC --set_gpu_mode auto"))

    @unittest.skipUnless(sys.platform == "linux", "the native supervisor requires Linux")
    def test_window_adopts_orphans_when_inner_supervisor_is_killed(self):
        args, env, marker, unit = self.full_window_fixture(kill_inner=True)
        result = self.run_script(*args, env=env)
        self.assertEqual(result.returncode, 137, result.stderr)
        self.assert_gone(marker)
        self.assertFalse((unit / "result.json").exists())
        trace = self.trace_lines()
        self.assertLess(trace.index("REAPED"), trace.index("BOINC --set_gpu_mode auto"))

    @unittest.skipUnless(sys.platform == "linux", "the native supervisor requires Linux")
    def test_direct_native_runner_reaps_orphans_when_inner_supervisor_is_killed(self):
        _, env, marker, unit = self.full_window_fixture(kill_inner=True)
        args = (
            "bash", str(self.work / "scripts" / "yiddish" / "qwen-native-units.sh"),
            "--units", str(unit.parent), "--app-dir", str(self.work / "app"),
            "--app", "usnm_qwen_1.17_x86_64-pc-linux-gnu_nvidia_vulkan_8b",
            "--plan-class", "synthetic", "--scratch", str(self.work / "scratch"),
            "--until", str(int(time.time()) + 15), "--no-boinc", "--unit-seconds", "1",
            "--gpu-log", str(self.work / "gpu.log"),
        )
        try:
            result = self.run_script(*args, env=env)
            self.assertEqual(result.returncode, 137, result.stderr)
            self.assert_gone(marker)
            self.assertFalse((unit / "result.json").exists())
        finally:
            if marker.exists():
                for pid in json.loads(marker.read_text(encoding="utf-8")):
                    try:
                        fd = os.pidfd_open(pid)
                    except ProcessLookupError:
                        continue
                    try:
                        signal.pidfd_send_signal(fd, signal.SIGKILL)
                    except ProcessLookupError:
                        pass
                    finally:
                        os.close(fd)

    @unittest.skipUnless(sys.platform == "linux", "the native supervisor requires Linux")
    def test_window_cancellation_reaps_descendants_before_gpu_restore(self):
        args, env, marker, _ = self.full_window_fixture()
        process = subprocess.Popen(args, cwd=self.work, env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        try:
            limit = time.monotonic() + 6
            while not marker.exists() and time.monotonic() < limit:
                time.sleep(0.05)
            self.assertTrue(marker.exists())
            process.send_signal(signal.SIGTERM)
            _, stderr = process.communicate(timeout=15)
            self.assertEqual(process.returncode, 143, stderr.decode())
            self.assert_gone(marker)
            trace = self.trace_lines()
            self.assertLess(trace.index("REAPED"), trace.index("BOINC --set_gpu_mode auto"))
        finally:
            if process.poll() is None:
                if marker.exists():
                    for pid in json.loads(marker.read_text(encoding="utf-8")):
                        try:
                            os.kill(pid, signal.SIGKILL)
                        except ProcessLookupError:
                            pass
                process.terminate()
                try:
                    process.communicate(timeout=8)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.communicate(timeout=8)

    @unittest.skipUnless(sys.platform == "linux", "the native supervisor requires Linux")
    def test_window_keeps_gpu_suspended_if_outer_supervisor_is_killed(self):
        args, env, marker, _ = self.full_window_fixture()
        process = subprocess.Popen(args, cwd=self.work, env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        descriptors = []
        try:
            limit = time.monotonic() + 6
            while not marker.exists() and time.monotonic() < limit:
                time.sleep(0.05)
            self.assertTrue(marker.exists())
            pids = json.loads(marker.read_text(encoding="utf-8"))

            def parent(pid):
                return int(Path(f"/proc/{pid}/stat").read_text().rsplit(")", 1)[1].split()[1])

            inner = parent(pids[0])
            outer = parent(parent(inner))
            self.assertEqual(parent(outer), process.pid)
            descriptors = [os.pidfd_open(pid) for pid in [inner, outer, *pids]]
            signal.pidfd_send_signal(descriptors[1], signal.SIGKILL)
            _, stderr = process.communicate(timeout=8)
            self.assertEqual(process.returncode, 125, stderr.decode())
            trace = self.trace_lines()
            self.assertIn("BOINC --set_gpu_mode never 0", trace)
            self.assertNotIn("BOINC --set_gpu_mode auto", trace)

            signal.pidfd_send_signal(descriptors[0], signal.SIGTERM)
            limit = time.monotonic() + 8
            while time.monotonic() < limit:
                try:
                    self.assert_gone(marker)
                    break
                except AssertionError:
                    time.sleep(0.1)
            self.assert_gone(marker)
        finally:
            for fd in descriptors:
                try:
                    signal.pidfd_send_signal(fd, signal.SIGKILL)
                except ProcessLookupError:
                    pass
                os.close(fd)
            if process.poll() is None:
                process.kill()
                process.communicate(timeout=8)

    @unittest.skipUnless(sys.platform == "linux", "the native supervisor requires Linux")
    def test_deadline_kills_detached_descendants(self):
        marker = self.work / "pids.json"
        until = time.time() + 1.5
        result = self.run_script(
            sys.executable, "-I", str(HERE / "run-deadline.py"),
            "--until", str(until), "--boinc-until", str(until + 60),
            "--", sys.executable, "-c", self.descendant_workload(marker, orphan=False),
        )
        self.assertEqual(result.returncode, 124, result.stderr)
        self.assert_gone(marker)

    @unittest.skipUnless(sys.platform == "linux", "the native supervisor requires Linux")
    def test_successful_parent_cannot_leave_detached_orphans(self):
        marker = self.work / "pids.json"
        result = self.run_script(
            sys.executable, "-I", str(HERE / "run-deadline.py"),
            "--until", str(time.time() + 10), "--no-boinc", "--", sys.executable, "-c",
            self.descendant_workload(marker, orphan=True),
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assert_gone(marker)

    @unittest.skipUnless(sys.platform == "linux", "the native supervisor requires Linux")
    def test_interrupted_supervisor_cleans_up_before_returning(self):
        marker = self.work / "pids.json"
        args = (
            sys.executable, "-I", str(HERE / "run-deadline.py"),
            "--until", str(time.time() + 20), "--no-boinc", "--", sys.executable, "-c",
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
