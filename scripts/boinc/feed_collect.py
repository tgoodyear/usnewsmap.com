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
import signal
import subprocess
import sys
import time
import urllib.request
import urllib.error
import xml.etree.ElementTree as ET

from feed_status import APP_NAMES, MAX_AGE, TICK_LIMIT_S, atomic_json

MAX_BYTES = 8 * 1024 * 1024
MAX_ROWS = 8000
MAX_MATCH_EDGES = 100000
MAX_PROFILE_CHECKS = 1000000
TIMEOUT = 15
NAME = re.compile(r"[a-zA-Z0-9_-]{1,80}")
SOURCE_UNKNOWN_REASONS = ("task_resource_usage_unavailable", "active_task_resource_usage_unavailable")
# An own client reporting another BOINC host than its configured binding.
BINDING_MISMATCH = "host_binding_mismatch"
# feeder-slot-guard.py prints one aggregate report and exits 0 (match, maintenance), 1 (drift) or 2 (#1462).
GUARD_STATUSES = ("match", "maintenance", "drift", "unknown")
GUARD_UNAVAILABLE = {"status": "unknown", "reason": "guard_unavailable", "app_ids": []}
SYSTEMD_FAILURE_FIELDS = {
    # UNIT_FAILURE_RESULT and UNIT_FAILED (failed start job), not free-text messages.
    "d9b373ed55a64feb8242e02dbe79a49c": "UNIT_RESULT",
    "be02cf6855d2428ba40df7e9d022f03d": "JOB_RESULT",
}


class SupplyUnknown(ValueError):
    """A bounded model cannot prove a feasible concurrent allocation."""


def read_file(path, limit=MAX_BYTES):
    with Path(path).open("rb") as stream:
        value = stream.read(limit + 1)
    if len(value) > limit:
        raise ValueError("source size limit")
    return value


def run(argv, cwd=None, limit=MAX_BYTES, exits=(0,)):
    """Bound reads and clean up the owned local process group before reaping."""
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
        # Keep the leader waitable until group cleanup; reaping it first can hide
        # surviving descendants and release its process-group identity for reuse.
        while (status := os.waitid(os.P_PID, process.pid, os.WEXITED | os.WNOHANG | os.WNOWAIT)) is None:
            left = deadline - time.monotonic()
            if left <= 0:
                raise TimeoutError("source timeout")
            time.sleep(min(.01, left))
        if status.si_code != os.CLD_EXITED or status.si_status not in exits:
            raise ValueError("source command failed")
        return bytes(output)
    finally:
        try:
            os.killpg(process.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        except PermissionError:
            # Darwin reports EPERM for zombie-only groups. The leader stays
            # waitable; no UID-changing/privileged descendants are supported.
            if sys.platform != "darwin" or os.waitid(
                    os.P_PID, process.pid, os.WEXITED | os.WNOHANG | os.WNOWAIT) is None:
                raise
        finally:
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
            or not 0 <= now - natural(document.get("observed_at")) <= MAX_AGE):
        raise ValueError("source unavailable, stale or future-dated")
    if document.get("error"):
        if kind == "client" and document["error"] in SOURCE_UNKNOWN_REASONS:
            raise SupplyUnknown(document["error"])
        raise ValueError("source unavailable")
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
        if "feeder_allocation" in document:
            guard = document["feeder_allocation"]
            if (not isinstance(guard, dict) or set(guard) != {"status", "reason", "app_ids"}
                    or guard["status"] not in GUARD_STATUSES
                    or not isinstance(guard["reason"], str) or not NAME.fullmatch(guard["reason"])
                    or not isinstance(guard["app_ids"], list) or len(guard["app_ids"]) > 768):
                raise ValueError("invalid feeder allocation")
            for app_id in guard["app_ids"]:
                natural(app_id)
    if kind == "client":
        if (natural(document["host_id"]) == 0 or not isinstance(document["versions"], list)
                or len(document["versions"]) > 256
                or any(not isinstance(version, dict) for version in document["versions"])):
            raise ValueError("invalid own host")
        if (not isinstance(document.get("gpu_memory", {}), dict)
                or not isinstance(document.get("gpu_capabilities", {}), dict)
                or any(not isinstance(caps, dict) for caps in document.get("gpu_capabilities", {}).values())):
            raise ValueError("invalid GPU capabilities")
        for field in ("idle_cpu", "available_cpu", "memory_bytes", "disk_bytes"):
            value = document.get(field, document["idle_cpu"]) if field == "available_cpu" else document[field]
            if type(value) not in (int, float) or not math.isfinite(value) or value < 0:
                raise ValueError("invalid capacity")
        if not isinstance(document["idle_gpu"], dict) or len(document["idle_gpu"]) > 8:
            raise ValueError("invalid devices")
        for field in ("idle_gpu", "gpu_devices", "used_gpu"):
            devices = document.get(field, {})
            if not isinstance(devices, dict) or len(devices) > 8:
                raise ValueError("invalid devices")
            for value in devices.values():
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
        specific = node.findall("project_specific")
        if len(specific) > 1 or specific and any(node.find(tag) is not None for tag in (
                "apps_selected", "app_id", "allow_non_preferred_apps")):
            raise ValueError("ambiguous project-specific preferences")
        app_prefs = specific[0] if specific else node
        selection = app_prefs.find("apps_selected")
        ids = app_prefs.findall("app_id") if selection is None else selection.findall("app_id")
        selected = {int(n.text) for n in ids}
        allowed = [app["name"] for app in apps if int(app["id"]) in selected] if ids or selection is not None else list(APP_NAMES)
        fallback = app_prefs.findtext("allow_non_preferred_apps", "0").strip() == "1" and set(allowed) != set(APP_NAMES)
        blocked = []
        for resource, tag in (("CPU", "no_cpu"), ("NVIDIA", "no_cuda"), ("ATI", "no_ati"),
                              ("intel_gpu", "no_intel_gpu"), ("apple_gpu", "no_apple_gpu")):
            if node.findtext(tag, "0").strip() == "1":
                blocked.append(resource)
        preferences[str(int(row["id"]))] = {"apps": allowed, "blocked": blocked, "fallback": fallback}
    return preferences


def guard_report(raw):
    """Keep the guard's status, reason and drifted app IDs; anything else is unavailable."""
    report = json.loads(raw)
    if (not isinstance(report, dict) or report.get("event") != "boinc_feeder_slot_guard"
            or type(report.get("schema_version")) is not int or report["schema_version"] != 1
            or report.get("status") not in GUARD_STATUSES
            or not isinstance(report.get("reason"), str) or not NAME.fullmatch(report["reason"])):
        raise ValueError("invalid guard report")
    ids = []
    for key in ("added_app_ids", "removed_app_ids", "weight_changed_app_ids"):
        values = report.get(key, [])
        if not isinstance(values, list) or len(values) > 256:
            raise ValueError("invalid guard app IDs")
        ids += [natural(value) for value in values]
    return {"status": report["status"], "reason": report["reason"], "app_ids": sorted(set(ids))}


def feeder_allocation(guard, project, container=None, container_project="/project/usnmocr"):
    """The server image's read-only slot guard. Its failure is unknown, not the source's."""
    command = [sys.executable, guard, "--project", str(project)]
    if container:
        command = ["podman", "exec", "--user", "boincadm", container,
                   "/usr/bin/python3", guard, "--project", container_project]
    try:
        return guard_report(run(command, limit=65536, exits=(0, 1, 2)))
    except (OSError, ValueError, TypeError, RecursionError):
        return dict(GUARD_UNAVAILABLE)


def server(project, env_file, container=None, container_project="/project/usnmocr", feeder_guard=None):
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
    data |= {"plans": plans, "enabled_until": window(env_file)}
    if feeder_guard:
        data["feeder_allocation"] = feeder_allocation(feeder_guard, project, container, container_project)
    return data


def client_resource_usage(node, default_cpu):
    """Read one complete client resource record, never merge WU and version fields."""
    try:
        coprocs = node.findall("coproc")
        if len(node.findall("avg_ncpus")) > 1 or len(coprocs) > 1:
            raise ValueError("ambiguous resource record")
        cpu = float(node.findtext("avg_ncpus", str(default_cpu)))
        gpu, count = None, 0
        if coprocs:
            coproc = coprocs[0]
            if len(coproc.findall("type")) != 1 or len(coproc.findall("count")) != 1:
                raise ValueError("incomplete GPU resource record")
            gpu = coproc.findtext("type", "").strip()
            count = float(coproc.findtext("count"))
            if not gpu or not math.isfinite(count) or count <= 0:
                raise ValueError("invalid GPU demand")
        if not math.isfinite(cpu) or cpu < 0 or not gpu and cpu == 0:
            raise ValueError("invalid CPU demand")
        return {"cpus": cpu, "gpu": gpu, "gpu_count": count}
    except (ValueError, TypeError):
        raise SupplyUnknown("task_resource_usage_unavailable") from None


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
        versions.append({"project": owner, "app": app, "version_num": int(v.findtext("version_num")),
                         "plan_class": v.findtext("plan_class", ""),
                         "platform": v.findtext("platform", state.findtext(".//platform_name")),
                         **client_resource_usage(v, 1)})
    # Count queued/download-pending work as occupied: it already feeds this device.
    used_cpu, used_memory, used_gpu = 0, 0, {}
    units = {(owner, w.findtext("name")): w for owner, w in scoped if w.tag == "workunit"}
    for owner, result in scoped:
        if result.tag != "result":
            continue
        # Stock RESULT_COMPUTE_ERROR=3, FILES_UPLOADING=4: neither occupies execution.
        if int(result.findtext("state", "0")) >= 3 or flag(result, "ready_to_report"):
            continue
        if flag(result, "suspended_via_gui"):
            raise ValueError("suspended queued task")
        active = result.findall("active_task")
        if active and (len(active) != 1 or active[0].findtext("active_task_state") != "0"):
            # Running jobs retain old reservations after app_config/scheduler
            # updates; get_state exposes only a rounded display string for them.
            raise SupplyUnknown("active_task_resource_usage_unavailable")
        if flag(result, "coproc_missing"):
            raise SupplyUnknown("task_resource_usage_unavailable")
        unit = units.get((owner, result.findtext("wu_name")))
        app = unit.findtext("app_name") if unit is not None else None
        match = [v for v in versions if v["project"] == owner and v["app"] == app
                 and v["version_num"] == int(result.findtext("version_num", "0"))
                 and v["plan_class"] == result.findtext("plan_class", "")
                 and v["platform"] == result.findtext("platform", state.findtext(".//platform_name"))]
        if len(match) != 1:
            # Unknown or other-project work must not inflate idle capacity.
            raise ValueError("unaccounted client task")
        usage = match[0]
        if unit is not None and (unit.find("avg_ncpus") is not None or unit.find("coproc") is not None):
            usage = client_resource_usage(unit, 0)
        memory_bound = unit.findtext("rsc_memory_bound") if unit is not None else None
        if memory_bound is None or not math.isfinite(float(memory_bound)) or float(memory_bound) < 0:
            raise ValueError("occupied task memory unavailable")
        used_memory += float(memory_bound)
        used_cpu += usage["cpus"]
        if usage["gpu"]:
            used_gpu[usage["gpu"]] = used_gpu.get(usage["gpu"], 0) + usage["gpu_count"]
    gpus, gpu_devices, gpu_memory, capabilities = {}, {}, {}, {}
    for node in state.findall(".//coprocs/*"):
        vendor = {"coproc_cuda": "NVIDIA", "coproc_ati": "ATI",
                  "coproc_intel_gpu": "intel_gpu", "coproc_apple_gpu": "apple_gpu"}.get(node.tag)
        if vendor:
            gpu_devices[vendor] = float(node.findtext("count", "0"))
            gpus[vendor] = max(0, gpu_devices[vendor] - used_gpu.get(vendor, 0))
            gpu_memory[vendor] = max([float(n.text or 0) for tag in
                ("available_ram", "totalGlobalMem", "global_mem_size")
                for n in node.findall(".//" + tag)] or [0])
            properties = node.findall("coproc_opencl")
            prop = properties[0] if len(properties) == 1 else None
            opencl = re.search(r"OpenCL\s+(\d+)\.(\d+)",
                               prop.findtext("opencl_device_version", "") if prop is not None else "")
            flags = node.findall("have_opencl")
            support = flags[0].text if len(flags) == 1 else None
            try:
                opencl_memory = float(prop.findtext("global_mem_size")) if prop is not None else None
            except (TypeError, ValueError):
                opencl_memory = None
            capabilities[vendor] = {
                "min_cuda_version": int(node.findtext(".//cudaVersion", "0")),
                "min_nvidia_compcap": int(node.findtext(".//major", "0")) * 100 + int(node.findtext(".//minor", "0")),
                "min_opencl_version": int(opencl[1]) * 100 + int(opencl[2]) if opencl else 0,
                "have_opencl": support == "1" if support in ("0", "1") else None,
                "opencl_memory_bytes": opencl_memory if opencl_memory is not None and math.isfinite(opencl_memory)
                and opencl_memory > 0 else None,
            }
    if any(device not in gpu_devices or gpu_devices[device] <= 0 for device in used_gpu):
        raise SupplyUnknown("task_resource_usage_unavailable")
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
            "available_cpu": max(0, cpus - used_cpu),
            "cpus": cpus, "idle_gpu": gpus, "gpu_memory": gpu_memory, "gpu_capabilities": capabilities,
            "gpu_devices": gpu_devices, "used_gpu": used_gpu,
            "client_version": sum(int(state.findtext(".//" + tag, "0")) * factor for tag, factor in (
                ("core_client_major_version", 10000), ("core_client_minor_version", 100), ("core_client_release", 1))),
            "docker": container_runtime(read_command=run)
            if any(v["plan_class"].startswith("docker") for v in monitored) else False,
            "memory_bytes": max(0, memory * fraction - used_memory),
            "disk_bytes": max(0, min(float(free), float(allowance) - used))}


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
        message_id = entry.get("MESSAGE_ID")
        field = SYSTEMD_FAILURE_FIELDS.get(message_id) if isinstance(message_id, str) else None
        if field and entry.get("UNIT") == unit:
            outcome = entry.get(field)
            if not isinstance(outcome, str) or not outcome or outcome in ("success", "done"):
                raise ValueError("invalid native controller failure evidence")
            last_failure = max(last_failure, int(entry["__REALTIME_TIMESTAMP"]) // 1000000)
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


def gpu_task_memory(plan):
    return (float(plan.get("gpu_ram_used_mb", 0)) or float(plan.get("min_gpu_ram_mb", 0))) * 1e6


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
        capabilities = host.get("gpu_capabilities", {}).get(gpu, {})
        expected = {"nvidia": "NVIDIA", "ati": "ATI", "amd": "ATI", "intel": "intel_gpu", "apple": "apple_gpu"}
        if not plan.get("gpu_type"):
            raise SupplyUnknown("live_resource_type_mismatch")
        if expected.get(plan["gpu_type"]) != gpu:
            return None
        if "opencl" in plan:
            if capabilities.get("have_opencl") is not True:
                raise SupplyUnknown("opencl_support_unavailable")
            memory = capabilities.get("opencl_memory_bytes")
            if type(memory) not in (int, float) or not math.isfinite(memory) or memory <= 0:
                raise SupplyUnknown("opencl_memory_unavailable")
            if numbers["min_gpu_ram_mb"] * 1e6 > memory:
                return None
            if gpu_task_memory(numbers) and memory < host.get("gpu_memory", {}).get(gpu, 0):
                raise SupplyUnknown("opencl_memory_profile_unavailable")
        if (numbers["ngpus"] or 1) != version["gpu_count"]:
            raise ValueError("live GPU policy differs from installed resource profile")
        # Without an explicit CPU demand the GPU profile depends on host performance
        # data this collector does not model; a cached value is not current evidence.
        if not numbers["avg_ncpus"]:
            raise ValueError("unmodeled GPU CPU demand")
        if numbers["min_gpu_ram_mb"] * 1e6 > host.get("gpu_memory", {}).get(gpu, 0):
            return None
        if gpu_task_memory(numbers) and (
                host.get("gpu_devices", {}).get(gpu) != 1
                or host.get("used_gpu", {}).get(gpu, 0) != 0
                or not math.isfinite(host.get("gpu_memory", {}).get(gpu, 0))
                or host.get("gpu_memory", {}).get(gpu, 0) <= 0):
            raise SupplyUnknown("gpu_memory_packing")
        for gate in ("min_cuda_version", "min_nvidia_compcap", "min_opencl_version"):
            if integers[gate] > capabilities.get(gate, 0):
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


def supply(database, hosts, resource, resource_usage=None):
    """Maximum matching: a replica and a host/workunit pair can each be used once.

    Work offered to two classes is not two results, and two replicas of a unit
    cannot fill two slots on the same host.
    """
    prepared, host_limits, host_costs = [], {}, {}
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
        for device, count in capacities.items():
            demands = {gpu_task_memory(database["plans"].get(v["plan_class"], {}))
                       for v in versions if v["gpu"] == device}
            if len(demands) > 1:
                raise SupplyUnknown("ambiguous_resource_profile")
            if demands and (demand := next(iter(demands))):
                capacities[device] = min(count, math.floor(host["gpu_memory"][device] / demand))
        limit = sum(capacities.values())
        demands = {v["cpus"] for v in versions}
        cpu = host.get("available_cpu", host["idle_cpu"])
        if len(demands) == 1:
            limit = min(limit, math.floor(cpu / next(iter(demands))))
        elif sum(count * max(v["cpus"] for v in versions if (v["gpu"] or "cpu") == device)
                 for device, count in capacities.items()) > cpu:
            raise SupplyUnknown("mixed_resource_packing")
        host_limits[host["host_id"]] = limit
        host_costs[host["host_id"]] = {"cpu": next(iter(demands)) if len(demands) == 1 else None}
        prepared.append((host, profiles, capacities))
    # No stock-dependent branch can bypass the host/profile validation above.
    slots, candidates = [], {}
    edges, profile_checks = 0, 0
    for host, profiles, capacities in prepared:
        for device, count in capacities.items():
            for n in range(count):
                if len(slots) >= MAX_MATCH_EDGES:
                    raise SupplyUnknown("candidate_slot_budget")
                slot = (host["host_id"], device, n)
                slots.append(slot)
                candidates[slot] = {}
                for unit in database["units"]:
                    demands = []
                    for v, bounds in profiles:
                        if (v["gpu"] or "cpu") != device:
                            continue
                        if profile_checks >= MAX_PROFILE_CHECKS:
                            raise SupplyUnknown("candidate_scan_budget")
                        profile_checks += 1
                        if eligible(unit, host, v, bounds):
                            plan = database["plans"].get(v["plan_class"], {})
                            demands.append((v["cpus"], gpu_task_memory(plan) if v["gpu"] else 0))
                    if demands:
                        if len(set(demands)) != 1:
                            raise SupplyUnknown("ambiguous_resource_profile")
                        if edges >= MAX_MATCH_EDGES:
                            raise SupplyUnknown("candidate_edge_budget")
                        edges += 1
                        candidates[slot][unit["id"]] = demands[0]
    # Capacitated network flow, including host/unit nodes (one replica per host).
    graph, host_capacity = {}, {}
    units = {u["id"]: u for u in database["units"]}
    def edge(a, b, count):
        graph.setdefault(a, {})[b] = count
        graph.setdefault(b, {}).setdefault(a, 0)
    for host in hosts:
        host_slots = [slot for slot in slots if slot[0] == host["host_id"]]
        limit = host_limits.get(host["host_id"], 0)
        costs = host_costs.setdefault(host["host_id"], {"cpu": None})
        for field, budget in (("rsc_disk_bound", "disk_bytes"), ("rsc_memory_bound", "memory_bytes")):
            weights = {units[uid][field] for slot in host_slots for uid in candidates[slot]}
            costs["disk" if field == "rsc_disk_bound" else "memory"] = next(iter(weights)) if len(weights) == 1 else None
            if len(weights) == 1 and (weight := next(iter(weights))):
                limit = min(limit, math.floor(host[budget] / weight))
        demands = {cost[0] for slot in host_slots for cost in candidates[slot].values()}
        if len(demands) == 1:
            limit = min(limit, math.floor(host.get("available_cpu", host["idle_cpu"]) / next(iter(demands))))
        host_capacity[host["host_id"]] = limit
        edge("source", ("host", host["host_id"]), limit)
        for device in {slot[1] for slot in host_slots}:
            device_slots = [slot for slot in host_slots if slot[1] == device]
            limit = len(device_slots)
            demands = {cost[1] for slot in device_slots for cost in candidates[slot].values()}
            if len(demands) == 1 and (demand := next(iter(demands))):
                limit = min(limit, math.floor(host["gpu_memory"][device] / demand))
            edge(("host", host["host_id"]), ("device", host["host_id"], device), limit)
    for slot in slots:
        edge(("device", slot[0], slot[1]), slot, 1)
        for uid in candidates[slot]:
            pair = ("pair", slot[0], uid)
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
    used = {h["host_id"]: {"cpu": 0, "memory": 0, "disk": 0, "gpu_memory": {},
                          "capacity": host_capacity[h["host_id"]], "costs": host_costs[h["host_id"]],
                          "allocated": 0} for h in hosts}
    for slot in slots:
        usage = used[slot[0]]
        for uid, (cpu, gpu_memory) in candidates[slot].items():
            if graph[("pair", slot[0], uid)][slot]:
                usage["allocated"] += 1
                usage["cpu"] += cpu
                usage["memory"] += units[uid]["rsc_memory_bound"]
                usage["disk"] += units[uid]["rsc_disk_bound"]
                usage["gpu_memory"][slot[1]] = usage["gpu_memory"].get(slot[1], 0) + gpu_memory
    # A weighted optimum needs a different solver. Never label an infeasible
    # maximum match healthy, or report a greedy lower bound as a real shortage.
    if any(used[h["host_id"]]["disk"] > h["disk_bytes"] for h in hosts):
        raise ValueError("maximum match exceeds cumulative host disk budget")
    for host in hosts:
        usage = used[host["host_id"]]
        if usage["cpu"] > host.get("available_cpu", host["idle_cpu"]) or usage["memory"] > host["memory_bytes"]:
            raise SupplyUnknown("mixed_resource_packing")
        if any(amount > host.get("gpu_memory", {}).get(device, 0)
               for device, amount in usage["gpu_memory"].items()):
            raise SupplyUnknown("mixed_gpu_memory_packing")
    if resource_usage is not None:
        resource_usage.update(used)
    return {"idle": sum(host_capacity.values()), "eligible": flow}


def own_clients(config):
    """The allowlist of own-client observations: {source name: (location, expected host id or None)}.

    Empty by default. An entry is a location, or {"location": ..., "host_id": N} once its
    binding to that BOINC host has been verified; volunteer hosts never belong here.
    """
    clients = config.get("clients", [])
    if not isinstance(clients, list) or len(clients) > 32:
        raise ValueError("configure at most 32 project-owned client observations")
    allowed = {}
    for i, entry in enumerate(clients):
        if isinstance(entry, dict) and set(entry) == {"location", "host_id"}:
            location, host_id = entry["location"], natural(entry["host_id"])
            if host_id == 0:
                raise ValueError("invalid client binding")
        else:
            location, host_id = entry, None
        if not isinstance(location, str) or not location:
            raise ValueError("invalid client location")
        allowed[f"client_{i}"] = (location, host_id)
    bound = [host_id for _, host_id in allowed.values() if host_id is not None]
    if (len({location for location, _ in allowed.values()}) != len(allowed)
            or len(set(bound)) != len(bound)):
        raise ValueError("configure only distinct project-owned client observations")
    return allowed


def collect(config):
    # The feeder allocation check is opt-in, so a config without it keeps today's checks.
    guarded = config.get("feeder_allocation", False)
    if type(guarded) is not bool:
        raise ValueError("feeder_allocation must be true or false")
    locations = {"server": config["server"], "controller": config["controller"]}
    # The backlog source is optional until it is deployed: unknown, never healthy.
    if config.get("backlog") is not None:
        locations["backlog"] = config["backlog"]
    allowed = own_clients(config)
    clients = list(allowed)
    locations.update({name: location for name, (location, _) in allowed.items()})
    facts, documents = {"sources": {}, "errors": []}, {}
    if "backlog" not in locations:
        facts["sources"]["backlog"] = {"observed_at": None, "state": "unknown", "reason": "not_configured"}
        facts["errors"].append("backlog")
    with ThreadPoolExecutor(max_workers=8) as executor:
        futures = {name: executor.submit(read_source, url) for name, url in locations.items()}
        wait(futures.values())
        completed = int(time.time())
        for name, future in futures.items():
            try:
                document = fresh(future.result(), completed, name.split("_")[0])
                expected = allowed.get(name, (None, None))[1]
                if expected is not None and document["host_id"] != expected:
                    raise SupplyUnknown(BINDING_MISMATCH)
                facts["sources"][name] = {"observed_at": document["observed_at"], "state": "fresh"}
                documents[name] = document
            except (OSError, ValueError, KeyError, TypeError, http.client.HTTPException) as error:
                if isinstance(error, SupplyUnknown) and str(error) in (*SOURCE_UNKNOWN_REASONS, BINDING_MISMATCH):
                    reason = str(error)
                else:
                    reason = "http_protocol_error" if isinstance(error, http.client.HTTPException) else "unavailable_or_invalid"
                facts["sources"][name] = {"observed_at": None, "state": "unknown", "reason": reason}
                facts["errors"].append(name)
    database = documents.get("server")
    hosts = [documents.get(name) for name in clients]
    if not clients:
        # No own capacity to judge is unknown, not zero idle slots.
        facts["supply_errors"] = {resource: "no_own_clients_configured" for resource in ("cpu", "gpu")}
        facts["errors"] += ["cpu_no_own_clients_configured", "gpu_no_own_clients_configured"]
    elif database and all(hosts):
        facts["supply"] = {}
        usage = {"cpu": {}, "gpu": {}}
        for resource in ("cpu", "gpu"):
            try:
                if len({h["host_id"] for h in hosts}) != len(hosts):
                    raise ValueError("duplicate host")
                facts["supply"][resource] = supply(database, hosts, resource, usage[resource])
            except SupplyUnknown as error:
                facts.setdefault("supply_errors", {})[resource] = str(error)
                facts["errors"].append(resource + "_" + str(error))
            except (ValueError, TypeError, KeyError, ZeroDivisionError):
                facts["errors"].append(resource + "_eligibility")
        for field, budget in (("disk", "disk_bytes"), ("cpu", "idle_cpu"), ("memory", "memory_bytes")):
            if any(sum(u.get(h["host_id"], {}).get(field, 0) for u in usage.values()) >
                   (h.get("available_cpu", h[budget]) if field == "cpu" else h[budget]) for h in hosts):
                for resource in tuple(facts["supply"]):
                    del facts["supply"][resource]
                    reason = "shared_" + field + "_budget"
                    facts.setdefault("supply_errors", {})[resource] = reason
                    facts["errors"].append(resource + "_" + reason)
        for resource, other in (("cpu", "gpu"), ("gpu", "cpu")):
            current = facts["supply"].get(resource)
            if not current:
                continue
            capacity, unknown = 0, False
            for host in hosts:
                used = usage[resource][host["host_id"]]
                other_used = usage[other].get(host["host_id"], {})
                limit, missing = used["capacity"], False
                for field, budget in (("cpu", "idle_cpu"), ("memory", "memory_bytes"), ("disk", "disk_bytes")):
                    consumed = other_used.get(field, 0)
                    if not consumed:
                        continue
                    cost = used["costs"][field]
                    available = host.get("available_cpu", host[budget]) if field == "cpu" else host[budget]
                    if cost is None:
                        missing = True
                    elif cost:
                        limit = min(limit, math.floor(max(0, available - consumed) / cost))
                # Preserve the already-proven allocation; unknown applies only
                # to unproven extra headroom, not to independent healthy lanes.
                unknown |= limit < used["allocated"] or missing and limit > used["allocated"]
                capacity += limit
            if unknown:
                del facts["supply"][resource]
                reason = "shared_capacity_unproven"
                facts.setdefault("supply_errors", {})[resource] = reason
                facts["errors"].append(resource + "_" + reason)
            else:
                current["idle"] = capacity
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
    if guarded:
        reason = "not_reported" if "server" in documents else "server_unavailable"
        facts["feeder_allocation"] = documents.get("server", {}).get(
            "feeder_allocation", {"status": "unknown", "reason": reason, "app_ids": []})
    if "controller" in documents:
        facts["tick"] = documents["controller"]["tick"]
    if "backlog" in documents:
        facts["backlogs"] = documents["backlog"]["backlogs"]
    if "server" not in documents or not clients or any(name not in documents for name in clients):
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
    p.add_argument("--feeder-guard", help="The image's feeder-slot-guard.py, run read-only (as boincadm in "
                   "--container) and reported as feeder_allocation")
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
            result.update(server(args.project, args.window_env, args.container, args.container_project,
                                 args.feeder_guard))
        elif args.source == "client":
            result.update(client(args.data_dir, args.port))
        elif args.source == "controller":
            result["tick"] = controller(args.unit)
        else:
            result.update(backlog(args.dashboard, args.requests, args.outbox, args.selection))
    except SupplyUnknown as error:
        result["error"] = str(error) if str(error) in SOURCE_UNKNOWN_REASONS else "source unavailable"
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
