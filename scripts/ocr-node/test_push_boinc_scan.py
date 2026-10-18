import os
import subprocess
import tempfile
import unittest
from pathlib import Path

SCRIPT = Path(__file__).with_name("push-boinc-scan.sh")


class PushKeepPage(unittest.TestCase):
    def setUp(self):
        self.tmp = Path(tempfile.mkdtemp())
        bin_dir = self.tmp / "bin"
        bin_dir.mkdir()
        # az: no blob exists yet, uploads succeed; every call is logged.
        az = bin_dir / "az"
        az.write_text('#!/usr/bin/env bash\necho "$*" >> "$STUB_LOG"\n'
                      '[ "$3" = show ] && exit 1\nexit 0\n')
        az.chmod(0o755)
        self.log = self.tmp / "az.log"
        self.root = self.tmp / "scans"
        self.env = {**os.environ, "PATH": f"{bin_dir}:{os.environ['PATH']}", "STUB_LOG": str(self.log)}

    def scan(self, page, ext):
        path = self.root / "keep" / f"{page}.{ext}"
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(b"scan")
        Path(f"{path}.json").write_text("{}")

    def run_push(self, *pages, extra=()):
        argv = [str(SCRIPT), "--account", "usnmex", "--scans-root", str(self.root)]
        for page in pages:
            argv += ["--keep-page", page]
        return subprocess.run(argv + list(extra), env=self.env, capture_output=True, text=True)

    def test_finds_the_format_and_reports_what_it_pushed(self):
        self.scan("nbu_gage/sn1/1890-05-03/1/7", "jp2")
        self.scan("nbu_gage/sn1/1890-05-03/1/8", "jpg")
        r = self.run_push("nbu_gage/sn1/1890-05-03/1/7", "nbu_gage/sn1/1890-05-03/1/8")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(
            r.stdout.splitlines(),
            ["pushed keep/nbu_gage/sn1/1890-05-03/1/7.jp2", "pushed keep/nbu_gage/sn1/1890-05-03/1/8.jpg"],
        )
        uploads = [line for line in self.log.read_text().splitlines() if "upload" in line]
        self.assertEqual(len(uploads), 4)  # each scan and its sidecar

    def test_a_missing_page_is_named_and_the_others_still_go_up(self):
        self.scan("nbu_gage/sn1/1890-05-03/1/7", "jp2")
        r = self.run_push("nbu_gage/sn1/1890-05-03/1/7", "nbu_gage/sn1/1890-05-03/1/9")
        self.assertEqual(r.returncode, 3, r.stderr)
        self.assertEqual(
            r.stdout.splitlines(),
            ["pushed keep/nbu_gage/sn1/1890-05-03/1/7.jp2", "missing nbu_gage/sn1/1890-05-03/1/9"],
        )
        self.assertEqual(len([l for l in self.log.read_text().splitlines() if "upload" in l]), 2)

    def test_a_scan_without_its_sidecar_is_missing(self):
        self.scan("nbu_gage/sn1/1890-05-03/1/7", "jp2")
        (self.root / "keep/nbu_gage/sn1/1890-05-03/1/7.jp2.json").unlink()
        r = self.run_push("nbu_gage/sn1/1890-05-03/1/7")
        self.assertEqual(r.returncode, 3)
        self.assertEqual(r.stdout.splitlines(), ["missing nbu_gage/sn1/1890-05-03/1/7"])

    def test_refuses_a_path_that_is_not_a_page(self):
        for bad in ("../x/y/1890-05-03/1/7", "nbu_gage/sn1/1890-05-03/1", "NBU/sn1/1890-05-03/1/7"):
            self.assertEqual(self.run_push(bad).returncode, 1, bad)


if __name__ == "__main__":
    unittest.main()
