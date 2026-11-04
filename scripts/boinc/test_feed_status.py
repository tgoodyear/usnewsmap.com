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

    def test_delivery_test_prints_only_the_synthetic_line(self):
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            code = F.main(["--delivery-test"])
        self.assertEqual(code, 0)
        lines = out.getvalue().splitlines()
        self.assertEqual(len(lines), 1)
        record = json.loads(lines[0])
        self.assertEqual(set(record), {"schema_version", "event", "observed_at", "test"})
        self.assertEqual(record["event"], "boinc_feed_delivery_test")
        self.assertIs(record["test"], True)
        self.assertIsInstance(record["observed_at"], int)
        # Never mixed with live collection, which would write state.
        with self.assertRaises(SystemExit), contextlib.redirect_stderr(io.StringIO()):
            F.main(["--delivery-test", "--config", "feed.json", "--state-dir", "state"])

    def test_alert_rules_keep_delivery_tests_out_of_health(self):
        module = (ROOT / "infra/modules/boinc-feed-alerts.bicep").read_text()
        lines = module.split("var lines = '''", 1)[1].split("'''", 1)[0]
        events = lines.split("let FeedEvents = FeedLines", 1)[1]
        self.assertIn("| where Feed.event == 'boinc_feed_health';", events)
        self.assertIn("FeedEvents\n| summarize arg_max(TimeGenerated, *)", events)
        self.assertIn("| where Feed.schema_version == 1", lines)
        condition, missing = module.split("var queries = [", 1)[1].split("name: alertNames.missing", 1)
        test_branch = condition.rsplit("| union (", 1)[1]
        self.assertIn(f"Feed.event == '{F.DELIVERY_TEST_EVENT}' and tobool(Feed.test)", test_branch)
        self.assertIn("Observed between (ago(10m) .. now())", test_branch)
        self.assertIn("| project Check = 'delivery_test'", test_branch)
        self.assertNotIn("FeedLines", missing.split("]", 1)[0])
        self.assertNotIn("delivery", missing.split("]", 1)[0])


class DurationTest(unittest.TestCase):
    def setUp(self):
        self.state = {}
        self.facts = {
            "enabled_until": NOW + 86400,
            "backlogs": {"usnm_ocr": 2, "usnm_qwen": 0},
            "last_workunits": {"usnm_ocr": NOW - 10, "usnm_qwen": NOW},
            "supply": {"cpu": {"idle": 4, "eligible": 3}, "gpu": {"idle": 1, "eligible": 1}},
            "tick": {"ok": True, "seconds": 900},
        }

    def tick(self, elapsed, **overrides):
        args = dict(now=NOW + elapsed, monotonic=10000 + elapsed, boot_id="boot", config_id="cfg")
        args.update(overrides)
        document, self.state = F.evaluate(self.facts, self.state, **args)
        self.document = document
        return {c["code"]: c for c in document["checks"]}

    def test_exact_thresholds_and_new_workunit_resets(self):
        self.assertEqual(self.tick(0)["cpu_shortage"]["state"], "pending")
        for at in range(60, 1800, 60):
            self.tick(at)
        self.assertEqual(self.tick(1799)["cpu_shortage"]["state"], "pending")
        self.assertEqual(self.tick(1800)["cpu_shortage"]["state"], "firing")
        for at in range(1860, 3600, 60):
            self.tick(at)
        self.assertEqual(self.tick(3599)["usnm_ocr_no_work"]["state"], "pending")
        self.assertEqual(self.tick(3600)["usnm_ocr_no_work"]["state"], "firing")
        self.facts["last_workunits"]["usnm_ocr"] = NOW + 3601
        self.assertEqual(self.tick(3660)["usnm_ocr_no_work"]["duration_s"], 0)
        self.facts["backlogs"]["usnm_ocr"] = 0
        self.assertEqual(self.tick(3720)["usnm_ocr_no_work"]["state"], "healthy")

    def test_unknown_and_recovery_interrupt_durations(self):
        for at in range(0, 1800, 60):
            self.tick(at)
        del self.facts["supply"]["cpu"]
        self.assertEqual(self.tick(1800)["cpu_shortage"]["state"], "unknown")
        self.facts["supply"]["cpu"] = {"idle": 4, "eligible": 3}
        self.assertEqual(self.tick(1860)["cpu_shortage"]["duration_s"], 0)
        self.facts["supply"]["cpu"]["eligible"] = 4
        self.assertEqual(self.tick(1920)["cpu_shortage"]["state"], "healthy")
        self.facts["supply"]["cpu"]["eligible"] = 3
        self.assertEqual(self.tick(1980)["cpu_shortage"]["duration_s"], 0)

    def test_restart_persists_but_boot_config_gap_and_clock_reset(self):
        for change in ({"boot_id": "new"}, {"config_id": "new"}, {"monotonic": 9999},
                       {"now": NOW - 1}, {"now": NOW + 1000, "monotonic": 11000}):
            self.state = {}
            self.tick(0)
            self.state = json.loads(json.dumps(self.state))  # next process
            self.assertEqual(self.tick(60)["cpu_shortage"]["duration_s"], 60)
            checks = self.tick(120, **change)
            self.assertEqual(checks["cpu_shortage"]["duration_s"], 0)
            self.assertEqual(checks["collection"]["state"], "firing")

    def test_immediate_conditions_and_metrics_contract(self):
        self.facts["enabled_until"] = NOW + 21600
        self.facts["tick"] = {"ok": False, "seconds": 1}
        checks = self.tick(0)
        self.assertEqual(checks["qwen_refill_window"]["state"], "firing")
        self.assertEqual(checks["controller_tick"]["state"], "firing")
        self.facts["tick"] = {"ok": None, "seconds": 901}
        self.assertEqual(self.tick(60)["controller_tick"]["state"], "firing")
        self.facts["tick"] = {"ok": True, "seconds": 900}
        self.assertEqual(self.tick(120)["controller_tick"]["state"], "healthy")
        text = F.metrics(self.document)
        self.assertIn('boinc_feed_check{check="cpu_shortage",state="pending"} 1', text)
        self.assertEqual(len([s for s in text.splitlines()
                              if s.startswith("boinc_feed_check{") and s.endswith("} 1")]), 7)
        self.assertIn('boinc_feed_idle_slots{check="cpu_shortage"} 4', text)
        self.assertEqual(json.loads(json.dumps(self.document))["schema_version"], 1)

    def test_failure_between_observations_is_not_hidden_by_success(self):
        self.tick(0)
        self.facts["tick"]["last_failure_at"] = NOW + 20
        self.assertEqual(self.tick(60)["controller_tick"]["state"], "firing")
        self.assertEqual(self.tick(120)["controller_tick"]["state"], "healthy")

    def test_alert_retains_firing_shortage_after_latest_observation_recovers(self):
        for elapsed in range(0, 1801, 60):
            checks = self.tick(elapsed)
        self.assertEqual(checks["cpu_shortage"]["state"], "firing")
        self.facts["supply"]["cpu"]["eligible"] = 4
        self.assertEqual(self.tick(1860)["cpu_shortage"]["state"], "healthy")
        # Contract for the retained-history branch; actual workspace query delivery
        # remains a rollout gate, not a local Kusto execution claim.
        query = (ROOT / "infra/modules/boinc-feed-alerts.bicep").read_text()
        retained = query.split("| union (", 1)[1].split("| distinct Check", 1)[0]
        self.assertIn("Observed between (ago(10m) .. now())", retained)
        self.assertIn("| where Check.state == 'firing'", retained)
        self.assertIn("| project Check = tostring(Check.code)", retained)
        self.assertNotIn("controller_tick", retained)

    def test_corrupt_duration_state_resets_instead_of_trusting_old_health(self):
        self.state = {"schema_version": 1, "time": "broken", "conditions": []}
        self.assertEqual(self.tick(0)["collection"]["state"], "firing")


if __name__ == "__main__":
    unittest.main()
