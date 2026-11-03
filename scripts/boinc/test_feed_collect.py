import contextlib
from copy import deepcopy
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import io
import json
import os
from pathlib import Path
import shutil
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
                                "avg_ncpus": "1"},
                      "docker": {}},
            "units": [{"id": 5, "app": "usnm_qwen", "batch": 8, "app_version_num": 117,
                       "hr_class": 0, "assignments": 0, "hosts": [], "unsent": 2,
                       "rsc_memory_bound": 10, "rsc_disk_bound": 10}]}


class EligibilityTests(unittest.TestCase):
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
        db["plans"]["half"] = {"min_batch": "2", "max_batch": "2", "ngpus": ".5", "avg_ncpus": "1"}
        with self.assertRaisesRegex(ValueError, "mixed GPU resource profiles"):
            C.supply(db, [h], "gpu")
        half["gpu_count"] = 1
        db["plans"]["half"]["ngpus"] = "1"
        self.assertEqual(C.supply(db, [h], "gpu"), {"idle": 1, "eligible": 1})
        half.update(gpu="NVIDIA", gpu_count=.5)
        db["plans"]["half"]["ngpus"] = ".5"
        h["idle_gpu"]["NVIDIA"] = 1
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
        self.assertEqual(C.supply(db, [h], "gpu"), {"idle": 2, "eligible": 1})

    def test_cumulative_disk_budget_is_checked_per_host_not_per_workunit(self):
        db, h = database(), host()
        h.update(cpus=2, idle_cpu=2)
        db["units"] = [dict(db["units"][0], id=i, app="usnm_ocr", rsc_disk_bound=600) for i in (5, 6)]
        self.assertEqual(C.supply(db, [h], "cpu"), {"idle": 2, "eligible": 1})
        db["units"][1]["rsc_disk_bound"] = 500
        with self.assertRaisesRegex(ValueError, "cumulative host disk budget"):
            C.supply(db, [h], "cpu")
        db["units"][1]["rsc_disk_bound"] = 600
        h["disk_bytes"] = 1200
        self.assertEqual(C.supply(db, [h], "cpu"), {"idle": 2, "eligible": 2})
        self.assertEqual(C.supply(db, [host(9), host(10)], "cpu"), {"idle": 2, "eligible": 2})

    def test_shared_results_and_one_replica_per_host_are_not_double_counted(self):
        db = database()
        self.assertEqual(C.supply(db, [host(9, 2)], "gpu"), {"idle": 2, "eligible": 1})
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


class ControllerTests(unittest.TestCase):
    def observe(self, events=(), **status):
        properties = {"LoadState": "loaded", "SubState": "dead", "Result": "success",
                      "ExecMainStartTimestampMonotonic": 100000000,
                      "ExecMainExitTimestampMonotonic": 120000000}
        properties.update(status)
        journal = "\n".join(json.dumps({
            "__REALTIME_TIMESTAMP": str(at * 1000000), "MESSAGE": json.dumps(event),
        }) for at, event in events).encode()
        systemd = "\n".join(f"{key}={value}" for key, value in properties.items()).encode()
        with patch.object(C, "run", side_effect=[journal, systemd]), \
                patch.object(C.time, "time", return_value=NOW):
            return C.controller("usnm-boinc-priority-controller.service")

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

    def test_disk_is_shared_between_cpu_and_gpu_allocations(self):
        docs = self.documents()
        docs["/server"]["units"][0]["rsc_disk_bound"] = 600
        docs["/server"]["units"].append(dict(docs["/server"]["units"][0], id=6, app="usnm_ocr"))
        config = {name: "/" + name for name in ("server", "controller", "backlog")}
        config["clients"] = ["/own"]
        with patch.object(C, "read_source", side_effect=lambda path: docs[path]), \
                patch.object(C.time, "time", return_value=NOW):
            facts = C.collect(config)
            self.assertEqual(facts["supply"], {})
            self.assertCountEqual(facts["errors"], ["cpu_disk_budget", "gpu_disk_budget"])
            docs["/own"]["disk_bytes"] = 1200
            facts = C.collect(config)
            self.assertEqual(facts["errors"], [])
            self.assertEqual(facts["supply"], {"cpu": {"idle": 1, "eligible": 1},
                                              "gpu": {"idle": 1, "eligible": 1}})

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
                result = C.client(root, rpc.server_address[1])
                self.assertEqual(result["host_id"], 9)
                self.assertEqual(result["idle_cpu"], 2)
                self.assertEqual(result["disk_bytes"], 300)
                self.assertEqual(operations, list(answers))
                self.assertNotIn("secret", json.dumps(result))
                answers["get_state"] = answers["get_state"].replace("</client_state>", """
<workunit><name>finished</name><app_name>usnm_ocr</app_name></workunit>
<result><wu_name>finished</wu_name><version_num>117</version_num><plan_class>docker</plan_class><state>4</state></result>
</client_state>""")
                self.assertEqual(C.client(root, rpc.server_address[1])["idle_cpu"], 2)
                answers["get_state"] = answers["get_state"].replace("</project>",
                    "<no_rsc_pref>CPU</no_rsc_pref><no_rsc_config>intel_gpu</no_rsc_config></project>")
                result = C.client(root, rpc.server_address[1])
                self.assertEqual(result["idle_cpu"], 0)
                self.assertEqual(result["idle_gpu"]["intel_gpu"], 0)
            finally:
                rpc.shutdown()
                thread.join()

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
