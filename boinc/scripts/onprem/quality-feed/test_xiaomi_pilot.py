"""Tests for the pilot timer step (#1998): the commands are faked, nothing is staged."""
import datetime
import importlib.util
import json
import os
import sys
import tempfile
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path[:0] = [str(HERE), str(HERE.parent / "ocr-feed")]
spec = importlib.util.spec_from_file_location("xiaomi_pilot", HERE / "xiaomi_pilot.py")
xp = importlib.util.module_from_spec(spec)
spec.loader.exec_module(xp)

T0 = datetime.datetime(2026, 11, 20, 12, 0, tzinfo=datetime.timezone.utc)


def hours(h):
    return T0 + datetime.timedelta(hours=h)


class Fake:
    """Records the commands and answers page-collect with `summary`."""

    def __init__(self, summary):
        self.summary = summary
        self.cmds = []

    def __call__(self, cmd):
        self.cmds.append(cmd)
        if "submit" in cmd and "--max-units" in cmd:
            return json.dumps({"submitted": 1, "uncertain": 0})
        return json.dumps(self.summary) if "page-collect" in cmd else ""


class Pilot(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        d = Path(self.tmp.name)
        self.pilot = d / "pilot"
        self.pilot.mkdir()
        (self.pilot / "pilot.csv").write_text("doc_id\n")
        (self.pilot / "campaign.json").write_text("{}")
        for i in range(4):
            (self.pilot / f"members-{i:03d}.json").write_text("[]")
        (self.pilot / "strata.json").write_text("{}")
        self.state = d / "state.json"
        self.a = xp.parser().parse_args([
            "--state", str(self.state), "--pilot-dir", str(self.pilot), "--root", str(d / "root"),
            "--quality-root", str(d / "q"), "--project", str(d / "p"), "--spool", str(d / "s"),
            "--crop-states", str(d / "c"), "--held", str(d / "held"), "--engine-image", "sha256:a",
            "--engine-llama", "b", "--engine-weights", "c", "--chunk-pages", "500",
            "--max-in-flight", "1000", "--total", "2000"])

    def saved(self):
        return json.loads(self.state.read_text())

    def test_the_first_run_stages_and_submits_a_chunk_with_the_page_rule_target(self):
        fake = Fake({"pages": 0})
        rec = xp.step(self.a, T0, fake)
        self.assertEqual(rec["action"], "submitted members-000.json")
        stage, submit = fake.cmds[1], fake.cmds[2]
        self.assertIn("--page-mode", stage)
        self.assertEqual(submit[submit.index("--target") + 1], "native-xiaomi-page")
        self.assertEqual(self.saved()["next_chunk"], 1)

    def test_runs_are_paced_by_the_pages_in_flight(self):
        fake = Fake({"pages": 600, "pages_pending": 600})
        xp.step(self.a, T0, fake)  # 600 + 500 > 1000: wait
        self.assertEqual(len(fake.cmds), 1)
        fake = Fake({"pages": 600, "pages_pending": 400, "held": 200})
        self.assertEqual(xp.step(self.a, hours(1), fake)["action"], "submitted members-000.json")

    def test_all_settled_when_every_chunk_is_staged_and_nothing_waits(self):
        self.a.total = 100000
        self.state.write_text(json.dumps({"started_at": T0.isoformat(), "next_chunk": 4}))
        rec = xp.step(self.a, T0, Fake({"pages": 1800, "pages_pending": 0, "held": 1800}))
        self.assertEqual(rec["stopped"], "all-settled")
        self.assertEqual(self.saved()["stopped"]["reason"], "all-settled")

    def test_too_many_invalid_stops_it_only_once_enough_have_returned(self):
        st = {"started_at": T0.isoformat()}
        # Four-page units: 150 units are 600 pages, and still under the 200-unit threshold.
        few = {"pages": 600, "pages_pending": 0, "units": 150, "pending": 0, "units_invalid": 100}
        self.assertIsNone(xp.decide(few, st, 10000, T0))
        many = {"pages": 900, "pages_pending": 0, "units": 250, "pending": 0, "units_invalid": 51}
        self.assertEqual(xp.decide(many, st, 10000, T0), "invalid-share")
        # Exactly 20% of the returned units is not over it.
        edge = {"pages": 900, "pages_pending": 0, "units": 250, "pending": 0, "units_invalid": 50}
        self.assertIsNone(xp.decide(edge, st, 10000, T0))

    def test_no_return_for_48_hours_stops_it_and_a_return_resets_the_clock(self):
        s = {"pages": 500, "pages_pending": 500}
        state = {"started_at": T0.isoformat(), "last_returned": 0}
        self.assertIsNone(xp.decide(s, state, 10000, hours(47)))
        self.assertEqual(xp.decide(s, state, 10000, hours(49)), "no-return")
        state = {"started_at": T0.isoformat(), "last_return_at": hours(40).isoformat(), "last_returned": 5}
        s = {"pages": 505, "pages_pending": 500}
        self.assertIsNone(xp.decide(s, state, 10000, hours(60)))

    def test_a_stopped_pilot_collects_but_submits_nothing_and_keeps_its_reason(self):
        self.state.write_text(json.dumps({"started_at": T0.isoformat(), "next_chunk": 1,
                                          "stopped": {"reason": "no-return", "at": T0.isoformat()}}))
        fake = Fake({"pages": 10, "pages_pending": 0})
        rec = xp.step(self.a, hours(5), fake)
        self.assertEqual((rec["action"], rec["stopped"]), ("stopped", "no-return"))
        self.assertEqual(len(fake.cmds), 1)
        self.assertEqual(self.saved()["stopped"]["reason"], "no-return")

    def test_the_draw_is_made_once_and_only_with_a_dsn(self):
        # A draw cut short (pilot.csv, no campaign.json) is made again.
        (self.pilot / "campaign.json").unlink()
        with self.assertRaises(SystemExit):
            xp.step(self.a, T0, Fake({"pages": 0}))
        self.a.dsn_file = "/dsn"
        fake = Fake({"pages": 0})
        xp.step(self.a, T0, fake)
        self.assertTrue(fake.cmds[0][1].endswith("pilot_select.py"))
        self.assertEqual(fake.cmds[0][fake.cmds[0].index("--chunk") + 1], "500")

    def test_a_chunk_staged_but_not_submitted_resumes_without_reserving_again(self):
        class Flaky(Fake):
            def __call__(self, cmd):
                if "submit" in cmd and "--max-units" in cmd and not getattr(self, "ok", False):
                    self.cmds.append(cmd)
                    raise SystemExit("submit failed")
                return super().__call__(cmd)

        fake = Flaky({"pages": 0})
        with self.assertRaises(SystemExit):
            xp.step(self.a, T0, fake)
        self.assertEqual(self.saved()["active_chunk"], "members-000.json")
        self.assertEqual(self.saved()["next_chunk"], 0)
        # The staged chunk now counts in flight, and 600 + 500 would not fit: it still resumes.
        fake = Fake({"pages": 600, "pages_pending": 600})
        rec = xp.step(self.a, hours(1), fake)
        self.assertEqual(rec["action"], "submitted members-000.json")
        self.assertNotIn("active_chunk", self.saved())
        self.assertEqual(self.saved()["next_chunk"], 1)

    def test_a_chunk_with_units_lacking_a_receipt_is_not_advanced_or_replayed(self):
        class Uncertain(Fake):
            def __call__(self, cmd):
                if "submit" in cmd:
                    self.cmds.append(cmd)
                    return json.dumps({"submitted": 3, "uncertain": 2})
                return super().__call__(cmd)

        rec = xp.step(self.a, T0, Uncertain({"pages": 0}))
        self.assertEqual(rec["action"], "unresolved members-000.json")
        st = self.saved()
        self.assertEqual((st["next_chunk"], st["active_chunk"]), (0, "members-000.json"))
        self.assertEqual(st["unresolved"]["uncertain"], 2)
        # The next run collects and reports, and runs neither stage nor submit.
        fake = Fake({"pages": 10, "pages_pending": 10})
        rec = xp.step(self.a, hours(1), fake)
        self.assertEqual(rec["action"], "unresolved members-000.json")
        self.assertEqual(len(fake.cmds), 1)
        # No summary line at all counts as unresolved too.
        self.assertEqual(xp.submit_summary("models staged\n"), {"uncertain": -1})
        self.assertEqual(xp.submit_summary('{"submitted": 1, "uncertain": 0}\n')["uncertain"], 0)

    def test_main_writes_one_log_line_a_run(self):
        log = Path(self.tmp.name) / "log"
        orig = xp.run_cmd
        xp.run_cmd = Fake({"pages": 0})
        xp.step.__defaults__ = (None, xp.run_cmd)
        try:
            xp.main(["--state", str(self.state), "--pilot-dir", str(self.pilot), "--root", "r",
                     "--quality-root", "q", "--project", "p", "--spool", "s", "--crop-states", "c",
                     "--held", "h", "--engine-image", "i", "--engine-llama", "l", "--engine-weights", "w",
                     "--log", str(log)])
        finally:
            xp.run_cmd = orig
            xp.step.__defaults__ = (None, orig)
        lines = log.read_text().splitlines()
        self.assertEqual(len(lines), 1)
        self.assertEqual(json.loads(lines[0])["chunk"], 1)


if __name__ == "__main__":
    unittest.main()
