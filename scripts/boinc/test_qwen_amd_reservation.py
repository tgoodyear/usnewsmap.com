import copy
import importlib.util
import contextlib
import io
import json
from pathlib import Path
import shutil
import subprocess
import unittest
import uuid
import xml.etree.ElementTree as ET
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[2]


def load(name, path):
    spec = importlib.util.spec_from_file_location(name, ROOT / path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


R = load("amd_reservation", "scripts/boinc/qwen-amd-reservation.py")
POLICY = load("amd_reservation_refresh", "contrib-compute/images/server/refresh-plan-policy.py")
HELPER = ROOT / "scripts/boinc/native-qwen-cohort.php"
LEASE = "a" * 32


class AmdReservation(unittest.TestCase):
    def setUp(self):
        self.root = ROOT / (".local-amd-reservation-tests-" + uuid.uuid4().hex)
        self.root.mkdir()
        self.addCleanup(shutil.rmtree, self.root)
        self.source = (ROOT / "contrib-compute/boinc/plan_class_spec.xml").read_bytes()
        root, nodes = R.parse_policy(self.source)
        for tag, value in (("min_wu_id", "2"), ("max_wu_id", "1")):
            ET.SubElement(nodes["docker_nvidia_cuda_8b"], tag).text = value
        for name in ("intel_vulkan_8b", "nvidia_vulkan_8b"):
            nodes[name].find("min_wu_id").text = "90"
            nodes[name].find("max_wu_id").text = "99"
        self.policy = R.serialize(root)
        self.snapshot = {
            "unit": {"id": 100, "appid": 6, "batch": 8, "min_quorum": 2, "target_nresults": 2,
                     "rsc_disk_bound": 19327352832, "canonical_resultid": 0, "error_mask": 0,
                     "transitioner_flags": 0, "need_validate": 0, "app_version_num": 0},
            "app": {"name": "usnm_qwen"}, "newer": 0, "assignments": 0,
            "results": [dict(id=value, server_state=2, sent_time=0, hostid=0, outcome=0,
                             validate_state=0, app_version_id=0, received_time=0) for value in (201, 202)],
            "versions": [{"id": 68, "version_num": 114, "platformid": 5, "plan_class": R.AMD},
                         {"id": 37, "version_num": 105, "platformid": 2, "plan_class": R.APPLE}],
        }

    def reserved(self, expires=2000):
        return R.reserve_policy(self.policy, LEASE, 100, expires)

    def test_only_tail_competitors_are_excluded_and_release_keeps_other_windows(self):
        reserved = self.reserved()
        _, before = R.parse_policy(self.policy)
        _, after = R.parse_policy(reserved)
        self.assertEqual(R.bounds(after[R.AMD]), (100, 100))
        self.assertEqual(after[R.APPLE].findtext("max_wu_id"), "99")
        for name in before.keys() - {R.AMD, R.APPLE}:
            self.assertEqual(ET.tostring(before[name]), ET.tostring(after[name]))
        # A concurrent reviewed prefix refill must survive release.
        root, nodes = R.parse_policy(reserved)
        nodes["intel_vulkan_8b"].find("min_wu_id").text = "95"
        released = R.release_policy(R.serialize(root), LEASE, 100, self.policy)
        root, nodes = R.parse_policy(released)
        self.assertEqual(R.bounds(nodes[R.AMD]), (2, 1))
        self.assertIsNone(nodes[R.APPLE].find("max_wu_id"))
        self.assertEqual(nodes["intel_vulkan_8b"].findtext("min_wu_id"), "95")
        self.assertIsNone(R.reservation_marker(root))
        self.assertEqual(R.release_policy(released, LEASE, 100, self.policy), released)

    def test_release_never_clobbers_a_different_lease_or_changed_owned_bound(self):
        reserved = self.reserved()
        self.assertEqual(R.release_policy(reserved, "b" * 32, 100, self.policy), reserved)
        root, nodes = R.parse_policy(reserved)
        nodes[R.APPLE].find("max_wu_id").text = "98"
        with self.assertRaisesRegex(ValueError, "ceiling changed"):
            R.release_policy(R.serialize(root), LEASE, 100, self.policy)
        with self.assertRaisesRegex(ValueError, "already owns"):
            R.reserve_policy(reserved, "b" * 32, 101, 2000)

    def test_reservation_rejects_competing_general_or_crossing_windows(self):
        for name in R.COMPETITORS:
            for general in (False, True):
                with self.subTest(name=name, general=general):
                    root, nodes = R.parse_policy(self.policy)
                    if general:
                        for tag in ("min_wu_id", "max_wu_id"):
                            nodes[name].remove(nodes[name].find(tag))
                    else:
                        nodes[name].find("min_wu_id").text = "99"
                        nodes[name].find("max_wu_id").text = "100"
                    with self.assertRaises(ValueError):
                        R.reserve_policy(R.serialize(root), LEASE, 100, 2000)

    def test_strict_pair_refuses_assigned_failed_unknown_and_not_latest_work(self):
        R.strict_pair(self.snapshot, 100, 68)
        mutations = (
            lambda s: s["unit"].update(min_quorum=1),
            lambda s: s.update(assignments=1),
            lambda s: s.update(newer=1),
            lambda s: s["results"][0].update(hostid=1),
            lambda s: s["results"][0].update(outcome=1),
            lambda s: s["results"][0].update(app_version_id=37),
            lambda s: s["versions"].append({"plan_class": ""}),
            lambda s: s["versions"][0].update(version_num=111),
        )
        for mutate in mutations:
            current = copy.deepcopy(self.snapshot)
            mutate(current)
            with self.assertRaises(ValueError):
                R.strict_pair(current, 100, 68)

    def test_actual_assignment_race_and_expiry_have_distinct_release_reasons(self):
        intent = {"host": 5, "app_version": 68, "expires": 2000}
        self.assertIsNone(R.tick_reason(self.snapshot, intent, 1999))
        self.assertEqual(R.tick_reason(self.snapshot, intent, 2000), "lease_expired")
        current = copy.deepcopy(self.snapshot)
        current["results"][0].update(hostid=1, app_version_id=37, sent_time=10, server_state=4)
        self.assertIn("no_assignment_stolen", R.tick_reason(current, intent, 1999))
        current["results"][1].update(hostid=5, app_version_id=68, sent_time=11, server_state=4)
        self.assertEqual(R.tick_reason(current, intent, 1999),
                         "actual_amd_assignment_not_scientific_acceptance")

    def test_restart_preserves_live_ceiling_and_expires_both_owned_gates(self):
        with patch.object(POLICY.time, "time", return_value=1999):
            live = POLICY.merge_policy(self.source, self.reserved())
        root, nodes = R.parse_policy(live)
        self.assertEqual(nodes[R.APPLE].findtext("max_wu_id"), "99")
        self.assertEqual(R.bounds(nodes[R.AMD]), (100, 100))
        self.assertIsNotNone(R.reservation_marker(root))
        with patch.object(POLICY.time, "time", return_value=2000):
            expired = POLICY.merge_policy(self.source, live)
        root, nodes = R.parse_policy(expired)
        self.assertIsNone(nodes[R.APPLE].find("max_wu_id"))
        self.assertEqual(R.bounds(nodes[R.AMD]), (2, 1))
        self.assertIsNone(R.reservation_marker(root))

    @unittest.skipUnless(shutil.which("php"), "PHP unavailable")
    def test_existing_cohort_helper_cannot_open_over_the_reserved_tail(self):
        def guarded(updated):
            code = ("require $argv[1]; native_qwen_reservation_guard("
                    + json.dumps(self.reserved().decode()) + ", " + json.dumps(updated.decode()) + ");")
            return subprocess.run(["php", "-r", code, str(HELPER)], capture_output=True)
        self.assertEqual(guarded(self.reserved()).returncode, 0)
        root, nodes = R.parse_policy(self.reserved())
        nodes["nvidia_vulkan_8b"].find("max_wu_id").text = "100"
        self.assertNotEqual(guarded(R.serialize(root)).returncode, 0)
        nodes["nvidia_vulkan_8b"].find("min_wu_id").text = "101"
        nodes["nvidia_vulkan_8b"].find("max_wu_id").text = "110"
        self.assertEqual(guarded(R.serialize(root)).returncode, 0)

    def input_fixture(self, text="א" * 4096):
        download = self.root / "download"
        download.mkdir(exist_ok=True)
        page = b"immutable page bytes independently checked by the producer"
        job = {"doc_id": "ordinary-source-page", "input": {"bytes": len(page), "sha256": R.digest(page)},
               "engine": dict(R.ENGINE, models=R.MODELS), "v": 1, "prompt_version": 1,
               "max_tokens_per_1000px2": 4, "group": "qwen-nonenglish-v1", "language": "yid",
               "crops": [{"id": "c01", "loc_text": text}]}
        raw = json.dumps(job).encode()
        (download / "input_qwen_json").write_bytes(raw)
        (download / "input_page_jpg").write_bytes(page)
        self.snapshot["unit"]["xml_doc"] = "".join(
            f"<file_info><name>input_{suffix}</name><url>https://example.test/usnmocr_download/input_{suffix}"
            "</url></file_info>" for suffix in ("qwen_json", "page_jpg"))
        expected = {"workunit_id": 100, "job_sha256": R.digest(raw), "page_sha256": R.digest(page),
                    "doc_id": job["doc_id"], "quality_sha256": "a" * 64, "provenance_sha256": "b" * 64,
                    "provenance": {"doc_id": job["doc_id"]}}
        return expected

    def test_input_hash_identity_and_utf8_limit_are_independent_gates(self):
        expected = self.input_fixture()
        self.assertEqual(R.validate_inputs(self.root, self.snapshot, expected)["crop_count"], 1)
        expected["doc_id"] = "different-page"
        with self.assertRaises(ValueError):
            R.validate_inputs(self.root, self.snapshot, expected)
        expected = self.input_fixture("א" * 4097)
        with self.assertRaisesRegex(ValueError, "UTF-8"):
            R.validate_inputs(self.root, self.snapshot, expected)
        expected = self.input_fixture()
        (self.root / "download/input_page_jpg").write_bytes(b"changed")
        with self.assertRaisesRegex(ValueError, "changed"):
            R.validate_inputs(self.root, self.snapshot, expected)

    def test_failed_policy_replace_preserves_original_and_only_cleans_owned_stage(self):
        path = self.root / "plan_class_spec.xml"
        path.write_bytes(self.policy)
        for operation in ("fsync", "replace"):
            with self.subTest(operation=operation):
                with patch.object(R.os, operation, side_effect=OSError("storage failure")):
                    with self.assertRaises(OSError):
                        R.replace_policy(path, self.policy, self.reserved())
                self.assertEqual(path.read_bytes(), self.policy)
                self.assertEqual(list(self.root.glob(".amd-reservation-*")), [])

    def install_cli_lease(self):
        expected = self.input_fixture()
        (self.root / "plan_class_spec.xml").write_bytes(self.policy)
        (self.root / "config.xml").write_text(
            "<boinc><config><one_result_per_host_per_wu/></config></boinc>")
        evidence = self.root / "evidence.json"
        evidence.write_text(json.dumps(expected))
        rollback = self.root / "rollback.json"
        rollback.write_text(json.dumps({"lease": LEASE, "active": True, "deadline": 2000}))
        common = ["--project", str(self.root), "--lease", LEASE, "--apply"]
        reserve = ["reserve", *common, "--workunit", "100", "--expected-policy-sha256", R.digest(self.policy),
                   "--cohort-helper", str(HELPER), "--cohort-helper-sha256", R.digest(HELPER.read_bytes()),
                   "--evidence", str(evidence), "--rollback-receipt", str(rollback)]
        with patch.object(R, "snapshot", return_value=self.snapshot), \
                patch.object(R, "scheduler_processes", return_value={}), \
                patch.object(R.time, "time", return_value=1000), contextlib.redirect_stdout(io.StringIO()):
            R.main(reserve)
        return common

    @unittest.skipUnless(shutil.which("php"), "PHP unavailable")
    def test_real_policy_journal_tick_and_db_independent_deadline_release(self):
        common = self.install_cli_lease()
        with patch.object(R, "snapshot", return_value=self.snapshot), \
                patch.object(R, "scheduler_processes", return_value={}), \
                patch.object(R.time, "time", return_value=1000):
            output = io.StringIO()
            with contextlib.redirect_stdout(output):
                R.main(["tick", *common])
            self.assertTrue(json.loads(output.getvalue())["old_schedulers_drained_and_pair_untouched"])
        journal = self.root / "ops-issue-559/amd-reservation"
        intent = json.loads((journal / (LEASE + ".json")).read_bytes())
        self.assertEqual(intent["before_sha256"], R.digest(self.policy))
        self.assertFalse(intent["accuracy_established"])
        with patch.object(R, "snapshot", side_effect=RuntimeError("DB unavailable")), \
                contextlib.redirect_stdout(io.StringIO()):
            R.main(["release", *common])
        _, nodes = R.parse_policy((self.root / "plan_class_spec.xml").read_bytes())
        self.assertIsNone(nodes[R.APPLE].find("max_wu_id"))
        self.assertEqual(R.bounds(nodes[R.AMD]), (2, 1))

    @unittest.skipUnless(shutil.which("php"), "PHP unavailable")
    def test_db_read_crossing_deadline_closes_instead_of_qualifying_stale_lease(self):
        common = self.install_cli_lease()
        clock = [1999]
        def delayed_snapshot(*_):
            clock[0] = 2001
            return self.snapshot
        output = io.StringIO()
        with patch.object(R, "snapshot", side_effect=delayed_snapshot), \
                patch.object(R, "scheduler_processes", return_value={}), \
                patch.object(R.time, "time", side_effect=lambda: clock[0]), contextlib.redirect_stdout(output):
            R.main(["tick", *common])
        receipt = json.loads(output.getvalue())
        self.assertEqual(receipt["reason"], "lease_expired")
        self.assertFalse(receipt["old_schedulers_drained_and_pair_untouched"])
        _, nodes = R.parse_policy((self.root / "plan_class_spec.xml").read_bytes())
        self.assertIsNone(nodes[R.APPLE].find("max_wu_id"))
        self.assertEqual(R.bounds(nodes[R.AMD]), (2, 1))

    def test_scheduler_pid_reuse_does_not_hold_the_guard(self):
        with patch.object(R, "scheduler_processes", return_value={"123": "new-start"}):
            self.assertFalse(R.old_scheduler_alive(self.root, {"123": "old-start"}))
        with patch.object(R, "scheduler_processes", return_value={"123": "old-start"}):
            self.assertTrue(R.old_scheduler_alive(self.root, {"123": "old-start"}))

    def test_drain_proof_also_requires_the_owned_policy_still_to_exclude_competitors(self):
        intent = {"lease": LEASE, "workunit": 100, "expires": 2000}
        self.assertTrue(R.guarded_policy(self.reserved(), intent))
        self.assertFalse(R.guarded_policy(self.policy, intent))
        root, nodes = R.parse_policy(self.reserved())
        nodes["intel_vulkan_8b"].find("max_wu_id").text = "100"
        self.assertFalse(R.guarded_policy(R.serialize(root), intent))
        nodes["intel_vulkan_8b"].find("min_wu_id").text = "101"
        nodes["intel_vulkan_8b"].find("max_wu_id").text = "110"
        self.assertTrue(R.guarded_policy(R.serialize(root), intent))

    def test_newer_feed_work_does_not_invalidate_an_existing_untouched_reservation(self):
        current = copy.deepcopy(self.snapshot)
        current["newer"] = 19
        with self.assertRaisesRegex(ValueError, "newest"):
            R.strict_pair(current, 100, 68)
        R.strict_pair(current, 100, 68, newest=False)

if __name__ == "__main__":
    unittest.main()
