import contextlib
import importlib.util
import io
import json
from pathlib import Path
import re
import tempfile
import unittest
from datetime import datetime, timedelta, timezone
from http.client import IncompleteRead
from unittest.mock import patch

spec = importlib.util.spec_from_file_location("observer", Path(__file__).with_name("data_flow_observer.py"))
observer = importlib.util.module_from_spec(spec)
spec.loader.exec_module(observer)
NOW = datetime(2026, 10, 16, 20, tzinfo=timezone.utc)

def ago(**delta):
    return observer.utc(NOW - timedelta(**delta))


XML = b"""<server_status><database_file_states>
<results_ready_to_send>12</results_ready_to_send><results_in_progress>3</results_in_progress>
<tasks_by_app><app><name>usnm_ocr</name><unsent>2</unsent><in_progress>1</in_progress></app>
</tasks_by_app></database_file_states></server_status>"""


class Observer(unittest.TestCase):
    def local(self):
        return {"generated_at": "2026-10-16T19:55:00Z",
                "fetch": {"queue_configured": True, "total": {"pages": 50, "bytes": 1},
                          "queue_summary": {"pages": 30, "pages_fetched": 10, "pages_left": 20}},
                "nodes": [{"name": "node-3", "ok": True, "last_ok": "2026-10-16T19:59:00Z",
                           "pages_per_hour": 4},
                          {"name": "node-4", "ok": False, "pages_per_hour": 900}]}

    def test_local_counts_do_not_include_retired_or_missing_workers(self):
        stages = observer.local_stages(self.local(), NOW)
        self.assertEqual([item["count"] for item in stages], [10, 30, 20, 50, 4])
        self.assertEqual(stages[0]["observed_at"], "2026-10-16T19:55:00Z")

    def test_malformed_local_counts_and_time_fail(self):
        for key, value in [("pages_fetched", 31), ("pages_fetched", True), ("pages", -1),
                           ("pages_left", 31)]:
            payload = self.local()
            payload["fetch"]["queue_summary"][key] = value
            with self.assertRaises(ValueError):
                observer.local_stages(payload, NOW)
        payload = self.local()
        payload["generated_at"] = "2026-10-17T00:00:00Z"
        with self.assertRaises(ValueError):
            observer.local_stages(payload, NOW)
        for key, value in [("queue_configured", False), ("index_error", "unreadable"),
                           ("queue_error", "unreadable")]:
            payload = self.local()
            payload["fetch"][key] = value
            with self.assertRaises(ValueError):
                observer.local_stages(payload, NOW)

    def status(self):
        return {"generated_at": "2026-10-16T19:58:00Z", "stale": False, "error": None,
                "published": {"index_version": "pages-v20261022-1",
                              "published_at": "2026-10-16T05:19:12Z", "pages": 1000,
                              "sources": {"ocr_local": {"pages": 197}}}}

    def test_published_ocr_pages_are_the_indexed_stage(self):
        (item,) = observer.published_stages(json.dumps(self.status()).encode(), NOW)
        self.assertEqual((item["id"], item["count"], item["observed_at"]),
                         ("indexed", 197, "2026-10-16T19:58:00Z"))
        self.assertIn("pages-v20261022-1", item["detail"])
        value = observer.snapshot([{"source": "published-index", "stage": item}], [], NOW)
        (indexed,) = [s for s in value["stages"] if s["id"] == "indexed"]
        self.assertEqual(indexed["count"], 197)
        # A pipeline-store failure doesn't hide the serving index's own count.
        (item,) = observer.published_stages(
            json.dumps({**self.status(), "stale": True, "error": "store"}).encode(), NOW)
        self.assertEqual(item["count"], 197)
        with self.assertRaises(ValueError):
            observer.published_stages(
                json.dumps({**self.status(), "generated_at": "2027-01-01T00:00:00Z"}).encode(), NOW)
        empty = self.status()
        empty["published"]["sources"]["ocr_local"] = None
        (item,) = observer.published_stages(json.dumps(empty).encode(), NOW)
        self.assertEqual(item["count"], 0)
        bad = self.status()
        bad["published"]["sources"]["ocr_local"]["pages"] = 1001
        with self.assertRaises(ValueError):
            observer.published_stages(json.dumps(bad).encode(), NOW)

    def test_published_index_failure_leaves_other_sources(self):
        with patch.object(observer, "fetch",
                          side_effect=[json.dumps(self.local()).encode(), XML, OSError("x")]):
            events, failures = observer.collect("http://local/", "https://project/", NOW,
                                                api_status_url="https://api/")
        self.assertEqual(len(events), 9)
        self.assertEqual([f["code"] for f in failures], ["published-index-unavailable"])
        value = observer.snapshot(events, failures, NOW)
        (indexed,) = [s for s in value["stages"] if s["id"] == "indexed"]
        self.assertIsNone(indexed["count"])

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
        payload["generated_at"] = "0001-01-01T00:00:00+01:00"
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
        payload["fetch"]["queue_summary"].update(pages_fetched=10**12 + 1, pages=10**12 + 1)
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

    def live_catalog(self, **stages):
        """A catalog holding `stages` as {id: (count, observed_at, extra fields)}."""
        catalog = observer.snapshot([], [], NOW)
        rows = []
        for identifier, (amount, at, extra) in stages.items():
            rows.append({"id": identifier, "label": identifier, "count": amount, "observed_at": at,
                         "state": "observed", "detail": "From a test.", **extra})
        catalog.update(observed_at=max(row["observed_at"] for row in rows), stages=rows)
        return catalog

    def join_live(self, catalog):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "catalog.json"
            path.write_text(json.dumps(catalog))
            return observer.add_catalog(observer.snapshot([], [], NOW), path, NOW)

    def test_live_rows_take_aggregates_and_carry_their_budget(self):
        value = self.join_live(self.live_catalog(live_queued=(1005, ago(hours=1), {}),
                                              live_allocated=(225, ago(hours=1), {}),
                                              live_imported=(71, ago(hours=1), {}),
                                              imported=(201, ago(days=15), {"historical": True})))
        rows = {item["id"]: item for item in value["stages"]}
        self.assertEqual((rows["queued"]["count"], rows["queued"]["state"]), (1005, "observed"))
        self.assertEqual(rows["allocated"]["max_age_secs"], observer.LIVE_MAX_AGE_SECS)
        # A live import count replaces the one-page proof, and isn't historical for having had one.
        self.assertEqual(rows["imported"]["count"], 71)
        self.assertNotIn("historical", rows["imported"])
        self.assertEqual(value["observed_at"], ago(hours=1))

    def test_a_row_without_live_evidence_stays_unknown_not_inferred(self):
        value = self.join_live(self.live_catalog(live_imported=(71, ago(hours=1), {})))
        rows = {item["id"]: item for item in value["stages"]}
        for identifier in ("queued", "allocated", "collected", "validated", "quality_loaded"):
            self.assertEqual((rows[identifier]["state"], rows[identifier]["count"]), ("unknown", None))
        self.assertEqual((rows["accepted"]["state"], rows["accepted"]["count"]), ("unknown", None))

    def test_historical_stage_leaves_the_overall_age_but_keeps_its_own(self):
        value = self.join_live(self.live_catalog(
            live_imported=(71, ago(hours=1), {}),
            accepted=(1, ago(days=8), {"historical": True}),
            runs=(0, ago(days=15), {"historical": True})))
        rows = {item["id"]: item for item in value["stages"]}
        self.assertIs(rows["accepted"]["historical"], True)
        self.assertEqual(rows["accepted"]["observed_at"], ago(days=8))
        self.assertEqual(value["observed_at"], ago(hours=1))
        issue = next(i for i in value["issues"] if i["code"] == "historical-stages")
        self.assertIn("accepted, run_history", issue["detail"])

    def test_a_stopped_live_stage_still_pins_the_overall_age(self):
        value = self.join_live(self.live_catalog(live_imported=(71, ago(days=2), {}),
                                              accepted=(1, ago(days=8), {"historical": True})))
        self.assertEqual(value["observed_at"], ago(days=2))

    def test_a_stage_without_the_historical_mark_pins_the_overall_age(self):
        value = self.join_live(self.live_catalog(accepted=(1, ago(days=8), {}),
                                              live_imported=(71, ago(hours=1), {})))
        self.assertEqual(value["observed_at"], ago(days=8))

    def test_catalog_cannot_mark_undated_or_nonboolean_evidence_historical(self):
        for mark in ("yes", 1):
            with self.assertRaises(ValueError):
                self.join_live(self.live_catalog(accepted=(1, ago(days=8), {"historical": mark})))
        catalog = self.live_catalog(live_imported=(71, ago(hours=1), {}))
        catalog["stages"].append({"id": "accepted", "state": "unknown", "count": None,
                                  "observed_at": None, "detail": "None.", "historical": True})
        with self.assertRaises(ValueError):
            self.join_live(catalog)

    def test_a_full_issue_list_keeps_the_dashboards_limit_with_historical_stages(self):
        catalog = self.live_catalog(accepted=(1, ago(days=8), {"historical": True}))
        catalog["issues"] = [{"code": f"issue-{index}", "detail": "Detail."} for index in range(128)]
        self.assertLessEqual(len(self.join_live(catalog)["issues"]), observer.MAX_ISSUES)

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

    def test_explicit_import_proof_is_joined_without_inventing_acceptance_or_indexing(self):
        catalog = observer.snapshot([], [], NOW)
        catalog["stages"] = [
            {"id": "imported", "state": "observed", "count": 1,
             "observed_at": "2026-10-15T12:00:00Z",
             "detail": "One independently traced committed import; partial coverage."}
        ]
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "catalog.json"
            path.write_text(json.dumps(catalog))
            value = observer.add_catalog(observer.snapshot([], [], NOW), path, NOW)
        stages = {item["id"]: item for item in value["stages"]}
        self.assertEqual(stages["imported"]["count"], 1)
        self.assertEqual(stages["imported"]["observed_at"], "2026-10-15T12:00:00Z")
        self.assertIn("partial", stages["imported"]["detail"])
        self.assertIsNone(stages["accepted"]["count"])
        self.assertIsNone(stages["indexed"]["count"])

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

    def joined(self, max_age_days=None, **change):
        """Join a catalog snapshot with the given fields changed, as the observer does."""
        catalog = observer.snapshot([], [], NOW)
        catalog.update(change)
        options = {} if max_age_days is None else {"max_age_days": max_age_days}
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "catalog.json"
            path.write_text(json.dumps(catalog))
            return observer.add_catalog(observer.snapshot([], [], NOW), path, NOW, **options)

    @staticmethod
    def frozen(value):
        return [issue for issue in value["issues"] if issue["code"] == "catalog-frozen"]

    def test_catalog_within_the_age_limit_is_not_flagged_frozen(self):
        # The existing tests keep a snapshot one to two days behind its newest evidence.
        for generated, newest in [(ago(), ago(days=2)), (ago(days=1), ago(days=2)), (ago(days=3), ago(days=3))]:
            with self.subTest(generated=generated, newest=newest):
                value = self.joined(generated_at=generated, observed_at=newest)
                self.assertEqual(self.frozen(value), [])

    def test_catalog_exactly_at_the_limit_is_not_frozen_but_a_second_past_it_is(self):
        self.assertEqual(self.frozen(self.joined(generated_at=ago(days=3))), [])
        self.assertEqual(self.frozen(self.joined(observed_at=ago(days=3))), [])
        self.assertEqual(len(self.frozen(self.joined(generated_at=ago(days=3, seconds=1)))), 1)
        self.assertEqual(len(self.frozen(self.joined(observed_at=ago(days=3, seconds=1)))), 1)

    def test_old_catalog_is_flagged_frozen_with_its_ages_and_still_joined(self):
        # The shape of the audited snapshot: generated about five days ago, evidence about eight.
        value = self.joined(
            generated_at=ago(days=5), observed_at=ago(days=8),
            runs={"total": 4, "pages": 2},
            artifacts={"locations": 10, "verified": 1, "missing": 2},
            stages=[observer.stage("runs", "Runs", 2, ago(days=8), ""),
                    observer.stage("artifacts", "Artifacts", 3, ago(days=8), "")])
        (issue,) = self.frozen(value)
        self.assertIn(f"generated {ago(days=5)} (5 days old)", issue["detail"])
        self.assertIn(f"newest evidence observed {ago(days=8)} (8 days old)", issue["detail"])
        self.assertIn("limit is 3 days", issue["detail"])
        self.assertLessEqual(len(issue["detail"]), 512)
        self.assertEqual(observer.bounded_text(issue["detail"], 512), issue["detail"])
        self.assertLessEqual(len(issue["code"]), 64)
        self.assertEqual(value["runs"], {"total": 4, "pages": 2})
        self.assertEqual(next(s for s in value["stages"] if s["id"] == "run_history")["count"], 4)
        self.assertEqual(value["observed_at"], ago(days=8))
        self.assertIn("partial-catalog", [item["code"] for item in value["issues"]])

    def test_either_timestamp_alone_freezes_the_catalog(self):
        stale_snapshot = self.joined(generated_at=ago(days=5))
        (issue,) = self.frozen(stale_snapshot)
        self.assertIn("(5 days old)", issue["detail"])
        self.assertNotIn("newest evidence", issue["detail"])
        stale_evidence = self.joined(observed_at=ago(days=8))
        (issue,) = self.frozen(stale_evidence)
        self.assertIn("generated 2026-10-16T20:00:00Z (0 days old)", issue["detail"])
        self.assertIn("newest evidence observed 2026-10-08T20:00:00Z (8 days old)", issue["detail"])

    def test_age_limit_is_configurable(self):
        eight_days = {"generated_at": ago(days=5), "observed_at": ago(days=8)}
        self.assertEqual(self.frozen(self.joined(max_age_days=10, **eight_days)), [])
        self.assertEqual(len(self.frozen(self.joined(max_age_days=7, **eight_days))), 1)
        (issue,) = self.frozen(self.joined(max_age_days=1, generated_at=ago(), observed_at=ago(days=2)))
        self.assertIn("limit is 1 day,", issue["detail"])
        self.assertIn("(2 days old)", issue["detail"])

    def test_frozen_flag_survives_a_full_catalog_issue_list(self):
        # The dashboard rejects a snapshot with more than 128 issues. The join already holds
        # partial-catalog and catalog-frozen, so 126 catalog issues fit and more are cut.
        for count, truncated in [(126, False), (127, True), (128, True)]:
            with self.subTest(catalog_issues=count):
                value = self.joined(
                    generated_at=ago(days=5),
                    issues=[{"code": "sample", "detail": f"Issue {index}"} for index in range(count)])
                codes = [issue["code"] for issue in value["issues"]]
                self.assertEqual(len(value["issues"]), 128)
                self.assertEqual(codes.count("catalog-frozen"), 1)
                self.assertEqual(codes.count("partial-catalog"), 1)
                self.assertEqual("catalog-issues-truncated" in codes, truncated)
                self.assertEqual(codes[-1] == "catalog-issues-truncated", truncated)

    def test_future_catalog_timestamps_are_rejected_not_flagged_frozen(self):
        for field in ("generated_at", "observed_at"):
            with self.subTest(field=field), self.assertRaisesRegex(ValueError, "Future"):
                self.joined(**{field: observer.utc(NOW + timedelta(hours=2))})
        # A clock skew inside the existing 60 second tolerance is accepted, and is not frozen.
        value = self.joined(generated_at=observer.utc(NOW + timedelta(seconds=30)))
        self.assertEqual(self.frozen(value), [])

    def run_main(self, *args, catalog=None):
        """Run main() against a catalog snapshot, with the two network sources stubbed out."""
        with tempfile.TemporaryDirectory() as directory:
            target = Path(directory) / "pipeline.json"
            argv = ["observer", "--ocr-url", "http://local/", "--snapshot", str(target), *args]
            if catalog is not None:
                source = Path(directory) / "catalog.json"
                source.write_text(json.dumps(catalog))
                argv += ["--catalog-snapshot", str(source)]
            output = io.StringIO()
            with patch.object(observer, "collect", return_value=([], [])), \
                    patch("sys.argv", argv), contextlib.redirect_stdout(output):
                status = observer.main()
            return status, json.loads(target.read_text()), json.loads(output.getvalue())

    def test_main_reports_a_frozen_catalog_without_failing_the_run(self):
        # Fixed old dates, so the real clock main() reads makes them older whenever this runs.
        catalog = {**observer.snapshot([], [], NOW), "generated_at": "2020-01-02T00:00:00Z",
                   "observed_at": "2020-01-01T00:00:00Z"}
        status, value, event = self.run_main(catalog=catalog)
        self.assertEqual((status, event["failed_sources"]), (0, 0))
        codes = [issue["code"] for issue in value["issues"]]
        self.assertIn("catalog-frozen", codes)
        self.assertNotIn("catalog-unavailable", codes)
        # The flag reaches the check: a six-year-old snapshot is still frozen at the 365 day maximum.
        _, value, _ = self.run_main("--catalog-max-age-days", "365", catalog=catalog)
        (issue,) = self.frozen(value)
        self.assertIn("limit is 365 days,", issue["detail"])

    def test_main_future_catalog_is_unavailable_not_frozen(self):
        catalog = {**observer.snapshot([], [], NOW), "generated_at": "2999-01-01T00:00:00Z"}
        status, value, event = self.run_main(catalog=catalog)
        self.assertEqual((status, event["failed_sources"]), (1, 1))
        codes = [issue["code"] for issue in value["issues"]]
        self.assertIn("catalog-unavailable", codes)
        self.assertNotIn("catalog-frozen", codes)

    def test_main_rejects_an_unusable_age_limit(self):
        for limit in ["0", "-1", "366", "three", "1.5"]:
            with self.subTest(limit=limit), contextlib.redirect_stderr(io.StringIO()), \
                    patch("sys.argv", ["observer", "--ocr-url", "http://local/", "--snapshot", "x",
                                       "--catalog-max-age-days", limit]):
                with self.assertRaises(SystemExit) as raised:
                    observer.main()
            self.assertEqual(raised.exception.code, 2)


class RetirementProcedure(unittest.TestCase):
    """The retired receiver's shutdown steps in design 14 are commands an operator pastes as root."""

    def section(self):
        text = Path(__file__).resolve().parents[2].joinpath("docs/design/14-corpus-data-flow.md").read_text()
        return text.split("## Retired worker evidence transport\n")[1].split("\n## ")[0]

    def test_service_is_stopped_after_its_timer_and_before_its_failed_state_is_reset(self):
        # Disabling the timer does not stop a tick it already started. Left running, that tick can
        # fail after reset-failed (degraded again) or find its files removed.
        section = self.section()
        (block,) = re.findall(r"```bash\n(.*?)```", section, re.S)
        timer, service = "usnm-worker-evidence-sync.timer", "usnm-worker-evidence-sync.service"
        self.assertEqual(block.splitlines(), [
            f"systemctl disable --now {timer}",
            f"systemctl stop {service}",
            f"systemctl reset-failed {service}",
        ])
        # The files are removed only after those commands, and systemd is then told they are gone.
        after = section.split(block + "```\n")[1]
        self.assertIn("remove the two unit files", after)
        self.assertIn("systemctl daemon-reload", after)
        self.assertLess(after.index("remove the two unit files"), after.index("systemctl daemon-reload"))


if __name__ == "__main__":
    unittest.main()
