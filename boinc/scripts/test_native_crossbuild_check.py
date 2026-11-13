import datetime as dt
import hashlib
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest
from types import SimpleNamespace

ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location("xb", ROOT / "boinc/scripts/native_crossbuild_check.py")
X = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(X)

WIN = "sha256:" + "a" * 64
NOW = dt.datetime(2026, 11, 14, 12, 0, tzinfo=dt.timezone.utc).timestamp()
LINUX_TEXT = "the quick brown fox jumps over the lazy dog\n" * 20


class DistanceTest(unittest.TestCase):
    def test_known_distances(self):
        self.assertEqual(X.distance("kitten", "sitting"), 3)
        self.assertEqual(X.distance("", "abc"), 3)
        self.assertEqual(X.distance("abc", ""), 3)
        self.assertEqual(X.distance("same", "same"), 0)


class JudgeTest(unittest.TestCase):
    def test_identical_is_not_flagged(self):
        self.assertFalse(X.judge(LINUX_TEXT, LINUX_TEXT)["flagged"])

    def test_cer_over_ten_percent_flags(self):
        text = "word " * 200
        edited = text.replace("word", "wxrd", 60)  # 60 edits of 1000 chars: CER 6%, F1 hit
        self.assertLess(X.judge(edited, text)["cer"], 0.10)
        heavy = text.replace("word", "xxxx", 120)
        r = X.judge(heavy, text)
        self.assertGreater(r["cer"], 0.10)
        self.assertTrue(r["flagged"])

    def test_one_to_eight_percent_layout_noise_passes(self):
        words = [f"w{i}" for i in range(500)]
        text = " ".join(words)
        shuffled = " ".join(reversed(words))  # same words, other order: F1 1.0
        r = X.judge(shuffled[: len(text)], text)
        self.assertEqual(r["word_f1"], 1.0)
        self.assertFalse(X.judge(text.replace("w1", "w9", 3), text)["flagged"])

    def test_low_word_f1_flags_with_small_cer(self):
        text = " ".join(f"w{i}" for i in range(100))
        bad = text.replace("w", "v", 20)  # 20 of 100 words changed: F1 0.8, CER ~4%
        r = X.judge(bad, text)
        self.assertLess(r["cer"], 0.10)
        self.assertLess(r["word_f1"], 0.9)
        self.assertTrue(r["flagged"])

    def test_ratio_outside_band_flags_even_with_low_cer(self):
        text = "a" * 1000
        self.assertTrue(X.judge("a" * 899, text)["flagged"])
        self.assertTrue(X.judge("a" * 1101, text)["flagged"])
        self.assertFalse(X.judge("a" * 1000, text)["flagged"])

    def test_cer_limit_is_adjustable(self):
        text = " ".join(["same"] * 50)
        edited = text.replace("same", "sbme", 5)
        self.assertGreater(X.judge(edited, text, cer_limit=0.01, f1_limit=0)["cer"], 0.01)
        self.assertTrue(X.judge(edited, text, cer_limit=0.01, f1_limit=0)["flagged"])
        self.assertFalse(X.judge(edited, text, cer_limit=0.5, f1_limit=0)["flagged"])

    def test_distribution(self):
        d = X.distribution([0.01, 0.02, 0.09])
        self.assertEqual((d["min"], d["median"], d["max"]), (0.01, 0.02, 0.09))
        self.assertIsNone(X.distribution([]))

    def test_a_missing_half_is_flagged(self):
        r = X.judge(LINUX_TEXT[: len(LINUX_TEXT) // 3], LINUX_TEXT)
        self.assertTrue(r["flagged"])
        self.assertLess(r["ratio"], 0.9)


class SampleTest(unittest.TestCase):
    def pages(self, per_host):
        return [{"job": f"{h}-{i}", "host": h} for h, n in per_host.items() for i in range(n)]

    def test_is_seeded_and_repeatable(self):
        pages = self.pages({"15": 40, "16": 40})
        self.assertEqual(X.sample(pages, "2026-11-14", 10), X.sample(list(reversed(pages)), "2026-11-14", 10))
        self.assertNotEqual(X.sample(pages, "2026-11-14", 10), X.sample(pages, "2026-11-15", 10))

    def test_is_stratified_by_host(self):
        got = X.sample(self.pages({"15": 40, "16": 40, "17": 3}), "d", 10)
        counts = {h: sum(p["host"] == h for p in got) for h in ("15", "16", "17")}
        self.assertEqual(counts, {"15": 4, "16": 3, "17": 3})

    def test_a_small_host_gives_what_it_has(self):
        got = X.sample(self.pages({"15": 40, "16": 2}), "d", 10)
        self.assertEqual(len(got), 10)
        self.assertEqual(sum(p["host"] == "16" for p in got), 2)

    def test_fewer_pages_than_count(self):
        self.assertEqual(len(X.sample(self.pages({"15": 3}), "d", 10)), 3)
        self.assertEqual(X.sample([], "d", 10), [])


class ConsecutiveTest(unittest.TestCase):
    def test_counts_back_from_the_date(self):
        run, days = X.consecutive_days({"flagged_days": {"2026-11-13": True}}, "2026-11-14", True)
        self.assertEqual(run, 2)
        self.assertEqual(X.consecutive_days({"flagged_days": days}, "2026-11-15", False)[0], 0)

    def test_a_gap_resets_and_a_rerun_is_idempotent(self):
        state = {"flagged_days": {"2026-11-12": True, "2026-11-13": False}}
        run, days = X.consecutive_days(state, "2026-11-14", True)
        self.assertEqual(run, 1)
        self.assertEqual(X.consecutive_days({"flagged_days": days}, "2026-11-14", True)[0], 1)


class ScanPathTest(unittest.TestCase):
    def test_layout(self):
        page = {"page": "sn83030272/1876-10-08/ed-1/seq-7", "batch": "nn_carson",
                "scan_url": "https://tile.loc.gov/x/0746.jp2"}
        self.assertEqual(X.scan_path("/s", page),
                         Path("/s/keep/nn_carson/sn83030272/1876-10-08/1/7.jp2"))


class TrustFileTest(unittest.TestCase):
    def test_repo_lists_a_windows_build(self):
        images = X.windows_images(ROOT / "boinc/project/native-build-trust.json")
        self.assertTrue(images)
        self.assertTrue(all(i.startswith("sha256:") for i in images))


FAKE_TESSERACT = """#!/bin/sh
# usage: tesseract IMAGE BASE -l L --psm N --dpi N txt alto
test "$OMP_THREAD_LIMIT" = 1 || exit 9
test "$3" = -l && test "$5" = --psm && test "$7" = --dpi && test "$9" = txt || exit 8
cat "$1" > "$2.txt"
"""


class EndToEndTest(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        t = Path(self.tmp.name)
        self.handoff, self.scans, self.out = t / "handoff", t / "scans", t / "out"
        (self.handoff / "spool").mkdir(parents=True)
        (self.handoff / "dispatch/jobs").mkdir(parents=True)
        self.tess = t / "tesseract"
        self.tess.write_text(FAKE_TESSERACT)
        self.tess.chmod(0o755)
        (t / "td").mkdir()
        self.trust = t / "trust.json"
        self.trust.write_text(json.dumps({"builds": {WIN: {"platform": "windows_x86_64"},
                                                     "sha256:" + "b" * 64: {"platform": "x86_64-pc-linux-gnu"}}}))

    def add(self, n, host, windows_text, scan_text=LINUX_TEXT, image=WIN, age=3600, scan_bytes=None):
        job = f"{n:064x}"
        spool = self.handoff / "spool" / job
        spool.mkdir()
        (spool / "host_id").write_text(host + "\n")
        (spool / f"usnm_{job}__file_result.json").write_text(json.dumps(
            {"engine": {"image": image, "profile": "eng", "version": "5.5.0"}}))
        (spool / f"usnm_{job}__file_text.txt").write_text(windows_text)
        scan = scan_text.encode() if scan_bytes is None else scan_bytes
        jobs = self.handoff / "dispatch/jobs" / job
        jobs.mkdir()
        (jobs / "request.json").write_text(json.dumps({
            "message": {"page": f"sn1/1900-01-0{n}/ed-1/seq-1", "batch": "b"},
            "scan": {"url": "https://x/0001.jp2", "sha256": hashlib.sha256(scan_text.encode()).hexdigest()}}))
        d = self.scans / "keep/b/sn1" / f"1900-01-0{n}" / "1"
        d.mkdir(parents=True)
        (d / "1.jp2").write_bytes(scan)
        import os
        for f in spool.iterdir():
            os.utime(f, (NOW - age, NOW - age))
        return job

    def args(self, **kw):
        a = dict(handoff=self.handoff, scans=self.scans, tesseract=self.tess, tessdata=self.out.parent / "td",
                 out_dir=self.out, cer_limit=0.10, f1_limit=0.90, trust_file=self.trust, work_dir=None, count=10, threads=2, issue=None,
                 repo="r/r")
        a.update(kw)
        return SimpleNamespace(**a)

    def report(self):
        return json.loads((self.out / "crossbuild-2026-11-14.json").read_text())

    def test_clean_day(self):
        self.add(1, "15", LINUX_TEXT)
        self.add(2, "16", LINUX_TEXT)
        self.assertEqual(X.run(self.args(), now=NOW), 0)
        r = self.report()
        self.assertEqual(r["summary"]["pages"], 2)
        self.assertEqual(r["summary"]["flagged"], 0)
        self.assertEqual(r["consecutive_flag_days"], 0)

    def test_ignores_old_pages_and_other_builds(self):
        self.add(1, "15", LINUX_TEXT, age=25 * 3600)
        self.add(2, "15", LINUX_TEXT, image="sha256:" + "b" * 64)
        self.assertEqual(X.run(self.args(), now=NOW), 0)
        self.assertEqual(self.report()["summary"]["pages"], 0)

    def test_flags_a_short_windows_text_and_counts_days(self):
        self.add(1, "15", LINUX_TEXT[: len(LINUX_TEXT) // 3])
        self.assertEqual(X.run(self.args(), now=NOW), 3)
        self.assertEqual(self.report()["consecutive_flag_days"], 1)
        self.add(2, "15", LINUX_TEXT[: len(LINUX_TEXT) // 3], age=-23 * 3600)
        self.assertEqual(X.run(self.args(), now=NOW + 24 * 3600), 3)
        r = json.loads((self.out / "crossbuild-2026-11-15.json").read_text())
        self.assertEqual(r["consecutive_flag_days"], 2)

    def test_two_flagged_days_say_pause_review(self):
        (self.out).mkdir()
        (self.out / "state.json").write_text(json.dumps({"flagged_days": {"2026-11-13": True}}))
        self.add(1, "15", "x" * 10)
        X.run(self.args(), now=NOW)
        self.assertIn("PAUSE-REVIEW", X.summary_line(self.report()))

    def test_scan_with_other_bytes_is_flagged_not_compared(self):
        self.add(1, "15", LINUX_TEXT, scan_bytes=b"different")
        self.assertEqual(X.run(self.args(), now=NOW), 3)
        self.assertIn("not the bytes", self.report()["rows"][0]["error"])

    def test_engine_failure_is_flagged(self):
        self.tess.write_text("#!/bin/sh\nexit 1\n")
        self.add(1, "15", LINUX_TEXT)
        self.assertEqual(X.run(self.args(), now=NOW), 3)
        self.assertIn("exited 1", self.report()["rows"][0]["error"])


if __name__ == "__main__":
    unittest.main()
