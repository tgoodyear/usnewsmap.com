"""Offline regressions for the GPU-study jobs' pinned Git checkouts."""

import base64
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import unittest
from types import SimpleNamespace


ROOT = Path(__file__).resolve().parents[2]
JOBS = ROOT / "docs/reports/2026-11-gpu-studies/scripts/jobs"
WORK = ROOT / f".gpu-study-git-pins-test-{os.getpid()}"
PINS = {"american-stories.sh": "f307e5c", "export-gguf.sh": "091b6a7fc"}
MODEL_ARCHIVE = b"synthetic model archive; no network\n"
READING = {"bboxes": [{"raw_text": "synthetic reading"}]}


MOCK_COMMAND = r"""#!/usr/bin/env python3
import base64
import hashlib
import json
import os
from pathlib import Path
import shutil
import sys

command = Path(sys.argv[0]).name
args = sys.argv[1:]
repo = Path(os.environ["REPO_DIR"])
work = Path(os.environ["WORK_DIR"])
pin = os.environ["PIN"]
with open(os.environ["TRACE"], "a", encoding="utf-8") as trace:
    trace.write(json.dumps({"command": command, "args": args}) + "\n")

def fail(message, status=91):
    print(message, file=sys.stderr)
    sys.exit(status)

def require_pin():
    state = repo / "fixture-head"
    if not state.exists() or state.read_text(encoding="utf-8") != pin:
        fail("mock command reached without the pinned checkout")

def option(name):
    return args[args.index(name) + 1]

if command == "apt-get":
    sys.exit(0)

if command == "git":
    if args[:2] == ["clone", "-q"]:
        if Path(args[-1]) != repo:
            fail("unexpected clone destination")
        # A failed clone can leave a partial or pre-existing default checkout.
        repo.mkdir(parents=True, exist_ok=True)
        (repo / "fixture-head").write_text("default", encoding="utf-8")
        (repo / "requirements.txt").write_text("synthetic\n", encoding="utf-8")
        if os.environ["FAIL_STAGE"] == "clone":
            fail("synthetic git clone failure", 71)
    elif args[:2] == ["-C", str(repo)] and args[2:] == ["rev-parse", "HEAD"]:
        print("d" * 40)
    elif args[:2] == ["-C", str(repo)] and args[2:] == ["checkout", "-q", pin]:
        if os.environ["FAIL_STAGE"] == "checkout":
            fail("synthetic git checkout failure", 72)
        (repo / "fixture-head").write_text(pin, encoding="utf-8")
    elif args[:2] == ["-C", str(repo)] and args[2:] == ["log", "-1", "--format=%H %s"]:
        require_pin()
        print(pin + "0" * (40 - len(pin)) + " synthetic pinned commit")
    else:
        fail("unexpected git command: " + repr(args))
    sys.exit(0)

require_pin()

if command == "curl":
    Path(option("-o")).write_bytes(Path(os.environ["MODEL_REPLY"]).read_bytes())
elif command == "sha256sum":
    if args == ["-c", "--quiet", "-"]:
        expected, filename = sys.stdin.readline().rstrip("\n").split("  ", 1)
        actual = hashlib.sha256(Path(filename).read_bytes()).hexdigest()
        sys.exit(0 if expected == actual else 1)
    for filename in args:
        digest = hashlib.sha256(Path(filename).read_bytes()).hexdigest()
        print(digest + "  " + filename)
elif command == "unzip":
    destination = Path(option("-d"))
    destination.mkdir(parents=True, exist_ok=True)
    (destination / "synthetic-model").write_text("fixture\n", encoding="utf-8")
    if os.environ.get("REMOVE_CHECKOUT_ON_UNZIP") == "1":
        shutil.rmtree(repo)
elif command == "python":
    if args[:2] == ["-m", "pip"]:
        if "-r" in args and (Path.cwd() != repo or option("-r") != "requirements.txt"):
            fail("requirements installed outside the pinned checkout")
        print("synthetic install")
    elif args and args[0] == "-c":
        if args[-2:] != [os.environ["BASE_REPO"], os.environ["BASE_REV"]]:
            fail("unexpected snapshot arguments")
        (work / "base").mkdir()
    elif args and Path(args[0]).name == "merge_lora.py":
        if Path(option("--base")) != Path(os.environ["EXPECTED_BASE"]):
            fail("unexpected merge base")
        if Path(option("--adapter")) != work / "in" / "fixture-adapter":
            fail("unexpected adapter")
        destination = Path(option("--out"))
        destination.mkdir()
        (destination / "synthetic-merged").write_text("fixture\n", encoding="utf-8")
    elif args and Path(args[0]) == repo / "convert_hf_to_gguf.py":
        if not (work / "merged" / "synthetic-merged").exists():
            fail("conversion before merge")
        Path(option("--outfile")).write_bytes(b"synthetic f16 gguf")
    else:
        fail("unexpected python command: " + repr(args))
elif command == "cmake":
    if args[:2] == ["-S", str(repo)]:
        Path(option("-B")).mkdir(parents=True, exist_ok=True)
    elif args and args[0] == "--build" and args[2:4] == ["--target", "llama-quantize"]:
        destination = Path(args[1]) / "bin"
        destination.mkdir(parents=True, exist_ok=True)
        (destination / "llama-quantize").symlink_to(os.environ["MOCK_COMMAND"])
    else:
        fail("unexpected cmake command: " + repr(args))
elif command == "nproc":
    print("2")
elif command == "llama-quantize":
    if len(args) != 3 or Path(args[0]).read_bytes() != b"synthetic f16 gguf":
        fail("unexpected quantization input")
    Path(args[1]).write_bytes(b"synthetic quantized gguf")
elif command == "timeout":
    if args[:3] != ["300", "python", "AmericanStories/src/run_img2txt_yolo_pipeline.py"]:
        fail("unexpected reader command")
    if not (Path(option("--manifest_path")) / "page.jpg").exists():
        fail("missing synthetic input")
    (Path(option("--output_save_path")) / "page.jpg").write_text(
        json.dumps({"bboxes": [{"raw_text": "synthetic reading"}]}), encoding="utf-8"
    )
elif command == "base64":
    if len(args) != 2 or args[0] != "-w0":
        fail("unexpected base64 command")
    sys.stdout.buffer.write(base64.b64encode(Path(args[1]).read_bytes()))
else:
    fail("unexpected mocked command: " + command)
"""


class GPUStudyGitPinsTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        WORK.mkdir()
        cls.addClassCleanup(shutil.rmtree, WORK)

    def setUp(self):
        self.root = WORK / self._testMethodName
        self.root.mkdir()

    def fixture(self, job, scenario, fail_stage="", remote_base=False, remove_checkout=False):
        # Include /work in the fixture path even when the checkout is not on GitHub Actions.
        case = self.root / job.removesuffix(".sh") / "runner/work" / scenario
        work, bin_dir = case / "container", case / "bin"
        inp, out = work / "in", work / "out"
        for directory in (inp, out, bin_dir, case / "local-base"):
            directory.mkdir(parents=True)
        repo = work / ("as/AmericanStories" if job == "american-stories.sh" else "llama")
        (inp / "images").mkdir()
        (inp / "images/page.jpg").write_bytes(b"synthetic image, never read by OCR\n")
        (inp / "list.tsv").write_text(
            "".join(f"page-{n}\timages/page.jpg\n" for n in range(1, 11)), encoding="utf-8"
        )
        (inp / "merge_lora.py").write_text("# synthetic, never executed\n", encoding="utf-8")
        (inp / "fixture-adapter").write_bytes(b"synthetic adapter")
        base_repo, base_rev = (
            ("Qwen/Qwen3-VL-8B-Instruct", "0c351dd01ed87e9c1b53cbc748cba10e6187ff3b")
            if remote_base else ("", "")
        )
        (inp / "export.env").write_text(
            "NAME=fixture\nADAPTER=fixture-adapter\nQUANT=Q4_K_M\n"
            f"BASEREPO={base_repo}\nBASEREV={base_rev}\n",
            encoding="utf-8",
        )
        model_reply = case / "model-reply"
        model_reply.write_bytes(MODEL_ARCHIVE)

        mock = case / "mock-command.py"
        mock.write_text(MOCK_COMMAND, encoding="utf-8")
        mock.chmod(0o755)
        for command in (
            "apt-get", "git", "curl", "sha256sum", "unzip", "python",
            "cmake", "nproc", "timeout", "base64",
        ):
            (bin_dir / command).symlink_to(mock)
        (bin_dir / "python3").symlink_to(sys.executable)
        for command in ("mkdir", "wc", "head", "tail", "ls", "tee", "cp", "rm", "date"):
            executable = shutil.which(command)
            self.assertIsNotNone(executable, command)
            (bin_dir / command).symlink_to(executable)

        source = (JOBS / job).read_text(encoding="utf-8")
        if job == "american-stories.sh":
            source, count = re.subn(
                r"(?m)^MODELS_SHA256=[0-9a-f]{64}$",
                "MODELS_SHA256=" + hashlib.sha256(MODEL_ARCHIVE).hexdigest(),
                source,
            )
            self.assertEqual(count, 1)
            self.assertEqual(source.count("d=$(mktemp -d)"), 1)
        replacements = {
            "/work": str(work),
            "/opt/base": str(case / "local-base"),
            "d=$(mktemp -d)": 'd="$PAGE_DIR"; mkdir -p "$d"',
        }
        source = re.sub(
            "|".join(map(re.escape, replacements)),
            lambda match: replacements[match.group()],
            source,
        )
        script = case / job
        script.write_text(source, encoding="utf-8")
        env = {
            "PATH": str(bin_dir),
            "HOME": str(case),
            "LC_ALL": "C",
            "PYTHONDONTWRITEBYTECODE": "1",
            "TMPDIR": str(case),
            "BASE_REPO": base_repo,
            "BASE_REV": base_rev,
            "EXPECTED_BASE": str(work / "base" if remote_base else case / "local-base"),
            "FAIL_STAGE": fail_stage,
            "MOCK_COMMAND": str(mock),
            "MODEL_REPLY": str(model_reply),
            "PAGE_DIR": str(case / "page"),
            "PIN": PINS[job],
            "REMOVE_CHECKOUT_ON_UNZIP": "1" if remove_checkout else "0",
            "REPO_DIR": str(repo),
            "TRACE": str(case / "trace.jsonl"),
            "WORK_DIR": str(work),
        }
        return SimpleNamespace(job=job, script=script, work=work, out=out, repo=repo, env=env)

    def run_job(self, fixture):
        return subprocess.run(
            ["/bin/bash", str(fixture.script)],
            cwd=fixture.script.parent,
            env=fixture.env,
            text=True,
            capture_output=True,
            timeout=30,
            check=False,
        )

    def events(self, fixture):
        trace = Path(fixture.env["TRACE"])
        return [json.loads(line) for line in trace.read_text(encoding="utf-8").splitlines()]

    def assert_no_results(self, fixture):
        for name in ("as.b64", "llama-commit.txt", "progress.txt", "fixture-Q4_K_M.gguf"):
            self.assertFalse((fixture.out / name).exists(), name)
        self.assertFalse((fixture.work / "as/models.zip").exists())

    def assert_diagnostic(self, fixture, result, text, git_error):
        log = fixture.out / ("setup.log" if fixture.job == "american-stories.sh" else "export.log")
        contents = log.read_text(encoding="utf-8")
        self.assertIn(git_error, contents)
        self.assertIn(text, contents)
        self.assertIn(text, result.stderr)

    def test_failed_clone_stops_even_with_a_default_checkout_left_behind(self):
        for job in PINS:
            with self.subTest(job=job):
                fixture = self.fixture(job, "clone-failure", fail_stage="clone")
                result = self.run_job(fixture)
                self.assertEqual(result.returncode, 6, result.stderr)
                events = self.events(fixture)
                self.assertEqual([event["command"] for event in events], ["apt-get", "apt-get", "git"])
                self.assertEqual(events[-1]["args"][:2], ["clone", "-q"])
                self.assertEqual(events[-1]["args"][-1], str(fixture.repo))
                self.assertEqual((fixture.repo / "fixture-head").read_text(), "default")
                label = "American Stories" if job == "american-stories.sh" else "llama.cpp"
                self.assert_diagnostic(fixture, result, f"{label}: git clone failed", "synthetic git clone failure")
                self.assert_no_results(fixture)

    def test_failed_checkout_never_uses_the_default_branch(self):
        for job in PINS:
            with self.subTest(job=job):
                fixture = self.fixture(job, "checkout-failure", fail_stage="checkout")
                result = self.run_job(fixture)
                self.assertEqual(result.returncode, 6, result.stderr)
                events = self.events(fixture)
                expected = ["apt-get", "apt-get", "git"]
                if job == "american-stories.sh":
                    expected.append("git")
                expected.append("git")
                self.assertEqual([event["command"] for event in events], expected)
                self.assertEqual(
                    events[-1]["args"], ["-C", str(fixture.repo), "checkout", "-q", PINS[job]]
                )
                self.assertEqual((fixture.repo / "fixture-head").read_text(), "default")
                label = "American Stories" if job == "american-stories.sh" else "llama.cpp"
                self.assert_diagnostic(
                    fixture, result, f"{label}: git checkout of pinned commit {PINS[job]} failed",
                    "synthetic git checkout failure",
                )
                self.assert_no_results(fixture)

    def test_missing_american_stories_checkout_cannot_install_from_another_directory(self):
        fixture = self.fixture("american-stories.sh", "missing-checkout", remove_checkout=True)
        result = self.run_job(fixture)
        self.assertEqual(result.returncode, 6, result.stderr)
        commands = [event["command"] for event in self.events(fixture)]
        self.assertIn("unzip", commands)
        self.assertNotIn("python", commands)
        self.assertNotIn("timeout", commands)
        self.assertIn("American Stories: cannot enter pinned checkout", result.stderr)
        self.assertIn(
            "American Stories: cannot enter pinned checkout",
            (fixture.out / "setup.log").read_text(encoding="utf-8"),
        )
        self.assertFalse((fixture.out / "as.b64").exists())

    def test_pinned_american_stories_path_reaches_only_synthetic_readers(self):
        fixture = self.fixture("american-stories.sh", "success")
        self.assertIn("/runner/work/", str(fixture.script))
        result = self.run_job(fixture)
        self.assertEqual(result.returncode, 0, result.stderr)
        events = self.events(fixture)
        commands = [event["command"] for event in events]
        self.assertEqual(commands.count("curl"), 1)
        self.assertEqual(commands.count("unzip"), 1)
        self.assertEqual(commands.count("python"), 2)
        self.assertEqual(commands.count("timeout"), 10)
        git_events = [event["args"] for event in events if event["command"] == "git"]
        self.assertEqual(
            git_events[-1], ["-C", str(fixture.repo), "checkout", "-q", PINS[fixture.job]]
        )
        self.assertEqual((fixture.repo / "fixture-head").read_text(), PINS[fixture.job])
        rows = (fixture.out / "as.b64").read_text(encoding="utf-8").splitlines()
        self.assertEqual(len(rows), 10)
        for n, row in enumerate(rows, 1):
            item, seconds, encoded = row.split("\t")
            self.assertEqual(item, f"page-{n}")
            self.assertGreaterEqual(int(seconds), 0)
            self.assertEqual(json.loads(base64.b64decode(encoded)), READING)
        self.assertRegex((fixture.out / "progress.txt").read_text(), r"^10/\s*10\n$")

    def test_pinned_export_reaches_only_synthetic_build_and_conversion(self):
        for remote_base in (False, True):
            with self.subTest(remote_base=remote_base):
                fixture = self.fixture("export-gguf.sh", str(remote_base), remote_base=remote_base)
                self.assertIn("/runner/work/", str(fixture.script))
                result = self.run_job(fixture)
                self.assertEqual(result.returncode, 0, result.stderr)
                events = self.events(fixture)
                commands = [event["command"] for event in events]
                self.assertEqual(commands.count("cmake"), 2)
                self.assertEqual(commands.count("llama-quantize"), 1)
                python = [event["args"] for event in events if event["command"] == "python"]
                self.assertEqual(len(python), 4 if remote_base else 3)
                self.assertEqual(sum(args[0] == "-c" for args in python), int(remote_base))
                git_events = [event["args"] for event in events if event["command"] == "git"]
                self.assertEqual(
                    git_events[1], ["-C", str(fixture.repo), "checkout", "-q", PINS[fixture.job]]
                )
                self.assertEqual((fixture.repo / "fixture-head").read_text(), PINS[fixture.job])
                self.assertTrue(
                    (fixture.out / "llama-commit.txt").read_text().startswith(PINS[fixture.job])
                )
                output = fixture.out / "fixture-Q4_K_M.gguf"
                self.assertEqual(output.read_bytes(), b"synthetic quantized gguf")
                digest = hashlib.sha256(output.read_bytes()).hexdigest()
                self.assertEqual((fixture.out / "fixture.sha256").read_text().split()[0], digest)
                self.assertEqual((fixture.out / "progress.txt").read_text(), "done\n")


if __name__ == "__main__":
    unittest.main()
