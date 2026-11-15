"""The nightly archive report (#2008): python3 -m unittest -v boinc/scripts/onprem/test_archive_report.py

archive-report.sh runs against a fake archive tool that prints a canned dry-run result. The
report never applies, never pauses the controller and stops on any failed check.
"""

import json
import os
import subprocess
import tempfile
import unittest
from pathlib import Path

ONPREM = Path(__file__).resolve().parent
SCRIPT = ONPREM / "archive-report.sh"

FAKE_TOOL = r'''
import json, os, sys
from pathlib import Path
Path(os.environ["FAKE_LOG"]).open("a").write(" ".join(sys.argv[1:]) + "\n")
if os.environ.get("FAKE_FAILS"):
    sys.exit(3)
print("progress line, not JSON")
print(os.environ["FAKE_RESULT"])
'''


def result(**changes):
    value = {"event": "boinc_archive_dispatch", "apply": False, "plan": "main", "journals": 9000,
             "journal_cap": 10000, "resume": None, "pages": {"eligible": 1500},
             "batch": {"pages": 1000, "bytes": 1000},
             "after_batch": {"journals": 8000},
             "outstanding_pages": {"before": 5, "after": 5, "unchanged": True, "error": None}}
    value.update(changes)
    return json.dumps(value)


class ArchiveReport(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.root = Path(self.tmp.name)
        (self.root / "tool.py").write_text(FAKE_TOOL)
        (self.root / "archive/batches").mkdir(parents=True)
        (self.root / "state.other").mkdir()
        (self.root / "state.other/state.json").write_text("{}")

    def tearDown(self):
        self.tmp.cleanup()

    def run_report(self, fake, *extra, **env):
        environment = {**os.environ, "FAKE_LOG": str(self.root / "log"), "FAKE_RESULT": fake, **env}
        return subprocess.run(
            ["bash", str(SCRIPT), "--config", "c.json", "--state-dir", str(self.root / "state"),
             "--archive-dir", str(self.root / "archive"), "--report-dir", str(self.root / "report"),
             "--tool", str(self.root / "tool.py"), "--pages", "1000", *extra],
            env=environment, capture_output=True, text=True, timeout=60)

    def last(self, done):
        return json.loads([line for line in done.stdout.splitlines() if line.startswith("{")][-1])

    def test_ready_reports_the_batch_and_never_applies(self):
        done = self.run_report(result())
        self.assertEqual(done.returncode, 0, done.stderr)
        report = self.last(done)
        self.assertEqual((report["event"], report["ready"], report["eligible"]), ("boinc_archive_report", True, 1500))
        self.assertEqual((report["batch"]["pages"], report["journals_after"]), (1000, 8000))
        self.assertIn("--pages 1000", report["next_step"])
        self.assertEqual(json.loads((self.root / "report/latest.json").read_text()), report)
        calls = (self.root / "log").read_text().splitlines()
        self.assertEqual(len(calls), 1)
        self.assertNotIn("--apply", calls[0])
        self.assertNotIn("--confirm-controller-paused", calls[0])
        self.assertIn("--pages 1000 ", calls[0] + " ")
        self.assertIn(f"--check-state-dir {self.root / 'state.other'}", calls[0])

    def test_nothing_eligible_skips(self):
        done = self.run_report(result(pages={"eligible": 0}, batch={"pages": 0, "bytes": 0}))
        self.assertEqual(done.returncode, 0, done.stderr)
        report = self.last(done)
        self.assertEqual(report["ready"], False)
        self.assertIn("no page is drained", report["skipped"])

    def test_a_planned_batch_skips(self):
        done = self.run_report(result(resume={"batch": "0003", "pages": 10, "jobs": 10}))
        self.assertEqual(done.returncode, 0, done.stderr)
        self.assertIn("batch 0003 is planned already", self.last(done)["skipped"])

    def test_any_failed_check_refuses(self):
        cases = {
            "state": result(outstanding_pages={"unchanged": False, "error": "SQL/journal mismatch"}),
            "unchanged": result(outstanding_pages={"unchanged": False, "error": None}),
            "larger": result(batch={"pages": 1001, "bytes": 1}),
            "no counts": result(pages={}),
            "not a dry run": result(apply=True),
            "no result": "{}",
        }
        for name, fake in cases.items():
            with self.subTest(name):
                done = self.run_report(fake)
                self.assertEqual(done.returncode, 1, done.stdout)
                self.assertIn("boinc_archive_report_failure", done.stdout)
        self.assertFalse((self.root / "report/latest.json").exists())

    def test_a_failing_dry_run_refuses(self):
        done = self.run_report(result(), FAKE_FAILS="1")
        self.assertEqual(done.returncode, 1)
        self.assertIn("the dry run failed", done.stdout)

    def test_too_little_space_refuses(self):
        done = self.run_report(result(batch={"pages": 1000, "bytes": 1 << 60}))
        self.assertEqual(done.returncode, 1)
        self.assertIn("not enough free space", done.stdout)

    def test_an_unmounted_archive_and_bad_numbers_refuse(self):
        (self.root / "archive/batches").rmdir()
        self.assertEqual(self.run_report(result()).returncode, 1)
        (self.root / "archive/batches").mkdir()
        self.assertEqual(self.run_report(result(), "--pages", "0").returncode, 1)
        self.assertEqual(self.run_report(result(), "--pages", "x").returncode, 1)

    def test_the_service_runs_the_report_with_a_fixed_batch_and_the_timer_is_nightly(self):
        service = (ONPREM / "usnm-boinc-archive-report.service").read_text()
        exec_start = [line for line in service.splitlines() if line.startswith("ExecStart=")]
        self.assertEqual(len(exec_start), 1)
        self.assertIn("archive-report.sh --pages 1000", exec_start[0])
        self.assertNotIn("--apply", exec_start[0])
        timer = (ONPREM / "usnm-boinc-archive-report.timer").read_text()
        self.assertIn("OnCalendar=*-*-* 05:10:00 UTC", timer)
        self.assertIn("WantedBy=timers.target", timer)
        self.assertIn("archive-report", (ONPREM / "controller/install.sh").read_text())


if __name__ == "__main__":
    unittest.main()
