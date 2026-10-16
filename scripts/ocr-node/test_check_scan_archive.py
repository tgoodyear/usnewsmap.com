"""Bounded archive acceptance tests; no Azure or systemd access."""
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

SCRIPT = Path(__file__).with_name("check-scan-archive.sh")


class ArchiveAcceptanceTest(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        root = Path(self.tmp.name)
        self.root = root
        for directory in ("bin", "usr/local/bin", "etc/usnm", "etc/tmpfiles.d",
                          "run/lock/usnm-scan-archive"):
            (root / directory).mkdir(parents=True)
        (root / "etc/usnm/scan-archive.env").write_text("USNM_ARCHIVE_URL=https://example\n")
        self.stub("id", "echo 0\n")
        self.stub("getent", "exit 0\n")
        self.stub("systemd-tmpfiles", "exit 0\n")
        (root / "etc/tmpfiles.d/usnm-scan-archive.conf").touch()
        (root / "run/lock/usnm-scan-archive/lock").write_text("preserve")
        self.stub("flock", '[ -z "${STUB_LOCKED:-}" ]\n')
        self.stub("systemd-run", """
printf '%s\\n' "$*" >> "$STUB_CALLS"
case " $* " in
  *" --verify "*) phase=verify ;;
  *" --dry-run "*) phase=dry-run ;;
  *) phase=upload ;;
esac
[ "${STUB_FAIL:-}" != "$phase" ] || exit 1
cat "$STUB_REPORT"
""")
        worker = root / "usr/local/bin/usnm-ocr-worker"
        worker.touch()
        worker.chmod(0o755)
        self.calls = root / "calls"
        self.report = root / "report.json"
        self.good = dict(scanned=2, skipped=0, uploaded=0, uploaded_bytes=0, present=2,
                         would_upload=0, bad_local=0, conflicts=0, missing=0,
                         mismatched=0, errors=0, problems=[])
        self.report.write_text(json.dumps(self.good))
        self.env = dict(os.environ, PATH=str(root / "bin") + os.pathsep + os.environ["PATH"],
                        USNM_OCR_TEST_ROOT=str(root), STUB_CALLS=str(self.calls),
                        STUB_REPORT=str(self.report))

    def stub(self, name, body):
        path = self.root / "bin" / name
        path.write_text("#!/usr/bin/env bash\nset -eu\n" + body)
        path.chmod(0o755)

    def run_check(self, *args, **env):
        return subprocess.run(["bash", str(SCRIPT), *args], env=dict(self.env, **env),
                              text=True, capture_output=True, check=False)

    def test_runs_all_phases_with_the_service_identity_and_a_bound(self):
        result = self.run_check("--limit", "2")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        calls = self.calls.read_text().splitlines()
        self.assertEqual(len(calls), 3)
        self.assertIn("--dry-run", calls[0])
        self.assertNotIn("--dry-run", calls[1])
        self.assertIn("--verify", calls[2])
        for call in calls:
            self.assertIn("--uid=usnm --gid=usnm", call)
            self.assertIn("SupplementaryGroups=himds", call)
            self.assertIn("archive-scans --limit 2", call)
            self.assertNotIn("--state", call)
            self.assertIn("UnsetEnvironment=USNM_ARCHIVE_STATE", call)
            self.assertIn("Environment=RUST_LOG=off", call)
        self.assertEqual((self.root / "run/lock/usnm-scan-archive/lock").read_text(), "preserve")
        self.assertIn("2 scans verified", result.stdout)

    def test_verify_only_never_uploads(self):
        result = self.run_check("--verify-only")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        calls = self.calls.read_text().splitlines()
        self.assertEqual(len(calls), 1)
        self.assertIn("--verify", calls[0])

    def test_thousand_scan_boundary_disables_progress_logs(self):
        self.report.write_text(json.dumps(dict(self.good, scanned=1000, present=1000)))
        result = self.run_check("--limit", "1000")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(self.calls.read_text().count("Environment=RUST_LOG=off"), 3)

    def test_bad_limits_fail_before_any_command(self):
        for value in ("0", "-1", "1001", "01", "abc"):
            with self.subTest(value=value):
                result = self.run_check("--limit", value)
                self.assertNotEqual(result.returncode, 0)
                self.assertFalse(self.calls.exists())

    def test_held_lock_never_launches_a_sample(self):
        result = self.run_check(STUB_LOCKED="1")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("holds the archive lock", result.stderr)
        self.assertFalse(self.calls.exists())

    def test_command_failure_stops_before_upload_or_verify(self):
        result = self.run_check(STUB_FAIL="dry-run")
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(len(self.calls.read_text().splitlines()), 1)
        self.assertNotIn("acceptance passed", result.stdout)

    def test_empty_unclean_malformed_or_multiple_reports_fail(self):
        for report in (dict(self.good, scanned=0), dict(self.good, errors=1),
                       dict(self.good, skipped=1), {}, [], "not json"):
            with self.subTest(report=report):
                self.report.write_text(json.dumps(report))
                result = self.run_check("--verify-only")
                self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
                self.assertNotIn("acceptance passed", result.stdout)
        self.report.write_text(json.dumps(self.good) + "\n" + json.dumps(self.good))
        self.assertNotEqual(self.run_check("--verify-only").returncode, 0)

    def test_verify_requires_all_scans_present(self):
        self.report.write_text(json.dumps(dict(self.good, present=1)))
        result = self.run_check("--verify-only")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("did not confirm every", result.stderr)

    def test_other_phases_require_every_scan_accounted_for(self):
        self.report.write_text(json.dumps(dict(self.good, present=1)))
        result = self.run_check()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("dry-run did not account", result.stderr)
        self.assertEqual(len(self.calls.read_text().splitlines()), 1)


if __name__ == "__main__":
    unittest.main()
