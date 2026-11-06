import contextlib
from copy import deepcopy
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import io
import json
import os
from pathlib import Path
import shutil
import signal
import socket
import socketserver
import subprocess
import sys
import threading
import time
import unittest
from unittest.mock import patch
import uuid

sys.path.insert(0, str(Path(__file__).parent))
import feed_collect as C
import feed_status as F

ROOT = Path(__file__).resolve().parents[2]
NOW = 1_800_000_000


@contextlib.contextmanager
def fixture():
    root = ROOT / (".feed-fixture-" + uuid.uuid4().hex)
    root.mkdir()
    try:
        yield root
    finally:
        shutil.rmtree(root)


@contextlib.contextmanager
def server_fixture():
    if not shutil.which("php"):
        raise unittest.SkipTest("PHP unavailable")
    with fixture() as root:
        (root / "html/inc").mkdir(parents=True)
        (root / "config.xml").write_text(
            "<boinc><config><db_passwd>fixture-only-secret</db_passwd>"
            "<one_result_per_host_per_wu/></config></boinc>")
        (root / "plan_class_spec.xml").write_text(
            "<plan_classes><plan_class><name>intel</name><min_wu_id>2</min_wu_id>"
            "<max_wu_id>10</max_wu_id></plan_class></plan_classes>")
        (root / "window.env").write_text("ENABLED_UNTIL=1800086400\nMAX_ADMISSIONS=3\n")
        (root / "html/inc/boinc_db.inc").write_text("""<?php
class Result {
    private $rows;
    function __construct($rows) { $this->rows=$rows; }
    function fetch_assoc() { return array_shift($this->rows); }
}
class BoincDb {
    static function get($read) { if (!$read) throw new Exception(); return new BoincDb(); }
    function do_query($sql) {
        if (preg_match('/^(SET SESSION TRANSACTION READ ONLY|START TRANSACTION WITH CONSISTENT SNAPSHOT|ROLLBACK)$/',$sql)) return true;
        if (!str_starts_with($sql,'SELECT /*+ MAX_EXECUTION_TIME(2000) */')) throw new Exception('not a bounded read');
        if (str_contains($sql,'COALESCE(MAX')) return new Result([
            ['name'=>'usnm_ocr','id'=>'1','min_version'=>'0','last_workunit'=>'1799999990'],
            ['name'=>'usnm_qwen','id'=>'6','min_version'=>'117','last_workunit'=>'1799999995']]);
        return new Result([]);
    }
}
""")
        yield root


def version(app="usnm_qwen", plan="intel", gpu="intel_gpu"):
    return {"app": app, "version_num": 117, "plan_class": plan, "platform": "linux",
            "cpus": 1, "gpu": gpu, "gpu_count": 1 if gpu else 0}


def host(identity=9, count=1):
    return {"host_id": identity, "versions": [version(), version("usnm_ocr", "docker", None)],
            "cpus": 1, "idle_cpu": 1, "idle_gpu": {"intel_gpu": count}, "memory_bytes": 1000, "disk_bytes": 1000}


def database():
    return {"minimum_versions": {"usnm_ocr": 0, "usnm_qwen": 117},
            "host_preferences": {str(h): {"apps": list(C.APP_NAMES), "blocked": []} for h in (9, 10)},
            "versions": [version(), version("usnm_ocr", "docker", None)],
            "plans": {"intel": {"min_wu_id": "2", "max_wu_id": "10", "min_batch": "8", "max_batch": "8",
                                "avg_ncpus": "1", "gpu_type": "intel"},
                      "docker": {}},
            "units": [{"id": 5, "app": "usnm_qwen", "batch": 8, "app_version_num": 117,
                       "hr_class": 0, "assignments": 0, "hosts": [], "unsent": 2,
                       "rsc_memory_bound": 10, "rsc_disk_bound": 10}]}


@contextlib.contextmanager
def client_rpc_fixture():
    operations = []
    answers = {
        "auth1": "<nonce>nonce</nonce>",
        "auth2": "<authorized/>",
        "get_state": """<client_state><platform_name>linux</platform_name>
<project><master_url>https://boinc.usnewsmap.com/</master_url><hostid>9</hostid></project>
<host_info><p_ncpus>4</p_ncpus><m_nbytes>1000</m_nbytes></host_info>
<coprocs><coproc_intel_gpu><count>1</count></coproc_intel_gpu></coprocs>
<app_version><app_name>usnm_ocr</app_name><version_num>117</version_num><plan_class>docker</plan_class><avg_ncpus>1</avg_ncpus></app_version>
</client_state>""",
        "get_cc_status": "<cc_status><task_suspend_reason>0</task_suspend_reason><gpu_suspend_reason>0</gpu_suspend_reason></cc_status>",
        "get_global_prefs_working": "<global_preferences><max_ncpus_pct>50</max_ncpus_pct></global_preferences>",
        "get_disk_usage": "<disk_usage_summary><project><disk_usage>600</disk_usage></project><d_free>900</d_free><d_boinc>100</d_boinc><d_allowed>1000</d_allowed></disk_usage_summary>",
    }

    class Handler(socketserver.BaseRequestHandler):
        def handle(self):
            buffer = b""
            while True:
                chunk = self.request.recv(65536)
                if not chunk:
                    return
                buffer += chunk
                if b"\x03" not in buffer:
                    continue
                message, buffer = buffer.split(b"\x03", 1)
                op = C.xml(message)[0].tag
                operations.append(op)
                self.request.sendall(("<boinc_gui_rpc_reply>" + answers[op] + "</boinc_gui_rpc_reply>").encode() + b"\x03")

    with fixture() as root, socketserver.TCPServer(("127.0.0.1", 0), Handler) as rpc:
        (root / "gui_rpc_auth.cfg").write_text("secret")
        thread = threading.Thread(target=rpc.serve_forever)
        thread.start()
        try:
            yield root, rpc.server_address[1], answers, operations
        finally:
            rpc.shutdown()
            thread.join()


def queued_task(resources="", result_fields=""):
    return f"""<workunit><name>queued</name><app_name>usnm_ocr</app_name>
<rsc_memory_bound>100</rsc_memory_bound>{resources}</workunit>
<result><wu_name>queued</wu_name><version_num>117</version_num>
<plan_class>docker</plan_class><state>1</state>{result_fields}</result>"""


class EligibilityTests(unittest.TestCase):
    def test_cpu_only_live_policy_cannot_inherit_a_cached_gpu_resource_type(self):
        with server_fixture() as root:
            for selector in ("", "<gpu_type/>", "<gpu_type>nvidia</gpu_type>", "<gpu_type>intel</gpu_type>"):
                (root / "plan_class_spec.xml").write_text(
                    "<plan_classes><plan_class><name>intel</name><avg_ncpus>1</avg_ncpus>"
                    f"<ngpus>1</ngpus>{selector}</plan_class></plan_classes>")
                live = C.server(root, root / "window.env")["plans"]["intel"]
                for empty in (True, False):
                    with self.subTest(selector=selector, empty=empty):
                        db = database()
                        db["plans"]["intel"] = live
                        if empty:
                            db["units"] = []
                        if selector == "<gpu_type>intel</gpu_type>":
                            self.assertEqual(C.supply(db, [host()], "gpu"),
                                             {"idle": 1, "eligible": 0 if empty else 1})
                        else:
                            reason = "eligibility unavailable" if "nvidia" in selector else "live_resource_type_mismatch"
                            with self.assertRaisesRegex(ValueError, reason):
                                C.supply(db, [host()], "gpu")

    def test_opencl_requires_rpc_support_and_compatible_memory_evidence(self):
        with client_rpc_fixture() as (root, port, answers, _):
            original = answers["get_state"]
            for flag, prop, minimum, expected in (
                ("0", False, "0", "opencl_support_unavailable"),
                (None, True, "0", "opencl_support_unavailable"),
                ("1", False, "0", "opencl_memory_unavailable"),
                ("1", True, "0", None),
                ("1", True, "3000", "eligibility unavailable"),
                ("1", True, "1000", "opencl_memory_profile_unavailable"),
            ):
                with self.subTest(flag=flag, properties=prop, minimum=minimum):
                    support = f"<have_opencl>{flag}</have_opencl>" if flag is not None else ""
                    properties = """<coproc_opencl><global_mem_size>2000000000</global_mem_size>
<opencl_device_version>OpenCL 2.0 fixture</opencl_device_version></coproc_opencl>""" if prop else ""
                    answers["get_state"] = original.replace("<count>1</count>",
                        f"<count>1</count><available_ram>8000000000</available_ram>{support}{properties}")
                    observed = C.client(root, port)
                    db, h = database(), host()
                    h.update({key: observed[key] for key in ("gpu_memory", "gpu_capabilities", "gpu_devices")})
                    db["plans"]["intel"].update(opencl="1", min_gpu_ram_mb=minimum)
                    if expected:
                        with self.assertRaisesRegex(ValueError, expected):
                            C.supply(db, [h], "gpu")
                    else:
                        self.assertEqual(C.supply(db, [h], "gpu"), {"idle": 1, "eligible": 1})
                        db["plans"]["intel"]["min_opencl_version"] = "201"
                        with self.assertRaisesRegex(ValueError, "eligibility unavailable"):
                            C.supply(db, [h], "gpu")
                        db["plans"]["intel"]["min_opencl_version"] = "200"
                        self.assertEqual(C.supply(db, [h], "gpu"), {"idle": 1, "eligible": 1})
            answers["get_state"] = answers["get_state"].replace(
                "<available_ram>8000000000</available_ram>", "<available_ram>2000000000</available_ram>")
            observed = C.client(root, port)
            h.update({key: observed[key] for key in ("gpu_memory", "gpu_capabilities", "gpu_devices")})
            self.assertEqual(C.supply(db, [h], "gpu"), {"idle": 1, "eligible": 1})

    def test_every_idle_gpu_type_needs_an_installed_and_live_profile(self):
        db, h = database(), host()
        db["units"].append(dict(db["units"][0], id=6))
        h["idle_gpu"]["NVIDIA"] = 1
        with self.assertRaisesRegex(ValueError, "eligibility unavailable"):
            C.supply(db, [h], "gpu")
        db["host_preferences"]["9"]["blocked"] = ["NVIDIA"]
        self.assertEqual(C.supply(db, [h], "gpu"), {"idle": 1, "eligible": 1})
        db["host_preferences"]["9"]["blocked"] = []
        nvidia = version(plan="nvidia", gpu="NVIDIA")
        h["versions"].append(nvidia)
        db["plans"]["nvidia"] = {"gpu_type": "nvidia", "avg_ncpus": "1"}
        with self.assertRaisesRegex(ValueError, "eligibility unavailable"):
            C.supply(db, [h], "gpu")
        db["versions"].append(dict(nvidia, platform="incompatible"))
        with self.assertRaisesRegex(ValueError, "eligibility unavailable"):
            C.supply(db, [h], "gpu")
        db["versions"][-1]["platform"] = "linux"
        h.update(cpus=2, idle_cpu=2)
        self.assertEqual(C.supply(db, [h], "gpu"), {"idle": 2, "eligible": 2})

    def test_cpu_demand_must_match_live_fixed_and_host_specific_multicore_policy(self):
        db, h = database(), host()
        db["units"][0]["app"] = "usnm_ocr"
        h.update(cpus=2, idle_cpu=2)
        h["versions"][1]["cpus"] = 2
        db["plans"]["docker"]["avg_ncpus"] = "1"
        with self.assertRaisesRegex(ValueError, "live CPU demand"):
            C.supply(db, [h], "cpu")
        h["versions"][1]["cpus"] = 1
        self.assertEqual(C.supply(db, [h], "cpu"), {"idle": 2, "eligible": 1})
        db["plans"]["docker"] = {"max_threads": "4"}
        h.update(cpus=6, idle_cpu=6)
        with self.assertRaisesRegex(ValueError, "live CPU demand"):
            C.supply(db, [h], "cpu")
        h["versions"][1]["cpus"] = 4
        self.assertEqual(C.supply(db, [h], "cpu"), {"idle": 1, "eligible": 1})
        h.update(cpus=2, idle_cpu=2)
        with self.assertRaisesRegex(ValueError, "live CPU demand"):
            C.supply(db, [h], "cpu")
        h["versions"][1]["cpus"] = 2
        self.assertEqual(C.supply(db, [h], "cpu"), {"idle": 1, "eligible": 1})
        del h["cpus"]
        with self.assertRaisesRegex(ValueError, "CPU capacity unavailable"):
            C.supply(db, [h], "cpu")

    def test_policy_validation_cannot_be_bypassed_by_empty_or_rejected_stock(self):
        for resource, plan_name in (("cpu", "docker"), ("gpu", "intel")):
            for key, value in (("project_prefs_tag", "opt_in"), ("unmodeled_gate", "1"),
                               ("max_threads", "invalid"), ("mem_usage_per_cpu_mb", "10")):
                for empty in (True, False):
                    with self.subTest(resource=resource, policy=key, empty=empty):
                        db = database()
                        db["plans"][plan_name][key] = value
                        db["units"] = [] if empty else [dict(db["units"][0], hosts=[9])]
                        with self.assertRaises(ValueError):
                            C.supply(db, [host()], resource)

    def test_mixed_gpu_profiles_cannot_fill_fractional_slots_with_full_gpu_work(self):
        db, h = database(), host()
        db["units"].append(dict(db["units"][0], id=6))
        half = dict(version(plan="half"), gpu_count=.5)
        h["versions"].append(half)
        db["versions"].append(half)
        db["plans"]["half"] = {"min_batch": "2", "max_batch": "2", "ngpus": ".5",
                               "avg_ncpus": "1", "gpu_type": "intel"}
        with self.assertRaisesRegex(ValueError, "mixed GPU resource profiles"):
            C.supply(db, [h], "gpu")
        half["gpu_count"] = 1
        db["plans"]["half"]["ngpus"] = "1"
        self.assertEqual(C.supply(db, [h], "gpu"), {"idle": 1, "eligible": 1})
        half.update(gpu="NVIDIA", gpu_count=.5)
        db["plans"]["half"]["ngpus"] = ".5"
        db["plans"]["half"]["gpu_type"] = "nvidia"
        h["idle_gpu"]["NVIDIA"] = 1
        h.update(cpus=3, idle_cpu=3)
        self.assertEqual(C.supply(db, [h], "gpu"), {"idle": 3, "eligible": 1})

    def test_live_gpu_profile_must_match_installed_usage_even_without_stock(self):
        for field, value in (("ngpus", "2"), ("ngpus", "-1"), ("ngpus", "nan"),
                             ("avg_ncpus", "2"), ("gpu_utilization_tag", "fraction")):
            for empty in (False, True):
                db = database()
                db["plans"]["intel"][field] = value
                if empty:
                    db["units"] = []
                with self.subTest(field=field, value=value, empty=empty), self.assertRaises(ValueError):
                    C.supply(db, [host()], "gpu")
        db, h = database(), host(count=2)
        db["plans"]["intel"]["ngpus"] = "2"
        h["versions"][0]["gpu_count"] = 2
        self.assertEqual(C.supply(db, [h], "gpu"), {"idle": 1, "eligible": 1})
        db["plans"]["intel"]["ngpus"] = "0"
        h["versions"][0]["gpu_count"] = 1
        h.update(cpus=2, idle_cpu=2)
        self.assertEqual(C.supply(db, [h], "gpu"), {"idle": 2, "eligible": 1})

    def test_cumulative_disk_budget_is_checked_per_host_not_per_workunit(self):
        db, h = database(), host()
        h.update(cpus=2, idle_cpu=2)
        db["units"] = [dict(db["units"][0], id=i, app="usnm_ocr", rsc_disk_bound=600) for i in (5, 6)]
        self.assertEqual(C.supply(db, [h], "cpu"), {"idle": 1, "eligible": 1})
        db["units"][1]["rsc_disk_bound"] = 500
        with self.assertRaisesRegex(ValueError, "cumulative host disk budget"):
            C.supply(db, [h], "cpu")
        db["units"][1]["rsc_disk_bound"] = 600
        h["disk_bytes"] = 1200
        self.assertEqual(C.supply(db, [h], "cpu"), {"idle": 2, "eligible": 2})
        self.assertEqual(C.supply(db, [host(9), host(10)], "cpu"), {"idle": 2, "eligible": 2})

    def test_gpu_cpu_demand_caps_uniform_allocation_and_rejects_mixed_overcommit(self):
        db, h = database(), host(count=2)
        db["units"].append(dict(db["units"][0], id=6))
        self.assertEqual(C.supply(db, [h], "gpu"), {"idle": 1, "eligible": 1})
        h.update(cpus=2, idle_cpu=2)
        self.assertEqual(C.supply(db, [h], "gpu"), {"idle": 2, "eligible": 2})
        nvidia = dict(version(plan="nvidia", gpu="NVIDIA"), cpus=2)
        h["versions"].append(nvidia)
        h["idle_gpu"] = {"intel_gpu": 1, "NVIDIA": 1}
        db["versions"].append(nvidia)
        db["plans"]["nvidia"] = {"gpu_type": "nvidia", "avg_ncpus": "2"}
        with self.assertRaisesRegex(C.SupplyUnknown, "mixed_resource_packing"):
            C.supply(db, [h], "gpu")
        h.update(cpus=3, idle_cpu=3)
        self.assertEqual(C.supply(db, [h], "gpu"), {"idle": 2, "eligible": 2})
        # A CPU-app exclusion must not remove CPU support for permitted GPU work.
        h.update(idle_cpu=0, available_cpu=3)
        self.assertEqual(C.supply(db, [h], "gpu"), {"idle": 2, "eligible": 2})
        self.assertEqual(C.supply(db, [h], "cpu"), {"idle": 0, "eligible": 0})

        db, h = database(), host(count=2)
        h.update(cpus=4, idle_cpu=4)
        alternate = dict(version(plan="alternate"), cpus=2)
        db["plans"]["alternate"] = {"avg_ncpus": "2", "gpu_type": "intel"}
        db["versions"].append(alternate)
        h["versions"].append(alternate)
        db["units"].append(dict(db["units"][0], id=6))
        with self.assertRaisesRegex(C.SupplyUnknown, "ambiguous_resource_profile"):
            C.supply(db, [h], "gpu")

    def test_cumulative_ram_caps_uniform_allocation_and_rejects_mixed_packing(self):
        db, h = database(), host()
        h.update(cpus=2, idle_cpu=2)
        db["units"] = [dict(db["units"][0], id=i, app="usnm_ocr", rsc_memory_bound=600) for i in (5, 6)]
        self.assertEqual(C.supply(db, [h], "cpu"), {"idle": 1, "eligible": 1})
        db["units"][1]["rsc_memory_bound"] = 500
        with self.assertRaisesRegex(C.SupplyUnknown, "mixed_resource_packing"):
            C.supply(db, [h], "cpu")
        h["memory_bytes"] = 1100
        self.assertEqual(C.supply(db, [h], "cpu"), {"idle": 2, "eligible": 2})

    def test_fractional_gpu_memory_requires_provable_single_device_packing(self):
        db, h = database(), host()
        h.update(cpus=2, idle_cpu=2, gpu_devices={"intel_gpu": 1}, gpu_memory={"intel_gpu": 1000})
        h["versions"][0]["gpu_count"] = .5
        db["plans"]["intel"].update(ngpus=".5", gpu_ram_used_mb=".0006")
        db["units"].append(dict(db["units"][0], id=6))
        self.assertEqual(C.supply(db, [h], "gpu"), {"idle": 1, "eligible": 1})
        h["gpu_memory"]["intel_gpu"] = 1200
        self.assertEqual(C.supply(db, [h], "gpu"), {"idle": 2, "eligible": 2})
        for evidence in ({"gpu_devices": {"intel_gpu": 2}},
                         {"used_gpu": {"intel_gpu": .5}},
                         {"gpu_memory": {}}, {"gpu_devices": {}}):
            with self.subTest(evidence=evidence), self.assertRaisesRegex(C.SupplyUnknown, "gpu_memory_packing"):
                C.supply(db, [h | evidence], "gpu")

    def test_minimum_gpu_ram_is_the_task_reservation_unless_explicitly_overridden(self):
        db, h = database(), host()
        h.update(cpus=2, idle_cpu=2, gpu_devices={"intel_gpu": 1}, gpu_memory={"intel_gpu": 8e9})
        h["versions"][0]["gpu_count"] = .5
        db["plans"]["intel"].update(ngpus=".5", min_gpu_ram_mb="6000")
        db["units"].append(dict(db["units"][0], id=6))
        for explicit in (None, "0"):
            if explicit is not None:
                db["plans"]["intel"]["gpu_ram_used_mb"] = explicit
            with self.subTest(explicit=explicit):
                self.assertEqual(C.supply(db, [h], "gpu"), {"idle": 1, "eligible": 1})
                for evidence in ({"gpu_devices": {"intel_gpu": 2}},
                                 {"used_gpu": {"intel_gpu": .5}}):
                    with self.assertRaisesRegex(C.SupplyUnknown, "gpu_memory_packing"):
                        C.supply(db, [h | evidence], "gpu")
        db["plans"]["intel"]["gpu_ram_used_mb"] = "1000"
        self.assertEqual(C.supply(db, [h], "gpu"), {"idle": 2, "eligible": 2})

    def test_gpu_shortage_uses_resource_eligible_capacity_with_empty_or_plentiful_stock(self):
        db, h = database(), host()
        stock = [dict(db["units"][0], id=i) for i in range(2, 10)]
        h["idle_cpu"] = 0
        for units in ([], stock):
            db["units"] = units
            with self.subTest(stock=len(units)):
                self.assertEqual(C.supply(db, [h], "gpu"), {"idle": 0, "eligible": 0})
        h["idle_cpu"] = 1
        self.assertEqual(C.supply(db, [h], "gpu"), {"idle": 1, "eligible": 1})
        db["units"] = []
        supply = C.supply(db, [h], "gpu")
        self.assertEqual(supply, {"idle": 1, "eligible": 0})
        previous = {}
        for elapsed in range(0, 1801, 60):
            document, previous = F.evaluate({"supply": {"gpu": supply}}, previous,
                                            NOW + elapsed, 1000 + elapsed, "boot", "cfg")
        self.assertEqual(next(c for c in document["checks"] if c["code"] == "gpu_shortage")["state"], "firing")

    def test_candidate_budgets_stop_construction_before_scanning_remaining_stock(self):
        db = database()
        db["plans"]["intel"]["max_wu_id"] = "0"
        db["units"] = [dict(db["units"][0], id=i) for i in range(2, 102)]
        with patch.object(C, "MAX_MATCH_EDGES", 2), patch.object(C, "eligible", wraps=C.eligible) as eligible:
            with self.assertRaisesRegex(C.SupplyUnknown, "candidate_edge_budget"):
                C.supply(db, [host()], "gpu")
            self.assertEqual(eligible.call_count, 3)
        for unit in db["units"]:
            unit["hosts"] = [9]
        with patch.object(C, "MAX_PROFILE_CHECKS", 2), patch.object(C, "eligible", wraps=C.eligible) as eligible:
            with self.assertRaisesRegex(C.SupplyUnknown, "candidate_scan_budget"):
                C.supply(db, [host()], "gpu")
            self.assertEqual(eligible.call_count, 2)
        db["units"] = []
        with patch.object(C, "MAX_MATCH_EDGES", 2), self.assertRaisesRegex(C.SupplyUnknown, "candidate_slot_budget"):
            C.supply(db, [host(count=3)], "gpu")

    def test_shared_results_and_one_replica_per_host_are_not_double_counted(self):
        db = database()
        h = host(9, 2) | {"cpus": 2, "idle_cpu": 2}
        self.assertEqual(C.supply(db, [h], "gpu"), {"idle": 2, "eligible": 1})
        self.assertEqual(C.supply(db, [host(9), host(10)], "gpu"), {"idle": 2, "eligible": 2})
        db["units"][0]["unsent"] = 1
        self.assertEqual(C.supply(db, [host(9), host(10)], "gpu")["eligible"], 1)

    def test_policy_host_batch_memory_platform_and_version_constraints(self):
        for field, value in [("id", 11), ("batch", 2), ("hosts", [9]),
                             ("app_version_num", 118), ("app_version_num", 116),
                             ("assignments", 1), ("hr_class", 2),
                             ("rsc_memory_bound", 1001), ("rsc_disk_bound", 1001)]:
            db = database()
            db["units"][0][field] = value
            with self.subTest(field=field):
                self.assertEqual(C.supply(db, [host()], "gpu")["eligible"], 0)
        db = database()
        db["plans"]["intel"]["min_wu_id"] = "2"
        db["plans"]["intel"]["max_wu_id"] = "0"
        self.assertEqual(C.supply(db, [host()], "gpu")["eligible"], 1)
        db["plans"]["intel"]["max_wu_id"] = "1"
        self.assertEqual(C.supply(db, [host()], "gpu")["eligible"], 0)
        db["plans"]["intel"]["project_prefs_tag"] = "opt_in"
        with self.assertRaises(ValueError):
            C.supply(db, [host()], "gpu")

    def test_no_gpu_is_zero_capacity_not_unknown_and_no_versions_is_unknown(self):
        h = host()
        h["idle_gpu"] = {}
        self.assertEqual(C.supply(database(), [h], "gpu"), {"idle": 0, "eligible": 0})
        h = host()
        h["versions"] = []
        with self.assertRaises(ValueError):
            C.supply(database(), [h], "gpu")

    def test_account_app_selection_and_venue_are_authoritative(self):
        apps = [{"id": 1, "name": "usnm_ocr"}, {"id": 6, "name": "usnm_qwen"}]
        rows = [{"id": 9, "venue": "home", "project_prefs": """<project_preferences>
<apps_selected><app_id>1</app_id></apps_selected>
<venue name="home"><apps_selected><app_id>6</app_id></apps_selected><no_cuda>1</no_cuda></venue>
</project_preferences>"""}]
        preferences = C.host_preferences(rows, apps)
        self.assertEqual(preferences["9"], {"apps": ["usnm_qwen"], "blocked": ["NVIDIA"], "fallback": False})
        db = database()
        db["host_preferences"] = preferences
        self.assertEqual(C.supply(db, [host()], "cpu"), {"idle": 0, "eligible": 0})

    def test_stock_nested_project_specific_preferences_override_the_default_venue(self):
        apps = [{"id": 1, "name": "usnm_ocr"}, {"id": 6, "name": "usnm_qwen"}]
        raw = """<project_preferences>
<project_specific><apps_selected><app_id>1</app_id></apps_selected></project_specific>
<venue name="home"><no_cuda>1</no_cuda><project_specific>
<apps_selected><app_id>6</app_id></apps_selected>
<allow_non_preferred_apps>0</allow_non_preferred_apps>
</project_specific></venue></project_preferences>"""
        row = {"id": 9, "venue": "home", "project_prefs": raw}
        prefs = C.host_preferences([row], apps)
        self.assertEqual(prefs["9"], {"apps": ["usnm_qwen"], "blocked": ["NVIDIA"], "fallback": False})
        db = database() | {"host_preferences": prefs}
        self.assertEqual(C.supply(db, [host()], "cpu"), {"idle": 0, "eligible": 0})
        self.assertEqual(C.supply(db, [host()], "gpu"), {"idle": 1, "eligible": 1})
        row["venue"] = ""
        self.assertEqual(C.host_preferences([row], apps)["9"]["apps"], ["usnm_ocr"])
        row.update(venue="home", project_prefs=raw.replace(
            "<allow_non_preferred_apps>0", "<allow_non_preferred_apps>1"))
        db["host_preferences"] = C.host_preferences([row], apps)
        self.assertTrue(db["host_preferences"]["9"]["fallback"])
        with self.assertRaisesRegex(ValueError, "non-preferred fallback"):
            C.supply(db, [host()], "gpu")
        row["project_prefs"] = raw.replace("<venue name=\"home\">", '<venue name="home"><apps_selected/>')
        with self.assertRaisesRegex(ValueError, "ambiguous project-specific"):
            C.host_preferences([row], apps)


class ControllerTests(unittest.TestCase):
    def observe(self, events=(), native=(), **status):
        properties = {"LoadState": "loaded", "SubState": "dead", "Result": "success",
                      "ExecMainStartTimestampMonotonic": 100000000,
                      "ExecMainExitTimestampMonotonic": 120000000}
        properties.update(status)
        entries = [{"__REALTIME_TIMESTAMP": str(at * 1000000), **entry} for at, entry in native]
        entries.extend({
            "__REALTIME_TIMESTAMP": str(at * 1000000), "MESSAGE": json.dumps(event),
        } for at, event in events)
        journal = "\n".join(json.dumps(entry) for entry in
                            sorted(entries, key=lambda e: int(e["__REALTIME_TIMESTAMP"]))).encode()
        systemd = "\n".join(f"{key}={value}" for key, value in properties.items()).encode()
        with patch.object(C, "run", side_effect=[journal, systemd]), \
                patch.object(C.time, "time", return_value=NOW):
            return C.controller("usnm-boinc-priority-controller.service")

    def test_native_failures_survive_successful_or_running_restart_and_acknowledgement(self):
        for message_id, field, result in (
            ("d9b373ed55a64feb8242e02dbe79a49c", "UNIT_RESULT", "exit-code"),
            ("be02cf6855d2428ba40df7e9d022f03d", "JOB_RESULT", "dependency"),
        ):
            for running in (False, True):
                with self.subTest(message_id=message_id, running=running):
                    entry = {"MESSAGE_ID": message_id, "UNIT": "usnm-boinc-priority-controller.service",
                             field: result, "MESSAGE": "Native failure without application JSON or timings."}
                    events = [(NOW - 5, {"event": "boinc_batch_tick", "timings": {"tick_seconds": 1}})]
                    status = {"SubState": "running", "ExecMainStartTimestampMonotonic": 1000000000,
                              "ExecMainExitTimestampMonotonic": 0} if running else {}
                    with patch.object(C.time, "clock_gettime", return_value=1001):
                        tick = self.observe(events, native=[(NOW - 10, entry)], **status)
                    self.assertEqual(tick["last_failure_at"], NOW - 10)
                    previous = {}
                    for elapsed, expected in ((0, "firing"), (60, "healthy")):
                        document, previous = F.evaluate({"tick": tick}, previous, NOW + elapsed,
                                                        1000 + elapsed, "boot", "cfg")
                        self.assertEqual(next(c for c in document["checks"]
                                              if c["code"] == "controller_tick")["state"], expected)
                        previous = json.loads(json.dumps(previous))

    def test_native_failure_identity_and_invalid_evidence(self):
        events = [(NOW - 5, {"event": "boinc_batch_tick", "timings": {"tick_seconds": 1}})]
        entry = {"MESSAGE_ID": "d9b373ed55a64feb8242e02dbe79a49c",
                 "UNIT": "unrelated.service", "UNIT_RESULT": "exit-code", "MESSAGE": "Failed."}
        self.assertEqual(self.observe(events, native=[(NOW - 10, entry)])["last_failure_at"], 0)
        entry["UNIT"] = "usnm-boinc-priority-controller.service"
        del entry["UNIT_RESULT"]
        with self.assertRaisesRegex(ValueError, "invalid native controller failure evidence"):
            self.observe(events, native=[(NOW - 10, entry)])
        entry["MESSAGE_ID"] = "0e4284a0caca4bfc81c0bb6786972673"
        entry["UNIT_RESULT"] = "exec-condition"
        self.assertEqual(self.observe(events, native=[(NOW - 10, entry)])["last_failure_at"], 0)

    def test_running_tick_uses_systemd_clock_despite_prior_suspend(self):
        for elapsed, expected in ((10, "healthy"), (900, "healthy"), (901, "firing")):
            with self.subTest(elapsed=elapsed), \
                    patch.object(C.time, "CLOCK_BOOTTIME", -1, create=True), \
                    patch.object(C.time, "clock_gettime", side_effect=lambda clock:
                                 1000 + elapsed + (7200 if clock != C.time.CLOCK_MONOTONIC else 0)):
                tick = self.observe(SubState="running", ExecMainStartTimestampMonotonic=1000000000,
                                    ExecMainExitTimestampMonotonic=0)
                self.assertEqual(tick["seconds"], elapsed)
                document, _ = F.evaluate({"tick": tick}, {}, NOW, 1000 + elapsed, "boot", "cfg")
                self.assertEqual(next(c for c in document["checks"]
                                      if c["code"] == "controller_tick")["state"], expected)

    def test_failure_without_timings_survives_systemd_fallback_and_recovery(self):
        events = [(NOW - 10, {"event": "boinc_batch_failure", "schema_version": 1,
                             "at": NOW - 10, "error": "fixture early failure"})]
        failed = self.observe(events, SubState="failed", Result="exit-code")
        self.assertEqual(failed, {"ok": False, "seconds": 20, "finished_at": NOW,
                                  "last_failure_at": NOW - 10})
        events.append((NOW - 5, {"event": "boinc_batch_tick", "schema_version": 1,
                                "at": NOW - 5, "timings": {"tick_seconds": 12}}))
        recovered = self.observe(events)
        self.assertEqual(recovered, {"ok": True, "seconds": 12, "finished_at": NOW - 5,
                                     "last_failure_at": NOW - 10})
        previous = {}
        for elapsed, expected in ((0, "firing"), (60, "healthy")):
            document, previous = F.evaluate({"tick": recovered}, previous, NOW + elapsed,
                                            1000 + elapsed, "boot", "cfg")
            self.assertEqual(next(c for c in document["checks"]
                                  if c["code"] == "controller_tick")["state"], expected)

    def test_completed_runtime_breach_survives_a_short_or_running_successor(self):
        for seconds in (900, 901):
            for running in (False, True):
                with self.subTest(seconds=seconds, running=running):
                    events = [(NOW - 10, {"event": "boinc_batch_tick", "schema_version": 1,
                                         "timings": {"tick_seconds": seconds}})]
                    status = {}
                    if running:
                        status = {"SubState": "running", "ExecMainStartTimestampMonotonic": 1000000000,
                                  "ExecMainExitTimestampMonotonic": 0}
                    else:
                        events.append((NOW - 5, {"event": "boinc_batch_tick", "schema_version": 1,
                                                "timings": {"tick_seconds": 1}}))
                    with patch.object(C.time, "clock_gettime", return_value=1001):
                        tick = self.observe(events, **status)
                    self.assertEqual(tick["last_failure_at"], NOW - 10 if seconds > 900 else 0)
                    previous = {}
                    for elapsed in (0, 60):
                        document, previous = F.evaluate({"tick": tick}, previous, NOW + elapsed,
                                                        1000 + elapsed, "boot", "cfg")
                        expected = "firing" if seconds > 900 and elapsed == 0 else "healthy"
                        self.assertEqual(next(c for c in document["checks"]
                                              if c["code"] == "controller_tick")["state"], expected)
                        previous = json.loads(json.dumps(previous))


class CollectionTests(unittest.TestCase):
    def documents(self):
        def doc(source, **fields):
            return {"schema_version": 1, "source": source, "observed_at": NOW, **fields}
        return {
            "/server": doc("server", **database(), enabled_until=NOW + 86400,
                           last_workunits={"usnm_ocr": NOW, "usnm_qwen": NOW}),
            "/controller": doc("controller", tick={"ok": True, "seconds": 20, "finished_at": NOW}),
            "/backlog": doc("backlog", backlogs={"usnm_ocr": 1, "usnm_qwen": 2}),
            "/own": doc("client", **host()),
            "/volunteer": doc("client", **host(5, 100)),
        }

    def test_actual_http_sources_stale_unavailable_and_own_inventory(self):
        docs = self.documents()
        requests = []
        truncated = {}
        class Handler(BaseHTTPRequestHandler):
            def do_GET(self):
                requests.append(self.path)
                self.send_response(200 if self.path in docs else 503)
                if self.path in truncated:
                    body = json.dumps(docs[self.path]).encode()
                    if truncated[self.path] == "chunked":
                        self.send_header("Transfer-Encoding", "chunked")
                        body = b"20\r\n{\"truncated\":true}"
                    else:
                        self.send_header("Content-Length", str(len(body) + 1))
                    self.end_headers()
                    self.wfile.write(body)
                    self.close_connection = True
                    return
                self.end_headers()
                self.wfile.write(json.dumps(docs.get(self.path, {})).encode())
            def log_message(self, *_):
                pass
        with ThreadingHTTPServer(("127.0.0.1", 0), Handler) as http:
            thread = threading.Thread(target=http.serve_forever)
            thread.start()
            try:
                root = f"http://127.0.0.1:{http.server_port}"
                config = {k: root + "/" + k for k in ("server", "controller", "backlog")}
                config["clients"] = [root + "/own"]
                with patch.object(C.time, "time", return_value=NOW):
                    facts = C.collect(config)
                    self.assertEqual(facts["supply"]["gpu"], {"idle": 1, "eligible": 1})
                    self.assertNotIn("/volunteer", requests)
                    for encoding in ("chunked", "length"):
                        with self.subTest(encoding=encoding):
                            truncated["/controller"] = encoding
                            broken = C.collect(config)
                            self.assertEqual(broken["errors"], ["controller"])
                            self.assertEqual(broken["sources"]["controller"]["reason"], "http_protocol_error")
                            self.assertEqual(broken["supply"]["gpu"], {"idle": 1, "eligible": 1})
                            document, _ = F.evaluate(broken, {}, NOW, 1000, "boot", "cfg")
                            self.assertEqual(next(c for c in document["checks"]
                                                  if c["code"] == "controller_tick")["state"], "unknown")
                    truncated.clear()
                    self.assertEqual(C.collect(config)["errors"], [])
                    docs["/own"]["observed_at"] = NOW - 181
                    self.assertNotIn("supply", C.collect(config))
                    docs["/own"]["observed_at"] = NOW + 1
                    self.assertIn("client_0", C.collect(config)["errors"])
                    del docs["/server"]
                    self.assertNotIn("enabled_until", C.collect(config))
            finally:
                http.shutdown()
                thread.join()

    def test_unconfigured_clients_and_backlog_are_unknown_never_healthy(self):
        docs = self.documents()
        with patch.object(C, "read_source", side_effect=lambda path: docs[path]) as read, \
                patch.object(C.time, "time", return_value=NOW):
            for config in ({"server": "/server", "controller": "/controller"},
                           {"server": "/server", "controller": "/controller", "backlog": None, "clients": []}):
                with self.subTest(config=config):
                    facts = C.collect(config)
                    self.assertEqual(facts["sources"]["backlog"],
                                     {"observed_at": None, "state": "unknown", "reason": "not_configured"})
                    self.assertCountEqual(facts["errors"], [
                        "backlog", "cpu_no_own_clients_configured", "gpu_no_own_clients_configured"])
                    self.assertNotIn("supply", facts)
                    document, _ = F.evaluate(facts, {}, NOW, 1000, "boot", "cfg")
                    checks = {c["code"]: c for c in document["checks"]}
                    for resource in ("cpu", "gpu"):
                        check = checks[resource + "_shortage"]
                        self.assertEqual((check["state"], check["reason"]), ("unknown", "no_own_clients_configured"))
                    for app in F.APP_NAMES:
                        self.assertEqual(checks[app + "_no_work"]["state"], "unknown")
                    self.assertEqual(checks["controller_tick"]["state"], "healthy")
                    self.assertFalse(document["healthy"])
            self.assertEqual({call.args[0] for call in read.call_args_list}, {"/server", "/controller"})

    def test_feeder_allocation_is_opt_in_and_invalid_reports_fail_the_server_source(self):
        docs = self.documents()
        config = {"server": "/server", "controller": "/controller"}
        guarded = config | {"feeder_allocation": True}
        with patch.object(C, "read_source", side_effect=lambda path: deepcopy(docs[path])), \
                patch.object(C.time, "time", return_value=NOW):
            self.assertNotIn("feeder_allocation", C.collect(config))
            self.assertEqual(C.collect(guarded)["feeder_allocation"]["reason"], "not_reported")
            report = {"status": "drift", "reason": "registry_changed", "app_ids": [3, 7]}
            docs["/server"]["feeder_allocation"] = report
            facts = C.collect(guarded)
            self.assertEqual(facts["feeder_allocation"], report)
            self.assertNotIn("feeder_allocation", C.collect(config))
            for bad in (report | {"status": "fine"}, report | {"reason": "a b"}, report | {"app_ids": [-1]},
                        report | {"app_ids": "3"}, report | {"extra": 1}, "drift"):
                with self.subTest(bad=bad):
                    docs["/server"]["feeder_allocation"] = bad
                    facts = C.collect(guarded)
                    self.assertIn("server", facts["errors"])
                    self.assertEqual(facts["feeder_allocation"]["reason"], "server_unavailable")
            for value in ("yes", 1, None):
                with self.subTest(value=value), self.assertRaises(ValueError):
                    C.collect(config | {"feeder_allocation": value})

    def test_client_bindings_must_match_the_reported_host(self):
        docs = self.documents()
        config = {name: "/" + name for name in ("server", "controller", "backlog")}
        with patch.object(C, "read_source", side_effect=lambda path: docs[path]), \
                patch.object(C.time, "time", return_value=NOW):
            config["clients"] = [{"location": "/own", "host_id": 9}]
            facts = C.collect(config)
            self.assertEqual(facts["errors"], [])
            self.assertEqual(facts["supply"]["gpu"], {"idle": 1, "eligible": 1})
            config["clients"] = [{"location": "/own", "host_id": 12}]
            facts = C.collect(config)
            self.assertEqual(facts["sources"]["client_0"],
                             {"observed_at": None, "state": "unknown", "reason": "host_binding_mismatch"})
            self.assertEqual(facts["errors"], ["client_0"])
            self.assertNotIn("supply", facts)
        for clients in ("/own", [{"location": "/own"}], [{"location": "/own", "host_id": 0}],
                        [{"location": "/own", "host_id": True}], [{"location": "", "host_id": 9}],
                        ["/own", {"location": "/own", "host_id": 9}],
                        [{"location": "/own", "host_id": 9}, {"location": "/cpu-own", "host_id": 9}],
                        [f"/own-{i}" for i in range(33)]):
            with self.subTest(clients=clients), self.assertRaises(ValueError):
                C.own_clients({"clients": clients})

    def test_disk_is_shared_between_cpu_and_gpu_allocations(self):
        docs = self.documents()
        docs["/own"].update(cpus=2, idle_cpu=2)
        docs["/server"]["units"][0]["rsc_disk_bound"] = 600
        docs["/server"]["units"].append(dict(docs["/server"]["units"][0], id=6, app="usnm_ocr"))
        config = {name: "/" + name for name in ("server", "controller", "backlog")}
        config["clients"] = ["/own"]
        with patch.object(C, "read_source", side_effect=lambda path: docs[path]), \
                patch.object(C.time, "time", return_value=NOW):
            facts = C.collect(config)
            self.assertEqual(facts["supply"], {})
            self.assertCountEqual(facts["errors"], ["cpu_shared_disk_budget", "gpu_shared_disk_budget"])
            docs["/own"]["disk_bytes"] = 1200
            facts = C.collect(config)
            self.assertEqual(facts["errors"], [])
            self.assertEqual(facts["supply"], {"cpu": {"idle": 1, "eligible": 1},
                                              "gpu": {"idle": 1, "eligible": 1}})

    def test_shared_cpu_and_ram_overlap_are_unknown_then_recover_without_hiding_shortage(self):
        docs = self.documents()
        docs["/server"]["units"].append(dict(docs["/server"]["units"][0], id=6, app="usnm_ocr"))
        config = {name: "/" + name for name in ("server", "controller", "backlog")}
        config["clients"] = ["/own"]
        with patch.object(C, "read_source", side_effect=lambda path: docs[path]), \
                patch.object(C.time, "time", return_value=NOW):
            for reason, updates in (("shared_cpu_budget", {}),
                                    ("shared_memory_budget", {"cpus": 2, "idle_cpu": 2, "memory_bytes": 15})):
                docs["/own"].update(updates)
                facts = C.collect(config)
                self.assertEqual(facts["supply"], {})
                document, _ = F.evaluate(facts, {}, NOW, 1000, "boot", "cfg")
                for resource in ("cpu", "gpu"):
                    check = next(c for c in document["checks"] if c["code"] == resource + "_shortage")
                    self.assertEqual((check["state"], check["reason"]), ("unknown", reason))
                    self.assertIsNone(check["eligible_results"])
            docs["/own"]["memory_bytes"] = 20
            facts = C.collect(config)
            self.assertEqual(facts["errors"], [])
            self.assertEqual(facts["supply"]["cpu"], {"idle": 1, "eligible": 1})
            cpu_stock = docs["/server"]["units"].pop()
            limited = C.collect(config)
            self.assertEqual(limited["supply_errors"]["cpu"], "shared_capacity_unproven")
            self.assertNotIn("cpu", limited["supply"])
            docs["/server"]["units"].append(cpu_stock)
            # A separate CPU host has genuine free capacity: its shortage must
            # still fire, without counting CPU support reserved by the GPU host.
            docs["/server"]["host_preferences"]["9"]["apps"] = ["usnm_qwen"]
            docs["/server"]["host_preferences"]["10"]["apps"] = ["usnm_ocr"]
            docs["/cpu-own"] = docs["/own"] | host(10) | {"cpus": 2, "idle_cpu": 2, "idle_gpu": {}}
            config["clients"].append("/cpu-own")
            facts = C.collect(config)
            previous = {}
            for elapsed in range(0, 1801, 60):
                document, previous = F.evaluate(facts, previous, NOW + elapsed, 1000 + elapsed, "boot", "cfg")
            checks = {c["code"]: c for c in document["checks"]}
            self.assertEqual(checks["cpu_shortage"]["state"], "firing")
            self.assertEqual(checks["gpu_shortage"]["state"], "healthy")

    def test_missing_gpu_profile_is_unknown_without_poisoning_cpu_evidence(self):
        docs = self.documents()
        docs["/own"]["idle_gpu"]["NVIDIA"] = 1
        config = {name: "/" + name for name in ("server", "controller", "backlog")}
        config["clients"] = ["/own"]
        with patch.object(C, "read_source", side_effect=lambda path: docs[path]), \
                patch.object(C.time, "time", return_value=NOW):
            facts = C.collect(config)
            self.assertEqual(facts["errors"], ["gpu_eligibility"])
            self.assertIn("cpu", facts["supply"])
            self.assertNotIn("gpu", facts["supply"])
            docs["/server"]["host_preferences"]["9"]["blocked"] = ["NVIDIA"]
            recovered = C.collect(config)
            self.assertEqual(recovered["errors"], [])
            self.assertEqual(recovered["supply"]["gpu"], {"idle": 1, "eligible": 1})

    def test_late_source_forces_shared_completion_freshness(self):
        for delay, expected in ((5, "fresh"), (15, "unknown")):
            with self.subTest(delay=delay):
                docs = self.documents()
                docs["/server"]["observed_at"] = NOW - 175
                config = {name: "/" + name for name in ("server", "controller", "backlog")}
                config["clients"] = ["/own"]
                clock = [NOW]
                early_validation = threading.Event()
                real_fresh = C.fresh

                def read(path):
                    if path == "/own":
                        early_validation.wait(.1)
                        clock[0] = NOW + delay
                    return docs[path]

                def validate(document, now, kind):
                    if kind == "server":
                        early_validation.set()
                    return real_fresh(document, now, kind)

                with patch.object(C, "read_source", side_effect=read), \
                        patch.object(C, "fresh", side_effect=validate), \
                        patch.object(C.time, "time", side_effect=lambda: clock[0]):
                    facts = C.collect(config)
                self.assertEqual(facts["observed_at"], NOW + delay)
                self.assertEqual(facts["sources"]["server"]["state"], expected)
                self.assertEqual("enabled_until" in facts, expected == "fresh")
                self.assertEqual(facts["sources"]["controller"]["state"], "fresh")

    def test_slow_allocation_cannot_renew_expired_source_evidence(self):
        docs = self.documents()
        docs["/server"]["observed_at"] = NOW - 175
        config = {name: "/" + name for name in ("server", "controller", "backlog")}
        config["clients"] = ["/own"]
        clock = [NOW]
        real_supply = C.supply

        def allocate(*args):
            result = real_supply(*args)
            clock[0] = NOW + 15
            return result

        with patch.object(C, "read_source", side_effect=lambda path: docs[path]), \
                patch.object(C, "supply", side_effect=allocate), \
                patch.object(C.time, "time", side_effect=lambda: clock[0]):
            facts = C.collect(config)
        self.assertEqual(facts["observed_at"], NOW + 15)
        self.assertEqual(facts["sources"]["server"]["reason"], "stale_at_completion")
        self.assertNotIn("supply", facts)
        self.assertNotIn("enabled_until", facts)
        self.assertTrue(facts["tick"]["ok"])

    def test_monitor_does_not_replace_completion_time_with_later_publication_time(self):
        with fixture() as root:
            config = root / "config.json"
            config.write_text("{}")
            facts = {"observed_at": NOW, "enabled_until": NOW + 21601}
            with patch.object(C, "collect", return_value=facts), \
                    patch.object(F.time, "time", return_value=NOW + 1000), \
                    contextlib.redirect_stdout(io.StringIO()):
                document = F.monitor(config, root / "state")
            self.assertEqual(document["observed_at"], NOW)
            self.assertEqual(next(c for c in document["checks"]
                                  if c["code"] == "qwen_refill_window")["state"], "healthy")

    def test_wrong_json_shapes_are_source_local_and_recover(self):
        with fixture() as root:
            docs = self.documents()
            paths = {name: root / (name[1:] + ".json") for name in docs}
            for name, value in docs.items():
                paths[name].write_text(json.dumps(value))
            config = {name: str(paths["/" + name]) for name in ("server", "controller", "backlog")}
            config["clients"] = [str(paths["/own"])]
            cases = [(name, value) for name in ("/server", "/controller", "/backlog", "/own")
                     for value in (None, [])]
            cases += [
                ("/server", dict(docs["/server"], host_preferences=[])),
                ("/server", dict(docs["/server"], plans={"intel": []})),
                ("/controller", dict(docs["/controller"], tick=[])),
                ("/own", dict(docs["/own"], gpu_capabilities={"intel_gpu": None})),
            ]
            for name, value in cases:
                with self.subTest(source=name, value=value), patch.object(C.time, "time", return_value=NOW):
                    paths[name].write_text(json.dumps(value))
                    facts = C.collect(config)
                    source = "client_0" if name == "/own" else name[1:]
                    self.assertEqual(facts["errors"], [source])
                    self.assertEqual(facts["sources"][source]["state"], "unknown")
                    document, _ = F.evaluate(facts, {}, NOW, 1000, "boot", "cfg")
                    checks = {c["code"]: c["state"] for c in document["checks"]}
                    if name != "/server":
                        self.assertEqual(checks["qwen_refill_window"], "healthy")
                    if name != "/controller":
                        self.assertEqual(checks["controller_tick"], "healthy")
                    paths[name].write_text(json.dumps(docs[name]))
                    self.assertEqual(C.collect(config)["errors"], [])

    def test_failure_acknowledgement_waits_for_all_publication_steps(self):
        class FailedFlush(io.StringIO):
            def flush(self):
                raise OSError("fixture flush failure")

        for stage in ("feed-health.json", "feed-health.prom", "flush", "duration.json"):
            with self.subTest(stage=stage), fixture() as root:
                config = root / "config.json"
                config.write_text("{}")
                state_dir = root / "state"
                facts = {"tick": {"ok": True, "seconds": 1, "last_failure_at": 0}}
                real_write = F.atomic_text

                def write(path, value):
                    if path.name == stage:
                        raise OSError("fixture publication failure")
                    return real_write(path, value)

                with patch.object(C, "collect", side_effect=lambda _: dict(facts, observed_at=int(F.time.time()))), \
                        patch.object(F.time, "time", return_value=NOW) as wall:
                    with contextlib.redirect_stdout(io.StringIO()):
                        F.monitor(config, state_dir)
                    before = (state_dir / "duration.json").read_bytes()
                    facts["tick"]["last_failure_at"] = NOW + 10
                    wall.return_value = NOW + 60
                    output = FailedFlush() if stage == "flush" else io.StringIO()
                    with patch.object(F, "atomic_text", side_effect=write), \
                            contextlib.redirect_stdout(output), self.assertRaises(OSError):
                        F.monitor(config, state_dir)
                    self.assertEqual((state_dir / "duration.json").read_bytes(), before)
                    for elapsed, expected in ((120, "firing"), (180, "healthy")):
                        wall.return_value = NOW + elapsed
                        emitted = io.StringIO()
                        with contextlib.redirect_stdout(emitted):
                            document = F.monitor(config, state_dir)
                        self.assertEqual(next(c for c in document["checks"]
                                              if c["code"] == "controller_tick")["state"], expected)
                        self.assertEqual(json.loads(emitted.getvalue()), document)
                        self.assertEqual(json.loads((state_dir / "feed-health.json").read_text()), document)
                        self.assertIn(f'check="controller_tick",state="{expected}"}} 1',
                                      (state_dir / "feed-health.prom").read_text())
                        self.assertEqual(json.loads((state_dir / "duration.json").read_text())["last_failure_at"],
                                         NOW + 10)

    def test_actual_process_limits_and_failed_read(self):
        self.assertEqual(C.run([sys.executable, "-c", "print('observed')"]), b"observed\n")
        with self.assertRaises(ValueError):
            C.run([sys.executable, "-c", "print('x'*2000)"], limit=100)
        with self.assertRaises(ValueError):
            C.run([sys.executable, "-c", "raise SystemExit(2)"])
        with patch.object(C, "TIMEOUT", .05), self.assertRaises(TimeoutError):
            C.run([sys.executable, "-c", "import time; time.sleep(2)"])
        with patch.object(C, "TIMEOUT", .05), self.assertRaises(TimeoutError):
            C.run([sys.executable, "-c", "import os,time; os.close(1); time.sleep(2)"])

    def test_exited_process_leader_does_not_leave_its_child_running(self):
        for mode, error in (("timeout", TimeoutError), ("overflow", ValueError),
                            ("failure", ValueError), ("success", None)):
            with self.subTest(mode=mode), fixture() as root, socket.socket() as listener:
                listener.bind(("127.0.0.1", 0))
                listener.listen(1)
                listener.settimeout(2)
                code = f"""
import os
from pathlib import Path
import socket
import time
leader = os.getpid()
child = os.fork()
if child:
    Path("child.pid").write_text(str(child))
    os._exit(2 if {mode!r} == "failure" else 0)
while os.getppid() == leader:
    time.sleep(.01)
with socket.create_connection({listener.getsockname()!r}, timeout=2) as connection:
    connection.sendall(b"ready")
    if {mode!r} == "overflow":
        os.write(1, b"x" * 2000)
    elif {mode!r} != "timeout":
        os.close(1)
    time.sleep(30)
"""
                stopped = False
                try:
                    with patch.object(C, "TIMEOUT", 2):
                        if error:
                            with self.assertRaises(error):
                                C.run([sys.executable, "-c", code], cwd=root, limit=100)
                        else:
                            self.assertEqual(C.run([sys.executable, "-c", code], cwd=root, limit=100), b"")
                    connection, _ = listener.accept()
                    with connection:
                        connection.settimeout(2)
                        with connection.makefile("rb") as stream:
                            self.assertEqual(stream.read(5), b"ready")
                            try:
                                self.assertEqual(stream.read(1), b"")
                            except TimeoutError:
                                self.fail("collector left its exited leader's child running")
                            stopped = True
                finally:
                    if not stopped and (root / "child.pid").exists():
                        try:
                            os.kill(int((root / "child.pid").read_text()), signal.SIGKILL)
                        except ProcessLookupError:
                            pass

    def test_atomic_output_replaces_success_with_explicit_unknown(self):
        with fixture() as root:
            output = root / "status.json"
            F.atomic_json(output, {"old": "success"})
            with patch.object(C, "controller", side_effect=ValueError("private secret")):
                self.assertEqual(C.main(["controller", "--output", str(output)]), 1)
            result = json.loads(output.read_text())
            self.assertEqual(result["error"], "source unavailable")
            self.assertNotIn("private", output.read_text())
            self.assertFalse(output.with_suffix(".json.new").exists())

    def test_actual_evaluator_process_restart_preserves_duration_and_updates_json_metrics(self):
        with fixture() as root:
            docs = self.documents()
            now = int(time.time())
            config = {}
            for name, value in docs.items():
                value["observed_at"] = now
                if value["source"] == "controller":
                    value["tick"]["finished_at"] = now
                if value["source"] == "server":
                    value["last_workunits"] = {app: now - 20 for app in C.APP_NAMES}
                    value["enabled_until"] = now + 86400
                path = root / (name[1:] + ".json")
                path.write_text(json.dumps(value))
                config[name[1:]] = str(path)
            config["clients"] = [config.pop("own")]
            config.pop("volunteer")
            config_path = root / "config.json"
            config_path.write_text(json.dumps(config))
            args = [sys.executable, str(Path(F.__file__)), "--config", str(config_path),
                    "--state-dir", str(root / "state")]
            first = json.loads(C.run(args))
            self.assertEqual(next(c for c in first["checks"] if c["code"] == "collection")["state"], "firing")
            time.sleep(1.05)
            args[2:4] = [f"--config={config_path}"]
            second = json.loads(C.run(args))
            self.assertEqual(next(c for c in second["checks"] if c["code"] == "collection")["state"], "healthy")
            self.assertGreater(next(c for c in second["checks"] if c["code"] == "usnm_ocr_no_work")["duration_s"], 0)
            self.assertEqual(json.loads((root / "state/feed-health.json").read_text()), second)
            self.assertIn("boinc_feed_observed_at_seconds", (root / "state/feed-health.prom").read_text())

    def test_backlog_reads_real_ready_and_selection_files_without_assuming_empty(self):
        with fixture() as root:
            requests, outbox = root / "requests", root / "campaign"
            requests.mkdir()
            outbox.mkdir()
            from datetime import datetime, timezone
            checked = datetime.now(timezone.utc).isoformat()
            view = root / "view.json"
            view.write_text(json.dumps({"azure": {"queues_stale": False, "queues": {
                "checked_at": checked, "counts": {"ocr-priority": 0, "ocr-bulk": 2}}}}))
            self.assertIsNone(C.backlog(str(view), requests, outbox)["backlogs"]["usnm_qwen"])
            chunk = outbox / "c00001"
            chunk.mkdir()
            (chunk / "READY").write_text("1")
            self.assertGreater(C.backlog(str(view), requests, outbox)["backlogs"]["usnm_qwen"], 0)
            (chunk / "READY").unlink()
            selected = root / "selected.jsonl"
            selected.write_text("")
            self.assertEqual(C.backlog(str(view), requests, outbox, selected)["backlogs"]["usnm_qwen"], 0)
            os.utime(selected, (0, 0))
            self.assertIsNone(C.backlog(str(view), requests, outbox, selected)["backlogs"]["usnm_qwen"])

    def test_php_server_process_reads_existing_database_contract_and_policy(self):
        with server_fixture() as root:
            result = C.server(root, root / "window.env")
            self.assertEqual(result["last_workunits"]["usnm_qwen"], NOW - 5)
            self.assertEqual(result["enabled_until"], NOW + 86400)
            self.assertEqual(result["plans"]["intel"]["max_wu_id"], "10")

    def test_feeder_guard_reports_exit_codes_and_rejects_invalid_output(self):
        drift = {"event": "boinc_feeder_slot_guard", "schema_version": 1, "status": "drift",
                 "reason": "registry_changed", "added_app_ids": [7], "removed_app_ids": [3],
                 "weight_changed_app_ids": [7]}
        unknown = {"event": "boinc_feeder_slot_guard", "schema_version": 1, "status": "unknown",
                   "reason": "database_unavailable"}
        with fixture() as root:
            guard = root / "guard.py"
            cases = [
                (drift, 1, {"status": "drift", "reason": "registry_changed", "app_ids": [3, 7]}),
                (unknown, 2, {"status": "unknown", "reason": "database_unavailable", "app_ids": []}),
                (drift, 3, C.GUARD_UNAVAILABLE),
                (drift | {"event": "other"}, 1, C.GUARD_UNAVAILABLE),
                (drift | {"schema_version": True}, 1, C.GUARD_UNAVAILABLE),
                (drift | {"status": "fine"}, 1, C.GUARD_UNAVAILABLE),
                (drift | {"reason": "not a name"}, 1, C.GUARD_UNAVAILABLE),
                (drift | {"added_app_ids": [-1]}, 1, C.GUARD_UNAVAILABLE),
                (drift | {"removed_app_ids": "3"}, 1, C.GUARD_UNAVAILABLE),
            ]
            for report, code, expected in cases:
                with self.subTest(report=report, code=code):
                    guard.write_text(f"import json, sys\nprint(json.dumps({report!r}))\nsys.exit({code})\n")
                    self.assertEqual(C.feeder_allocation(str(guard), root), expected)
            guard.unlink()
            self.assertEqual(C.feeder_allocation(str(guard), root), C.GUARD_UNAVAILABLE)
            # The image's guard, outside its container, is a valid unknown report.
            real = ROOT / "contrib-compute/images/server/feeder-slot-guard.py"
            result = C.feeder_allocation(str(real), root)
            self.assertEqual(result["status"], "unknown")
            self.assertNotEqual(result, C.GUARD_UNAVAILABLE)

    def test_feeder_guard_runs_in_the_container_like_its_systemd_unit(self):
        match = (b'{"event":"boinc_feeder_slot_guard","reason":"allocation_matches_registry",'
                 b'"schema_version":1,"status":"match"}')
        guard = "/usr/local/lib/usnm-boinc/feeder-slot-guard.py"
        with patch.object(C, "run", return_value=match) as run:
            result = C.feeder_allocation(guard, "/host/project", container="usnm-boinc-server")
        self.assertEqual(result, {"status": "match", "reason": "allocation_matches_registry", "app_ids": []})
        argv = run.call_args.args[0]
        self.assertEqual(run.call_args.kwargs, {"limit": 65536, "exits": (0, 1, 2)})
        unit = (ROOT / "scripts/boinc/onprem/usnm-feeder-slot-guard.service").read_text()
        self.assertIn("ExecStart=/usr/bin/" + " ".join(argv) + "\n", unit)

    def test_php_server_reports_feeder_allocation_only_with_a_guard(self):
        with server_fixture() as root:
            guard = root / "guard.py"
            guard.write_text("import sys\nprint('{\"event\":\"boinc_feeder_slot_guard\",\"reason\":"
                             "\"reload_changed_app_order_or_weights\",\"schema_version\":1,"
                             "\"status\":\"drift\"}')\nsys.exit(1)\n")
            output = root / "status.json"
            args = ["server", "--project", str(root), "--window-env", str(root / "window.env"),
                    "--output", str(output)]
            self.assertEqual(C.main(args), 0)
            self.assertNotIn("feeder_allocation", json.loads(output.read_text()))
            self.assertEqual(C.main(args + ["--feeder-guard", str(guard)]), 0)
            document = json.loads(output.read_text())
            self.assertEqual(document["feeder_allocation"], {
                "status": "drift", "reason": "reload_changed_app_order_or_weights", "app_ids": []})
            # The fixture database's workunits are dated relative to NOW.
            self.assertEqual(C.fresh(document | {"observed_at": NOW}, NOW, "server")["feeder_allocation"],
                             document["feeder_allocation"])
            guard.write_text("raise SystemExit(2)\n")
            self.assertEqual(C.main(args + ["--feeder-guard", str(guard)]), 0)
            self.assertEqual(json.loads(output.read_text())["feeder_allocation"], C.GUARD_UNAVAILABLE)

    def test_container_config_resolves_when_host_config_symlink_is_broken(self):
        with server_fixture() as root:
            host_project = root / "host-mount"
            host_project.mkdir()
            shutil.copyfile(root / "plan_class_spec.xml", host_project / "plan_class_spec.xml")
            (host_project / "config.xml").symlink_to(root / "unmounted-shm/config.xml")
            (root / "shm").mkdir()
            (root / "config.xml").rename(root / "shm/config.xml")
            (root / "config.xml").symlink_to(root / "shm/config.xml")
            real_run = C.run
            outputs = []

            def container_run(argv, cwd=None):
                self.assertEqual(argv[:5], ["podman", "exec", "--workdir",
                                           "/project/usnmocr/html/inc", "fixture-server"])
                self.assertIsNone(cwd)
                output = real_run(argv[5:], cwd=root / "html/inc")
                outputs.append(output)
                self.assertNotIn(b"fixture-only-secret", output)
                self.assertNotIn(b"<boinc", output)
                return output

            self.assertTrue((host_project / "config.xml").is_symlink())
            self.assertFalse((host_project / "config.xml").exists())
            with patch.object(C, "run", side_effect=container_run):
                result = C.server(host_project, root / "window.env", container="fixture-server")
            self.assertEqual(len(outputs), 1)
            self.assertEqual(result["last_workunits"]["usnm_qwen"], NOW - 5)
            self.assertEqual(result["plans"]["intel"]["max_wu_id"], "10")
            self.assertNotIn("fixture-only-secret", json.dumps(result))

    def test_php_server_config_failure_replaces_success_without_exporting_secrets(self):
        with server_fixture() as root:
            output = root / "status.json"
            args = ["server", "--project", str(root), "--window-env", str(root / "window.env"),
                    "--output", str(output)]
            configs = {
                "missing": None,
                "disabled": "<boinc><config><one_result_per_host_per_wu>0</one_result_per_host_per_wu></config></boinc>",
                "absent": "<boinc><config/></boinc>",
                "malformed": "<boinc><config>fixture-only-secret",
                "oversized": " " * (C.MAX_BYTES + 1),
            }
            for name, config in configs.items():
                with self.subTest(config=name):
                    F.atomic_json(output, {"old": "success"})
                    if config is None:
                        (root / "config.xml").unlink()
                    else:
                        (root / "config.xml").write_text(config)
                    self.assertEqual(C.main(args), 1)
                    self.assertEqual(json.loads(output.read_text())["error"], "source unavailable")
                    self.assertNotIn("fixture-only-secret", output.read_text())
                    self.assertNotIn("old", json.loads(output.read_text()))

    def test_php_flag_matches_native_scheduler_raw_tokens(self):
        with server_fixture() as root:
            tag = "one_result_per_host_per_wu"
            for value in ("0", "", "<![CDATA[]]>", "<![CDATA[1]]>", "<disabled/>", "&#49;"):
                with self.subTest(value=value):
                    (root / "config.xml").write_text(f"<boinc><config><{tag}>{value}</{tag}></config></boinc>")
                    with self.assertRaises(ValueError):
                        C.server(root, root / "window.env")
            for raw in (f"<boinc><{tag}/><config/></boinc>",
                        f"<boinc><config><{tag}/><{tag}/></config></boinc>",
                        f"<boinc><config/><config><{tag}/></config></boinc>"):
                with self.subTest(config=raw):
                    (root / "config.xml").write_text(raw)
                    with self.assertRaises(ValueError):
                        C.server(root, root / "window.env")
            for flag in (f"<{tag}/>", f"<{tag}> \n1\t </{tag}>"):
                with self.subTest(flag=flag):
                    (root / "config.xml").write_text(f"<boinc><config>{flag}</config></boinc>")
                    self.assertEqual(C.server(root, root / "window.env")["enabled_until"], NOW + 86400)

    def test_php_server_rejects_config_changed_during_database_read(self):
        with server_fixture() as root:
            with (root / "html/inc/boinc_db.inc").open("a") as source:
                source.write("file_put_contents('../../config.xml', '<boinc/>');\n")
            with self.assertRaisesRegex(ValueError, "source command failed"):
                C.server(root, root / "window.env")

    def test_loopback_rpc_reads_only_and_never_uses_volunteer_hosts(self):
        with client_rpc_fixture() as (root, port, answers, operations):
            result = C.client(root, port)
            self.assertEqual(result["host_id"], 9)
            self.assertEqual(result["idle_cpu"], 2)
            self.assertEqual(result["disk_bytes"], 300)
            self.assertEqual(operations, list(answers))
            self.assertNotIn("secret", json.dumps(result))
            answers["get_state"] = answers["get_state"].replace("</client_state>", """
<workunit><name>finished</name><app_name>usnm_ocr</app_name></workunit>
<result><wu_name>finished</wu_name><version_num>117</version_num><plan_class>docker</plan_class><state>4</state></result>
</client_state>""")
            self.assertEqual(C.client(root, port)["idle_cpu"], 2)
            answers["get_state"] = answers["get_state"].replace(
                "<state>4</state>", "<state>1</state>").replace(
                "<name>finished</name>", "<name>finished</name><rsc_memory_bound>100</rsc_memory_bound>")
            result = C.client(root, port)
            self.assertEqual((result["idle_cpu"], result["memory_bytes"]), (1, 400))
            answers["get_state"] = answers["get_state"].replace(
                "<rsc_memory_bound>100</rsc_memory_bound>", "")
            with self.assertRaisesRegex(ValueError, "occupied task memory unavailable"):
                C.client(root, port)
            answers["get_state"] = answers["get_state"].replace("<state>1</state>", "<state>4</state>")
            answers["get_state"] = answers["get_state"].replace("</project>",
                "<no_rsc_pref>CPU</no_rsc_pref><no_rsc_config>intel_gpu</no_rsc_config></project>")
            result = C.client(root, port)
            self.assertEqual(result["idle_cpu"], 0)
            self.assertEqual(result["available_cpu"], 2)
            self.assertEqual(result["idle_gpu"]["intel_gpu"], 0)

    def test_workunit_resource_records_replace_version_records_for_each_project(self):
        with client_rpc_fixture() as (root, port, answers, _):
            base = answers["get_state"]
            coproc = "<coproc><type>intel_gpu</type><count>.5</count></coproc>"
            for resources, gpu_version, expected in (
                    ("<avg_ncpus>2</avg_ncpus>", False, (0, 1)),
                    ("<avg_ncpus>2</avg_ncpus>", True, (0, 1)),
                    ("<avg_ncpus>.25</avg_ncpus>" + coproc, False, (1.75, .5)),
                    ("<avg_ncpus>0</avg_ncpus>" + coproc, False, (2, .5))):
                with self.subTest(resources=resources, gpu_version=gpu_version):
                    state = base.replace("</app_version>", coproc + "</app_version>") if gpu_version else base
                    answers["get_state"] = state.replace("</client_state>", queued_task(resources) + "</client_state>")
                    observed = C.client(root, port)
                    self.assertEqual((observed["available_cpu"], observed["idle_gpu"]["intel_gpu"]), expected)
                    self.assertEqual(observed["memory_bytes"], 400)
            other = """<project><master_url>https://other.invalid/</master_url><hostid>7</hostid></project>
<app_version><app_name>usnm_ocr</app_name><version_num>117</version_num><plan_class>docker</plan_class>
<avg_ncpus>.25</avg_ncpus><coproc><type>intel_gpu</type><count>.25</count></coproc></app_version>"""
            other += queued_task("<avg_ncpus>.5</avg_ncpus><coproc><type>intel_gpu</type><count>.75</count></coproc>")
            answers["get_state"] = base.replace("</client_state>", queued_task() + other + "</client_state>")
            observed = C.client(root, port)
            self.assertEqual((observed["available_cpu"], observed["idle_gpu"]["intel_gpu"], observed["memory_bytes"]),
                             (.5, .25, 300))

    def test_unprovable_task_reservations_are_source_local_unknown_and_recover(self):
        with client_rpc_fixture() as (root, port, answers, _):
            base = answers["get_state"]
            coproc = "<coproc><type>intel_gpu</type><count>.5</count></coproc>"
            for resources in ("<avg_ncpus>-1</avg_ncpus>", "<avg_ncpus>nan</avg_ncpus>",
                              "<avg_ncpus>1</avg_ncpus>" + coproc * 2,
                              "<avg_ncpus>1</avg_ncpus><coproc><type>intel_gpu</type></coproc>",
                              "<avg_ncpus>1</avg_ncpus><coproc><type>absent_gpu</type><count>1</count></coproc>"):
                with self.subTest(resources=resources):
                    answers["get_state"] = base.replace("</client_state>", queued_task(resources) + "</client_state>")
                    with self.assertRaisesRegex(C.SupplyUnknown, "task_resource_usage_unavailable"):
                        C.client(root, port)
            for active in ("<active_task/>", "<active_task><active_task_state>1</active_task_state></active_task>",
                           "<active_task><active_task_state>9</active_task_state></active_task>"):
                fields = active + "<resources>0.25 CPUs + 0.25 Intel GPUs</resources>"
                answers["get_state"] = base.replace(
                    "</client_state>", queued_task("<avg_ncpus>2</avg_ncpus>", fields) + "</client_state>")
                with self.subTest(active=active), self.assertRaisesRegex(C.SupplyUnknown, "active_task_resource_usage_unavailable"):
                    C.client(root, port)
            output = root / "client.json"
            args = ["client", "--data-dir", str(root), "--port", str(port), "--output", str(output)]
            docs = self.documents()
            config = {name: "/" + name for name in ("server", "controller", "backlog")}
            config["clients"] = ["/own"]
            with patch.object(C.time, "time", return_value=NOW):
                self.assertEqual(C.main(args), 1)
                docs["/own"] = json.loads(output.read_text())
                with patch.object(C, "read_source", side_effect=lambda path: docs[path]):
                    facts = C.collect(config)
                self.assertEqual(facts["sources"]["client_0"]["reason"], "active_task_resource_usage_unavailable")
                document, _ = F.evaluate(facts, {}, NOW, 1000, "boot", "cfg")
                checks = {c["code"]: c["state"] for c in document["checks"]}
                self.assertEqual((checks["cpu_shortage"], checks["gpu_shortage"], checks["controller_tick"]),
                                 ("unknown", "unknown", "healthy"))
                answers["get_state"] = answers["get_state"].replace(
                    "<active_task_state>9</active_task_state>", "<active_task_state>0</active_task_state>")
                self.assertEqual(C.main(args), 0)
                recovered = json.loads(output.read_text())
                self.assertNotIn("error", recovered)
                self.assertEqual(recovered["available_cpu"], 0)
                answers["get_state"] = answers["get_state"].replace(
                    "<active_task_state>0</active_task_state>", "<active_task_state>1</active_task_state>").replace(
                    "<state>1</state>", "<state>4</state>")
                self.assertEqual(C.main(args), 0)
                self.assertEqual(json.loads(output.read_text())["available_cpu"], 2)

    @unittest.skipUnless(os.environ.get("USNM_FEED_DASH_BINARY"), "set USNM_FEED_DASH_BINARY for binary rehearsal")
    def test_python_observations_to_real_dashboard_http_and_durable_monitor(self):
        with fixture() as root:
            with socket.socket() as available:
                available.bind(("127.0.0.1", 0))
                port = available.getsockname()[1]
            address = f"http://127.0.0.1:{port}"
            docs = self.documents()
            docs.pop("/volunteer")
            docs["/client-own"] = docs.pop("/own")
            now = int(time.time())
            for value in docs.values():
                value["observed_at"] = now
            docs["/server"]["enabled_until"] = now - 1
            docs["/server"]["last_workunits"] = {app: now - 4000 for app in C.APP_NAMES}
            docs["/controller"]["tick"] = {"ok": False, "seconds": 2, "finished_at": now}
            args = [os.environ["USNM_FEED_DASH_BINARY"], "--listen", f"127.0.0.1:{port}",
                    "--allow", "127.0.0.1", "--scans", str(root), "--manifests", "none",
                    "--state-dir", str(root / "dashboard"),
                    "--feed-health", str(root / "monitor/feed-health.json")]
            for source in docs:
                args += ["--feed-push", source[1:] + "=127.0.0.1"]
            with (root / "dashboard.log").open("w") as log:
                process = subprocess.Popen(args, stdout=log, stderr=log)
                try:
                    for _ in range(100):
                        try:
                            with C.urllib.request.urlopen(address + "/healthz", timeout=.2):
                                break
                        except OSError:
                            if process.poll() is not None:
                                self.fail((root / "dashboard.log").read_text())
                            time.sleep(.05)
                    else:
                        self.fail("dashboard not responsive")
                    for source, value in docs.items():
                        request = C.urllib.request.Request(address + "/api/feed-source" + source,
                            data=json.dumps(value).encode(), method="PUT",
                            headers={"Content-Type": "application/json"})
                        with C.urllib.request.urlopen(request, timeout=2) as response:
                            self.assertEqual(response.status, 204)
                    source_dir = root / "dashboard/feed-sources"
                    config = {name: str(source_dir / (name + ".json"))
                              for name in ("server", "controller", "backlog")}
                    config["clients"] = [str(source_dir / "client-own.json")]
                    (root / "config.json").write_text(json.dumps(config))
                    command = [sys.executable, F.__file__, "--config", str(root / "config.json"),
                               "--state-dir", str(root / "monitor")]
                    document = json.loads(C.run(command))
                    view = C.read_source(address + "/api/view.json")
                    self.assertEqual(view["feed"]["observed_at"], document["observed_at"])
                    checks = {c["code"]: c["state"] for c in view["feed"]["checks"]}
                    self.assertEqual(checks["qwen_refill_window"], "firing")
                    self.assertEqual(checks["controller_tick"], "firing")
                    with C.urllib.request.urlopen(address, timeout=2) as response:
                        self.assertIn(b"BOINC feed / stale data", response.read())
                    (source_dir / "client-own.json").write_text("{}")
                    C.run(command)
                    view = C.read_source(address + "/api/view.json")
                    self.assertTrue(view["feed"]["stale"])
                    self.assertEqual(next(c for c in view["feed"]["checks"] if c["code"] == "gpu_shortage")["state"], "unknown")
                finally:
                    process.terminate()
                    try:
                        process.wait(timeout=5)
                    except subprocess.TimeoutExpired:
                        process.kill()
                        process.wait()


if __name__ == "__main__":
    unittest.main()
