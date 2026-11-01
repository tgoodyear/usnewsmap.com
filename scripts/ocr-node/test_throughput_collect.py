from datetime import datetime, timezone
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch


spec = importlib.util.spec_from_file_location("throughput_collect", Path(__file__).with_name("throughput-collect.py"))
collector = importlib.util.module_from_spec(spec)
spec.loader.exec_module(collector)

# 2026-11-01 12:30 UTC.
NOW = 1793536200
HOUR = 3600
# The last complete hour: 11:00.
THIS_HOUR = NOW // HOUR * HOUR - HOUR
QUALITY = "usnm-quality-ship.service"
QWEN = "usnm-qwen-ship.service"


def window(hours=3):
    return collector.Window(NOW, hours)


def tick(at, plan="p", selected=10, published=2, seconds=30.0):
    return json.dumps({"event": "boinc_batch_tick", "at": at, "plan": plan, "selected": selected,
                       "newly_published": published, "high_watermark": 300, "max_global_pending": 2000,
                       "timings": {"tick_seconds": seconds}})


class ControllerTests(unittest.TestCase):
    def test_ticks_phases_and_page_counts(self):
        w = window()
        at = THIS_HOUR - HOUR + 10
        entries = [(at, "", line) for line in (
            "Starting usnm-boinc-priority-controller.service...",
            tick(at, selected=100, published=1),
            tick(at + 300, selected=130, published=25, seconds=70.5),
            json.dumps({"event": "boinc_batch_phase", "at": at, "phase": "submit", "ok": False, "seconds": 12.5}),
            json.dumps({"event": "boinc_batch_failure", "at": at + 600, "error": "bridge failed",
                        "timings": {"tick_seconds": 5.0}}),
        )]
        limits = collector.collect_controller(w, entries)
        series = w.series
        self.assertEqual(series["ticks_ok"][1], 2)
        self.assertEqual(series["ticks_failed"][1], 1)
        self.assertEqual(series["tick_seconds"][1], 105.5)
        self.assertEqual(series["ocr_collect_pages"][1], 26)
        # The first tick only sets the base: its 100 pages aren't this hour's.
        self.assertEqual(series["ocr_claim_pages"][1], 30)
        self.assertEqual(series["phase_seconds.submit"][1], 12.5)
        self.assertEqual(series["phase_failed.submit"][1], 1)
        self.assertEqual(series["ticks_ok"][2], 0)
        self.assertIn("300 runnable", limits["usnm_ocr"])

    def test_a_failed_tick_counts_under_the_kind_of_its_error(self):
        w = window()
        at = THIS_HOUR + 10
        errors = ("bridge/batch-inventory failed; inspect the execution privately before retrying",
                  "bridge/batch-inventory failed; inspect the execution privately before retrying",
                  "terminal BOINC failure: sn123/1900-01-01/ed-1/seq-1 (job a1), sn123/2",
                  "can't read /tank/ocr/scans/x.json",
                  "ssh to 192.0.2.7 failed",
                  "tank/ocr/scans/x failed",
                  None)
        collector.collect_controller(w, [(at, "", json.dumps({
            "event": "boinc_batch_failure", "at": at, "error": e})) for e in errors])
        reasons = {name: values[2] for name, values in w.series.items() if name.startswith("reason.")}
        self.assertEqual(reasons, {"reason.controller.bridge-batch-inventory-failed": 2,
                                   "reason.controller.terminal-boinc-failure": 1,
                                   "reason.controller.other": 4})

    def test_a_kind_of_error_depends_only_on_its_text(self):
        for error in ("a brand new error", "bridge/a b failed", "failed at 192.0.2.7", "x" * 500, None):
            self.assertEqual(collector.reason_series(error), "reason.controller.other")
        self.assertEqual(collector.reason_series("claimed a page already in this plan with a different message"),
                         "reason.controller.claimed-a-page-already-in-this-plan-with-a-dif")
        self.assertTrue(all(len(collector.reason_series(k)) <= 64 for k in collector.KNOWN_FAILURES))

    def test_a_tick_across_two_hours_is_busy_in_both(self):
        w = window()
        ended = THIS_HOUR + 100
        collector.collect_controller(w, [(ended, "", tick(ended - 500, seconds=400.0))])
        self.assertEqual(w.series["tick_busy_seconds"][1], 300)
        self.assertEqual(w.series["tick_busy_seconds"][2], 100)
        self.assertEqual(w.series["tick_seconds"][2], 400)

    def test_the_last_passing_tick_says_how_near_its_limits_it_was(self):
        at = THIS_HOUR + 10
        event = json.loads(tick(at))
        event |= {"global_pending": 1939, "backpressure": True, "cohort": {
            "app": "usnm_ocr", "runnable_pending": 225, "low_watermark": 220}}
        limits = collector.collect_controller(window(), [(at, "", json.dumps(event))])
        self.assertIn("225 runnable", limits["usnm_ocr.now"])
        self.assertIn("1939 pending", limits["ocr_claim.now"])
        self.assertIn("held back", limits["ocr_claim.now"])
        limits = collector.collect_controller(window(), [(at, "", tick(at))])
        self.assertNotIn("usnm_ocr.now", limits)
        self.assertNotIn("ocr_claim.now", limits)

    def test_a_tick_counts_in_the_hour_it_ended(self):
        w = window()
        began = THIS_HOUR - 300
        collector.collect_controller(w, [(THIS_HOUR + 300, "", tick(began, published=40, seconds=600))])
        self.assertEqual(w.series["ticks_ok"], [0, 0, 1])
        self.assertEqual(w.series["ocr_collect_pages"], [0, 0, 40])
        self.assertEqual(w.series["tick_seconds"], [0, 0, 600])

    def test_a_new_plan_is_not_counted_as_claims(self):
        w = window()
        at = THIS_HOUR + 1
        collector.collect_controller(w, [(at, "", tick(at, plan="a", selected=500)),
                                         (at, "", tick(at + 300, plan="b", selected=20))])
        self.assertEqual(w.series["ocr_claim_pages"][2], 0)

    def test_page_text_like_lines_and_bad_phase_names_are_dropped(self):
        w = window()
        collector.collect_controller(w, [(0, "", "not json {"), (0, "", '{"event":"boinc_batch_phase","at":1}'),
                                         (0, "", json.dumps({"event": "boinc_batch_phase", "at": NOW,
                                                             "phase": "Bad Name; drop", "seconds": 1}))])
        self.assertFalse([name for name in w.series if name.startswith("phase_")])


class ShipTests(unittest.TestCase):
    def entries(self):
        base = THIS_HOUR - HOUR
        return [
            (base + 10, QUALITY, "quality-ship: c00178: 200 pages, server unsent 0"),
            (base + 310, QUALITY, "quality-ship: c00178: received 200 pages (deu,eng); staging"),
            (base + 900, QUALITY, "quality-ship: c00178: staged; submitting"),
            (base + 1000, QUALITY, "quality-ship: c00178: submitted, freed 717729552 bytes for 200 pages on "
                                   "the server, and removed from the outbox"),
            # A Qwen chunk retried: it counts once, and a deferral is counted apart.
            (base + 20, QWEN, "quality-ship: c00081: 24 pages, server unsent native-fresh-pair-gated"),
            (base + 30, QWEN, "quality-ship: c00081: received 24 pages (srp); staging"),
            (base + 40, QWEN, "quality-ship: c00081: staged; submitting"),
            (base + 50, QWEN, "quality-ship: c00081: native publication deferred; READY retained; no acknowledgement"),
            (base + 60, QWEN, "quality-ship: c00081: received 24 pages (srp); staging"),
            (base + 70, QWEN, "quality-ship: c00081: staged; submitting"),
            (base + 75, collector.QUALITY_LOAD_UNIT, "Starting usnm-quality-results.service - results..."),
            (base + 76, collector.QUALITY_LOAD_UNIT, json.dumps(
                {"campaign": "w", "pages": 0, "reader_rows": 0, "selected_unit": "bundle-1"})),
            (base + 85, collector.QUALITY_LOAD_UNIT, json.dumps(
                {"campaign": "w", "pages": 200, "reader_rows": 1, "selected_unit": "bundle-1"})),
            (base + 86, collector.QUALITY_LOAD_UNIT, json.dumps(
                {"campaign": "w", "pages": 200, "reader_rows": 1, "selected_unit": "bundle-1"})),
            (base + 87, collector.QUALITY_LOAD_UNIT, "usnm-quality-results.service: Deactivated successfully."),
            (base + 90, collector.QWEN_LOAD_UNIT, "Summary { pages: 2094, with_readings: 70, pending: 0 }"),
            (base + 91, collector.QWEN_LOAD_UNIT, json.dumps({"loaded_at": 1.0})),
            (base + 1000, collector.QWEN_LOAD_UNIT, "Starting usnm-qwen-results.service - results..."),
            # This run's load fails: its summary doesn't count, and the next run's does.
            (base + 1010, collector.QWEN_LOAD_UNIT, "Summary { pages: 2094, with_readings: 75, pending: 0 }"),
            (base + 1020, collector.QWEN_LOAD_UNIT,
             "usnm-qwen-results.service: Failed with result 'exit-code'."),
            (base + 1800, collector.QWEN_LOAD_UNIT, "Starting usnm-qwen-results.service - results..."),
            (base + 1810, collector.QWEN_LOAD_UNIT, "Summary { pages: 2094, with_readings: 78, pending: 0 }"),
            (base + 1830, collector.QWEN_LOAD_UNIT, json.dumps({"loaded_at": 1.5})),
            (base + 1835, collector.QWEN_LOAD_UNIT, "usnm-qwen-results.service: Deactivated successfully."),
        ]

    def test_chunks_and_loads_are_counted_once_in_their_hour(self):
        w = window()
        collector.collect_ship(w, self.entries())
        at = 1
        s = w.series
        self.assertEqual(s["quality_ship_pages"][at], 200)
        self.assertEqual(s["quality_ship_seconds"][at], 300)
        self.assertEqual(s["quality_stage_pages"][at], 200)
        self.assertEqual(s["quality_submit_pages"][at], 200)
        self.assertEqual(s["qwen_ship_pages"][at], 24)
        self.assertEqual(s["qwen_stage_pages"][at], 24)
        self.assertEqual(s["qwen_submit_pages"][at], 0)
        self.assertEqual(s["qwen_deferred"][at], 1)
        self.assertEqual(s["quality_load_pages"][at], 200)
        self.assertEqual(s["quality_load_seconds"][at], 12)
        self.assertEqual(s["qwen_load_pages"][at], 8)
        self.assertEqual(s["qwen_load_failed"][at], 1)
        self.assertEqual(s["qwen_load_runs"][at], 2)
        self.assertEqual(s["qwen_load_seconds"][at], 55)
        self.assertEqual(s["quality_ship_pages"][0], 0)

    def test_a_chunk_number_used_again_after_it_was_submitted_is_a_new_chunk(self):
        w = window()
        base = THIS_HOUR
        line = "quality-ship: c00001: "
        collector.collect_ship(w, [
            (base + 1, QUALITY, line + "100 pages, server unsent 0"),
            (base + 2, QUALITY, line + "received 100 pages (deu); staging"),
            (base + 3, QUALITY, line + "staged; submitting"),
            (base + 4, QUALITY, line + "submitted, freed 5 bytes for 100 pages on the server"),
            (base + 5, QUALITY, line + "50 pages, server unsent 0"),
            (base + 6, QUALITY, line + "received 50 pages (eng); staging"),
            (base + 7, QUALITY, line + "staged; submitting"),
            (base + 8, QUALITY, line + "submitted, freed 5 bytes for 50 pages on the server"),
        ])
        self.assertEqual(w.series["quality_ship_pages"][2], 150)
        self.assertEqual(w.series["quality_stage_pages"][2], 150)
        self.assertEqual(w.series["quality_submit_pages"][2], 150)

    def test_limits_come_from_the_settings_and_nothing_else_is_read(self):
        with tempfile.TemporaryDirectory() as directory:
            env = Path(directory) / "ship.env"
            env.write_text("CAMPAIGN=wave\nENGINE_IMAGE=sha256:abc\nMAX_UNSENT=300\nMAX_CHUNKS=1\n")
            limits = collector.ship_limits(env, "A=1 QWEN_MAX_READY=1000 B=2")
            self.assertEqual(limits["quality_ship"], "1 chunk a run")
            self.assertEqual(limits["usnm_quality"], "300 unsent results before shipping waits")
            self.assertEqual(limits["usnm_qwen"], "1000 ready replicas before shipping defers")
            self.assertNotIn("wave", json.dumps(limits))
            self.assertEqual(collector.ship_limits(Path(directory) / "missing", ""), {})


class BoincTests(unittest.TestCase):
    def test_outcomes_units_and_seconds(self):
        w = window()
        hour = THIS_HOUR - HOUR
        collector.collect_boinc(w, {
            "apps": {"1": "usnm_ocr", "6": "usnm_qwen"},
            "results": [
                [6, hour, 1, 1, 15, 0], [6, hour, 1, 4, 60, 0], [6, hour, 1, 0, 223, 0],
                [6, hour, 1, 2, 3, 0], [6, hour, 0, 2, 4, 0], [1, hour, 1, 1, 9, 2],
            ],
            "units": [[6, hour, 5, 12130], [1, hour, 9, 900]],
        })
        s = w.series
        self.assertEqual((s["qwen_valid"][1], s["qwen_inconclusive"][1], s["qwen_waiting"][1]), (15, 60, 223))
        self.assertEqual((s["qwen_invalid"][1], s["qwen_error"][1], s["qwen_units"][1]), (3, 4, 5))
        self.assertEqual(s["qwen_elapsed_seconds"][1], 12130)
        self.assertEqual(s["ocr_bundles"][1], 2)
        self.assertNotIn("quality_valid", s)
        self.assertEqual(s["qwen_valid"][0], 0)

    def test_the_query_only_reads_aggregates(self):
        upper = collector.PHP.upper()
        self.assertIn("SET SESSION TRANSACTION READ ONLY", upper)
        self.assertEqual(upper.count("MAX_EXECUTION_TIME(5000)"), 2)
        self.assertEqual(upper.count("LIMIT 20000"), 2)
        for word in ("INSERT ", "UPDATE ", "DELETE ", "DROP ", "XML_DOC", "STDERR_OUT"):
            self.assertNotIn(word, upper)

    def test_step_runs_are_the_lock_split_by_hour(self):
        w = window()
        start = THIS_HOUR - 100
        at = lambda epoch: datetime.fromtimestamp(epoch, timezone.utc).strftime("%Y-%m-%dT%H:%M:%S.123456789Z")
        name = "usnm-boinc-step-20261101T115840Z-quality-stage"
        collector.collect_steps(w, [
            {"Name": "usnm-boinc-mysql", "Status": "start", "Time": at(start)},
            # No died event, as after a restart: not counted, and not up to now.
            {"Name": "usnm-boinc-step-20261101T090000Z-collect", "Status": "start", "Time": at(start - 9000)},
            {"Name": name, "Status": "start", "Time": at(start)},
            {"Name": name, "Status": "died", "Time": at(start + 400)},
            {"Name": "usnm-boinc-step-20261101T120900Z-spool-out", "Status": "start",
             "Time": at(THIS_HOUR + 3540)},
        ])
        self.assertAlmostEqual(w.series["lock_seconds"][1], 100, delta=1)
        self.assertAlmostEqual(w.series["lock_seconds"][2], 360, delta=1)
        self.assertAlmostEqual(w.series["step_seconds.quality-stage"][2], 300, delta=1)
        self.assertEqual(w.series["step_runs.quality-stage"][2], 1)
        self.assertAlmostEqual(w.series["step_seconds.spool-out"][2], 60, delta=1)
        self.assertNotIn("step_seconds.collect", w.series)


class DocumentTests(unittest.TestCase):
    def test_hours_before_the_source_begins_are_null_not_zero(self):
        w = window()
        w.add("a", THIS_HOUR)
        w.forget_before(lambda name: True, THIS_HOUR - HOUR + 1)
        document = w.document("ship", {})
        self.assertEqual(document["series"]["a"], [None, None, 1])
        self.assertEqual(document["first_hour"], THIS_HOUR - 2 * HOUR)
        self.assertEqual(document["hours"], 3)
        self.assertEqual(document["observed_at"], "2026-11-01T12:30:00Z")

    def test_the_current_hour_is_left_out_until_it_is_whole(self):
        w = window()
        w.add("a", NOW)
        self.assertEqual(w.series["a"], [0, 0, 0])

    def test_step_series_stop_where_the_journal_begins(self):
        w = window()
        w.add("lock_seconds", THIS_HOUR, 5)
        w.add("qwen_valid", THIS_HOUR - 2 * HOUR, 2)
        w.forget_before(lambda name: name == "lock_seconds", THIS_HOUR - HOUR + 1)
        self.assertEqual(w.series["lock_seconds"], [None, None, 5])
        self.assertEqual(w.series["qwen_valid"], [2, 0, 0])

    def test_an_empty_journal_observed_nothing(self):
        w = window()
        w.add("a", THIS_HOUR)
        w.forget_before(lambda name: True, None)
        self.assertEqual(w.series["a"], [None, None, None])

    def test_journal_entries_about_a_unit_are_that_units(self):
        lines = [json.dumps({"__REALTIME_TIMESTAMP": "5000000", "UNIT": "x.service",
                             "_SYSTEMD_UNIT": "init.scope", "MESSAGE": "x.service: Failed with result 'signal'."}),
                 json.dumps({"__REALTIME_TIMESTAMP": "6000000", "_SYSTEMD_UNIT": "y.service", "MESSAGE": "hi"})]
        with patch.object(collector, "run", return_value="\n".join(lines)):
            entries = list(collector.journal(["x.service"], 1))
        self.assertEqual([entry[1] for entry in entries], ["x.service", "y.service"])

    def test_an_event_outside_the_window_is_ignored(self):
        w = window()
        w.add("a", THIS_HOUR - 3 * HOUR)
        self.assertEqual(w.series["a"], [0, 0, 0])

    def test_run_keeps_a_clean_environment(self):
        with patch.object(collector.subprocess, "run") as run:
            run.return_value.stdout = ""
            list(collector.journal(["x.service"], 7))
        command = run.call_args.args[0]
        self.assertIn("--since=@7", command)
        self.assertEqual(sorted(run.call_args.kwargs["env"]), ["LC_ALL", "PATH", "TZ"])

    def test_a_failed_push_fails_the_run_but_keeps_the_file(self):
        with tempfile.TemporaryDirectory() as directory:
            out = Path(directory) / "tp.json"
            argv = ["x", "--source", "ship", "--out", str(out), "--push-url", "http://dashboard.invalid/"]
            with patch("sys.argv", argv), patch.object(collector, "collect", return_value={"series": {}}), \
                    patch.object(collector.urllib.request, "urlopen", side_effect=OSError("refused")):
                with self.assertRaises(OSError):
                    collector.main()
            self.assertEqual(json.loads(out.read_text()), {"series": {}})


if __name__ == "__main__":
    unittest.main()
