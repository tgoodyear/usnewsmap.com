from datetime import datetime
import importlib.util
import json
from pathlib import Path
import unittest
from zoneinfo import ZoneInfo


spec = importlib.util.spec_from_file_location("boinc_week_collect", Path(__file__).with_name("boinc-week-collect.py"))
collector = importlib.util.module_from_spec(spec)
spec.loader.exec_module(collector)

EASTERN = ZoneInfo("America/New_York")
# Wed 11 Nov 2026, 16:30 Eastern: the last bucket is 15:00 to 18:00.
NOW = int(datetime(2026, 11, 11, 16, 30, tzinfo=EASTERN).timestamp())
HOUR = 3600
LAST = int(datetime(2026, 11, 11, 15, tzinfo=EASTERN).timestamp())
OCR, QWEN = 1, 6
APPS = {str(OCR): "usnm_ocr", str(QWEN): "usnm_qwen", "9": "Not An App"}


def rows(**parts):
    return {"active": [], "apps": APPS, "completed": [], "entered": [], "left": [], **parts}


class EdgesTests(unittest.TestCase):
    def test_a_week_of_eastern_three_hour_buckets_ending_after_now(self):
        starts, end = collector.edges(NOW)
        self.assertEqual(len(starts), 56)
        self.assertEqual(starts[-1], LAST)
        self.assertEqual(end, LAST + 3 * HOUR)
        self.assertEqual(starts[0], int(datetime(2026, 11, 4, 18, tzinfo=EASTERN).timestamp()))
        for start in starts:
            self.assertEqual(datetime.fromtimestamp(start, EASTERN).hour % 3, 0)

    def test_buckets_stay_on_local_boundaries_across_the_clock_change(self):
        # Sun 1 Nov 2026, 02:00 EDT became 01:00 EST: the 00:00 bucket is four hours long.
        starts, _ = collector.edges(int(datetime(2026, 11, 3, 10, tzinfo=EASTERN).timestamp()))
        hours = [datetime.fromtimestamp(s, EASTERN).hour for s in starts]
        self.assertTrue(all(h % 3 == 0 for h in hours))
        self.assertIn(4 * HOUR, {b - a for a, b in zip(starts, starts[1:])})


class AggregateTests(unittest.TestCase):
    def test_queue_completed_and_active(self):
        before = collector.edges(NOW)[0][0] - HOUR
        doc = collector.aggregate(rows(
            # 100 queued before the window, 10 more an hour into the last full bucket, 30 sent in it.
            entered=[[QWEN, before, 100], [QWEN, LAST - 2 * HOUR, 10], [OCR, LAST + HOUR, 5]],
            left=[[QWEN, LAST - HOUR, 30]],
            completed=[[QWEN, LAST - 3 * HOUR, 7], [QWEN, LAST - HOUR, 4], [OCR, LAST - HOUR, 2], [9, LAST, 1]],
            active=[[LAST - 3 * HOUR, 2, 10], [LAST - 2 * HOUR, 2, 10], [LAST - 2 * HOUR, 40, 11],
                    [LAST - HOUR, 41, 12], [LAST - HOUR, 41, 13], [LAST, 3, 14]],
        ), NOW)
        self.assertEqual(doc["schema"], 1)
        self.assertEqual(doc["bucket_hours"], 3)
        buckets = doc["buckets"]
        self.assertEqual(len(buckets), 56)
        first, full, last = buckets[0], buckets[-2], buckets[-1]
        self.assertEqual(first["queued"], {"usnm_qwen": 100})
        self.assertEqual(full["queued"], {"usnm_qwen": 80})
        self.assertFalse(full["partial"])
        # The partial bucket's queue is now's.
        self.assertTrue(last["partial"])
        self.assertEqual(last["queued"], {"usnm_ocr": 5, "usnm_qwen": 80})
        self.assertEqual(full["completed"], {"usnm_ocr": 2, "usnm_qwen": 11})
        # An app that isn't a short name is left out.
        self.assertEqual(last["completed"], {})
        self.assertEqual((full["volunteers"], full["hosts"]), (3, 4))
        self.assertEqual((last["volunteers"], last["hosts"]), (1, 1))
        self.assertNotIn("accounts", full)
        self.assertNotIn("volunteer_hosts", full)
        self.assertEqual(full["utc_offset_minutes"], -300)
        self.assertEqual(doc["generated_utc_offset_minutes"], -300)
        # Counts and app names only.
        text = json.dumps(doc)
        for word in ("userid", "hostid", "Not An App"):
            self.assertNotIn(word, text)


if __name__ == "__main__":
    unittest.main()
