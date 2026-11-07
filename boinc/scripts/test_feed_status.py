import importlib.util
import io
import json
from pathlib import Path
import contextlib
import unittest

ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location("feed_status", ROOT / "boinc/scripts/feed_status.py")
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
        # Without a Syslog table the missing rule must still count zero events.
        self.assertIn("(datatable(TimeGenerated: datetime, SyslogMessage: string)[])",
                      lines.split("let FeedEvents", 1)[0])
        condition, missing = module.split("var queries = [", 1)[1].split("name: alertNames.missing", 1)
        test_branch = condition.rsplit("| union (", 1)[1]
        self.assertIn(f"Feed.event == '{F.DELIVERY_TEST_EVENT}' and tobool(Feed.test)", test_branch)
        self.assertIn("Observed between (ago(10m) .. now())", test_branch)
        self.assertIn("| project Check = 'delivery_test'", test_branch)
        self.assertNotIn("FeedLines", missing.split("]", 1)[0])
        self.assertNotIn("delivery", missing.split("]", 1)[0])

    def test_condition_alert_holds_unknown_behind_collection(self):
        # One failed source read must not email every check it turned unknown:
        # unknown counts only once the evaluator's held collection check fires.
        module = (ROOT / "infra/modules/boinc-feed-alerts.bicep").read_text()
        condition = module.split("var queries = [", 1)[1].split("name: alertNames.missing", 1)[0]
        latest = condition.split("| union (", 1)[0]
        self.assertNotIn("in ('firing', 'unknown')", latest)
        self.assertIn("where tostring(Held.code) == 'collection'", latest)
        self.assertIn("(tostring(Check.state) == 'unknown' and CollectionFiring)", latest)
        self.assertEqual(F.COLLECTION_HOLD_S, 180)


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
            self.assertEqual((checks["collection"]["state"], checks["collection"]["reset"]), ("pending", True))

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

    def test_feeder_allocation_is_opt_in_and_drift_fires_after_two_guard_runs(self):
        self.assertNotIn("feeder_allocation", self.tick(0))
        self.state = {}
        self.facts["feeder_allocation"] = {"status": "match", "reason": "allocation_matches_registry", "app_ids": []}
        self.assertEqual(self.tick(0)["feeder_allocation"]["state"], "healthy")
        self.facts["feeder_allocation"] = {"status": "drift", "reason": "registry_changed", "app_ids": [3]}
        for at in range(60, 660, 60):
            checks = self.tick(at)
        self.assertEqual((checks["feeder_allocation"]["state"], checks["feeder_allocation"]["duration_s"]),
                         ("pending", F.FEEDER_DRIFT_HOLD_S - 60))
        checks = self.tick(660)
        self.assertEqual(checks["feeder_allocation"]["state"], "firing")
        self.assertEqual(checks["feeder_allocation"]["app_ids"], [3])
        self.assertEqual(checks["collection"]["state"], "healthy")
        self.assertEqual(len([s for s in F.metrics(self.document).splitlines()
                              if s.startswith("boinc_feed_check{") and s.endswith("} 1")]), 8)
        # Different drift is a new condition, not a continuation.
        self.facts["feeder_allocation"]["app_ids"] = [3, 7]
        self.assertEqual(self.tick(720)["feeder_allocation"]["state"], "pending")
        for at, status, reason in ((780, "maintenance", "stop_maintenance"), (840, "unknown", "guard_unavailable")):
            self.facts["feeder_allocation"] = {"status": status, "reason": reason, "app_ids": []}
            checks = self.tick(at)
            self.assertEqual((checks["feeder_allocation"]["state"], checks["feeder_allocation"]["reason"]),
                             ("unknown", reason))
            self.assertEqual(checks["collection"]["state"], "pending")

    def test_collection_fires_after_three_minutes_of_unknown_and_a_single_blip_never_fires(self):
        self.tick(0)
        self.assertEqual(self.tick(60)["collection"]["state"], "healthy")
        tick = self.facts.pop("tick")
        self.assertEqual(self.tick(120)["collection"]["state"], "pending")   # one failed source run
        self.facts["tick"] = tick
        self.assertEqual(self.tick(180)["collection"]["state"], "healthy")   # recovered: no email
        self.facts.pop("tick")
        for at in (240, 300, 360):
            self.assertEqual(self.tick(at)["collection"]["state"], "pending")
        fired = self.tick(420)
        self.assertEqual((fired["collection"]["state"], fired["collection"]["duration_s"]), ("firing", 180))

    def test_recurring_resets_fire_collection_and_a_single_reset_does_not(self):
        self.tick(0)
        self.assertEqual(self.tick(60)["collection"]["state"], "healthy")
        first = self.tick(300)                                   # one gap over 180 s: a reset
        self.assertEqual((first["collection"]["state"], first["collection"]["reset"]), ("pending", True))
        self.assertEqual(self.tick(360)["collection"]["state"], "healthy")
        for at, overrides in ((600, {}), (840, {}), (1080, {})):  # an evaluator running every four minutes
            checks = self.tick(at, **overrides)
        self.assertEqual(self.tick(1080 + 240)["collection"]["state"], "firing")
        self.assertEqual(checks["collection"]["state"], "firing")
        self.tick(1400)
        self.assertEqual(self.tick(1460)["collection"]["state"], "healthy")
        for at, config in ((1520, "a"), (1580, "b"), (1640, "c"), (1700, "d")):  # configs changing every minute
            checks = self.tick(at, config_id=config)
        self.assertEqual((checks["collection"]["state"], checks["collection"]["duration_s"]), ("firing", 180))

    def test_a_wall_clock_jump_or_a_reboot_cannot_shorten_the_collection_hold(self):
        self.tick(0)                                           # the first evaluation is a reset
        self.assertEqual(self.tick(60)["collection"]["state"], "healthy")
        self.facts.pop("tick")
        self.assertEqual(self.tick(120)["collection"]["state"], "pending")
        jumped = self.tick(780, monotonic=10180)              # wall +660 s, monotonic +60 s
        self.assertEqual((jumped["collection"]["state"], jumped["collection"]["duration_s"]), ("pending", 60))
        rebooted = self.tick(1500, monotonic=50, boot_id="next")  # a reboot adds nothing for that step
        self.assertEqual((rebooted["collection"]["state"], rebooted["collection"]["duration_s"]), ("pending", 60))
        self.assertEqual(self.tick(1560, monotonic=110, boot_id="next")["collection"]["state"], "pending")
        self.assertEqual(self.tick(1620, monotonic=170, boot_id="next")["collection"]["state"], "firing")
        self.facts["tick"] = {"ok": True, "seconds": 900}
        self.assertEqual(self.tick(1680, monotonic=230, boot_id="next")["collection"]["state"], "healthy")

    def test_corrupt_duration_state_resets_instead_of_trusting_old_health(self):
        self.state = {"schema_version": 1, "time": "broken", "conditions": []}
        collection = self.tick(0)["collection"]
        self.assertEqual((collection["state"], collection["reset"]), ("pending", True))


if __name__ == "__main__":
    unittest.main()


class QwenAdmissionTest(unittest.TestCase):
    """usnm_qwen_no_work from the native cohort controller (#1441, opt-in qwen_admission)."""

    def setUp(self):
        self.state = {}
        self.report = {"status": "no-fresh-verified-pairs", "admitted_unique": 592, "limit": 700,
                       "fresh_pairs_in_scan": 4, "currently_eligible_unsent_replicas": 0,
                       "native_gpu_inflight": {"9": 0}, "per_host_buffer_limit": 10, "admissible_backlog": 4}
        self.cohort = {"result": "ok", "timer_active": True, "last_completed_at": NOW, "last_failure_at": 0,
                       "failure_streak": 0, "failing_since": None, "report": self.report, "report_at": NOW}
        self.facts = {"enabled_until": NOW + 86400, "qwen_admission": True, "cohort": self.cohort,
                      "backlogs": {"usnm_ocr": 0, "usnm_qwen": None},
                      "last_workunits": {"usnm_ocr": NOW, "usnm_qwen": NOW - 99999},
                      "supply": {"cpu": {"idle": 0, "eligible": 0}, "gpu": {"idle": 0, "eligible": 0}},
                      "tick": {"ok": True, "seconds": 10}}

    def check(self, elapsed):
        self.cohort["last_completed_at"] = self.cohort["report_at"] = NOW + elapsed
        document, self.state = F.evaluate(self.facts, self.state, NOW + elapsed, 10000 + elapsed, "boot", "cfg")
        return next(c for c in document["checks"] if c["code"] == "usnm_qwen_no_work")

    def test_backlog_and_capacity_without_an_admission_for_an_hour_fires_and_admission_resets(self):
        for at in range(0, 3600, 60):
            self.assertEqual(self.check(at)["state"], "pending")
        fired = self.check(3600)
        self.assertEqual((fired["state"], fired["backlog"], fired["reason"]),
                         ("firing", 4, "no-fresh-verified-pairs"))
        self.report["admitted_unique"] = 593
        self.assertEqual(self.check(3660)["state"], "pending")

    def test_no_backlog_full_buffer_waiting_window_or_closed_window_is_healthy(self):
        for change in ({"admissible_backlog": 0}, {"native_gpu_inflight": {"9": 10}},
                       {"native_gpu_inflight": {"9": 0, "10": 10}},
                       {"status": "no-audited-fresh-inputs", "fresh_pairs_in_scan": None, "admissible_backlog": None,
                        "native_gpu_inflight": None, "per_host_buffer_limit": None,
                        "currently_eligible_unsent_replicas": None},
                       {"currently_eligible_unsent_replicas": 3}, {"admitted_unique": 700},
                       {"status": "closed-expired-budget-or-release-change"}):
            with self.subTest(change=change):
                self.setUp()
                self.report.update(change)
                self.assertEqual(self.check(0)["state"], "healthy")
        self.setUp()
        self.facts["enabled_until"] = NOW - 1
        checked = self.check(0)
        self.assertEqual((checked["state"], checked["backlog"]), ("healthy", 0))

    def test_controller_failure_stall_or_silence_fires_and_missing_evidence_is_unknown(self):
        for change, reason in (({"timer_active": False}, "cohort_timer_inactive"),
                               ({"failure_streak": 3, "failing_since": NOW - 150}, "cohort_run_failed"),
                               ({"last_completed_at": NOW - 901}, "cohort_stalled"),
                               ({"last_completed_at": NOW - 901, "result": "running"}, "cohort_stalled"),
                               ({"last_completed_at": None}, "cohort_stalled"),
                               ({"report_at": NOW - 1801}, "cohort_no_report"),
                               ({"report": None}, "cohort_no_report")):
            with self.subTest(reason=reason, change=change):
                self.setUp()
                self.cohort.update(change)
                document, _ = F.evaluate(self.facts, {}, NOW, 10000, "boot", "cfg")
                checked = next(c for c in document["checks"] if c["code"] == "usnm_qwen_no_work")
                self.assertEqual((checked["state"], checked["reason"]), ("firing", reason))
        for streak in (1, 2):
            with self.subTest(streak=streak):
                self.setUp()
                self.cohort.update(failure_streak=streak, failing_since=NOW - 60, result="failed")
                document, _ = F.evaluate(self.facts, {}, NOW, 10000, "boot", "cfg")
                self.assertEqual(next(c for c in document["checks"]
                                      if c["code"] == "usnm_qwen_no_work")["state"], "pending")
        for key in ("admissible_backlog", "per_host_buffer_limit", "native_gpu_inflight",
                    "currently_eligible_unsent_replicas"):
            with self.subTest(missing=key):
                self.setUp()
                self.report[key] = None
                document, _ = F.evaluate(self.facts, {}, NOW, 10000, "boot", "cfg")
                checked = next(c for c in document["checks"] if c["code"] == "usnm_qwen_no_work")
                self.assertEqual((checked["state"], checked["reason"]), ("unknown", "cohort_report_incomplete"))
        for cohort in (None, {"error": "cohort_unavailable"}):
            self.setUp()
            self.facts["cohort"] = cohort
            document, _ = F.evaluate(self.facts, {}, NOW, 10000, "boot", "cfg")
            checks = {c["code"]: c for c in document["checks"]}
            self.assertEqual(checks["usnm_qwen_no_work"]["state"], "unknown")
            self.assertEqual(checks["collection"]["state"], "pending")

    def test_without_the_opt_in_the_workunit_check_is_unchanged(self):
        del self.facts["qwen_admission"]
        document, _ = F.evaluate(self.facts, {}, NOW, 10000, "boot", "cfg")
        self.assertEqual(next(c for c in document["checks"] if c["code"] == "usnm_qwen_no_work")["state"],
                         "unknown")
