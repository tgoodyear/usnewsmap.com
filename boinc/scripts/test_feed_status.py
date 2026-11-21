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
        self.assertIn("FeedEvents\n| summarize arg_max(TimeGenerated, *)", missing)

    def test_condition_alert_holds_unknown_behind_collection(self):
        # One failed source read must not email every check it turned unknown:
        # unknown counts only once the evaluator's held collection check fires.
        module = (ROOT / "infra/modules/boinc-feed-alerts.bicep").read_text()
        condition = module.split("var queries = [", 1)[1].split("name: alertNames.missing", 1)[0]
        health = condition.split("| union (", 1)[0]
        self.assertNotIn("in ('firing', 'unknown')", health)
        self.assertIn("where tostring(Held.code) == 'collection'", health)
        self.assertIn("(State == 'unknown' and CollectionFiring and tostring(Check.excused) != 'true')", health)
        # #1698: unknown rows count over the same ten minutes as firing ones, so they don't flap.
        self.assertIn("FeedEvents\n| where Observed between (ago(10m) .. now())", health)
        self.assertNotIn("ago(3m)", health)
        self.assertEqual(F.COLLECTION_HOLD_S, 180)

    def test_condition_alert_raises_one_row_per_dead_source_and_names_the_tick_reason(self):
        module = (ROOT / "infra/modules/boinc-feed-alerts.bicep").read_text()
        health = module.split("var queries = [", 1)[1].split("name: alertNames.missing", 1)[0]
        health = health.split("| union (", 1)[0]
        self.assertIn("State == 'unknown' and isnotempty(Cause), Cause,", health)
        self.assertIn("(Code == 'controller_tick' or Code startswith 'plan_tick:') and isnotempty(Reason),", health)
        self.assertIn("strcat(Code, ':', Reason),", health)
        # collection only when something it counts has no cause row; an older evaluator's line
        # (no uncovered) still sends it.
        self.assertIn("(Code != 'collection' or Uncovered > 0)", health)
        self.assertIn("Uncovered = max(coalesce(toint(Held.uncovered), 1))", health)
        # Copilot on #1699: a null in a where clause drops the row, so an optional field read
        # without a default (not(tobool(...)) on a check without the field) would silence an
        # outage. Optional fields are read only through tostring or coalesce.
        for field in ("excused", "uncovered", "cause", "reason"):
            for use in __import__("re").findall(r"(\w+)\((?:Check|Held)\.%s\)" % field, health):
                self.assertIn(use, ("tostring", "toint"), field)
        self.assertNotIn("tobool(Check.", health)
        self.assertNotIn("toint(Held.uncovered) ", health)
        self.assertTrue(health.rstrip().endswith("| distinct Check"))


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
        # The window is closing but no GPU host asks for work idle (one slot, one result for it).
        self.assertEqual((checks["qwen_refill_window"]["state"], checks["qwen_refill_window"]["gpu_idle"]),
                         ("healthy", False))
        self.assertEqual(checks["controller_tick"]["state"], "firing")
        self.facts["tick"] = {"ok": None, "seconds": F.TICK_HANG_S + 1}
        self.assertEqual(self.tick(60)["controller_tick"]["state"], "firing")
        self.facts["tick"] = {"ok": True, "seconds": 900}
        self.assertEqual(self.tick(120)["controller_tick"]["state"], "healthy")
        text = F.metrics(self.document)
        self.assertIn('boinc_feed_check{check="cpu_shortage",state="pending"} 1', text)
        self.assertEqual(len([s for s in text.splitlines()
                              if s.startswith("boinc_feed_check{") and s.endswith("} 1")]), 7)
        self.assertIn('boinc_feed_idle_slots{check="cpu_shortage"} 4', text)
        self.assertEqual(json.loads(json.dumps(self.document))["schema_version"], 1)

    def ticks(self, *steps):
        """Run (elapsed, tick) steps from fresh state; return each controller_tick check."""
        self.state, out = {}, []
        for elapsed, tick in steps:
            self.facts["tick"] = {"ok": True, "seconds": 1, "finished_at": NOW + elapsed,
                                  "maintenance_failure_at": 0, "failed_in_a_row": 0} | tick
            out.append(self.tick(elapsed)["controller_tick"])
            self.state = json.loads(json.dumps(self.state))
        return out

    def test_one_failed_tick_waits_and_three_in_a_row_fire_until_a_tick_succeeds(self):
        checks = self.ticks((0, {"ok": False, "failed_in_a_row": 1}), (60, {"ok": False, "failed_in_a_row": 1}),
                            (600, {"ok": False, "failed_in_a_row": 2}), (660, {"ok": False, "failed_in_a_row": 3}),
                            (700, {"ok": None, "failed_in_a_row": 3}), (1200, {"failed_in_a_row": 0}))
        self.assertEqual([c["state"] for c in checks],
                         ["healthy", "healthy", "healthy", "firing", "firing", "healthy"])
        self.assertEqual([c["failed_in_a_row"] for c in checks], [1, 1, 2, 3, 3, 0])

    def test_a_failed_tick_with_no_success_for_thirty_minutes_fires(self):
        for last_ok, state in ((NOW - 1799, "healthy"), (NOW - 1800, "firing"), (None, "firing")):
            with self.subTest(last_ok_at=last_ok):
                checks = self.ticks((0, {"ok": False, "failed_in_a_row": 1, "last_ok_at": last_ok}))
                self.assertEqual(checks[0]["state"], state)
        # A success newer than the failure leaves no streak, and nothing fires however old the last was.
        self.assertEqual(self.ticks((0, {"failed_in_a_row": 0, "last_ok_at": NOW - 7200}))[0]["state"], "healthy")
        # Failures that alternate with good ticks (the old four-an-hour rule) don't fire.
        checks = self.ticks((0, {"ok": False, "failed_in_a_row": 1, "failed_last_hour": 5, "last_ok_at": NOW - 300}),
                            (300, {"failed_in_a_row": 0, "failed_last_hour": 5, "last_ok_at": NOW + 300}))
        self.assertEqual([c["state"] for c in checks], ["healthy", "healthy"])

    def test_a_maintenance_failure_fires_only_when_the_controller_stopped_ticking(self):
        checks = self.ticks((0, {"maintenance_failure_at": NOW - 30, "last_ok_at": NOW - 120}))
        self.assertEqual(checks[0]["state"], "healthy")
        # Seen first before the 30 minutes pass, it still fires when they do, and stays until a tick succeeds.
        failure = NOW - 600
        checks = self.ticks((0, {"maintenance_failure_at": failure, "last_ok_at": failure - 60}),
                            (600, {"maintenance_failure_at": failure, "last_ok_at": failure - 60}),
                            (1200, {"maintenance_failure_at": failure, "last_ok_at": failure - 60}),
                            (1500, {"maintenance_failure_at": failure, "last_ok_at": failure - 60}),
                            (1600, {"maintenance_failure_at": failure, "last_ok_at": NOW + 1500}))
        self.assertEqual([c["state"] for c in checks], ["healthy", "healthy", "firing", "firing", "healthy"])
        checks = self.ticks((0, {"maintenance_failure_at": failure, "last_ok_at": failure - 1800}),
                            (60, {"maintenance_failure_at": failure, "last_ok_at": failure - 1800}))
        self.assertEqual([c["state"] for c in checks], ["firing", "firing"])

    def test_a_tick_running_too_long_fires_at_once(self):
        checks = self.ticks((0, {"ok": None, "seconds": F.TICK_HANG_S + 1}),
                            (60, {"ok": None, "seconds": F.TICK_HANG_S}))
        self.assertEqual([c["state"] for c in checks], ["firing", "healthy"])

    def test_a_running_tick_past_the_limit_waits_for_the_hang_limit(self):
        # #1698: an hourly 20-minute tick fired every hour. Main's ticks took 20 to 41 minutes on 18 Nov
        # 2026 and finished, so a running tick waits an hour before it may be a hang.
        self.assertEqual(F.TICK_HANG_S, 4 * F.TICK_LIMIT_S)
        checks = self.ticks((0, {"ok": None, "seconds": 1200}), (60, {"ok": None, "seconds": 2460}),
                            (600, {"ok": None, "seconds": F.TICK_HANG_S + 1}))
        self.assertEqual([c["state"] for c in checks], ["healthy", "healthy", "firing"])
        # A tick that finished slow is not a failure: two in a row stay under the three.
        checks = self.ticks((0, {"seconds": 1200, "failed_in_a_row": 0}), (600, {"seconds": 1200, "failed_in_a_row": 0}))
        self.assertEqual([c["state"] for c in checks], ["healthy", "healthy"])

    def test_a_collector_without_the_streak_fires_on_any_failure(self):
        self.facts["tick"] = {"ok": False, "seconds": 1, "finished_at": NOW, "last_failure_at": NOW}
        checked = self.tick(0)["controller_tick"]
        self.assertEqual((checked["state"], checked["failed_in_a_row"]), ("firing", None))

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
        retained = query.split("var queries = [", 1)[1].split("| distinct Check", 1)[0]
        self.assertIn("Observed between (ago(10m) .. now())", retained)
        self.assertIn("(State == 'firing' and (Code != 'collection' or Uncovered > 0))", retained)

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
        # The hold runs down at half rate (#1698): six healthy minutes clear a full one.
        for at in range(1520, 1880, 60):
            self.assertEqual(self.tick(at)["collection"]["state"], "healthy")
        self.assertIsNone(self.state["collection_elapsed"])
        for at, config in ((1880, "a"), (1940, "b"), (2000, "c"), (2060, "d")):  # configs changing every minute
            checks = self.tick(at, config_id=config)
        self.assertEqual((checks["collection"]["state"], checks["collection"]["duration_s"]), ("firing", 180))

    def test_a_rotation_window_defers_shortage_and_no_work_and_tolerates_a_missing_tick(self):
        # #1687: a planned version rotation stops the controller and empties the queue for a while.
        rotation = {"reason": "#529 claim", "started_at": NOW, "until": NOW + 3900}
        for at in range(0, 3900, 60):
            checks = self.tick(at, rotation=rotation)
        self.assertEqual((checks["cpu_shortage"]["state"], checks["cpu_shortage"]["rotation_until"]),
                         ("pending", NOW + 3900))
        self.assertEqual(checks["usnm_ocr_no_work"]["state"], "pending")
        self.assertEqual(self.document["rotation"], {"reason": "#529 claim", "until": NOW + 3900})
        # The window ends: both fire at once, on their unbroken durations.
        checks = self.tick(3900)
        self.assertEqual((checks["cpu_shortage"]["state"], checks["cpu_shortage"]["duration_s"]), ("firing", 3900))
        self.assertEqual(checks["usnm_ocr_no_work"]["state"], "firing")
        self.assertNotIn("rotation_until", checks["cpu_shortage"])
        # A stopped timer: the controller source is missing, which doesn't count towards collection.
        self.state = {}
        rotation = dict(rotation, until=NOW + 10000)
        self.tick(0, rotation=rotation)
        self.facts.pop("tick")
        self.facts["errors"] = ["controller"]
        self.facts["sources"] = {"controller": {"observed_at": None, "state": "unknown",
                                                "reason": F.NO_RECENT_TICK}}
        for at in range(60, 600, 60):
            checks = self.tick(at, rotation=rotation)
        self.assertEqual((checks["collection"]["state"], checks["controller_tick"]["state"]), ("healthy", "unknown"))
        # Any other controller source failure (transport, a malformed or stale document) isn't excused.
        for reason in ("unavailable_or_invalid", "http_protocol_error", "stale_at_completion"):
            with self.subTest(reason=reason):
                self.state = {}
                self.facts["sources"]["controller"]["reason"] = reason
                for at in range(0, 300, 60):
                    checks = self.tick(at, rotation=rotation)
                self.assertEqual(checks["collection"]["state"], "firing")
        self.facts["sources"]["controller"]["reason"] = F.NO_RECENT_TICK
        self.state = {}
        self.tick(0, rotation=rotation)
        for at in range(60, 600, 60):
            checks = self.tick(at, rotation=rotation)
        # Any other unknown source still does, and a failing tick still fires.
        self.facts["errors"] = ["controller", "server"]
        for at in range(600, 840, 60):
            checks = self.tick(at, rotation=rotation)
        self.assertEqual(checks["collection"]["state"], "firing")
        self.facts["errors"] = []
        self.facts["tick"] = {"ok": False, "seconds": 1, "maintenance_failure_at": 0, "failed_in_a_row": 3}
        self.assertEqual(self.tick(900, rotation=rotation)["controller_tick"]["state"], "firing")
        # Without a window the same missing tick is unknown and collection fires as before.
        self.state = {}
        self.facts["sources"] = {"controller": {"observed_at": None, "state": "unknown",
                                                "reason": F.NO_RECENT_TICK}}
        self.facts["tick"] = {"ok": True, "seconds": 900}
        self.tick(0)
        self.facts.pop("tick")
        self.facts["errors"] = ["controller"]
        for at in range(60, 300, 60):
            checks = self.tick(at)
        self.assertEqual(checks["collection"]["state"], "firing")

    def run_archive(self, start, end, runs, step=60):
        """Evaluations every `step` seconds from `start` to `end`, with `runs(at)` as the controller
        source's archive_runs (None: none reported), the state passed on as JSON as between processes."""
        for at in range(start, end + 1, step):
            reported = runs(at)
            if reported is None:
                self.facts.pop("archive_runs", None)
            else:
                self.facts["archive_runs"] = reported
            self.state = json.loads(json.dumps(self.state))
            checks = self.tick(at)
        return checks["usnm_ocr_no_work"]

    def test_a_held_archive_run_defers_no_work_while_it_lasts_and_one_hold_after(self):
        # #835: the archive holds the tick lock and stops main's timer; the newest workunit is NOW - 10.
        def runs(at):
            return [] if at < 600 else [[NOW + 600, None if at < 4000 else NOW + 4000]]
        check = self.run_archive(0, 3960, runs)
        # While it runs, at the latest until ARCHIVE_WORKUNIT_MAX_AGE_S after the newest workunit.
        self.assertEqual((check["state"], check["archive_until"], check["duration_s"]),
                         ("pending", NOW - 10 + F.ARCHIVE_WORKUNIT_MAX_AGE_S, 3960))
        check = self.run_archive(4000, 7599, runs)
        self.assertEqual((check["state"], check["archive_until"]), ("pending", NOW + 7600))
        # One hold after the run ended, it fires on its unbroken duration.
        check = self.run_archive(7600, 7600, runs)
        self.assertEqual((check["state"], check["duration_s"]), ("firing", 7600))
        self.assertNotIn("archive_until", check)
        # Without the source's runs nothing waits: the hour fires as before.
        self.state, self.facts["last_workunits"]["usnm_ocr"] = {}, NOW
        self.assertEqual(self.run_archive(0, 3600, lambda at: None)["state"], "firing")

    def test_a_run_that_began_after_the_check_was_due_excuses_nothing(self):
        # The newest workunit was an hour old before the run began: a stall the archive didn't make.
        def runs(at):
            return [[NOW + 3700, None]] if at >= 3700 else []
        self.assertEqual(self.run_archive(0, 3600, runs)["state"], "firing")
        self.assertEqual(self.run_archive(3660, 4200, runs)["state"], "firing")
        # One that began a second before it was due does wait.
        self.state = {}
        self.assertEqual(self.run_archive(0, 3600, lambda at: [[NOW + 3590, None]] if at >= 3590 else [])["state"],
                         "pending")

    def test_the_deferral_is_capped_by_the_runs_length_and_the_workunits_age(self):
        # A run going for longer than ARCHIVE_PAUSE_MAX_S is no planned pause.
        start = NOW - F.ARCHIVE_PAUSE_MAX_S + 3700
        check = self.run_archive(0, 3690, lambda at: [[start, None]])
        self.assertEqual(check["state"], "pending")
        self.assertEqual(self.run_archive(3700, 3700, lambda at: [[start, None]])["state"], "firing")
        # Nor is one that ran too long and ended, or one under a minute (an operator's pause: it skips).
        for run in ([NOW - F.ARCHIVE_PAUSE_MAX_S - 1, NOW + 60], [NOW + 3000, NOW + 3000 + F.ARCHIVE_MIN_RUN_S - 1]):
            with self.subTest(run=run):
                self.state = {}
                self.assertEqual(self.run_archive(0, 3600, lambda at: [run] if at >= run[1] else [])["state"],
                                 "firing")
        # Runs back to back: whatever runs, it fires once the newest workunit is ARCHIVE_WORKUNIT_MAX_AGE_S old.
        self.state = {}
        limit = F.ARCHIVE_WORKUNIT_MAX_AGE_S - 10

        def back_to_back(at):
            return [[NOW + 600, NOW + 2400], [NOW + 2500, None]] if at >= 2500 else [[NOW + 600, None]]
        check = self.run_archive(0, limit - 60, back_to_back)
        self.assertEqual((check["state"], check["archive_until"]), ("pending", NOW + limit))
        self.assertEqual(self.run_archive(limit, limit, back_to_back)["state"], "firing")

    def test_a_units_earlier_run_is_remembered_once_systemd_shows_only_its_next(self):
        # Main's archive runs again at NOW + 5000: systemd no longer shows the run the gap began in,
        # and the new one began after the check was due. The earlier one ended when it was last seen.
        def runs(at):
            return [[NOW + 600, None]] if at < 5000 else [[NOW + 5000, None]]
        self.assertEqual(self.run_archive(0, 4980, runs, step=20)["state"], "pending")
        check = self.run_archive(5000, 8579, runs, step=20)
        self.assertEqual((check["state"], check["archive_until"]), ("pending", NOW + 4980 + F.NO_WORK_HOLD_S))
        self.assertEqual(self.run_archive(8580, 8580, runs)["state"], "firing")
        # The same with the controller source gone: a run is never taken to be still going unseen.
        self.state = {}
        self.run_archive(0, 1200, lambda at: [[NOW + 600, None]] if at >= 600 else [])
        check = self.run_archive(1260, 4799, lambda at: None)
        self.assertEqual((check["state"], check["archive_until"]), ("pending", NOW + 4800))
        self.assertEqual(self.run_archive(4800, 4800, lambda at: None)["state"], "firing")
        # And nothing else waits for an archive.
        self.state = {}
        self.facts["supply"]["cpu"] = {"idle": 4, "eligible": 3}
        checks = {}
        for at in range(0, 1801, 60):
            self.facts["archive_runs"] = [[NOW, None]]
            checks = self.tick(at)
        self.assertEqual(checks["cpu_shortage"]["state"], "firing")

    def test_the_rotation_window_is_bounded_and_written_by_named_flags(self):
        good = {"reason": "roll", "started_at": NOW - 60, "until": NOW + 60}
        self.assertEqual(F.rotation_window(json.dumps(good), NOW), good)
        for bad in (None, "", "{", "[]", json.dumps(dict(good, until=NOW)),
                    json.dumps(dict(good, started_at=NOW + 1)), json.dumps(dict(good, reason="")),
                    json.dumps(dict(good, until=NOW - 60 + 241 * 60)), json.dumps(dict(good, extra=1)),
                    json.dumps(dict(good, until="later"))):
            with self.subTest(bad=bad):
                self.assertIsNone(F.rotation_window(bad, NOW))
        import tempfile
        with tempfile.TemporaryDirectory() as folder:
            out = io.StringIO()
            with contextlib.redirect_stdout(out):
                self.assertEqual(F.main(["--rotation-start", "--state-dir", folder, "--minutes", "90",
                                         "--reason", "usnm_ocr 1.7 rotation"]), 0)
            written = json.loads((Path(folder) / F.ROTATION_FILE).read_text())
            self.assertEqual(written["until"] - written["started_at"], 5400)
            self.assertEqual(json.loads(out.getvalue())["action"], "start")
            self.assertIsNotNone(F.rotation_window(json.dumps(written), written["started_at"] + 1))
            for argv in (["--minutes", "241", "--reason", "x"], ["--minutes", "0", "--reason", "x"],
                         ["--minutes", "10"], ["--reason", "x"]):
                with self.subTest(argv=argv), self.assertRaises(SystemExit), \
                        contextlib.redirect_stderr(io.StringIO()):
                    F.main(["--rotation-start", "--state-dir", folder, *argv])
            with self.assertRaises(SystemExit), contextlib.redirect_stderr(io.StringIO()):
                F.main(["--rotation-start", "--rotation-end", "--state-dir", folder])
            with self.assertRaises(SystemExit), contextlib.redirect_stderr(io.StringIO()):
                F.main(["--rotation-end", "--state-dir", folder, "--minutes", "5"])
            # A file that isn't UTF-8 is no window, and doesn't stop the monitor.
            (Path(folder) / F.ROTATION_FILE).write_bytes(b'{"reason": "\xff\xfe", "until": 1}')
            self.assertIsNone(F.read_rotation(folder))
            self.assertIsNone(F.rotation_window(F.read_rotation(folder), NOW))
            (Path(folder) / F.ROTATION_FILE).write_text(json.dumps(written))
            self.assertEqual(json.loads(F.read_rotation(folder)), written)
            self.assertIsNone(F.read_rotation(Path(folder) / "missing"))
            out = io.StringIO()
            with contextlib.redirect_stdout(out):
                F.main(["--rotation-end", "--state-dir", folder])
            self.assertTrue(json.loads(out.getvalue())["had_window"])
            self.assertFalse((Path(folder) / F.ROTATION_FILE).exists())

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


class ControllerSignalsTest(unittest.TestCase):
    """#1698: the failure reason on controller_tick, and the opt-in controller signal checks."""

    def setUp(self):
        self.state = {}
        self.signals = {"caps": {}, "ownership_conflicts": 0,
                        "progress": {"published_at": NOW - 300, "first_tick_at": NOW - 7000, "pending": 40},
                        "tick_p90_s": 120.0}
        self.tick = {"ok": True, "seconds": 30, "finished_at": NOW, "maintenance_failure_at": 0,
                     "failed_in_a_row": 0, "reason": None, "same_error_in_a_row": 0, **self.signals}
        self.facts = {"enabled_until": NOW + 86400, "backlogs": {"usnm_ocr": 0, "usnm_qwen": 0},
                      "last_workunits": {"usnm_ocr": NOW, "usnm_qwen": NOW},
                      "supply": {"cpu": {"idle": 0, "eligible": 0}, "gpu": {"idle": 0, "eligible": 0}},
                      "tick": self.tick, "controller_signals": True}

    def checks(self, elapsed=0, **overrides):
        args = dict(now=NOW + elapsed, monotonic=10000 + elapsed, boot_id="boot", config_id="cfg")
        args.update(overrides)
        self.document, self.state = F.evaluate(self.facts, self.state, **args)
        self.state = json.loads(json.dumps(self.state))
        return {c["code"]: c for c in self.document["checks"]}

    def test_the_reason_and_a_sticky_failure_ride_on_controller_tick(self):
        self.tick.update(ok=False, failed_in_a_row=3, reason="cap_reached", same_error_in_a_row=2)
        checked = self.checks()["controller_tick"]
        self.assertEqual((checked["state"], checked["reason"], checked["same_error_in_a_row"], checked["sticky"]),
                         ("firing", "cap_reached", 2, False))
        self.tick.update(failed_in_a_row=3, same_error_in_a_row=3)
        self.assertIs(self.checks(60)["controller_tick"]["sticky"], True)
        # A collector from before #1698 has no reason: none is shown, nothing else changes.
        for key in ("reason", "same_error_in_a_row"):
            del self.tick[key]
        checked = self.checks(120)["controller_tick"]
        self.assertEqual((checked["state"], checked["reason"], checked["sticky"]), ("firing", None, False))

    def test_the_failing_step_rides_beside_a_step_failed_reason(self):
        self.tick.update(ok=False, failed_in_a_row=3, reason="step_failed", step="handoff/read-journals",
                         same_error_in_a_row=1, failed_last_hour=3, last_hour_reason="step_failed",
                         last_hour_step="handoff/read-journals")
        checked = self.checks()["controller_tick"]
        self.assertEqual((checked["reason"], checked["step"]), ("step_failed", "handoff/read-journals"))
        # Another class carries none, and a collector from before the field has none to show.
        self.tick.update(reason="cap_reached", step=None)
        self.assertNotIn("step", self.checks(60)["controller_tick"])
        del self.tick["step"], self.tick["last_hour_step"]
        self.assertNotIn("step", self.checks(120)["controller_tick"])

    def test_alternating_failures_never_fire_and_keep_the_reason_when_they_do(self):
        # Failures that alternate with good ticks make no streak and no 30 minutes without a success:
        # nothing fires (the four-an-hour rule is gone, #2054), and the hour's class stays in the line.
        for elapsed, ok, streak, reason in ((0, False, 1, "handoff_failed"), (300, True, 0, None),
                                            (600, False, 1, "handoff_failed"), (900, True, 0, None)):
            self.tick.update(ok=ok, failed_in_a_row=streak, reason=reason, same_error_in_a_row=streak,
                             failed_last_hour=5, last_hour_reason="handoff_failed", last_ok_at=NOW + elapsed - 200)
            checked = self.checks(elapsed)["controller_tick"]
            self.assertEqual(checked["state"], "healthy")
            self.assertEqual(checked["failed_last_hour"], 5)
        # Three in a row fire, named by the newest failure's class.
        self.tick.update(ok=False, failed_in_a_row=3, reason="handoff_failed", same_error_in_a_row=3)
        checked = self.checks(960)["controller_tick"]
        self.assertEqual((checked["state"], checked["reason"]), ("firing", "handoff_failed"))

    def test_a_cap_reports_its_rate_of_growth_and_the_hours_left(self):
        self.tick["caps"] = {"dispatch_journals": {"count": 9210, "limit": 10000}}
        self.tick["cap_rates"] = {"dispatch_journals": 150.0}
        checked = self.checks()["controller_cap"]
        self.assertEqual((checked["state"], checked["per_hour"], checked["hours_to_cap"]), ("firing", 150.0, 5.3))
        # Falling, or no rate yet: no hours to the cap.
        self.tick["cap_rates"] = {"dispatch_journals": -40.0}
        # Headroom is from the count at the rate's last warning (9,000), not the window's peak (9,210).
        self.tick["cap_latest"] = {"dispatch_journals": 9000}
        self.tick["cap_rates"] = {"dispatch_journals": 450.0}
        checked = self.checks(30)["controller_cap"]
        self.assertEqual((checked["current"], checked["hours_to_cap"]), (9000, 2.2))
        self.tick.pop("cap_latest")
        self.tick["cap_rates"] = {"dispatch_journals": -40.0}
        checked = self.checks(60)["controller_cap"]
        self.assertEqual((checked["per_hour"], checked["hours_to_cap"]), (-40.0, None))
        self.tick["cap_rates"] = {}
        checked = self.checks(120)["controller_cap"]
        self.assertEqual((checked["state"], checked["per_hour"], checked["hours_to_cap"]), ("firing", None, None))

    def test_excused_controller_checks_stay_unknown_but_send_no_row(self):
        # #1687: a rotation pauses the controller on purpose; an unrelated outage firing collection
        # must not page for the controller's checks too.
        del self.facts["tick"], self.facts["enabled_until"]
        self.facts["sources"] = {"controller": {"observed_at": None, "state": "unknown", "reason": F.NO_RECENT_TICK},
                                 "server": {"observed_at": None, "state": "unknown", "reason": "unavailable_or_invalid"}}
        self.facts["errors"] = ["controller", "server"]
        rotation = {"reason": "#529 claim", "started_at": NOW, "until": NOW + 3600}
        checks = self.checks(rotation=rotation)
        for code in ("controller_tick", *F.CONTROLLER_SIGNAL_CHECKS):
            self.assertEqual((checks[code]["state"], checks[code].get("excused")), ("unknown", True))
            self.assertNotIn("cause", checks[code])
        self.assertEqual(checks["qwen_refill_window"]["cause"], "source:server")
        self.assertNotIn("excused", checks["qwen_refill_window"])
        # Without the window the controller's checks are a source:controller row as usual.
        checks = self.checks(60)
        self.assertEqual((checks["controller_tick"].get("excused"), checks["controller_tick"]["cause"]),
                         (None, "source:controller"))
        module = (ROOT / "infra/modules/boinc-feed-alerts.bicep").read_text()
        self.assertIn("(State == 'unknown' and CollectionFiring and tostring(Check.excused) != 'true')", module)

    def test_signal_checks_are_opt_in(self):
        del self.facts["controller_signals"]
        self.assertFalse(set(F.CONTROLLER_SIGNAL_CHECKS) & set(self.checks()))
        self.facts["controller_signals"] = True
        checks = self.checks(60)
        self.assertEqual({code: checks[code]["state"] for code in F.CONTROLLER_SIGNAL_CHECKS},
                         dict.fromkeys(F.CONTROLLER_SIGNAL_CHECKS, "healthy"))
        self.assertEqual(checks["collection"]["state"], "healthy")

    def test_a_collector_without_signals_is_not_reported_and_no_controller_source_is_unknown(self):
        for field in F.SIGNAL_FIELDS:
            del self.tick[field]
        checks = self.checks()
        for code in F.CONTROLLER_SIGNAL_CHECKS:
            self.assertEqual((checks[code]["state"], checks[code]["reason"]), ("unknown", "not_reported"))
            self.assertNotIn("cause", checks[code])
        del self.facts["tick"]
        self.facts["sources"] = {"controller": {"observed_at": None, "state": "unknown",
                                                "reason": "unavailable_or_invalid"}}
        self.facts["errors"] = ["controller"]
        checks = self.checks(60)
        for code in ("controller_tick", *F.CONTROLLER_SIGNAL_CHECKS):
            self.assertEqual((checks[code]["state"], checks[code]["cause"]), ("unknown", "source:controller"))
        self.assertEqual(checks["collection"]["uncovered"], 0)

    def test_a_cap_fires_at_85_percent_and_names_the_worst(self):
        self.tick["caps"] = {"claims": {"count": 8100, "limit": 10000},
                             "dispatch_journals": {"count": 8499, "limit": 10000}}
        checked = self.checks()["controller_cap"]
        self.assertEqual((checked["state"], checked["cap"], checked["percent"]), ("healthy", "dispatch_journals", 85.0))
        self.tick["caps"]["dispatch_journals"]["count"] = 8500
        checked = self.checks(60)["controller_cap"]
        self.assertEqual((checked["state"], checked["percent"]), ("firing", 85.0))
        self.tick["caps"]["bridge_journals"] = {"count": 9608, "limit": 10000}
        checked = self.checks(120)["controller_cap"]
        self.assertEqual((checked["state"], checked["cap"], checked["percent"]), ("firing", "bridge_journals", 96.1))
        self.assertIn('boinc_feed_percent{check="controller_cap"} 96.1', F.metrics(self.document))

    def test_ownership_conflicts_fire_at_once(self):
        self.tick["ownership_conflicts"] = 3
        checked = self.checks()["ownership_conflict"]
        self.assertEqual((checked["state"], checked["ownership_conflicts"]), ("firing", 3))
        self.tick["ownership_conflicts"] = 0
        self.assertEqual(self.checks(60)["ownership_conflict"]["state"], "healthy")
        self.tick["ownership_conflicts"] = None
        self.assertEqual(self.checks(120)["ownership_conflict"]["state"], "unknown")

    def test_no_publication_for_two_hours_with_pending_work_fires(self):
        progress = self.tick["progress"]
        progress.update(published_at=NOW - F.PROGRESS_STALL_S + 1)
        self.assertEqual(self.checks()["controller_progress"]["state"], "healthy")
        progress.update(published_at=NOW - F.PROGRESS_STALL_S)
        self.assertEqual(self.checks(0)["controller_progress"]["state"], "firing")
        # None in the window: only a window that reaches back far enough proves it.
        progress.update(published_at=None, first_tick_at=NOW - F.PROGRESS_COVERAGE_S + 1)
        self.assertEqual(self.checks(0)["controller_progress"]["state"], "healthy")
        progress.update(first_tick_at=NOW - F.PROGRESS_COVERAGE_S)
        self.assertEqual(self.checks(0)["controller_progress"]["state"], "firing")
        # Nothing pending is nothing to publish.
        progress.update(pending=0)
        self.assertEqual(self.checks(0)["controller_progress"]["state"], "healthy")
        # A rotation window holds it, and it fires at the end if still true.
        progress.update(pending=5)
        rotation = {"reason": "#529 claim", "started_at": NOW, "until": NOW + 120}
        checked = self.checks(60, rotation=rotation)["controller_progress"]
        self.assertEqual((checked["state"], checked["rotation_until"]), ("pending", NOW + 120))
        self.assertEqual(self.checks(120)["controller_progress"]["state"], "firing")

    def test_a_tick_trend_fires_on_a_median_over_twice_the_baseline_held_for_an_hour(self):
        limit = F.TICK_TREND_FACTOR * F.TICK_BASELINE_P50_S
        self.assertEqual((F.TICK_BASELINE_P50_S, limit), (660, 1320))
        self.tick.update(tick_p50_s=limit, tick_n=3)
        self.assertEqual(self.checks()["tick_trend"]["state"], "healthy")
        self.tick.update(tick_p50_s=limit + 1, tick_n=3)
        for at in range(0, F.TICK_TREND_HOLD_S, 60):
            checked = self.checks(at)["tick_trend"]
        self.assertEqual((checked["state"], checked["p50_s"], checked["threshold_p50_s"]),
                         ("pending", 1321, 1320))
        self.assertEqual(self.checks(F.TICK_TREND_HOLD_S)["tick_trend"]["state"], "firing")
        # One tick in the hour is no median; none is controller_tick's to report; the old p90 alone is nothing.
        for change in ({"tick_n": 1}, {"tick_p50_s": None, "tick_n": 0}):
            self.state = {}
            self.tick.update({"tick_p50_s": limit + 1, "tick_n": 3, **change})
            for at in range(0, F.TICK_TREND_HOLD_S + 60, 60):
                checked = self.checks(at)["tick_trend"]
            self.assertEqual(checked["state"], "healthy")
        self.state = {}
        for key in ("tick_p50_s", "tick_n"):
            self.tick.pop(key, None)
        self.tick["tick_p90_s"] = 5000.0
        for at in range(0, F.TICK_TREND_HOLD_S + 60, 60):
            checked = self.checks(at)["tick_trend"]
        self.assertEqual(checked["state"], "healthy")


class CauseTest(unittest.TestCase):
    """#1698: an unknown check names the source it waits on, so one dead source is one alert."""

    def setUp(self):
        self.facts = {"backlogs": {"usnm_ocr": 0, "usnm_qwen": 0},
                      "tick": {"ok": True, "seconds": 30, "finished_at": NOW, "maintenance_failure_at": 0,
                               "failed_in_a_row": 0},
                      "sources": {"server": {"observed_at": None, "state": "unknown", "reason": "unavailable_or_invalid"},
                                  "controller": {"observed_at": NOW, "state": "fresh"},
                                  "backlog": {"observed_at": NOW, "state": "fresh"},
                                  "client_0": {"observed_at": NOW, "state": "fresh"}},
                      "errors": ["server"]}

    def evaluate(self, previous=None):
        document, state = F.evaluate(self.facts, previous or {}, NOW, 10000, "boot", "cfg")
        return {c["code"]: c for c in document["checks"]}, state

    def test_a_dead_server_tags_every_check_it_feeds_and_covers_collection(self):
        checks, _ = self.evaluate()
        server = ("qwen_refill_window", "usnm_ocr_no_work", "usnm_qwen_no_work", "cpu_shortage", "gpu_shortage")
        for code in server:
            self.assertEqual((checks[code]["state"], checks[code]["cause"]), ("unknown", "source:server"))
        self.assertEqual(checks["controller_tick"]["state"], "healthy")
        self.assertNotIn("cause", checks["controller_tick"])
        # The first evaluation is also a reset, which no cause row names.
        self.assertEqual(checks["collection"]["uncovered"], 1)
        _, state = self.evaluate()
        document, _ = F.evaluate(self.facts, state, NOW + 60, 10060, "boot", "cfg")
        collection = next(c for c in document["checks"] if c["code"] == "collection")
        self.assertEqual((collection["state"], collection["uncovered"]), ("pending", 0))

    def test_backlog_and_clients_and_errors_no_source_explains(self):
        self.facts["sources"]["server"] = {"observed_at": NOW, "state": "fresh"}
        self.facts.update(enabled_until=NOW + 86400, last_workunits={"usnm_ocr": NOW, "usnm_qwen": NOW})
        self.facts["sources"]["backlog"] = {"observed_at": None, "state": "unknown", "reason": "not_configured"}
        self.facts["sources"]["client_0"] = {"observed_at": None, "state": "unknown", "reason": "unavailable_or_invalid"}
        del self.facts["backlogs"]
        self.facts["errors"] = ["backlog", "client_0", "gpu_gpu_memory_packing"]
        checks, state = self.evaluate()
        self.assertEqual(checks["usnm_ocr_no_work"]["cause"], "source:backlog")
        self.assertEqual(checks["cpu_shortage"]["cause"], "source:clients")
        checks, _ = self.evaluate({**state, "time": NOW - 60, "monotonic": 9940})
        # The GPU model's own unknown is in no cause row: collection still names it.
        self.assertEqual(checks["collection"]["uncovered"], 1)
        # Under qwen_admission the Qwen check doesn't wait on the backlog source.
        self.facts["qwen_admission"] = True
        checks, _ = self.evaluate()
        self.assertEqual(checks["usnm_qwen_no_work"]["state"], "unknown")
        self.assertNotIn("cause", checks["usnm_qwen_no_work"])

    def test_no_own_clients_configured_is_the_clients_cause(self):
        self.facts["sources"]["server"] = {"observed_at": NOW, "state": "fresh"}
        self.facts.update(enabled_until=NOW + 86400, last_workunits={"usnm_ocr": NOW, "usnm_qwen": NOW},
                          supply_errors={"cpu": "no_own_clients_configured", "gpu": "no_own_clients_configured"},
                          errors=["cpu_no_own_clients_configured", "gpu_no_own_clients_configured"])
        del self.facts["sources"]["client_0"]
        _, state = self.evaluate()
        checks, _ = self.evaluate({**state, "time": NOW - 60, "monotonic": 9940})
        self.assertEqual(checks["gpu_shortage"]["cause"], "source:clients")
        self.assertEqual(checks["collection"]["uncovered"], 0)


class CollectionDecayTest(unittest.TestCase):
    """#1698: a healthy evaluation runs the collection hold down at half rate, not to zero."""

    def run_pattern(self, pattern, minutes):
        facts = {"enabled_until": NOW + 86400, "backlogs": {"usnm_ocr": 0, "usnm_qwen": 0},
                 "last_workunits": {"usnm_ocr": NOW, "usnm_qwen": NOW},
                 "supply": {"cpu": {"idle": 0, "eligible": 0}, "gpu": {"idle": 0, "eligible": 0}}}
        state, states = {}, []
        for minute in range(minutes):
            healthy = pattern[minute % len(pattern)] == "h"
            tick = {"ok": True, "seconds": 1, "finished_at": NOW, "maintenance_failure_at": 0,
                    "failed_in_a_row": 0}
            document, state = F.evaluate(facts | ({"tick": tick} if healthy else {}), state,
                                         NOW + 60 * minute, 10000 + 60 * minute, "boot", "cfg")
            states.append(next(c for c in document["checks"] if c["code"] == "collection")["state"])
        return states

    def test_a_source_down_most_minutes_fires_and_single_blips_never_do(self):
        self.assertIn("firing", self.run_pattern("huu", 30))
        self.assertNotIn("firing", self.run_pattern("hhhhhhhhhu", 120))
        self.assertNotIn("firing", self.run_pattern("hhhhhu", 120))


class DatabaseBackupTest(unittest.TestCase):
    """db_replica, db_dump, db_binlog and db_drill from the dbbackup source."""

    def setUp(self):
        self.state = {}
        self.facts = {
            "enabled_until": NOW + 86400,
            "backlogs": {"usnm_ocr": 0, "usnm_qwen": 0},
            "last_workunits": {"usnm_ocr": NOW, "usnm_qwen": NOW},
            "supply": {"cpu": {"idle": 1, "eligible": 1}, "gpu": {"idle": 1, "eligible": 1}},
            "tick": {"ok": True, "seconds": 60},
            "sources": {"dbbackup": {"state": "fresh", "observed_at": NOW}},
            "dbbackup_checks": True,
            "dbbackup": {
                "replica": {"io_running": True, "sql_running": True, "lag_s": 0},
                "r2": {"newest_dump_at": NOW - 3600, "newest_binlog_at": NOW - 120, "checked_at": NOW},
                "drill": {"pass": True, "finished_at": NOW - 86400},
            },
        }

    def tick(self, elapsed):
        document, self.state = F.evaluate(self.facts, self.state, NOW + elapsed, 10000 + elapsed, "boot", "cfg")
        return {c["code"]: c for c in document["checks"]}

    def test_healthy_and_off_without_the_source(self):
        checks = self.tick(0)
        for code in F.DB_CHECKS:
            self.assertEqual(checks[code]["state"], "healthy", code)
        self.assertEqual(checks["db_binlog"]["seconds"], 120)
        self.facts["dbbackup_checks"] = False
        self.state = {}
        self.assertFalse(set(F.DB_CHECKS) & set(self.tick(0)))

    def test_stopped_threads_fire_after_five_minutes(self):
        self.facts["dbbackup"]["replica"]["io_running"] = False
        self.assertEqual(self.tick(0)["db_replica"]["state"], "pending")
        for at in range(60, 300, 60):
            self.tick(at)
        checks = self.tick(300)
        self.assertEqual((checks["db_replica"]["state"], checks["db_replica"]["reason"]),
                         ("firing", "replica_io_stopped"))
        self.facts["dbbackup"]["replica"]["sql_running"] = True
        self.facts["dbbackup"]["replica"]["io_running"] = True
        self.assertEqual(self.tick(360)["db_replica"]["state"], "healthy")
        # SQL stopped throughout while IO flaps: one timer, so it still fires at five minutes.
        replica = self.facts["dbbackup"]["replica"]
        replica.update(sql_running=False, lag_s=None)
        for at in range(420, 720, 60):
            replica["io_running"] = (at // 60) % 2 == 0
            self.tick(at)
        replica["io_running"] = True
        checks = self.tick(720)
        self.assertEqual((checks["db_replica"]["state"], checks["db_replica"]["reason"]),
                         ("firing", "replica_sql_stopped"))
        replica.update(sql_running=True, lag_s=0)
        self.assertEqual(self.tick(780)["db_replica"]["state"], "healthy")
        # Lag over the limit fires at once, even with a thread stopped.
        self.facts["dbbackup"]["replica"].update(io_running=False, lag_s=301)
        self.assertEqual((self.tick(840)["db_replica"]["state"], self.tick(900)["db_replica"]["reason"]),
                         ("firing", "replica_lag"))

    def test_each_age_and_lag_limit(self):
        db = self.facts["dbbackup"]
        db["replica"]["lag_s"] = 301
        db["r2"].update(newest_dump_at=NOW - 26 * 3600 - 1, newest_binlog_at=NOW - 901)
        db["drill"]["finished_at"] = NOW - 35 * 86400 - 1
        checks = self.tick(0)
        self.assertEqual({code: (checks[code]["state"], checks[code]["reason"]) for code in F.DB_CHECKS}, {
            "db_replica": ("firing", "replica_lag"), "db_dump": ("firing", "dump_stale"),
            "db_binlog": ("firing", "binlog_stale"), "db_drill": ("firing", "drill_stale")})
        db["replica"]["lag_s"] = 300
        db["r2"].update(newest_dump_at=NOW - 26 * 3600 + 60, newest_binlog_at=NOW - 840)
        db["drill"]["finished_at"] = NOW - 35 * 86400 + 60
        checks = self.tick(60)
        self.assertTrue(all(checks[code]["state"] == "healthy" for code in F.DB_CHECKS))
        db["drill"]["pass"] = False
        self.assertEqual(self.tick(120)["db_drill"]["reason"], "drill_failed")

    def test_missing_objects_fire_and_part_failures_are_unknown(self):
        db = self.facts["dbbackup"]
        db["r2"].update(newest_dump_at=None, newest_binlog_at=None)
        db["drill"] = {"error": "drill_missing"}
        checks = self.tick(0)
        self.assertEqual((checks["db_dump"]["state"], checks["db_dump"]["reason"]), ("firing", "dump_missing"))
        self.assertEqual(checks["db_binlog"]["reason"], "binlog_missing")
        self.assertEqual((checks["db_drill"]["state"], checks["db_drill"]["reason"]), ("firing", "drill_missing"))
        db["replica"] = {"error": "replica_unavailable"}
        db["r2"] = {"error": "r2_unavailable"}
        db["drill"] = {"error": "drill_unreadable"}
        checks = self.tick(60)
        for code, reason in (("db_replica", "replica_unavailable"), ("db_dump", "r2_unavailable"),
                             ("db_binlog", "r2_unavailable"), ("db_drill", "drill_unreadable")):
            self.assertEqual((checks[code]["state"], checks[code]["reason"]), ("unknown", reason))
            self.assertNotIn("cause", checks[code])

    def test_a_dead_source_is_one_cause(self):
        del self.facts["dbbackup"]
        self.facts["sources"]["dbbackup"] = {"state": "unknown", "observed_at": None, "reason": "unavailable_or_invalid"}
        self.facts["errors"] = ["dbbackup"]
        self.tick(0)
        checks = self.tick(60)  # past the first evaluation's reset
        for code in F.DB_CHECKS:
            self.assertEqual((checks[code]["state"], checks[code]["cause"]), ("unknown", "source:dbbackup"))
        self.assertEqual(checks["collection"]["uncovered"], 0)

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
                      # Two idle GPU slots and nothing eligible for them: hosts asking for work and idle.
                      "supply": {"cpu": {"idle": 0, "eligible": 0}, "gpu": {"idle": 2, "eligible": 0}},
                      "tick": {"ok": True, "seconds": 10}}

    def check(self, elapsed):
        self.cohort["last_completed_at"] = self.cohort["report_at"] = NOW + elapsed
        document, self.state = F.evaluate(self.facts, self.state, NOW + elapsed, 10000 + elapsed, "boot", "cfg")
        return next(c for c in document["checks"] if c["code"] == "usnm_qwen_no_work")

    def evaluate_with(self, change, at, state):
        """One evaluation `at` seconds in, with the cohort in the same trouble as at 0: its
        timestamps move on with the clock, so a stalled run stays as stale as it was."""
        cohort = {**self.cohort, "last_completed_at": NOW + at, "report_at": NOW + at, **change}
        for key in ("last_completed_at", "report_at"):
            if cohort[key] is not None and key in change:
                cohort[key] += at
        self.facts["cohort"] = cohort
        return F.evaluate(self.facts, state, NOW + at, 10000 + at, "boot", "cfg")

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
                       # General admission everywhere: the refill owns no window and reports no budget.
                       {"status": "no-owned-window", "admitted_unique": 0, "limit": 0, "fresh_pairs_in_scan": None,
                        "admissible_backlog": None, "native_gpu_inflight": None, "per_host_buffer_limit": None,
                        "currently_eligible_unsent_replicas": None},
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
                # GPU hosts asking for work sit idle: pending for the hour, then firing.
                document, state = self.evaluate_with(change, 0, {})
                checked = next(c for c in document["checks"] if c["code"] == "usnm_qwen_no_work")
                self.assertEqual((checked["state"], checked["reason"]), ("pending", reason))
                for at in range(60, 3600, 60):
                    document, state = self.evaluate_with(change, at, state)
                document, state = self.evaluate_with(change, 3600, state)
                checked = next(c for c in document["checks"] if c["code"] == "usnm_qwen_no_work")
                self.assertEqual((checked["state"], checked["reason"]), ("firing", reason))
                # The same trouble with no GPU host idle, or none proven, never fires.
                for supply in ({"idle": 0, "eligible": 0}, {"idle": 2, "eligible": 2}, None):
                    self.setUp()
                    if supply is None:
                        del self.facts["supply"]["gpu"]
                    else:
                        self.facts["supply"]["gpu"] = supply
                    state = {}
                    for at in range(0, 3601, 60):
                        document, state = self.evaluate_with(change, at, state)
                    checked = next(c for c in document["checks"] if c["code"] == "usnm_qwen_no_work")
                    self.assertEqual(checked["state"], "healthy", supply)
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

    def test_the_workunit_path_is_gated_on_idle_gpu_hosts_too(self):
        del self.facts["qwen_admission"]
        self.facts["backlogs"]["usnm_qwen"] = 5
        for supply, expected in (({"idle": 2, "eligible": 0}, "firing"), ({"idle": 0, "eligible": 0}, "healthy")):
            self.facts["supply"]["gpu"] = supply
            state = {}
            for at in range(0, 3601, 60):
                document, state = F.evaluate(self.facts, state, NOW + at, 10000 + at, "boot", "cfg")
            checked = next(c for c in document["checks"] if c["code"] == "usnm_qwen_no_work")
            self.assertEqual(checked["state"], expected, supply)

    def test_without_the_opt_in_the_workunit_check_is_unchanged(self):
        del self.facts["qwen_admission"]
        document, _ = F.evaluate(self.facts, {}, NOW, 10000, "boot", "cfg")
        self.assertEqual(next(c for c in document["checks"] if c["code"] == "usnm_qwen_no_work")["state"],
                         "unknown")
