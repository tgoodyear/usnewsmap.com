"""Native OCR spot checks in the controller (#1863, docs/boinc/apps/native-ocr.md)."""
import copy
import importlib.util
import json
from pathlib import Path
import random
import tempfile
import unittest

SPEC = importlib.util.spec_from_file_location("test_batch_scheduler", Path(__file__).with_name("test_batch_scheduler.py"))
tbs = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(tbs)
batch, NOW = tbs.batch, tbs.NOW

LINUX = "sha256:4d6e2a3be41b3b9c9cf50e6404c6f056177cef547e9d9675298ee1fec94cc5d1"
WORKER_DAC9 = "sha256:dac9c123a52d7b10281d3204a815233da1fe9819afdadd4341a034a95441eb34"
MACOS = "sha256:78065a0a43d8f8bf67c00c014ae2b0540d51d0038e4050dbbd509e14b63fa836"


def validate(config):
    """The configuration as load_config reads it from a file."""
    with tempfile.TemporaryDirectory() as tmp:
        path = Path(tmp) / "plan.json"
        path.write_text(json.dumps({k: v for k, v in config.items() if k != "pages"}))
        return batch.load_config(path)


class Always:
    """A draw that always (or never) picks a page."""

    def __init__(self, value):
        self.value = value

    def random(self):
        return self.value


class SpotFake(tbs.NativeFake):
    """A native plan's BOINC side with the bridge's native-spot step: each replay is made unless
    named in `refused`, and a check returns its job's entry in `verdicts` (pending by default)."""

    def __init__(self, waiting, cpus):
        super().__init__(waiting, cpus)
        self.spot_calls = []
        self.verdicts = {}
        self.refused = set()
        self.spot_error = None

    def native_spot(self, submit, check):
        self.spot_calls.append((submit, check))
        if self.spot_error:
            raise RuntimeError(self.spot_error)

        def outcome(item, status):
            job, rest = item.split("=", 1)
            source, target = rest.split(":")[:2]
            return {"job": job, "source_host": int(source), "target_host": int(target), "status": status,
                    "detail": "test"}

        return {"submitted": [outcome(i, "refused" if i.split("=")[0] in self.refused else "submitted")
                              for i in submit.split(",") if i],
                "checked": [outcome(i, self.verdicts.get(i.split("=")[0], "pending"))
                            for i in check.split(",") if i],
                "nonce": "x", "schema_version": 1}


def spot_config(hosts=(13, 240), image=None, **spec):
    value = tbs.native_config(hosts)
    if image:
        value["image"] = image
    value["native_spot_check"] = {"canary_pages_per_host": 1, **spec}
    return value


class NativeSpotTests(unittest.TestCase):
    def setUp(self):
        self.config = spot_config()
        self.state = {}
        self.fake = SpotFake([tbs.message(n) for n in range(1, 60)], {13: 24, 240: 24})
        self.fake.assertions = []
        self.saved = batch.SPOT_RANDOM
        batch.SPOT_RANDOM = Always(0.0)
        self.addCleanup(setattr, batch, "SPOT_RANDOM", self.saved)

    def tick(self, now=NOW, **kwargs):
        return batch.tick(self.config, self.state, self.fake, lambda: None, now, **kwargs)

    def results_in(self):
        for job in self.fake.jobs.values():
            if job["stage"] == "submitted":
                job["spool"] = "/project/usnmocr/results/7"

    def cycle(self, now=NOW):
        """Send units, bring their results in and publish them, and replay the drawn ones."""
        self.tick(now)
        self.results_in()
        return self.tick(now)

    def test_without_the_setting_nothing_changes(self):
        config = tbs.native_config((13, 240))
        fake = SpotFake([tbs.message(n) for n in range(1, 20)], {13: 24, 240: 24})
        fake.assertions = []
        event = batch.tick(config, {}, fake, lambda: None, NOW)
        self.assertEqual(event["native"]["slots"], {"13": 4, "240": 4})
        self.assertNotIn("spot_check", event["native"])
        self.assertEqual(fake.spot_calls, [])
        # The setting is outside the allocation digest: turning it on is no migration.
        self.assertEqual(batch.digest(config), batch.digest({**config, "native_spot_check": {}}))

    def test_hosts_stay_at_canary_size_until_a_check_matches(self):
        event = self.tick()
        self.assertEqual(event["native"]["slots"], {"13": 1, "240": 1})
        self.assertEqual(event["native"]["spot_check"]["running"], {"13": False, "240": False})
        self.results_in()
        self.tick()
        # Each host's published page went to the other host.
        (submit, check), = self.fake.spot_calls
        self.assertEqual(check, "")
        pairs = sorted(tuple(item.split("=")[1].split(":")[:2]) for item in submit.split(","))
        self.assertEqual(pairs, [("13", "240"), ("240", "13")])
        self.assertTrue(all(item.endswith("a" * 64) for item in submit.split(",")))
        # Back and identical: both hosts' checks run, and they get their full room.
        for job in self.state["native_spot"]["pending"]:
            self.fake.verdicts[job] = "match"
        self.tick()
        event = self.tick()
        self.assertEqual(event["native"]["spot_check"]["running"], {"13": True, "240": True})
        self.assertEqual(event["native"]["slots"], {"13": 4, "240": 4})
        self.assertEqual(self.state["native_spot"]["unchecked"]["13"]["pages"], [])
        # Two days without a match: back to canary size.
        slots = batch.native_spot_slots(self.config, self.state, {13: 4, 240: 4}, NOW + 49 * 3600)
        self.assertEqual(slots, {13: 1, 240: 1})

    def test_a_host_with_no_peer_stays_at_canary_size_and_nothing_is_replayed(self):
        self.config = spot_config(hosts=(13,))
        self.fake.cpus = {13: 24}
        event = self.cycle()
        self.assertEqual(event["native"]["slots"], {"13": 1})
        self.assertEqual(self.fake.spot_calls, [])
        self.assertEqual(self.state["native_spot"]["counts"]["no_peer"], 1)
        self.assertEqual(len(self.state["native_spot"]["unchecked"]["13"]["pages"]), 1)
        # A second host that isn't active is no peer either.
        self.config = spot_config(hosts=(13, 240))
        self.fake.cpus = {13: 24}
        event = self.tick()
        self.assertEqual(event["native"]["slots"], {"13": 1, "240": 0})
        self.assertEqual(self.fake.spot_calls, [])

    def test_a_mismatch_holds_both_hosts_their_results_and_alerts(self):
        self.cycle()
        jobs = list(self.state["native_spot"]["pending"])
        for job in jobs:
            self.fake.verdicts[job] = "mismatch"
        event = self.tick()
        spot = self.state["native_spot"]
        self.assertEqual(sorted(spot["holds"]), ["13", "240"])
        alerts = [a for a in event["alerts"] if a["code"] == "native_spot_mismatch"]
        self.assertEqual(sorted(a["host"] for a in alerts), [13, 240])
        # The record names each host's pages published since its last match.
        record = spot["mismatches"][0]
        self.assertEqual({len(record["unchecked"][h]["pages"]) for h in ("13", "240")}, {1})
        # Neither host gets work, and nothing is replayed again or retried.
        self.fake.actions.clear()
        calls = len(self.fake.spot_calls)
        event = self.tick()
        self.assertEqual(event["native"]["slots"], {"13": 0, "240": 0})
        self.assertEqual(self.native_submissions(), [])
        self.assertEqual(len(self.fake.spot_calls), calls)
        self.assertEqual(len(spot["mismatches"]), 2)

    def native_submissions(self):
        return [unit for action in self.fake.actions if action[0] == "submit-native" for unit in action[1]]

    def test_a_release_after_review_lifts_one_hold_back_to_canary_size(self):
        self.cycle()
        for job in self.state["native_spot"]["pending"]:
            self.fake.verdicts[job] = "mismatch"
        self.tick()
        with self.assertRaisesRegex(ValueError, "note"):
            self.tick(release_spot_hold=(13, ""))
        with self.assertRaisesRegex(ValueError, "no native spot hold"):
            self.tick(release_spot_hold=(99, "looked"))
        event = self.tick(release_spot_hold=(240, "replica disk fault; host 13's page re-read matches"))
        spot = self.state["native_spot"]
        self.assertEqual(sorted(spot["holds"]), ["13"])
        self.assertEqual(spot["releases"][0]["note"], "replica disk fault; host 13's page re-read matches")
        # 240 has no peer while 13 is held: canary size, not its full room.
        self.assertEqual(event["native"]["slots"], {"13": 0, "240": 1})

    def test_refused_failed_and_unverified_replays_are_no_verdict(self):
        self.tick()
        self.results_in()
        jobs = list(self.state["native_targets"])
        self.fake.refused = {jobs[0]}
        self.tick()
        spot = self.state["native_spot"]
        self.assertEqual(spot["counts"]["refused"], 1)
        (left,) = spot["pending"]
        self.fake.verdicts[left] = "unverified"
        event = self.tick()
        self.assertEqual(spot["pending"], {})
        self.assertNotIn("holds", spot)
        self.assertEqual(event["native"]["slots"], {"13": 1, "240": 1})

    def test_a_failing_step_caps_every_host_and_alerts(self):
        self.cycle()
        for job in self.state["native_spot"]["pending"]:
            self.fake.verdicts[job] = "match"
        self.tick()
        self.fake.spot_error = "lock held"
        self.fake.verdicts.clear()
        # One more replay so the step runs again.
        self.state["native_spot"]["pending"]["f" * 64] = {
            "source": 13, "target": 240, "image": "sha256:" + "a" * 64, "page": "p", "spool": "/project/usnmocr/results/7",
            "at": NOW, "stage": "queued"}
        event = self.tick()
        self.assertEqual([a["code"] for a in event["alerts"] if a["code"].startswith("native_spot")],
                         ["native_spot_failing"])
        # The replay stays queued, and while the step fails every host is at canary size.
        self.assertEqual(self.state["native_spot"]["pending"]["f" * 64]["stage"], "queued")
        event = self.tick()
        self.assertEqual(event["native"]["slots"], {"13": 1, "240": 1})
        # The step works again: the checks run again.
        self.fake.spot_error = None
        self.tick()
        event = self.tick()
        self.assertEqual(event["native"]["slots"], {"13": 4, "240": 4})

    def test_a_peer_on_a_listed_build_is_used_and_an_unlisted_one_refused(self):
        peers = [{"host": 300, "image": WORKER_DAC9}]
        validate(spot_config(hosts=(13,), image=LINUX, peers=peers))
        with self.assertRaisesRegex(ValueError, "not listed as identical"):
            validate(spot_config(hosts=(13,), image=LINUX, peers=[{"host": 300, "image": MACOS}]))
        with self.assertRaisesRegex(ValueError, "not listed as identical"):
            validate(spot_config(hosts=(13,), peers=peers))
        with self.assertRaisesRegex(ValueError, "not one of the plan's native_hosts"):
            validate(spot_config(hosts=(13, 300), image=LINUX, peers=peers))
        # The checked plan's replay goes to the peer on its build.
        self.config = spot_config(hosts=(13,), peers=peers)
        self.fake.cpus = {13: 24}
        self.cycle()
        (submit, _), = self.fake.spot_calls
        self.assertEqual(submit.split("=")[1], f"13:300:{WORKER_DAC9[7:]}")
        self.assertTrue(batch.native_comparable(LINUX, WORKER_DAC9))
        self.assertFalse(batch.native_comparable(LINUX, MACOS))

    def test_settings_are_checked(self):
        for spec in ({"share": 0}, {"share": 0.6}, {"canary_pages_per_host": 5}, {"other": 1},
                     {"max_outstanding_per_peer": 0}, {"fresh_hours": 0}):
            with self.subTest(spec=spec), self.assertRaises(ValueError):
                validate(spot_config(**spec))
        container = tbs.queue_config()
        container["native_spot_check"] = {}
        with self.assertRaisesRegex(ValueError, "native_spot_check is for"):
            validate(container)


    def test_the_documented_empty_setting_turns_checks_on(self):
        config = tbs.native_config((13, 240))
        config.update(pages_per_host=40, max_pending=80, native_spot_check={})
        validate(config)
        event = batch.tick(config, self.state, self.fake, lambda: None, NOW)
        self.assertEqual(event["native"]["slots"], {"13": batch.NATIVE_SPOT_CANARY, "240": batch.NATIVE_SPOT_CANARY})
        self.assertIn("spot_check", event["native"])

    def test_a_mismatch_stops_work_to_both_hosts_in_the_same_tick(self):
        self.cycle()
        for job in self.state["native_spot"]["pending"]:
            self.fake.verdicts[job] = "mismatch"
        self.fake.actions.clear()
        event = self.tick()
        self.assertEqual(event["native"]["slots"], {"13": 0, "240": 0})
        self.assertEqual(self.native_submissions(), [])

    def test_every_replay_is_polled_in_turn(self):
        spot = self.state.setdefault("native_spot", {})
        spot["pending"] = {f"{n:064x}": {"source": 13, "target": 240, "image": "sha256:" + "a" * 64, "page": "p",
                                         "spool": "/project/usnmocr/results/7", "at": NOW, "stage": "submitted"}
                           for n in range(150)}
        batch.native_spot_step(self.config, self.state, self.fake, NOW)
        batch.native_spot_step(self.config, self.state, self.fake, NOW + 60)
        polled = set()
        for _, check in self.fake.spot_calls:
            polled |= {item.split("=")[0] for item in check.split(",")}
        self.assertEqual(len(polled), 150)


    def test_a_null_setting_is_refused(self):
        config = tbs.native_config((13, 240))
        config["native_spot_check"] = None
        with self.assertRaises(ValueError):
            validate(config)

    def test_an_overdue_replay_is_compared_before_it_is_given_up(self):
        self.cycle()
        jobs = list(self.state["native_spot"]["pending"])
        for entry in self.state["native_spot"]["pending"].values():
            entry["at"] = NOW - batch.NATIVE_SPOT_MAX_AGE - 3600
        self.fake.verdicts[jobs[0]] = "mismatch"
        batch.native_spot_step(self.config, self.state, self.fake, NOW)
        spot = self.state["native_spot"]
        # The one back is judged; the one still pending past its age is given up.
        self.assertIn(str(spot["mismatches"][0]["source"]), spot["holds"])
        self.assertEqual(spot["pending"], {})
        self.assertEqual(spot["counts"]["expired"], 1)

    def test_a_mismatch_record_keeps_its_evidence(self):
        self.cycle()
        spot = self.state["native_spot"]
        first, second = list(spot["pending"])
        source = str(spot["pending"][first]["source"])
        self.fake.verdicts[first] = "mismatch"
        batch.native_spot_step(self.config, self.state, self.fake, NOW)
        before = copy.deepcopy(spot["mismatches"][0])
        # A later match for the same host settles its live list, not the record.
        spot["pending"][second].update(source=int(source), target=99 if source == "13" else 13)
        self.fake.verdicts[second] = "match"
        batch.native_spot_step(self.config, self.state, self.fake, NOW + 60)
        self.assertEqual(spot["mismatches"][0], before)

    def test_a_page_a_stopped_tick_published_is_still_drawn_once(self):
        self.tick()
        self.results_in()

        class Stop(Exception):
            pass

        def stop(*args, **kwargs):
            raise Stop()

        original = batch.native_spot_tick
        batch.native_spot_tick = stop
        try:
            with self.assertRaises(Stop):
                self.tick()
        finally:
            batch.native_spot_tick = original
        self.tick()
        spot = self.state["native_spot"]
        self.assertEqual(len(spot["pending"]), 2)
        self.tick()
        self.assertEqual(spot["counts"]["drawn"], 2)


    def test_no_replay_is_made_while_draining_or_past_the_global_cap(self):
        self.tick()
        self.results_in()
        self.tick(drain=True)
        submits = [submit for submit, _ in self.fake.spot_calls if submit]
        self.assertEqual(submits, [])
        self.assertEqual({e["stage"] for e in self.state["native_spot"]["pending"].values()}, {"queued"})
        self.fake.other = 5
        pending = self.fake.inventory()["pending_workunits"]
        self.assertGreaterEqual(pending, 5)
        batch.tick(self.config, self.state, self.fake, lambda: None, NOW, max_global_pending=pending)
        self.assertEqual([submit for submit, _ in self.fake.spot_calls if submit], [])
        self.fake.other = 0
        self.tick()
        self.assertTrue([submit for submit, _ in self.fake.spot_calls if submit])


class SamplingTests(unittest.TestCase):
    def setUp(self):
        self.config = spot_config(max_outstanding_per_peer=100)

    def sample(self, jobs, rng=None, share=0.05):
        self.config["native_spot_check"]["share"] = share
        state = {}
        published = [(f"{n:064x}", 13, f"p{n}", "/project/usnmocr/results/7") for n in range(jobs)]
        # A peer with room for everything, so only the draw decides.
        self.config["native_spot_check"]["max_outstanding_per_peer"] = 100
        batch.native_spot_sample(self.config, state, {13: 4, 240: 4}, published, NOW, rng)
        return set(state["native_spot"].get("pending", {}))

    def test_the_share_is_drawn_from_the_os_after_results_come_back(self):
        self.assertIsInstance(batch.SPOT_RANDOM, random.SystemRandom)
        # 1000 pages at 5%: 50 expected; the peer's room (100) doesn't bind.
        counts = [len(self.sample(1000)) for _ in range(20)]
        self.assertTrue(30 <= sum(counts) / len(counts) <= 70, counts)

    def test_the_same_pages_are_not_picked_twice_in_a_row(self):
        # The draw doesn't depend on the job id, so knowing the rule doesn't tell a host which of
        # its pages are checked: two draws over the same 200 pages at 50% differ.
        self.assertNotEqual(self.sample(200, share=0.5), self.sample(200, share=0.5))

    def test_a_busy_peer_bounds_the_replays(self):
        self.config["native_spot_check"]["max_outstanding_per_peer"] = 3
        state = {}
        published = [(f"{n:064x}", 13, f"p{n}", "/project/usnmocr/results/7") for n in range(50)]
        batch.native_spot_sample(self.config, state, {13: 4, 240: 4}, published, NOW, Always(0.0))
        self.assertEqual(len(state["native_spot"]["pending"]), 3)
        self.assertEqual(state["native_spot"]["counts"]["peer_busy"], 47)


if __name__ == "__main__":
    unittest.main()
