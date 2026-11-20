"""Tests for the Tesseract re-read gain scorer (#1999): no database, no parquet, a stub scorer."""
import contextlib
import datetime as dt
import importlib.util
import io
import json
import stat
import tempfile
import unittest
from pathlib import Path
from unittest import mock

HERE = Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location("tess_gain", HERE / "tess_gain.py")
g = importlib.util.module_from_spec(spec)
spec.loader.exec_module(g)
spec2 = importlib.util.spec_from_file_location("tess_worstfirst", HERE / "tess_worstfirst.py")
w = importlib.util.module_from_spec(spec2)
spec2.loader.exec_module(w)

UTC = dt.timezone.utc


def at(day):
    return dt.datetime(2026, 11, day, tzinfo=UTC)


def doc(i):
    return f"sn{i:08d}_1900-01-01_ed-1_seq-1"


# A stand-in for usnm_quality: checks the job's identity the way QeTextJob::identity does and scores
# a page by its length, so the test can tell pages apart.
STUB = r"""#!/usr/bin/env python3
import hashlib, json
j = json.load(open("qe_text.json", encoding="utf-8"))
ident = json.dumps([j["v"], j["archive"], j["group"], j["lock"], j["language"], j["pages"], j["qe"]],
                   separators=(",", ":"), ensure_ascii=False)
assert hashlib.sha256(ident.encode()).hexdigest() == j["id"], "identity"
assert hashlib.sha256(open("qe.bin", "rb").read()).hexdigest() == j["qe"]["input"]["sha256"]
rows = [{"doc_id": p["doc_id"], "qe": None if not p["text"].strip() else {"lm_oov": len(p["text"]) / 10.0}, "null_reason": None}
        for p in j["pages"]]
json.dump({"v": 1, "id": j["id"], "rows": rows}, open("qe_text_result.json", "w"))
"""


class Ledger(unittest.TestCase):
    def q(self, d, plan, day):
        return {"doc_id": d, "plan": plan, "queued_at": at(day)}

    def o(self, d, outcome, day, cause=None):
        return {"doc_id": d, "outcome": outcome, "cause": cause, "at": at(day)}

    def test_pass_rate_counts_published_against_dead_lettered_and_held(self):
        queued = [self.q(doc(i), "c-w1", 1) for i in range(1, 9)] + [self.q(doc(99), "other", 1)]
        outcomes = [self.o(doc(1), "imported", 2), self.o(doc(2), "imported", 2), self.o(doc(3), "imported", 2),
                    self.o(doc(4), "imported", 2),                      # held below
                    self.o(doc(5), "dead_lettered", 2, "permanent"),
                    self.o(doc(6), "not_sent", 2),
                    self.o(doc(7), "dead_lettered", 2, "unroutable"),   # goes round again: pending
                    self.o(doc(99), "imported", 2)]                     # another campaign's
        states = g.ledger_outcomes(queued, outcomes, "c-w1")
        self.assertEqual(len(states), 8)
        rate, c, published = g.pass_rate(states, {doc(4)})
        self.assertEqual(published, {doc(1), doc(2), doc(3)})
        self.assertEqual((c["published"], c["held"], c["dead_lettered"], c["not_sent"], c["pending"]), (3, 1, 1, 1, 2))
        self.assertAlmostEqual(rate, 3 / 6)

    def test_an_outcome_older_than_the_latest_queue_row_is_pending(self):
        queued = [self.q(doc(1), "c-w1", 1), self.q(doc(1), "c-w1", 5)]
        self.assertEqual(g.ledger_outcomes(queued, [self.o(doc(1), "imported", 3)], "c-w1"), {doc(1): "pending"})

    def test_a_page_requeued_under_another_plan_leaves_the_campaign(self):
        queued = [self.q(doc(1), "c-w1", 1), self.q(doc(1), "later", 5)]
        self.assertEqual(g.ledger_outcomes(queued, [], "c-w1"), {})

    def test_nothing_settled_has_no_rate(self):
        self.assertIsNone(g.pass_rate({doc(1): "pending"}, set())[0])


class Gain(unittest.TestCase):
    cuts = [1, 2, 3, 4, 5, 6, 7, 8, 9]

    def test_decile_bounds(self):
        self.assertEqual(g.decile_of(0.5, self.cuts), 1)
        self.assertEqual(g.decile_of(5.0, self.cuts), 6)
        self.assertEqual(g.decile_of(99, self.cuts), 10)

    def test_gain_is_loc_minus_ours_and_median_is_over_pages(self):
        pairs = [("a", 9.5, 2.5), ("b", 9.5, 3.5), ("c", 0.5, 0.75), ("d", 0.5, 0.25), ("e", 9.5, 9.0)]
        s = g.gain_summary(pairs, self.cuts)
        self.assertEqual(s["n_paired"], 5)
        self.assertEqual(s["median_gain"], 0.5)         # gains 7, 6, -0.25, 0.25, 0.5
        self.assertEqual(s["by_decile"]["10"]["n"], 3)
        self.assertEqual(s["by_decile"]["10"]["median_gain"], 6.0)
        self.assertEqual(s["by_decile"]["1"]["median_gain"], 0.0)

    def test_empty(self):
        self.assertIsNone(g.gain_summary([], self.cuts)["median_gain"])

    def test_wave_done_follows_the_timers_test(self):
        cfg = dict(g.DEFAULTS)
        st = {"wave": 1, "wave_queued": 20000, "wave_started": "2026-11-15T00:00:00Z", "published": 18999}
        self.assertFalse(g.wave_done(cfg, st, at(16)))
        self.assertTrue(g.wave_done(cfg, dict(st, published=19000), at(16)))
        self.assertTrue(g.wave_done(cfg, st, dt.datetime(2026, 12, 7, tzinfo=UTC)))

    def test_an_early_chunk_is_not_a_done_wave(self):
        cfg = dict(g.DEFAULTS)
        st = {"wave": 1, "wave_queued": 3000, "wave_started": "2026-11-15T00:00:00Z", "published": 3000}
        self.assertFalse(g.wave_done(cfg, st, at(16)))
        self.assertEqual(w.decide(dict(w.DEFAULTS, rebuild_done_marker="/nonexistent/marker"),
                                  dict(st, outstanding=0, exhausted=False, campaign="c"), at(16), 3000, None)[0], "enqueue")

    def test_stopped_or_exhausted_is_not_done(self):
        st = {"wave": 1, "wave_queued": 20000, "wave_started": "2026-11-15T00:00:00Z", "published": 20000}
        self.assertFalse(g.wave_done(g.DEFAULTS, dict(st, exhausted=True), at(16)))

    def test_hold_matches_the_reading_it_names(self):
        tag = "tesseract/5.5.1 profile=eng image=x models=y route=r@r1 contract=1"
        self.assertTrue(g.is_held([(None, "boinc-a")], tag, "boinc-a"))
        self.assertFalse(g.is_held([(None, "boinc-b")], tag, "boinc-a"))
        self.assertTrue(g.is_held([("tesseract/5.5.1 eng", None)], tag, "boinc-a"))
        self.assertTrue(g.is_held([(tag, "boinc-a")], tag, "boinc-a"))
        self.assertFalse(g.is_held([("tesseract/5.5.1 eng", "boinc-b")], tag, "boinc-a"))
        self.assertFalse(g.is_held([("other/1 eng", None)], tag, "boinc-a"))


class Contract(unittest.TestCase):
    def test_shared_settings_match_the_wave_timer(self):
        for k in ("language", "campaign_prefix", "wave_sizes", "complete_fraction", "max_wave_days", "pause_after",
                  "rebuild_done_marker", "dsn_file"):
            self.assertEqual(g.DEFAULTS[k], w.DEFAULTS[k], k)

    def test_what_the_timer_reads_is_what_is_written(self):
        gain = {"pass_rate": 0.9, "median_gain": 0.4}
        cfg = dict(w.DEFAULTS, rebuild_done_marker="/nonexistent/marker")
        st = {"wave": 1, "wave_queued": 20000, "wave_skipped": 0, "outstanding": 0, "exhausted": False,
              "wave_started": "2026-11-15T12:00:00Z", "campaign": "c"}
        self.assertEqual(w.decide(cfg, st, at(16), 20000, gain)[0], "next_wave")

    def test_job_identity_and_shape(self):
        j = g.unit_job("tess-worstfirst-eng-w1", "eng", "a" * 64, b"qe", [("p_1", "héllo \"x\"\n")])
        self.assertEqual(list(j), ["v", "id", "archive", "group", "lock", "language", "pages", "qe"])
        self.assertEqual(list(j["pages"][0]), ["doc_id", "language_source", "text"])
        self.assertEqual(list(j["qe"]), ["input", "language"])
        self.assertEqual(list(j["qe"]["input"]), ["bytes", "sha256", "source"])
        self.assertRegex(j["id"], r"^[0-9a-f]{64}$")
        self.assertRegex(g.GROUP, r"^[a-z0-9-]{1,32}$")


class Run(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        d = Path(self.tmp.name)
        self.state, self.qe = d / "state", d / "qe"
        self.state.mkdir()
        self.qe.mkdir()
        (self.qe / "eng.qe").write_bytes(b"qe bytes")
        self.lockfile = d / "resources.lock.json"
        self.lockfile.write_text(json.dumps({"languages": {"eng": {"sha256": g.sha256(b"qe bytes")}}}))
        self.stub = d / "usnm_quality"
        self.stub.write_text(STUB)
        self.stub.chmod(self.stub.stat().st_mode | stat.S_IXUSR)
        (d / "dsn").write_text("postgresql:///x\n")
        self.cfg = d / "cfg.json"
        self.cfg.write_text(json.dumps({
            "scorer": str(self.stub), "qe_dir": str(self.qe), "dsn_file": str(d / "dsn"),
            "rebuild_done_marker": str(d / "none"), "pause_after": "2999-01-01T00:00:00Z",
            "unit_pages": 2, "score_pages_per_run": 100, "qe_lock_file": str(self.lockfile),
            "wave_sizes": [5], "complete_fraction": 0.8}))
        self.campaign = "tess-worstfirst-eng-w1"
        (self.state / "state.json").write_text(json.dumps({
            "campaign": self.campaign, "wave": 1, "wave_queued": 5, "published": 4,
            "wave_started": g.iso(g.now_utc())}))
        ids = [doc(i) for i in range(1, 6)]
        self.queued = [{"doc_id": i, "plan": self.campaign, "queued_at": at(1)} for i in ids]
        self.outcomes = [{"doc_id": i, "outcome": "imported", "cause": None, "at": at(2)} for i in ids[:4]]
        self.outcomes.append({"doc_id": ids[4], "outcome": "dead_lettered", "cause": "permanent", "at": at(2)})
        # ours: 10 characters a page scores 1.0, 20 scores 2.0, and so on; LoC's score is a fixed 5.0
        self.texts = {doc(i): "x" * (10 * i) for i in range(1, 5)}
        self.loc = {doc(i): 5.0 for i in range(1, 6)}

    def run_job(self, *extra, holds=None, loc_lock=None):
        def psql(dsn, sql):
            if "percentile_cont" in sql:
                return "\n".join(str(x) for x in range(1, 10)) + "\n"
            raise AssertionError(sql)
        out = io.StringIO()
        with mock.patch.object(g, "ledger_rows", lambda root, kind, cols: self.queued if kind == "queued" else self.outcomes), \
                mock.patch.object(g, "published_readings", lambda root, ids: {
                    d: (t, "tesseract/5.5.1 profile=eng", "boinc-" + d[-12:]) for d, t in self.texts.items() if d in ids}), \
                mock.patch.object(g, "hold_rows", lambda dsn, ids: holds or {}), \
                mock.patch.object(g, "loc_scores", lambda dsn, ids, lang: (
                    {d: v for d, v in self.loc.items() if d in ids}, {loc_lock or g.sha256(self.lockfile.read_bytes())})), \
                mock.patch.object(g, "psql", psql), contextlib.redirect_stdout(out):
            g.main(["run", "--config", str(self.cfg), "--state-dir", str(self.state), *extra])
        lines = out.getvalue().strip().splitlines()
        self.assertEqual(len(lines), 1)
        return json.loads(lines[0])

    def path(self, suffix):
        return self.state / "gain" / (self.campaign + suffix)

    def final(self):
        return json.loads(self.path(".json").read_text())

    def test_writes_what_the_timer_reads(self):
        line = self.run_job()
        self.assertEqual(line["status"], "written")
        f = self.final()
        # ours scores 1, 2, 3, 4 against LoC's 5: gains 4, 3, 2, 1
        self.assertEqual(f["median_gain"], 2.5)
        self.assertEqual(f["n_paired"], 4)
        self.assertAlmostEqual(f["pass_rate"], 4 / 5)
        self.assertEqual((f["ledger"]["published"], f["ledger"]["dead_lettered"]), (4, 1))
        self.assertEqual(line["scored_now"], 4)

    def test_rerun_scores_nothing_and_writes_the_same_file(self):
        self.run_job()
        first = self.final()
        again = self.run_job()
        self.assertEqual(again["scored_now"], 0)
        second = self.final()
        first.pop("computed_at")
        second.pop("computed_at")
        self.assertEqual(first, second)

    def test_changed_text_is_scored_again(self):
        self.run_job()
        self.texts[doc(1)] = "y" * 100
        self.assertEqual(self.run_job()["scored_now"], 1)
        self.assertEqual(self.final()["n_paired"], 4)

    def test_held_page_is_out_of_the_rate_and_the_gain(self):
        self.run_job(holds={doc(4): [(None, "boinc-" + doc(4)[-12:])]})
        f = self.final()
        self.assertEqual(f["ledger"]["held"], 1)
        self.assertAlmostEqual(f["pass_rate"], 3 / 5)
        self.assertEqual(f["n_paired"], 3)

    def test_a_hold_on_another_reading_does_not_count(self):
        self.run_job(holds={doc(4): [(None, "boinc-someone-else")]})
        self.assertEqual(self.final()["ledger"]["held"], 0)

    def test_nothing_final_until_the_wave_is_done(self):
        (self.state / "state.json").write_text(json.dumps({
            "campaign": self.campaign, "wave": 1, "wave_queued": 5, "published": 1, "wave_started": g.iso(g.now_utc())}))
        self.assertEqual(self.run_job()["status"], "progress")
        self.assertFalse(self.path(".json").exists())
        self.assertTrue(self.path(".progress.json").exists())

    def test_nothing_final_while_pages_await_a_score(self):
        self.cfg.write_text(json.dumps(dict(json.loads(self.cfg.read_text()), score_pages_per_run=2)))
        self.assertEqual(self.run_job()["status"], "progress")
        self.assertFalse(self.path(".json").exists())
        self.assertEqual(self.run_job()["status"], "written")      # the cache holds the first two

    def test_an_incomplete_ledger_copy_or_missing_text_is_not_final(self):
        del self.queued[-1]
        self.assertIn("ledger copy", self.run_job()["why"])
        self.queued.append({"doc_id": doc(5), "plan": self.campaign, "queued_at": at(1)})
        del self.texts[doc(2)]
        self.assertIn("not in the dataset copy", self.run_job()["why"])
        self.assertFalse(self.path(".json").exists())

    def test_a_changed_resource_or_scorer_is_scored_again(self):
        self.run_job()
        self.stub.write_text(STUB + "\n# another build\n")
        self.assertEqual(self.run_job()["scored_now"], 4)

    def test_a_resource_file_that_is_not_the_pinned_one_fails(self):
        (self.qe / "eng.qe").write_bytes(b"other")
        with self.assertRaises(SystemExit):
            self.run_job()
        self.assertFalse(self.path(".json").exists())

    def test_loc_rows_with_another_lock_fail(self):
        with self.assertRaises(SystemExit):
            self.run_job(loc_lock="c" * 64)
        self.assertFalse(self.path(".json").exists())

    def test_the_timer_running_blocks_the_final_file(self):
        import fcntl
        with open(self.state / "lock", "w") as f:
            fcntl.flock(f, fcntl.LOCK_EX)
            self.assertEqual(self.run_job()["status"], "progress")
        self.assertFalse(self.path(".json").exists())

    def test_units_are_packed_by_bytes_and_oversize_pages_are_not_sent(self):
        self.cfg.write_text(json.dumps(dict(json.loads(self.cfg.read_text()), unit_pages=100, max_unit_bytes=300)))
        self.texts[doc(1)] = "z" * (g.MAX_PAGE_TEXT + 1)
        line = self.run_job()
        self.assertEqual(line["scored_now"], 4)
        self.assertEqual(self.final()["n_paired"], 3)
        self.assertEqual(self.final()["no_words"], 1)

    def test_no_state_is_idle(self):
        (self.state / "state.json").unlink()
        self.assertEqual(self.run_job()["status"], "idle")

    def test_paused_after_the_cutoff_until_the_marker(self):
        self.cfg.write_text(json.dumps(dict(json.loads(self.cfg.read_text()), pause_after="2020-01-01T00:00:00Z")))
        self.assertEqual(self.run_job()["status"], "paused")

    def test_a_failing_scorer_fails_the_run_and_writes_nothing(self):
        self.stub.write_text("#!/bin/sh\nexit 3\n")
        with self.assertRaises(SystemExit):
            self.run_job()
        self.assertFalse(self.path(".json").exists())


if __name__ == "__main__":
    unittest.main()
