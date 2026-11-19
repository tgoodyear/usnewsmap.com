"""Tests for fetch_handoff.py: the cross-build check's copy of recent native results, from the
hand-off root on the BOINC host (#1367) or, before the move, from the Azure container.
python3 -m unittest boinc/scripts/onprem/crossbuild-check/test_fetch_handoff.py"""

import datetime as dt
import importlib.util
import json
import os
import subprocess
import sys
import tempfile
import time
import unittest
from pathlib import Path
from unittest import mock

HERE = Path(__file__).resolve().parent
SCRIPTS = HERE.parents[1]
sys.path.insert(0, str(SCRIPTS))


def load(path, name):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    sys.modules.setdefault(name, module)
    spec.loader.exec_module(module)
    return module


F = load(HERE / "fetch_handoff.py", "fetch_handoff")
HELPER = load(SCRIPTS / "test_handoff_ssh.py", "test_handoff_ssh")
SSH = sys.modules["handoff_ssh"]
IMAGE = "sha256:" + "e" * 64
NOW = dt.datetime.now(dt.timezone.utc)


def job(n):
    return f"{n:064x}"


class FetchHandoff(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        base = Path(self.tmp.name).resolve()
        self.root = base / "root"
        self.root.mkdir(mode=0o700)
        self.host = HELPER.Host(self.root)
        self.dest = base / "out" / "handoff"
        self.check = base / "check.py"
        self.check.write_text(f"def selected_images(trust, image):\n    return {{{IMAGE!r}}}\n")
        self.config = base / "boinc-priority-controller.json"
        self.config.write_text(json.dumps({"env": "prod", "plan": "native-feed-4",
                                           "step_command": HELPER.COMMAND}))
        self.argv = ["--dest", str(self.dest), "--check-script", str(self.check), "--trust-file", "t.json",
                     "--owner", "", "--config", str(self.config), "--scripts-dir", str(SCRIPTS)]

    def result(self, n, *, hours_ago=1.0, image=IMAGE, host="13", age_days=None):
        completed = NOW - dt.timedelta(hours=hours_ago)
        spool = self.root / "spool" / job(n)
        spool.mkdir(parents=True, exist_ok=True)
        (spool / f"usnm_{job(n)}__file_result.json").write_text(json.dumps(
            {"completed_at": completed.isoformat(), "engine": {"image": image}}))
        (spool / f"usnm_{job(n)}__file_text.txt").write_text(f"text {n}")
        (spool / "host_id").write_text(host + "\n")
        request = self.root / "dispatch/jobs" / job(n)
        request.mkdir(parents=True, exist_ok=True)
        (request / "request.json").write_text(f'{{"job": {n}}}')
        (request / "record.json").write_text("{}")
        if age_days is not None:
            old = time.time() - age_days * 86400
            for path in spool.iterdir():
                os.utime(path, (old, old))

    def fetch(self, *extra):
        with mock.patch.object(F, "make_box", lambda args: (SSH, json.loads(args.config.read_text()))), \
                mock.patch.object(SSH, "SshHandoff", lambda config, **kw: SSH_CLASS(config, runner=self.host)):
            return F.main([*self.argv, *extra])

    def test_copies_the_selected_jobs_spool_and_request_from_the_host(self):
        self.result(1, hours_ago=2)
        self.result(2, hours_ago=1)
        self.result(3, hours_ago=3, image="sha256:" + "f" * 64)  # another build
        self.result(4, hours_ago=5 * 24, age_days=5)  # outside the window
        self.assertEqual(self.fetch("--handoff", "onprem", "--days", "2"), None)
        got = sorted(str(p.relative_to(self.dest)) for p in self.dest.rglob("*") if p.is_file())
        self.assertEqual(got, sorted(
            [f"spool/{job(n)}/{name}" for n in (1, 2) for name in (f"usnm_{job(n)}__file_result.json",
                                                                 f"usnm_{job(n)}__file_text.txt", "host_id")]
            + [f"dispatch/jobs/{job(n)}/request.json" for n in (1, 2)]))
        self.assertEqual((self.dest / f"spool/{job(2)}/usnm_{job(2)}__file_text.txt").read_text(), "text 2")
        # Only reads: the steps asked of the host are the listings and reads.
        self.assertEqual({call[0] for call in self.host.calls}, {"archive-list", "archive-read"})

    def test_newest_first_up_to_max_jobs_and_only_the_named_hosts(self):
        self.result(1, hours_ago=5, host="13")
        self.result(2, hours_ago=4, host="9")
        self.result(3, hours_ago=3, host="13")
        self.result(4, hours_ago=2, host="13")
        self.fetch("--handoff", "onprem", "--host", "13", "--max-jobs", "2")
        self.assertEqual(sorted(p.name for p in (self.dest / "spool").iterdir()), [job(3), job(4)])
        listed = [call[1] for call in self.host.calls if call[0] == "archive-list"]
        self.assertTrue(any(v.get("ARCHIVE_SUFFIX") == "host_id" for v in listed))

    def test_the_listing_asks_for_files_changed_within_the_window_with_a_days_slack(self):
        self.result(1)
        self.fetch("--handoff", "onprem", "--days", "2")
        first = [call[1] for call in self.host.calls if call[0] == "archive-list"][0]
        self.assertEqual((first["ARCHIVE_PREFIX"], first["ARCHIVE_SUFFIX"], first["ARCHIVE_MAX_AGE_SECS"]),
                         ("spool/", "__file_result.json", str(3 * 86400)))

    def test_a_failed_read_fails_the_step_and_leaves_the_old_copy(self):
        self.result(1)
        self.dest.mkdir(parents=True)
        (self.dest / "old").write_text("kept")
        self.host.fail = lambda step, variables: subprocess.CompletedProcess(
            [], 1, b"", b"error: nope\n") if step == "archive-read" else None
        with self.assertRaises(SSH.HandoffRefused):
            self.fetch("--handoff", "onprem")
        self.assertEqual((self.dest / "old").read_text(), "kept")

    def test_auto_follows_the_plans_flags_and_azure_stays_available(self):
        self.assertIsInstance(F.make_source(self.args()), F.AzureSource)
        self.config.with_suffix(".flags").write_text("--local-lock\n--ssh-reads\n")
        self.assertIsInstance(F.make_source(self.args()), F.HostSource)
        self.assertIsInstance(F.make_source(self.args("azure")), F.AzureSource)

    def args(self, mode="auto"):
        import argparse
        return argparse.Namespace(handoff=mode, config=self.config, scripts_dir=SCRIPTS)

    def test_the_azure_source_still_downloads_with_az(self):
        calls = []
        with mock.patch.object(F, "az_download", lambda pattern, dest: calls.append(pattern)):
            source = F.AzureSource()
            source.stage_results("stage", 2, True)
            source.copy_job("new", job(1))
        self.assertEqual(calls, ["spool/*/*__file_result.json", "spool/*/host_id", f"spool/{job(1)}/*",
                                 f"dispatch/jobs/{job(1)}/request.json"])


SSH_CLASS = SSH.SshHandoff

if __name__ == "__main__":
    unittest.main()
