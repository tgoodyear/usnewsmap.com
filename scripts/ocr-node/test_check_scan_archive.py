"""Bounded archive acceptance tests; no Azure or systemd access."""
import json
import os
from pathlib import Path
import re
import subprocess
import tempfile
import unittest

SCRIPT = Path(__file__).with_name("check-scan-archive.sh")
EDITION = "keep/b1/sn1/1900-01-01/1"


class ArchiveAcceptanceTest(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        root = Path(self.tmp.name)
        self.root = root
        for directory in ("bin", "usr/local/bin", "etc/usnm", "etc/tmpfiles.d",
                          "run/lock/usnm-scan-archive", "tank/ocr/scans/keep", "proc"):
            (root / directory).mkdir(parents=True)
        self.scans = root / "tank/ocr/scans"
        (root / "etc/usnm/scan-archive.env").write_text("USNM_ARCHIVE_URL=https://example\n")
        (root / "proc/loadavg").write_text("0.50 0.40 0.30 1/100 1\n")
        self.stub("id", "echo 0\n")
        self.stub("getent", "exit 0\n")
        self.stub("systemd-tmpfiles", "exit 0\n")
        self.stub("ionice", 'printf "ionice %s\\n" "$*" >> "$STUB_PRIO"\n')
        self.stub("renice", 'printf "renice %s\\n" "$*" >> "$STUB_PRIO"\n')
        (root / "etc/tmpfiles.d/usnm-scan-archive.conf").touch()
        (root / "run/lock/usnm-scan-archive/lock").write_text("preserve")
        self.stub("flock", '[ -z "${STUB_LOCKED:-}" ]\n')
        self.stub("systemctl", """
case "$1" in
  show) echo "${STUB_PID:-0}" ;;
  stop) printf '%s\\n' "$*" >> "$STUB_STOPS" ;;
  *) exit 1 ;;
esac
""")
        self.stub("systemd-run", """
printf '%s\\n' "$*" >> "$STUB_CALLS"
case " $* " in
  *" --verify "*) phase=verify ;;
  *" --dry-run "*) phase=dry-run ;;
  *) phase=upload ;;
esac
[ -z "${STUB_SLEEP:-}" ] || sleep "$STUB_SLEEP"
[ "${STUB_FAIL:-}" != "$phase" ] || exit 1
cat "$STUB_REPORT"
""")
        worker = root / "usr/local/bin/usnm-ocr-worker"
        worker.touch()
        worker.chmod(0o755)
        self.calls = root / "calls"
        self.stops = root / "stops"
        self.prio = root / "prio"
        self.report = root / "report.json"
        self.good = dict(scanned=2, skipped=0, uploaded=0, uploaded_bytes=0, present=2,
                         would_upload=0, bad_local=0, conflicts=0, missing=0,
                         mismatched=0, errors=0, problems=[])
        self.report.write_text(json.dumps(self.good))
        for name in (f"{EDITION}/1.jp2", f"{EDITION}/2.jp2"):
            self.scan(name)
        # Neither is a scan: a scaled sample and a sidecar on its own.
        self.scan(f"{EDITION}/1.pct50.jpg")
        (self.scans / EDITION / "3.jp2.json").write_text("{}")
        self.env = dict(os.environ, PATH=str(root / "bin") + os.pathsep + os.environ["PATH"],
                        USNM_OCR_TEST_ROOT=str(root), STUB_CALLS=str(self.calls),
                        STUB_STOPS=str(self.stops), STUB_PRIO=str(self.prio),
                        STUB_REPORT=str(self.report))

    def stub(self, name, body):
        path = self.root / "bin" / name
        path.write_text("#!/usr/bin/env bash\nset -eu\n" + body)
        path.chmod(0o755)

    def scan(self, name, sidecar=True):
        path = self.scans / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(b"x" * 1000)
        if sidecar:
            Path(f"{path}.json").write_text("{}")

    def manifest(self, names):
        path = self.root / "archived.tsv"
        path.write_text("".join(f"{n}\t1000\t1\t2\t3\n" for n in names))
        return path

    def run_check(self, *args, **env):
        return subprocess.run(["bash", str(SCRIPT), *args], env=dict(self.env, **env),
                              text=True, capture_output=True, check=False)

    def bound(self, call):
        """The scans and sidecars one unit can see, relative to its sample root."""
        root = re.search(r"--scans (\S+)", call).group(1)
        found = []
        for pair in re.findall(r"BindReadOnlyPaths=(\S+:\S+) (\S+:\S+)", call):
            for source, target in (bind.split(":") for bind in pair):
                self.assertEqual(source, str(self.scans / target[len(root) + 1:]))
                found.append(target[len(root) + 1:])
        return sorted(found)

    def test_runs_all_phases_on_one_sample_at_idle_priority_with_a_deadline(self):
        result = self.run_check("--limit", "2")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        calls = self.calls.read_text().splitlines()
        self.assertEqual(len(calls), 3)
        self.assertIn("--dry-run", calls[0])
        self.assertNotIn("--dry-run", calls[1])
        self.assertIn("--verify", calls[2])
        want = [f"{EDITION}/1.jp2", f"{EDITION}/1.jp2.json",
                f"{EDITION}/2.jp2", f"{EDITION}/2.jp2.json"]
        for call in calls:
            self.assertIn("--uid=usnm --gid=usnm", call)
            self.assertIn("SupplementaryGroups=himds", call)
            self.assertIn("--limit 2 --concurrency 1", call)
            self.assertNotIn("--state", call)
            self.assertIn("UnsetEnvironment=USNM_ARCHIVE_STATE", call)
            self.assertIn("Environment=RUST_LOG=off", call)
            for prop in ("IOSchedulingClass=idle", "Nice=19", "RuntimeMaxSec=1800",
                         "ProtectSystem=strict", "NoNewPrivileges=yes"):
                self.assertIn(f"-p {prop} ", call)
            sample = re.search(r"--scans (\S+)", call).group(1)
            self.assertIn(f"-p TemporaryFileSystem={sample}:ro ", call)
            # The worker lists only the sample, never the real scans root.
            self.assertNotEqual(sample, str(self.scans))
            self.assertEqual(self.bound(call), want)
        self.assertRegex(self.prio.read_text(), r"^ionice -c 3 -p (\d+)\nrenice -n 19 -p \1\n$")
        self.assertEqual((self.root / "run/lock/usnm-scan-archive/lock").read_text(), "preserve")
        self.assertEqual(sorted(p.name for p in (self.root / "run").iterdir()), ["lock"])
        self.assertIn("2 scans verified", result.stdout)

    def test_verify_only_never_uploads(self):
        result = self.run_check("--verify-only", "--deadline", "600")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        calls = self.calls.read_text().splitlines()
        self.assertEqual(len(calls), 1)
        self.assertIn("--verify", calls[0])
        self.assertIn("-p RuntimeMaxSec=600 ", calls[0])

    def test_without_a_manifest_the_sample_stops_at_the_limit(self):
        for batch in ("b2", "b3"):
            for seq in range(1, 6):
                self.scan(f"keep/{batch}/sn2/1901-01-01/1/{seq}.jp2")
        self.report.write_text(json.dumps(dict(self.good, scanned=4, present=4)))
        result = self.run_check("--limit", "4", "--verify-only")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        bound = self.bound(self.calls.read_text())
        self.assertEqual(len(bound), 8)
        self.assertTrue(all(re.fullmatch(r"keep/b\d/sn\d/[\d-]+/1/\d\.jp2(\.json)?", b)
                            for b in bound), bound)

    def test_manifest_sample_is_spread_over_the_whole_list(self):
        names = [f"keep/b{i}/sn1/1900-01-01/1/1.jp2" for i in range(10)]
        for name in names:
            self.scan(name)
        self.report.write_text(json.dumps(dict(self.good, scanned=3, present=3)))
        result = self.run_check("--limit", "3", "--verify-only",
                                "--sample-from", str(self.manifest(names)))
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        scans = [b for b in self.bound(self.calls.read_text()) if b.endswith(".jp2")]
        self.assertEqual(scans, [names[0], names[3], names[6]])
        self.assertIn("3 scans (0.0 MB), spread over the 10 lines", result.stdout)

    def test_a_short_manifest_is_sampled_whole(self):
        names = [f"{EDITION}/1.jp2", f"{EDITION}/2.jp2"]
        result = self.run_check("--verify-only", "--sample-from", str(self.manifest(names)))
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("--limit 2 ", self.calls.read_text())

    def test_unsafe_or_missing_sampled_files_fail_before_any_unit(self):
        self.scan("keep/b/s/d/1/nosidecar.jp2", sidecar=False)
        for name in ("../etc/passwd.jp2", "keep/../x.jp2", "keep/./x/1.jp2",
                     "keep/a b/1.jp2", "keep/x/1.jp2:/etc/x", "keep/%n/1.jp2",
                     "buffer/b/s/d/1/1.jp2", f"{EDITION}/1.pct50.jpg",
                     f"{EDITION}/3.jp2.json", "keep/.hidden/1.jp2", "",
                     f"{EDITION}/9.jp2", "keep/b/s/d/1/nosidecar.jp2"):
            with self.subTest(name=name):
                result = self.run_check("--verify-only", "--sample-from",
                                        str(self.manifest([name])))
                self.assertNotEqual(result.returncode, 0, result.stdout)
                self.assertRegex(result.stderr, "not a keep-set scan name|missing here")
                self.assertFalse(self.calls.exists())

    def test_progress_reports_reads_and_load(self):
        (self.root / "proc/4242").mkdir()
        (self.root / "proc/4242/io").write_text("rchar: 1500000\nwchar: 0\n")
        result = self.run_check("--verify-only", "--progress", "1",
                                STUB_SLEEP="3", STUB_PID="4242")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertRegex(result.stdout,
                         r"== verify: \d+s of 1800s; worker read 1\.5 of 0\.0 MB; load 0\.50")
        self.assertFalse(self.stops.exists())

    def test_high_load_stops_only_its_own_unit_and_fails(self):
        (self.root / "proc/loadavg").write_text("31.20 20.00 10.00 1/100 1\n")
        result = self.run_check("--verify-only", "--progress", "1", "--max-load", "24",
                                STUB_SLEEP="3")
        self.assertNotEqual(result.returncode, 0)
        stops = self.stops.read_text().splitlines()
        self.assertEqual(len(stops), 1)
        self.assertRegex(stops[0], r"^stop usnm-scan-archive-check-\d+-verify$")
        self.assertIn("host load 31.20 is above --max-load 24", result.stderr)
        self.assertNotIn("acceptance passed", result.stdout)

    def test_bad_options_fail_before_any_command(self):
        for args in (("--limit", "0"), ("--limit", "-1"), ("--limit", "1001"),
                     ("--limit", "01"), ("--limit", "abc"), ("--deadline", "59"),
                     ("--deadline", "86401"), ("--deadline", "abc"), ("--progress", "0"),
                     ("--max-load", "0"), ("--max-load", "x"),
                     ("--sample-from", str(self.root / "absent.tsv"))):
            with self.subTest(args=args):
                result = self.run_check(*args)
                self.assertNotEqual(result.returncode, 0)
                self.assertFalse(self.calls.exists())
                self.assertFalse(self.prio.exists())

    def test_thousand_scan_boundary_disables_progress_logs(self):
        names = [f"keep/b1/sn1/1900-01-02/1/{seq}.jp2" for seq in range(1000)]
        for name in names:
            self.scan(name)
        self.report.write_text(json.dumps(dict(self.good, scanned=1000, present=1000)))
        result = self.run_check("--limit", "1000", "--sample-from", str(self.manifest(names)))
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(self.calls.read_text().count("Environment=RUST_LOG=off"), 3)

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

    def test_empty_unclean_malformed_short_or_multiple_reports_fail(self):
        for report in (dict(self.good, scanned=0), dict(self.good, scanned=1, present=1),
                       dict(self.good, errors=1), dict(self.good, skipped=1), {}, [],
                       "not json"):
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
