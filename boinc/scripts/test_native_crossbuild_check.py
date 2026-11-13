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

    def test_ratio_range_is_adjustable(self):
        text = "same " * 200
        longer = text + "same " * 10  # ratio 1.05, CER 5%, word F1 0.98
        self.assertFalse(X.judge(longer, text, f1_limit=0.9)["flagged"])
        self.assertTrue(X.judge(longer, text, f1_limit=0.9, ratio_range=(0.98, 1.02))["flagged"])

    def test_linux_replication_thresholds(self):
        words = " ".join(f"w{i}" for i in range(200))
        edited = words.replace("w1", "x1", 5)
        self.assertTrue(X.judge(edited, words, 0.01, 0.98, (0.98, 1.02))["flagged"])
        self.assertFalse(X.judge(words, words, 0.01, 0.98, (0.98, 1.02))["flagged"])
        self.assertFalse(X.judge(edited, words)["flagged"])  # the Windows defaults let it through

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
        model = b"model"
        (t / "td/eng.traineddata").write_bytes(model)
        self.lock = t / "models.lock"
        self.lock.write_text("# c\ntessdata/eng.traineddata http://x %s\n" % hashlib.sha256(model).hexdigest())
        base = "sha256:" + hashlib.sha256(FAKE_TESSERACT.encode()).hexdigest()
        self.trust = t / "trust.json"
        self.trust.write_text(json.dumps({"builds": {
            WIN: {"platform": "windows_x86_64", "engine_base": "sha256:" + "c" * 64},
            "sha256:" + "b" * 64: {"platform": "x86_64-pc-linux-gnu", "engine_base": base}}}))

    def add(self, n, host, windows_text, scan_text=LINUX_TEXT, image=WIN, completed=NOW - 3600,
            scan_bytes=None, text_sha=None, request_scan_sha=None):
        job = f"{n:064x}"
        engine = {"image": image, "profile": "eng", "version": "5.5.0"}
        raw = windows_text.encode()
        scan_sha = hashlib.sha256(scan_text.encode()).hexdigest()
        spool = self.handoff / "spool" / job
        spool.mkdir()
        (spool / "host_id").write_text(host + "\n")
        (spool / f"usnm_{job}__file_result.json").write_text(json.dumps({
            "job_id": job, "scan_sha256": scan_sha, "engine": engine,
            "text_sha256": text_sha or hashlib.sha256(raw).hexdigest(),
            "completed_at": dt.datetime.fromtimestamp(completed, dt.timezone.utc).isoformat()}))
        (spool / f"usnm_{job}__file_text.txt").write_bytes(raw)
        jobs = self.handoff / "dispatch/jobs" / job
        jobs.mkdir()
        (jobs / "request.json").write_text(json.dumps({
            "id": job, "engine": engine,
            "message": {"page": f"sn1/1900-01-0{n}/ed-1/seq-1", "batch": "b"},
            "scan": {"url": "https://x/0001.jp2", "sha256": request_scan_sha or scan_sha}}))
        d = self.scans / "keep/b/sn1" / f"1900-01-0{n}" / "1"
        d.mkdir(parents=True)
        (d / "1.jp2").write_bytes(scan_text.encode() if scan_bytes is None else scan_bytes)
        return job

    def args(self, **kw):
        a = dict(handoff=self.handoff, scans=self.scans, tesseract=self.tess, tessdata=self.out.parent / "td",
                 out_dir=self.out, cer_limit=0.10, f1_limit=0.90, trust_file=self.trust,
                 models_lock=self.lock, work_dir=None, count=10, threads=2, issue=None, repo="r/r")
        a.update(kw)
        return SimpleNamespace(**a)

    def report(self, day="2026-11-14"):
        return json.loads((self.out / f"crossbuild-{day}.json").read_text())

    def test_clean_day(self):
        self.add(1, "15", LINUX_TEXT)
        self.add(2, "16", LINUX_TEXT)
        self.assertEqual(X.run(self.args(), now=NOW), 0)
        r = self.report()
        self.assertEqual(r["summary"]["pages"], 2)
        self.assertEqual(r["summary"]["flagged"], 0)
        self.assertEqual(r["consecutive_flag_days"], 0)
        self.assertEqual(r["summary"]["cer_distribution"]["max"], 0.0)

    def test_ignores_old_pages_and_other_builds(self):
        self.add(1, "15", LINUX_TEXT, completed=NOW - 25 * 3600)
        self.add(2, "15", LINUX_TEXT, image="sha256:" + "b" * 64)
        self.assertEqual(X.run(self.args(), now=NOW), 0)
        self.assertEqual(self.report()["summary"]["pages"], 0)

    def test_window_follows_completed_at_not_file_times(self):
        import os
        job = self.add(1, "15", LINUX_TEXT, completed=NOW - 25 * 3600)
        for f in (self.handoff / "spool" / job).iterdir():
            os.utime(f, (NOW, NOW))
        self.assertEqual(X.run(self.args(), now=NOW), 0)
        self.assertEqual(self.report()["summary"]["pages"], 0)

    def test_a_checked_job_is_not_sampled_again(self):
        self.add(1, "15", LINUX_TEXT[: len(LINUX_TEXT) // 3])
        self.assertEqual(X.run(self.args(), now=NOW), 3)
        self.assertEqual(X.run(self.args(), now=NOW + 3600), 3)  # the day's earlier finding stays
        self.assertEqual(self.report()["summary"]["pages"], 1)  # and the job isn't read twice
        self.assertEqual(X.run(self.args(), now=NOW + 25 * 3600), 0)
        self.assertEqual(self.report("2026-11-15")["summary"]["pages"], 0)

    def test_line_jobs_are_skipped(self):
        job = self.add(1, "15", LINUX_TEXT)
        f = self.handoff / "dispatch/jobs" / job / "request.json"
        r = json.loads(f.read_text())
        r["lines"] = [{"x": 1}]
        f.write_text(json.dumps(r))
        X.run(self.args(), now=NOW)
        self.assertEqual(self.report()["summary"]["pages"], 0)

    def test_a_result_still_being_written_is_deferred(self):
        job = self.add(1, "15", LINUX_TEXT)
        (self.handoff / "spool" / job / "host_id").unlink()
        X.run(self.args(), now=NOW)
        self.assertEqual(self.report()["summary"]["pages"], 0)
        (self.handoff / "spool" / job / "host_id").write_text("15\n")
        X.run(self.args(), now=NOW + 60)
        self.assertEqual(self.report()["summary"]["pages"], 1)

    def test_scan_in_buffer_is_found(self):
        self.add(1, "15", LINUX_TEXT)
        keep = self.scans / "keep/b/sn1/1900-01-01/1/1.jp2"
        (self.scans / "buffer/b/sn1/1900-01-01/1").mkdir(parents=True)
        keep.rename(self.scans / "buffer/b/sn1/1900-01-01/1/1.jp2")
        self.assertEqual(X.run(self.args(), now=NOW), 0)
        self.assertEqual(self.report()["summary"]["pages"], 1)

    def test_multi_model_profile_checks_each_model(self):
        model = self.out.parent / "td"
        (model / "deu.traineddata").write_bytes(b"d")
        (model / "deu_latf.traineddata").write_bytes(b"l")
        self.lock.write_text(self.lock.read_text() + "".join(
            f"tessdata/{n}.traineddata http://x {hashlib.sha256(b).hexdigest()}\n"
            for n, b in (("deu", b"d"), ("deu_latf", b"l"))))
        X.verify_engine(self.args(), {"deu_latf+deu"})
        (model / "deu.traineddata").write_bytes(b"x")
        with self.assertRaises(SystemExit):
            X.verify_engine(self.args(), {"deu_latf+deu"})

    def test_rerun_keeps_the_days_flags(self):
        self.add(1, "15", LINUX_TEXT[: len(LINUX_TEXT) // 3])
        self.assertEqual(X.run(self.args(), now=NOW), 3)
        self.assertEqual(X.run(self.args(), now=NOW + 60), 3)
        r = self.report()
        self.assertEqual((r["summary"]["pages"], r["summary"]["flagged"], r["consecutive_flag_days"]), (1, 1, 1))

    def test_missing_handoff_records_nothing(self):
        import shutil
        shutil.rmtree(self.handoff / "spool")
        with self.assertRaises(SystemExit):
            X.run(self.args(), now=NOW)
        self.assertFalse((self.out / "state.json").exists())

    def test_flags_a_short_windows_text_and_counts_days(self):
        self.add(1, "15", LINUX_TEXT[: len(LINUX_TEXT) // 3])
        self.assertEqual(X.run(self.args(), now=NOW), 3)
        self.assertEqual(self.report()["consecutive_flag_days"], 1)
        self.add(2, "15", LINUX_TEXT[: len(LINUX_TEXT) // 3], completed=NOW + 3600)
        self.assertEqual(X.run(self.args(), now=NOW + 24 * 3600), 3)
        self.assertEqual(self.report("2026-11-15")["consecutive_flag_days"], 2)

    def test_two_flagged_days_say_pause_review(self):
        self.out.mkdir()
        (self.out / "state.json").write_text(json.dumps({"flagged_days": {"2026-11-13": True}}))
        self.add(1, "15", "x" * 10)
        X.run(self.args(), now=NOW)
        self.assertIn("PAUSE-REVIEW", X.summary_line(self.report()))

    def test_scan_with_other_bytes_is_invalid_not_compared(self):
        self.add(1, "15", LINUX_TEXT, scan_bytes=b"different")
        self.assertEqual(X.run(self.args(), now=NOW), 0)
        r = self.report()
        self.assertEqual(r["summary"]["invalid"], 1)
        self.assertIn("not the bytes", r["rows"][0]["invalid"])

    def test_text_checksum_and_request_binding_are_checked(self):
        self.add(1, "15", LINUX_TEXT, text_sha="0" * 64)
        self.add(2, "15", LINUX_TEXT, request_scan_sha="1" * 64)
        self.assertEqual(X.run(self.args(), now=NOW), 0)
        reasons = sorted(r["invalid"] for r in self.report()["rows"])
        self.assertEqual(reasons, ["result does not match its dispatch request", "text does not match its checksum"])

    def test_non_utf8_text_is_invalid(self):
        job = self.add(1, "15", LINUX_TEXT)
        f = self.handoff / "spool" / job / f"usnm_{job}__file_text.txt"
        raw = b"\xff\xfe bad"
        f.write_bytes(raw)
        rf = self.handoff / "spool" / job / f"usnm_{job}__file_result.json"
        r = json.loads(rf.read_text())
        r["text_sha256"] = hashlib.sha256(raw).hexdigest()
        rf.write_text(json.dumps(r))
        X.run(self.args(), now=NOW)
        self.assertIn("UTF-8", self.report()["rows"][0]["invalid"])

    def test_unrecorded_engine_or_model_is_refused(self):
        self.add(1, "15", LINUX_TEXT)
        self.tess.write_text(FAKE_TESSERACT + "# other\n")
        with self.assertRaises(SystemExit):
            X.run(self.args(), now=NOW)
        self.tess.write_text(FAKE_TESSERACT)
        (self.out.parent / "td/eng.traineddata").write_bytes(b"drifted")
        with self.assertRaises(SystemExit):
            X.run(self.args(), now=NOW)

    def test_engine_failure_is_flagged(self):
        bad = "#!/bin/sh\nexit 1\n"
        self.tess.write_text(bad)
        self.trust.write_text(json.dumps({"builds": {
            WIN: {"platform": "windows_x86_64", "engine_base": "sha256:" + "c" * 64},
            "sha256:" + "b" * 64: {"platform": "x86_64-pc-linux-gnu",
                                   "engine_base": "sha256:" + hashlib.sha256(bad.encode()).hexdigest()}}}))
        self.add(1, "15", LINUX_TEXT)
        self.assertEqual(X.run(self.args(), now=NOW), 3)
        self.assertIn("exited 1", self.report()["rows"][0]["error"])

    def linux_args(self, **kw):
        out = self.out.parent / "out-linux"
        return self.args(out_dir=out, image="4d6e", hosts=["13"], cer_limit=0.01, f1_limit=0.98,
                         ratio_min=0.98, ratio_max=1.02, label="linux", plan="native-feed-4", **kw)

    def set_linux_build(self):
        self.lin = "sha256:4d6e" + "b" * 60
        self.trust.write_text(json.dumps({"builds": {
            WIN: {"platform": "windows_x86_64", "engine_base": "sha256:" + "c" * 64},
            self.lin: {"platform": "x86_64-pc-linux-gnu",
                       "engine_base": "sha256:" + hashlib.sha256(FAKE_TESSERACT.encode()).hexdigest()}}}))

    def test_image_and_host_selector_picks_only_that_build(self):
        self.set_linux_build()
        self.add(1, "13", LINUX_TEXT, image=self.lin)
        self.add(2, "14", LINUX_TEXT, image=self.lin)  # other host
        self.add(3, "13", LINUX_TEXT)  # Windows build
        self.assertEqual(X.run(self.linux_args(), now=NOW), 0)
        r = json.loads((self.out.parent / "out-linux/crossbuild-2026-11-14.json").read_text())
        self.assertEqual([(x["host"], x["job"]) for x in r["rows"]], [("13", f"{1:064x}")])
        self.assertEqual(r["windows_images"], [self.lin])
        self.assertEqual(r["plan"], "native-feed-4")
        self.assertEqual(r["limits"]["ratio"], [0.98, 1.02])
        # the default selector still means the Windows builds, in its own state dir
        self.assertEqual(X.run(self.args(), now=NOW), 0)
        self.assertEqual([x["job"] for x in self.report()["rows"]], [f"{3:064x}"])

    def test_unknown_image_prefix_is_an_error(self):
        with self.assertRaises(SystemExit):
            X.run(self.args(image="deadbeef"), now=NOW)

    def test_instances_keep_separate_state_and_streaks(self):
        self.set_linux_build()
        self.add(1, "13", LINUX_TEXT.replace("quick", "quack", 4), image=self.lin)  # CER 0.4%, F1 passes defaults
        self.add(2, "13", LINUX_TEXT, image=WIN)
        self.assertEqual(X.run(self.linux_args(), now=NOW), 3)  # 4 of 20 lines changed: F1 under 0.98
        self.assertEqual(X.run(self.args(), now=NOW), 0)
        lin_dir = self.out.parent / "out-linux"
        self.assertEqual(json.loads((lin_dir / "state.json").read_text())["flagged_days"], {"2026-11-14": True})
        self.assertEqual(json.loads((self.out / "state.json").read_text())["flagged_days"], {"2026-11-14": False})
        self.assertIn("crossbuild [linux] 2026-11-14", X.summary_line(
            json.loads((lin_dir / "crossbuild-2026-11-14.json").read_text())))
        self.assertTrue(X.summary_line(self.report()).startswith("crossbuild 2026-11-14"))


if __name__ == "__main__":
    unittest.main()
