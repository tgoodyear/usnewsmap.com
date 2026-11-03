import importlib.util
import io
import json
from pathlib import Path
import contextlib
import unittest

ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location("feed_status", ROOT / "scripts/boinc/feed_status.py")
F = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(F)

NOW = 1_800_000_000


class SnapshotTest(unittest.TestCase):
    def snap(self, **overrides):
        args = dict(now=NOW, enabled_until=NOW + 24 * 3600, backlog_open=False,
                    seconds_since_workunit=0, unsent=50, idle_slots=10,
                    last_tick_ok=True, last_tick_seconds=60)
        args.update(overrides)
        return F.snapshot(**args)

    def test_healthy_snapshot_raises_nothing(self):
        s = self.snap()
        self.assertFalse(s["refill_expired_or_soon"])
        self.assertFalse(s["unsent_below_idle"])
        self.assertFalse(s["tick_failed_or_slow"])

    def test_refill_window_within_six_hours_or_expired(self):
        self.assertTrue(self.snap(enabled_until=NOW + 6 * 3600)["refill_expired_or_soon"])
        self.assertTrue(self.snap(enabled_until=NOW - 1)["refill_expired_or_soon"])
        self.assertFalse(self.snap(enabled_until=NOW + 6 * 3600 + 1)["refill_expired_or_soon"])

    def test_unsent_below_idle_slots(self):
        self.assertTrue(self.snap(unsent=9, idle_slots=10)["unsent_below_idle"])
        self.assertFalse(self.snap(unsent=10, idle_slots=10)["unsent_below_idle"])

    def test_tick_failure_or_over_fifteen_minutes(self):
        self.assertTrue(self.snap(last_tick_ok=False)["tick_failed_or_slow"])
        self.assertTrue(self.snap(last_tick_seconds=15 * 60 + 1)["tick_failed_or_slow"])
        self.assertFalse(self.snap(last_tick_seconds=15 * 60)["tick_failed_or_slow"])

    def test_backlog_fields_pass_through(self):
        s = self.snap(backlog_open=True, seconds_since_workunit=3700)
        self.assertTrue(s["backlog_open"])
        self.assertEqual(s["seconds_since_workunit"], 3700)


class MainTest(unittest.TestCase):
    def test_prints_one_json_line_and_sends_nothing(self):
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            code = F.main(["--enabled-until", str(NOW + 3600), "--seconds-since-workunit", "0",
                           "--unsent", "3", "--idle-slots", "8", "--last-tick-ok",
                           "--last-tick-seconds", "30", "--now", str(NOW)])
        self.assertEqual(code, 0)
        lines = out.getvalue().splitlines()
        self.assertEqual(len(lines), 1)
        record = json.loads(lines[0])
        self.assertTrue(record["refill_expired_or_soon"])
        self.assertTrue(record["unsent_below_idle"])
        self.assertFalse(record["tick_failed_or_slow"])


if __name__ == "__main__":
    unittest.main()
