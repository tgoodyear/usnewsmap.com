import importlib.util
from datetime import datetime, timezone
import json
from pathlib import Path
import unittest

SPEC = importlib.util.spec_from_file_location("observe_node", Path(__file__).with_name("observe_node.py"))
observe = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(observe)


class ObserveNodeTests(unittest.TestCase):
    def test_real_window_counts_success_not_deadletters_and_ignores_other_payloads(self):
        now = datetime(2026, 10, 16, 12, tzinfo=timezone.utc)
        def event(time, **fields):
            return json.dumps({"timestamp": "2026-10-16T" + time + "Z", "fields": fields})
        report = observe.summarize([
            event("11:50:00", message="page done", status="ok", secs=60, queue="ocr-bulk",
                  language="deu", engine="pinned"),
            event("11:59:00", message="page done", status="ok", secs=90, queue="ocr-bulk",
                  language="deu", engine="pinned"),
            event("11:44:00", message="page done", status="ok", secs=200),
            event("11:58:00", message="dead-lettered"),
            event("12:01:00", message="page done", status="ok", secs=30),
            "not json", "5", "{}",
        ], now, 900)
        self.assertEqual(report["successful_pages"], 2)
        self.assertEqual(report["pages_per_hour"], 8)
        self.assertEqual(report["median_page_seconds"], 75)
        self.assertEqual(report["last_success_age_seconds"], 60)
        self.assertEqual(report["error_events"], 1)
        self.assertEqual(report["workload"], [("ocr-bulk", "deu", "pinned")])

    def test_no_recent_completed_pages_is_not_ready_even_with_a_live_process(self):
        report = {"worker": {"ActiveState": "active"}, "throughput": {
            "successful_pages": 20, "error_events": 0, "last_success_age_seconds": 1000}}
        self.assertFalse(observe.baseline_ready(report))
        report["throughput"]["last_success_age_seconds"] = 10
        self.assertTrue(observe.baseline_ready(report))
        report["throughput"]["error_events"] = 1
        self.assertFalse(observe.baseline_ready(report))
        report["throughput"].update(error_events=0, last_success_age_seconds=None)
        self.assertFalse(observe.baseline_ready(report))


if __name__ == "__main__":
    unittest.main()
