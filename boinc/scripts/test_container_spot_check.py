"""Container OCR spot checks in the controller (#1934, docs/boinc/server/batch-scheduler.md)."""
import copy
import importlib.util
import json
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

SPEC = importlib.util.spec_from_file_location("test_batch_scheduler", Path(__file__).with_name("test_batch_scheduler.py"))
tbs = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(tbs)
batch, NOW = tbs.batch, tbs.NOW

SPOOL = "/project/usnmocr/results/7"
# Pages the first cycle of a plan with batch_size 10 and 4 pages a host sends and publishes.
N = 8


def validate(config):
    """The configuration as load_config reads it from a file."""
    with tempfile.TemporaryDirectory() as tmp:
        path = Path(tmp) / "plan.json"
        path.write_text(json.dumps({k: v for k, v in config.items() if k != "pages"}))
        return batch.load_config(path)


def spot_config(**spec):
    value = tbs.queue_config()
    value.update(batch_size=10, max_pending=40, pages_per_host=4, max_global_pending=1000, spot_check=spec)
    return value


class Always:
    """A draw that always gives `value`."""

    def __init__(self, value):
        self.value = value

    def random(self):
        return self.value


class SpotFake(tbs.QueueFake):
    """A container plan's BOINC side with the bridge's container-spot step: every replay goes from
    host 1 to host 9 unless named in `refused` or `deferred`, and a check returns its job's entry
    in `verdicts` (pending by default)."""

    def __init__(self, waiting):
        super().__init__(waiting)
        self.spot_calls = []
        self.verdicts = {}
        self.refused = set()
        self.deferred = set()
        self.spot_error = None
        self.cpus = None
        self.lock_held = False

    def inventory(self):
        snapshot = super().inventory()
        if self.cpus is not None:
            snapshot["host_cpus"] = {str(host): n for host, n in self.cpus.items()}
        return snapshot

    def container_spot(self, submit, check, active, budget_secs):
        self.actions.append(("container-spot", submit, check))
        if self.lock_held:
            raise batch.StepLockTimeout("bridge/container-spot failed: the BOINC host's step lock stayed held")
        self.spot_calls.append({"submit": submit, "check": check, "active": active, "budget": budget_secs})
        if self.spot_error:
            raise RuntimeError(self.spot_error)

        def made(job):
            status = "deferred" if job in self.deferred else "refused" if job in self.refused else "submitted"
            return {"job": job, "source_host": 1, "target_host": 9, "status": status, "detail": "test"}

        def compared(item):
            job, rest = item.split("=", 1)
            source, target = rest.split(":")[:2]
            return {"job": job, "source_host": int(source), "target_host": int(target),
                    "status": "deferred" if job in self.deferred else self.verdicts.get(job, "pending"),
                    "detail": "test"}

        return {"submitted": [made(job) for job in submit.split(",") if job],
                "checked": [compared(item) for item in check.split(",") if item],
                "nonce": "x", "schema_version": 1}


class ContainerSpotTests(unittest.TestCase):
    def setUp(self):
        self.config = spot_config()
        self.state = {}
        self.fake = SpotFake([tbs.message(n) for n in range(1, 60)])
        self.fake.assertions = []
        self.saved = batch.SPOT_RANDOM
        batch.SPOT_RANDOM = Always(0.0)
        self.addCleanup(setattr, batch, "SPOT_RANDOM", self.saved)

    def tick(self, now=NOW, **kwargs):
        return batch.tick(self.config, self.state, self.fake, lambda: None, now, **kwargs)

    def results_in(self):
        for job in self.fake.jobs.values():
            if job["stage"] == "submitted":
                job["spool"] = SPOOL

    def cycle(self, now=NOW):
        """Send units, bring their results in and publish them, and replay the drawn ones."""
        self.tick(now)
        self.results_in()
        return self.tick(now)

    def spot(self):
        return self.state["container_spot"]

    def test_without_the_setting_nothing_changes(self):
        config = tbs.queue_config()
        fake = SpotFake([tbs.message(n) for n in range(1, 20)])
        fake.assertions = []
        state = {}
        event = batch.tick(config, state, fake, lambda: None, NOW)
        for job in fake.jobs.values():
            job["spool"] = SPOOL
        event = batch.tick(config, state, fake, lambda: None, NOW)
        self.assertNotIn("spot_check", event)
        self.assertNotIn("container_spot", state)
        self.assertEqual(fake.spot_calls, [])
        # The block is outside the allocation digest: turning it on is no migration.
        self.assertEqual(batch.digest(config), batch.digest({**config, "spot_check": {}}))

    def test_the_documented_empty_block_turns_checks_on_at_two_percent(self):
        self.config = validate(spot_config())
        self.assertEqual(batch.CONTAINER_SPOT_SHARE, 0.02)
        for value, drawn in ((0.0199, N), (0.02, 0)):
            with self.subTest(value=value):
                self.state = {}
                self.fake = SpotFake([tbs.message(n) for n in range(1, 60)])
                self.fake.assertions = []
                batch.SPOT_RANDOM = Always(value)
                self.cycle()
                self.assertEqual(self.spot().get("counts", {}).get("drawn", 0), drawn)
                self.assertEqual(self.spot()["counts"]["published"], N)

    def test_settings_are_checked(self):
        for bad in ({"share": 0}, {"share": 0.6}, {"share": "x"}, {"max_per_tick": 0}, {"max_per_tick": 101},
                    {"max_outstanding": 0}, {"budget_secs": 0}, {"budget_secs": 601}, {"budget_secs": 1.5},
                    {"lock_wait_secs": 61}, {"lock_wait_secs": -1}, {"peers": []}):
            with self.subTest(bad=bad):
                with self.assertRaises(ValueError):
                    validate(spot_config(**bad))
        validate(spot_config(share=0.5, max_per_tick=100, max_outstanding=1000, budget_secs=600, lock_wait_secs=0))
        with self.assertRaises(ValueError):
            validate({**spot_config(), "spot_check": None})
        # Native plans have native_spot_check, and it isn't this block.
        native = tbs.native_config((13,))
        native["spot_check"] = {}
        with self.assertRaisesRegex(ValueError, "native_spot_check"):
            validate(native)

    def test_a_published_page_is_drawn_and_replayed_after_submit(self):
        self.cycle()
        (call,) = self.fake.spot_calls
        self.assertEqual(call["check"], "")
        submitted = call["submit"].split(",")
        self.assertEqual(len(submitted), N)
        self.assertEqual(call["budget"], batch.CONTAINER_SPOT_BUDGET_SECS)
        pending = self.spot()["pending"]
        self.assertEqual({e["stage"] for e in pending.values()}, {"submitted"})
        self.assertEqual({(e["source"], e["target"]) for e in pending.values()}, {(1, 9)})
        # Back and within the limits: counted, and the host's last match recorded.
        for job in pending:
            self.fake.verdicts[job] = "match"
        event = self.tick()
        self.assertEqual(self.spot()["counts"]["match"], N)
        self.assertEqual(self.spot()["last_match"], {"1": NOW})
        self.assertEqual(event["spot_check"]["counts"]["match"], N)
        self.assertEqual(event["spot_check"]["pending"], 0)

    def test_only_the_pages_a_tick_published_are_drawn(self):
        self.tick()
        self.assertEqual(self.fake.spot_calls, [])
        self.assertEqual(self.state.get("container_spot", {}).get("pending", {}), {})
        # A held result is not a published page.
        self.fake.scope_held = True
        self.results_in()
        self.tick()
        self.assertEqual(self.fake.spot_calls, [])
        self.assertEqual(self.state.get("container_spot", {}).get("pending", {}), {})

    def test_a_page_a_stopped_tick_published_is_still_drawn_once(self):
        self.tick()
        self.results_in()
        # The tick publishes the pages, then stops before the step: the draw already ran.
        self.fake.container_spot = lambda *a: (_ for _ in ()).throw(KeyboardInterrupt())
        with self.assertRaises(KeyboardInterrupt):
            self.tick()
        self.assertEqual(self.spot()["counts"]["drawn"], N)
        # A later tick doesn't draw them again, and one that lost its draw does.
        del self.fake.container_spot
        self.tick()
        self.assertEqual(self.spot()["counts"]["drawn"], N)
        self.spot()["pending"].clear()
        self.spot()["seen"].clear()
        self.tick()
        self.assertEqual(self.spot()["counts"]["drawn"], 2 * N)

    def test_pages_published_before_the_setting_was_turned_on_are_not_drawn(self):
        config = self.config
        self.config = tbs.queue_config()
        self.config.update(batch_size=10, max_pending=40, pages_per_host=4, max_global_pending=1000)
        self.tick()
        self.results_in()
        self.tick()
        self.config = config
        self.tick()
        self.assertEqual(self.spot()["pending"], {})
        self.assertEqual(self.spot().get("counts", {}).get("drawn", 0), 0)

    def test_a_plan_with_bundles_cannot_have_it(self):
        value = spot_config()
        value.update(pages_per_unit={"ocr-bulk": 4}, bundle_app_version=7)
        with self.assertRaisesRegex(ValueError, "single-page"):
            validate(value)
        validate({**spot_config(), "pages_per_unit": {"ocr-bulk": 1}})

    def test_a_deferred_comparison_keeps_its_place_in_the_rotation(self):
        self.config = spot_config(max_per_tick=2)
        self.cycle()
        pending = self.spot()["pending"]
        for job, entry in pending.items():
            entry.update(stage="submitted", source=1, target=9)
        first = sorted(pending)[:2]
        self.fake.deferred = {first[0]}
        self.tick(NOW)
        self.assertNotIn("polled", pending[first[0]])
        self.assertEqual(pending[first[1]]["polled"], NOW)

    def test_a_mismatch_holds_both_hosts_and_alerts_until_released(self):
        self.cycle()
        for job in self.spot()["pending"]:
            self.fake.verdicts[job] = "mismatch"
        event = self.tick()
        spot = self.spot()
        self.assertEqual(sorted(spot["holds"]), ["1", "9"])
        alerts = [a for a in event["alerts"] if a["code"] == "container_spot_mismatch"]
        self.assertEqual(sorted(a["host"] for a in alerts), [1, 9])
        self.assertEqual(event["spot_check"]["held"], [1, 9])
        self.assertEqual(len(spot["mismatches"]), N)
        # A mismatch is never retried away.
        calls = len(self.fake.spot_calls)
        self.fake.verdicts.clear()
        self.tick()
        self.assertEqual(spot["pending"], {})
        self.assertEqual(spot["counts"]["mismatch"], N)
        self.assertEqual(len(self.fake.spot_calls), calls)
        with self.assertRaisesRegex(ValueError, "note"):
            self.tick(release_container_hold=(1, ""))
        with self.assertRaisesRegex(ValueError, "no container spot hold"):
            self.tick(release_container_hold=(99, "looked"))
        event = self.tick(release_container_hold=(9, "replica disk fault"))
        self.assertEqual(sorted(spot["holds"]), ["1"])
        self.assertEqual(spot["releases"][0]["note"], "replica disk fault")
        self.assertEqual([a["host"] for a in event["alerts"] if a["code"] == "container_spot_mismatch"], [1])

    def test_refused_failed_and_unverified_replays_are_no_verdict(self):
        self.cycle()
        jobs = sorted(self.spot()["pending"])
        for job, status in zip(jobs, ("failed", "unverified")):
            self.fake.verdicts[job] = status
        self.tick()
        spot = self.spot()
        self.assertEqual((spot["counts"]["failed"], spot["counts"]["unverified"]), (1, 1))
        self.assertNotIn("holds", spot)
        self.assertEqual(spot["last_unverified"]["status"], "unverified")
        # A page the step refuses is dropped and counted, with the reason.
        self.fake.refused = {"e" * 64}
        self.spot()["pending"]["e" * 64] = {"spool": SPOOL, "at": NOW, "stage": "queued"}
        self.tick()
        self.assertNotIn("e" * 64, self.spot()["pending"])
        self.assertEqual(self.spot()["counts"]["refused"], 1)
        self.assertEqual(self.spot()["last_refused"]["detail"], "test")

    def test_a_step_that_stops_or_fails_never_fails_the_tick(self):
        self.cycle()
        self.fake.spot_error = "usnm-boinc failed"
        event = self.tick()
        self.assertEqual(event["spot_check"]["step"], "failed")
        self.assertEqual([a["code"] for a in event["alerts"] if a["code"].startswith("container_spot")],
                         ["container_spot_failing"])
        self.fake.spot_error = None
        event = self.tick()
        self.assertEqual(event["spot_check"]["step"], "ran")
        self.assertIsNone(event["spot_check"]["failing"])

        def explode(*args):
            raise TypeError("a bug in the controller's step")
        self.fake.container_spot = explode
        event = self.tick()
        self.assertEqual(event["spot_check"]["step"], "failed")
        self.assertIn("TypeError", self.spot()["failing"]["reason"])

    def test_a_held_step_lock_never_delays_submit(self):
        self.cycle()
        self.results_in()
        self.fake.lock_held = True
        self.fake.actions.clear()
        event = self.tick()
        kinds = [action[0] for action in self.fake.actions]
        # Submit went first and sent work; the spot step came after it and was skipped.
        self.assertIn("submit", kinds)
        self.assertIn("container-spot", kinds)
        self.assertLess(max(i for i, k in enumerate(kinds) if k == "submit"), kinds.index("container-spot"))
        self.assertLess(max(i for i, k in enumerate(kinds) if k == "collect"), kinds.index("container-spot"))
        spot = event["spot_check"]
        self.assertEqual(spot["step"], "skipped")
        self.assertIsNone(spot["failing"])
        self.assertEqual([a["code"] for a in event["alerts"] if a["code"].startswith("container_spot")], [])
        self.assertEqual(spot["lock_busy"], {"since": NOW, "ticks": 1, "last": NOW})

    def test_the_step_asks_for_a_short_lock_wait_and_a_time_bound_of_its_own(self):
        directory = Path(self.enterContext(tempfile.TemporaryDirectory()))
        cases = (({}, "10", "60"), ({"lock_wait_secs": 0, "budget_secs": 30}, "0", "30"),
                 ({"lock_wait_secs": 30, "budget_secs": 600}, "30", "600"))
        for spec, wait, budget in cases:
            with self.subTest(spec=spec):
                config = spot_config(**spec)
                config["step_command"] = ["ssh", "boinc.example", "--"]
                transport = batch.Transport(config, directory, step_lock_wait_secs=1200)
                stderr = "error: another bridge step still holds the lock after 10s\n"
                failed = subprocess.CompletedProcess([], 1, "", stderr)
                with patch.object(batch.subprocess, "run", return_value=failed) as run:
                    with self.assertRaises(batch.StepLockTimeout):
                        transport.container_spot("a" * 64, "", "1,9", int(budget))
                argv, kwargs = run.call_args
                argv = argv[0]
                self.assertEqual(argv[argv.index("--lock-wait-secs") + 1], wait)
                self.assertIn(f"CONTAINER_SPOT_BUDGET_SECS={budget}", argv)
                self.assertIn("CONTAINER_SPOT_ACTIVE=1,9", argv)
                # The run is stopped a few minutes past lock wait plus budget, not after 30 minutes.
                minutes = int(argv[argv.index("--wait-mins") + 1])
                self.assertEqual(minutes, -(-(int(wait) + int(budget)) // 60) + batch.CONTAINER_SPOT_HEADROOM_MINS)
                self.assertEqual(kwargs["timeout"], minutes * 60 + 60)
                # submit-remote keeps the long wait.
                argv, _, _ = transport._step_argv("bridge", "submit-remote", {})
                self.assertEqual(argv[argv.index("--lock-wait-secs") + 1], "1200")

    def test_a_tick_takes_at_most_max_per_tick_items_and_the_rest_wait(self):
        self.config = spot_config(max_per_tick=4)
        self.cycle()
        (call,) = self.fake.spot_calls
        self.assertEqual(len(call["submit"].split(",")), 4)
        pending = self.spot()["pending"]
        self.assertEqual(sorted(e["stage"] for e in pending.values()),
                         ["queued"] * (N - 4) + ["submitted"] * 4)
        # Next tick: the four out are compared first, and the queue gets no room beside them.
        self.fake.spot_calls.clear()
        self.tick()
        (call,) = self.fake.spot_calls
        self.assertEqual((len(call["check"].split(",")), call["submit"]), (4, ""))
        # With them settled, the next four are made.
        for job, entry in list(pending.items()):
            if entry["stage"] == "submitted":
                self.fake.verdicts[job] = "match"
        self.fake.spot_calls.clear()
        self.tick()
        self.tick()
        self.assertEqual([len(c["submit"].split(",")) for c in self.fake.spot_calls if c["submit"]], [4])

    def test_items_the_budget_left_stay_as_they_were(self):
        self.cycle()
        pending = self.spot()["pending"]
        self.fake.deferred = set(pending)
        self.tick()
        self.assertEqual(self.spot()["counts"]["deferred"], N)
        self.assertEqual(len(pending), N)
        self.assertEqual({e["stage"] for e in pending.values()}, {"submitted"})
        # A queued page the budget did not reach is made on a later tick.
        self.state["container_spot"]["pending"] = {}
        self.cycle_state = {"e" * 64: {"spool": SPOOL, "at": NOW, "stage": "queued"}}
        self.spot()["pending"].update(self.cycle_state)
        self.fake.deferred = {"e" * 64}
        self.tick()
        self.assertEqual(self.spot()["pending"]["e" * 64]["stage"], "queued")
        self.fake.deferred = set()
        self.tick()
        self.assertEqual(self.spot()["pending"]["e" * 64]["stage"], "submitted")

    def test_the_draw_never_queues_past_max_outstanding(self):
        self.config = spot_config(max_outstanding=3)
        self.fake.lock_held = True
        self.cycle()
        spot = self.spot()
        self.assertEqual(len(spot["pending"]), 3)
        self.assertEqual(spot["counts"]["backlog"], N - 3)
        self.assertEqual(spot["counts"]["drawn"], N)

    def test_a_skipped_step_is_tried_again_and_a_queued_page_is_given_up_in_time(self):
        self.fake.lock_held = True
        self.cycle()
        queued = copy.deepcopy(self.spot()["pending"])
        self.assertEqual({e["stage"] for e in queued.values()}, {"queued"})
        event = self.tick(NOW + 600)
        self.assertEqual(self.spot()["pending"], queued)
        self.assertEqual(self.spot()["lock_busy"], {"since": NOW, "ticks": 2, "last": NOW + 600})
        self.assertEqual(event["spot_check"]["queued"], N)
        self.fake.lock_held = False
        self.tick(NOW + 1200)
        self.assertEqual({e["stage"] for e in self.spot()["pending"].values()}, {"submitted"})
        self.assertNotIn("lock_busy", self.spot())
        # A page never made is given up after 96 hours.
        old = NOW - batch.CONTAINER_SPOT_MAX_AGE - 1
        self.spot()["pending"]["e" * 64] = {"spool": SPOOL, "at": old, "stage": "queued"}
        self.fake.lock_held = True
        self.tick(NOW)
        self.assertNotIn("e" * 64, self.spot()["pending"])
        self.assertEqual(self.spot()["counts"]["expired"], 1)

    def test_a_long_run_of_skips_alerts(self):
        self.cycle()
        self.fake.lock_held = True
        self.tick(NOW)
        self.spot()["lock_busy"]["since"] = NOW - batch.CONTAINER_SPOT_LOCK_BUSY_ALERT_SECS
        event = self.tick(NOW)
        self.assertEqual([a["code"] for a in event["alerts"] if a["code"].startswith("container_spot")],
                         ["container_spot_lock_busy"])

    def test_no_replay_is_made_while_draining_or_past_the_global_cap(self):
        self.cycle()
        self.spot()["pending"].clear()
        for number in range(3):
            self.spot()["pending"][f"{number:064x}"] = {"spool": SPOOL, "at": NOW, "stage": "queued"}
        self.fake.spot_calls.clear()
        self.tick(drain=True)
        self.assertEqual(self.fake.spot_calls, [])
        # Comparisons go on while draining.
        self.spot()["pending"][f"{9:064x}"] = {"spool": SPOOL, "at": NOW, "stage": "submitted",
                                               "source": 1, "target": 9}
        self.tick(drain=True)
        (call,) = self.fake.spot_calls
        self.assertEqual(call["submit"], "")
        self.assertTrue(call["check"].startswith(f"{9:064x}=1:9:"))
        # Past the global cap: nothing new, with or without an explicit max_global_pending.
        self.fake.spot_calls.clear()
        self.fake.other = 100
        self.config["max_global_pending"] = 50
        self.tick()
        self.assertEqual([c for c in self.fake.spot_calls if c["submit"]], [])
        del self.config["max_global_pending"]
        self.fake.other = 100000
        self.tick()
        self.assertEqual([c for c in self.fake.spot_calls if c["submit"]], [])

    def test_peers_are_the_hosts_the_inventory_saw_ask_for_work(self):
        self.fake.cpus = {1: 8, 9: 8, 2: 4}
        self.cycle()
        self.assertEqual(self.fake.spot_calls[0]["active"], "1,2,9")
        # Known, and none asking: the queued pages wait, and no step is run for them.
        self.fake.spot_calls.clear()
        self.fake.cpus = {}
        self.spot()["pending"]["e" * 64] = {"spool": SPOOL, "at": NOW, "stage": "queued"}
        self.spot()["pending"]["e" * 64]["at"] = NOW
        for entry in self.spot()["pending"].values():
            if entry["stage"] == "submitted":
                entry["stage"] = "queued"
        self.tick()
        self.assertEqual(self.fake.spot_calls, [])
        # Unknown (an inventory with no host_cpus): no restriction is sent.
        self.fake.cpus = None
        self.tick()
        self.assertIsNone(self.fake.spot_calls[0]["active"])


if __name__ == "__main__":
    unittest.main()
