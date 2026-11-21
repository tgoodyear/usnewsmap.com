"""Native OCR spot checks in the controller (#1863, docs/boinc/apps/native-ocr.md)."""
import copy
import importlib.util
import json
from pathlib import Path
import random
import subprocess
import tarfile
import tempfile
import unittest
from unittest.mock import patch

SPEC = importlib.util.spec_from_file_location("test_batch_scheduler", Path(__file__).with_name("test_batch_scheduler.py"))
tbs = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(tbs)
batch, NOW = tbs.batch, tbs.NOW
REPORT_SPEC = importlib.util.spec_from_file_location("native_spot_pair_report",
                                                     Path(__file__).with_name("native_spot_pair_report.py"))
REPORT = importlib.util.module_from_spec(REPORT_SPEC)
REPORT_SPEC.loader.exec_module(REPORT)

LINUX = "sha256:4d6e2a3be41b3b9c9cf50e6404c6f056177cef547e9d9675298ee1fec94cc5d1"
WORKER_DAC9 = "sha256:dac9c123a52d7b10281d3204a815233da1fe9819afdadd4341a034a95441eb34"
WINDOWS = "sha256:bb9bfecc433ad85ca3ac3f83d4b08fc6b8ad73b64938ae58075083cab5c013b3"
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


class Sequence:
    """A draw that gives these values in turn, then the last one for good."""

    def __init__(self, values):
        self.values = list(values)

    def random(self):
        return self.values.pop(0) if len(self.values) > 1 else self.values[0]


class SpotFake(tbs.NativeFake):
    """A native plan's BOINC side with the bridge's native-spot step: each replay is made unless
    named in `refused`, and a check returns its job's entry in `verdicts` (pending by default)."""

    def __init__(self, waiting, cpus):
        super().__init__(waiting, cpus)
        self.spot_calls = []
        self.verdicts = {}
        # A compared pair's measures by job, as the bridge reports them (`pair`).
        self.pairs = {}
        # A third reading's agreement with the first replay (`first.agrees`) by job: False by default.
        self.agrees_first = {}
        # The two texts of a pair under word F1 0.50, by job, as the bridge reports them (`texts`).
        self.texts = {}
        self.refused = set()
        self.spot_error = None
        # The BOINC host's step lock stays held past the step's wait: onprem-step.sh refuses
        # before it runs anything (#1919).
        self.lock_held = False

    def native_spot(self, submit, check):
        self.actions.append(("native-spot", submit, check))
        if self.lock_held:
            raise batch.StepLockTimeout("bridge/native-spot failed: the BOINC host's step lock stayed held")
        self.spot_calls.append((submit, check))
        if self.spot_error:
            raise RuntimeError(self.spot_error)

        def outcome(item, status):
            job, rest = item.split("=", 1)
            source, target = rest.split(":")[:2]
            found = {"job": job, "source_host": int(source), "target_host": int(target), "status": status,
                     "detail": "test"}
            if status in ("match", "outside", "mismatch"):
                found["pair"] = self.pairs.get(job, pair())
                if job in self.texts:
                    found["texts"] = self.texts[job]
            parts = rest.split(":")
            if len(parts) == 5:
                found["first"] = {"host": int(parts[4]), "image": LINUX, "cer": 0.02, "word_f1": 0.97,
                                  "agrees": self.agrees_first.get(job, False)}
            return found

        return {"submitted": [outcome(i, "refused" if i.split("=")[0] in self.refused else "submitted")
                              for i in submit.split(",") if i],
                "checked": [outcome(i, self.verdicts.get(i.split("=")[0], "pending"))
                            for i in check.split(",") if i],
                "nonce": "x", "schema_version": 1}


def pair(cer=0.01, f1=0.99, gate=True, status_image=LINUX):
    """A compared pair's measures, as the bridge reports them."""
    return {"source_image": WINDOWS, "target_image": status_image, "cer": cer, "word_f1": f1, "length_ratio": 1.0,
            "peer_chars": 1000, "peer_damage_rate": 0.02, "peer_confidence": 91.0,
            "peer_gate": "unweighed" if gate else "short", "peer_gate_pass": gate}


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
        # The checked plan's replay goes to the peer on its build, once that host is active.
        self.config = spot_config(hosts=(13,), peers=peers)
        self.fake.cpus = {13: 24, 300: 8}
        self.cycle()
        (submit, _), = self.fake.spot_calls
        self.assertEqual(submit.split("=")[1], f"13:300:{WORKER_DAC9[7:]}")
        self.assertTrue(batch.native_comparable(LINUX, WORKER_DAC9))
        self.assertFalse(batch.native_comparable(LINUX, MACOS))

    def test_a_linux_peer_of_a_windows_plan_needs_the_opt_in_and_a_listed_pair(self):
        peers = [{"host": 9, "image": LINUX}]
        # Off by default: the Linux build is not listed as identical to the Windows one.
        with self.assertRaisesRegex(ValueError, "not listed as identical"):
            validate(spot_config(hosts=(16,), image=WINDOWS, peers=peers))
        validate(spot_config(hosts=(16,), image=WINDOWS, peers=peers, cross_build=True))
        # Identical peers stay fine with it on; a build that isn't a listed pair is still refused.
        validate(spot_config(hosts=(13,), image=LINUX, peers=[{"host": 300, "image": WORKER_DAC9}], cross_build=True))
        with self.assertRaisesRegex(ValueError, "nor as a cross-build pair"):
            validate(spot_config(hosts=(16,), image=WINDOWS, peers=[{"host": 9, "image": MACOS}], cross_build=True))
        with self.assertRaisesRegex(ValueError, "cross_build: true or false"):
            validate(spot_config(hosts=(16,), image=WINDOWS, peers=peers, cross_build="yes"))
        self.assertTrue(batch.native_cross_build(LINUX, WINDOWS) and batch.native_cross_build(WINDOWS, LINUX))
        self.assertFalse(batch.native_cross_build(LINUX, LINUX))
        self.assertFalse(batch.native_cross_build(LINUX, WORKER_DAC9))
        self.assertFalse(batch.native_cross_build(LINUX, MACOS))
        self.assertFalse(batch.native_comparable(LINUX, WINDOWS))

    def test_a_windows_page_is_replayed_on_the_linux_peer_and_a_page_outside_the_limits_holds_nobody(self):
        self.config = spot_config(hosts=(16,), cross_build=True, peers=[{"host": 9, "image": LINUX}])
        self.fake.cpus = {16: 24, 9: 6}
        self.cycle()
        (submit, _), = self.fake.spot_calls
        self.assertEqual(submit.split("=")[1], f"16:9:{LINUX[7:]}")
        (job,) = self.state["native_spot"]["pending"]
        self.fake.verdicts[job] = "outside"
        self.tick()
        spot = self.state["native_spot"]
        self.assertEqual(spot["pending"], {})
        self.assertNotIn("holds", spot)
        self.assertEqual(spot["counts"]["outside"], 1)
        self.assertEqual(spot["last_unverified"]["status"], "outside")
        # No match: the host is still at canary size.
        event = self.tick()
        self.assertEqual(event["native"]["slots"], {"16": 1})

    def test_settings_are_checked(self):
        for spec in ({"share": 0}, {"share": 0.6}, {"canary_pages_per_host": 5}, {"other": 1},
                     {"max_outstanding_per_peer": 0}, {"fresh_hours": 0}, {"lock_wait_secs": -1},
                     {"lock_wait_secs": 61}, {"lock_wait_secs": 1.5}):
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

    def test_a_mismatch_stops_work_to_both_hosts_from_the_next_tick(self):
        self.cycle()
        for job in self.state["native_spot"]["pending"]:
            self.fake.verdicts[job] = "mismatch"
        self.fake.actions.clear()
        event = self.tick()
        # The step runs after this tick's submit (#1919); the tick line shows the room from now on.
        kinds = [action[0] for action in self.fake.actions]
        self.assertIn("native-spot", kinds)
        self.assertNotIn("submit-native", kinds[kinds.index("native-spot"):])
        self.assertEqual(event["native"]["slots"], {"13": 0, "240": 0})
        self.fake.actions.clear()
        self.results_in()
        self.tick()
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

    def test_a_replay_takes_a_unit_of_its_peers_shared_room(self):
        # #1780: peer 240's limit is 4; another plan has 2 units on it and this plan 1, so only
        # one of three queued replays goes, and the others wait, queued.
        spot = self.state.setdefault("native_spot", {})
        spot["pending"] = {f"{n:064x}": {"source": 13, "target": 240, "image": "sha256:" + "a" * 64, "page": "p",
                                         "spool": "/project/usnmocr/results/7", "at": NOW, "stage": "queued"}
                           for n in range(3)}
        config = {**self.config, "units_per_cpu": 1, "pages_per_host": 4}
        room = batch.native_replay_room(config, {"host_cpus": {"240": 8, "13": 2}}, {240: 2},
                                        {"e" * 64: 240, "f" * 64: 13}, False)
        self.assertEqual(room, {240: 1, 13: 1})
        batch.native_spot_step(config, self.state, self.fake, NOW, target_room=room)
        sent = [item.split("=")[0] for item in self.fake.spot_calls[0][0].split(",")]
        self.assertEqual(sent, [f"{0:064x}"])
        self.assertEqual(sum(e["stage"] == "queued" for e in self.state["native_spot"]["pending"].values()), 2)
        # Without a per-host count: no limit for a listed plan, none left for one from the trust list.
        self.assertIsNone(batch.native_replay_room(config, {}, None, {}, False))
        self.assertEqual(batch.native_replay_room(config, {}, None, {}, True), {})

    def test_queued_replays_keep_their_room_through_repeated_full_refills(self):
        # #2080 follow-up: host 240's room is 4 and the queue never runs dry. Without room kept
        # for it, every refill filled 240 before the replay step ran, and its replay never went.
        self.config["native_spot_check"]["canary_pages_per_host"] = 4
        self.fake.cpus = {13: 4, 240: 4}
        inventory = self.fake.inventory
        self.fake.inventory = lambda: {**inventory(), "native_host_units": {}}
        spot = self.state.setdefault("native_spot", {})
        replay = "a" * 64
        spot["pending"] = {replay: {"source": 13, "target": 240, "image": self.config["image"], "page": "p",
                                    "spool": "/project/usnmocr/results/7", "at": NOW, "stage": "queued"}}
        batch.SPOT_RANDOM = Always(1.0)  # no new draws: only this replay is queued
        for n in range(3):
            self.fake.actions.clear()
            self.tick(NOW + 60 * n)
            to_240 = sum(host == 240 for _, host in self.native_submissions())
            outstanding = sum(host == 240 for host in self.state["native_targets"].values())
            if self.state["native_spot"]["pending"].get(replay, {}).get("stage") != "queued":
                break
            self.results_in()
        self.assertEqual(n, 0)
        self.assertEqual(self.state["native_spot"]["pending"][replay]["stage"], "submitted")
        # The refill left 240 one unit short of its room for it, and the shared limit held.
        self.assertEqual((to_240, outstanding), (3, 3))

    def test_a_replays_room_is_counted_from_the_admission_inventory_when_nothing_is_submitted(self):
        # #1780 review: side by side, another plan filled peer 240 after this tick's first
        # inventory. This tick submits no OCR unit (its target is 0), and its replay still
        # sees 240 full in the inventory taken under the admission lock, so it stays queued.
        # Host 13 has no CPUs reported, so the plan's target is 0 and the refill block is skipped;
        # 240 is a configured peer.
        self.config = spot_config(hosts=(13,), peers=[{"host": 240, "image": self.config["image"]}])
        self.fake.cpus = {13: 0, 240: 24}
        spot = self.state.setdefault("native_spot", {})
        spot["pending"] = {"a" * 64: {"source": 13, "target": 240, "image": self.config["image"], "page": "p",
                                      "spool": "/project/usnmocr/results/7", "at": NOW, "stage": "queued"}}
        inventory = self.fake.inventory
        calls = []

        def counted():
            calls.append(1)
            full = {"240": {"units": 0 if len(calls) == 1 else 24, "jobs": []}}
            return {**inventory(), "native_host_units": full}
        self.fake.inventory = counted

        class Admission:
            waited = 0.0

            def acquire(self):
                return True

            def release(self):
                pass
        self.tick(admission=Admission())
        self.assertGreater(len(calls), 1)
        self.assertEqual(spot["pending"]["a" * 64]["stage"], "queued")
        self.assertEqual([call for call in self.fake.spot_calls if call[0]], [])

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

    def test_every_replay_is_polled_in_turn_while_the_step_fails(self):
        spot = self.state.setdefault("native_spot", {})
        spot["pending"] = {f"{n:064x}": {"source": 13, "target": 240, "image": "sha256:" + "a" * 64, "page": "p",
                                         "spool": "/project/usnmocr/results/7", "at": NOW, "stage": "submitted"}
                           for n in range(150)}
        self.fake.spot_error = "a replay journal is unreadable"
        batch.native_spot_step(self.config, self.state, self.fake, NOW)
        batch.native_spot_step(self.config, self.state, self.fake, NOW + 60)
        polled = set()
        for _, check in self.fake.spot_calls:
            polled |= {item.split("=")[0] for item in check.split(",")}
        self.assertEqual(len(polled), 150)

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

        original = batch.native_spot_draw
        batch.native_spot_draw = stop
        try:
            with self.assertRaises(Stop):
                self.tick()
        finally:
            batch.native_spot_draw = original
        self.tick()
        spot = self.state["native_spot"]
        self.assertEqual(len(spot["pending"]), 2)
        self.tick()
        self.assertEqual(spot["counts"]["drawn"], 2)


    def test_a_manual_retrys_child_is_drawn_from_by_the_host_in_its_journal(self):
        # #1919: a retry's child is never in native_targets (the automatic submit fills it), so
        # its host comes from the journal, and it is noted once.
        job = "c" * 64
        owned = {"page-1": {"stage": "published", "job": {"id": job},
                            "retry": {"native": {"target_host": 13}}},
                 "page-2": {"stage": "published", "job": {"id": "d" * 64}}}
        state = {}
        drawn = []
        original = batch.native_spot_sample
        batch.native_spot_sample = lambda config, state, slots, published, *rest: drawn.extend(published)
        try:
            for _ in range(2):
                batch.native_spot_draw({}, state, lambda: None, 1000, owned,
                                       {job: {"spool": "/spool/c"}, "d" * 64: {"spool": "/spool/d"}}, {})
        finally:
            batch.native_spot_sample = original
        # The page with no journal host and no target is skipped; the retry is drawn once.
        self.assertEqual(drawn, [(job, 13, "page-1", "/spool/c")])

    def test_a_page_is_never_read_again_on_its_own_host_even_when_it_is_a_configured_peer(self):
        # #1919: a manual retry may go to a configured peer, whose own pages need another peer.
        config = {"native_hosts": [13], "image": "a" * 64,
                  batch.NATIVE_SPOT_KEY: {"peers": [{"host": 20, "image": "b" * 64},
                                                    {"host": 21, "image": "b" * 64}]}}
        peers = batch.native_spot_peers(config, {}, {13: 1}, 20)
        self.assertEqual(sorted(host for host, _ in peers), [13, 21])

    def test_no_replay_is_made_while_draining_or_past_the_global_cap(self):
        self.tick()
        self.results_in()
        back = [j for j, v in self.fake.jobs.items() if v["spool"]]
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


class CrossBuildRulingTests(unittest.TestCase):
    """The Science Coordinator's ruling of 16 Nov 2026 on the cross-build tier: a pair outside the
    limits lets no page through, a third reading settles it, the host's rolling window can trip
    its feed hold, and every pair is logged."""

    def setUp(self):
        # The fake plan's own build is not Windows bb9bfecc: its pair with the Linux build is
        # listed here, as native-build-trust.json lists the real one.
        listed = patch.object(batch, "native_cross_build", lambda a, b, registry=None: LINUX in (a, b) and a != b)
        listed.start()
        self.addCleanup(listed.stop)
        self.config = spot_config(hosts=(16,), cross_build=True,
                                  peers=[{"host": 9, "image": LINUX}],
                                  third_peers=[{"host": 13, "image": LINUX}])
        self.state = {}
        self.fake = SpotFake([tbs.message(n) for n in range(1, 40)], {16: 24, 9: 6, 13: 6})
        self.fake.assertions = []
        self.saved = batch.SPOT_RANDOM
        batch.SPOT_RANDOM = Always(0.0)
        self.addCleanup(setattr, batch, "SPOT_RANDOM", self.saved)
        self.log = []
        self.stored = []

    def tick(self, now=NOW, **kwargs):
        return batch.tick(self.config, self.state, self.fake, lambda: None, now,
                          pair_log=self.log.append, resolution_log=self.stored.append, **kwargs)

    def results_in(self):
        for job in self.fake.jobs.values():
            if job["stage"] == "submitted":
                job["spool"] = "/project/usnmocr/results/7"

    def gated_job(self):
        """One result, back and drawn for the Linux peer: kept out of collection."""
        self.config["native_spot_check"]["canary_pages_per_host"] = 1
        self.tick()
        self.results_in()
        self.tick()
        (job,) = self.state["native_spot"]["gated"]
        return job

    def published(self):
        return {row["job"]["id"] for row in self.fake.rows if row["stage"] == "published"}

    def collected(self):
        return [action[1] for action in self.fake.actions if action[0] == "collect"]

    def test_a_drawn_windows_page_is_kept_out_of_collection_until_its_pair_matches(self):
        job = self.gated_job()
        self.assertEqual(self.state["native_spot"]["gated"][job]["phase"], "first")
        self.assertNotIn(job, self.collected())
        self.assertNotIn(job, self.published())
        self.assertEqual(self.state["native_spot"]["unchecked"]["16"]["pages"], [])
        self.fake.verdicts[job] = "match"
        self.tick()
        self.assertEqual(self.state["native_spot"]["gated"], {})
        self.tick()
        self.assertIn(job, self.published())

    def test_an_outside_pair_lets_nothing_through_and_does_not_count_toward_revocation(self):
        job = self.gated_job()
        self.fake.verdicts[job] = "outside"
        self.fake.pairs[job] = pair(cer=0.3, f1=0.6)
        self.tick()
        del self.fake.verdicts[job]
        spot = self.state["native_spot"]
        self.assertEqual(spot["gated"][job]["phase"], "third_wait")
        for _ in range(3):
            self.tick()
        self.assertNotIn(job, self.collected())
        self.assertNotIn(job, self.published())
        # Nobody is held and no mismatch record is made: nothing here can revoke a page.
        self.assertNotIn("holds", spot)
        self.assertNotIn("mismatches", spot)
        self.assertEqual(spot["counts"]["outside"], 1)

    def third_pending(self, first_pair=None):
        """A page whose first pair was outside and whose third reading is out on host 13."""
        job = self.gated_job()
        self.fake.verdicts[job] = "outside"
        self.fake.pairs[job] = first_pair or pair(cer=0.05, f1=0.85)
        self.tick()
        self.tick()
        entry = self.state["native_spot"]["pending"][job]
        self.assertEqual((entry["target"], entry["third"], entry["first"]), (13, True, 9))
        return job

    def resolved(self, job, status, agrees_first, third_pair=None):
        self.fake.verdicts[job] = status
        self.fake.agrees_first[job] = agrees_first
        self.fake.pairs[job] = third_pair or pair(cer=0.04, f1=0.93 if status == "match" else 0.8)
        self.tick()
        return self.state["native_spot"]

    def test_a_third_reading_is_checked_against_the_first_replay_as_well_as_the_original(self):
        job = self.third_pending()
        self.assertTrue(any(f"{job}=16:13:{LINUX[7:]}:/project/usnmocr/results/7:9" in check
                            for _, check in self.fake.spot_calls) or job in self.state["native_spot"]["pending"])
        self.assertEqual(batch._spot_item(job, self.state["native_spot"]["pending"][job], True),
                         f"{job}=16:13:{LINUX[7:]}:/project/usnmocr/results/7:9")

    def test_a_third_reading_agreeing_with_the_original_only_imports_the_original(self):
        job = self.third_pending()
        spot = self.resolved(job, "match", False)
        self.assertEqual(spot["gated"], {})
        self.assertEqual(spot["last_released"]["why"], "third reading agreed with the original")
        self.tick()
        self.assertIn(job, self.published())
        (record,) = self.stored
        self.assertEqual((record["outcome"], record["imported"]), ("original", "original"))
        # The first replay was the wrong one: host 9, on the Linux build.
        self.assertEqual(spot["wrong_tally"], {"by_host": {"9": 1}, "by_build": {LINUX: 1}, "unsettled": 0})

    def test_a_third_reading_agreeing_with_the_first_replay_only_keeps_the_page_out_and_names_the_original_wrong(self):
        job = self.third_pending()
        spot = self.resolved(job, "outside", True)
        self.assertEqual(spot["gated"][job]["phase"], "first_wins")
        self.assertNotIn(job, self.published())
        self.assertEqual(spot["wrong_tally"]["by_host"], {"16": 1})
        self.assertEqual(self.stored[-1]["outcome"], "first")
        self.assertIsNone(self.stored[-1]["imported"])
        event = self.tick()
        self.assertIn("native_spot_first_reading_wins", [a["code"] for a in event["alerts"]])
        self.assertNotIn(job, self.collected())

    def test_a_third_reading_agreeing_with_both_or_neither_leaves_the_page_with_its_current_text(self):
        for status, agrees, outcome in (("match", True, "both"), ("outside", False, "neither")):
            with self.subTest(outcome):
                self.setUp()
                job = self.third_pending()
                spot = self.resolved(job, status, agrees)
                self.assertEqual(spot["gated"][job]["phase"], "rejected")
                self.tick()
                self.assertNotIn(job, self.published())
                self.assertNotIn(job, self.collected())
                self.assertEqual(self.stored[-1]["outcome"], outcome)
                self.assertEqual(spot["wrong_tally"], {"by_host": {}, "by_build": {}, "unsettled": 1})
                alerts = [a["code"] for a in batch.native_spot_alerts(self.config, self.state)]
                self.assertIn("native_spot_page_withheld", alerts)
                # It is not read a fourth time.
                calls = len(self.fake.spot_calls)
                self.tick()
                self.assertEqual(len(self.fake.spot_calls), calls)

    def test_every_resolution_is_logged_with_its_readings_pairs_and_outcome(self):
        job = self.third_pending(pair(cer=0.05, f1=0.85))
        with patch("sys.stderr") as err:
            self.resolved(job, "match", False, pair(cer=0.04, f1=0.93))
        lines = [json.loads(call.args[0]) for call in err.write.call_args_list
                 if call.args and call.args[0].startswith("{") and "native_spot_resolution" in call.args[0]]
        self.assertEqual(len(lines), 1)
        for record in (lines[0], self.stored[0]):
            self.assertEqual((record["job"], record["outcome"]), (job, "original"))
            self.assertEqual(record["readings"], {"original": {"host": 16, "build": WINDOWS},
                                                  "first": {"host": 9, "build": LINUX},
                                                  "third": {"host": 13, "build": LINUX}})
            self.assertEqual(record["pairs"], {"original_first": {"cer": 0.05, "word_f1": 0.85},
                                               "original_third": {"cer": 0.04, "word_f1": 0.93},
                                               "first_third": {"cer": 0.02, "word_f1": 0.97}})
            self.assertEqual(record["wrong"], [{"host": 9, "build": LINUX}])
        self.tick()
        self.assertEqual(len(self.stored), 1)

    def test_a_page_waits_for_a_third_reading_when_no_host_can_take_it(self):
        del self.config["native_spot_check"]["third_peers"]
        job = self.gated_job()
        self.fake.verdicts[job] = "outside"
        self.tick()
        event = self.tick()
        self.assertEqual(self.state["native_spot"]["gated"][job]["phase"], "third_wait")
        self.assertNotIn(job, self.published())
        self.assertIn("native_spot_third_reading_waiting", [a["code"] for a in event["alerts"]])

    def test_a_third_reading_that_cannot_be_made_is_tried_on_another_host_then_left_withheld(self):
        self.config["native_spot_check"]["third_peers"] = [{"host": 13, "image": LINUX}, {"host": 14, "image": LINUX}]
        self.fake.cpus[14] = 6
        job = self.gated_job()
        self.fake.verdicts[job] = "outside"
        self.tick()
        del self.fake.verdicts[job]
        self.fake.refused.add(job)
        self.tick()
        self.assertEqual(self.state["native_spot"]["gated"][job]["tried"], [9, 13])
        self.fake.refused.discard(job)
        self.tick()
        self.assertEqual(self.state["native_spot"]["pending"][job]["target"], 14)
        self.assertNotIn(job, self.published())

    def test_a_pair_with_no_verdict_lets_the_page_through_as_it_always_did(self):
        job = self.gated_job()
        self.fake.verdicts[job] = "unverified"
        self.tick()
        self.assertEqual(self.state["native_spot"]["gated"], {})
        self.tick()
        self.assertIn(job, self.published())
        self.assertEqual(self.state["native_spot"]["unchecked"]["16"]["pages"][0][0], job)

    def test_a_same_build_pair_is_not_kept_out(self):
        config = spot_config(hosts=(13, 240))
        fake = SpotFake([tbs.message(n) for n in range(1, 20)], {13: 24, 240: 24})
        fake.assertions = []
        state = {}
        batch.tick(config, state, fake, lambda: None, NOW)
        for job in fake.jobs.values():
            job["spool"] = "/project/usnmocr/results/7"
        back = sorted(fake.jobs)
        batch.tick(config, state, fake, lambda: None, NOW)
        self.assertNotIn("gated", state["native_spot"])
        self.assertGreater(len(state["native_spot"]["pending"]), 0)
        self.assertEqual(sorted(a[1] for a in fake.actions if a[0] == "collect"), back)

    def test_a_drain_keeps_nothing_out_since_it_makes_no_replay(self):
        self.tick()
        self.results_in()
        back = [j for j, v in self.fake.jobs.items() if v["spool"]]
        self.tick(drain=True)
        self.assertNotIn("gated", self.state.get("native_spot", {}))
        self.assertEqual(sorted(self.collected()), sorted(back))

    def pair_entry(self, job, source=16):
        return {"source": source, "target": 9, "image": LINUX, "page": f"p/{job}", "spool": "/x", "at": NOW,
                "stage": "queued"}

    def feed(self, status, f1, job, gate=True, now=NOW):
        outcome = {"job": job, "status": status, "detail": "test",
                   "pair": pair(f1=f1, gate=gate, cer=0.3 if status == "outside" else 0.01)}
        batch.native_spot_pair(self.config, self.state, job, self.pair_entry(job), outcome, now)

    def test_more_than_a_quarter_of_the_last_pairs_outside_holds_the_feed_and_alerts_with_details(self):
        for n in range(15):
            self.feed("match", 0.97, f"{n:064x}")
        # 5 of 20: exactly a quarter is not more than a quarter.
        for n in range(15, 20):
            self.feed("outside", 0.7, f"{n:064x}")
        self.assertNotIn("holds", self.state["native_spot"])
        self.feed("outside", 0.7, f"{20:064x}")
        hold = self.state["native_spot"]["holds"]["16"]
        self.assertEqual(hold["kind"], "cross_build_trip")
        self.assertEqual((hold["window_pairs"], hold["window_outside"]), (21, 6))
        self.assertIn("6 of the last 21 pairs are outside", hold["reasons"][0])
        alerts = batch.native_spot_alerts(self.config, self.state)
        (alert,) = [a for a in alerts if a["code"] == "native_spot_cross_build_trip"]
        self.assertEqual(alert["host"], 16)
        self.assertEqual(len(alert["pairs"]), 6)
        self.assertEqual(alert["pairs"][0]["word_f1"], 0.7)
        # The feed pauses: no room, until a release.
        self.assertEqual(batch.native_spot_slots(self.config, self.state, {16: 4}, NOW), {16: 0})

    def test_the_window_is_the_last_fifty_pairs_so_old_outsides_age_out(self):
        for n in range(3):
            self.feed("outside", 0.7, f"{n:064x}")
        for n in range(3, 70):
            self.feed("match", 0.97, f"{n:064x}")
        window = self.state["native_spot"]["window"]["16"]
        self.assertEqual(len(window), batch.NATIVE_SPOT_WINDOW)
        self.assertEqual(sum(p["status"] == "outside" for p in window), 0)
        self.assertNotIn("holds", self.state["native_spot"])

    def test_early_outside_pairs_do_not_trip_the_share_before_there_are_enough_pairs(self):
        for n in range(8):
            self.feed("outside", 0.7, f"{n:064x}")
        self.assertNotIn("holds", self.state["native_spot"])

    LOC_WORDS = [f"w{n}" for n in range(10)]

    def reading(self, right):
        """A ten-word text with `right` of LoC's words and the rest not."""
        return " ".join(self.LOC_WORDS[:right] + [f"x{n}" for n in range(10 - right)])

    def low_pair(self, job, host_right, peer_right, loc=True, now=NOW):
        """A first pair under word F1 0.50, as the bridge reports it with its texts, then scored
        against LoC's text (the ten words, or none)."""
        outcome = {"job": job, "status": "outside", "detail": "test", "pair": pair(f1=0.3, cer=0.4),
                   "texts": {"original": self.reading(host_right), "replay": self.reading(peer_right)}}
        batch.native_spot_pair(self.config, self.state, job, self.pair_entry(job), outcome, now)
        asked = []

        def loc_text(page):
            asked.append(page)
            return " ".join(self.LOC_WORDS) if loc else None

        batch.native_spot_score_low(self.config, self.state, loc_text, now)
        return asked

    def hold(self):
        return self.state["native_spot"].get("holds", {}).get("16")

    def test_a_low_f1_pair_where_the_hosts_reading_is_far_below_the_peers_against_loc_holds_the_feed(self):
        self.feed("match", 0.97, f"{1:064x}")
        asked = self.low_pair(f"{2:064x}", host_right=1, peer_right=10)
        self.assertEqual(asked, [f"p/{2:064x}"])
        hold = self.hold()
        self.assertEqual(hold["kind"], "cross_build_trip")
        self.assertIn("0.2 or more below the peer's against LoC's text", hold["reasons"][0])
        self.assertEqual([p["job"] for p in hold["pairs"]], [f"{2:064x}"])
        self.assertEqual(hold["pairs"][0]["loc"], "host_worse")
        record = self.state["native_spot"]["low_f1"][-1]
        self.assertEqual((record["verdict"], record["host"], record["peer_host"]), ("host_worse", 16, 9))
        self.assertAlmostEqual(record["loc_f1"]["host"], 0.1)
        self.assertEqual(record["loc_f1"]["peer"], 1.0)
        alert = next(a for a in batch.native_spot_alerts(self.config, self.state)
                     if a["code"] == "native_spot_cross_build_trip")
        self.assertEqual(alert["host"], 16)

    def test_a_gap_of_exactly_point_two_is_a_fault_and_a_smaller_one_is_not(self):
        self.low_pair(f"{1:064x}", host_right=7, peer_right=9)
        self.assertIsNotNone(self.hold())
        self.setUp()
        self.state = {}
        self.low_pair(f"{2:064x}", host_right=8, peer_right=9)
        self.assertIsNone(self.hold())

    def test_a_low_f1_pair_where_both_readings_are_far_from_loc_is_a_hard_page_and_no_trip(self):
        self.low_pair(f"{1:064x}", host_right=2, peer_right=3)
        self.assertIsNone(self.hold())
        record = self.state["native_spot"]["low_f1"][-1]
        self.assertEqual(record["verdict"], "similar")
        self.assertEqual(self.state["native_spot"]["window"]["16"][0]["loc"], "similar")
        self.assertNotIn("native_spot_low_f1_unscored", [a["code"] for a in batch.native_spot_alerts(self.config, self.state)])
        # A host that read the page better than the peer is no fault either.
        self.low_pair(f"{2:064x}", host_right=9, peer_right=1)
        self.assertIsNone(self.hold())
        self.assertEqual(self.state["native_spot"]["counts"]["low_f1_similar"], 2)

    def test_with_no_loc_text_the_first_low_f1_pair_only_alerts_and_a_second_in_the_window_holds(self):
        self.feed("match", 0.97, f"{1:064x}")
        self.low_pair(f"{2:064x}", host_right=1, peer_right=10, loc=False)
        self.assertIsNone(self.hold())
        alerts = batch.native_spot_alerts(self.config, self.state)
        (alert,) = [a for a in alerts if a["code"] == "native_spot_low_f1_unscored"]
        self.assertEqual((alert["host"], [p["job"] for p in alert["pairs"]]), (16, [f"{2:064x}"]))
        self.assertEqual(self.state["native_spot"]["low_f1"][-1]["verdict"], "unavailable")
        # Another pair in the window, however it is scored, is not a second unscored one.
        self.low_pair(f"{3:064x}", host_right=2, peer_right=3)
        self.assertIsNone(self.hold())
        self.low_pair(f"{4:064x}", host_right=1, peer_right=10, loc=False)
        hold = self.hold()
        self.assertIn("2 pairs under word F1 0.5 in the last 4 with no LoC text", hold["reasons"][0])
        alerts = batch.native_spot_alerts(self.config, self.state)
        self.assertNotIn("native_spot_low_f1_unscored", [a["code"] for a in alerts])
        self.assertIn("native_spot_cross_build_trip", [a["code"] for a in alerts])

    def test_the_second_unscored_pair_must_fall_in_the_same_window(self):
        self.low_pair(f"{1:064x}", host_right=1, peer_right=10, loc=False)
        for n in range(2, 60):
            self.feed("match", 0.97, f"{n:064x}")
        self.low_pair(f"{99:064x}", host_right=1, peer_right=10, loc=False)
        self.assertIsNone(self.hold())

    def test_a_pair_with_no_texts_has_none_to_score_and_waits_for_nothing(self):
        outcome = {"job": f"{1:064x}", "status": "outside", "detail": "test", "pair": pair(f1=0.2, cer=0.5)}
        batch.native_spot_pair(self.config, self.state, f"{1:064x}", self.pair_entry(f"{1:064x}"), outcome, NOW)
        self.assertEqual(self.state["native_spot"]["window"]["16"][0]["loc"], "unavailable")
        self.assertNotIn("loc_queue", self.state["native_spot"])

    def test_loc_text_that_fails_to_come_is_no_text(self):
        outcome = {"job": f"{1:064x}", "status": "outside", "detail": "test", "pair": pair(f1=0.2, cer=0.5),
                   "texts": {"original": "a b", "replay": "c d"}}
        batch.native_spot_pair(self.config, self.state, f"{1:064x}", self.pair_entry(f"{1:064x}"), outcome, NOW)

        def broken(page):
            raise TimeoutError("archive not read in time")

        batch.native_spot_score_low(self.config, self.state, broken, NOW)
        self.assertEqual(self.state["native_spot"]["low_f1"][-1]["verdict"], "unavailable")

    def test_a_provider_that_raises_anything_is_no_text(self):
        import xml.etree.ElementTree as ET
        for error in (ET.ParseError("bad alto"), tarfile.ReadError("bad archive"), KeyError("x")):
            with self.subTest(type(error).__name__):
                self.state = {}
                job = f"{1:064x}"
                outcome = {"job": job, "status": "outside", "detail": "test", "pair": pair(f1=0.2, cer=0.5),
                           "texts": {"original": "a b", "replay": "c d"}}
                batch.native_spot_pair(self.config, self.state, job, self.pair_entry(job), outcome, NOW)

                def broken(page):
                    raise error

                batch.native_spot_score_low(self.config, self.state, broken, NOW)
                self.assertEqual(self.state["native_spot"]["window"]["16"][0]["loc"], "unavailable")
                self.assertNotIn("loc_queue", self.state["native_spot"])

    def test_a_third_reading_queued_before_the_resolution_rule_gets_its_first_host(self):
        job = self.third_pending()
        entry = self.state["native_spot"]["pending"][job]
        del entry["first"]
        entry["stage"] = "submitted"
        del self.fake.verdicts[job]
        self.tick()
        self.assertEqual(self.state["native_spot"]["pending"][job]["first"], 9)
        self.assertTrue(any(f"{job}=16:13:{LINUX[7:]}:/project/usnmocr/results/7:9" in check
                            for _, check in self.fake.spot_calls))

    def test_the_bridge_wrapper_takes_a_third_readings_check_with_its_first_host(self):
        text = (Path(__file__).parents[1] / "images/server/bridge-job.sh").read_text()
        line = next(l for l in text.splitlines() if l.strip().startswith('check="$item:'))
        item = "[0-9a-f]{64}=[1-9][0-9]{0,9}:[1-9][0-9]{0,9}:[0-9a-f]{64}"
        base = f"{'a' * 64}=16:13:{'b' * 64}:/p/results/0"
        script = (f"item='{item}'\nPROJ=/p\n{line.strip()}\n"
                  'for v in "$@"; do [[ "$v" =~ ^($check(,$check){0,99})?$ ]] && echo ok || echo no; done')
        done = subprocess.run(["bash", "-c", script, "x", base, base + ":9", base + ":9," + base,
                               base + ":x", base + ":9:9"], capture_output=True, text=True, check=True)
        self.assertEqual(done.stdout.split(), ["ok", "ok", "ok", "no", "no"])

    def test_only_a_few_pairs_are_scored_a_tick_and_the_rest_wait_in_the_state(self):
        for n in range(5):
            outcome = {"job": f"{n:064x}", "status": "outside", "detail": "test", "pair": pair(f1=0.2, cer=0.5),
                       "texts": {"original": "a b", "replay": "c d"}}
            batch.native_spot_pair(self.config, self.state, f"{n:064x}", self.pair_entry(f"{n:064x}"), outcome, NOW + n)
        asked = []
        batch.native_spot_score_low(self.config, self.state, lambda page: asked.append(page) or "a b", NOW)
        self.assertEqual(len(asked), batch.NATIVE_SPOT_LOC_PER_TICK)
        self.assertEqual(len(self.state["native_spot"]["loc_queue"]), 3)
        pending = [r for r in self.state["native_spot"]["window"]["16"] if r["loc"] == "pending"]
        self.assertEqual(len(pending), 3)

    def test_a_tick_scores_a_low_pair_with_the_loc_text_it_is_given(self):
        job = self.gated_job()
        self.fake.verdicts[job] = "outside"
        self.fake.pairs[job] = pair(cer=0.5, f1=0.2)
        self.fake.texts[job] = {"original": self.reading(1), "replay": self.reading(10)}
        self.tick(loc_text=lambda page: " ".join(self.LOC_WORDS))
        self.assertEqual(self.state["native_spot"]["holds"]["16"]["kind"], "cross_build_trip")
        self.assertNotIn("loc_queue", self.state["native_spot"])
        # No texts ride in the state once scored.
        self.assertNotIn("texts", json.dumps(self.state))

    def test_a_release_starts_a_new_window(self):
        self.low_pair(f"{1:064x}", host_right=1, peer_right=10)
        self.assertIn("16", self.state["native_spot"]["holds"])
        batch.release_native_spot_hold(self.state, 16, "looked at the pair", NOW)
        self.assertNotIn("16", self.state["native_spot"]["holds"])
        self.assertNotIn("16", self.state["native_spot"]["window"])

    def test_the_word_f1_is_the_word_bag_dice_after_gold_normalisation(self):
        self.assertEqual(batch.word_f1("", ""), 1.0)
        self.assertEqual(batch.word_f1("a b c d", "a b c d"), 1.0)
        self.assertEqual(batch.word_f1("a b", "c d"), 0.0)
        self.assertAlmostEqual(batch.word_f1("Claremont is far", "claremont is\nclose"), 2 * 2 / 6)
        self.assertEqual(batch.word_f1("Clare-\nmont", "Claremont"), 1.0)
        self.assertEqual(batch.word_f1("a, b. c", "a b c"), 1.0)

    def test_every_pair_is_logged_passes_included(self):
        job = self.gated_job()
        self.fake.verdicts[job] = "match"
        self.fake.pairs[job] = pair(cer=0.02, f1=0.97)
        with patch("sys.stderr") as err:
            self.tick()
        lines = [json.loads(call.args[0]) for call in err.write.call_args_list
                 if call.args and call.args[0].startswith("{") and "native_spot_pair" in call.args[0]]
        self.assertEqual(len(lines), 1)
        for record in (lines[0], self.log[-1]):
            self.assertEqual((record["event"], record["status"], record["job"]), ("native_spot_pair", "match", job))
            self.assertEqual((record["source_host"], record["peer_host"]), (16, 9))
            self.assertEqual((record["source_build"], record["peer_build"]), (WINDOWS, LINUX))
            self.assertEqual((record["cer"], record["word_f1"]), (0.02, 0.97))
            self.assertEqual((record["peer_gate"], record["peer_gate_pass"]), ("unweighed", True))
            self.assertEqual((record["peer_damage_rate"], record["peer_confidence"]), (0.02, 91.0))
            self.assertEqual(record["page"], self.state["native_spot"]["gated"].get(job, {"page": record["page"]})["page"])

    def test_a_third_peer_must_be_another_build_than_the_plans(self):
        listed = {"host": 13, "image": LINUX}
        with patch.object(batch, "native_cross_build", lambda a, b, registry=None: LINUX in (a, b) and a != b):
            validate(spot_config(hosts=(16,), cross_build=True, peers=[{"host": 9, "image": LINUX}],
                                 third_peers=[listed]))
            same = spot_config(hosts=(16,), cross_build=True, peers=[{"host": 9, "image": LINUX}],
                               third_peers=[{"host": 14, "image": tbs.native_config()["image"]}])
            with self.assertRaisesRegex(ValueError, "third peer 14's build .* cross-build pair"):
                validate(same)
            with self.assertRaisesRegex(ValueError, "cross_build is off"):
                validate(spot_config(hosts=(16,), peers=[], third_peers=[listed]))
        # And a third peer that is a host of the plan's own build is never used for a reading.
        self.config["native_spot_check"]["third_peers"] = [{"host": 14, "image": tbs.native_config()["image"]}]
        self.fake.cpus[14] = 6
        job = self.gated_job()
        self.fake.verdicts[job] = "outside"
        self.tick()
        del self.fake.verdicts[job]
        self.tick()
        self.assertNotIn(job, self.state["native_spot"]["pending"])
        self.assertEqual(self.state["native_spot"]["gated"][job]["phase"], "third_wait")

    def test_a_drain_publishes_a_page_without_a_cross_build_pair(self):
        self.tick()
        self.results_in()
        self.tick(drain=True)
        self.assertEqual(self.state["native_spot"].get("pending", {}), {})
        self.assertEqual(self.state["native_spot"].get("counts", {}).get("no_peer"), 1)

    def test_removing_the_setting_does_not_release_a_withheld_page(self):
        job = self.gated_job()
        self.fake.verdicts[job] = "outside"
        self.tick()
        self.assertEqual(self.state["native_spot"]["gated"][job]["phase"], "third_wait")
        del self.config["native_spot_check"]
        self.tick()
        self.tick()
        self.assertNotIn(job, self.collected())
        self.assertNotIn(job, self.published())

    def test_a_kept_out_page_does_not_stop_the_refill(self):
        self.config["native_spot_check"]["canary_pages_per_host"] = 3
        self.fake.cpus.pop(13)
        self.tick()
        one, two = sorted(self.fake.jobs)[:2]
        for job in (one, two):
            self.fake.jobs[job]["spool"] = "/project/usnmocr/results/7"
        # The first result is drawn for the Linux peer and kept out; the second is passed over.
        batch.SPOT_RANDOM = Sequence([0.0, 1.0])
        before = len(self.native_submitted())
        self.tick()
        self.assertEqual(list(self.state["native_spot"]["gated"]), [one])
        self.assertIn(two, self.published())
        # The slot the published page freed is filled in the same tick, while the other result is
        # kept out: it is not a held result, which would stop every claim and submit.
        self.assertGreater(len(self.native_submitted()), before)
        self.fake.verdicts[one] = "outside"
        self.tick()
        self.assertEqual(self.state["native_spot"]["gated"][one]["phase"], "third_wait")

    def test_replays_count_retained_results_but_not_the_ones_they_settle(self):
        self.config["native_spot_check"]["canary_pages_per_host"] = 3
        self.fake.cpus.pop(13)
        self.tick()
        one = sorted(self.fake.jobs)[0]
        self.fake.jobs[one]["spool"] = "/project/usnmocr/results/7"
        rooms = []
        real = batch.native_spot_step

        def step(config, state, transport, now, submit_room=100, **kwargs):
            rooms.append(submit_room)
            return real(config, state, transport, now, submit_room=submit_room, **kwargs)

        retained = batch.retained_backlog
        with patch.object(batch, "native_spot_step", step):
            self.tick(max_global_pending=50)
            self.assertEqual(list(self.state["native_spot"]["gated"]), [one])
            # The kept-out result is in the cap (retained_backlog) but not in the replay budget.
            with_gated = rooms[-1]
            self.tick(max_global_pending=50)
            self.assertEqual(rooms[-1], with_gated)
            # Two other retained results are.
            with patch.object(batch, "retained_backlog",
                              lambda *args, **kwargs: (retained(*args, **kwargs)[0] + 2, retained(*args, **kwargs)[1])):
                self.tick(max_global_pending=50)
            self.assertEqual(rooms[-1], with_gated - 2)

    def test_the_pair_store_reads_its_history_once_for_a_whole_flush(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / batch.NATIVE_SPOT_PAIR_LOG
            for n in range(5):
                batch.append_pair_log(path, {"event": "native_spot_pair", "job": f"{n:064x}", "peer_host": 9})
            store = batch.PairStore(path)
            reads = []
            real = Path.read_text

            def counting(self, *args, **kwargs):
                reads.append(self)
                return real(self, *args, **kwargs)

            with patch.object(Path, "read_text", counting):
                wrote = [store.append({"event": "native_spot_pair", "job": f"{n:064x}", "peer_host": 9})
                         for n in range(3, 9)]
            self.assertEqual(wrote, [False, False, True, True, True, True])
            self.assertEqual(len(reads), 1)
            self.assertEqual(len(path.read_text().splitlines()), 9)

    def native_submitted(self):
        return [unit for action in self.fake.actions if action[0] == "submit-native" for unit in action[1]]

    def test_a_pair_the_store_cannot_take_is_kept_and_tried_again_without_a_duplicate(self):
        self.feed("match", 0.97, f"{1:064x}")
        queue = self.state["native_spot"]["pair_log"]
        self.assertEqual(len(queue), 1)

        def broken(record):
            raise OSError("disk full")

        batch.native_spot_flush_pairs(self.state, broken, NOW)
        self.assertEqual(len(self.state["native_spot"]["pair_log"]), 1)
        self.assertEqual(self.state["native_spot"]["pair_log_failing"]["queued"], 1)
        alerts = batch.native_spot_alerts(self.config, self.state)
        self.assertIn("native_spot_pair_log_failing", [a["code"] for a in alerts])
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / batch.NATIVE_SPOT_PAIR_LOG
            batch.native_spot_flush_pairs(self.state, lambda r: batch.append_pair_log(path, r), NOW)
            self.assertNotIn("pair_log", self.state["native_spot"])
            self.assertNotIn("pair_log_failing", self.state["native_spot"])
            # The controller stopped between the write and its state: the same pair comes again.
            record = json.loads(path.read_text())
            self.assertFalse(batch.append_pair_log(path, record))
            self.assertEqual(len(path.read_text().splitlines()), 1)
            self.assertEqual(len(REPORT.read_pairs(path)), 1)
            # A raw duplicate line, as an older writer could leave, counts once in the report.
            path.write_text(path.read_text() * 2)
            self.assertEqual(len(REPORT.read_pairs(path)), 1)

    def test_the_pair_store_appends_one_line_a_pair_and_the_report_reads_it(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / batch.NATIVE_SPOT_PAIR_LOG
            for n in range(120):
                batch.append_pair_log(path, {"event": "native_spot_pair", "at": NOW + n, "job": f"{n:064x}", "source_host": 16,
                                             "peer_host": 9, "status": "outside" if n % 5 == 0 else "match",
                                             "cer": 0.01 * (n % 30), "word_f1": 1 - 0.005 * (n % 30),
                                             "peer_gate_pass": True, "third_reading": False,
                                             "source_build": WINDOWS, "peer_build": LINUX, "page": f"p{n}"})
            self.assertEqual(len(path.read_text().splitlines()), 120)
            report = REPORT.report(REPORT.read_pairs(path), min_pairs=100)
            self.assertEqual(report["pairs"], 120)
            self.assertEqual(report["by_status"], {"match": 96, "outside": 24})
            self.assertIn("p50", report["cer"])
            self.assertEqual(report["hosts"]["16"]["outside"], 24)
            self.assertIsNone(REPORT.report(REPORT.read_pairs(path), min_pairs=500))


class SpotStepLockTests(unittest.TestCase):
    """#1919: the native-spot step never gates or delays a plan's submit."""

    def setUp(self):
        self.config = spot_config()
        self.state = {}
        self.fake = SpotFake([tbs.message(n) for n in range(1, 60)], {13: 24, 240: 24})
        self.fake.assertions = []
        self.saved = batch.SPOT_RANDOM
        batch.SPOT_RANDOM = Always(0.0)
        self.addCleanup(setattr, batch, "SPOT_RANDOM", self.saved)

    def tick(self, now=NOW, **kwargs):
        # An inventory taken at `now`, so ticks hours apart are each fresh.
        inventory = type(self.fake).inventory.__get__(self.fake)
        with patch.object(self.fake, "inventory", lambda: {**inventory(), "created_at": now}):
            return batch.tick(self.config, self.state, self.fake, lambda: None, now, **kwargs)

    def results_in(self):
        for job in self.fake.jobs.values():
            if job["stage"] == "submitted":
                job["spool"] = "/project/usnmocr/results/7"

    def matched(self):
        """Both hosts' checks running: a replay of each matched."""
        self.tick()
        self.results_in()
        self.tick()
        for job in self.state["native_spot"]["pending"]:
            self.fake.verdicts[job] = "match"
        self.tick()

    def test_a_held_step_lock_never_delays_submit(self):
        self.matched()
        self.results_in()
        self.fake.lock_held = True
        self.fake.actions.clear()
        event = self.tick()
        kinds = [action[0] for action in self.fake.actions]
        # Submit went first and sent work; the spot step came after it and was skipped.
        self.assertIn("submit-native", kinds)
        self.assertIn("native-spot", kinds)
        self.assertLess(kinds.index("submit-native"), kinds.index("native-spot"))
        spot = event["native"]["spot_check"]
        self.assertEqual(spot["step"], "skipped")
        # A busy lock is no failure: no alert, no cap, and the hosts keep their full room.
        self.assertIsNone(spot["failing"])
        self.assertEqual([a["code"] for a in event["alerts"] if a["code"].startswith("native_spot")], [])
        self.assertEqual(event["native"]["slots"], {"13": 4, "240": 4})
        self.assertEqual(spot["lock_busy"], {"since": NOW, "ticks": 1, "last": NOW})

    def test_the_step_asks_for_a_short_lock_wait_whatever_the_long_one(self):
        directory = Path(self.enterContext(tempfile.TemporaryDirectory()))
        for spec, wait in (({}, str(batch.NATIVE_SPOT_LOCK_WAIT_SECS)), ({"lock_wait_secs": 0}, "0"),
                           ({"lock_wait_secs": 30}, "30")):
            with self.subTest(spec=spec):
                config = spot_config(**spec)
                config["step_command"] = ["ssh", "boinc.example", "--"]
                transport = batch.Transport(config, directory, step_lock_wait_secs=1200)
                stderr = "error: another bridge step still holds the lock after 10s\n"
                failed = subprocess.CompletedProcess([], 1, "", stderr)
                with patch.object(batch.subprocess, "run", return_value=failed) as run:
                    with self.assertRaises(batch.StepLockTimeout):
                        transport.native_spot("x", "")
                argv, kwargs = run.call_args
                argv = argv[0]
                self.assertEqual(argv[argv.index("--lock-wait-secs") + 1], wait)
                # The long wait isn't added to its budget either.
                self.assertEqual((argv[argv.index("--wait-mins") + 1], kwargs["timeout"]), ("30", 2100))
                # submit-remote keeps the long wait.
                argv, _, _ = transport._step_argv("bridge", "submit-remote", {})
                self.assertEqual(argv[argv.index("--lock-wait-secs") + 1], "1200")

    def test_a_skipped_step_is_tried_again_on_a_later_tick(self):
        self.tick()
        self.results_in()
        self.fake.lock_held = True
        self.tick()
        spot = self.state["native_spot"]
        queued = dict(spot["pending"])
        self.assertEqual(len(queued), 2)
        self.assertEqual({entry["stage"] for entry in queued.values()}, {"queued"})
        self.assertEqual(self.fake.spot_calls, [])
        # Skipped again: nothing piles up, nothing is lost or given up.
        event = self.tick(NOW + 600)
        self.assertEqual(spot["pending"], queued)
        self.assertEqual(spot["counts"]["lock_busy"], 2)
        self.assertEqual(event["native"]["spot_check"]["queued"], 2)
        self.assertEqual(spot["lock_busy"], {"since": NOW, "ticks": 2, "last": NOW + 600})
        # The lock is free: the same replays are made, and the streak ends.
        self.fake.lock_held = False
        event = self.tick(NOW + 1200)
        (submit, _), = self.fake.spot_calls
        self.assertEqual({item.split("=")[0] for item in submit.split(",")}, set(queued))
        self.assertEqual({entry["stage"] for entry in spot["pending"].values()}, {"submitted"})
        self.assertNotIn("lock_busy", spot)
        self.assertEqual(event["native"]["spot_check"]["step"], "ran")

    def test_a_long_run_of_skips_alerts_and_the_checks_lapse_after_fresh_hours(self):
        self.matched()
        self.fake.lock_held = True
        self.state["native_spot"]["pending"]["f" * 64] = {
            "source": 13, "target": 240, "image": "sha256:" + "a" * 64, "page": "p",
            "spool": "/project/usnmocr/results/7", "at": NOW, "stage": "queued"}
        event = self.tick(NOW + 3600)
        self.assertNotIn("native_spot_lock_busy", [a["code"] for a in event["alerts"]])
        event = self.tick(NOW + 3600 + batch.NATIVE_SPOT_LOCK_BUSY_ALERT_SECS)
        self.assertIn("native_spot_lock_busy", [a["code"] for a in event["alerts"]])
        self.assertEqual(event["native"]["slots"], {"13": 4, "240": 4})
        # No match for fresh_hours: canary size, as for any host whose checks don't run.
        event = self.tick(NOW + 49 * 3600)
        self.assertEqual(event["native"]["slots"], {"13": 1, "240": 1})
        # The queued replay was given up at its age, not kept forever.
        event = self.tick(NOW + batch.NATIVE_SPOT_MAX_AGE + 3600)
        self.assertNotIn("f" * 64, self.state["native_spot"]["pending"])

    def test_a_step_that_raises_anything_does_not_fail_the_tick(self):
        self.tick()
        self.results_in()

        def broken(submit, check):
            raise TypeError("unexpected report")

        self.fake.native_spot = broken
        event = self.tick()
        self.assertEqual(event["native"]["spot_check"]["step"], "failed")
        self.assertIn("native_spot_failing", [a["code"] for a in event["alerts"]])

    def test_replays_go_only_to_a_configured_peer_that_is_active(self):
        self.config = spot_config(hosts=(13,), peers=[{"host": 300, "image": "sha256:" + "a" * 64}])
        self.fake.cpus = {13: 24}
        self.tick()
        self.results_in()
        self.tick()
        # 300 hasn't asked for work lately: a replay assigned to it would sit unsent.
        self.assertEqual(self.fake.spot_calls, [])
        self.assertEqual(self.state["native_spot"]["counts"]["no_peer"], 1)
        self.fake.cpus = {13: 24, 300: 8}
        self.results_in()
        self.tick()
        self.tick()
        self.assertTrue(any(":300:" in submit for submit, _ in self.fake.spot_calls))

    def test_a_queued_replay_waits_while_its_peer_is_away(self):
        self.tick()
        self.results_in()
        self.fake.lock_held = True
        self.tick()
        queued = dict(self.state["native_spot"]["pending"])
        self.assertEqual(len(queued), 2)
        # Host 240 stops asking for work while the lock is busy: its replay isn't made.
        self.fake.lock_held = False
        self.fake.cpus = {13: 24}
        self.tick(NOW + 600)
        (submit, _), = self.fake.spot_calls
        self.assertEqual([item.split("=")[1].split(":")[1] for item in submit.split(",")], ["13"])
        waiting = [job for job, entry in self.state["native_spot"]["pending"].items() if entry["stage"] == "queued"]
        self.assertEqual(len(waiting), 1)
        self.assertEqual(queued[waiting[0]]["target"], 240)

    def test_no_replay_is_made_while_the_plan_is_paused(self):
        self.tick()
        self.results_in()
        self.state["native_paused"] = {"at": NOW, "reason": "test", "jobs": []}
        self.tick()
        self.assertEqual([submit for submit, _ in self.fake.spot_calls if submit], [])

    def test_an_unexpected_error_ends_a_run_of_skips(self):
        self.tick()
        self.results_in()
        self.fake.lock_held = True
        self.tick()
        self.assertIn("lock_busy", self.state["native_spot"])

        def broken(submit, check):
            raise TypeError("unexpected report")

        self.fake.native_spot = broken
        self.tick(NOW + 60)
        self.assertNotIn("lock_busy", self.state["native_spot"])

    def native_submissions(self):
        return [unit for action in self.fake.actions if action[0] == "submit-native" for unit in action[1]]

    def test_replay_room_counts_the_units_this_tick_submitted(self):
        self.tick()
        self.results_in()
        # The cap has room for two units. The tick's draw queued two replays, and the refill
        # leaves the cap's room to them (#2081): it submits nothing, and the replays go.
        pending = self.fake.inventory()["pending_workunits"]
        self.fake.actions.clear()
        self.tick(max_global_pending=pending + 2)
        self.assertEqual(self.native_submissions(), [])
        self.assertEqual(len([item for submit, _ in self.fake.spot_calls if submit for item in submit.split(",")]), 2)

    def test_queued_replays_keep_their_global_room_through_repeated_capped_refills(self):
        # #2081: the refill fills the global cap each tick; the replay step runs after it. A
        # replay queued to an active peer keeps its place under the cap, and goes on the first
        # tick, while the refill takes the rest of the room.
        self.config["native_spot_check"]["canary_pages_per_host"] = 4
        self.tick()
        self.results_in()
        batch.SPOT_RANDOM = Always(1.0)  # no new draws
        replay = "e" * 64
        self.state["native_spot"]["pending"] = {replay: {
            "source": 13, "target": 240, "image": self.config["image"], "page": "p",
            "spool": "/project/usnmocr/results/7", "at": NOW, "stage": "queued"}}
        for n in range(3):
            pending = self.fake.inventory()["pending_workunits"]
            self.fake.actions.clear()
            self.fake.spot_calls.clear()
            self.tick(NOW + 60 * n, max_global_pending=pending + 2)
            submitted = len(self.native_submissions())
            if self.state["native_spot"]["pending"].get(replay, {}).get("stage") != "queued":
                break
            self.results_in()
        self.assertEqual(n, 0)
        self.assertEqual(self.state["native_spot"]["pending"][replay]["stage"], "submitted")
        self.assertEqual(submitted, 1)

    def test_the_global_reservation_skips_replays_that_cannot_go_this_tick(self):
        # #2081 review: a replay whose peer has no shared room left (another plan fills 240) and
        # one past its age are kept no room under the global cap, so the refill takes all of it.
        self.config["native_spot_check"]["canary_pages_per_host"] = 4
        self.tick()
        self.results_in()
        batch.SPOT_RANDOM = Always(1.0)  # no new draws
        entry = {"image": self.config["image"], "page": "p", "spool": "/project/usnmocr/results/7",
                 "stage": "queued"}
        self.state["native_spot"]["pending"] = {
            "e" * 64: {**entry, "source": 13, "target": 240, "at": NOW},
            "f" * 64: {**entry, "source": 240, "target": 13, "at": NOW - batch.NATIVE_SPOT_MAX_AGE - 3600}}
        inventory = type(self.fake).inventory.__get__(self.fake)
        full = {"240": {"units": 1000, "jobs": []}}
        pending = inventory()["pending_workunits"]
        self.fake.actions.clear()
        self.fake.spot_calls.clear()
        with patch.object(self.fake, "inventory",
                          lambda: {**inventory(), "created_at": NOW + 60, "native_host_units": full}):
            batch.tick(self.config, self.state, self.fake, lambda: None, NOW + 60,
                       max_global_pending=pending + 2)
        self.assertEqual(len(self.native_submissions()), 2)
        self.assertEqual(self.state["native_spot"]["pending"]["e" * 64]["stage"], "queued")


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
