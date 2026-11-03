"""Offline synthetic regressions for the Study 2 and Study 5 protocols."""

import json
import os
from pathlib import Path
import shlex
import shutil
import subprocess
import sys
import unittest


REPO = Path(__file__).resolve().parents[2]
REPORT = REPO / "docs/reports/2026-11-gpu-studies"
SCRIPTS = REPORT / "scripts"


def executable(path, text):
    path.write_text(text, encoding="utf-8")
    path.chmod(0o755)


class ProtocolFixture(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.workspace = Path.cwd() / f".gpu-study-protocol-{os.getpid()}-{cls.__name__}"
        cls.workspace.mkdir()

    @classmethod
    def tearDownClass(cls):
        shutil.rmtree(cls.workspace)

    def setUp(self):
        self.work = self.workspace / self._testMethodName
        self.work.mkdir()
        self.trace = self.work / "trace.jsonl"

    def events(self):
        if not self.trace.exists():
            return []
        return [json.loads(line) for line in self.trace.read_text(encoding="utf-8").splitlines()]


FAKE_SCORER = """#!/usr/bin/env python3
import argparse, json, os
from pathlib import Path

parser = argparse.ArgumentParser()
parser.add_argument("command", choices=("score", "invent"))
parser.add_argument("--refs", required=True)
parser.add_argument("--hyp", action="append", default=[])
parser.add_argument("--baseline")
parser.add_argument("--out", required=True)
parser.add_argument("--fold", action="store_true")
args = parser.parse_args()
with open(os.environ["TRACE"], "a", encoding="utf-8") as trace:
    trace.write(json.dumps(vars(args)) + "\\n")
if args.command == "score":
    rows = [{"group": "all", "reader": "baseline", "cer": 0.4},
            {"group": "all", "reader": "s250", "cer": 0.1}]
else:
    rows = [{"group": "all", "reader": "adapter", "crops": 1}]
Path(args.out).write_text("".join(json.dumps(row) + "\\n" for row in rows), encoding="utf-8")
"""


class Study2PhaseTests(ProtocolFixture):
    def setUp(self):
        super().setUp()
        self.root = self.work / "study"
        self.root.mkdir()
        scorer = self.work / "scorer"
        executable(scorer, FAKE_SCORER)
        self.env = os.environ.copy()
        self.env.update(GPU_STUDY_ROOT=str(self.root), TOOL=str(scorer), TRACE=str(self.trace),
                        PYTHONDONTWRITEBYTECODE="1")

    def analyze(self, *args, env=None):
        return subprocess.run(
            ["/bin/bash", str(SCRIPTS / "analyze-2.sh"), *args],
            cwd=self.work, env=self.env if env is None else env,
            capture_output=True, text=True, timeout=30, check=False,
        )

    def test_missing_invalid_or_ambiguous_phase_never_creates_outputs_or_scores(self):
        cases = (
            (), ("--phase",), ("--phase", ""), ("--phase", "val"),
            ("--phase", "TEST"), ("--phase", "--phase", "test"),
            ("--phase", "validation", "--phase", "test"),
            ("--phase", "test", "--phase", "test"),
            ("validation",), ("--unknown",),
        )
        for args in cases:
            with self.subTest(args=args):
                result = self.analyze(*args)
                self.assertEqual(result.returncode, 2, result.stderr)
                self.assertTrue(result.stderr.strip())
                self.assertEqual(self.events(), [])
                self.assertFalse((self.root / "analysis").exists())

        no_workspace = self.env.copy()
        no_workspace.pop("GPU_STUDY_ROOT")
        no_workspace.pop("GPU_STUDY_DEV", None)
        result = self.analyze(env=no_workspace)
        self.assertEqual(result.returncode, 2, result.stderr)
        self.assertIn("--phase", result.stderr)
        self.assertEqual(self.events(), [])

    @unittest.skipUnless(shutil.which("jq"), "Study 2 analysis requires jq")
    def test_validation_scores_only_validation_checkpoints(self):
        result = self.analyze("--phase", "validation")
        self.assertEqual(result.returncode, 0, result.stderr)
        events = self.events()
        self.assertEqual(len(events), 1)
        event = events[0]
        self.assertEqual((event["command"], event["refs"], event["out"], event["fold"]),
                         ("score", "lora/validation-refs.jsonl", "analysis/s2/validation.jsonl", False))
        self.assertEqual(event["hyp"], [
            "baseline=results/s2-1/val-baseline.jsonl",
            *[f"s{step}=results/s2-1/val-s{step}.jsonl" for step in (250, 500, 1000, 2000)],
        ])
        self.assertIn('"s250"', result.stdout)
        self.assertFalse((self.root / "analysis/s2/test.jsonl").exists())

    @unittest.skipUnless(shutil.which("jq"), "Study 2 analysis requires jq")
    def test_test_phase_scores_and_invents_only_on_the_test_split(self):
        result = self.analyze("--phase", "test")
        self.assertEqual(result.returncode, 0, result.stderr)
        events = self.events()
        self.assertEqual([(event["command"], event["fold"]) for event in events],
                         [("score", False), ("score", True), ("invent", False)])
        self.assertTrue(all(event["refs"] == "lora/test-refs.jsonl" for event in events))
        self.assertTrue(all(event["hyp"] == [
            "baseline=results/s2-2/test-baseline.jsonl",
            "adapter=results/s2-2/test-adapter.jsonl",
        ] for event in events))
        self.assertFalse((self.root / "analysis/s2/validation.jsonl").exists())


FAKE_HUB = """import json, os
def snapshot_download(repo, revision, **kwargs):
    with open(os.environ["TRACE"], "a", encoding="utf-8") as trace:
        trace.write(json.dumps({"event": "download", "repo": repo, "revision": revision,
                                "local_dir": kwargs["local_dir"]}) + "\\n")
    if os.environ.get("SYNTHETIC_DOWNLOAD_FAILURE") == "1":
        raise RuntimeError("synthetic download failure")
    return "synthetic snapshot"
"""

FAKE_WORKER = """import argparse, json, os
from pathlib import Path
parser = argparse.ArgumentParser()
parser.add_argument("--base")
parser.add_argument("--base-revision")
parser.add_argument("--guard")
parser.add_argument("--adapter")
args, _ = parser.parse_known_args()
with open(os.environ["TRACE"], "a", encoding="utf-8") as trace:
    trace.write(json.dumps({"event": Path(__file__).name, **vars(args)}) + "\\n")
"""


class Study5PinnedBaseTests(ProtocolFixture):
    def setUp(self):
        super().setUp()
        self.inp = self.work / "in"
        self.out = self.work / "out"
        modules = self.work / "modules"
        binaries = self.work / "bin"
        for path in (self.inp, self.out, modules, binaries):
            path.mkdir()
        (modules / "huggingface_hub.py").write_text(FAKE_HUB, encoding="utf-8")
        for name in ("lora_train.py", "lora_infer.py"):
            (self.inp / name).write_text(FAKE_WORKER, encoding="utf-8")
        (self.inp / "guard.txt").write_text("stop\n", encoding="utf-8")
        for name in ("python", "python3"):
            (binaries / name).symlink_to(sys.executable)
        for name in ("seq", "rm", "cat"):
            (binaries / name).symlink_to(shutil.which(name))
        executable(binaries / "curl", '#!/bin/bash\nprintf \'{"event":"head-lookup"}\\n\' >>"$TRACE"\nexit 88\n')
        source = (SCRIPTS / "jobs/lora8b-1.sh").read_text(encoding="utf-8")
        old = "O=/work/out; I=/work/in;"
        self.assertEqual(source.count(old), 1)
        replacement = f"O={shlex.quote(str(self.out))}; I={shlex.quote(str(self.inp))};"
        self.job = self.work / "job.sh"
        self.job.write_text(source.replace(old, replacement), encoding="utf-8")
        self.env = os.environ.copy()
        self.env.pop("SYNTHETIC_DOWNLOAD_FAILURE", None)
        self.env.update(PATH=str(binaries), PYTHONPATH=str(modules), TRACE=str(self.trace),
                        PYTHONDONTWRITEBYTECODE="1")

    def run_job(self, **env):
        return subprocess.run(
            ["/bin/bash", str(self.job)], cwd=self.work, env={**self.env, **env},
            capture_output=True, text=True, timeout=30, check=False,
        )

    def test_retained_revision_is_used_for_download_receipt_and_training(self):
        expected = json.loads((REPORT / "data/s5/8b-train-report.json").read_text())["base_revision"]
        self.assertRegex(expected, r"^[0-9a-f]{40}$")
        result = self.run_job()
        self.assertEqual(result.returncode, 0, result.stderr)
        events = self.events()
        self.assertEqual([event["event"] for event in events],
                         ["download", "lora_train.py", *(["lora_infer.py"] * 5)])
        self.assertEqual(events[0]["repo"], "Qwen/Qwen3-VL-8B-Instruct")
        self.assertEqual(events[0]["revision"], expected)
        self.assertEqual(events[1]["base_revision"], expected)
        self.assertEqual(events[1]["base"], events[0]["local_dir"])
        self.assertEqual((self.out / "base-revision.txt").read_text(), expected + "\n")

    def test_failed_download_clears_stale_revision_and_does_not_train(self):
        (self.out / "base-revision.txt").write_text("stale\n", encoding="utf-8")
        result = self.run_job(SYNTHETIC_DOWNLOAD_FAILURE="1")
        self.assertEqual(result.returncode, 6, result.stderr)
        self.assertFalse((self.out / "base-revision.txt").exists())
        self.assertEqual([event["event"] for event in self.events()], ["download"])


if __name__ == "__main__":
    unittest.main()
