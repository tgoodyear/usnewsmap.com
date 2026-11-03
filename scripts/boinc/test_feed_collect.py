import contextlib
from copy import deepcopy
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
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
            "idle_cpu": 1, "idle_gpu": {"intel_gpu": count}, "memory_bytes": 1000, "disk_bytes": 1000}


def database():
    return {"minimum_versions": {"usnm_ocr": 0, "usnm_qwen": 117},
            "host_preferences": {str(h): {"apps": list(C.APP_NAMES), "blocked": []} for h in (9, 10)},
            "versions": [version(), version("usnm_ocr", "docker", None)],
            "plans": {"intel": {"min_wu_id": "2", "max_wu_id": "10", "min_batch": "8", "max_batch": "8"},
                      "docker": {}},
            "units": [{"id": 5, "app": "usnm_qwen", "batch": 8, "app_version_num": 117,
                       "hr_class": 0, "assignments": 0, "hosts": [], "unsent": 2,
                       "rsc_memory_bound": 10, "rsc_disk_bound": 10}]}


class EligibilityTests(unittest.TestCase):
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
        class Handler(BaseHTTPRequestHandler):
            def do_GET(self):
                requests.append(self.path)
                self.send_response(200 if self.path in docs else 503)
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
                    docs["/own"]["observed_at"] = NOW - 181
                    self.assertNotIn("supply", C.collect(config))
                    docs["/own"]["observed_at"] = NOW + 1
                    self.assertIn("client_0", C.collect(config)["errors"])
                    del docs["/server"]
                    self.assertNotIn("enabled_until", C.collect(config))
            finally:
                http.shutdown()
                thread.join()

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
