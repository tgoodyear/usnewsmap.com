"""The Fraktur pilot's dry run: paired jobs and commands for an operator, nothing run."""

import hashlib
import importlib.util
import io
import json
from contextlib import redirect_stderr, redirect_stdout
import os
from pathlib import Path
import shlex
import shutil
import subprocess
import tempfile
import unittest
from unittest.mock import patch

HERE = Path(__file__).resolve().parent
_spec = importlib.util.spec_from_file_location("fraktur_plan", HERE / "plan.py")
plan = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(plan)

IMAGE = "sha256:" + "ab" * 32
# Stands in for podman when the generated script is run: logs each call, writes the page's
# outputs into the mounted work directory, or fails for one page's roman-model job.
STUB_PODMAN = """#!/usr/bin/env python3
import os
import sys
from pathlib import Path
args = sys.argv[1:]
if "--version" in args:
    print("tesseract stub")
    sys.exit(0)
with open(os.environ["STUB_LOG"], "a") as log:
    log.write(" ".join(args[:1] + [a for a in args if a.startswith("fraktur-pilot-")]) + "\\n")
if args[0] == "rm":
    sys.exit(0)
work = Path(args[args.index("--volume") + 1].split(":")[0])
model = args[args.index("-l") + 1]
if "sn10000001" in str(work) and model == "deu":
    print("stub failure", file=sys.stderr)
    sys.exit(3)
scan = (work / args[args.index("--workdir") + 3]).read_text()
(work / "page.txt").write_text(f"{model} read {scan}")
(work / "page.xml").write_text("<alto/>")
"""


def has_bash5():
    found = shutil.which("bash")
    if not found:
        return False
    probe = subprocess.run([found, "-c", 'echo "${EPOCHREALTIME:-}"'], capture_output=True, text=True)
    return bool(probe.stdout.strip())


class PlanTests(unittest.TestCase):
    def setUp(self):
        self.root = Path(self.enterContext(tempfile.TemporaryDirectory()))
        self.scans = self.root / "scans"
        records = []
        for i, (sample, fmt) in enumerate((("ger", "jp2"), ("ger", "jpeg"), ("ger", "jp2"), ("dan", "jp2"))):
            body = f"scan {i}".encode()
            path = f"keep/b_one/sn1000000{i}/1880-01-0{i + 1}/1/1.{plan_ext(fmt)}"
            (self.scans / path).parent.mkdir(parents=True, exist_ok=True)
            (self.scans / path).write_bytes(body)
            records.append({
                "decade": 1880, "doc_id": f"sn1000000{i}_1880-01-0{i + 1}_ed-1_seq-1", "lccn": f"sn1000000{i}",
                "sample": sample, "scan_bytes": len(body), "scan_format": fmt, "scan_path": path,
                "scan_sha256": hashlib.sha256(body).hexdigest(),
            })
        self.records = records
        self.pages = self.root / "pages.jsonl"
        self.pages.write_text("".join(json.dumps(r) + "\n" for r in records))

    def run_plan(self, *extra):
        argv = [
            "--pages", str(self.pages), "--tesseract-image", IMAGE, "--scans-root", str(self.scans),
            "--work-root", str(self.root / "work"), *extra,
        ]
        out, err = io.StringIO(), io.StringIO()
        with redirect_stdout(out), redirect_stderr(err), patch.object(subprocess, "Popen") as popen:
            try:
                status = plan.main(argv)
            except SystemExit as e:
                status = e.code
        popen.assert_not_called()
        return status, out.getvalue(), err.getvalue()

    def test_pairs_each_page_with_both_models_in_the_workers_sandbox_and_runs_nothing(self):
        jobs_out, commands_out = self.root / "jobs.jsonl", self.root / "run.sh"
        status, out, _ = self.run_plan("--jobs-out", str(jobs_out), "--commands-out", str(commands_out))
        self.assertEqual(status, 0)
        self.assertIn("dry run: nothing is run or queued", out)
        self.assertIn("variant -l deu_latf: 3 jobs", out)
        self.assertIn("variant -l deu: 3 jobs", out)
        jobs = [json.loads(line) for line in jobs_out.read_text().splitlines()]
        self.assertEqual([(j["doc_id"][:10], j["variant"]) for j in jobs], [
            ("sn10000000", "deu_latf"), ("sn10000000", "deu"),
            ("sn10000001", "deu_latf"), ("sn10000001", "deu"),
            ("sn10000002", "deu_latf"), ("sn10000002", "deu"),
        ])
        self.assertEqual(jobs[2]["input"], "input.jpg")
        self.assertEqual(len({j["work"] for j in jobs}), 6)
        script = commands_out.read_text()
        commands = [line for line in script.splitlines() if line.startswith("job ")]
        self.assertEqual(len(commands), 6)
        self.assertEqual(len({shlex.split(c)[4] for c in commands}), 6)
        for flag in ("--network=none", "--read-only", "--cap-drop=all", "--pull=never", IMAGE, "--psm 3 --dpi 300 txt alto"):
            self.assertTrue(all(flag in c for c in commands), flag)
        self.assertIn("-l deu_latf", commands[0])
        self.assertFalse((self.root / "work").exists())
        self.assertEqual(subprocess.call(["bash", "-n", str(commands_out)]), 0)

    @unittest.skipUnless(has_bash5() and shutil.which("timeout"), "needs bash 5 and timeout")
    def test_the_written_commands_record_every_job_and_need_a_fresh_work_root(self):
        commands_out = self.root / "run.sh"
        self.assertEqual(self.run_plan("--commands-out", str(commands_out))[0], 0)
        stubs = self.root / "bin"
        stubs.mkdir()
        (stubs / "podman").write_text(STUB_PODMAN)
        (stubs / "podman").chmod(0o755)
        log = self.root / "podman.log"
        env = dict(os.environ, PATH=f"{stubs}{os.pathsep}{os.environ['PATH']}", STUB_LOG=str(log))
        (self.scans / self.records[0]["scan_path"]).unlink()
        run = subprocess.run(["bash", str(commands_out)], env=env, capture_output=True, text=True)
        self.assertEqual(run.returncode, 0, run.stderr)
        work = self.root / "work"
        self.assertEqual((work / "engine-version").read_text(), "tesseract stub\n")
        calls = log.read_text().splitlines()
        for i, r in enumerate(self.records[:3]):
            for model in ("deu_latf", "deu"):
                job = work / "ger" / r["doc_id"] / model
                name = f"fraktur-pilot-ger-{r['doc_id']}-{model}"
                exit_status = {0: "copy-failed", 1: "3" if model == "deu" else "0"}.get(i, "0")
                self.assertEqual((job / "exit").read_text(), exit_status + "\n", job)
                self.assertEqual((job / "page.txt").exists(), exit_status == "0", job)
                if exit_status == "0":
                    self.assertEqual((job / "page.txt").read_text(), f"{model} read scan {i}")
                elif exit_status == "3":
                    self.assertEqual((job / "stderr").read_text(), "stub failure\n")
                else:
                    self.assertTrue((job / "stderr").read_text())
                runs = [c for c in calls if c.endswith(" " + name)]
                self.assertEqual(runs, [] if i == 0 else [f"run {name}", f"rm {name}"], name)
                start, end = map(float, (job / "time").read_text().split())
                self.assertLessEqual(start, end)
                self.assertEqual([f.name for f in job.iterdir() if f.name.startswith("input")], [])
        self.assertFalse((work / "dan").exists())
        again = subprocess.run(["bash", str(commands_out)], env=env, capture_output=True, text=True)
        self.assertEqual(again.returncode, 1)
        self.assertIn("exists: write the run to a fresh work root", again.stderr)
        self.assertEqual(log.read_text().splitlines(), calls)

    def test_refuses_an_image_tag_an_unpinned_model_a_single_variant_or_a_relative_root(self):
        for extra in (
            ("--tesseract-image", "usnm-ocr/tesseract:latest"),
            ("--variants", "deu_latf,deu_fraktur"),
            ("--variants", "deu"),
            ("--work-root", "pilot-run"),
        ):
            argv = ["--pages", str(self.pages), "--scans-root", str(self.scans), "--work-root", str(self.root)]
            argv += ["--tesseract-image", IMAGE] if extra[0] != "--tesseract-image" else []
            with redirect_stderr(io.StringIO()), self.assertRaises(SystemExit) as caught:
                plan.main(argv + list(extra))
            self.assertEqual(caught.exception.code, 2, extra)

    def test_verify_scans_reports_missing_and_changed_scans(self):
        self.assertEqual(self.run_plan("--verify-scans")[0], 0)
        (self.scans / self.records[0]["scan_path"]).write_bytes(b"scan X")
        (self.scans / self.records[2]["scan_path"]).unlink()
        status, out, err = self.run_plan("--verify-scans")
        self.assertEqual(status, 1)
        self.assertIn("scans verified: 1 of 3", out)
        self.assertIn(f"{self.records[0]['doc_id']}: sha256 differs", err)
        self.assertIn(f"{self.records[2]['doc_id']}: missing", err)

    def test_other_samples_take_their_own_variants(self):
        status, out, _ = self.run_plan("--sample", "dan", "--variants", "dan,frak2021")
        self.assertEqual(status, 0)
        self.assertIn("pages 1,", out)
        self.assertIn("variant -l frak2021: 1 jobs", out)


def plan_ext(fmt):
    return {"jp2": "jp2", "jpeg": "jpg"}[fmt]


if __name__ == "__main__":
    unittest.main()
