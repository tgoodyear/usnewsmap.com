import importlib.util
import json
from pathlib import Path
import tempfile
import unittest
from datetime import datetime, timezone
from unittest.mock import patch

spec = importlib.util.spec_from_file_location("observer", Path(__file__).with_name("data_flow_observer.py"))
observer = importlib.util.module_from_spec(spec)
spec.loader.exec_module(observer)
NOW = datetime(2026, 10, 16, 20, tzinfo=timezone.utc)
XML = b"""<server_status><database_file_states>
<results_ready_to_send>12</results_ready_to_send><results_in_progress>3</results_in_progress>
<tasks_by_app><app><name>usnm_ocr</name><unsent>2</unsent><in_progress>1</in_progress></app>
</tasks_by_app></database_file_states></server_status>"""


class Observer(unittest.TestCase):
    def local(self):
        return {"fetch": {"status": {"updated_at": "2026-10-16T19:55:00Z"},
                          "sets": [{"name": "keep", "fetched": 10, "total": 30}]},
                "nodes": [{"name": "node-3", "ok": True, "last_ok": "2026-10-16T19:59:00Z",
                           "pages_per_hour": 4},
                          {"name": "node-4", "ok": False, "pages_per_hour": 900}]}

    def test_local_counts_do_not_include_retired_or_missing_workers(self):
        stages = observer.local_stages(self.local(), NOW)
        self.assertEqual([item["count"] for item in stages], [10, 30, 20, 4])
        self.assertEqual(stages[0]["observed_at"], "2026-10-16T19:55:00Z")

    def test_malformed_local_counts_and_time_fail(self):
        for key, value in [("fetched", 31), ("fetched", True), ("total", -1)]:
            payload = self.local()
            payload["fetch"]["sets"][0][key] = value
            with self.assertRaises(ValueError):
                observer.local_stages(payload, NOW)
        payload = self.local()
        payload["fetch"]["status"]["updated_at"] = "2026-10-17T00:00:00Z"
        with self.assertRaises(ValueError):
            observer.local_stages(payload, NOW)

    def test_boinc_counts_are_replicas_not_eligible_pages(self):
        stages = observer.boinc_stages(XML, NOW)
        self.assertEqual([item["count"] for item in stages], [12, 3, 2, 1])
        self.assertIn("cache age unavailable", stages[0]["detail"])

    def test_bad_xml_and_missing_counters_fail(self):
        for body in [b"<html/>", b"<!DOCTYPE x><server_status/>",
                     XML.replace(b"<unsent>2</unsent>", b""),
                     XML.replace(b"<unsent>2</unsent>", b"<unsent>-1</unsent>"),
                     b"x" * (observer.MAX_BYTES + 1)]:
            with self.subTest(body=body[:20]), self.assertRaises(ValueError):
                observer.boinc_stages(body, NOW)

    def test_failures_remain_explicit_and_other_source_survives(self):
        with patch.object(observer, "fetch", side_effect=[OSError("private diagnostics"), XML]):
            events, failures = observer.collect("http://local/", "https://project/", NOW)
        self.assertEqual(len(events), 4)
        self.assertEqual(failures[0]["code"], "local-ocr-unavailable")
        self.assertNotIn("private diagnostics", json.dumps(failures))

    def test_empty_sources_never_become_fresh_or_accepted(self):
        value = observer.snapshot([], [], NOW)
        self.assertIsNone(value["observed_at"])
        self.assertTrue(all(item["count"] is None for item in value["stages"]))

    def test_source_age_survives_new_generation(self):
        events = [{"source": "local", "stage": stage}
                  for stage in observer.local_stages(self.local(), NOW)]
        value = observer.snapshot(events, [], NOW)
        self.assertEqual(value["observed_at"], "2026-10-16T19:55:00Z")
        self.assertEqual(value["generated_at"], "2026-10-16T20:00:00Z")
        self.assertIsNone(next(s for s in value["stages"] if s["id"] == "indexed")["count"])

    def test_atomic_snapshot_replaces_and_leaves_no_temporary_file(self):
        with tempfile.TemporaryDirectory() as directory:
            target = Path(directory) / "pipeline.json"
            observer.atomic_json(target, observer.snapshot([], [], NOW))
            self.assertIsNone(json.loads(target.read_text())["observed_at"])
            self.assertEqual([p.name for p in target.parent.iterdir()], ["pipeline.json"])

    def test_credentials_rejected_before_network(self):
        for url in ["https://user:secret@example.org/", "https://example.org/?unexpected=1",
                    "file:///etc/passwd"]:
            with self.assertRaises(ValueError):
                observer.fetch(url)


if __name__ == "__main__":
    unittest.main()
