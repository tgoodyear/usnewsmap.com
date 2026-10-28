import copy
import configparser
import importlib.util
import json
from pathlib import Path
import shutil
import shlex
import subprocess
from types import SimpleNamespace
import unittest
import uuid
import xml.etree.ElementTree as ET
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location("auto_cohort", ROOT / "scripts/boinc/qwen-auto-cohort.py")
R = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(R)
HELPER = ROOT / "scripts/boinc/native-qwen-cohort.php"


class AutomaticCohort(unittest.TestCase):
    def setUp(self):
        self.root = ROOT / (".local-auto-cohort-tests-" + uuid.uuid4().hex)
        self.project = self.root / "usnmocr"
        self.project.mkdir(parents=True)
        self.addCleanup(shutil.rmtree, self.root)
        self.config = self.project / "config.xml"
        self.config.write_text("<boinc><config><one_result_per_host_per_wu/></config></boinc>")
        root = ET.fromstring((ROOT / "contrib-compute/boinc/plan_class_spec.xml").read_bytes())
        docker = root.find("plan_class[name='docker_nvidia_cuda_8b']")
        for tag, value in (("min_wu_id", "2"), ("max_wu_id", "1")):
            ET.SubElement(docker, tag).text = value
        self.policy = self.project / "plan_class_spec.xml"
        self.policy.write_bytes(ET.tostring(root))
        self.audit = {}
        self.rows = [self.input(100), self.input(101)]
        audit_path = self.root / "qwen/qwen-nonenglish-v1/audit.json"
        audit_path.write_text(json.dumps(self.audit))
        self.database = {"rows": self.rows, "app": {"name": "usnm_qwen", "homogeneous_redundancy": 0},
                         "gpu_inflight": [],
                         "versions": [{"id": 66, "version_num": 114, "platformid": 1,
                                       "plan_class": "nvidia_vulkan_8b"},
                                      {"id": 67, "version_num": 114, "platformid": 1,
                                       "plan_class": "intel_vulkan_8b"}]}
        inc = self.project / "html/inc"
        inc.mkdir(parents=True)
        (self.project / "database.json").write_text(json.dumps(self.rows))
        (inc / "boinc_db.inc").write_text("""<?php
$GLOBALS['fixture']=json_decode(file_get_contents(__DIR__.'/../../database.json'));
class BoincWorkunit {
 static function lookup_id($id) {foreach($GLOBALS['fixture'] as $r)if($r->unit->id==$id)return $r->unit;return null;}
}
class BoincApp {static function lookup_id($id){return (object)['name'=>'usnm_qwen'];}}
class BoincResult {static function enum($where){preg_match('/workunitid=(\\d+)/',$where,$m);
 foreach($GLOBALS['fixture'] as $r)if($r->unit->id==$m[1])return $r->results;return [];}}
class BoincDb {static function get(){return new self();}function count($table,$where){return 0;}}
""")
        self.args = SimpleNamespace(
            project=self.project, helper=HELPER, helper_sha256=R.sha(HELPER.read_bytes()),
            config_sha256=R.sha(self.config.read_bytes()), initial_policy_sha256=R.sha(self.policy.read_bytes()),
            controller="a" * 32, minimum_workunit=100, max_admissions=500, max_inflight_per_host=10,
            enabled_until=10000000000,
            intel_host=9, nvidia_host=10, apply=True,
        )

    def input(self, number, language="yid"):
        doc, job_id = "ordinary-page-" + str(number), f"{number:064x}"
        page = b"immutable JPEG fixture bytes-" + str(number).encode()
        job = {"doc_id": doc, "id": job_id, "v": 1, "prompt_version": 1,
               "max_tokens_per_1000px2": 4, "group": "qwen-nonenglish-v1", "language": language,
               "engine": dict(R.ENGINE, models=R.MODELS),
               "input": {"bytes": len(page), "sha256": R.sha(page)},
               "crops": [{"id": "c01", "loc_text": "א" * 4096}]}
        raw = json.dumps(job).encode()
        download = self.project / "download"
        download.mkdir(exist_ok=True)
        job_name, page_name = str(number) + "_qwen_json", str(number) + "_page_jpg"
        (download / job_name).write_bytes(raw)
        (download / page_name).write_bytes(page)
        unit = self.root / "qwen/qwen-nonenglish-v1/units" / doc
        unit.mkdir(parents=True, exist_ok=True)
        for name, value in (("qwen.json", raw), ("page.jpg", page),
                            ("submitted", ("usnm_qwen_" + job_id).encode()),
                            ("sample.json", json.dumps({"doc_id": doc, "lang": language,
                                                       "damage_rate": 0.9, "damage": "high"}).encode())):
            (unit / name).write_bytes(value)
        quality = self.root / "quality/qwen-nonenglish-v1/pages" / doc
        quality.mkdir(parents=True, exist_ok=True)
        (quality / "quality.json").write_text(json.dumps(
            {"doc_id": doc, "language": language, "inputs": {
                "page": job["input"], "alto": {"sha256": "c" * 64}}}))
        texts = self.root / "quality/qwen-nonenglish-v1/texts"
        texts.mkdir(exist_ok=True)
        (texts / (doc + ".loc-provenance.json")).write_text(json.dumps({
            "doc_id": doc, "archive": "ordinary-source.tar.bz2", "member": doc + "/ocr.xml",
            "archive_bytes": 1000000, "sha256": "c" * 64}))
        self.audit[job_id] = {"workunit_id": number, "job_sha256": R.sha(raw),
                              "batch": 8, "min_quorum": 2, "target_nresults": 2}
        return {
            "unit": {"id": number, "name": "usnm_qwen_" + job_id, "appid": 6, "batch": 8,
                     "min_quorum": 2, "target_nresults": 2, "rsc_disk_bound": 19327352832,
                     "canonical_resultid": 0, "error_mask": 0, "transitioner_flags": 0,
                     "need_validate": 0, "app_version_num": 0, "hr_class": 0,
                     "xml_doc": "".join("<file_info><name>" + name + "</name><url>"
                                        "https://example.test/usnmocr_download/" + name
                                        + "</url></file_info>" for name in (job_name, page_name))},
            "results": [dict(id=number * 2 + i, server_state=2, sent_time=0, hostid=0, outcome=0,
                             validate_state=0, app_version_id=0, received_time=0) for i in range(2)],
            "assignments": 0,
        }

    def test_selects_oldest_contiguous_pristine_prefix_without_crossing_reservation(self):
        rows = [self.input(i) for i in range(100, 115)]
        self.assertEqual(len(R.choose(rows, 100, None, 500)), 10)
        self.assertEqual([r["unit"]["id"] for r in R.choose(rows, 100, 105, 500)], list(range(100, 105)))
        self.assertEqual([r["unit"]["id"] for r in R.choose(rows, 105, 105, 2)], [106, 107])
        rows[0]["results"][0]["hostid"] = 1
        self.assertFalse(R.fresh(rows[0]))
        self.assertEqual(R.choose(rows, 100, None, 0), [])

    def test_source_hash_archive_binding_damage_and_multibyte_contract(self):
        for language in ("yid", "srp"):
            row = self.input(200, language)
            key = row["unit"]["name"].removeprefix("usnm_qwen_")
            proof = R.bind_inputs(self.project, row, key, self.audit[key])
            self.assertEqual(proof["source_quality"]["damage_rate"], 0.9)
            self.assertFalse(proof["accuracy_established"])
        raw = self.project / "download/200_page_jpg"
        raw.write_bytes(b"changed")
        with self.assertRaisesRegex(ValueError, "page identity"):
            R.bind_inputs(self.project, row, key, self.audit[key])

    def test_remaining_ordinary_peer_not_headline_controls_window_drain(self):
        rows = copy.deepcopy(self.rows)
        self.assertEqual(R.available_in_window(rows, [100, 101], (9, 10)), 4)
        for row in rows:
            row["results"][0].update(hostid=9, server_state=5, validate_state=4)
            row["results"][1].update(hostid=10, server_state=5, validate_state=4)
            row["results"].append(dict(id=999, hostid=0, server_state=2))
        self.assertEqual(R.available_in_window(rows, [100, 101], (9, 10)), 0)
        rows[0]["results"][0]["hostid"] = 1
        self.assertEqual(R.available_in_window(rows, [100, 101], (9, 10)), 1)

    def test_existing_two_model_metadata_does_not_admit_new_models_or_oversize_utf8(self):
        for mutation in (
            lambda job: job["engine"]["models"].append(dict(R.MODEL, name="unreviewed")),
            lambda job: job["crops"][0].update(loc_text="א" * 4097),
        ):
            row = self.input(200)
            path = self.project / "download/200_qwen_json"
            job = json.loads(path.read_bytes())
            mutation(job)
            raw = json.dumps(job).encode()
            path.write_bytes(raw)
            unit = self.root / "qwen/qwen-nonenglish-v1/units/ordinary-page-200/qwen.json"
            unit.write_bytes(raw)
            key = row["unit"]["name"].removeprefix("usnm_qwen_")
            self.audit[key]["job_sha256"] = R.sha(raw)
            with self.assertRaises(ValueError):
                R.bind_inputs(self.project, row, key, self.audit[key])

    @unittest.skipUnless(shutil.which("php"), "PHP unavailable")
    def test_real_helper_revalidates_fresh_pair_and_publishes_one_owned_window(self):
        with patch.object(R, "read_database", return_value=self.database):
            receipt = R.run(self.args)
        self.assertEqual(receipt["status"], "refilled-finite-window")
        self.assertEqual(R.policy_window(self.policy.read_bytes())[0], [100, 101])
        state = json.loads((self.project / "ops-issue-559/qwen-auto-cohort/state.json").read_bytes())
        self.assertEqual(state["admitted"], [100, 101])
        self.assertNotIn("pending", state)
        # A second ordinary tick must not overwrite/advance a still-eligible window.
        with patch.object(R, "read_database", return_value=self.database):
            self.assertEqual(R.run(self.args)["status"], "current-window-still-eligible")

    @unittest.skipUnless(shutil.which("php"), "PHP unavailable")
    def test_uncertain_publication_reconciles_exact_intent_without_double_admission(self):
        original_persist = R.persist
        def interrupt(path, value, **kw):
            if path.name == "state.json" and value["window"] == [100, 101] and "pending" not in value:
                raise OSError("observer lost after atomic policy publication")
            original_persist(path, value, **kw)
        with patch.object(R, "read_database", return_value=self.database), patch.object(R, "persist", side_effect=interrupt):
            with self.assertRaises(OSError):
                R.run(self.args)
        # A separate AMD canary must not turn a known NVIDIA/Intel publication into
        # an unknown owner merely because the whole policy SHA changed.
        root = ET.fromstring(self.policy.read_bytes())
        amd = root.find("plan_class[name='amd_vulkan_8b']")
        amd.find("min_wu_id").text = amd.find("max_wu_id").text = "200"
        self.policy.write_bytes(ET.tostring(root))
        with patch.object(R, "read_database", side_effect=AssertionError("must not replay DB/publication")):
            recovered = R.run(self.args)
        self.assertEqual(recovered["status"], "reconciled-exact-published-intent")
        self.assertEqual(recovered["admitted_unique"], 2)

    @unittest.skipUnless(shutil.which("php"), "PHP unavailable")
    def test_external_resource_gate_change_is_not_silently_adopted(self):
        with patch.object(R, "read_database", return_value=self.database):
            R.run(self.args)
        root = ET.fromstring(self.policy.read_bytes())
        root.find("plan_class[name='intel_vulkan_8b']/min_gpu_ram_mb").text = "1"
        self.policy.write_bytes(ET.tostring(root))
        with self.assertRaisesRegex(ValueError, "resource gates changed"):
            R.run(self.args)

    @unittest.skipUnless(shutil.which("php"), "PHP unavailable")
    def test_expiry_closes_own_window_without_database_or_source_audit(self):
        with patch.object(R, "read_database", return_value=self.database):
            R.run(self.args)
        (self.root / "qwen/qwen-nonenglish-v1/audit.json").unlink()
        self.args.enabled_until = 1
        with patch.object(R, "read_database", side_effect=AssertionError("DB unavailable")):
            receipt = R.run(self.args)
        self.assertEqual(receipt["status"], "closed-expired-budget-or-release-change")
        self.assertEqual(R.policy_window(self.policy.read_bytes())[0], [2, 1])

    @unittest.skipUnless(shutil.which("php"), "PHP unavailable")
    def test_atomic_expected_policy_gate_and_fresh_replica_refusal(self):
        before = self.policy.read_bytes()
        bad = subprocess.run(
            ["php", str(HELPER), "--project", str(self.project), "--plan-class", "shared_8b",
             "--stop", "--expected-policy-sha256", "0" * 64, "--apply"], capture_output=True)
        self.assertNotEqual(bad.returncode, 0)
        self.assertEqual(self.policy.read_bytes(), before)
        clean_snapshot = copy.deepcopy(self.database)
        self.rows[0]["results"][0]["outcome"] = 1
        (self.project / "database.json").write_text(json.dumps(self.rows))
        with patch.object(R, "read_database", return_value=clean_snapshot):
            with self.assertRaises(subprocess.CalledProcessError):
                R.run(self.args)
        self.assertEqual(self.policy.read_bytes(), before)

    def test_immutable_receipt_collision_never_overwrites_existing_evidence(self):
        path = self.root / "immutable.json"
        R.persist(path, {"original": True}, exclusive=True)
        with self.assertRaises(FileExistsError):
            R.persist(path, {"wrong": True}, exclusive=True)
        self.assertEqual(json.loads(path.read_bytes()), {"original": True})

    @unittest.skipUnless(shutil.which("php"), "PHP unavailable")
    def test_refill_does_not_accumulate_a_500_task_unvalidated_gpu_backlog(self):
        self.database["gpu_inflight"] = [{"hostid": 9} for _ in range(10)]
        before = self.policy.read_bytes()
        with patch.object(R, "read_database", return_value=self.database):
            self.assertEqual(R.run(self.args)["status"], "bounded-gpu-inflight-buffer-full")
            self.assertEqual(self.policy.read_bytes(), before)
            self.database["gpu_inflight"].pop()
            receipt = R.run(self.args)
        self.assertEqual(receipt["target"], [100, 100])
        self.assertEqual(receipt["native_gpu_inflight"][9], 9)

    def test_in_container_deadline_precedes_host_mutex_release_deadline(self):
        config = configparser.ConfigParser(interpolation=None)
        config.read(ROOT / "scripts/boinc/usnm-qwen-cohort-auto.service")
        command = shlex.split(config["Service"]["ExecStart"])
        podman, timeout = command.index("/usr/bin/podman"), command.index("/usr/bin/timeout")
        self.assertEqual(timeout, podman + 3)
        self.assertEqual(command[podman + 1], "exec")
        inner = float(command[timeout + 3].removesuffix("s"))
        grace = float(command[timeout + 2].split("=", 1)[1].removesuffix("s"))
        self.assertLess(inner + grace, float(config["Service"]["TimeoutStartSec"]))

    @unittest.skipUnless(shutil.which("php"), "PHP unavailable")
    def test_recovered_expired_publication_is_closed_in_same_tick_without_db(self):
        original_persist = R.persist
        def interrupt(path, value, **kw):
            if path.name == "state.json" and value["window"] == [100, 101] and "pending" not in value:
                raise OSError("observer lost after publication")
            original_persist(path, value, **kw)
        with patch.object(R, "read_database", return_value=self.database), patch.object(R, "persist", side_effect=interrupt):
            with self.assertRaises(OSError):
                R.run(self.args)
        self.args.enabled_until = 1
        (self.root / "qwen/qwen-nonenglish-v1/audit.json").unlink()
        with patch.object(R, "read_database", side_effect=AssertionError("DB unavailable")):
            receipt = R.run(self.args)
        self.assertEqual(receipt["status"], "closed-expired-budget-or-release-change")
        self.assertEqual(R.policy_window(self.policy.read_bytes())[0], [2, 1])

    @unittest.skipUnless(shutil.which("php"), "PHP unavailable")
    def test_stale_thousand_unit_prefix_cannot_starve_later_fresh_input(self):
        all_rows = {}
        for number in [100, 101, *range(200, 1200)]:
            row = copy.deepcopy(self.rows[0])
            row["unit"]["id"] = number
            row["results"][0].update(hostid=1, server_state=4)
            all_rows[number] = row
            self.audit[f"{number:064x}"] = {"workunit_id": number}
        new = self.input(1300)
        all_rows[1300] = new
        (self.root / "qwen/qwen-nonenglish-v1/audit.json").write_text(json.dumps(self.audit))
        (self.project / "database.json").write_text(json.dumps([new]))
        seen = []
        def scan(project, ids, *hosts):
            seen.append(ids)
            return self.database | {"rows": [all_rows[i] for i in ids]}
        with patch.object(R, "read_database", side_effect=scan):
            self.assertEqual(R.run(self.args)["status"], "no-fresh-verified-pairs")
            receipt = R.run(self.args)
        self.assertEqual(len(seen[0]), 1000)
        self.assertIn(1300, seen[1])
        self.assertEqual(receipt["target"], [1300, 1300])

    @unittest.skipUnless(shutil.which("php"), "PHP unavailable")
    def test_standalone_fresh_pair_helper_also_requires_distinct_host_gate(self):
        self.config.write_text("<boinc><config/></boinc>")
        before = self.policy.read_bytes()
        proc = subprocess.run(
            ["php", str(HELPER), "--project", str(self.project), "--plan-class", "shared_8b",
             "--fresh-pair", "--workunit", "100", "--workunit-end", "101", "--apply"],
            capture_output=True, text=True)
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn("requires one_result_per_host", proc.stderr)
        self.assertEqual(self.policy.read_bytes(), before)


if __name__ == "__main__":
    unittest.main()
