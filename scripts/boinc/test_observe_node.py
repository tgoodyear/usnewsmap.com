import importlib.util
from datetime import datetime, timezone
import json
from pathlib import Path
import unittest
from unittest.mock import patch

SPEC = importlib.util.spec_from_file_location("observe_node", Path(__file__).with_name("observe_node.py"))
observe = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(observe)


class ObserveNodeTests(unittest.TestCase):
    def test_delayed_journal_query_keeps_error_at_start_of_advertised_window(self):
        now = datetime(2026, 10, 16, 12, 0, 0, 500000, tzinfo=timezone.utc)
        failure = json.dumps({"timestamp": "2026-10-16T11:45:01Z", "level": "ERROR",
                              "fields": {"message": "delete failed; the page will be redone"}})
        with patch.object(observe.subprocess, "check_output", return_value=failure) as query:
            logs = observe.journal("worker.service", now, 900)
        args = query.call_args.args[0]
        self.assertEqual(args[args.index("--since") + 1], f"@{int(now.timestamp() - 900)}")
        self.assertFalse(any("ago" in arg for arg in args))
        self.assertEqual(observe.summarize(logs.splitlines(), now, 900)["error_events"], 1)

    def test_actual_worker_failures_never_pass_even_with_recent_successes(self):
        now = datetime(2026, 10, 16, 12, tzinfo=timezone.utc)
        successes = [json.dumps({"timestamp": "2026-10-16T11:59:00Z", "fields": {
            "message": "page done", "status": "ok", "secs": 60}})] * 10
        for level, message in (
            ("WARN", "page failed; it reappears when its visibility runs out"),
            ("WARN", "page abandoned: out of time, nothing written; it reappears for another node"),
            ("WARN", "receive failed"),
            ("ERROR", "delete failed; the page will be redone"),
            ("ERROR", "unanticipated failure"),
            ("WARN", "message was gone at delete; another node may redo it"),
        ):
            failure = json.dumps({"timestamp": "2026-10-16T11:59:30Z", "level": level,
                                  "fields": {"message": message}})
            throughput = observe.summarize([*successes, failure], now, 900)
            self.assertEqual(throughput["successful_pages"], 10)
            self.assertEqual(throughput["error_events"], 1)
            self.assertFalse(observe.baseline_ready({
                "worker": {"ActiveState": "active"}, "throughput": throughput}))
        failed_page = json.dumps({"timestamp": "2026-10-16T11:59:30Z", "level": "ERROR",
                                  "fields": {"message": "page done", "status": "failed"}})
        self.assertEqual(observe.summarize([failed_page], now, 900)["error_events"], 1)

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
