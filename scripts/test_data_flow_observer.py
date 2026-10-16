import importlib.util
import json
from pathlib import Path
import tempfile
import unittest
from datetime import datetime, timezone
from http.client import IncompleteRead
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
                     "<!DOCTYPE x><server_status/>".encode("utf-16"),
                     XML.replace(b"<unsent>2</unsent>", b""),
                     XML.replace(b"<unsent>2</unsent>", b"<unsent>-1</unsent>"),
                     b"x" * (observer.MAX_BYTES + 1)]:
            with self.subTest(body=body[:20]), self.assertRaises(ValueError):
                observer.boinc_stages(body, NOW)

    def test_timestamp_range_errors_preserve_other_source(self):
        payload = self.local()
        payload["fetch"]["status"]["updated_at"] = "0001-01-01T00:00:00+01:00"
        with patch.object(observer, "fetch", side_effect=[json.dumps(payload).encode(), XML]):
            events, failures = observer.collect("http://local/", "https://project/", NOW)
        self.assertEqual(len(events), 4)
        self.assertEqual(failures[0]["code"], "local-ocr-unavailable")

    def test_failures_remain_explicit_and_other_source_survives(self):
        with patch.object(observer, "fetch", side_effect=[OSError("private diagnostics"), XML]):
            events, failures = observer.collect("http://local/", "https://project/", NOW)
        self.assertEqual(len(events), 4)
        self.assertEqual(failures[0]["code"], "local-ocr-unavailable")
        self.assertNotIn("private diagnostics", json.dumps(failures))

    def test_truncated_http_and_deep_json_preserve_healthy_source(self):
        for broken in [IncompleteRead(b"truncated", 100), b"[" * 2000 + b"]" * 2000]:
            with self.subTest(broken=type(broken)), patch.object(
                observer, "fetch", side_effect=[broken, XML]
            ):
                events, failures = observer.collect("http://local/", "https://project/", NOW)
            self.assertEqual(len(events), 4)
            self.assertEqual(failures[0]["code"], "local-ocr-unavailable")

    def test_dashboard_counter_limit_is_inclusive_and_oversize_is_source_isolated(self):
        self.assertEqual(observer.count(10**12), 10**12)
        payload = self.local()
        payload["fetch"]["sets"][0].update(fetched=10**12 + 1, total=10**12 + 1)
        with patch.object(observer, "fetch", side_effect=[json.dumps(payload).encode(), XML]):
            events, failures = observer.collect("http://local/", "https://project/", NOW)
        self.assertEqual(len(events), 4)
        self.assertEqual(failures[0]["code"], "local-ocr-unavailable")
        value = observer.snapshot(events, failures, NOW)
        self.assertTrue(all(stage["count"] is None or stage["count"] <= 10**12
                            for stage in value["stages"]))

    def test_wrong_local_json_shapes_cannot_suppress_boinc_observation(self):
        for malformed in [{"fetch": None}, {"fetch": []},
                          {"fetch": {"status": []}}, {"fetch": {"status": {"updated_at": "2026-10-16T19:55:00Z"},
                                                                 "sets": [None]}}]:
            with self.subTest(payload=malformed), patch.object(
                observer, "fetch", side_effect=[json.dumps(malformed).encode(), XML]
            ):
                events, failures = observer.collect("http://local/", "https://project/", NOW)
            self.assertEqual(len(events), 4)
            self.assertEqual(failures[0]["code"], "local-ocr-unavailable")

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

    def test_catalog_counts_are_scoped_and_keep_their_own_age(self):
        catalog = observer.snapshot([], [], NOW)
        catalog.update(observed_at="2026-10-15T12:00:00Z",
                       runs={"total": 4, "pages": 2},
                       artifacts={"locations": 10, "verified": 1, "missing": 2},
                       coverage=["Bounded local sample; not a full inventory"],
                       issues=[{"code": "size_mismatch", "detail": "One local file has an unexpected size."}],
                       stages=[
                           observer.stage("runs", "Runs", 2, "2026-10-14T12:00:00Z", ""),
                           observer.stage("artifacts", "Artifacts", 3, "2026-10-15T12:00:00Z", ""),
                       ])
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "catalog.json"
            path.write_text(json.dumps(catalog))
            value = observer.add_catalog(observer.snapshot([], [], NOW), path, NOW)
        self.assertEqual(value["runs"]["total"], 4)
        item = next(s for s in value["stages"] if s["id"] == "run_history")
        self.assertEqual(item["observed_at"], "2026-10-14T12:00:00Z")
        self.assertEqual(value["observed_at"], "2026-10-14T12:00:00Z")
        self.assertIsNone(next(s for s in value["stages"] if s["id"] == "accepted")["count"])
        self.assertIn("partial", value["coverage"][0])
        self.assertIn("Bounded local sample; not a full inventory", value["coverage"])
        self.assertIn("size_mismatch", [issue["code"] for issue in value["issues"]])
        self.assertEqual(value["artifacts"]["locations"], 10)
        self.assertEqual(next(s for s in value["stages"] if s["id"] == "artifact_verified")["count"], 1)

    def test_combined_inventory_retains_mandatory_stages_within_dashboard_budget(self):
        apps = b"".join(
            f"<app><name>usnm_app{index}</name><unsent>2</unsent><in_progress>1</in_progress></app>".encode()
            for index in range(28))
        body = XML.replace(XML[XML.index(b"<app>"):XML.index(b"</app>") + 6], apps)
        events = [{"source": "local", "stage": item}
                  for item in observer.local_stages(self.local(), NOW)]
        events += [{"source": "boinc", "stage": item} for item in observer.boinc_stages(body, NOW)]
        value = observer.snapshot(events, [], NOW)
        self.assertEqual(len(value["stages"]), 64)
        for identifier in ("boinc_unsent", "boinc_in_progress", "scan_cached",
                           "scan_planned", "scan_remaining", "accepted", "artifact_verified"):
            self.assertIn(identifier, [item["id"] for item in value["stages"]])
        self.assertIn("stage-detail-truncated", [issue["code"] for issue in value["issues"]])

    def test_runs_only_catalog_does_not_turn_unknown_artifacts_into_zero(self):
        catalog = observer.snapshot([], [], NOW)
        catalog.update(observed_at="2026-10-16T19:00:00Z",
                       runs={"total": 1, "pages": 1},
                       stages=[
                           observer.stage("runs", "Runs", 1, "2026-10-15T12:00:00Z", ""),
                           {"id": "artifacts", "state": "unknown", "count": None, "observed_at": None},
                       ])
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "catalog.json"
            path.write_text(json.dumps(catalog))
            value = observer.add_catalog(observer.snapshot([], [], NOW), path, NOW)
        artifact = next(s for s in value["stages"] if s["id"] == "artifact_verified")
        self.assertEqual(artifact["state"], "unknown")
        self.assertIsNone(artifact["count"])
        self.assertIsNone(artifact["observed_at"])
        self.assertEqual(next(s for s in value["stages"] if s["id"] == "run_history")["count"], 1)

    def test_catalog_cannot_smuggle_inconsistent_or_untimestamped_counts(self):
        catalog = observer.snapshot([], [], NOW)
        for change in [
            {"runs": {"total": 1, "pages": 2}},
            {"artifacts": {"locations": 1, "verified": 2, "missing": 0}},
            {"runs": {"total": 1, "pages": 1}},
        ]:
            with tempfile.TemporaryDirectory() as directory:
                path = Path(directory) / "catalog.json"
                path.write_text(json.dumps({**catalog, **change}))
                with self.assertRaises(ValueError):
                    observer.add_catalog(observer.snapshot([], [], NOW), path, NOW)

    def test_invalid_catalog_issue_leaves_live_snapshot_unchanged(self):
        catalog = observer.snapshot([], [], NOW)
        catalog["issues"] = [{"code": "bad\ncode", "detail": "Invalid control character"}]
        value = observer.snapshot([], [], NOW)
        original = json.dumps(value)
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "catalog.json"
            path.write_text(json.dumps(catalog))
            with self.assertRaises(ValueError):
                observer.add_catalog(value, path, NOW)
        self.assertEqual(json.dumps(value), original)

    def test_join_caps_coverage_and_issues_with_explicit_omission(self):
        catalog = observer.snapshot([], [], NOW)
        catalog["coverage"] = [f"Coverage {index}" for index in range(32)]
        catalog["issues"] = [{"code": "sample", "detail": f"Issue {index}"} for index in range(128)]
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "catalog.json"
            path.write_text(json.dumps(catalog))
            value = observer.add_catalog(observer.snapshot([], [], NOW), path, NOW)
        self.assertEqual(len(value["coverage"]), 32)
        self.assertIn("omitted", value["coverage"][-1])
        self.assertEqual(len(value["issues"]), 128)
        self.assertEqual(value["issues"][-1]["code"], "catalog-issues-truncated")


if __name__ == "__main__":
    unittest.main()
