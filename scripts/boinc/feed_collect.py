#!/usr/bin/env python3
"""Bounded read-only BOINC observations. No admission, dispatch, or feed ownership.

Run source collectors beside their existing services. The evaluator reads their
atomic files (or existing private HTTP mirrors), never invokes a remote shell.
"""
import argparse
from concurrent.futures import ThreadPoolExecutor, wait
from datetime import datetime
import http.client
import json
import math
import os
from pathlib import Path
import re
import selectors
import subprocess
import sys
import time
import urllib.request
import urllib.error
import xml.etree.ElementTree as ET

from feed_status import APP_NAMES, MAX_AGE, TICK_LIMIT_S, atomic_json

MAX_BYTES = 8 * 1024 * 1024
MAX_ROWS = 8000
TIMEOUT = 15
NAME = re.compile(r"[a-zA-Z0-9_-]{1,80}")


def read_file(path, limit=MAX_BYTES):
    with Path(path).open("rb") as stream:
        value = stream.read(limit + 1)
    if len(value) > limit:
        raise ValueError("source size limit")
    return value


def run(argv, cwd=None, limit=MAX_BYTES):
    """Bound time AND output, including a child that never closes stdout."""
    process = subprocess.Popen(argv, cwd=cwd, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
                               start_new_session=True)
    output = bytearray()
    try:
        with selectors.DefaultSelector() as selector:
            selector.register(process.stdout, selectors.EVENT_READ)
            deadline = time.monotonic() + TIMEOUT
            while True:
                left = deadline - time.monotonic()
                if left <= 0 or not selector.select(left):
                    raise TimeoutError("source timeout")
                chunk = os.read(process.stdout.fileno(), min(65536, limit + 1 - len(output)))
                if not chunk:
                    break
                output.extend(chunk)
                if len(output) > limit:
                    raise ValueError("source size limit")
        if process.wait(timeout=max(.01, deadline - time.monotonic())):
            raise ValueError("source command failed")
        return bytes(output)
    finally:
        if process.poll() is None:
            import signal
            os.killpg(process.pid, signal.SIGKILL)
        process.wait()
        process.stdout.close()


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, *args, **kwargs):
        raise ValueError("source redirects are not accepted")


def read_source(location):
    if not isinstance(location, str) or not location:
        raise ValueError("invalid source location")
    if location.startswith(("http://", "https://")):
        request = urllib.request.Request(location, headers={"Accept": "application/json"})
        try:
            response = urllib.request.build_opener(NoRedirect).open(request, timeout=TIMEOUT)
        except urllib.error.HTTPError as error:
            error.close()
            raise ValueError("source HTTP failure") from None
        with response:
            deadline = time.monotonic() + TIMEOUT
            chunks, size = [], 0
            while True:
                chunk = response.read1(min(65536, MAX_BYTES + 1 - size))
                if time.monotonic() > deadline:
                    raise TimeoutError("source timeout")
                if not chunk:
                    break
                chunks.append(chunk)
                size += len(chunk)
                if size > MAX_BYTES:
                    raise ValueError("source size limit")
            if response.length not in (None, 0):
                raise http.client.IncompleteRead(b"", response.length)
            return json.loads(b"".join(chunks))
    return json.loads(read_file(location))


def natural(value, maximum=2**53):
    if type(value) is not int or not 0 <= value <= maximum:
        raise ValueError("invalid integer")
    return value


def fresh(document, now, kind):
    if not isinstance(document, dict):
        raise ValueError("source must be an object")
    if (type(document.get("schema_version")) is not int or document.get("schema_version") != 1 or document.get("source") != kind
            or not 0 <= now - natural(document.get("observed_at")) <= MAX_AGE
            or document.get("error")):
        raise ValueError("source unavailable, stale or future-dated")
    if kind == "controller":
        tick = document["tick"]
        if not isinstance(tick, dict):
            raise ValueError("invalid tick")
        if tick["ok"] is not None and type(tick["ok"]) is not bool:
            raise ValueError("invalid tick outcome")
        if type(tick["seconds"]) not in (int, float) or not math.isfinite(tick["seconds"]) or tick["seconds"] < 0:
            raise ValueError("invalid tick duration")
        if not 0 <= now - natural(tick["finished_at"]) <= 900:
            raise ValueError("stale controller evidence")
        if natural(tick.get("last_failure_at", 0)) > now:
            raise ValueError("future controller failure")
    if kind == "backlog":
        if not isinstance(document["backlogs"], dict):
            raise ValueError("invalid backlogs")
        for app in APP_NAMES:
            if document["backlogs"][app] is not None:
                natural(document["backlogs"][app])
    if kind == "server":
        if any(not isinstance(document[key], dict) for key in (
                "last_workunits", "minimum_versions", "host_preferences", "plans")):
            raise ValueError("invalid server mappings")
        natural(document["enabled_until"])
        for app in APP_NAMES:
            if natural(document["last_workunits"][app]) > now:
                raise ValueError("future workunit")
            natural(document["minimum_versions"][app])
        if (len(document["plans"]) > 256 or any(not isinstance(plan, dict)
                or any(not isinstance(value, str) for value in plan.values())
                for plan in document["plans"].values())):
            raise ValueError("invalid scheduler policies")
        for prefs in document["host_preferences"].values():
            if (not isinstance(prefs, dict)
                    or not isinstance(prefs["apps"], list) or not set(prefs["apps"]) <= set(APP_NAMES)
                    or not isinstance(prefs["blocked"], list)
                    or not set(prefs["blocked"]) <= {"CPU", "NVIDIA", "ATI", "intel_gpu", "apple_gpu"}
                    or type(prefs.get("fallback", False)) is not bool):
                raise ValueError("invalid host preferences")
        if (not isinstance(document["units"], list) or not isinstance(document["versions"], list)
                or len(document["units"]) > MAX_ROWS or len(document["versions"]) > 256
                or any(not isinstance(row, dict) for row in document["units"] + document["versions"])):
            raise ValueError("inventory limit")
        for unit in document["units"]:
            if unit["app"] not in APP_NAMES or not isinstance(unit["hosts"], list):
                raise ValueError("invalid workunit")
            for key in ("id", "batch", "app_version_num", "hr_class", "unsent", "assignments"):
                natural(unit[key])
            for host in unit["hosts"]:
                natural(host)
            for key in ("rsc_memory_bound", "rsc_disk_bound"):
                if type(unit[key]) not in (int, float) or not math.isfinite(unit[key]) or unit[key] < 0:
                    raise ValueError("invalid workunit resource bound")
        if len({u["id"] for u in document["units"]}) != len(document["units"]):
            raise ValueError("duplicate workunits")
        for version in document["versions"]:
            natural(version["version_num"])
            if (version["app"] not in APP_NAMES or not isinstance(version["platform"], str)
                    or version["plan_class"] and version["plan_class"] not in document["plans"]):
                raise ValueError("invalid dispatchable version")
    if kind == "client":
        if (natural(document["host_id"]) == 0 or not isinstance(document["versions"], list)
                or len(document["versions"]) > 256
                or any(not isinstance(version, dict) for version in document["versions"])):
            raise ValueError("invalid own host")
        if (not isinstance(document.get("gpu_memory", {}), dict)
                or not isinstance(document.get("gpu_capabilities", {}), dict)
                or any(not isinstance(caps, dict) for caps in document.get("gpu_capabilities", {}).values())):
            raise ValueError("invalid GPU capabilities")
        for field in ("idle_cpu", "memory_bytes", "disk_bytes"):
            if type(document[field]) not in (int, float) or not math.isfinite(document[field]) or document[field] < 0:
                raise ValueError("invalid capacity")
        if not isinstance(document["idle_gpu"], dict) or len(document["idle_gpu"]) > 8:
            raise ValueError("invalid devices")
        for value in document["idle_gpu"].values():
            if type(value) not in (int, float) or not math.isfinite(value) or not 0 <= value <= 64:
                raise ValueError("invalid GPU capacity")
        for version in document["versions"]:
            if (version["app"] not in APP_NAMES or not isinstance(version["platform"], str)
                    or version["gpu"] is not None and not isinstance(version["gpu"], str)):
                raise ValueError("invalid client application")
            natural(version["version_num"])
            for key in ("cpus", "gpu_count"):
                value = version[key]
                if type(value) not in (int, float) or not math.isfinite(value) or value < 0:
                    raise ValueError("invalid resource profile")
            if version["cpus"] <= 0 or bool(version["gpu"]) != (version["gpu_count"] > 0):
                raise ValueError("invalid slot profile")
    return document


def xml(raw):
    if b"<!DOCTYPE" in raw.upper() or b"<!ENTITY" in raw.upper():
        raise ValueError("unsafe XML")
    return ET.fromstring(raw)


def window(path):
    values = re.findall(r"^ENABLED_UNTIL=([0-9]+)\s*$",
                        read_file(path, 65536).decode(), re.MULTILINE)
    if len(values) != 1:
        raise ValueError("missing or duplicate ENABLED_UNTIL")
    return int(values[0])


# The BOINC connection and TLS settings remain in boinc_db.inc/config.xml.
# No result text, project credentials, user names or host addresses are exported.
PHP = r"""
function feed_config() {
    $raw = @file_get_contents('../../config.xml', false, null, 0, 8388609);
    if ($raw === false || strlen($raw) > 8388608 || preg_match('/<!DOCTYPE|<!ENTITY/i', $raw)) {
        throw new Exception('invalid scheduler config');
    }
    return $raw;
}
$feed_config_before = feed_config();
libxml_use_internal_errors(true);
$feed_config_xml = simplexml_load_string($feed_config_before, 'SimpleXMLElement', LIBXML_NONET);
libxml_clear_errors();
if ($feed_config_xml === false) throw new Exception('invalid scheduler config');
// Match native_qwen_one_result_per_host's raw tokens without deploying that operator helper.
$feed_flags = $feed_config_xml->xpath('/boinc/config/one_result_per_host_per_wu');
if (count($feed_flags) !== 1 ||
    count($feed_config_xml->xpath('//one_result_per_host_per_wu')) !== 1 ||
    preg_match_all('/<one_result_per_host_per_wu[\s>\/]/', $feed_config_before) !== 1 ||
    preg_match_all('/<config[\s>\/]/', $feed_config_before) !== 1 ||
    preg_match('/<config>(?:(?!<\/config>).)*(?:<one_result_per_host_per_wu\s*\/>|'
        .'<one_result_per_host_per_wu>\s*1\s*<\/one_result_per_host_per_wu>)/s', $feed_config_before) !== 1) {
    throw new Exception('requires one-result-per-host scheduling');
}
require_once 'boinc_db.inc';
$db = BoincDb::get(true);
function rows($sql) {
    global $db;
    $r = $db->do_query($sql);
    if (!$r) throw new Exception('read failed');
    $out = [];
    while ($row = $r->fetch_assoc()) {
        if (count($out) >= 8000) throw new Exception('row limit');
        $out[] = $row;
    }
    return $out;
}
if (!$db->do_query('SET SESSION TRANSACTION READ ONLY') ||
    !$db->do_query('START TRANSACTION WITH CONSISTENT SNAPSHOT')) throw new Exception('read transaction');
$apps = rows("SELECT /*+ MAX_EXECUTION_TIME(2000) */ a.name,a.id,a.min_version,
    COALESCE(MAX(w.create_time),0) AS last_workunit
    FROM app a LEFT JOIN workunit w ON a.id=w.appid
    WHERE a.name IN ('usnm_ocr','usnm_qwen') GROUP BY a.id");
$versions = rows("SELECT /*+ MAX_EXECUTION_TIME(2000) */ a.name AS app,
    av.id,av.version_num,av.plan_class,p.name AS platform
    FROM app_version av JOIN app a ON a.id=av.appid JOIN platform p ON p.id=av.platformid
    WHERE av.deprecated=0 AND a.name IN ('usnm_ocr','usnm_qwen')");
$preferences = rows("SELECT /*+ MAX_EXECUTION_TIME(2000) */ h.id,h.venue,u.project_prefs
    FROM host h JOIN user u ON u.id=h.userid LIMIT 8001");
$units = rows("SELECT /*+ MAX_EXECUTION_TIME(2000) */ a.name AS app,w.id,w.batch,
    w.app_version_num,w.hr_class,w.rsc_memory_bound,w.rsc_disk_bound,
    COUNT(*) AS unsent,
    (SELECT COUNT(*) FROM assignment x WHERE x.workunitid=w.id) AS assignments,
    (SELECT JSON_ARRAYAGG(r2.hostid) FROM result r2 WHERE r2.workunitid=w.id
        AND r2.hostid>0) AS hosts
    FROM workunit w JOIN app a ON a.id=w.appid JOIN result r ON r.workunitid=w.id
    WHERE a.name IN ('usnm_ocr','usnm_qwen') AND r.server_state=2
        AND r.hostid=0 AND r.sent_time=0 AND r.outcome=0
        AND w.canonical_resultid=0 AND w.error_mask=0
    GROUP BY w.id LIMIT 8001");
$db->do_query('ROLLBACK');
if ($feed_config_before !== feed_config()) throw new Exception('config changed during observation');
echo json_encode(['apps'=>$apps,'versions'=>$versions,'units'=>$units,'preferences'=>$preferences], JSON_THROW_ON_ERROR);
"""


def host_preferences(rows, apps):
    """Reduce authoritative server account/venue preferences; never export their raw XML."""
    preferences = {}
    for row in rows:
        raw = (row["project_prefs"] or "").encode()
        root = xml(raw) if raw.strip().startswith(b"<project_preferences") else xml(b"<project_preferences>" + raw + b"</project_preferences>")
        venues = [v for v in root.findall("venue") if v.get("name") == row["venue"]]
        if len(venues) > 1:
            raise ValueError("ambiguous venue preferences")
        node = venues[0] if venues else root
        selection = node.find("apps_selected")
        ids = node.findall("app_id") if selection is None else selection.findall("app_id")
        selected = {int(n.text) for n in ids}
        allowed = [app["name"] for app in apps if int(app["id"]) in selected] if ids or selection is not None else list(APP_NAMES)
        fallback = node.findtext("allow_non_preferred_apps", "0") == "1" and set(allowed) != set(APP_NAMES)
        blocked = []
        for resource, tag in (("CPU", "no_cpu"), ("NVIDIA", "no_cuda"), ("ATI", "no_ati"),
                              ("intel_gpu", "no_intel_gpu"), ("apple_gpu", "no_apple_gpu")):
            if node.findtext(tag, "0").strip() == "1":
                blocked.append(resource)
        preferences[str(int(row["id"]))] = {"apps": allowed, "blocked": blocked, "fallback": fallback}
    return preferences


def server(project, env_file, container=None, container_project="/project/usnmocr"):
    # PHP validates config in its own namespace; only inventory crosses the container boundary.
    before = read_file(project / "plan_class_spec.xml")
    command = ["php", "-r", PHP]
    if container:
        command = ["podman", "exec", "--workdir", container_project + "/html/inc", container, *command]
    data = json.loads(run(command, cwd=None if container else project / "html/inc"))
    if before != read_file(project / "plan_class_spec.xml"):
        raise ValueError("policy changed during observation")
    plans = {}
    for node in xml(before).findall("plan_class"):
        name = node.findtext("name")
        if not name or name in plans:
            raise ValueError("invalid plan classes")
        plans[name] = {child.tag: child.text or "" for child in node}
        if len(plans[name]) != len(node) or any(len(child) for child in node):
            raise ValueError("ambiguous or unsupported policy")
    for row in data["units"]:
        row["hosts"] = list(set(map(int, json.loads(row["hosts"])))) if row["hosts"] else []
        for field in ("id", "batch", "app_version_num", "hr_class", "unsent", "assignments"):
            row[field] = int(row[field])
        for field in ("rsc_memory_bound", "rsc_disk_bound"):
            row[field] = float(row[field])
    for row in data["versions"]:
        row["id"], row["version_num"] = int(row["id"]), int(row["version_num"])
    data["host_preferences"] = host_preferences(data.pop("preferences"), data["apps"])
    data["last_workunits"] = {r["name"]: int(r["last_workunit"]) for r in data["apps"]}
    data["minimum_versions"] = {r["name"]: int(r["min_version"]) for r in data.pop("apps")}
    return data | {"plans": plans, "enabled_until": window(env_file)}


def client(data_dir, port):
    from client_account import MASTER, RPC, flag

    password = read_file(data_dir / "gui_rpc_auth.cfg", 1024).decode().strip()
    rpc = RPC(port, password)
    try:
        state = rpc.call("get_state")
        status = rpc.call("get_cc_status")
        prefs = rpc.call("get_global_prefs_working")
        disk = rpc.call("get_disk_usage")
    finally:
        rpc.close()
    projects = [p for p in state.findall(".//project")
                if p.findtext("master_url", "").rstrip("/") == MASTER.rstrip("/")]
    if len(projects) != 1:
        raise ValueError("project not attached uniquely")
    project = projects[0]
    host_id = int(project.findtext("hostid", "0"))
    if host_id <= 0 or flag(project, "suspended_via_gui") or flag(project, "dont_request_more_work"):
        raise ValueError("client is not accepting work")
    if any(int(status.findtext(".//" + tag, "-1")) != 0
           for tag in ("task_suspend_reason", "gpu_suspend_reason")):
        raise ValueError("client suspended or status unavailable")
    ncpus = int(state.findtext(".//host_info/p_ncpus", "0"))
    percent = float(prefs.findtext(".//max_ncpus_pct", "100")) or 100
    cpus = max(1, math.floor(ncpus * min(percent, 100) / 100)) if ncpus else 0
    # get_state groups app/version/workunit/result siblings after each project.
    # Names are only unique within a project, not across the whole client.
    scoped, current = [], None
    for node in state.find(".//client_state"):
        if node.tag == "project":
            current = node.findtext("master_url", "").rstrip("/")
        elif node.tag in ("app_version", "workunit", "result"):
            scoped.append((current, node))
    versions = []
    for owner, v in scoped:
        if v.tag != "app_version":
            continue
        app = v.findtext("app_name")
        coproc = v.find("coproc")
        versions.append({"project": owner, "app": app, "version_num": int(v.findtext("version_num")),
                         "plan_class": v.findtext("plan_class", ""),
                         "platform": v.findtext("platform", state.findtext(".//platform_name")),
                         "cpus": float(v.findtext("avg_ncpus", "1")),
                         "gpu": coproc.findtext("type") if coproc is not None else None,
                         "gpu_count": float(coproc.findtext("count", "1")) if coproc is not None else 0})
    # Count queued/download-pending work as occupied: it already feeds this device.
    used_cpu, used_gpu = 0, {}
    units = {(owner, w.findtext("name")): w.findtext("app_name") for owner, w in scoped if w.tag == "workunit"}
    for owner, result in scoped:
        if result.tag != "result":
            continue
        # Stock RESULT_COMPUTE_ERROR=3, FILES_UPLOADING=4: neither occupies execution.
        if int(result.findtext("state", "0")) >= 3 or flag(result, "ready_to_report"):
            continue
        if flag(result, "suspended_via_gui"):
            raise ValueError("suspended queued task")
        app = units.get((owner, result.findtext("wu_name")))
        match = [v for v in versions if v["project"] == owner and v["app"] == app
                 and v["version_num"] == int(result.findtext("version_num", "0"))
                 and v["plan_class"] == result.findtext("plan_class", "")
                 and v["platform"] == result.findtext("platform", state.findtext(".//platform_name"))]
        if len(match) != 1:
            # Unknown or other-project work must not inflate idle capacity.
            raise ValueError("unaccounted client task")
        v = match[0]
        used_cpu += v["cpus"]
        if v["gpu"]:
            used_gpu[v["gpu"]] = used_gpu.get(v["gpu"], 0) + v["gpu_count"]
    gpus, gpu_memory, capabilities = {}, {}, {}
    for node in state.findall(".//coprocs/*"):
        vendor = {"coproc_cuda": "NVIDIA", "coproc_ati": "ATI",
                  "coproc_intel_gpu": "intel_gpu", "coproc_apple_gpu": "apple_gpu"}.get(node.tag)
        if vendor:
            gpus[vendor] = max(0, float(node.findtext("count", "0")) - used_gpu.get(vendor, 0))
            gpu_memory[vendor] = max([float(n.text or 0) for tag in
                ("available_ram", "totalGlobalMem", "global_mem_size")
                for n in node.findall(".//" + tag)] or [0])
            opencl = re.search(r"OpenCL\s+(\d+)\.(\d+)", node.findtext(".//opencl_device_version", ""))
            capabilities[vendor] = {
                "min_cuda_version": int(node.findtext(".//cudaVersion", "0")),
                "min_nvidia_compcap": int(node.findtext(".//major", "0")) * 100 + int(node.findtext(".//minor", "0")),
                "min_opencl_version": int(opencl[1]) * 100 + int(opencl[2]) if opencl else 0,
            }
    blocked = {node.text for tag in ("no_rsc_pref", "no_rsc_apps", "no_rsc_ams", "no_rsc_config")
               for node in project.findall(tag)}
    for vendor, tag in (("NVIDIA", "no_cuda"), ("ATI", "no_ati"), ("intel_gpu", "no_intel_gpu"),
                        ("apple_gpu", "no_apple_gpu")):
        if vendor in blocked or flag(project, tag) or flag(project, tag + "_pref"):
            gpus[vendor] = 0
    memory = float(state.findtext(".//host_info/m_nbytes", "0"))
    fraction = min(float(prefs.findtext(".//ram_max_used_busy_pct", "50")),
                   float(prefs.findtext(".//ram_max_used_idle_pct", "90"))) / 100
    allowance = disk.findtext(".//d_allowed")
    free = disk.findtext(".//d_free")
    if allowance is None or free is None or not cpus or not versions:
        raise ValueError("client capacities or application versions unavailable")
    used = float(disk.findtext(".//d_boinc", "0")) + sum(
        float(node.findtext("disk_usage", "0")) for node in disk.findall(".//project"))
    from acceptance_client import container_runtime
    monitored = [{k: value for k, value in v.items() if k != "project"} for v in versions
                 if v["project"] == MASTER.rstrip("/") and v["app"] in APP_NAMES]
    return {"host_id": host_id, "versions": monitored,
            "idle_cpu": 0 if "CPU" in blocked or flag(project, "no_cpu_pref") or flag(project, "no_cpu")
            else max(0, cpus - used_cpu),
            "cpus": cpus, "idle_gpu": gpus, "gpu_memory": gpu_memory, "gpu_capabilities": capabilities,
            "client_version": sum(int(state.findtext(".//" + tag, "0")) * factor for tag, factor in (
                ("core_client_major_version", 10000), ("core_client_minor_version", 100), ("core_client_release", 1))),
            "docker": container_runtime() if any(v["plan_class"].startswith("docker") for v in monitored) else False,
            "memory_bytes": memory * fraction, "disk_bytes": max(0, min(float(free), float(allowance) - used))}


def controller(unit):
    if not re.fullmatch(r"[a-zA-Z0-9@_.-]+\.service", unit):
        raise ValueError("invalid unit")
    raw = run(["journalctl", "--no-pager", "--quiet", "-u", unit, "-o", "json",
               "--since=-2h", "-n", "2000"])
    latest, last_failure = None, 0
    for line in raw.splitlines():
        entry = json.loads(line)
        if not isinstance(entry, dict):
            continue
        try:
            event = json.loads(entry.get("MESSAGE", ""))
        except (ValueError, TypeError):
            continue
        if not isinstance(event, dict) or event.get("event") not in ("boinc_batch_tick", "boinc_batch_failure"):
            continue
        finished = int(entry["__REALTIME_TIMESTAMP"]) // 1000000
        ok = event["event"] == "boinc_batch_tick"
        if not ok:
            last_failure = max(last_failure, finished)
        timings = event.get("timings")
        if not isinstance(timings, dict):
            continue
        try:
            seconds = float(timings["tick_seconds"])
        except (KeyError, TypeError, ValueError):
            continue
        if not math.isfinite(seconds) or seconds < 0:
            continue
        if seconds > TICK_LIMIT_S:
            last_failure = max(last_failure, finished)
        latest = {"ok": ok, "seconds": seconds, "finished_at": finished}
    status = dict(line.split("=", 1) for line in run([
        "systemctl", "show", unit, "-p", "LoadState", "-p", "SubState", "-p", "Result",
        "-p", "ExecMainStartTimestampMonotonic", "-p", "ExecMainExitTimestampMonotonic",
    ]).decode().splitlines() if "=" in line)
    if status.get("LoadState") != "loaded":
        raise ValueError("controller not loaded")
    start = int(status.get("ExecMainStartTimestampMonotonic", "0")) / 1e6
    end = int(status.get("ExecMainExitTimestampMonotonic", "0")) / 1e6
    # This is the existing oneshot OCR controller, NOT a long-running Temporal worker.
    running = status.get("SubState") in ("start", "running") and start > end
    if running:
        return {"ok": None, "seconds": max(0, time.clock_gettime(time.CLOCK_MONOTONIC) - start),
                "finished_at": int(time.time()), "last_failure_at": last_failure}
    if status.get("Result") != "success":
        return {"ok": False, "seconds": max(0, end - start), "finished_at": int(time.time()),
                "last_failure_at": last_failure}
    if latest is None or time.time() - latest["finished_at"] > 900:
        raise ValueError("no recent completed controller tick")
    return latest | {"last_failure_at": last_failure}


def backlog(dashboard, requests, outbox, selection=None):
    view = read_source(dashboard)
    azure = view["azure"]
    q = azure["queues"]
    checked = datetime.fromisoformat(q["checked_at"].replace("Z", "+00:00")).timestamp()
    if azure["queues_stale"] or q.get("error") or not 0 <= time.time() - checked <= MAX_AGE:
        raise ValueError("queue evidence unavailable")
    result = {"usnm_ocr": sum(natural(q["counts"][key]) for key in ("ocr-priority", "ocr-bulk"))}
    # Inspect the existing immutable source requests and their inspector receipts.
    # Missing progress is not 'no backlog'; stale progress cannot close a backlog.
    pending, unknown = 0, False
    if not requests.is_dir():
        raise ValueError("source requests unavailable")
    for index, path in enumerate(requests.glob("*.json")):
        if index >= 1000:
            raise ValueError("request inventory limit")
        record = json.loads(read_file(path, 2 * 1024 * 1024))
        receipt_path = path.with_suffix(".progress")
        if not receipt_path.exists():
            unknown = True
            continue
        receipt = json.loads(read_file(receipt_path))
        if (not 0 <= time.time() - receipt_path.stat().st_mtime <= MAX_AGE
                or receipt["request_id"] != record["request_id"]
                or receipt["pages_sha256"] != record["pages_sha256"]):
            unknown = True
            continue
        counts = receipt["source_dispositions"]
        pending += sum(natural(counts.get(k, 0)) for k in (
            "pending_audit", "awaiting_preparation", "awaiting_publication", "preparation_quarantined"))
    if not outbox.is_dir():
        raise ValueError("Qwen campaign outbox unavailable")
    for index, path in enumerate(outbox.glob("c*/READY")):
        if index >= 1000:
            raise ValueError("outbox inventory limit")
        if path.is_file():
            pending += 1
    # The existing selector's actual output covers automatic replenishment as
    # well as explicit requests. Without fresh selection, an empty request list
    # does not prove that the app has no backlog.
    if selection is None or not selection.is_file() or not 0 <= time.time() - selection.stat().st_mtime <= MAX_AGE:
        unknown = True
    else:
        pending += sum(1 for line in read_file(selection).splitlines() if json.loads(line))
    result["usnm_qwen"] = pending if pending else None if unknown else 0
    return {"backlogs": result}


def host_profile(host, version, database):
    """Validate policy and live resource demand independently of workunit inventory."""
    plan = database["plans"][version["plan_class"]] if version["plan_class"] else {}
    if plan.get("project_prefs_tag"):
        raise ValueError("account-gated plan requires evidence")
    known = {"name", "project_prefs_tag", "min_wu_id", "max_wu_id", "min_batch", "max_batch",
             "docker", "gpu_type", "cuda", "opencl", "min_core_client_version",
             "min_ncpus", "max_threads", "avg_ncpus", "min_gpu_ram_mb",
             "gpu_ram_used_mb", "ngpus", "min_cuda_version", "min_nvidia_compcap", "min_opencl_version"}
    if set(plan) - known:
        raise ValueError("unsupported scheduler eligibility gate")
    for flag in ("docker", "cuda", "opencl"):
        if flag in plan and plan[flag].strip() not in ("", "1"):
            raise ValueError("unsupported scheduler flag")
    integers = {key: int(plan.get(key, "1" if key == "max_threads" else "0")) for key in (
        "min_wu_id", "max_wu_id", "min_batch", "max_batch", "max_threads",
        "min_core_client_version", "min_cuda_version", "min_nvidia_compcap", "min_opencl_version")}
    numbers = {key: float(plan.get(key, "1" if key == "ngpus" else "0")) for key in (
        "min_ncpus", "avg_ncpus", "min_gpu_ram_mb", "gpu_ram_used_mb", "ngpus")}
    if (any(value < 0 for value in integers.values())
            or any(not math.isfinite(value) or value < 0 for value in numbers.values())):
        raise ValueError("unmodeled scheduler resource policy")
    available = [v for v in database["versions"] if v["app"] == version["app"]
                 and v["plan_class"] == version["plan_class"] and v["platform"] == version["platform"]]
    minimum = database["minimum_versions"][version["app"]]
    if not available or minimum and version["version_num"] not in [
            v["version_num"] for v in available if v["version_num"] >= minimum]:
        return None
    if not minimum and version["version_num"] != max(v["version_num"] for v in available):
        return None
    effective = host.get("cpus")
    if numbers["min_ncpus"] or not version["gpu"] and not numbers["avg_ncpus"]:
        if type(effective) not in (int, float) or not math.isfinite(effective) or effective <= 0:
            raise ValueError("effective host CPU capacity unavailable")
    if (integers["min_core_client_version"] > host.get("client_version", 0)
            or numbers["min_ncpus"] and numbers["min_ncpus"] > effective
            or "docker" in plan and not host.get("docker")):
        return None
    if version["gpu"]:
        gpu = version["gpu"]
        expected = {"nvidia": "NVIDIA", "ati": "ATI", "amd": "ATI", "intel": "intel_gpu", "apple": "apple_gpu"}
        if "gpu_type" in plan and expected.get(plan["gpu_type"]) != gpu:
            return None
        if (numbers["ngpus"] or 1) != version["gpu_count"]:
            raise ValueError("live GPU policy differs from installed resource profile")
        # Without an explicit CPU demand the GPU profile depends on host performance
        # data this collector does not model; a cached value is not current evidence.
        if not numbers["avg_ncpus"]:
            raise ValueError("unmodeled GPU CPU demand")
        if numbers["min_gpu_ram_mb"] * 1e6 > host.get("gpu_memory", {}).get(gpu, 0):
            return None
        for gate in ("min_cuda_version", "min_nvidia_compcap", "min_opencl_version"):
            if integers[gate] > host.get("gpu_capabilities", {}).get(gpu, {}).get(gate, 0):
                return None
        demand = numbers["avg_ncpus"]
    else:
        if any(key in plan for key in ("gpu_type", "cuda", "opencl", "ngpus")):
            raise ValueError("CPU profile has an unmodeled GPU policy")
        demand = numbers["avg_ncpus"] or min(integers["max_threads"], effective)
    if demand <= 0 or demand != version["cpus"]:
        raise ValueError("live CPU demand differs from installed resource profile")
    return (integers["min_wu_id"], integers["max_wu_id"] or 2**63,
            integers["min_batch"], integers["max_batch"] or 2**63)


def eligible(unit, host, version, bounds):
    return (unit["app"] == version["app"] and host["host_id"] not in unit["hosts"]
            and not unit["assignments"] and not unit["hr_class"]
            and unit["rsc_memory_bound"] <= host["memory_bytes"]
            and unit["rsc_disk_bound"] <= host["disk_bytes"]
            and (unit["app_version_num"] == 0 or unit["app_version_num"] == version["version_num"])
            and bounds[0] <= unit["id"] <= bounds[1] and bounds[2] <= unit["batch"] <= bounds[3])


def supply(database, hosts, resource, disk_usage=None):
    """Maximum matching: a replica and a host/workunit pair can each be used once.

    Work offered to two classes is not two results, and two replicas of a unit
    cannot fill two slots on the same host.
    """
    prepared = []
    for host in hosts:
        prefs = database["host_preferences"][str(host["host_id"])]
        if prefs.get("fallback"):
            raise ValueError("non-preferred fallback needs all-application inventory")
        app = "usnm_ocr" if resource == "cpu" else "usnm_qwen"
        if app not in prefs["apps"] or resource == "cpu" and "CPU" in prefs["blocked"]:
            continue
        if resource == "gpu" and not any(host["idle_gpu"].values()):
            continue
        if resource == "cpu" and host["idle_cpu"] == 0:
            continue
        devices = {gpu for gpu, count in host["idle_gpu"].items()
                   if count > 0 and gpu not in prefs["blocked"]} if resource == "gpu" else {"cpu"}
        if not devices:
            continue
        profiles = []
        for version in host["versions"]:
            if version["app"] != app or (version["gpu"] or "cpu") not in devices:
                continue
            bounds = host_profile(host, version, database)
            if bounds is not None:
                profiles.append((version, bounds))
        if devices - {v["gpu"] or "cpu" for v, _ in profiles}:
            raise ValueError("host application eligibility unavailable for idle device")
        versions = [v for v, _ in profiles]
        if len({v["cpus"] for v in versions}) > 1 and resource == "cpu":
            raise ValueError("mixed CPU resource profiles require scheduler evidence")
        if resource == "gpu" and any(
                len({v["gpu_count"] for v in versions if v["gpu"] == device}) > 1
                for device in {v["gpu"] for v in versions}):
            raise ValueError("mixed GPU resource profiles require weighted matching")
        capacities = {}
        for v in versions:
            device = v["gpu"] or "cpu"
            capacity = (host["idle_gpu"].get(device, 0) / v["gpu_count"] if v["gpu"]
                        else host["idle_cpu"] / v["cpus"])
            capacities[device] = max(capacities.get(device, 0), math.floor(capacity))
        if any(count > 1024 for count in capacities.values()):
            raise ValueError("slot limit")
        prepared.append((host, profiles, capacities))
    # No stock-dependent branch can bypass the host/profile validation above.
    slots, candidates = [], {}
    for host, profiles, capacities in prepared:
        for device, count in capacities.items():
            for n in range(count):
                slot = (host["host_id"], device, n)
                slots.append(slot)
                candidates[slot] = [u["id"] for u in database["units"] if any(
                    (v["gpu"] or "cpu") == device and eligible(u, host, v, bounds) for v, bounds in profiles)]
                if sum(len(v) for v in candidates.values()) > 100000:
                    raise ValueError("eligibility graph limit")
    # Capacitated network flow, including host/unit nodes (one replica per host).
    graph, pairs = {}, set()
    bounds = {u["id"]: u["rsc_disk_bound"] for u in database["units"]}
    def edge(a, b, count):
        graph.setdefault(a, {})[b] = count
        graph.setdefault(b, {}).setdefault(a, 0)
    for host in hosts:
        host_slots = [slot for slot in slots if slot[0] == host["host_id"]]
        weights = {bounds[uid] for slot in host_slots for uid in candidates[slot]}
        limit = len(host_slots)
        if len(weights) == 1:
            weight = next(iter(weights))
            if weight:
                limit = min(limit, math.floor(host["disk_bytes"] / weight))
        edge("source", ("host", host["host_id"]), limit)
    for slot in slots:
        edge(("host", slot[0]), slot, 1)
        for uid in candidates[slot]:
            pair = ("pair", slot[0], uid)
            pairs.add(pair)
            edge(slot, pair, 1)
            edge(pair, ("unit", uid), 1)
    for unit in database["units"]:
        edge(("unit", unit["id"]), "sink", unit["unsent"])
    flow = 0
    while slots:
        parent, queue = {"source": None}, ["source"]
        for node in queue:
            for other, capacity in graph.get(node, {}).items():
                if capacity > 0 and other not in parent:
                    parent[other] = node
                    queue.append(other)
            if "sink" in parent:
                break
        if "sink" not in parent:
            break
        node = "sink"
        while parent[node] is not None:
            prev = parent[node]
            graph[prev][node] -= 1
            graph[node][prev] += 1
            node = prev
        flow += 1
    used = {h["host_id"]: 0 for h in hosts}
    for pair in pairs:
        if graph[("unit", pair[2])][pair]:
            used[pair[1]] += bounds[pair[2]]
    # A weighted optimum needs a different solver. Never label an infeasible
    # maximum match healthy, or report a greedy lower bound as a real shortage.
    if any(used[h["host_id"]] > h["disk_bytes"] for h in hosts):
        raise ValueError("maximum match exceeds cumulative host disk budget")
    if disk_usage is not None:
        disk_usage.update(used)
    return {"idle": len(slots), "eligible": flow}


def collect(config):
    locations = {"server": config["server"], "controller": config["controller"],
                 "backlog": config["backlog"]}
    clients = config["clients"]
    if not isinstance(clients, list) or not 1 <= len(clients) <= 32 or len(set(clients)) != len(clients):
        raise ValueError("configure only distinct project-owned client observations")
    locations.update({f"client_{i}": value for i, value in enumerate(clients)})
    facts, documents = {"sources": {}, "errors": []}, {}
    with ThreadPoolExecutor(max_workers=8) as executor:
        futures = {name: executor.submit(read_source, url) for name, url in locations.items()}
        wait(futures.values())
        completed = int(time.time())
        for name, future in futures.items():
            try:
                document = fresh(future.result(), completed, name.split("_")[0])
                facts["sources"][name] = {"observed_at": document["observed_at"], "state": "fresh"}
                documents[name] = document
            except (OSError, ValueError, KeyError, TypeError, http.client.HTTPException) as error:
                reason = "http_protocol_error" if isinstance(error, http.client.HTTPException) else "unavailable_or_invalid"
                facts["sources"][name] = {"observed_at": None, "state": "unknown", "reason": reason}
                facts["errors"].append(name)
    database = documents.get("server")
    hosts = [documents.get(f"client_{i}") for i in range(len(clients))]
    if database and all(hosts):
        facts["supply"] = {}
        disk = {"cpu": {}, "gpu": {}}
        for resource in ("cpu", "gpu"):
            try:
                if len({h["host_id"] for h in hosts}) != len(hosts):
                    raise ValueError("duplicate host")
                facts["supply"][resource] = supply(database, hosts, resource, disk[resource])
            except (ValueError, TypeError, KeyError, ZeroDivisionError):
                facts["errors"].append(resource + "_eligibility")
        if any(sum(used.get(h["host_id"], 0) for used in disk.values()) > h["disk_bytes"] for h in hosts):
            for resource in tuple(facts["supply"]):
                del facts["supply"][resource]
                facts["errors"].append(resource + "_disk_budget")
    # Allocation can take time too. Publish one completion timestamp, after a
    # final freshness check, rather than renewing earlier evidence's age.
    completed = int(time.time())
    for name, document in list(documents.items()):
        try:
            fresh(document, completed, name.split("_")[0])
        except ValueError:
            del documents[name]
            facts["sources"][name] = {"observed_at": None, "state": "unknown", "reason": "stale_at_completion"}
            facts["errors"].append(name)
    if "server" in documents:
        facts.update(enabled_until=documents["server"]["enabled_until"],
                     last_workunits=documents["server"]["last_workunits"])
    if "controller" in documents:
        facts["tick"] = documents["controller"]["tick"]
    if "backlog" in documents:
        facts["backlogs"] = documents["backlog"]["backlogs"]
    if "server" not in documents or any(f"client_{i}" not in documents for i in range(len(clients))):
        facts.pop("supply", None)
    facts["observed_at"] = completed
    return facts


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    sub = parser.add_subparsers(dest="source", required=True)
    p = sub.add_parser("server")
    p.add_argument("--project", type=Path, required=True)
    p.add_argument("--window-env", type=Path, required=True)
    p.add_argument("--container", help="Existing local BOINC container; PHP config and database reads run inside it")
    p.add_argument("--container-project", default="/project/usnmocr")
    p = sub.add_parser("client")
    p.add_argument("--data-dir", type=Path, required=True)
    p.add_argument("--port", type=int, default=31416)
    p = sub.add_parser("controller")
    p.add_argument("--unit", default="usnm-boinc-priority-controller.service")
    p = sub.add_parser("backlog")
    p.add_argument("--dashboard", required=True)
    p.add_argument("--requests", type=Path, required=True)
    p.add_argument("--outbox", type=Path, required=True, help="Existing Qwen campaign directory (not all apps)")
    p.add_argument("--selection", type=Path, help="Read-only mirror of the current Qwen selector's selected.jsonl")
    for p in sub.choices.values():
        p.add_argument("--output", type=Path, required=True)
        p.add_argument("--push-url", help="Existing private OCR dashboard /api/feed-source/<name>")
    args = parser.parse_args(argv)
    result = {"schema_version": 1, "source": args.source, "observed_at": int(time.time())}
    try:
        if args.source == "server":
            result.update(server(args.project, args.window_env, args.container, args.container_project))
        elif args.source == "client":
            result.update(client(args.data_dir, args.port))
        elif args.source == "controller":
            result["tick"] = controller(args.unit)
        else:
            result.update(backlog(args.dashboard, args.requests, args.outbox, args.selection))
    except Exception:
        # Source errors must replace the last success; never publish stderr, paths or credentials.
        result["error"] = "source unavailable"
    atomic_json(args.output, result)
    if args.push_url:
        request = urllib.request.Request(args.push_url, data=json.dumps(result).encode(),
                                         headers={"Content-Type": "application/json"}, method="PUT")
        try:
            with urllib.request.build_opener(NoRedirect).open(request, timeout=TIMEOUT) as response:
                if response.status != 204:
                    raise ValueError("source push rejected")
        except (OSError, ValueError, http.client.HTTPException):
            print("feed observation push unavailable", file=sys.stderr)
            return 1
    return int("error" in result)


if __name__ == "__main__":
    sys.exit(main())
