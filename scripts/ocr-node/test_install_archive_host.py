"""Host installer tests in a private filesystem with stub account/systemd tools."""
import hashlib
import os
from pathlib import Path
import re
import subprocess
import tempfile
import unittest

HERE = Path(__file__).parent


class HostInstallTest(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        root = self.root = Path(self.tmp.name)
        self.stage = root / "stage"
        self.stage.mkdir()
        (root / "bin").mkdir()
        for directory in ("tank/ocr/scans/keep", "etc/systemd/system", "etc/tmpfiles.d",
                          "usr/local/bin", "etc/usnm"):
            (root / directory).mkdir(parents=True)
        self.sha = hashlib.sha256(b"binary").hexdigest()
        for name in ("usnm-ocr-worker", "check-scan-archive.sh", "usnm-scan-archive.conf",
                     "usnm-scan-archive.service", "usnm-scan-archive.timer",
                     "usnm-scan-archive-verify.service", "usnm-scan-archive-verify.timer"):
            (self.stage / name).write_bytes(b"binary")
        for name, body in {
            "id": 'if [ "${1:-}" = -u ]; then echo 0; fi',
            "getent": "exit 0",
            "usermod": "exit 0",
            "chown": "exit 0",
            "runuser": "exit 0",
            "systemd-tmpfiles": "exit 0",
            "systemd-analyze": "exit 0",
            "sha256sum": 'shasum -a 256 "$@"',
            "systemctl": 'if [ "$1" = is-active ]; then [ "${STUB_ACTIVE:-}" = "${3:-}" ]; else exit 0; fi',
        }.items():
            path = root / "bin" / name
            path.write_text("#!/usr/bin/env bash\nset -eu\n" + body + "\n")
            path.chmod(0o755)
        script = (HERE / "install-archive-host.sh").read_text()
        script = re.sub(r"/(?:tank|usr/local|etc|var)/",
                        lambda match: str(root) + match[0], script)
        self.script = root / "install.sh"
        self.script.write_text(script)
        self.env = dict(os.environ, PATH=str(root / "bin") + os.pathsep + os.environ["PATH"])

    def run_it(self, *args, **env):
        return subprocess.run(
            ["bash", str(self.script), "--dir", str(self.stage),
             "--release", "ocr-worker-20261016-c66d480", "--binary-sha256", self.sha,
             "--archive-url", "https://example.blob.core.windows.net/scans", *args],
            env=dict(self.env, **env), capture_output=True, text=True, check=False)

    def test_dry_run_never_installs(self):
        result = self.run_it("--dry-run")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertFalse((self.root / "usr/local/bin/usnm-ocr-worker").exists())

    def test_installs_without_starting_services(self):
        result = self.run_it()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        binary = self.root / "usr/local/bin/usnm-ocr-worker"
        self.assertEqual(binary.read_bytes(), b"binary")
        self.assertIn("no upload or timer was started", result.stdout)
        self.assertEqual(self.run_it().returncode, 0)

    def test_checksum_mismatch_fails_before_changes(self):
        (self.stage / "usnm-ocr-worker").write_bytes(b"different")
        self.assertNotEqual(self.run_it().returncode, 0)
        self.assertFalse((self.root / "usr/local/bin/usnm-ocr-worker").exists())

    def test_active_archive_or_worker_refuses_install(self):
        for unit in ("usnm-scan-archive.service", "usnm-ocr-worker.service"):
            result = self.run_it(STUB_ACTIVE=unit)
            self.assertNotEqual(result.returncode, 0, result.stdout)
        self.assertFalse((self.root / "usr/local/bin/usnm-ocr-worker").exists())

    def test_conflicting_environment_is_preserved(self):
        envfile = self.root / "etc/usnm/scan-archive.env"
        envfile.write_text("USNM_ARCHIVE_URL=https://different.blob.core.windows.net/scans\n")
        self.assertNotEqual(self.run_it().returncode, 0)
        self.assertIn("different", envfile.read_text())
        self.assertFalse((self.root / "usr/local/bin/usnm-ocr-worker").exists())

    def test_spool_options_configure_evict_schedule_and_seed_done_markers(self):
        tsv = self.root / "archived.tsv"
        tsv.write_text("sn83030214/1900-01-01/ed-1/seq-1.jp2\t123\t456\n"
                       "buffer/sn83030214/1900-01-02/ed-1/seq-1.jp2\t789\t101\n")
        result = self.run_it(
            "--set", "both", "--evict", "--on-calendar", "*:0/15",
            "--seed-done", str(tsv)
        )
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        env_text = (self.root / "etc/usnm/scan-archive.env").read_text()
        self.assertIn("USNM_ARCHIVE_SET=both\n", env_text)
        self.assertIn("USNM_ARCHIVE_EVICT=true\n", env_text)
        spool_conf = (self.root / "etc/systemd/system/usnm-scan-archive.service.d/spool.conf").read_text()
        self.assertIn("User=ocrdata", spool_conf)
        self.assertIn("/tank/ocr/scans", spool_conf)
        timer_conf = (self.root / "etc/systemd/system/usnm-scan-archive.timer.d/override.conf").read_text()
        self.assertIn("OnCalendar=*:0/15", timer_conf)
        self.assertTrue((self.root / "tank/ocr/scans/keep/sn83030214/1900-01-01/ed-1/seq-1.jp2.done").exists())
        self.assertTrue((self.root / "tank/ocr/scans/buffer/sn83030214/1900-01-02/ed-1/seq-1.jp2.done").exists())


if __name__ == "__main__":
    unittest.main()
