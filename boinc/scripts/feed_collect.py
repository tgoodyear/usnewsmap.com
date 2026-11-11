#!/usr/bin/env python3
"""Bounded read-only BOINC observations. No admission, dispatch, or feed ownership.

Run source collectors beside their existing services. The evaluator reads their
atomic files (or existing private HTTP mirrors), never invokes a remote shell.
"""
import argparse
import contextlib
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

from feed_status import APP_NAMES, MAX_AGE, NO_PROVEN_CLIENTS, TICK_FAILURE_SAME_S, TICK_LIMIT_S, atomic_json

MAX_BYTES = 8 * 1024 * 1024
MAX_ROWS = 8000
MAX_MATCH_EDGES = 100000
MAX_PROFILE_CHECKS = 1000000
TIMEOUT = 15
# The server source's database inventory: four reads of at most 5 s each (the MAX_EXECUTION_TIME
# hints in PHP), plus the container exec and PHP start-up. Under memory pressure on the server
# host the old 2 s statement limit failed many minutes (11 Nov 2026), so the inventory gets its
# own deadline.
INVENTORY_TIMEOUT = 25
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


# Why the server source failed, as one fixed code naming the stage, never error text. The
# PHP inventory reports its own failures (PHP_FAILURES) on its last output line and exits 3.
PHP_FAILURES = ("scheduler_config_invalid", "one_result_per_host_missing", "database_unavailable",
                "database_query_timeout", "database_query_failed", "database_row_limit", "config_changed",
                "inventory_php_error")
PHP_FAILURE_EXIT = 3
SERVER_FAILURES = PHP_FAILURES + (
    "policy_unreadable",         # plan_class_spec.xml can't be read
    "policy_invalid",            # duplicate, nested or unnamed plan classes
    "policy_changed",            # plan_class_spec.xml changed during the observation
    "inventory_timeout",         # the container exec or PHP ran past INVENTORY_TIMEOUT
    "inventory_command_failed",  # podman or php failed without a report
    "inventory_invalid",         # output that isn't the expected inventory
    "preferences_invalid",       # an account's project preferences can't be read
    "window_invalid",            # no single ENABLED_UNTIL in the cohort env file
)


class SourceFailed(ValueError):
    """A source failure with a fixed reason code from SERVER_FAILURES."""

    def __init__(self, reason):
        super().__init__(reason)
        self.reason = reason


@contextlib.contextmanager
def stage(reason):
    """Any failure inside is `reason`; one already classified keeps its own."""
    try:
        yield
    except SourceFailed:
        raise
    except Exception:
        raise SourceFailed(reason) from None


# The BOINC database's backups and replica (db-backup.md, db-replica.md): each part of the
# dbbackup source reports its own failure as one fixed code, so one part down makes only its
# own check unknown.
DBBACKUP_FAILURES = (
    "replica_unavailable",   # the replica's MySQL can't be reached or queried
    "replica_not_replica",   # it answers but has no replication channel (promoted, or never set up)
    "r2_unavailable",        # the bucket can't be listed
    "drill_missing",         # no drill result yet
    "drill_unreadable",      # a drill result that isn't the expected record
)
# The R2 listing is cached this long, so a source run each minute lists the bucket every five.
R2_CHECK_EVERY_S = 300
# An R2 observation older than this is not evidence.
R2_MAX_AGE_S = 900
DUMP_KEY = re.compile(r"dump/[0-9]{4}/[0-9]{8}T[0-9]{6}Z\.json")
BINLOG_KEY = re.compile(r"[A-Za-z0-9_-]+\.[0-9]{6}\.gz")


# The controller source's only error a planned rotation may excuse (#1687): no tick finished in
# the last 15 minutes, its timer stopped or a tick running by hand. Every other controller
# failure (transport, a malformed or stale document) keeps its own reason.
NO_RECENT_TICK = "no_recent_controller_tick"


# A failed tick's error as one fixed class (#1698), first match wins. The health output carries
# the class and how many runs in a row failed with the same error, never the text: errors can
# name pages and jobs (the health output carries no transcripts).
FAILURE_CLASSES = (
    ("ownership_conflict", r"already allocated to another plan|another plan's journal"
                           r"|already in this plan with a different message"),
    ("cap_reached", r"dispatch inventory limit|too many claimed pages|spool entries"),
    ("uncertain_submission", r"uncertain (BOINC|server) submission"),
    ("terminal_failure", r"terminal BOINC failure"),
    ("held_result", r"\bheld\b"),
    ("lock_busy", r"^BlockingIOError$|another controller may be active"),
    ("status_fetch", r"server status|BOINC status|server_status"),
    ("handoff_failed", r"hand-off storage|hand-off JSON"),
    ("step_failed", r"^(bridge|handoff)/[a-z0-9-]+ failed|cached scan push failed|batch journal inventory failed"),
)
FAILURE_REASONS = tuple(name for name, _ in FAILURE_CLASSES) + ("other",)
# The controller's own warnings and tick fields (#1698), reported beside the tick.
SIGNAL_FIELDS = ("caps", "ownership_conflicts", "progress", "tick_p90_s")
PROGRESS_FIELDS = ("published_at", "first_tick_at", "pending")
TICK_TREND_WINDOW_S = 3600
MAX_CAPS = 16


def failure_reason(error):
    """The fixed class of a boinc_batch_failure error; anything unmatched is "other"."""
    if not isinstance(error, str):
        return "other"
    for name, pattern in FAILURE_CLASSES:
        if re.search(pattern, error):
            return name
    return "other"


def same_error(error):
    """An error with its numbers and ids masked, so a repeat naming another page still matches."""
    return re.sub(r"[0-9a-f]{8,}|\d+", "#", error)[:500] if isinstance(error, str) else None


def percentile(values, fraction):
    """Nearest-rank percentile of a non-empty list."""
    ordered = sorted(values)
    return ordered[max(0, math.ceil(fraction * len(ordered)) - 1)]


class NoRecentTick(SupplyUnknown):
    def __init__(self):
        super().__init__("no recent completed controller tick")


def read_file(path, limit=MAX_BYTES):
    with Path(path).open("rb") as stream:
        value = stream.read(limit + 1)
    if len(value) > limit:
        raise ValueError("source size limit")
    return value


def run(argv, cwd=None, limit=MAX_BYTES, exits=(0,), timeout=None, env=None):
    """Bound reads and clean up the owned local process group before reaping."""
    process = subprocess.Popen(argv, cwd=cwd, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
                               start_new_session=True, env=env)
    output = bytearray()
    try:
        with selectors.DefaultSelector() as selector:
            selector.register(process.stdout, selectors.EVENT_READ)
            deadline = time.monotonic() + (TIMEOUT if timeout is None else timeout)
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


def validate_cohort(record, now):
    """Shape of the server's cohort record; a bad one is a bad server source, as for any field."""
    if not isinstance(record, dict):
        raise ValueError("invalid cohort record")
    if "error" in record:
        if set(record) != {"error"} or record["error"] != "cohort_unavailable":
            raise ValueError("invalid cohort record")
        return
    if (set(record) != {"result", "timer_active", "last_completed_at", "last_failure_at", "failure_streak",
                        "failing_since", "report", "report_at"}
            or record["result"] not in ("running", "ok", "failed") or type(record["timer_active"]) is not bool):
        raise ValueError("invalid cohort record")
    natural(record["failure_streak"])
    if (record["failing_since"] is None) != (record["failure_streak"] == 0):
        raise ValueError("invalid cohort failure streak")
    for key in ("last_completed_at", "report_at", "failing_since"):
        if record[key] is not None and natural(record[key]) > now:
            raise ValueError("future cohort evidence")
    if natural(record["last_failure_at"]) > now:
        raise ValueError("future cohort evidence")
    report = record["report"]
    if report is None:
        return
    if (not isinstance(report, dict) or set(report) != {*COHORT_REPORT_FIELDS, "admissible_backlog"}
            or record["report_at"] is None):
        raise ValueError("invalid cohort report")
    if not isinstance(report["status"], str) or not NAME.fullmatch(report["status"]):
        raise ValueError("invalid cohort status")
    for key in ("admitted_unique", "limit"):
        natural(report[key])
    for key in ("fresh_pairs_in_scan", "currently_eligible_unsent_replicas", "per_host_buffer_limit",
                "admissible_backlog"):
        if report[key] is not None:
            natural(report[key])
    inflight = report["native_gpu_inflight"]
    if inflight is not None and (not isinstance(inflight, dict) or len(inflight) > 8):
        raise ValueError("invalid cohort inflight")
    for value in (inflight or {}).values():
        natural(value)


def validate_signals(tick, now):
    """The controller's own signals (#1698), all or none."""
    caps, progress, p90 = tick["caps"], tick["progress"], tick["tick_p90_s"]
    if not isinstance(caps, dict) or len(caps) > MAX_CAPS:
        raise ValueError("invalid controller caps")
    for name, cap in caps.items():
        if (not isinstance(name, str) or not NAME.fullmatch(name) or not isinstance(cap, dict)
                or set(cap) != {"count", "limit"} or natural(cap["limit"]) == 0):
            raise ValueError("invalid controller cap")
        natural(cap["count"])
    if tick["ownership_conflicts"] is not None:
        natural(tick["ownership_conflicts"])
    if not isinstance(progress, dict) or set(progress) != set(PROGRESS_FIELDS):
        raise ValueError("invalid controller progress")
    for field in PROGRESS_FIELDS:
        if progress[field] is not None and natural(progress[field]) > now and field != "pending":
            raise ValueError("future controller progress")
    if p90 is not None and (type(p90) not in (int, float) or not math.isfinite(p90) or p90 < 0):
        raise ValueError("invalid controller tick trend")


def validate_dbbackup(document, now):
    """Each part is its record or {"error": <a DBBACKUP_FAILURES code>}. An R2 observation older
    than R2_MAX_AGE_S becomes r2_unavailable: a cache that stopped refreshing proves nothing."""
    fields = {"replica": ("io_running", "sql_running", "lag_s"),
              "r2": ("newest_dump_at", "newest_binlog_at", "checked_at"),
              "drill": ("pass", "finished_at")}
    for part, keys in fields.items():
        record = document[part]
        if not isinstance(record, dict):
            raise ValueError("invalid dbbackup part")
        if "error" in record:
            if set(record) != {"error"} or record["error"] not in DBBACKUP_FAILURES:
                raise ValueError("invalid dbbackup failure")
            continue
        if set(record) != set(keys):
            raise ValueError("invalid dbbackup record")
    replica = document["replica"]
    if "error" not in replica:
        if type(replica["io_running"]) is not bool or type(replica["sql_running"]) is not bool:
            raise ValueError("invalid replica threads")
        if replica["lag_s"] is not None:
            natural(replica["lag_s"])
    r2 = document["r2"]
    if "error" not in r2:
        for key in ("newest_dump_at", "newest_binlog_at"):
            if r2[key] is not None and natural(r2[key]) > now + 300:
                raise ValueError("future backup object")
        if not 0 <= now - natural(r2["checked_at"]) <= R2_MAX_AGE_S:
            document["r2"] = {"error": "r2_unavailable"}
    drill = document["drill"]
    if "error" not in drill:
        if type(drill["pass"]) is not bool or natural(drill["finished_at"]) > now + 300:
            raise ValueError("invalid drill record")


def fresh(document, now, kind):
    if not isinstance(document, dict):
        raise ValueError("source must be an object")
    if (type(document.get("schema_version")) is not int or document.get("schema_version") != 1 or document.get("source") != kind
            or not 0 <= now - natural(document.get("observed_at")) <= MAX_AGE):
        raise ValueError("source unavailable, stale or future-dated")
    if document.get("error"):
        if kind == "client" and document["error"] in SOURCE_UNKNOWN_REASONS:
            raise SupplyUnknown(document["error"])
        if kind == "controller" and document["error"] == NO_RECENT_TICK:
            raise SupplyUnknown(NO_RECENT_TICK)
        if kind == "server" and document["error"] in SERVER_FAILURES:
            raise SourceFailed(document["error"])
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
        if any(natural(tick.get(key, 0)) > now for key in (
                "last_failure_at", "controller_failure_at", "maintenance_failure_at")):
            raise ValueError("future controller failure")
        if "failed_in_a_row" in tick:
            # The evaluator reads both together.
            natural(tick["failed_in_a_row"])
            natural(tick.get("maintenance_failure_at"))
        if "failed_last_hour" in tick:
            natural(tick["failed_last_hour"])
            if tick.get("last_hour_reason") is not None and tick["last_hour_reason"] not in FAILURE_REASONS:
                raise ValueError("invalid controller failure reason")
        if "reason" in tick or "same_error_in_a_row" in tick:
            # Read together (#1698): a class from the fixed list, never error text.
            if tick.get("reason") is not None and tick.get("reason") not in FAILURE_REASONS:
                raise ValueError("invalid controller failure reason")
            natural(tick.get("same_error_in_a_row"))
        if any(field in tick for field in SIGNAL_FIELDS):
            validate_signals(tick, now)
    if kind == "dbbackup":
        validate_dbbackup(document, now)
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
        if "cohort" in document:
            # An unusable cohort record makes only the Qwen check unknown, never the server source.
            try:
                validate_cohort(document["cohort"], now)
            except (ValueError, KeyError, TypeError):
                document["cohort"] = {"error": "cohort_unavailable"}
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
        if ("capacity_unproven" in document or "unproven" in document) and (
                document.get("capacity_unproven") is not True or document.get("unproven") not in SOURCE_UNKNOWN_REASONS):
            # Both or neither (#1698): a host whose running tasks can't be counted.
            raise ValueError("invalid unproven capacity")
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
// A failure is one fixed code on the last output line (feed_collect.PHP_FAILURES), never its text.
function feed_fail($code) {
    echo "\n", json_encode(['feed_failure' => $code]), "\n";
    exit(3);
}
// ER_QUERY_TIMEOUT (3024): a statement ran past its MAX_EXECUTION_TIME hint.
function feed_db_code($errno) {
    return intval($errno) === 3024 ? 'database_query_timeout' : 'database_query_failed';
}
// php -r never calls set_exception_handler for a top-level throw, so the body is one try.
try {
function feed_config() {
    $raw = @file_get_contents('../../config.xml', false, null, 0, 8388609);
    if ($raw === false || strlen($raw) > 8388608 || preg_match('/<!DOCTYPE|<!ENTITY/i', $raw)) {
        throw new Exception('scheduler_config_invalid');
    }
    return $raw;
}
$feed_config_before = feed_config();
libxml_use_internal_errors(true);
$feed_config_xml = simplexml_load_string($feed_config_before, 'SimpleXMLElement', LIBXML_NONET);
libxml_clear_errors();
if ($feed_config_xml === false) throw new Exception('scheduler_config_invalid');
// Match native_qwen_one_result_per_host's raw tokens without deploying that operator helper.
$feed_flags = $feed_config_xml->xpath('/boinc/config/one_result_per_host_per_wu');
if (count($feed_flags) !== 1 ||
    count($feed_config_xml->xpath('//one_result_per_host_per_wu')) !== 1 ||
    preg_match_all('/<one_result_per_host_per_wu[\s>\/]/', $feed_config_before) !== 1 ||
    preg_match_all('/<config[\s>\/]/', $feed_config_before) !== 1 ||
    preg_match('/<config>(?:(?!<\/config>).)*(?:<one_result_per_host_per_wu\s*\/>|'
        .'<one_result_per_host_per_wu>\s*1\s*<\/one_result_per_host_per_wu>)/s', $feed_config_before) !== 1) {
    throw new Exception('one_result_per_host_missing');
}
require_once 'boinc_db.inc';
// get() prints an error page and exits when it can't connect; get_aux() leaves a null instance.
BoincDb::get_aux(1);
$db = BoincDb::$instance;
if (!$db) throw new Exception('database_unavailable');
function rows($sql) {
    global $db;
    $r = $db->do_query($sql);
    if (!$r) throw new Exception(feed_db_code(method_exists($db, 'base_errno') ? $db->base_errno() : 0));
    $out = [];
    while ($row = $r->fetch_assoc()) {
        if (count($out) >= 8000) throw new Exception('database_row_limit');
        $out[] = $row;
    }
    return $out;
}
if (!$db->do_query('SET SESSION TRANSACTION READ ONLY') ||
    !$db->do_query('START TRANSACTION WITH CONSISTENT SNAPSHOT')) throw new Exception('database_unavailable');
$apps = rows("SELECT /*+ MAX_EXECUTION_TIME(5000) */ a.name,a.id,a.min_version,
    COALESCE(MAX(w.create_time),0) AS last_workunit
    FROM app a LEFT JOIN workunit w ON a.id=w.appid
    WHERE a.name IN ('usnm_ocr','usnm_qwen') GROUP BY a.id");
$versions = rows("SELECT /*+ MAX_EXECUTION_TIME(5000) */ a.name AS app,
    av.id,av.version_num,av.plan_class,p.name AS platform
    FROM app_version av JOIN app a ON a.id=av.appid JOIN platform p ON p.id=av.platformid
    WHERE av.deprecated=0 AND a.name IN ('usnm_ocr','usnm_qwen')");
$preferences = rows("SELECT /*+ MAX_EXECUTION_TIME(5000) */ h.id,h.venue,u.project_prefs
    FROM host h JOIN user u ON u.id=h.userid LIMIT 8001");
$units = rows("SELECT /*+ MAX_EXECUTION_TIME(5000) */ a.name AS app,w.id,w.batch,
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
if ($feed_config_before !== feed_config()) throw new Exception('config_changed');
echo json_encode(['apps'=>$apps,'versions'=>$versions,'units'=>$units,'preferences'=>$preferences], JSON_THROW_ON_ERROR);
} catch (Throwable $e) {
    // PHP 8.1+ mysqli throws on a failed query (its code is the MySQL error number) rather
    // than do_query() returning false.
    if ($e instanceof mysqli_sql_exception) feed_fail(feed_db_code($e->getCode()));
    $codes = ['scheduler_config_invalid', 'one_result_per_host_missing', 'database_unavailable',
              'database_query_timeout', 'database_query_failed', 'database_row_limit', 'config_changed'];
    feed_fail(in_array($e->getMessage(), $codes, true) ? $e->getMessage() : 'inventory_php_error');
}
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


def inventory(project, container=None, container_project="/project/usnmocr"):
    """The PHP database inventory, or SourceFailed with the stage that failed."""
    command = ["php", "-r", PHP]
    if container:
        command = ["podman", "exec", "--workdir", container_project + "/html/inc", container, *command]
    try:
        output = run(command, cwd=None if container else project / "html/inc",
                     exits=(0, PHP_FAILURE_EXIT), timeout=INVENTORY_TIMEOUT)
    except TimeoutError:
        raise SourceFailed("inventory_timeout") from None
    except Exception:
        raise SourceFailed("inventory_command_failed") from None
    # BOINC's do_query echoes a line of its own before a failure report, so read the last line.
    lines = output.strip().splitlines()
    try:
        report = json.loads(lines[-1]) if lines else None
    except ValueError:
        report = None
    if isinstance(report, dict) and set(report) == {"feed_failure"}:
        raise SourceFailed(report["feed_failure"] if report["feed_failure"] in PHP_FAILURES
                           else "inventory_php_error")
    with stage("inventory_invalid"):
        data = json.loads(output)
        if not isinstance(data, dict) or not all(isinstance(data.get(key), list)
                                                 for key in ("apps", "versions", "units", "preferences")):
            raise ValueError("invalid inventory")
    return data


def server(project, env_file, container=None, container_project="/project/usnmocr", feeder_guard=None,
           cohort_unit=None, cohort_timer=None):
    # PHP validates config in its own namespace; only inventory crosses the container boundary.
    # Each stage's failure is its own fixed code (SERVER_FAILURES), so a failing minute says where.
    with stage("policy_unreadable"):
        before = read_file(project / "plan_class_spec.xml")
    data = inventory(project, container, container_project)
    with stage("policy_unreadable"):
        after = read_file(project / "plan_class_spec.xml")
    if before != after:
        raise SourceFailed("policy_changed")
    plans = {}
    with stage("policy_invalid"):
        for node in xml(before).findall("plan_class"):
            name = node.findtext("name")
            if not name or name in plans:
                raise ValueError("invalid plan classes")
            plans[name] = {child.tag: child.text or "" for child in node}
            if len(plans[name]) != len(node) or any(len(child) for child in node):
                raise ValueError("ambiguous or unsupported policy")
    with stage("inventory_invalid"):
        for row in data["units"]:
            row["hosts"] = list(set(map(int, json.loads(row["hosts"])))) if row["hosts"] else []
            for field in ("id", "batch", "app_version_num", "hr_class", "unsent", "assignments"):
                row[field] = int(row[field])
            for field in ("rsc_memory_bound", "rsc_disk_bound"):
                row[field] = float(row[field])
        for row in data["versions"]:
            row["id"], row["version_num"] = int(row["id"]), int(row["version_num"])
        preferences, apps = data.pop("preferences"), data["apps"]
    with stage("preferences_invalid"):
        data["host_preferences"] = host_preferences(preferences, apps)
    with stage("inventory_invalid"):
        data["last_workunits"] = {r["name"]: int(r["last_workunit"]) for r in data["apps"]}
        data["minimum_versions"] = {r["name"]: int(r["min_version"]) for r in data.pop("apps")}
    with stage("window_invalid"):
        enabled_until = window(env_file)
    data |= {"plans": plans, "enabled_until": enabled_until}
    if feeder_guard:
        data["feeder_allocation"] = feeder_allocation(feeder_guard, project, container, container_project)
    if cohort_unit:
        data["cohort"] = cohort(cohort_unit, cohort_timer or cohort_unit.rsplit(".", 1)[0] + ".timer")
    return data


COHORT_REPORT_FIELDS = ("status", "admitted_unique", "limit", "fresh_pairs_in_scan",
                        "currently_eligible_unsent_replicas", "native_gpu_inflight", "per_host_buffer_limit")
COHORT_BACKLOG_WINDOW_S = 30 * 60
SYSTEMD_FINISHED = ("39f53479d3a045ac8e11786248231fbf", "7ad2d189f7e94e70a38c781354912448")


def cohort(unit, timer):
    """The native Qwen cohort controller (#1441): its last runs and its last report.

    The oneshot unit runs every minute and prints one JSON report a run (indented, so one
    journal line per output line, grouped here by invocation); a run that found the step
    lock busy exits 75 and prints none. Read-only: the journal, systemctl show and nothing
    else. Errors go in the record, so the server source still reports."""
    try:
        if not all(re.fullmatch(r"[a-zA-Z0-9@_.-]+\.(service|timer)", name) for name in (unit, timer)):
            raise ValueError("invalid unit")
        raw = run(["journalctl", "--no-pager", "--quiet", "--all", "-u", unit, "-o", "json",
                   "--since=-45min", "-n", "20000"])
        # outcomes: invocation -> (time, failed), from systemd's own terminal messages for the unit.
        runs, outcomes = {}, {}
        for line in raw.splitlines():
            entry = json.loads(line)
            if not isinstance(entry, dict):
                continue
            at = int(entry["__REALTIME_TIMESTAMP"]) // 1000000
            invocation = entry.get("_SYSTEMD_INVOCATION_ID") or entry.get("INVOCATION_ID")
            message_id = entry.get("MESSAGE_ID")
            field = SYSTEMD_FAILURE_FIELDS.get(message_id) if isinstance(message_id, str) else None
            if (field or message_id in SYSTEMD_FINISHED) and entry.get("UNIT") == unit:
                key = invocation or "at-%d" % at
                failed = bool(field) or outcomes.get(key, (0, False))[1]
                outcomes[key] = (max(at, outcomes.get(key, (0, False))[0]), failed)
                continue
            message = entry.get("MESSAGE")
            if (not invocation or not isinstance(message, str) or entry.get("_TRANSPORT") != "stdout"
                    or entry.get("SYSLOG_IDENTIFIER") in ("systemd", "podman")):
                continue
            runs.setdefault(invocation, []).append((at, message))
        report, report_at, admissible = None, None, None
        now = time.time()
        for lines in runs.values():
            text = [message for _, message in lines]
            if "{" not in text:
                continue
            try:
                document = json.loads("\n".join(text[text.index("{"):]))
            except ValueError:
                continue
            if not isinstance(document, dict):
                continue
            # A report without the admitted count and budget (a reconciled publication)
            # is not a full report; the next run's is.
            if (type(document.get("admitted_unique")) is int and type(document.get("limit")) is int
                    and (report_at is None or lines[-1][0] >= report_at)):
                report, report_at = document, lines[-1][0]
            # Each run scans one page of the audited candidates (the page moves on), so the
            # backlog is the most admissible fresh units any run saw in the last half hour:
            # fresh pairs less those passed over as out of cohort.
            fresh_pairs = document.get("fresh_pairs_in_scan")
            if type(fresh_pairs) is int and now - lines[-1][0] <= COHORT_BACKLOG_WINDOW_S:
                skipped = document.get("out_of_cohort_skipped") or []
                count = max(0, fresh_pairs - (len(skipped) if isinstance(skipped, list) else fresh_pairs))
                admissible = max(admissible or 0, count)
        if report is not None:
            report = {key: report.get(key) for key in COHORT_REPORT_FIELDS} | {"admissible_backlog": admissible}
        status = dict(item.split("=", 1) for item in run([
            "systemctl", "show", unit, "-p", "LoadState", "-p", "SubState", "-p", "Result",
        ]).decode().splitlines() if "=" in item)
        timer_state = dict(item.split("=", 1) for item in run(
            ["systemctl", "show", timer, "-p", "ActiveState"]).decode().splitlines() if "=" in item)
        if status.get("LoadState") != "loaded":
            raise ValueError("cohort controller not loaded")
        running = status.get("SubState") in ("start", "running")
        # Distinct finished runs, oldest first: the failures since the last success are the streak.
        finished = sorted(outcomes.values())
        streak = []
        for at, failed in reversed(finished):
            if not failed:
                break
            streak.append(at)
        record = {"result": "running" if running else "ok" if status.get("Result") == "success" else "failed",
                  "timer_active": timer_state.get("ActiveState") == "active",
                  "last_completed_at": max((at for at, failed in finished if not failed), default=None),
                  "last_failure_at": max((at for at, failed in finished if failed), default=0),
                  "failure_streak": len(streak), "failing_since": min(streak, default=None),
                  "report": report, "report_at": report_at}
        validate_cohort(record, int(time.time()))
        return record
    except Exception:
        return {"error": "cohort_unavailable"}


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


def config_fingerprint(data_dir):
    """Every app_config.xml under projects/, as (project, mtime_ns, size); None when
    the directory can't be listed or a config can't be read. Explicit listing, so
    permission errors aren't hidden (Path.glob hides them)."""
    root = Path(data_dir) / "projects"
    try:
        entries = sorted(root.iterdir())
    except FileNotFoundError:
        return [] if not root.exists() else None
    except OSError:
        return None
    found = []
    for project in entries:
        try:
            if not project.is_dir():
                continue
            info = (project / "app_config.xml").stat()
        except FileNotFoundError:
            continue
        except OSError:
            return None
        found.append([project.name, info.st_mtime_ns, info.st_size])
    return found


def session_record(session):
    try:
        record = json.loads(read_file(session, 65536))
    except (OSError, ValueError):
        return {}
    return record if isinstance(record, dict) else {}


def reservations_proven(session, client_start, fingerprint, messages):
    """Whether started tasks still hold their current records, kept per client session.

    A started task keeps the reservation it started with; the client gives a new usage
    only to unstarted tasks, at startup and when it rereads its config files. So within
    one client session (client_start_time), started tasks match their records while the
    client has not reread a config since startup. The source reads the client's message
    log every poll from a saved cursor: a session is trusted when first seen only if the
    log still starts at its first message, every app_config.xml predates the start, and
    no "Re-reading" line appears; afterwards any reread, any gap in the log (messages
    rotated out between polls) or any change to the configs (edit, new file, deletion)
    ends the trust until the client restarts. Without a record, nothing is trusted."""
    try:
        start = float(client_start)
    except (TypeError, ValueError):
        return False
    if session is None or not math.isfinite(start) or start <= 0:
        return False
    record = session_record(session)
    same = record.get("client_start") == start
    cursor = record.get("cursor", 0) if same else 0
    seqnos, reread = [], False
    if messages is not None:
        for message in messages.findall(".//msg"):
            try:
                seqnos.append(int(message.findtext("seqno", "")))
            except ValueError:
                messages = None
                break
            reread |= "Re-reading" in message.findtext("body", "")
    held = record.get("proven")
    if messages is None or type(cursor) is not int:
        proven, last = False, cursor if type(cursor) is int else 0
    else:
        contiguous = not seqnos or min(seqnos) == cursor + 1
        proven = (fingerprint is not None and contiguous and not reread
                  and (held is True and record.get("fingerprint") == fingerprint if same
                       else all(mtime / 1e9 < start for _, mtime, _ in fingerprint)))
        last = max(seqnos, default=cursor)
    atomic_json(session, {"client_start": start, "cursor": last, "fingerprint": fingerprint,
                          "proven": proven})
    return proven


def client(data_dir, port, session=None):
    from client_account import MASTER, RPC, flag

    password = read_file(data_dir / "gui_rpc_auth.cfg", 1024).decode().strip()
    rpc = RPC(port, password)
    try:
        state = rpc.call("get_state")
        status = rpc.call("get_cc_status")
        prefs = rpc.call("get_global_prefs_working")
        disk = rpc.call("get_disk_usage")
        client_start = state.findtext(".//time_stats/client_start_time")
        messages = None
        if session is not None:
            record = session_record(session)
            try:
                same = record.get("client_start") == float(client_start)
            except (TypeError, ValueError):
                same = False
            cursor = record.get("cursor", 0) if same else 0
            if type(cursor) is int and cursor >= 0:
                messages = rpc.call("get_messages", {"seqno": str(cursor)})
    finally:
        rpc.close()
    proven = reservations_proven(session, client_start, config_fingerprint(data_dir), messages)
    projects = [p for p in state.findall(".//project")
                if p.findtext("master_url", "").rstrip("/") == MASTER.rstrip("/")]
    if len(projects) != 1:
        raise ValueError("project not attached uniquely")
    project = projects[0]
    host_id = int(project.findtext("hostid", "0"))
    if host_id <= 0:
        raise ValueError("client is not attached")
    reasons = {tag: int(status.findtext(".//" + tag, "-1")) for tag in ("task_suspend_reason", "gpu_suspend_reason")}
    if any(reason < 0 for reason in reasons.values()):
        raise ValueError("client status unavailable")
    # A client that is suspended, or set to fetch no more work, has no idle slot for
    # the feed to fill: zero capacity, not unknown. GPU suspension alone zeroes the GPUs.
    paused = (flag(project, "suspended_via_gui") or flag(project, "dont_request_more_work")
              or reasons["task_suspend_reason"] != 0)
    gpu_paused = paused or reasons["gpu_suspend_reason"] != 0
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
    # The first task whose reservation can't be counted (#1698). The host is busy with it, so
    # the source still publishes what it proves (host, devices, versions) with no idle capacity
    # and `capacity_unproven`, and the evaluator leaves the host out of the shortage checks.
    unproven = None
    units = {(owner, w.findtext("name")): w for owner, w in scoped if w.tag == "workunit"}
    for owner, result in ([] if paused else scoped):
        if result.tag != "result":
            continue
        # Stock RESULT_COMPUTE_ERROR=3, FILES_UPLOADING=4: neither occupies execution.
        if int(result.findtext("state", "0")) >= 3 or flag(result, "ready_to_report"):
            continue
        if flag(result, "suspended_via_gui"):
            raise ValueError("suspended queued task")
        active = result.findall("active_task")
        if active and (len(active) != 1 or active[0].findtext("active_task_state") != "0") and not (
                len(active) == 1 and proven and owner == MASTER.rstrip("/")):
            # Running jobs retain old reservations after app_config/scheduler
            # updates; get_state exposes only a rounded display string for them.
            # They count at their records only in a trusted session, and only for
            # this project, whose scheduler-side changes the profile checks cover.
            unproven = unproven or "active_task_resource_usage_unavailable"
            continue
        if flag(result, "coproc_missing"):
            unproven = unproven or "task_resource_usage_unavailable"
            continue
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
            try:
                usage = client_resource_usage(unit, 0)
            except SupplyUnknown as error:
                unproven = unproven or str(error)
                continue
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
        unproven = unproven or "task_resource_usage_unavailable"
    if gpu_paused:
        gpus = {vendor: 0 for vendor in gpus}
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
    available = 0 if paused else max(0, cpus - used_cpu)
    document = {"host_id": host_id, "versions": monitored, "paused": paused,
                "idle_cpu": 0 if "CPU" in blocked or flag(project, "no_cpu_pref") or flag(project, "no_cpu")
                else available,
                "available_cpu": available,
                "cpus": cpus, "idle_gpu": gpus, "gpu_memory": gpu_memory, "gpu_capabilities": capabilities,
                "gpu_devices": gpu_devices, "used_gpu": used_gpu,
                "client_version": sum(int(state.findtext(".//" + tag, "0")) * factor for tag, factor in (
                    ("core_client_major_version", 10000), ("core_client_minor_version", 100),
                    ("core_client_release", 1))),
                "docker": container_runtime(read_command=run)
                if any(v["plan_class"].startswith("docker") for v in monitored) else False,
                "memory_bytes": 0 if paused else max(0, memory * fraction - used_memory),
                "disk_bytes": 0 if paused else max(0, min(float(free), float(allowance) - used))}
    if unproven:
        # No idle capacity is claimed, so an evaluator from before #1698 reads a full host, not a
        # healthy one with spare slots, and a newer one excludes it (feed_collect.collect).
        document.update(idle_cpu=0, available_cpu=0, memory_bytes=0, disk_bytes=0,
                        idle_gpu={vendor: 0 for vendor in gpus}, capacity_unproven=True, unproven=unproven)
        del document["used_gpu"]
    return document


def controller(unit, maintenance=()):
    """The controller's last tick and last failure, from its unit's journal and systemd.

    `maintenance` are units that pause the controller to do its upkeep, such as the daily
    held-page archive (#835): their failures count as the controller's, and while one runs, or
    for 15 minutes after it ends, a tick that is overdue because of the pause isn't unknown."""
    units = (unit, *maintenance)
    if not all(re.fullmatch(r"[a-zA-Z0-9@_.-]+\.service", u) for u in units) or len(set(units)) != len(units):
        raise ValueError("invalid unit")
    # --all: without it, journalctl's JSON output nulls any field over 4096 bytes, and a
    # successful tick's line (its timings) is longer, so only failures were ever seen.
    raw = run(["journalctl", "--no-pager", "--quiet", "--all", *(a for u in units for a in ("-u", u)),
               "-o", "json", "--since=-2h", "-n", "2000"])
    # The controller's own failures and its maintenance units' are kept apart: one failed tick
    # waits for a second (feed_status), a maintenance failure doesn't.
    latest, controller_failure, maintenance_failure = None, 0, 0
    # The controller's finished runs, each one's lines grouped by invocation: key -> [time, failed].
    # A run's JSON line and systemd's failure lines for it are one run. Each JSON line ends a
    # run; a systemd line without an invocation id joins the run that ended under a minute before.
    runs = {}
    # The error each run failed with, by the same key (a run's last failure line).
    errors = {}
    # The controller's own signals (#1698): the largest share of each lifetime cap it warned
    # about in the window, each plan's latest tick, when a live plan last published, and the
    # tick durations of the last hour.
    caps, plans, published_at, first_live, recent = {}, {}, None, None, []
    reported = False

    def record(at, failed, invocation, joins=False, error=None):
        key = invocation
        if not key and not joins:
            key = "at-%d" % at
            while key in runs:
                key += "+"
        elif not key:
            near = [k for k, (t, _) in runs.items() if 0 <= at - t <= TICK_FAILURE_SAME_S]
            key = max(near, key=lambda k: runs[k][0]) if near else "at-%d" % at
        elif key not in runs:
            # A run seen first without its id (an anonymous line under a minute before) is this one.
            near = [k for k, (t, _) in runs.items() if k.startswith("at-") and 0 <= at - t <= TICK_FAILURE_SAME_S]
            if near:
                moved = max(near, key=lambda k: runs[k][0])
                runs[key] = runs.pop(moved)
                if moved in errors:
                    errors[key] = errors.pop(moved)
        t, was = runs.get(key, (at, False))
        runs[key] = [max(t, at), was or failed]
        if error is not None:
            errors[key] = error

    for line in raw.splitlines():
        entry = json.loads(line)
        if not isinstance(entry, dict):
            continue
        message_id = entry.get("MESSAGE_ID")
        field = SYSTEMD_FAILURE_FIELDS.get(message_id) if isinstance(message_id, str) else None
        if field and entry.get("UNIT") in units:
            outcome = entry.get(field)
            if not isinstance(outcome, str) or not outcome or outcome in ("success", "done"):
                raise ValueError("invalid native controller failure evidence")
            at = int(entry["__REALTIME_TIMESTAMP"]) // 1000000
            if entry["UNIT"] == unit:
                controller_failure = max(controller_failure, at)
                record(at, True, entry.get("INVOCATION_ID") or entry.get("_SYSTEMD_INVOCATION_ID"), joins=True)
            else:
                maintenance_failure = max(maintenance_failure, at)
            continue
        try:
            event = json.loads(entry.get("MESSAGE", ""))
        except (ValueError, TypeError):
            continue
        if not isinstance(event, dict) or event.get("event") not in (
                "boinc_batch_tick", "boinc_batch_failure", "boinc_batch_cap_warning"):
            continue
        # Only the controller's own ticks: a maintenance unit's lines are read for its failures.
        if maintenance and entry.get("_SYSTEMD_UNIT", unit) != unit:
            continue
        finished = int(entry["__REALTIME_TIMESTAMP"]) // 1000000
        if event["event"] == "boinc_batch_cap_warning":
            cap, count, limit = event.get("cap"), event.get("count"), event.get("limit")
            if (isinstance(cap, str) and NAME.fullmatch(cap) and type(count) is int and type(limit) is int
                    and count >= 0 and limit > 0 and (cap in caps or len(caps) < MAX_CAPS)):
                worst = caps.get(cap)
                if worst is None or count * worst["limit"] > worst["count"] * limit:
                    caps[cap] = {"count": count, "limit": limit}
            continue
        ok = event["event"] == "boinc_batch_tick"
        invocation = entry.get("_SYSTEMD_INVOCATION_ID") or entry.get("INVOCATION_ID")
        if not ok:
            controller_failure = max(controller_failure, finished)
            record(finished, True, invocation, error=event.get("error"))
        elif isinstance(event.get("plan"), str) and "ownership_conflicts" in event:
            reported = True
            draining = event.get("draining") is True
            pending = sum(value for value in (event.get("observed_pending"), event.get("collection_backlog"))
                          if type(value) is int and value >= 0)
            conflicts = event["ownership_conflicts"]
            plans[event["plan"][:64]] = {"draining": draining, "pending": pending,
                                         "conflicts": conflicts if type(conflicts) is int and conflicts >= 0 else 0}
            if not draining:
                first_live = finished if first_live is None else min(first_live, finished)
                published = event.get("newly_published")
                if type(published) is int and published > 0:
                    published_at = finished
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
            controller_failure = max(controller_failure, finished)
        if ok:  # a failure's line was recorded above
            record(finished, seconds > TICK_LIMIT_S, invocation)
        # Every tick's duration, failed ones too: a failure that runs long is part of the trend.
        if time.time() - finished <= TICK_TREND_WINDOW_S:
            recent.append(seconds)
        latest = {"ok": ok, "seconds": seconds, "finished_at": finished}
    # Failed or slow runs since the last good one, newest first: feed_status fires on two. The
    # newest error among them names the reason, and the runs that failed with that same error
    # (numbers masked), newest first until another error, are same_error_in_a_row: three is a
    # sticky failure (#1698). A slow run or one killed without a line has no error and is skipped.
    streak, reason, repeats, first, counting = 0, None, 0, None, True
    for key in sorted(runs, key=lambda k: runs[k][0], reverse=True):
        if not runs[key][1]:
            break
        streak += 1
        error = same_error(errors.get(key))
        if error is None or not counting:
            continue
        if first is None:
            first, reason = error, failure_reason(errors[key])
        counting = error == first
        repeats += counting
    # Failed or slow runs in the last hour, good runs between them or not: failures that alternate
    # with good ticks never make a streak (#1698).
    failed_last_hour = sum(failed and time.time() - at <= TICK_TREND_WINDOW_S for at, failed in runs.values())
    # The newest error in that hour, good runs between or not: the reason the hourly count fires
    # with, so a success between failures doesn't change the alert's name.
    hour = [key for key in errors if runs[key][1] and time.time() - runs[key][0] <= TICK_TREND_WINDOW_S]
    last_hour_reason = failure_reason(errors[max(hour, key=lambda k: runs[k][0])]) if hour else None
    live = [plan for plan in plans.values() if not plan["draining"]]
    failures = {"last_failure_at": max(controller_failure, maintenance_failure),
                "controller_failure_at": controller_failure, "maintenance_failure_at": maintenance_failure,
                "failed_in_a_row": streak, "failed_last_hour": failed_last_hour,
                "last_hour_reason": last_hour_reason,
                "reason": reason, "same_error_in_a_row": repeats,
                "caps": caps,
                # None: no tick line in the window reports it (a scheduler from before #1685).
                "ownership_conflicts": max((p["conflicts"] for p in live), default=0) if reported else None,
                "progress": {"published_at": published_at, "first_tick_at": first_live,
                             "pending": max((p["pending"] for p in live), default=0) if reported else None},
                "tick_p90_s": round(percentile(recent, 0.9), 3) if recent else None}
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
                "finished_at": int(time.time())} | failures
    # A failed unit counts only from fresh runs: a controller that failed and stopped would see its
    # failures leave the journal window and its count fall, so stale history is unknown, not healthy.
    last_run = max((t for t, _ in runs.values()), default=None)
    failed = status.get("Result") != "success"
    if failed and last_run is not None and time.time() - last_run <= 900:
        return {"ok": False, "seconds": max(0, end - start), "finished_at": int(time.time())} | failures
    if failed or latest is None or time.time() - latest["finished_at"] > 900:
        paused = paused_by(maintenance)
        if paused:
            # Paused on purpose: nothing ran, so nothing ran long or failed.
            return {"ok": None, "seconds": 0, "finished_at": int(time.time()), **failures,
                    "paused_by": paused}
        raise NoRecentTick()
    return latest | failures


def paused_by(maintenance):
    """The first maintenance unit that is running, or ended under 15 minutes ago, else None."""
    for unit in maintenance:
        status = dict(line.split("=", 1) for line in run([
            "systemctl", "show", unit, "-p", "LoadState", "-p", "SubState",
            "-p", "ExecMainStartTimestampMonotonic", "-p", "ExecMainExitTimestampMonotonic",
        ]).decode().splitlines() if "=" in line)
        if status.get("LoadState") != "loaded":
            continue
        start = int(status.get("ExecMainStartTimestampMonotonic", "0")) / 1e6
        end = int(status.get("ExecMainExitTimestampMonotonic", "0")) / 1e6
        now = time.clock_gettime(time.CLOCK_MONOTONIC)
        if status.get("SubState") in ("start", "running") and start > end:
            return unit
        if start and end >= start and 0 <= now - end <= 900:
            return unit
    return None


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
    # The installed version stands for its platform and plan class (#1687): the scheduler sends
    # whichever loaded version a unit needs and the client downloads it, so a host with only an
    # older version installed still takes work. Loaded as stock sched_shmem.cpp does: every
    # undeprecated version at or above min_version when it is set, otherwise only the highest.
    available = [v["version_num"] for v in database["versions"] if v["app"] == version["app"]
                 and v["plan_class"] == version["plan_class"] and v["platform"] == version["platform"]]
    minimum = database["minimum_versions"][version["app"]]
    served = frozenset(n for n in available if n >= minimum) if minimum else frozenset(available and [max(available)])
    if not served:
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
            integers["min_batch"], integers["max_batch"] or 2**63, served)


def eligible(unit, host, version, bounds):
    """`bounds` is host_profile's: workunit and batch bounds, then the version numbers loaded
    for the version's platform and plan class. An unpinned unit gets the newest of them, a
    pinned one only its own number."""
    return (unit["app"] == version["app"] and host["host_id"] not in unit["hosts"]
            and not unit["assignments"] and not unit["hr_class"]
            and unit["rsc_memory_bound"] <= host["memory_bytes"]
            and unit["rsc_disk_bound"] <= host["disk_bytes"]
            and (unit["app_version_num"] == 0 or unit["app_version_num"] in bounds[4])
            and bounds[0] <= unit["id"] <= bounds[1] and bounds[2] <= unit["batch"] <= bounds[3])


def supply(database, hosts, resource, resource_usage=None):
    """Maximum matching: a replica and a host/workunit pair can each be used once.

    Work offered to two classes is not two results, and two replicas of a unit
    cannot fill two slots on the same host.
    """
    prepared, host_limits, host_costs = [], {}, {}
    for host in hosts:
        prefs = database["host_preferences"][str(host["host_id"])]
        # No idle slot (a paused client, or a full one) can't make a shortage, whatever its preferences.
        if resource == "gpu" and not any(host["idle_gpu"].values()):
            continue
        if resource == "cpu" and host["idle_cpu"] == 0:
            continue
        if prefs.get("fallback"):
            raise ValueError("non-preferred fallback needs all-application inventory")
        app = "usnm_ocr" if resource == "cpu" else "usnm_qwen"
        if app not in prefs["apps"] or resource == "cpu" and "CPU" in prefs["blocked"]:
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


def replica_status(cnf):
    """SHOW REPLICA STATUS on the local replica, as the monitor user: thread states and lag only."""
    import pymysql  # python3-pymysql, only on the replica host

    with stage("replica_unavailable"):
        connection = pymysql.connect(read_default_file=str(cnf), connect_timeout=10, read_timeout=10,
                                     write_timeout=10, cursorclass=pymysql.cursors.DictCursor)
        try:
            with connection.cursor() as cursor:
                cursor.execute("SHOW REPLICA STATUS")
                rows = cursor.fetchall()
        finally:
            connection.close()
    if len(rows) != 1:
        raise SourceFailed("replica_not_replica")
    row = rows[0]
    lag = row.get("Seconds_Behind_Source")
    return {"io_running": row.get("Replica_IO_Running") == "Yes",
            "sql_running": row.get("Replica_SQL_Running") == "Yes",
            "lag_s": None if lag is None else natural(int(lag))}


def r2_listing(rclone, bucket, r2_env, now):
    """The newest dump manifest and the newest shipped binlog in the bucket, as Unix times."""
    values = {}
    for line in read_file(r2_env, 4096).decode().splitlines():
        key, _, value = line.partition("=")
        if key in ("R2_ACCESS_KEY_ID", "R2_SECRET_ACCESS_KEY", "R2_ENDPOINT") and value:
            values[key] = value
    if len(values) != 3:
        raise SourceFailed("r2_unavailable")
    env = {"PATH": os.environ.get("PATH", "/usr/bin:/bin"), "RCLONE_CONFIG_R2_TYPE": "s3",
           "RCLONE_CONFIG_R2_PROVIDER": "Cloudflare", "RCLONE_CONFIG_R2_NO_CHECK_BUCKET": "true",
           "RCLONE_CONFIG_R2_ACCESS_KEY_ID": values["R2_ACCESS_KEY_ID"],
           "RCLONE_CONFIG_R2_SECRET_ACCESS_KEY": values["R2_SECRET_ACCESS_KEY"],
           "RCLONE_CONFIG_R2_ENDPOINT": values["R2_ENDPOINT"]}

    def newest(prefix, pattern, recursive):
        # A Go time layout with its offset: rclone 1.68 has no "unix" name for it.
        argv = [str(rclone), "--config", "/dev/null", "lsf", "--files-only", "--format", "tp",
                "--time-format", "2006-01-02T15:04:05Z07:00"] + (["-R"] if recursive else []) + [
                    "r2:%s/%s" % (bucket, prefix)]
        with stage("r2_unavailable"):
            # rclone exits 3 for a prefix with nothing under it yet.
            output = run(argv, env=env, exits=(0, 3)).decode()
        times = []
        for line in output.splitlines():
            stamp, _, name = line.partition(";")
            if pattern.fullmatch(prefix + name if recursive else name):
                times.append(int(datetime.fromisoformat(stamp.replace("Z", "+00:00")).timestamp()))
        latest = max(times, default=None)
        if latest is not None and latest > now + 300:
            raise SourceFailed("r2_unavailable")
        return latest

    dump = newest("dump/", DUMP_KEY, True)
    uuids = set()
    with stage("r2_unavailable"):
        for line in run([str(rclone), "--config", "/dev/null", "lsf", "--dirs-only", "r2:%s/binlog/" % bucket],
                        env=env, exits=(0, 3)).decode().splitlines():
            if re.fullmatch(r"[0-9a-f-]{36}/", line):
                uuids.add(line[:-1])
    # A rebuilt primary ships under its new server UUID; the newest binlog of any is the one that counts.
    binlogs = [newest("binlog/%s/" % uuid, BINLOG_KEY, False) for uuid in sorted(uuids)]
    binlog = max((t for t in binlogs if t is not None), default=None)
    return {"newest_dump_at": dump, "newest_binlog_at": binlog, "checked_at": now}


def drill_result(path, now):
    """The last restore drill's outcome, from the record restore-drill.sh leaves on the replica."""
    try:
        record = json.loads(read_file(path, 65536))
    except FileNotFoundError:
        raise SourceFailed("drill_missing") from None
    except (OSError, ValueError):
        raise SourceFailed("drill_unreadable") from None
    if (not isinstance(record, dict) or type(record.get("pass")) is not bool
            or type(record.get("finished_at")) is not int or not 0 < record["finished_at"] <= now + 300):
        raise SourceFailed("drill_unreadable")
    return {"pass": record["pass"], "finished_at": record["finished_at"]}


def dbbackup(replica_cnf, rclone, bucket, r2_env, drill_file, cache, now):
    """The dbbackup source: each part is its record, or {"error": <code>} on its own."""
    def part(read, default):
        try:
            return read()
        except SourceFailed as error:
            return {"error": error.reason if error.reason in DBBACKUP_FAILURES else default}
        except Exception:
            return {"error": default}

    def r2():
        # Cached for R2_CHECK_EVERY_S, so the bucket is listed every five minutes, not each run.
        try:
            cached = json.loads(read_file(cache, 4096))
            if (isinstance(cached, dict) and type(cached.get("checked_at")) is int
                    and 0 <= now - cached["checked_at"] < R2_CHECK_EVERY_S):
                return cached
        except (OSError, ValueError):
            pass
        result = r2_listing(rclone, bucket, r2_env, now)
        atomic_json(cache, result)
        return result

    return {"replica": part(lambda: replica_status(replica_cnf), "replica_unavailable"),
            "r2": part(r2, "r2_unavailable"),
            "drill": part(lambda: drill_result(drill_file, now), "drill_unreadable")}


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
    # Opt-in (#1441): judge usnm_qwen by the native cohort controller's admissions,
    # from the server source's cohort record, instead of by workunit creation.
    admission = config.get("qwen_admission", False)
    if type(admission) is not bool:
        raise ValueError("qwen_admission must be true or false")
    # Opt-in (#1698): controller_cap, ownership_conflict, controller_progress and tick_trend from the
    # controller source's signals. Turn it on only once the dashboard accepts those checks.
    signals = config.get("controller_signals", False)
    if type(signals) is not bool:
        raise ValueError("controller_signals must be true or false")
    locations = {"server": config["server"], "controller": config["controller"]}
    # The backlog source is optional until it is deployed: unknown, never healthy.
    if config.get("backlog") is not None:
        locations["backlog"] = config["backlog"]
    # Opt-in: the BOINC database's backups and replica (db_replica, db_dump, db_binlog, db_drill).
    # Set it only once the dashboard accepts the dbbackup source and those checks.
    if config.get("dbbackup") is not None:
        locations["dbbackup"] = config["dbbackup"]
    allowed = own_clients(config)
    clients = list(allowed)
    locations.update({name: location for name, (location, _) in allowed.items()})
    facts, documents = {"sources": {}, "errors": [], "qwen_admission": admission,
                        "controller_signals": signals, "dbbackup_checks": "dbbackup" in locations}, {}
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
                if document.get("capacity_unproven"):
                    # Fresh, so it never counts towards collection; the shortage checks leave it out.
                    facts["sources"][name]["unproven"] = document["unproven"]
                documents[name] = document
            except (OSError, ValueError, KeyError, TypeError, http.client.HTTPException) as error:
                if isinstance(error, SupplyUnknown) and str(error) in (*SOURCE_UNKNOWN_REASONS, BINDING_MISMATCH,
                                                                     NO_RECENT_TICK):
                    reason = str(error)
                elif isinstance(error, SourceFailed) and error.reason in SERVER_FAILURES:
                    # The server's own fixed code for the stage that failed, as in its document.
                    reason = error.reason
                else:
                    reason = "http_protocol_error" if isinstance(error, http.client.HTTPException) else "unavailable_or_invalid"
                facts["sources"][name] = {"observed_at": None, "state": "unknown", "reason": reason}
                facts["errors"].append(name)
    database = documents.get("server")
    hosts = [documents.get(name) for name in clients]
    # A host whose running tasks can't be counted (capacity_unproven) is busy: leaving it out can
    # only under-count idle slots, so it can hide a shortage on that host but never make one up.
    # Both lanes and the shared budgets below see the same proven hosts, as the shared-capacity
    # rule keeps only proven allocation (#1698).
    proven = [h for h in hosts if h and not h.get("capacity_unproven")]
    excluded = [{"source": name, "host_id": documents[name]["host_id"], "reason": documents[name]["unproven"]}
                for name in clients if documents.get(name, {}).get("capacity_unproven")]
    if excluded:
        facts["excluded_hosts"] = excluded
    if not clients:
        # No own capacity to judge is unknown, not zero idle slots.
        facts["supply_errors"] = {resource: "no_own_clients_configured" for resource in ("cpu", "gpu")}
        facts["errors"] += ["cpu_no_own_clients_configured", "gpu_no_own_clients_configured"]
    elif database and all(hosts) and not proven:
        # Every own client fresh but unproven: nothing to judge, as with none configured.
        facts["supply_errors"] = {resource: NO_PROVEN_CLIENTS for resource in ("cpu", "gpu")}
        facts["errors"] += ["cpu_" + NO_PROVEN_CLIENTS, "gpu_" + NO_PROVEN_CLIENTS]
    elif database and all(hosts):
        every, hosts = hosts, proven
        facts["supply"] = {}
        usage = {"cpu": {}, "gpu": {}}
        for resource in ("cpu", "gpu"):
            try:
                if len({h["host_id"] for h in every}) != len(every):
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
        if "cohort" in documents["server"]:
            facts["cohort"] = documents["server"]["cohort"]
    if guarded:
        reason = "not_reported" if "server" in documents else "server_unavailable"
        facts["feeder_allocation"] = documents.get("server", {}).get(
            "feeder_allocation", {"status": "unknown", "reason": reason, "app_ids": []})
    if "controller" in documents:
        facts["tick"] = documents["controller"]["tick"]
    if "backlog" in documents:
        facts["backlogs"] = documents["backlog"]["backlogs"]
    if "dbbackup" in documents:
        facts["dbbackup"] = {part: documents["dbbackup"][part] for part in ("replica", "r2", "drill")}
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
    p.add_argument("--cohort-unit", help="The native Qwen cohort controller's oneshot unit, read-only "
                   "(journal and systemctl show), reported as cohort for usnm_qwen_no_work")
    p = sub.add_parser("client")
    p.add_argument("--data-dir", type=Path, required=True)
    p.add_argument("--port", type=int, default=31416)
    p = sub.add_parser("controller")
    p.add_argument("--unit", default="usnm-boinc-priority-controller.service")
    p.add_argument("--maintenance-unit", action="append", default=[],
                   help="A unit that pauses the controller for upkeep, such as the held-page archive (#835): "
                        "its failures count as the controller's, and its pause isn't a missing tick")
    p = sub.add_parser("backlog")
    p.add_argument("--dashboard", required=True)
    p.add_argument("--requests", type=Path, required=True)
    p.add_argument("--outbox", type=Path, required=True, help="Existing Qwen campaign directory (not all apps)")
    p.add_argument("--selection", type=Path, help="Read-only mirror of the current Qwen selector's selected.jsonl")
    p = sub.add_parser("dbbackup", help="The BOINC database's replica, R2 backups and restore drill, on the replica host")
    p.add_argument("--replica-cnf", type=Path, required=True,
                   help="MySQL option file for the monitor user on the local replica (REPLICATION CLIENT only)")
    p.add_argument("--r2-env", type=Path, required=True, help="The bucket's reader key: R2_ACCESS_KEY_ID, "
                   "R2_SECRET_ACCESS_KEY, R2_ENDPOINT")
    p.add_argument("--rclone", type=Path, default=Path("/usr/local/lib/usnm-db-backup/rclone"))
    p.add_argument("--bucket", default="usnm-boinc-db-backup")
    p.add_argument("--drill-file", type=Path, default=Path("/var/lib/usnm-db-backup/last-drill.json"),
                   help="The record restore-drill.sh leaves here after each drill")
    for p in sub.choices.values():
        p.add_argument("--output", type=Path, required=True)
        p.add_argument("--push-url", help="Existing private OCR dashboard /api/feed-source/<name>")
    args = parser.parse_args(argv)
    result = {"schema_version": 1, "source": args.source, "observed_at": int(time.time())}
    try:
        if args.source == "server":
            result.update(server(args.project, args.window_env, args.container, args.container_project,
                                 args.feeder_guard, args.cohort_unit))
        elif args.source == "client":
            result.update(client(args.data_dir, args.port, args.output.with_name(args.output.stem + ".session.json")))
        elif args.source == "controller":
            result["tick"] = controller(args.unit, tuple(args.maintenance_unit))
        elif args.source == "dbbackup":
            result.update(dbbackup(args.replica_cnf, args.rclone, args.bucket, args.r2_env, args.drill_file,
                                   args.output.with_name(args.output.stem + ".r2.json"), result["observed_at"]))
        else:
            result.update(backlog(args.dashboard, args.requests, args.outbox, args.selection))
    except NoRecentTick:
        result["error"] = NO_RECENT_TICK
    except SupplyUnknown as error:
        result["error"] = str(error) if str(error) in SOURCE_UNKNOWN_REASONS else "source unavailable"
    except SourceFailed as error:
        result["error"] = error.reason if error.reason in SERVER_FAILURES else "source unavailable"
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
