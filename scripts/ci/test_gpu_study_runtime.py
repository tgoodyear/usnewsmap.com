"""Runtime regressions for GPU-study scripts, using only synthetic inputs."""

import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import textwrap
import time
import unittest


ROOT = Path(__file__).resolve().parents[2]
STUDY = ROOT / "docs" / "reports" / "2026-11-gpu-studies" / "scripts"
WORK = ROOT / f".gpu-study-runtime-test-{os.getpid()}"


def executable(path, text):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(text, encoding="utf-8")
    path.chmod(0o755)


def digest(text):
    return hashlib.sha256(text.encode("utf-8")).hexdigest()


def json_lines(path):
    return [json.loads(line) for line in path.read_text(encoding="utf-8").splitlines()]


class GPUStudyRuntimeTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        WORK.mkdir()
        cls.addClassCleanup(shutil.rmtree, WORK)

    def setUp(self):
        self.work = WORK / self._testMethodName
        self.work.mkdir()

    def environment(self, root, bin_dir):
        env = os.environ.copy()
        env.update(
            PATH=str(bin_dir) + os.pathsep + os.environ["PATH"],
            LC_ALL="C",
            PYTHONDONTWRITEBYTECODE="1",
            TMPDIR=str(root),
            TEST_PYTHON=sys.executable,
        )
        return env

    def run_command(self, command, env):
        return subprocess.run(
            command, cwd=ROOT, env=env, text=True, capture_output=True, timeout=60, check=False
        )

    def job_fixture(self, script_name, statuses, device=True, case=None):
        root = self.work / (case or script_name)
        root.mkdir()
        work, app, venv, bin_dir, home = (
            root / "work", root / "app", root / "venv" / "bin", root / "bin", root / "home"
        )
        for directory in (work / "in", work / "out", app, venv, bin_dir, home):
            directory.mkdir(parents=True)
        input_dir, output_dir = work / "in", work / "out"
        (input_dir / "run.py").write_text("# synthetic fixture\n", encoding="utf-8")
        (input_dir / "surya_read.py").write_text("# never executed\n", encoding="utf-8")
        for name in ("list.tsv", "first.tsv", "second.tsv", "third.tsv"):
            (input_dir / name).write_text("synthetic\n", encoding="utf-8")
        (input_dir / "runs.tsv").write_text(
            "first.tsv\tline\tfailed\nsecond.tsv\tregion\tpassed\nthird.tsv\tline\tsignalled\n",
            encoding="utf-8",
        )

        python_stub = root / "python_standin.py"
        python_stub.write_text(
            r"""import json
import os
from pathlib import Path
import signal
import sys
import time

def record(message):
    with open(os.environ["TRACE"], "a", encoding="utf-8") as trace:
        trace.write(message + "\n")

args = sys.argv[1:]
if args[:2] == ["-m", "pip"]:
    record("PIP install")
    sys.exit(0)
if args[:2] == ["-m", "vllm.entrypoints.openai.api_server"]:
    def stop(signum, frame):
        record("SERVER stopped")
        sys.exit(0)
    signal.signal(signal.SIGTERM, stop)
    record("SERVER started")
    Path(os.environ["SERVER_READY"]).touch()
    deadline = time.monotonic() + 10
    while time.monotonic() < deadline:
        time.sleep(0.05)
    record("SERVER expired")
    sys.exit(1)
if args and args[0] == "-c":
    record("TORCH probe")
    print("synthetic torch")
    sys.exit(0)
if args and Path(args[0]).name == "surya_read.py":
    record("READ " + json.dumps(args[1:]))
    output = Path(args[args.index("--out") + 1])
    status = json.loads(os.environ["READER_STATUSES"])[output.stem]
    print("synthetic reader " + output.stem, flush=True)
    if status == 0:
        output.write_text("synthetic result\n", encoding="utf-8")
    if status == "SIGTERM":
        os.kill(os.getpid(), signal.SIGTERM)
    sys.exit(status)
record("UNEXPECTED " + json.dumps(args))
sys.exit(97)
""",
            encoding="utf-8",
        )
        python_launcher = '#!/bin/bash\nexec "$TEST_PYTHON" "$PYTHON_STUB" "$@"\n'
        executable(bin_dir / "python3", python_launcher)
        executable(venv / "python", python_launcher)
        executable(
            venv / "pip",
            """#!/bin/bash
printf 'PIP %s\\n' "$*" >> "$TRACE"
case "$*" in
  --version) echo 'synthetic pip';;
  freeze) echo 'surya-ocr==0.22.1';;
  '-q install --no-cache-dir '*) echo 'synthetic install';;
  *) exit 97;;
esac
""",
        )
        executable(
            app / "llama-server",
            """#!/bin/bash
printf 'DEVICE %s\\n' "$*" >> "$TRACE"
[ "$*" = '--list-devices' ] || exit 97
if [ "$STUB_DEVICE" = 1 ]; then echo CUDA0; else echo CPU; fi
""",
        )
        executable(
            bin_dir / "date",
            """#!/bin/bash
[ "$*" = '+%s' ] || exit 97
n=0
if [ -e "$DATE_COUNT" ]; then read -r n < "$DATE_COUNT"; fi
printf '%s\\n' "$((n + 1))" > "$DATE_COUNT"
printf '%s\\n' "$((1000 + n * 7))"
if [ "${FAIL_DATE_AT:-0}" = "$((n + 1))" ]; then exit 19; fi
""",
        )
        executable(
            bin_dir / "curl",
            """#!/bin/bash
[ "$*" = '-fsS -m 2 http://127.0.0.1:8000/health' ] || exit 97
for ((n = 0; n < 200; n++)); do
  [ -e "$SERVER_READY" ] && exit 0
  /bin/sleep 0.01
done
exit 1
""",
        )

        # Relocate the jobs' fixed container paths; all executable logic remains unchanged.
        source = (STUDY / "jobs" / script_name).read_text(encoding="utf-8")
        for original, replacement in (
            ("HOME=/tmp", 'HOME="$TEST_HOME"'),
            ("/opt/venv/bin", str(venv)),
            ("/app", str(app)),
            ("/work", str(work)),
        ):
            source = source.replace(original, replacement)
        script = root / script_name
        script.write_text(source, encoding="utf-8")

        env = self.environment(root, bin_dir)
        env.update(
            DATE_COUNT=str(root / "date-count"),
            FAIL_DATE_AT="",
            PYTHON_STUB=str(python_stub),
            READER_STATUSES=json.dumps(statuses),
            SERVER_READY=str(root / "server-ready"),
            STUB_DEVICE="1" if device else "0",
            TEST_HOME=str(home),
            TRACE=str(root / "trace"),
        )
        return script, input_dir, output_dir, env

    def trace(self, env):
        path = Path(env["TRACE"])
        return path.read_text(encoding="utf-8").splitlines() if path.exists() else []

    def assert_reads(self, env, expected):
        reads = [json.loads(line[5:]) for line in self.trace(env) if line.startswith("READ ")]
        self.assertEqual(reads, expected)

    def test_native_receipts_save_success_failure_and_signal_statuses(self):
        for status, expected, fail_date in (
            (0, 0, False), (23, 23, False), ("SIGTERM", 143, False), (23, 23, True)
        ):
            with self.subTest(status=status, fail_date=fail_date):
                script, inputs, outputs, env = self.job_fixture(
                    "surya-native.sh", {"surya-native": status},
                    case=f"native-{expected}-{fail_date}",
                )
                if fail_date:
                    env["FAIL_DATE_AT"] = "2"
                result = self.run_command(["bash", str(script)], env)
                events = self.trace(env)
                deadline = time.monotonic() + 11
                while (
                    "SERVER started" in events
                    and "SERVER stopped" not in events
                    and "SERVER expired" not in events
                    and time.monotonic() < deadline
                ):
                    time.sleep(0.02)
                    events = self.trace(env)
                self.assertIn("SERVER stopped", events)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(
                    (outputs / "timing.txt").read_text(encoding="utf-8"),
                    f"native seconds 7 exit {expected}\n",
                )
                self.assertEqual((outputs / "progress.txt").read_text(encoding="utf-8"), "done\n")
                self.assertEqual(
                    (outputs / "native.log").read_text(encoding="utf-8"),
                    "synthetic reader surya-native\n",
                )
                self.assertEqual((outputs / "surya-native.jsonl").exists(), status == 0)
                self.assert_reads(env, [[
                    "--list", str(inputs / "list.tsv"), "--mode", "line",
                    "--out", str(outputs / "surya-native.jsonl"),
                ]])

    def test_loop_receipts_keep_each_reader_status_and_continue(self):
        statuses = {"failed": 23, "passed": 0, "signalled": "SIGTERM"}
        for name in ("surya.sh", "surya6.sh"):
            with self.subTest(script=name):
                script, inputs, outputs, env = self.job_fixture(name, statuses)
                env["FAIL_DATE_AT"] = "2"
                result = self.run_command(["bash", str(script)], env)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(
                    (outputs / "timing.txt").read_text(encoding="utf-8"),
                    "failed seconds 7 exit 23\npassed seconds 7 exit 0\nsignalled seconds 7 exit 143\n",
                )
                self.assertEqual((outputs / "progress.txt").read_text(encoding="utf-8"), "done\n")
                for run in statuses:
                    self.assertEqual(
                        (outputs / f"{run}.log").read_text(encoding="utf-8"),
                        f"synthetic reader {run}\n",
                    )
                    self.assertEqual((outputs / f"{run}.jsonl").exists(), run == "passed")
                self.assert_reads(env, [
                    ["--list", str(inputs / "first.tsv"), "--mode", "line",
                     "--out", str(outputs / "failed.jsonl")],
                    ["--list", str(inputs / "second.tsv"), "--mode", "region",
                     "--out", str(outputs / "passed.jsonl")],
                    ["--list", str(inputs / "third.tsv"), "--mode", "line",
                     "--out", str(outputs / "signalled.jsonl")],
                ])

    def test_device_failure_still_prevents_reader_start(self):
        for name in ("surya.sh", "surya6.sh"):
            with self.subTest(script=name):
                script, _, outputs, env = self.job_fixture(name, {}, device=False)
                result = self.run_command(["bash", str(script)], env)
                self.assertEqual(result.returncode, 8, result.stderr)
                self.assertEqual(
                    (outputs / "progress.txt").read_text(encoding="utf-8"), "no CUDA device\n"
                )
                self.assertFalse((outputs / "timing.txt").exists())
                self.assert_reads(env, [])

    def sets_fixture(self, name, crop_count, doc_count):
        root = self.work / name
        root.mkdir()
        conv, dta, out, bin_dir = root / "conv", root / "dta", root / "out", root / "bin"
        bln = conv / "bln600"
        bln.mkdir(parents=True)
        dta.mkdir()
        bin_dir.mkdir()
        crops = []
        for index in range(crop_count):
            crop = {
                "crop_id": f"crop-{index:04d}",
                "transcription": f"reference {index}",
                "image": f"image-{index:04d}.png",
            }
            crops.append(crop)
            (bln / crop["image"]).write_bytes(f"bln {index}\n".encode())
        (bln / "crops.jsonl").write_text(
            "".join(json.dumps(crop) + "\n" for crop in crops), encoding="utf-8"
        )
        chosen_crops = sorted(
            (crop["crop_id"] for crop in crops),
            key=lambda item: (digest("gpu-studies-202611:bln600:" + item), item),
        )[:200]
        chosen_set = set(chosen_crops)
        bln_rows = [
            {"id": c["crop_id"], "ref": c["transcription"], "page": c["crop_id"],
             "source": "bln600", "lang": "eng", "file": c["image"]}
            for c in crops if c["crop_id"] in chosen_set
        ]

        docs = [f"doc-{index:03d}" for index in range(doc_count)]
        chosen_docs = sorted(
            docs, key=lambda item: (digest("gpu-studies-202611:dta19:" + item), item)
        )[:30]
        doc_lines = {}
        for doc in docs:
            directory = dta / doc
            directory.mkdir()
            doc_lines[doc] = [f"line-{index:03d}" for index in range(12 if doc == chosen_docs[0] else 2)]
            for line in doc_lines[doc]:
                (directory / f"{line}.gt.txt").write_text(f"reference {doc}/{line}\n", encoding="utf-8")
                (directory / f"{line}.nrm.png").write_bytes(f"image {doc}/{line}\n".encode())
        dta_rows, dta_images = [], {}
        for doc in chosen_docs:
            lines = sorted(
                doc_lines[doc],
                key=lambda item: (digest(f"gpu-studies-202611:dta19:{doc}/{item}"), item),
            )[:10]
            for line in lines:
                identifier = "dta19-" + digest(f"{doc}/{line}")[:16]
                dta_rows.append({
                    "id": identifier, "ref": f"reference {doc}/{line}\n", "page": doc,
                    "source": "dta19", "lang": "deu", "file": f"images/dta/{identifier}.png",
                })
                dta_images[f"{identifier}.png"] = f"image {doc}/{line}\n".encode()

        # Pace the real sort output so an early-closing consumer reliably breaks its pipe.
        sort_stub = root / "sort_standin.py"
        sort_stub.write_text(
            textwrap.dedent("""\
                import os
                from pathlib import Path
                import signal
                import subprocess
                import sys
                import time

                counter = Path(os.environ["SORT_COUNTER"])
                call = int(counter.read_text()) + 1 if counter.exists() else 1
                counter.write_text(str(call))
                data = sys.stdin.buffer.read()
                if str(call) == os.environ.get("FAIL_SORT_AT"):
                    print("synthetic sort failure", file=sys.stderr)
                    sys.exit(37)
                result = subprocess.run(
                    [os.environ["REAL_SORT"], *sys.argv[1:]], input=data, capture_output=True
                )
                if result.returncode:
                    sys.stderr.buffer.write(result.stderr)
                    sys.exit(result.returncode)
                signal.signal(signal.SIGPIPE, signal.SIG_DFL)
                for number, line in enumerate(result.stdout.splitlines(keepends=True), 1):
                    sys.stdout.buffer.write(line)
                    sys.stdout.buffer.flush()
                    if number in (30, 200):
                        time.sleep(0.3)
                """),
            encoding="utf-8",
        )
        executable(bin_dir / "sort", '#!/bin/bash\nexec "$TEST_PYTHON" "$SORT_STUB" "$@"\n')
        env = self.environment(root, bin_dir)
        real_sort = shutil.which("sort")
        self.assertIsNotNone(real_sort)
        env.update(
            FAIL_SORT_AT="", REAL_SORT=real_sort,
            SORT_COUNTER=str(root / "sort-count"), SORT_STUB=str(sort_stub),
        )
        return {
            "conv": conv, "dta": dta, "out": out, "env": env,
            "crops": chosen_crops, "bln_rows": bln_rows, "docs": chosen_docs,
            "dta_rows": dta_rows, "dta_images": dta_images,
        }

    def run_sets(self, fixture):
        return self.run_command([
            "bash", str(STUDY / "s5-sets.sh"), "--conv", str(fixture["conv"]),
            "--dta", str(fixture["dta"]), "--out", str(fixture["out"]),
        ], fixture["env"])

    def assert_sets_output(self, fixture, result):
        self.assertEqual(result.returncode, 0, result.stderr)
        out = fixture["out"]
        self.assertEqual((out / "bln.ids").read_text(encoding="utf-8").splitlines(), fixture["crops"])
        self.assertEqual(json_lines(out / "bln.jsonl"), fixture["bln_rows"])
        self.assertEqual((out / "dta.docs").read_text(encoding="utf-8").splitlines(), fixture["docs"])
        self.assertEqual(json_lines(out / "dta.jsonl"), fixture["dta_rows"])
        self.assertFalse((out / "lines").exists())
        bln_images = {
            path.name: path.read_bytes() for path in (out / "images" / "bln").iterdir()
        }
        self.assertEqual(bln_images, {
            row["file"]: (fixture["conv"] / "bln600" / row["file"]).read_bytes()
            for row in fixture["bln_rows"]
        })
        self.assertEqual(
            {path.name: path.read_bytes() for path in (out / "images" / "dta").iterdir()},
            fixture["dta_images"],
        )
        for name in ("bln.jsonl", "dta.jsonl"):
            self.assertIn(hashlib.sha256((out / name).read_bytes()).hexdigest(), result.stdout)

    def test_bln_selector_consumes_sort_and_keeps_first_200(self):
        fixture = self.sets_fixture("bln", 600, 3)
        self.assert_sets_output(fixture, self.run_sets(fixture))

    def test_dta_selector_consumes_sort_and_keeps_first_30(self):
        fixture = self.sets_fixture("dta", 3, 40)
        self.assert_sets_output(fixture, self.run_sets(fixture))

    def test_sort_failures_still_abort_at_each_selection(self):
        for call in (1, 2):
            with self.subTest(sort_call=call):
                fixture = self.sets_fixture(f"failure-{call}", 3, 3)
                fixture["env"]["FAIL_SORT_AT"] = str(call)
                result = self.run_sets(fixture)
                self.assertEqual(result.returncode, 37, result.stderr)
                self.assertIn("synthetic sort failure", result.stderr)
                self.assertEqual((fixture["out"] / "bln.jsonl").exists(), call == 2)
                self.assertFalse((fixture["out"] / "dta.jsonl").exists())

    def test_input_failures_still_abort_before_dta_output(self):
        for kind in ("bad-bln-json", "missing-dta"):
            with self.subTest(input=kind):
                fixture = self.sets_fixture(kind, 3, 3)
                if kind == "bad-bln-json":
                    (fixture["conv"] / "bln600" / "crops.jsonl").write_text(
                        "not json\n", encoding="utf-8"
                    )
                else:
                    shutil.rmtree(fixture["dta"])
                result = self.run_sets(fixture)
                self.assertNotEqual(result.returncode, 0)
                self.assertEqual((fixture["out"] / "bln.jsonl").exists(), kind == "missing-dta")
                self.assertFalse((fixture["out"] / "dta.jsonl").exists())


if __name__ == "__main__":
    unittest.main()
