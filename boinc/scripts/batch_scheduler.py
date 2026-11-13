#!/usr/bin/env python3
"""One bounded BOINC OCR tick; offline validation unless explicitly --apply."""

import argparse
import base64
import concurrent.futures
import contextlib
import copy
import fcntl
import stat
import functools
import hashlib
import json
import math
import os
import re
import shutil
import subprocess
import socket
import sys
import tempfile
import threading
import time
import urllib.parse
import urllib.request
import uuid
import xml.etree.ElementTree as ET
from pathlib import Path


REPO = Path(__file__).resolve().parents[2]
ID = re.compile(r"[0-9a-f]{64}")
STAGES = {"prepared", "reserved", "submitting", "submitted", "published", "failed"}
MAX_JOBS = 10000
# Pages whose held result (#835) archive_history.py archived (state `archived_held`): one small
# entry each, so a re-claim of the page's message keeps the archived job instead of starting
# another. Kept until the page is restored for publishing, so the cap is generous.
MAX_ARCHIVED_HELD = 200000
# The bridge reports its existing-workunit cap (batch-inventory.py, #1483); an older bridge that
# doesn't report one keeps MAX_JOBS. The controller accepts no report above this ceiling.
MAX_EXISTING_WORKUNITS_CEILING = 200000
MAX_GLOBAL_PENDING = 100000
DEFAULT_MAX_GLOBAL_PENDING = 1000
# The sanity bound on a counter in BOINC's server status (parse_server_status). The counters are
# observations, not limits: a large backlog in another app (a Qwen or quality wave) is still a
# good status, and capping its counters at MAX_GLOBAL_PENDING failed every OCR tick (#1698). A
# counter above this is garbage, not a backlog.
MAX_STATUS_COUNT = 10_000_000
# The feeder's shared-memory work slots (shmem_work_items in boinc/project/config.xml.template).
# With --other-app-budget, every app's unassimilated work, counted in full, stays under it (#1474).
FEEDER_SHMEM_SLOTS = 4096
MAX_STATUS_BYTES = 1024 * 1024
MAX_FAILURE_OUTPUT_CHARS = 16384
MAX_COLLECT_PARALLEL = 8  # handoff-job.sh's COLLECT_PARALLEL
OCR_APP_NAME = "usnm_ocr"
# The native OCR build's app (#1780, docs/boinc/apps/native-ocr.md): a plan whose `app` it is
# sends every unit to one of its `native_hosts`, as the trust list admits.
NATIVE_APP_NAME = "usnm_ocr_native"
SUPPORTED_COHORT_APPS = (OCR_APP_NAME, NATIVE_APP_NAME)
MAX_NATIVE_HOSTS = 100
SURYA_APP_NAME = "usnm_surya"
SURYA_DEFAULTS = {"timeout_secs": 600, "stage_pages": 50, "submit_units": 200}
SURYA_REPLICAS = 2  # results per unit: --target_nresults in crates/usnm-boinc/src/surya/submit.rs
SUPPORTED_COHORT_PLATFORMS = ("all",)
SUPPORTED_COHORT_RESOURCES = ("cpu",)
APP_NAME = re.compile(r"[A-Za-z0-9_-]{1,80}")
ZERO_WORK_LINE = re.compile(r"(?:No tasks sent|Got 0 new tasks|Sending reply to \[HOST#\d+\]: 0 results)")


class ConfigError(ValueError):
    """Invalid scheduler or cohort configuration."""


class FailureReceiptError(RuntimeError):
    """A failed command's private evidence could not be persisted."""


def raise_transport_errors(errors):
    receipt_errors = [error for error in errors if isinstance(error, FailureReceiptError)]
    if receipt_errors:
        raise FailureReceiptError("; ".join(dict.fromkeys(
            str(error) for error in [errors[0], *receipt_errors]
        )))
    raise errors[0]


# The percentage of a lifetime cap past which a tick warns that history needs archiving (#1483).
CAP_WARNING_PERCENT = 80


def warn_near_cap(cap, count, limit, now=None):
    """One event on stderr while a count that stops the tick at `limit` is past 80% of it, so
    the cap is a warning before it is an outage (#1483). The tick itself is unchanged. Each cap
    is checked once a tick, so it repeats each tick until history is archived."""
    if count * 100 <= limit * CAP_WARNING_PERCENT:
        return
    try:
        print(json.dumps({
            "event": "boinc_batch_cap_warning", "schema_version": 1,
            "at": int(time.time() if now is None else now),
            "cap": cap, "count": count, "limit": limit,
            "message": f"{cap} at {count} of {limit}: archive drained history (#1483) before it stops the tick",
        }, sort_keys=True), file=sys.stderr, flush=True)
    except (OSError, ValueError):
        # A closed or broken log stream never stops a tick.
        pass


def require(ok, message):
    if not ok:
        raise ValueError(message)


COMMON = {"schema_version", "env", "account", "plan", "manifest", "image", "push_command",
          "scans_root", "manifests_root", "batch_size", "max_pending", "pages_per_host"}
PAGES_FIELDS = COMMON | {"batch", "pages"}
QUEUE_FIELDS = COMMON | {"source", "claim_queues", "lease_hours"}
# A Surya plan (#968) keeps usnm_surya topped up from a quality campaign's staged pages: it has
# no page list, manifest or scan push, and takes nothing off the OCR work queues.
SURYA_FIELDS = {"schema_version", "env", "plan", "app", "surya", "low_watermark", "high_watermark"}
SURYA_OPTIONAL = {"step_command", "server_status_url"}
SURYA_REQUIRED = {"campaign", "quality_campaign", "engine_image", "engine_llama", "engine_weights"}
OPTIONAL_FIELDS = {
    "app", "platform", "resource", "cohort", "cohorts",
    "low_watermark", "high_watermark", "max_global_pending", "server_status_url", "step_command",
    "gold_onboarding", "pages_per_unit", "bundle_app_version", "handoff_root", "native_hosts",
    "units_per_cpu",
}
# OCR bundles (#1059): at most this many pages of one queue in one usnm_ocr workunit
# (crates/usnm-ocr-contract/src/boinc.rs, bundle::MAX_PAGES), 1 (a page alone) by default.
MAX_PAGES_PER_UNIT = 32
QUEUE_OF_REASON = {"user": "ocr-priority", "bulk": "ocr-bulk"}
# Gold onboarding (#901): each tick, when due, the controller runs the bridge's gold-onboard
# step (a gold unit for every untrusted host without one outstanding) and gold-promote (admit
# the hosts whose complete native evidence passes the scope's existing calibration), so a new
# host is calibrated before its first priority result is held at collection. Not part of the
# allocation: its settings stay out of the config digest.
GOLD_REQUIRED = {"campaign", "language", "profile", "engine_base"}
# `image`: the worker digest to onboard hosts onto, when it isn't the plan's own (#1687): a
# plan rotating onto a new worker first calibrates its hosts on it, still feeding the old one.
GOLD_OPTIONAL = {"engine_version", "max_hosts", "interval_minutes", "image"}
GOLD_DEFAULT_INTERVAL_MINUTES = 60
# While results are held at collection, a scope's wait is cut to at most this. An hour, not ten
# minutes: a gold run takes about 12 minutes on CT 230, so a shorter wait ran it on every tick and
# kept ticks past controller_tick's 15-minute limit. Held results from hosts gold can't calibrate
# never clear, so the shorter wait never ended.
GOLD_HELD_INTERVAL_SECONDS = 60 * 60
GOLD_STEPS = ("gold-onboard", "gold-promote")
GOLD_SKIPPED_AT_CAP = "skipped: global safety cap"
QUEUES = ("ocr-bulk", "ocr-priority")
PAGE_KEY = re.compile(r"[A-Za-z0-9_-]+/\d{4}-\d{2}-\d{2}/ed-\d+/seq-\d+")


def _nonneg_int(value, maximum=MAX_GLOBAL_PENDING, message="invalid integer counter"):
    require(type(value) is int and 0 <= value <= maximum, message)
    return value


def _validate_status_url(url):
    require(isinstance(url, str) and len(url) <= 512, "invalid server_status_url")
    parsed = urllib.parse.urlsplit(url)
    require(parsed.scheme == "https" and bool(parsed.hostname) and
            parsed.username is None and parsed.password is None,
            "server_status_url must be credential-free https")
    return url


def normalize_cohort_apps(app):
    if not isinstance(app, str) or app not in SUPPORTED_COHORT_APPS:
        raise ConfigError(
            f"unsupported cohort app; Transport only dispatches {OCR_APP_NAME} (and, for a native "
            f"plan, {NATIVE_APP_NAME}) workunits"
        )
    return app


def _validate_native_plan(config):
    """A native OCR plan (#1780): queue-fed, single pages only, its `native_hosts` the only hosts
    its units may go to (none, by default: no units), and nothing that would make a unit any
    host could get. Every other plan names no native hosts."""
    native = config.get("app") == NATIVE_APP_NAME
    if not native:
        require("native_hosts" not in config, f"native_hosts is for {NATIVE_APP_NAME} plans")
        require("units_per_cpu" not in config, f"units_per_cpu is for {NATIVE_APP_NAME} plans")
        for spec in (config.get("cohorts") or {}).values():
            require(spec.get("app", OCR_APP_NAME) == OCR_APP_NAME,
                    f"a {NATIVE_APP_NAME} cohort needs a {NATIVE_APP_NAME} plan")
        return
    require(type(config.get("units_per_cpu", 1)) is int and 1 <= config.get("units_per_cpu", 1) <= 16,
            "units_per_cpu: 1 to 16")
    hosts = config.get("native_hosts")
    require(isinstance(hosts, list) and len(hosts) <= MAX_NATIVE_HOSTS and
            all(type(host) is int and 1 <= host <= 9999999999 for host in hosts) and
            len(set(hosts)) == len(hosts),
            f"a {NATIVE_APP_NAME} plan needs native_hosts, up to {MAX_NATIVE_HOSTS} distinct host ids "
            "(an empty list sends nothing)")
    require(config.get("source") == "queue" and config.get("claim_languages"),
            f"a {NATIVE_APP_NAME} plan is queue-fed with claim_languages, the scopes its hosts are trusted for")
    for key in ("cohort", "cohorts", "gold_onboarding", "bundle_app_version"):
        require(key not in config, f"a {NATIVE_APP_NAME} plan takes no {key}")
    require(max(config.get("pages_per_unit", {}).values(), default=1) == 1,
            f"a {NATIVE_APP_NAME} unit is one page")


def normalize_cohort_platform(platform):
    if not isinstance(platform, str) or platform not in SUPPORTED_COHORT_PLATFORMS:
        raise ConfigError(
            f"unsupported cohort platform; {OCR_APP_NAME} queue accounting is application-scoped across all platforms"
        )
    return platform


def normalize_cohort_resource(resource):
    if not isinstance(resource, str) or resource not in SUPPORTED_COHORT_RESOURCES:
        raise ConfigError(
            f"unsupported cohort resource; {OCR_APP_NAME} queue accounting is CPU-only"
        )
    return resource


def _validate_cohort_spec(spec):
    require(isinstance(spec, dict) and set(spec) <= {
        "app", "platform", "resource", "low_watermark", "high_watermark", "pages_per_host",
    }, "invalid cohort configuration")
    if "app" in spec:
        normalize_cohort_apps(spec["app"])
    if "platform" in spec:
        normalize_cohort_platform(spec["platform"])
    if "resource" in spec:
        normalize_cohort_resource(spec["resource"])
    if "pages_per_host" in spec:
        require(type(spec["pages_per_host"]) is int and 1 <= spec["pages_per_host"] <= 100,
                "invalid cohort pages_per_host")
    if "high_watermark" in spec:
        require(type(spec["high_watermark"]) is int and 1 <= spec["high_watermark"] <= MAX_JOBS,
                "invalid cohort high_watermark")
    if "low_watermark" in spec:
        require(type(spec["low_watermark"]) is int and 0 <= spec["low_watermark"] <= MAX_JOBS,
                "invalid cohort low_watermark")
    if "low_watermark" in spec and "high_watermark" in spec:
        require(spec["low_watermark"] <= spec["high_watermark"],
                "low_watermark cannot exceed high_watermark")


def parse_zero_work_replies(raw):
    """Count zero-work ('No tasks sent') scheduler replies from an int, host list, or log text."""
    if raw is None:
        return 0
    if type(raw) is int:
        return _nonneg_int(raw, MAX_STATUS_COUNT, "invalid zero_work_requests count")
    if isinstance(raw, list):
        require(len(raw) <= MAX_JOBS, "oversized zero_work_hosts list")
        return len(raw)
    if isinstance(raw, str):
        require(len(raw) <= MAX_STATUS_BYTES, "oversized scheduler log")
        return len(ZERO_WORK_LINE.findall(raw))
    raise ValueError("invalid zero_work_requests input")


def parse_server_status(raw):
    """Parse per-app queue counts from BOINC server_status.php?xml=1 or structured status."""
    if isinstance(raw, (bytes, bytearray)):
        require(len(raw) <= MAX_STATUS_BYTES, "oversized BOINC status XML")
        try:
            text = bytes(raw).decode("utf-8")
        except UnicodeDecodeError as error:
            raise ValueError("invalid BOINC status encoding") from error
        return parse_server_status(text)
    if isinstance(raw, str):
        require(len(raw.encode("utf-8")) <= MAX_STATUS_BYTES, "oversized BOINC status payload")
        stripped = raw.strip()
        if stripped.startswith("{"):
            return parse_server_status(json.loads(stripped))
        upper = stripped.upper()
        require("<!DOCTYPE" not in upper and "<!ENTITY" not in upper, "unsafe BOINC status XML")
        try:
            root = ET.fromstring(stripped)
        except ET.ParseError as error:
            raise ValueError("invalid BOINC status XML") from error
        require(root.tag == "server_status", "expected server_status XML root")
        database = root.find("database_file_states")
        container = database if database is not None else root

        def xml_int(element, tag, required=True):
            value = element.findtext(tag)
            if value is None:
                require(not required, f"missing BOINC status <{tag}>")
                return None
            require(value.isascii() and value.isdecimal(), f"invalid BOINC status <{tag}>")
            return _nonneg_int(int(value), MAX_STATUS_COUNT, f"out-of-range BOINC status <{tag}>")

        apps_nodes = container.findall("tasks_by_app/app")
        if not apps_nodes and container is not root:
            apps_nodes = root.findall(".//tasks_by_app/app")
        require(len(apps_nodes) <= 100, "oversized BOINC app inventory")
        apps = {}
        for app_node in apps_nodes:
            name = app_node.findtext("name", "")
            require(APP_NAME.fullmatch(name) and name not in apps, "invalid/duplicate BOINC app")
            unsent = xml_int(app_node, "unsent")
            in_progress = xml_int(app_node, "in_progress")
            apps[name] = {
                "unsent": unsent,
                "in_progress": in_progress,
                "runnable": unsent + in_progress,
            }
        ready = xml_int(container, "results_ready_to_send", required=False)
        in_prog = xml_int(container, "results_in_progress", required=False)
        zero_work = xml_int(container, "zero_work_requests", required=False)
        if zero_work is None:
            zero_work = xml_int(container, "no_tasks_sent", required=False) or 0
        return {
            "apps": apps,
            "results_ready_to_send": sum(a["unsent"] for a in apps.values()) if ready is None else ready,
            "results_in_progress": sum(a["in_progress"] for a in apps.values()) if in_prog is None else in_prog,
            "zero_work_requests": zero_work,
            "cohorts": {},
        }
    require(isinstance(raw, dict), "invalid server_status payload")
    raw_apps = raw.get("apps")
    if raw_apps is None:
        raw_apps = raw.get("tasks_by_app")
    if raw_apps is None:
        reserved_keys = {
            "results_ready_to_send", "results_in_progress", "zero_work_requests",
            "no_tasks_sent", "zero_work_hosts", "scheduler_log", "cohorts",
        }
        raw_apps = {k: v for k, v in raw.items() if k not in reserved_keys and isinstance(v, dict)}
    apps = {}
    if isinstance(raw_apps, list):
        require(len(raw_apps) <= 100, "oversized BOINC app inventory")
        for item in raw_apps:
            require(isinstance(item, dict) and isinstance(item.get("name"), str), "invalid BOINC app item")
            name = item["name"]
            require(APP_NAME.fullmatch(name) and name not in apps, "invalid/duplicate BOINC app")
            require("unsent" in item and "in_progress" in item, "missing BOINC app queue counters")
            unsent = _nonneg_int(item["unsent"], MAX_STATUS_COUNT, "invalid app unsent count")
            in_progress = _nonneg_int(item["in_progress"], MAX_STATUS_COUNT,
                                      "invalid app in_progress count")
            apps[name] = {"unsent": unsent, "in_progress": in_progress, "runnable": unsent + in_progress}
    elif isinstance(raw_apps, dict):
        require(len(raw_apps) <= 100, "oversized BOINC app inventory")
        for name, item in raw_apps.items():
            require(isinstance(name, str) and APP_NAME.fullmatch(name) and isinstance(item, dict),
                    "invalid BOINC app entry")
            require("unsent" in item and "in_progress" in item, "missing BOINC app queue counters")
            unsent = _nonneg_int(item["unsent"], MAX_STATUS_COUNT, "invalid app unsent count")
            in_progress = _nonneg_int(item["in_progress"], MAX_STATUS_COUNT,
                                      "invalid app in_progress count")
            apps[name] = {"unsent": unsent, "in_progress": in_progress, "runnable": unsent + in_progress}
    else:
        raise ValueError("invalid BOINC apps structure")
    ready = raw.get("results_ready_to_send")
    in_prog = raw.get("results_in_progress")
    zero_work = parse_zero_work_replies(
        raw.get("zero_work_requests")
        if "zero_work_requests" in raw
        else raw.get("no_tasks_sent", raw.get("zero_work_hosts", raw.get("scheduler_log")))
    )
    cohorts = raw.get("cohorts") or {}
    require(isinstance(cohorts, dict) and len(cohorts) <= 32, "invalid status cohorts")
    return {
        "apps": apps,
        "results_ready_to_send": (sum(a["unsent"] for a in apps.values())
                                  if ready is None else _nonneg_int(ready, MAX_STATUS_COUNT)),
        "results_in_progress": (sum(a["in_progress"] for a in apps.values())
                                if in_prog is None else _nonneg_int(in_prog, MAX_STATUS_COUNT)),
        "zero_work_requests": zero_work,
        "cohorts": cohorts,
    }


parse_server_status_xml = parse_server_status


def _validate_commands(config, keys):
    for key in keys:
        if key not in config:
            continue
        command = config[key]
        require(isinstance(command, list) and command and
                all(isinstance(arg, str) and re.fullmatch(r"[A-Za-z0-9_/@=.:+-]+", arg)
                    for arg in command), f"{key} must be a trusted, literal argv prefix")


def _load_surya_config(config):
    require(SURYA_FIELDS <= set(config) <= (SURYA_FIELDS | SURYA_OPTIONAL)
            and config["schema_version"] == 1, "invalid surya config fields/version")
    for key, pattern in (("env", r"[a-z][a-z0-9-]{0,30}"), ("plan", r"[A-Za-z0-9-]{1,64}")):
        require(isinstance(config[key], str) and re.fullmatch(pattern, config[key]), f"invalid {key}")
    # Both marks are required: the tick tops the queue up to the high one.
    _validate_cohort_spec({k: config[k] for k in ("low_watermark", "high_watermark")})
    surya = config["surya"]
    require(isinstance(surya, dict) and SURYA_REQUIRED <= set(surya) <= (SURYA_REQUIRED | set(SURYA_DEFAULTS)),
            "invalid surya fields")
    for key in ("campaign", "quality_campaign"):
        require(isinstance(surya[key], str) and re.fullmatch(r"[a-z0-9][a-z0-9-]{0,40}", surya[key]),
                f"invalid surya {key}")
    for key in ("engine_image", "engine_llama", "engine_weights"):
        require(isinstance(surya[key], str) and re.fullmatch(r"sha256:[0-9a-f]{64}", surya[key]),
                f"invalid surya {key}")
    for key, low, high in (("timeout_secs", 60, 14400), ("stage_pages", 1, 500), ("submit_units", 1, 2000)):
        if key in surya:
            require(type(surya[key]) is int and low <= surya[key] <= high, f"invalid surya {key}")
    if "server_status_url" in config:
        _validate_status_url(config["server_status_url"])
    _validate_commands(config, ("step_command",))
    return config


def _validate_gold_onboarding(entries):
    require(isinstance(entries, list) and 1 <= len(entries) <= 4, "gold_onboarding is a list of 1 to 4 scopes")
    seen = set()
    for entry in entries:
        require(isinstance(entry, dict) and GOLD_REQUIRED <= set(entry) <= (GOLD_REQUIRED | GOLD_OPTIONAL),
                "invalid gold_onboarding fields")
        for key, pattern in (("campaign", r"[a-z0-9][a-z0-9-]{0,40}"), ("language", r"[a-z]{3}"),
                             ("profile", r"[A-Za-z0-9_+-]{1,64}"),
                             # The worker's TESSERACT_IMAGE as the gold reports record it: a bare
                             # digest (what calibration and bridge-job use) or name@digest.
                             ("engine_base", r"(?:[A-Za-z0-9._/:-]{1,180}@)?sha256:[0-9a-f]{64}"),
                             ("engine_version", r"[0-9]{1,3}(\.[0-9]{1,3}){1,3}"),
                             # A pinned worker digest, as the plan's own `image`.
                             ("image", r"sha256:[0-9a-f]{64}")):
            if key in entry:
                require(isinstance(entry[key], str) and re.fullmatch(pattern, entry[key]),
                        f"invalid gold_onboarding {key}")
        for key, low, high in (("max_hosts", 1, 100), ("interval_minutes", 10, 1440)):
            if key in entry:
                require(type(entry[key]) is int and low <= entry[key] <= high,
                        f"gold_onboarding {key} is {low} to {high}")
        scope = (entry["language"], entry["profile"])
        require(scope not in seen, "one gold_onboarding entry per language and profile")
        seen.add(scope)


def load_config(path):
    config = json.loads(path.read_text())
    if isinstance(config, dict) and config.get("app") == SURYA_APP_NAME:
        return _load_surya_config(config)
    queue = config.get("source") == "queue"
    required = QUEUE_FIELDS if queue else PAGES_FIELDS
    optional = OPTIONAL_FIELDS | ({"claim_languages"} if queue else set())
    require(required <= set(config) <= (required | optional) and config["schema_version"] == 1,
            "invalid config fields/version")
    for key, pattern in (
        ("env", r"[a-z][a-z0-9-]{0,30}"), ("account", r"[a-z0-9]{3,24}"),
        ("plan", r"[A-Za-z0-9-]{1,64}"),
        ("image", r"sha256:[0-9a-f]{64}"),
        ("manifest", r"[A-Za-z0-9][A-Za-z0-9._/-]*"),
        ("scans_root", r"/[A-Za-z0-9_./-]+"), ("manifests_root", r"/[A-Za-z0-9_./-]+"),
    ):
        require(isinstance(config[key], str) and re.fullmatch(pattern, config[key])
                and ".." not in config[key], f"invalid {key}")
    require(not config["manifest"].startswith("/") and
            config["manifest"].endswith("/manifest.parquet"), "invalid manifest snapshot path")
    for key, maximum in (("batch_size", MAX_TICK_PAGES), ("max_pending", 10000), ("pages_per_host", 100)):
        require(type(config[key]) is int and 1 <= config[key] <= maximum, f"invalid {key}")
    if "app" in config or "platform" in config or "resource" in config or \
            "low_watermark" in config or "high_watermark" in config:
        _validate_cohort_spec({
            k: config[k] for k in ("app", "platform", "resource", "low_watermark", "high_watermark")
            if k in config
        })
    if "cohort" in config:
        require(isinstance(config["cohort"], str) and
                re.fullmatch(r"[A-Za-z0-9_.:-]{1,96}", config["cohort"]), "invalid cohort")
    if "cohorts" in config:
        require(isinstance(config["cohorts"], dict) and 1 <= len(config["cohorts"]) <= 32,
                "invalid cohorts")
        for cohort_key, cohort_spec in config["cohorts"].items():
            require(isinstance(cohort_key, str) and
                    re.fullmatch(r"[A-Za-z0-9_.:-]{1,96}", cohort_key), "invalid cohort key")
            _validate_cohort_spec(cohort_spec)
    if "max_global_pending" in config:
        require(type(config["max_global_pending"]) is int and
                1 <= config["max_global_pending"] <= MAX_GLOBAL_PENDING, "invalid max_global_pending")
    if "server_status_url" in config:
        _validate_status_url(config["server_status_url"])
    _validate_commands(config, ("push_command", "step_command"))
    if "handoff_root" in config:
        require(queue and isinstance(config["handoff_root"], str) and
                re.fullmatch(r"/[A-Za-z0-9_./-]+", config["handoff_root"]) and
                ".." not in config["handoff_root"] and config["handoff_root"] != "/",
                "local handoff_root requires a queue plan and an absolute directory")
    if "gold_onboarding" in config:
        _validate_gold_onboarding(config["gold_onboarding"])
    _validate_native_plan(config)
    if "pages_per_unit" in config:
        units = config["pages_per_unit"]
        require(isinstance(units, dict) and units and set(units) <= set(QUEUES) and
                all(type(n) is int and 1 <= n <= MAX_PAGES_PER_UNIT for n in units.values()),
                f"pages_per_unit maps ocr-bulk and/or ocr-priority to 1 to {MAX_PAGES_PER_UNIT}")
    if "bundle_app_version" in config:
        require(type(config["bundle_app_version"]) is int and
                1 <= config["bundle_app_version"] <= 999999999, "invalid bundle_app_version")
    require(max(config.get("pages_per_unit", {}).values(), default=1) == 1 or
            "bundle_app_version" in config,
            "bundles need bundle_app_version, the usnm_ocr version_num whose worker reads them")
    if queue:
        # BOINC is a second consumer of the work queues, beside the on-prem nodes
        # (docs/boinc/server/batch-scheduler.md#queue-fed-work). The page list is whatever it claims.
        require(config["source"] == "queue", "invalid source")
        queues = config["claim_queues"]
        require(isinstance(queues, list) and queues and len(set(queues)) == len(queues)
                and all(q in QUEUES for q in queues), "claim_queues is ocr-bulk and/or ocr-priority")
        require(type(config["lease_hours"]) is int and 1 <= config["lease_hours"] <= 168,
                "lease_hours is 1 to 168")
        # Optional: only pages in these source languages, the trust calibration's scope (#866).
        languages = config.get("claim_languages")
        require(languages is None or (
            isinstance(languages, list) and languages and len(set(languages)) == len(languages)
            and all(isinstance(l, str) and re.fullmatch(r"[a-z]{3}", l) for l in languages)),
            "claim_languages is a non-empty list of three-letter codes")
        config["pages"] = []
        return config
    for key, pattern in (("batch", r"[a-z]{2,10}_[A-Za-z0-9_]+"),):
        require(isinstance(config[key], str) and re.fullmatch(pattern, config[key])
                and ".." not in config[key], f"invalid {key}")
    require(len(config["batch"]) <= 128 and not re.search(r"_ver[0-9]+$", config["batch"]),
            "batch must be an unversioned base batch of at most 128 bytes")
    require(isinstance(config["pages"], list) and 1 <= len(config["pages"]) <= MAX_JOBS,
            "allocate 1-10000 explicit pages")
    seen = set()
    for page in config["pages"]:
        require(set(page) == {"page", "scan"}, "invalid page fields")
        require(isinstance(page["page"], str) and PAGE_KEY.fullmatch(page["page"]),
                "invalid page key")
        require(page["page"] not in seen, "duplicate page allocation")
        seen.add(page["page"])
        scan = page["scan"]
        require(isinstance(scan, str) and re.fullmatch(r"/[A-Za-z0-9_./-]+", scan)
                and ".." not in scan and scan.startswith(config["scans_root"].rstrip("/") + "/keep/")
                and scan.endswith((".jp2", ".jpg", ".jpeg", ".png")), "invalid cached scan path")
    return config


# Not allocation: turning these on or changing them needs no migration. A native plan's hosts
# (#1780) are widened one at a time, by hand.
UNALLOCATED = ("gold_onboarding", "native_hosts")


def digest(config):
    allocation = {key: value for key, value in config.items() if key not in UNALLOCATED}
    return hashlib.sha256(json.dumps(allocation, sort_keys=True).encode()).hexdigest()


def save(path, state):
    staging = path.with_suffix(".new")
    with staging.open("w") as out:
        json.dump(state, out, sort_keys=True)
        out.write("\n")
        out.flush()
        os.fsync(out.fileno())
    os.replace(staging, path)
    fd = os.open(path.parent, os.O_RDONLY)
    try:
        os.fsync(fd)
    finally:
        os.close(fd)


class Timings:
    """Where a tick's time goes (#902): wall and CPU seconds for each phase (inventory,
    journals, collect, claim, push, export, submit, ...) and for each kind of command the
    transport runs (one bridge or hand-off step, one storage call, one scan push), with how
    many ran and how many failed. CPU counts this process and the commands it waited for,
    such as the az CLI under each step. Each phase is reported as it ends, so a long tick
    shows its progress in the journal before its summary line."""

    def __init__(self, report=None, clock=time.monotonic, cpu=None):
        self.report = report
        self.clock = clock
        self.cpu = cpu or (lambda: sum(os.times()[:4]))
        self.started = clock()
        self.cpu_started = self.cpu()
        self.phases = {}
        self.commands = {}
        self.execution_routes = {}
        self.scan_pages = {}
        self.current = None

    @staticmethod
    def _add(table, key, seconds, cpu, ok):
        entry = table.setdefault(key, {"count": 0, "failed": 0, "seconds": 0.0, "cpu_seconds": 0.0})
        entry["count"] += 1
        entry["failed"] += not ok
        entry["seconds"] = round(entry["seconds"] + seconds, 3)
        entry["cpu_seconds"] = round(entry["cpu_seconds"] + cpu, 3)

    @contextlib.contextmanager
    def phase(self, name):
        if self.current is not None:
            # Inside another phase (collect_batch calling collect): that one holds the time.
            yield
            return
        self.current = name
        start, cpu = self.clock(), self.cpu()
        ran = sum(entry["count"] for entry in self.commands.values())
        ok = False
        try:
            yield
            ok = True
        finally:
            self.current = None
            seconds, cpu = self.clock() - start, self.cpu() - cpu
            self._add(self.phases, name, seconds, cpu, ok)
            if self.report:
                try:
                    self.report({
                        "event": "boinc_batch_phase", "schema_version": 1, "at": int(time.time()),
                        "phase": name, "ok": ok, "seconds": round(seconds, 3),
                        "cpu_seconds": round(cpu, 3),
                        "commands": sum(entry["count"] for entry in self.commands.values()) - ran,
                    })
                except (OSError, ValueError):
                    # A closed or broken log stream never stops a tick or strands its lease.
                    pass

    def command(self, key, seconds, cpu, ok, execution_route=None):
        self._add(self.commands, key, seconds, cpu, ok)
        if execution_route is not None:
            self._add(self.execution_routes, execution_route, seconds, cpu, ok)

    def summary(self):
        return {
            "tick_seconds": round(self.clock() - self.started, 3),
            "cpu_seconds": round(self.cpu() - self.cpu_started, 3),
            "phases": copy.deepcopy(self.phases),
            "commands": copy.deepcopy(self.commands),
            "execution_routes": copy.deepcopy(self.execution_routes),
            "scan_pages": copy.deepcopy(self.scan_pages),
        }


def timed(phase):
    """Run a Transport method as one phase of its Timings."""
    def wrap(method):
        @functools.wraps(method)
        def run(self, *args, **kwargs):
            with self.timings.phase(phase):
                return method(self, *args, **kwargs)
        return run
    return wrap


class StepLockTimeout(RuntimeError):
    """A step that never started: the BOINC host's step lock stayed held for the whole lock wait
    (`onprem-step.sh` refused before running anything)."""


# Steps that take the host's shared step lock (usnm-boinc-step.lock), where a Qwen ship or collect
# holds it for 5 to 17 minutes. With --step-lock-wait-secs they ask for a longer wait than the
# host's own default (300 s), per step, so the other steps keep the host's setting.
LONG_LOCK_STEPS = {("bridge", "submit-remote"), ("bridge", "gold-onboard"), ("bridge", "gold-promote")}
MAX_STEP_LOCK_WAIT_SECS = 1200
# A native plan retries a submit that only timed out on the step lock this many ticks in a row
# before it pauses like any other failed submit.
NATIVE_LOCK_RETRY_LIMIT = 6
LOCK_TIMEOUT_LINE = re.compile(
    r"error: another bridge step (still holds the lock after \d+s|is running on this host\b.*)")


SSH_NOTICE_LINE = re.compile(r"Warning: Permanently added .*|Connection to .* closed\.?")


def step_lock_timeout(returncode, stderr):
    """True when a failed step's stderr says only that onprem-step.sh gave up on the lock, before
    it started a container: that one line, alone or with ssh's own notices."""
    if returncode != 1 or not isinstance(stderr, str):
        return False
    lines = [line.strip() for line in stderr.splitlines() if line.strip()]
    # Besides that line, only ssh's own notices: any other line (flock's "No locks available",
    # a step's output) means something else went wrong.
    others = [line for line in lines if not LOCK_TIMEOUT_LINE.fullmatch(line)]
    return (len(others) == len(lines) - 1
            and all(SSH_NOTICE_LINE.fullmatch(line) for line in others))


class ClaimFailed(RuntimeError):
    """The claim step failed after taking messages: `claims` are the ones it did take."""

    def __init__(self, message, claims):
        super().__init__(message)
        self.claims = claims


def chunks(items, size=None):
    """`items` in order, in lists of at most `size` (STEP_IDS by default)."""
    size = STEP_IDS if size is None else size
    items = list(items)
    return [items[i:i + size] for i in range(0, len(items), size)]


def unit_steps(units):
    """Workunits (lists of job ids) in order, grouped so no group holds more than STEP_IDS ids.
    A unit is never split: JOB_UNITS keeps each bundle whole."""
    groups, current, size = [], [], 0
    for unit in units:
        if current and size + len(unit) > STEP_IDS:
            groups.append(current)
            current, size = [], 0
        current.append(unit)
        size += len(unit)
    if current:
        groups.append(current)
    return groups


# --onprem-handoff choices and the hand-off steps each one moves to the on-prem host. A claim's
# release goes with it: both take pages off and back onto the work queues.
# Most base batches one batched export execution takes (--batch-export): each is about half a
# minute of the hand-off job's 30-minute timeout.
EXPORT_BATCHES = 20
# Every bridge and hand-off step takes at most STEP_IDS pages, jobs or doc ids per execution.
# A tick may handle up to MAX_TICK_PAGES; the transport splits each list into steps of STEP_IDS.
STEP_IDS = 100
# A tick's claim goes on past a step that took fewer pages than it asked for when that step read
# a queue to its receive budget (the queue may hold usable pages further back: settled held pages
# and unusable ones are read past, not claimed), but starts no new step after this many seconds
# of claiming, so the tick stays well inside its 15 minutes.
CLAIM_SECONDS = 300
# Each plan's list of pages whose held result is settled (archived_held, its own and the
# --check-state-dir plans'), each with its job's message sha256, as <prefix><plan>.<NNNN>.json: the claim
# step reads every plan's list and deletes such a message when it comes back instead of claiming
# it again (usnm-boinc claim::HELD_PAGES_PREFIX). One file per plan, so no plan's list replaces
# another's.
HELD_PAGES_PREFIX = "scheduler/held-pages/"
# Pages in one part of a plan's held-pages list: about 7 MB, well inside the reader's 64 MiB a
# part, so the 200,000 archived pages of each of several plans never make a part it refuses.
HELD_PAGES_PER_PART = 50000
MAX_TICK_PAGES = 500
# --rest-journals: requests at once, and the most changed journals a tick fetches one by one
# before it takes one az download-batch of all of them (at this rate, a minute's work).
JOURNAL_REQUESTS = 16
JOURNAL_DELTA_REST = 2000
# --rest-journals lists each digit's journals with this delimiter (#308). A job's directory
# holds record.json, request.json and accepted/*, and only record.json is read. Each listed
# blob costs about 760 bytes of the account's egress, a grouped prefix about 110. A name
# after the prefix is the rest of the hex job id and then the file, so with "t" the first
# "t" falls in "accepted/" or "request.json" and those group into one prefix each per job,
# while record.json has no "t" and is listed whole, with its ETag. On 6 November 2026 one
# digit's listing went from 1,079,496 bytes to 314,199, with the same records and ETags.
JOURNAL_LIST_DELIMITER = "t"
# Every journal name must reach the listing as a blob, never inside a prefix.
assert not set(JOURNAL_LIST_DELIMITER) & set("0123456789abcdef/record.json")

ONPREM_HANDOFF = {"claim": ("claim", "release", "dead-letter"), "collect": ("collect",),
                  "export": ("export",)}
# The local expiring lock (--local-lock, #1367): the BOINC host holds the record, the controller
# renews it every LOCK_HEARTBEAT_SECS, and a record unrenewed for LOCK_TTL_SECS is free.
LOCK_HEARTBEAT_SECS = 30
LOCK_TTL_SECS = 300
# The controller stops dispatching this long before the lease could lapse, covering a step
# already in flight and clock rounding; the host's clock decides, this one is conservative.
LOCK_DISPATCH_MARGIN_SECS = 150
# The hand-off steps that change state; with the local lock each carries the holder and
# generation, and the host refuses it if the record has moved on.
FENCED_HANDOFF_STEPS = frozenset(("claim", "release", "dead-letter", "collect", "export"))
# The bridge steps that write the hand-off root (spool-out, the inventory) or dispatch work.
FENCED_BRIDGE_STEPS = frozenset(("spool-out", "submit-remote", "batch-inventory"))


class Transport:
    def __init__(self, config, directory, *, status_url=None, status_file=None,
                 batch_submit=False, batch_spool_out=False, batch_collect=False, batch_export=False,
                 onprem_handoff=(), timings=None, rest_journals=False, collect_parallel=1,
                 skip_truncated=False, extra_claim_languages=(), step_lock_wait_secs=0,
                 local_lock=False):
        self.config = config
        # --local-lock: the controller lock is a record on the BOINC host with a heartbeat, not
        # an Azure lease or a local flock. Off by default.
        require(not local_lock or (config.get("step_command") and set(onprem_handoff) == set(ONPREM_HANDOFF)),
                "--local-lock needs the config's step_command and every hand-off step on-prem (claim, collect, export)")
        # It replaces the Azure lease that archive, restore and reconcile still take, so it is
        # only for a plan whose hand-off root is not in Azure.
        require(not local_lock or "handoff_root" in config,
                "--local-lock is for a plan with a local handoff_root, not one backed by Azure")
        self.local_lock = local_lock
        self.lock_fence = None
        self.lock_lost = None
        self.lock_deadline = None
        # --step-lock-wait-secs: how long LONG_LOCK_STEPS wait for the host's step lock (0: the host's own).
        require(0 <= step_lock_wait_secs <= MAX_STEP_LOCK_WAIT_SECS, "invalid --step-lock-wait-secs")
        self.step_lock_wait_secs = step_lock_wait_secs
        # Languages the claim takes beside the configuration's claim_languages (--claim-language):
        # a flag, so adding one doesn't change the allocation digest. It can only widen an
        # explicit list; without claim_languages the claim takes every routable language anyway.
        extra = list(extra_claim_languages)
        listed = config.get("claim_languages")
        require(not extra or (
            listed and len(set(extra)) == len(extra) and not set(extra) & set(listed)
            and all(isinstance(l, str) and re.fullmatch(r"[a-z]{3}", l) for l in extra)),
            "--claim-language adds new three-letter codes to the configuration's claim_languages")
        self.extra_claim_languages = extra
        self.directory = directory
        self.status_url = status_url
        self.status_file = status_file
        # One job execution for a whole tick's submissions, spool-outs or collects (#932),
        # instead of one per page. Each needs the image whose step takes JOB_IDS: off until
        # that image is live.
        self.batch_submit = batch_submit
        self.batch_spool_out = batch_spool_out
        self.batch_collect = batch_collect
        # Jobs the batched collect runs at once (COLLECT_PARALLEL, #1447): one by default.
        require(1 <= collect_parallel <= MAX_COLLECT_PARALLEL and (collect_parallel == 1 or batch_collect),
                f"collect_parallel is 1 to {MAX_COLLECT_PARALLEL}, and above 1 only with batch_collect")
        self.collect_parallel = collect_parallel
        # One hand-off execution for all of a tick's export batches (HANDOFF_BATCHES), instead of
        # one per base batch: queued pages come from many batches.
        self.batch_export = batch_export
        # List and fetch journals over the storage REST API, not one az process per list or file
        # (--rest-journals): a list of the dispatch prefix costs az half a minute of CPU.
        self.rest_journals = rest_journals
        # Check each claimed page's JP2 headers before it is pushed, and dead-letter one LoC holds
        # cut short instead (push-boinc-scan.sh --skip-truncated, #1457). Off until the hand-off
        # image whose dead-letter step takes SOURCE_TRUNCATED_DOCS is live.
        self.skip_truncated = skip_truncated
        self._storage_token = None
        # The hand-off steps that run on the on-prem BOINC host through step_command
        # (usnm-boinc-step --target handoff, #967 phase 3) instead of the Azure hand-off job:
        # "claim" (with its release), "collect" and/or "export". Off by default.
        unknown = set(onprem_handoff) - set(ONPREM_HANDOFF)
        require(not unknown, "onprem_handoff is claim, collect and/or export")
        require(not onprem_handoff or config.get("step_command"),
                "on-prem hand-off steps need the config's step_command")
        self.onprem_steps = {step for choice in onprem_handoff for step in ONPREM_HANDOFF[choice]}
        self.handoff_root = Path(config["handoff_root"]) if "handoff_root" in config else None
        if self.handoff_root is not None:
            require(set(onprem_handoff) == set(ONPREM_HANDOFF),
                    "local handoff needs every hand-off step on-prem")
            require(self.handoff_root.is_dir() and
                    self.handoff_root.resolve() == self.handoff_root,
                    "local handoff root must exist and contain no symlinks")
            info = self.handoff_root.stat()
            require(info.st_uid == os.geteuid() and info.st_mode & 0o777 == 0o700,
                    "local handoff root must be owned by the controller user with mode 0700")
        self.timings = timings or Timings()
        # The last claim step's summary: whether it stopped at its receive budget.
        self.last_claim_summary = None
        self._journal_snapshot = None
        self._journal_records = None

    @property
    def handoff_url(self):
        if self.handoff_root is not None:
            return "/handoff-state"
        return f"https://{self.config['account']}.blob.core.windows.net/boinc-handoff"

    def retain_failure(self, label, *, returncode=None, error=None, stdout=None, stderr=None):
        def tail(value):
            if isinstance(value, bytes):
                value = value.decode("utf-8", errors="replace")
            return (value or "")[-MAX_FAILURE_OUTPUT_CHARS:]

        receipt = {
            "schema_version": 1, "at": int(time.time()), "label": label,
            "returncode": returncode, "exception": type(error).__name__ if error else None,
            "stdout_tail": tail(stdout), "stderr_tail": tail(stderr),
        }
        temporary = None
        try:
            with tempfile.NamedTemporaryFile(
                mode="w", dir=self.directory, prefix=".command-failure-", delete=False
            ) as output:
                temporary = Path(output.name)
                json.dump(receipt, output, sort_keys=True)
                output.write("\n")
                output.flush()
                os.fsync(output.fileno())
            os.replace(temporary, self.directory / "last-command-failure.json")
            fd = os.open(self.directory, os.O_RDONLY)
            try:
                os.fsync(fd)
            finally:
                os.close(fd)
        except OSError as failure:
            raise FailureReceiptError(f"{label} failed; private command failure receipt could not be saved") from failure
        finally:
            if temporary is not None:
                try:
                    temporary.unlink(missing_ok=True)
                except OSError as failure:
                    raise FailureReceiptError(
                        f"{label} failed; private command failure receipt cleanup failed"
                    ) from failure

    def run(self, argv, label, timeout=2100, ok=(0,), *, execution_route=None):
        # In the tick's timings a storage call goes by its az verb (list, download, lease, ...).
        kind = f"storage {argv[3]}" if argv[:3] == ["az", "storage", "blob"] and len(argv) > 3 else label
        start, cpu, passed = self.timings.clock(), self.timings.cpu(), False
        try:
            done = subprocess.run(argv, check=False, text=True, encoding="utf-8", errors="replace", stdout=subprocess.PIPE,
                                  stderr=subprocess.PIPE, timeout=timeout, cwd=REPO)
            passed = done.returncode in ok
        except (OSError, subprocess.SubprocessError) as error:
            self.retain_failure(
                label, error=error, stdout=getattr(error, "stdout", None),
                stderr=getattr(error, "stderr", None),
            )
            raise RuntimeError(f"{label} failed; inspect the execution privately before retrying") from error
        finally:
            seconds, used = self.timings.clock() - start, self.timings.cpu() - cpu
            self.timings.command(kind, seconds, used, passed, execution_route)
        if not passed:
            self.retain_failure(label, returncode=done.returncode, stdout=done.stdout, stderr=done.stderr)
            if step_lock_timeout(done.returncode, done.stderr):
                raise StepLockTimeout(f"{label} failed: the BOINC host's step lock stayed held; "
                                      "nothing ran")
            raise RuntimeError(f"{label} failed; inspect the execution privately before retrying")
        return done.stdout

    def az(self, *args):
        require(self.handoff_root is None, "Azure handoff access forbidden in local mode")
        return self.run(["az", "storage", "blob", *args, "--auth-mode", "login",
                         "--account-name", self.config["account"], "--container-name",
                         "boinc-handoff", "--only-show-errors"], "hand-off storage")

    def blob(self, name):
        if self.handoff_root is not None:
            path = self._local_object(name)
            require(path.stat().st_size <= 16 * 1024 * 1024, "hand-off JSON exceeds limit")
            return json.loads(path.read_text())
        path = self.directory / "read.json"
        self.az("download", "--name", name, "--file", str(path), "--overwrite", "true", "-o", "none")
        require(path.stat().st_size <= 16 * 1024 * 1024, "hand-off JSON exceeds limit")
        return json.loads(path.read_text())

    def deadline_retry(self, job_id, message_sha256):
        """The operator's deadline retry of a dead-lettered job's page for its newer message
        (`usnm-boinc claim deadline-retry`, #1417), or None. Only a claim conflict asks."""
        require(ID.fullmatch(job_id) and ID.fullmatch(message_sha256), "invalid deadline retry lookup")
        name = f"dispatch/jobs/{job_id}/deadline-retry/{message_sha256}.json"
        if self.handoff_root is not None:
            path = self.handoff_root / name
            if not path.exists():
                return None
            return self.blob(name)
        if self.az("exists", "--name", name, "--query", "exists", "-o", "tsv").strip() != "true":
            return None
        return self.blob(name)

    def _local_object(self, name):
        require(re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9._-]*(/[A-Za-z0-9][A-Za-z0-9._-]*)*", name)
                and all(part not in (".", "..") for part in name.split("/")),
                "invalid local handoff object path")
        path = self.handoff_root / name
        require(path.resolve() == path and stat.S_ISREG(path.stat().st_mode),
                "missing or unsafe local handoff object")
        return path

    def _step_argv(self, target, step, variables):
        # step_command runs the bridge steps somewhere other than the Azure bridge job, such as
        # the on-prem BOINC host's boinc/scripts/onprem-step.sh over ssh (#942), with the same
        # arguments. Hand-off steps stay on the Azure launcher unless chosen with
        # onprem_handoff (--onprem-handoff), which sends them there too as --target handoff.
        # The lock steps (--local-lock) always run there.
        command = ["bash", str(REPO / "boinc/scripts/start-boinc-bridge.sh")]
        lock_step = target == "handoff" and step.startswith("lock-")
        onprem = target == "bridge" or (target == "handoff" and (step in self.onprem_steps or lock_step))
        if onprem and self.config.get("step_command"):
            command = self.config["step_command"]
        variables = dict(variables)
        if target == "handoff" and onprem and not lock_step:
            variables["EXPECTED_HANDOFF_URL"] = self.handoff_url
        if (((target == "handoff" and step in FENCED_HANDOFF_STEPS)
             or (target == "bridge" and step in FENCED_BRIDGE_STEPS)) and onprem
                and self.lock_fence is not None):
            variables["LOCK_HOLDER"], variables["LOCK_GENERATION"] = self.lock_fence
        wait_mins, timeout, extra = (2, 120, []) if lock_step else (30, 2100, [])
        if (onprem and self.config.get("step_command") and self.step_lock_wait_secs
                and (target, step) in LONG_LOCK_STEPS):
            # The wait comes out of --wait-mins, so it is added to it: the step keeps its 30
            # minutes after the lock is won. The command's own timeout follows.
            wait_mins += -(-self.step_lock_wait_secs // 60)
            timeout += self.step_lock_wait_secs + 60
            extra = ["--lock-wait-secs", str(self.step_lock_wait_secs)]
        argv = command + ["--env", self.config["env"],
                          "--target", target, "--step", step, "--wait-mins", str(wait_mins)] + extra
        for key, value in variables.items():
            argv += ["--var", f"{key}={value}"]
        route = "onprem" if onprem and self.config.get("step_command") else "azure"
        return argv, timeout, f"{route}/{target}/{step}"

    @contextlib.contextmanager
    def _local_lock(self):
        plan = re.sub(r"[^A-Za-z0-9._-]", "-", str(self.config.get("plan", "controller")))[:60]
        holder = f"{plan}-{uuid.uuid4().hex[:12]}"
        acquire_started = time.monotonic()
        with self.timings.phase("lock"):
            out = self.step("handoff", "lock-acquire", LOCK_HOLDER=holder,
                            LOCK_HOST=re.sub(r"[^A-Za-z0-9._-]", "-", socket.gethostname()) or "controller",
                            LOCK_PID=os.getpid(), LOCK_TTL=LOCK_TTL_SECS)
        try:
            generation = int(json.loads(out.strip().splitlines()[-1])["generation"])
        except (ValueError, KeyError, IndexError, TypeError) as error:
            raise RuntimeError("lock-acquire returned no generation") from error
        self.lock_fence = (holder, generation)
        self.lock_lost = None
        # No dispatch after this (monotonic) time unless a renewal moved it on. It is measured
        # from when the acquire or renewal was sent, so a slow reply never lengthens it.
        self.lock_deadline = acquire_started + LOCK_TTL_SECS - LOCK_DISPATCH_MARGIN_SECS
        stop = threading.Event()

        def heartbeat():
            while not stop.wait(LOCK_HEARTBEAT_SECS):
                started = time.monotonic()
                argv, timeout, _ = self._step_argv(
                    "handoff", "lock-renew",
                    {"LOCK_HOLDER": holder, "LOCK_GENERATION": generation, "LOCK_TTL": LOCK_TTL_SECS})
                # An attempt may not outlast the lease: cap it by the time left on it.
                lease_end = self.lock_deadline + LOCK_DISPATCH_MARGIN_SECS
                timeout = max(1, min(timeout, int(lease_end - started)))
                try:
                    done = subprocess.run(argv, check=False, text=True, stdout=subprocess.PIPE,
                                          stderr=subprocess.PIPE, timeout=timeout, cwd=REPO)
                    ok, why = done.returncode == 0, done.stderr
                except (OSError, subprocess.SubprocessError) as error:
                    ok, why = False, str(error)
                if ok:
                    self.lock_deadline = started + LOCK_TTL_SECS - LOCK_DISPATCH_MARGIN_SECS
                elif "lock moved on" in why or "lock expired" in why:
                    self.lock_lost = "the controller lock was taken or expired"
                    return
                elif time.monotonic() >= self.lock_deadline + LOCK_DISPATCH_MARGIN_SECS:
                    self.lock_lost = f"no lock renewal for {LOCK_TTL_SECS} s"
                    return

        thread = threading.Thread(target=heartbeat, name="lock-heartbeat", daemon=True)
        thread.start()
        try:
            yield
        finally:
            stop.set()
            thread.join(timeout=5)
            self.lock_fence = None
            self.lock_deadline = None
            try:
                with self.timings.phase("unlock"):
                    self.step("handoff", "lock-release", LOCK_HOLDER=holder, LOCK_GENERATION=generation)
            except RuntimeError as error:
                # A killed or failed release expires by itself within the TTL.
                print(f"warning: lock-release failed ({error}); it expires in {LOCK_TTL_SECS} s",
                      file=sys.stderr)

    def lock_status(self):
        return self.step("handoff", "lock-status")

    @contextlib.contextmanager
    def lock(self):
        if self.local_lock:
            with self._local_lock():
                yield
            return
        if self.handoff_root is not None:
            with (self.handoff_root / ".controller.lock").open("a") as lock:
                fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
                yield
            return
        if self.config.get("app") == SURYA_APP_NAME:
            # A Surya plan touches no hand-off container: the host lock in main() and the
            # one-step-at-a-time lock of the bridge host are its guards.
            yield
            return
        name = "scheduler/controller.lock"
        lease = None
        failure = None
        try:
            with self.timings.phase("lock"):
                lease = self._acquire(name)
            yield
        except RuntimeError as error:
            failure = error
            raise
        finally:
            # Released whenever it was taken, even if something after the acquire failed.
            if lease is not None:
                try:
                    with self.timings.phase("unlock"):
                        self.az("lease", "release", "--blob-name", name, "--lease-id", lease, "-o", "none")
                except RuntimeError as error:
                    if failure is not None:
                        raise_transport_errors([error, failure])
                    raise

    def _acquire(self, name):
        if self.az("exists", "--name", name, "--query", "exists", "-o", "tsv").strip() != "true":
            marker = self.directory / "lease-marker.json"
            marker.write_text('{"schema_version":1}\n')
            self.az("upload", "--name", name, "--file", str(marker), "--overwrite", "false", "-o", "none")
        lease = uuid.uuid4().hex
        lease = str(uuid.UUID(lease))
        self.az("lease", "acquire", "--blob-name", name, "--lease-duration", "-1",
                "--proposed-lease-id", lease, "-o", "none")
        return lease

    def step(self, target, step, **variables):
        if self.lock_lost is not None and step not in ("lock-release", "lock-status"):
            raise RuntimeError(f"controller lock lost: {self.lock_lost}; the tick stops")
        if (self.lock_deadline is not None and step not in ("lock-release", "lock-status", "lock-renew")
                and time.monotonic() >= self.lock_deadline):
            self.lock_lost = f"no lock renewal inside {LOCK_TTL_SECS - LOCK_DISPATCH_MARGIN_SECS} s"
            raise RuntimeError(f"controller lock lost: {self.lock_lost}; the tick stops")
        argv, timeout, route = self._step_argv(target, step, variables)
        return self.run(argv, f"{target}/{step}", timeout=timeout, execution_route=route)

    @timed("inventory")
    def inventory(self):
        nonce = uuid.uuid4().hex
        self.step("bridge", "batch-inventory", BATCH_NONCE=nonce,
                  HANDOFF_URL=self.handoff_url)
        result = self.blob("scheduler/inventory.json")
        require(result.get("nonce") == nonce, "stale/concurrent inventory; another controller may be active")
        return result

    def storage_token(self):
        """A token for the storage REST API, from the identity az logged in with."""
        # az hands back its cached token, which may be near its end: go by the token's own expiry.
        if self._storage_token is None or self._storage_token[1] < time.time() + 600:
            token = json.loads(self.run(["az", "account", "get-access-token", "--resource",
                                         "https://storage.azure.com/", "--query",
                                         "{token:accessToken,expires:expires_on}", "-o", "json"], "storage token"))
            self._storage_token = (token["token"], int(token["expires"]))
        return self._storage_token[0]

    def storage_get(self, query):
        """GET a path and query of the hand-off container, retrying transient failures. The token
        goes in a header and never into an error."""
        require(self.handoff_root is None, "Azure handoff access forbidden in local mode")
        url = f"https://{self.config['account']}.blob.core.windows.net/boinc-handoff{query}"
        headers = {"Authorization": f"Bearer {self.storage_token()}", "x-ms-version": "2023-11-03"}
        for attempt in range(3):
            try:
                with urllib.request.urlopen(urllib.request.Request(url, headers=headers), timeout=60) as response:
                    return response.read()
            except OSError as error:  # URLError and HTTPError are OSErrors
                if attempt == 2:
                    raise RuntimeError(f"hand-off storage read failed: {type(error).__name__}") from None
                time.sleep(1 + attempt)
        raise AssertionError("unreachable")

    def list_journal_prefix(self, digit):
        """The name and ETag of every journal (and any other blob without the delimiter in its
        name) of the job ids that start with `digit`."""
        entries, marker = [], ""
        while True:
            query = urllib.parse.urlencode({"restype": "container", "comp": "list", "maxresults": 5000,
                                            "prefix": f"dispatch/jobs/{digit}",
                                            "delimiter": JOURNAL_LIST_DELIMITER, "marker": marker})
            root = ET.fromstring(self.storage_get(f"?{query}"))
            # The grouped prefixes (Blobs/BlobPrefix) name no journal and are skipped.
            for blob in root.iterfind("./Blobs/Blob"):
                entries.append({"name": blob.findtext("Name"), "etag": blob.findtext("Properties/Etag")})
            marker = root.findtext("NextMarker") or ""
            if not marker:
                return entries

    def journal_listing(self):
        if not self.rest_journals:
            return json.loads(self.az("list", "--prefix", "dispatch/jobs/", "--num-results", "*", "--query",
                                      "[].{name:name,etag:properties.etag}", "-o", "json"))
        # A job id starts with a hex digit, so one list per digit fetches the prefix in parallel.
        self.storage_token()  # before the threads, so they share one
        start, cpu = self.timings.clock(), self.timings.cpu()
        passed = False
        try:
            with concurrent.futures.ThreadPoolExecutor(JOURNAL_REQUESTS) as pool:
                listed = list(pool.map(self.list_journal_prefix, "0123456789abcdef"))
            passed = True
        finally:
            self.timings.command("storage rest list", self.timings.clock() - start,
                                 self.timings.cpu() - cpu, passed)
        return [entry for entries in listed for entry in entries]

    def download_journals(self, cache, names):
        """Fetch these journals into the cache, each one replaced whole."""
        if not self.rest_journals:
            for name in names:
                path = cache / name
                path.parent.mkdir(parents=True, mode=0o700, exist_ok=True)
                self.az("download", "--name", name, "--file", str(path),
                        "--overwrite", "true", "-o", "none")
            return

        def fetch(name):
            data = self.storage_get("/" + urllib.parse.quote(name))
            path = cache / name
            path.parent.mkdir(parents=True, mode=0o700, exist_ok=True)
            partial = path.with_name(path.name + ".partial")
            partial.write_bytes(data)
            partial.replace(path)

        self.storage_token()
        start, cpu = self.timings.clock(), self.timings.cpu()
        passed = False
        try:
            with concurrent.futures.ThreadPoolExecutor(JOURNAL_REQUESTS) as pool:
                list(pool.map(fetch, names))
            passed = True
        finally:
            self.timings.command("storage rest download", self.timings.clock() - start,
                                 self.timings.cpu() - cpu, passed)

    def journal_inventory(self):
        if self.handoff_root is not None:
            listing = {}
            jobs = self.handoff_root / "dispatch/jobs"
            require(jobs.is_dir() and jobs.resolve() == jobs, "local dispatch inventory is missing or unsafe")
            for entry in jobs.iterdir():
                if not re.fullmatch(r"[0-9a-f]{64}", entry.name):
                    continue
                require(entry.resolve() == entry and stat.S_ISDIR(entry.stat().st_mode),
                        "unsafe local job directory")
                path = entry / "record.json"
                try:
                    info = path.lstat()
                except FileNotFoundError:
                    continue
                require(stat.S_ISREG(info.st_mode), "unsafe local journal")
                name = str(path.relative_to(self.handoff_root))
                path = self._local_object(name)
                require(path.stat().st_size <= 131072, "invalid/oversized journal metadata")
                listing[name] = hashlib.sha256(path.read_bytes()).hexdigest()
            require(len(listing) <= MAX_JOBS, "dispatch inventory limit; reconcile drained journals")
            return listing
        listing = self.journal_listing()
        require(isinstance(listing, list), "invalid dispatch inventory")
        snapshot = {}
        for entry in listing:
            require(isinstance(entry, dict) and isinstance(entry.get("name"), str),
                    "invalid dispatch inventory entry")
            name = entry["name"]
            if not re.fullmatch(r"dispatch/jobs/[0-9a-f]{64}/record.json", name):
                continue
            etag = entry.get("etag")
            require(isinstance(etag, str) and bool(etag.strip()), "missing journal ETag")
            require(name not in snapshot, "duplicate journal inventory entry")
            snapshot[name] = etag
        names = list(snapshot)
        require(len(names) <= MAX_JOBS and len(set(names)) == len(names),
                "dispatch inventory limit/consistency failure; reconcile drained journals")
        return snapshot

    def read_journal_cache(self, cache, names, expected):
        require(len(names) <= MAX_JOBS and all(
            isinstance(name, str) and re.fullmatch(r"dispatch/jobs/[0-9a-f]{64}/record.json", name)
            for name in names), "invalid journal cache inventory")
        paths = list(cache.rglob("*"))
        require(not any(path.is_symlink() for path in paths), "invalid journal cache path")
        actual = {str(path.relative_to(cache)) for path in paths if path.is_file()}
        require(actual == set(names), "journal inventory changed during download; retry under the owner lock")
        records, checksums = [], {}
        for name in sorted(names):
            path = cache / name
            require(path.stat().st_size <= 131072, "invalid/oversized journal metadata")
            data = path.read_bytes()
            checksums[name] = hashlib.sha256(data).hexdigest()
            if name in expected:
                require(checksums[name] == expected[name], "journal cache content digest mismatch")
            record = json.loads(data)
            require(record["job"]["id"] == name.split("/")[2], "journal filename/identity mismatch")
            records.append(record)
        return records, checksums

    @timed("journals")
    def records(self):
        snapshot = self.journal_inventory()
        names = list(snapshot)
        if self.handoff_root is not None:
            records = []
            for name in sorted(names):
                data = self._local_object(name).read_bytes()
                require(hashlib.sha256(data).hexdigest() == snapshot[name],
                        "local journal changed during inventory; retry under owner lock")
                record = json.loads(data)
                require(record["job"]["id"] == name.split("/")[2], "journal filename/identity mismatch")
                records.append(record)
            require(self.journal_inventory() == snapshot, "local journal inventory changed during read")
            return records
        # Re-list every time, but avoid the second full download within a tick
        # when every journal identity and ETag is unchanged.
        if self._journal_snapshot == snapshot:
            return copy.deepcopy(self._journal_records)
        self._journal_snapshot = None
        self._journal_records = None
        cache = self.directory / "journal-cache"
        manifest = self.directory / "journal-snapshot.json"
        base = {"schema_version": 1, "account": self.config["account"], "container": "boinc-handoff"}
        identity = {**base, "entries": snapshot}
        require(not cache.is_symlink() and not manifest.is_symlink(), "invalid journal cache path")
        reusable = False
        delta = None
        if manifest.exists():
            require(manifest.stat().st_size <= MAX_JOBS * 512, "oversized journal cache manifest")
            stored = json.loads(manifest.read_text())
            require(isinstance(stored, dict), "invalid journal cache manifest")
            reusable = {key: value for key, value in stored.items() if key != "files"} == identity
            if reusable:
                require(isinstance(stored.get("files"), dict) and set(stored["files"]) == set(names),
                        "invalid journal cache content inventory")
            elif {key: value for key, value in stored.items() if key not in ("entries", "files")} == base:
                previous = stored.get("entries")
                require(isinstance(previous, dict) and all(
                    isinstance(etag, str) and bool(etag.strip()) for etag in previous.values()),
                    "invalid journal cache inventory")
                require(isinstance(stored.get("files"), dict) and set(stored["files"]) == set(previous),
                        "invalid journal cache content inventory")
                changed = [name for name in names if previous.get(name) != snapshot[name]]
                # Bound CLI startups; broad changes keep the single batch download.
                if len(changed) <= (JOURNAL_DELTA_REST if self.rest_journals else 32):
                    self.read_journal_cache(cache, previous, stored["files"])
                    delta = changed
        if not reusable:
            if manifest.exists():
                manifest.unlink()
                fd = os.open(self.directory, os.O_RDONLY)
                try:
                    os.fsync(fd)
                finally:
                    os.close(fd)
            if delta is not None:
                for name in set(previous) - set(names):
                    path = cache / name
                    path.unlink()
                    path.parent.rmdir()
                self.download_journals(cache, delta)
            else:
                if cache.exists():
                    shutil.rmtree(cache)
                cache.mkdir(mode=0o700)
            if names and delta is None:
                pattern = "dispatch/jobs/" + "[0-9a-f]" * 64 + "/record.json"
                self.run([
                    "az", "storage", "blob", "download-batch", "--source", "boinc-handoff",
                    "--destination", str(cache), "--pattern", pattern, "--overwrite", "true",
                    "--account-name", self.config["account"], "--auth-mode", "login",
                    "--only-show-errors", "-o", "none",
                ], "batch journal inventory")
        expected = stored["files"] if reusable else {}
        if delta is not None:
            expected = {name: stored["files"][name] for name in names if name not in delta}
        records, checksums = self.read_journal_cache(cache, names, expected)
        if not reusable:
            require(self.journal_inventory() == snapshot,
                    "journal ETags changed during download; retry under the owner lock")
            save(manifest, {**identity, "files": checksums})
        self._journal_snapshot = snapshot
        self._journal_records = copy.deepcopy(records)
        return records

    @property
    def push_destination(self):
        """Where a queue-fed page's scan goes: the BOINC host when export runs there (#1116),
        else the hand-off container."""
        return "onprem" if "export" in self.onprem_steps else "container"

    def push_args(self, skip_truncated=False):
        argv = self.config["push_command"] + [
            "--account", self.config["account"], "--scans-root", self.config["scans_root"],
            "--manifests-root", self.config["manifests_root"],
            "--manifest", self.config["manifests_root"].rstrip("/") + "/" + self.config["manifest"],
        ]
        if self.push_destination == "onprem" and self.config.get("source") == "queue":
            # Export runs on the BOINC host, so the scans go there over the step command
            # (onprem-step.sh --target scans), not to the hand-off container (#1116).
            argv += ["--to-onprem", "--onprem-env", self.config["env"]]
            for word in self.config["step_command"]:
                argv += ["--onprem-step-arg", word]
        if skip_truncated:
            argv.append("--skip-truncated")
        return argv

    @timed("export")
    def export(self, pages, job_ids=()):
        """Export `pages`, pushing again first any whose scan is not where this export reads it.
        Returns {page: why its JP2 is cut short at LoC} for the pages that push found cut short
        (checked as the first push is, with --skip-truncated-scans, #1457): they are not exported,
        and the caller settles them like a first push's."""
        queue = self.config.get("source") == "queue"
        truncated_pages = {}
        if queue:
            # A page whose scan went to another destination (the export moved on-prem, or back,
            # after it was pushed) is pushed again where this export reads it.
            dest = self.push_destination
            # A deadline retry's page (#1417) was pushed for its failed job, days or weeks ago:
            # neither the BOINC host's staged scans (4 days) nor the hand-off container's
            # scans/ (30 days) are kept that long for sure, so it goes again, wherever export
            # reads it. Only an operator's recorded retry names `supersedes`.
            stale = sorted({page["scan_base"] for page in pages
                            if page.get("scan_base") and (page.get("pushed_to", "container") != dest
                                                          or page.get("supersedes"))})
            if stale:
                # Checked as a first push is (#1457): a scan that was cut short, or became so
                # after its first push, is not sent again.
                found, missing, truncated = self.push_pages(stale)
                require(not missing, "a claimed page's scan is gone from the scan server; release it")
                truncated_pages = {page["page"]: truncated[page["scan_base"]] for page in pages
                                   if page.get("scan_base") in truncated}
                pages = [page for page in pages if page["page"] not in truncated_pages]
                # The push finds each scan's format again: export asks for the file it sent.
                root = self.config["scans_root"].rstrip("/")
                pages = [dict(page, scan=f"{root}/{found[page['scan_base']]}")
                         if page.get("scan_base") in found else page for page in pages]
        if not queue:
            argv = self.push_args()
            for page in pages:
                argv += ["--scan", page["scan"]]
            self.run(argv, "cached scan push", execution_route="azure/scans")
        if not pages:
            return truncated_pages
        # The exporter takes one base batch at a time; queued pages come from many.
        batches = {}
        for page in pages:
            batches.setdefault(page.get("batch") or self.config["batch"], []).append(page)
        root = len(self.config["scans_root"].rstrip("/")) + 1
        groups = sorted(batches.items())
        if self.batch_export and len(groups) > 1:
            # One execution per EXPORT_BATCHES batches: HANDOFF_BATCHES names each page's batch,
            # in HANDOFF_PAGES' order, and the job exports each batch on its own, going on past
            # one that fails (#1055). The cap keeps an execution well inside the job's timeout.
            groups = [(None, [(batch, page) for batch, group in groups[i:i + EXPORT_BATCHES]
                              for page in group])
                      for i in range(0, len(groups), EXPORT_BATCHES)]
        else:
            groups = [(batch, [(batch, page) for page in group]) for batch, group in groups]
        for batch, group in groups:
            variables = dict(
                HANDOFF_PAGES=",".join(page["page"] for _, page in group),
                HANDOFF_PLAN=self.config["plan"],
                HANDOFF_MANIFEST=self.config["manifest"], ENGINE_IMAGE=self.config["image"],
                HANDOFF_SCANS=",".join(page["scan"][root:] for _, page in group),
                HANDOFF_JOBS=",".join(job_ids))
            if batch is None:
                variables["HANDOFF_BATCHES"] = ",".join(b for b, _ in group)
            else:
                variables["HANDOFF_BATCH"] = batch
            if queue:
                variables["HANDOFF_CLAIMS"] = "1"
            retried = sorted({page["supersedes"] for _, page in group if page.get("supersedes")})
            if retried:
                variables["HANDOFF_DEADLINE_RETRIES"] = ",".join(retried)
            self.step("handoff", "export", **variables)
        return truncated_pages

    @timed("claim")
    def claim(self, count):
        """Take up to `count` usable pages off the work queues: one claim step per STEP_IDS,
        stopping early once a step takes fewer than it asked for (the queues ran dry). A later
        step's failure keeps the claims the earlier ones made (ClaimFailed carries them all)."""
        claims = []
        started = time.monotonic()
        while len(claims) < count:
            want = min(STEP_IDS, count - len(claims))
            self.last_claim_summary = None
            try:
                got = self.claim_step(want)
            except ClaimFailed as error:
                raise ClaimFailed(str(error), claims + error.claims) from error
            except (ValueError, KeyError, TypeError, OSError, RuntimeError) as error:
                if claims:
                    # As main() reports it: no paths or command output from an I/O error.
                    message = str(error) if isinstance(error, (ValueError, RuntimeError)) \
                        else type(error).__name__
                    raise ClaimFailed(message, claims) from error
                raise
            claims += got
            if time.monotonic() - started > CLAIM_SECONDS:
                # Out of time: keep what was taken and start no new step.
                break
            if len(got) < want:
                # The queues ran dry, unless the step stopped at its receive budget with more
                # to read (#1751): then go on while there is time.
                summary = self.last_claim_summary or {}
                if summary.get("budget_spent") is not True:
                    break
        return claims

    def publish_held(self, pages):
        """Put this plan's held-pages list (doc id -> message sha256s) in the hand-off container
        for the claim step, when it differs from the one this state directory last put there:
        in parts of at most HELD_PAGES_PER_PART pages, <prefix><plan>.<NNNN>.json, each well
        inside the reader's 64 MiB, and then the plan's parts from a longer list go. Only this
        plan writes its own parts, under the hand-off container's controller lease."""
        plan = self.config["plan"]
        items = sorted(pages.items())
        parts = {}
        for number, start in enumerate(range(0, len(items), HELD_PAGES_PER_PART)):
            parts[f"{HELD_PAGES_PREFIX}{plan}.{number:04d}.json"] = (json.dumps(
                {"schema_version": 1, "pages": dict(items[start:start + HELD_PAGES_PER_PART])},
                sort_keys=True, separators=(",", ":")) + "\n").encode()
        digest = hashlib.sha256(b"".join(name.encode() + b"\n" + body
                                         for name, body in parts.items())).hexdigest()
        marker = self.directory / "held-pages.sha256"
        try:
            if marker.read_text().strip() == digest:
                return False
        except OSError:
            pass
        # These writes check neither holder nor generation, so a controller that lost the lock
        # could change the list a successor relies on. Until they go through a fenced host step
        # (write-held-pages), the local lock refuses to publish.
        if self.local_lock:
            raise RuntimeError("--local-lock cannot publish held pages yet: the write is not fenced")
        mine = f"{HELD_PAGES_PREFIX}{plan}."
        if self.handoff_root is not None:
            directory = self.handoff_root / HELD_PAGES_PREFIX
            directory.mkdir(parents=True, exist_ok=True)
            for name, body in parts.items():
                staged = directory / ".held-pages.tmp"
                staged.write_bytes(body)
                os.replace(staged, self.handoff_root / name)
            there = [f"{HELD_PAGES_PREFIX}{path.name}" for path in directory.glob(f"{plan}.*.json")]
            for name in there:
                if name not in parts:
                    (self.handoff_root / name).unlink()
        else:
            staged = self.directory / "held-pages.json"
            for name, body in parts.items():
                staged.write_bytes(body)
                self.az("upload", "--name", name, "--file", str(staged), "--overwrite", "true",
                        "--content-type", "application/json", "-o", "none")
            there = json.loads(self.az("list", "--prefix", mine, "--num-results", "*",
                                       "--query", "[].name", "-o", "json") or "[]")
            for name in there:
                if name not in parts and name.startswith(mine) and name.endswith(".json"):
                    self.az("delete", "--name", name, "-o", "none")
        marker.write_text(digest + "\n")
        return True

    def claim_step(self, count):
        """Take up to `count` (at most STEP_IDS) pages: one hand-off claim step."""
        nonce = uuid.uuid4().hex
        failure = None
        languages = self.config.get("claim_languages")
        languages = list(languages) + self.extra_claim_languages if languages else None
        try:
            self.step("handoff", "claim", HANDOFF_MANIFEST=self.config["manifest"], CLAIM_NONCE=nonce,
                      CLAIM_MAX=count, CLAIM_QUEUES=",".join(self.config["claim_queues"]),
                      CLAIM_LEASE_HOURS=self.config["lease_hours"],
                      **({"CLAIM_LANGUAGES": ",".join(languages)} if languages else {}))
        except RuntimeError as error:
            # Messages taken before the failure are hidden now and their claims are written:
            # record them if the job got as far as its summary, then stop the tick.
            failure = error
        try:
            summary = self.blob("scheduler/claims.json")
            require(summary.get("schema_version") == 1 and summary.get("nonce") == nonce,
                    "stale/concurrent claim summary; another controller may be active")
        except (RuntimeError, ValueError) as error:
            if failure:
                raise_transport_errors([failure, error])
            raise
        self.last_claim_summary = summary
        if failure:
            raise ClaimFailed(str(failure), summary["claims"]) from failure
        settled = {key: summary.get(key) for key in ("received", "held", "hidden", "skipped", "budget_spent")
                   if key in summary}
        if settled:
            try:
                print(json.dumps({"event": "boinc_batch_claim_step", "schema_version": 1,
                                  "asked": count, "claimed": len(summary["claims"]), **settled},
                                 sort_keys=True), file=sys.stderr, flush=True)
            except (OSError, ValueError):
                pass
        return summary["claims"]

    def push_pages(self, bases, skip_truncated=None):
        """push_page_step for each STEP_IDS of `bases`, merged: one push run stays well inside
        its timeout however many pages a tick handles."""
        found, missing, truncated = {}, [], {}
        for group in chunks(bases):
            f, m, t = self.push_page_step(group, skip_truncated)
            found.update(f)
            missing += m
            truncated.update(t)
        return found, missing, truncated

    @timed("push")
    def push_page_step(self, bases, skip_truncated=None):
        """Push the cached scans of claimed pages, finding each one's format on the scan server.
        Returns ({base: path under scans/}, [bases with no cached scan], {base: why its JP2 is
        cut short at LoC}); the last is empty unless the JP2s are checked (skip_truncated)."""
        check = self.skip_truncated if skip_truncated is None else skip_truncated
        argv = self.push_args(skip_truncated=check)
        for base in bases:
            # The script names a page by where the fetcher files it under keep/, without "keep/".
            require(base.startswith("keep/"), "claimed scan is not under keep/")
            argv += ["--keep-page", base[len("keep/"):]]
        route = "onprem" if "--to-onprem" in argv else "azure"
        output = self.run(argv, "cached scan push", ok=(0, 3), execution_route=f"{route}/scans")
        pushed, missing, truncated = {}, [], {}
        for line in output.splitlines():
            kind, _, value = line.partition(" ")
            if kind == "pushed":
                pushed[value] = value
            elif kind == "missing":
                missing.append("keep/" + value)
            elif kind == "truncated" and check:
                page, _, reason = value.partition(" ")
                truncated["keep/" + page] = reason
        found = {}
        for base in bases:
            match = [rel for rel in pushed if rel.startswith(f"{base}.")]
            if match:
                found[base] = match[0]
        require(set(found) | set(missing) | set(truncated) == set(bases)
                and len(found) + len(missing) + len(truncated) == len(bases),
                "scan push did not account for every claimed page")
        counts = self.timings.scan_pages.setdefault(route, {"pushed": 0, "missing": 0})
        counts["pushed"] += len(found)
        counts["missing"] += len(missing)
        if truncated:
            counts["source_truncated"] = counts.get("source_truncated", 0) + len(truncated)
        return found, missing, truncated

    @timed("release")
    def release(self, doc_ids):
        for group in chunks(doc_ids):
            self.step("handoff", "release", HANDOFF_DOCS=",".join(group))

    @timed("dead-letter")
    def dead_letter(self, job_ids):
        """Pages whose workunit failed for good go to ocr-deadletter (#1154), each dead letter
        saying why from its job's journal (#1159)."""
        self.step("handoff", "dead-letter", JOB_IDS=",".join(job_ids))

    @timed("dead-letter-truncated")
    def dead_letter_truncated(self, claims):
        """Claimed pages whose scan LoC holds cut short (#1457) go to ocr-deadletter unexported:
        `claims` are (doc id, sha256 of the message claimed), so a newer claim isn't taken."""
        self.step("handoff", "dead-letter",
                  SOURCE_TRUNCATED_DOCS=",".join(f"{doc_id}:{message}" for doc_id, message in claims))

    @timed("submit")
    def submit(self, job_id):
        self.step("bridge", "submit-remote", JOB_ID=job_id,
                  HANDOFF_URL=self.handoff_url)

    @timed("collect")
    def collect(self, job_id, spool):
        self.step("bridge", "spool-out", JOB_ID=job_id, HANDOFF_SPOOL=spool,
                  HANDOFF_URL=self.handoff_url)
        self.step("handoff", "collect", JOB_ID=job_id)

    @timed("submit")
    def submit_batch(self, job_ids, units=None):
        """Submit these jobs: one bridge execution for all of them when batch_submit is on.
        The bridge stops at the first failure, as one execution per job would have. With
        `units` (lists of job ids, from units_of) holding a group of more than one, each group
        is one OCR bundle workunit (#1059): JOB_UNITS, one execution per unit, or one for all of
        them when batch_submit is on."""
        if not job_ids:
            return
        if units and any(len(unit) > 1 for unit in units):
            url = self.handoff_url
            groups = unit_steps(units) if self.batch_submit else [[unit] for unit in units]
            for group in groups:
                self.step("bridge", "submit-remote",
                          JOB_UNITS=",".join("+".join(unit) for unit in group),
                          BUNDLE_APP_VERSION=self.config["bundle_app_version"], HANDOFF_URL=url)
            return
        if not self.batch_submit:
            for job_id in job_ids:
                self.submit(job_id)
            return
        for group in chunks(job_ids):
            self.step("bridge", "submit-remote", JOB_IDS=",".join(group),
                      HANDOFF_URL=self.handoff_url)

    @timed("submit")
    def submit_native(self, units):
        """Submit (job id, host id) pairs as usnm_ocr_native units, each assigned to its host
        (#1780): NATIVE_UNITS, one bridge execution a step. The bridge creates a unit only if the
        trust list admits its host, and stops at the first failure."""
        for group in chunks(units):
            self.step("bridge", "submit-remote",
                      NATIVE_UNITS=",".join(f"{job_id}={host}" for job_id, host in group),
                      HANDOFF_URL=self.handoff_url)

    @timed("collect")
    def collect_batch(self, pairs):
        """Collect these (job, spool) pairs. With both flags off, exactly the per-job flow
        (spool-out then collect, job by job, stopping at the first failure). Batched, a failed
        spool-out doesn't stop collection: every job is still collected (one whose spool-out
        failed then fails in its own collect), and the first failure is raised only after all
        of them were tried, so one held page never keeps the others unpublished."""
        if not pairs:
            return
        if not (self.batch_spool_out or self.batch_collect):
            for job_id, spool in pairs:
                self.collect(job_id, spool)
            return
        url = self.handoff_url
        errors = []
        if self.batch_spool_out:
            for group in chunks(pairs):
                try:
                    self.step("bridge", "spool-out",
                              HANDOFF_SPOOLS=",".join(f"{j}={s}" for j, s in group), HANDOFF_URL=url)
                except RuntimeError as error:
                    errors.append(error)
        else:
            for job_id, spool in pairs:
                try:
                    self.step("bridge", "spool-out", JOB_ID=job_id, HANDOFF_SPOOL=spool, HANDOFF_URL=url)
                except RuntimeError as error:
                    errors.append(error)
        if self.batch_collect:
            parallel = {"COLLECT_PARALLEL": str(self.collect_parallel)} if self.collect_parallel > 1 else {}
            for group in chunks(pairs):
                try:
                    self.step("handoff", "collect", JOB_IDS=",".join(j for j, _ in group), **parallel)
                except RuntimeError as error:
                    errors.append(error)
        else:
            for job_id, _ in pairs:
                try:
                    self.step("handoff", "collect", JOB_ID=job_id)
                except RuntimeError as error:
                    errors.append(error)
        if errors:
            raise_transport_errors(errors)

    @timed("status")
    def server_status(self, url=None, path=None):
        path = self.status_file if path is None else path
        if path is not None:
            try:
                raw = Path(path).read_bytes()
            except OSError as error:
                raise RuntimeError("server status file read failed; inspect the path privately") from error
            return parse_server_status(raw)
        target_url = url or self.status_url or self.config.get("server_status_url")
        if not target_url:
            return None
        _validate_status_url(target_url)
        try:
            with urllib.request.urlopen(target_url, timeout=15) as response:
                raw = response.read(MAX_STATUS_BYTES + 1)
        except OSError as error:
            raise RuntimeError("server status fetch failed; inspect the endpoint privately") from error
        return parse_server_status(raw)


def ocr_workunit_counts(snapshot, pending_jobs):
    """(pending OCR workunits, pages beyond one per workunit). An inventory that predates
    bundles has no pending_ocr_workunits: every pending page is its own workunit."""
    workunits = snapshot.get("pending_ocr_workunits", len(pending_jobs))
    require(type(workunits) is int and 0 <= workunits <= len(pending_jobs) and
            (workunits > 0 or not pending_jobs), "invalid pending OCR workunit count")
    return workunits, len(pending_jobs) - workunits


def served_versions(app_versions):
    """{(platform, plan class): (published, loaded)} for usnm_ocr, from the inventory's
    ocr_app_versions (#1687): the version numbers not deprecated in the database, and those of
    them BOINC's feeder loads. As stock sched_shmem.cpp does, that is every one at or above the
    app's min_version when it is set, and otherwise only the highest in its platform and plan
    class. A workunit pinned with app_version_num gets only a loaded version of that number, so
    work pinned to any other one is never sent; a held (deprecated) version isn't loaded at all."""
    require(isinstance(app_versions, dict) and set(app_versions) == {"min_version", "versions"}
            and type(app_versions["min_version"]) is int and app_versions["min_version"] >= 0
            and isinstance(app_versions["versions"], list), "invalid app version inventory")
    published = {}
    for entry in app_versions["versions"]:
        require(isinstance(entry, dict) and set(entry) == {"platform", "plan_class", "version_num"}
                and isinstance(entry["platform"], str) and isinstance(entry["plan_class"], str)
                and type(entry["version_num"]) is int and entry["version_num"] > 0,
                "invalid app version inventory")
        published.setdefault((entry["platform"], entry["plan_class"]), set()).add(entry["version_num"])
    minimum = app_versions["min_version"]
    return {key: (numbers, {n for n in numbers if n >= minimum} if minimum else {max(numbers)})
            for key, numbers in published.items()}


def pinned_version_problem(snapshot, pin):
    """Why BOINC wouldn't send a unit pinned to usnm_ocr version `pin`, or None when it would
    (#1687): `pin` must be published and loaded wherever it is published. None too when the
    inventory predates the check (a server image without ocr_app_versions); the tick says so."""
    if "ocr_app_versions" not in snapshot:
        return None
    served = served_versions(snapshot["ocr_app_versions"])
    minimum = snapshot["ocr_app_versions"]["min_version"]

    def where(platform, plan):
        return platform + (f" ({plan})" if plan else "")
    if minimum and pin < minimum:
        excluded = sorted(where(*key) for key, (numbers, _) in served.items() if pin in numbers)
        return (f"bundle_app_version {pin} is below usnm_ocr's min_version {minimum}, so the feeder "
                f"loads it nowhere{' (published on ' + ', '.join(excluded) + ')' if excluded else ''}: "
                f"units pinned to it would go unsent. Lower min_version to {pin} or below "
                "(app-version-hold.php --action min-version) or rotate the plan onto a loaded version")
    outranked = sorted(f"{max(loaded)} on {where(platform, plan)}"
                       for (platform, plan), (numbers, loaded) in served.items()
                       if pin in numbers and pin not in loaded)
    if outranked:
        return (f"bundle_app_version {pin} is not the usnm_ocr version the scheduler sends: "
                f"{', '.join(outranked)} outranks it, so units pinned to {pin} would go unsent. "
                "Hold the newer version (boinc/scripts/app-version-hold.php) or rotate the plan onto it")
    if not any(pin in loaded for _, loaded in served.values()):
        return (f"bundle_app_version {pin} is not a usnm_ocr version the scheduler loads: it isn't "
                "published, or it is held (deprecated). Release it or rotate the plan")
    return None


def units_of(config, records):
    """Split the records to submit into workunits (#1059): by queue and page deadline (a bundle's
    pages share one, #1159), in their order, into groups of at most that queue's pages_per_unit
    (1, a page alone, by default)."""
    sizes = config.get("pages_per_unit", {})
    groups = {}
    for record in records:
        queue = QUEUE_OF_REASON.get(record["job"]["message"].get("reason"), "ocr-bulk")
        deadline = record["job"].get("engine", {}).get("timeout_secs")
        groups.setdefault(queue, {}).setdefault(deadline, []).append(record["job"]["id"])
    units = []
    for queue in QUEUES:
        size = sizes.get(queue, 1)
        for ids in groups.get(queue, {}).values():
            units += [ids[i:i + size] for i in range(0, len(ids), size)]
    return units


def deadline_retried(page, roots, deadline_retries):
    """A page's root journals when each after the first is an operator's deadline retry of the
    one before (#1417): the failed job names the newer message's queued_at and deadline in the
    controller's state (QueueFeed.deadline_retry), and exactly one journal has them. The last one
    may have none yet: its retry was taken but its export hasn't journaled the new job, which the
    next tick exports again. Returns the last one alone, or the roots as they are when they don't
    form that one chain."""
    def key(queued_at, timeout_secs):
        return (queued_at, timeout_secs) if isinstance(queued_at, str) and type(timeout_secs) is int else None

    by_key = {}
    for record in roots:
        job = record["job"]
        found = key(job["message"].get("queued_at"), job["engine"].get("timeout_secs"))
        if found is not None:
            by_key.setdefault(found, []).append(record)
    after = {}
    for record in roots:
        entry = deadline_retries.get(record["job"]["id"])
        if not isinstance(entry, dict):
            continue
        matching = by_key.get(key(entry.get("queued_at"), entry.get("timeout_secs")), [])
        followers = [other for other in matching if other is not record]
        if entry.get("page") != page or record["stage"] != "failed" or len(followers) > 1:
            return roots
        if followers:
            after[record["job"]["id"]] = followers[0]
    starts = [record for record in roots
              if not any(record is follower for follower in after.values())]
    if len(starts) != 1:
        return roots
    chain = [starts[0]]
    while chain[-1]["job"]["id"] in after and len(chain) <= len(roots):
        chain.append(after[chain[-1]["job"]["id"]])
    if len(chain) != len(roots) or len({record["job"]["id"] for record in chain}) != len(roots):
        return roots
    return [chain[-1]]


def owned_records(config, records, deadline_retries=None, conflicts=None):
    """Each allocated page's current journal: its one root, or its last reviewed retry
    (#1328). `deadline_retries` (the controller's state of that name, #1417) lets a page's
    dead-lettered root give way to the root an operator's deadline retry exported.

    An allocated page that another plan's journal is for is refused, naming the page and the
    plan. Given a dict as `conflicts` (a queue-fed tick, #1685), such a page is left out of the
    result instead: `conflicts[page]` names the other plans and holds this plan's own current
    journal for it, or None, and says whether it is `settled_elsewhere`: this plan has no
    journal of the page, and every other plan's journal of it is published or kept by a held
    scope. That page's work is done and owned there: a held page's queue message comes back
    after its lease and a live plan claims it again, which is no fault (10 Nov). The plan's own
    journals of that page are checked all the same."""
    # Each allocated page's base batch: one for the whole cohort, or the claimed page's own.
    wanted = {page["page"]: page.get("batch") or config["batch"] for page in config["pages"]}
    by_page = {}
    foreign = {}
    foreign_done = {}
    for record in records:
        require(record["stage"] in STAGES and ID.fullmatch(record["job"]["id"]),
                "invalid dispatch journal")
        job = record["job"]
        page = job["message"]["page"]
        if record["plan"] != config["plan"]:
            if page in wanted:
                require(conflicts is not None,
                        f"page {page} is already allocated to another plan ({str(record['plan'])[:64]}); "
                        "reconcile the allocation before export (archive_history.py reconcile, #1685)")
                foreign.setdefault(page, set()).add(str(record["plan"])[:64])
                done = record["stage"] == "published" or kept_by_policy(record)
                foreign_done[page] = foreign_done.get(page, True) and done
            continue
        require(wanted.get(page) == job["message"]["batch"],
                "plan already exists with a different allocation/image; operator migration required")
        by_page.setdefault(page, []).append(record)
    owned = {}
    for page, history in by_page.items():
        roots = [record for record in history if not record.get("retry")]
        chain = roots
        if len(roots) > 1 and deadline_retries:
            roots = deadline_retried(page, roots, deadline_retries)
        require(len(roots) == 1, "multiple jobs for one allocated page; reconcile before feeding")
        # A deadline retry's dead-lettered roots are the plan's too.
        for record in chain:
            require(record["job"]["engine"]["image"] == config["image"],
                    "plan already exists with a different allocation/image; operator migration required")
        current = roots[0]
        remaining = [record for record in history if record.get("retry")]
        while remaining:
            children = [record for record in remaining
                        if record["retry"].get("previous_job") == current["job"]["id"]]
            require(len(children) == 1 and current["stage"] == "failed",
                    "invalid or branching reviewed retry lineage")
            child = children[0]
            job, parent = child["job"], current["job"]
            require(job.get("v") == 2 and job.get("retry_of") == parent["id"]
                    and type(job.get("attempt")) is int
                    and job["attempt"] == parent.get("attempt", 0) + 1
                    and child["retry"].get("previous_attempt") == parent.get("attempt", 0)
                    and type(child["retry"].get("app_version_num")) is int
                    and child["retry"]["app_version_num"] > 0
                    and job["message"].get("language") == parent["message"].get("language")
                    and job["message"].get("reason") == parent["message"].get("reason")
                    and job["engine"]["image"].startswith("sha256:")
                    and ID.fullmatch(job["engine"]["image"][7:]),
                    "invalid reviewed retry lineage")
            remaining.remove(child)
            current = child
        owned[page] = current
    for page, plans in foreign.items():
        own = owned.pop(page, None)
        conflicts[page] = {"plans": sorted(plans), "own": own,
                           "settled_elsewhere": own is None and foreign_done[page]}
    return owned


BATCH = re.compile(r"[a-z]{2,10}_[A-Za-z0-9_]+")


class QueueFeed:
    """Pages claimed off the work queues, kept in the controller's state.

    The hand-off job's `claim` step takes usable messages off `ocr-bulk` (and `ocr-priority`
    when asked), as a second consumer beside the on-prem nodes. Each claimed page is recorded
    here *before* anything else is done for it, because its message is hidden from the nodes
    for the lease and only a later `collect` deletes it. A page whose scan turns out not to be
    on the scan server goes back to the queue. One whose JP2 LoC holds cut short (checked with
    --skip-truncated-scans, #1457) is dead-lettered and never exported, and kept in the state's
    `source_truncated`, a list for LoC. The record also stays after the page is
    published: its journal belongs to this plan, and `owned_records` refuses a journal for a
    page the plan doesn't know."""

    def __init__(self, config, state, transport, persist, sibling_archived=None):
        self.config = config
        self.transport = transport
        self.persist = persist
        self.state = state
        self.claims = state.setdefault("claims", {})
        require(len(self.claims) <= MAX_JOBS, "too many claimed pages; archive drained history first")
        # Held pages archive_history.py archived (#835): page -> {"job", "message", ...}.
        self.archived_held = state.get("archived_held", {})
        require(isinstance(self.archived_held, dict) and len(self.archived_held) <= MAX_ARCHIVED_HELD,
                "invalid or oversized archived_held state")
        # Other plans' archived held pages (--check-state-dir): a plan with the same worker
        # exports the same job id, which BOINC already has, so their messages are kept
        # hidden here too rather than claimed as new work.
        self.sibling_archived = sibling_archived or {}
        # Messages whose job, in any plan, is settled for good now: published, or kept by a held
        # scope (doc id -> message sha256s). The tick sets them from the journals before it fills.
        self.settled_messages = {}

    def restore(self):
        self.config["pages"] = [
            {"page": page, "batch": claim["batch"], "scan": claim["scan"],
             "scan_base": claim["scan_base"], "pushed_to": claim.get("pushed_to", "container"),
             **({"supersedes": claim["supersedes"]} if claim.get("supersedes") else {}),
             **({"cut_short": True} if "export_truncated" in claim else {})}
            for page, claim in sorted(self.claims.items()) if claim.get("scan")]

    @staticmethod
    def checked(claim):
        require(isinstance(claim, dict) and {"page", "doc_id", "batch", "scan"} <= set(claim),
                "invalid claim")
        page, batch, scan = claim["page"], claim["batch"], claim["scan"]
        require(isinstance(page, str) and PAGE_KEY.fullmatch(page) and isinstance(batch, str)
                and BATCH.fullmatch(batch) and not re.search(r"_ver[0-9]+$", batch),
                "invalid claimed page or batch")
        lccn, date, edition, seq = page.split("/")
        require(claim["doc_id"] == page.replace("/", "_")
                and scan == f"keep/{batch}/{lccn}/{date}/{edition[3:]}/{seq[4:]}",
                "claim names a different page than its scan")
        return page, {"page": page, "doc_id": claim["doc_id"], "batch": batch,
                      "scan_base": scan, "scan": None, "message": claim.get("message_sha256")}

    def held_pages(self):
        """Doc id -> the message sha256s of this plan's and the --check-state-dir plans' archived
        held pages: those messages, back after their lease, are settled work, not new work."""
        pages = {}
        entries = [(page, entry) for page, entry in self.archived_held.items()]
        entries += [(page, entry) for page, listed in self.sibling_archived.items() for entry in listed]
        for page, entry in entries:
            message = entry.get("message") if isinstance(entry, dict) else None
            if isinstance(page, str) and PAGE_KEY.fullmatch(page) and isinstance(message, str) \
                    and ID.fullmatch(message):
                pages.setdefault(page.replace("/", "_"), set()).add(message)
        for doc, messages in self.settled_messages.items():
            pages.setdefault(doc, set()).update(messages)
        return {doc: sorted(messages) for doc, messages in sorted(pages.items())}

    def publish_settled(self):
        """Put the held-pages list out when it changed, whether or not this tick claims (#1754).
        `fill` publishes before a claim, but a tick that doesn't claim (the queue is full, a
        result is held at collection, the plan drains) settles pages too, and the seeder reads
        the same list to stop counting and retrying them. A failure here only leaves the list
        as it was: the tick goes on, and the next one tries again."""
        publish = getattr(self.transport, "publish_held", None)
        if publish is None:
            return
        try:
            publish(self.held_pages())
        except (RuntimeError, OSError) as error:
            print(json.dumps({"event": "boinc_batch_held_pages_unpublished", "schema_version": 1,
                              "error": str(error)[:500]}), file=sys.stderr, flush=True)

    def fill(self, count, selected, owned=None, now=None):
        """Make `count` more pages ready: claim them (less any already claimed but not yet
        started), push their scans, give back the ones with none and dead-letter the ones LoC
        holds cut short. `owned` is the tick's owned_records, for a newer claim of a
        dead-lettered page (deadline_retry)."""
        now = time.time() if now is None else now
        self.retry_releases()
        started = set(selected)
        waiting = [claim for page, claim in self.claims.items() if page not in started]
        need = max(0, count - len(waiting))
        if need:
            publish = getattr(self.transport, "publish_held", None)
            if publish is not None:
                publish(self.held_pages())
            try:
                fresh = self.transport.claim(need)
            except ClaimFailed as error:
                # Their messages are hidden now: keep the claims that were made, then stop.
                self.record(error.claims, need, owned)
                raise
            self.record(fresh, need, owned)
        unpushed = {page: claim for page, claim in self.claims.items()
                    if claim.get("scan") is None and not claim.get("release")
                    and "source_truncated" not in claim}
        if unpushed:
            found, missing, truncated = self.transport.push_pages(
                sorted(claim["scan_base"] for claim in unpushed.values()))
            root = self.config["scans_root"].rstrip("/")
            for page, claim in unpushed.items():
                if claim["scan_base"] in found:
                    claim["scan"] = f"{root}/{found[claim['scan_base']]}"
                    claim["pushed_to"] = getattr(self.transport, "push_destination", "container")
                elif claim["scan_base"] in truncated:
                    claim["source_truncated"] = truncated[claim["scan_base"]]
                else:
                    claim["release"] = True
            self.persist()
        self.retry_releases()
        self.retry_truncated(now)
        self.restore()

    def record(self, fresh, need, owned=None):
        require(isinstance(fresh, list) and len(fresh) <= need, "claimed more pages than asked")
        conflict = False
        errors = []
        for item in fresh:
            page, claim = self.checked(item)
            archived = self.archived_held.get(page)
            if archived is None and page in self.sibling_archived:
                # Any other plan's archive of the page with this message stands for it; one
                # with another message is two attempts at once, as below.
                others = self.sibling_archived[page]
                archived = next((entry for entry in others if entry.get("message") == claim["message"]),
                                others[0])
            if page not in self.claims and archived is not None:
                # Its held result is archived (#835): the same message, hidden again for a fresh
                # lease, stands for the archived job, as for a claim still here. A different
                # message is two attempts at once, as below.
                conflict = conflict or claim["message"] is None or claim["message"] != archived.get("message")
                continue
            if page in self.claims:
                # Its lease ran out while the job was pending and the same message came again,
                # now hidden for a fresh lease: the existing claim, its scan and its job stand.
                # A different message for the page is two attempts at once: stop, once the
                # others are kept. Their messages are hidden for the lease. Unless an operator
                # retries the dead-lettered page with another deadline (#1417).
                if claim["message"] is not None and claim["message"] != self.claims[page].get("message"):
                    try:
                        if self.deadline_retry(page, claim, owned):
                            continue
                    except (RuntimeError, ValueError, OSError) as error:
                        # Its record couldn't be read or isn't valid: stop too, once the others
                        # are kept.
                        errors.append(error)
                        continue
                conflict = conflict or claim["message"] is None or claim["message"] != self.claims[page].get("message")
                continue
            self.claims[page] = claim
        self.persist()
        if errors:
            raise_transport_errors(errors)
        require(not conflict, "claimed a page already in this plan with a different message")

    def deadline_retry(self, page, claim, owned):
        """A newer message for a page whose job failed for good and was dead-lettered, which an
        operator retries with another page deadline (`usnm-boinc claim deadline-retry`, #1417):
        the hand-off container holds its record, for this job and message. The claim takes the
        old one's place, naming the job it supersedes, and the next export builds its job with
        the recorded deadline. Without a record (or a transport that can't read one) it is the
        conflict it always was."""
        lookup = getattr(self.transport, "deadline_retry", None)
        record = (owned or {}).get(page)
        if lookup is None or record is None or record["stage"] != "failed" or record.get("retry") \
                or record["job"]["id"] not in self.state.get("dead_lettered", {}):
            return False
        job = record["job"]
        found = lookup(job["id"], claim["message"])
        if found is None:
            return False
        timeout = found.get("timeout_secs") if isinstance(found, dict) else None
        require(isinstance(found, dict) and found.get("v") == 1 and found.get("failed_job") == job["id"]
                and found.get("doc_id") == claim["doc_id"] and found.get("message_sha256") == claim["message"]
                and type(timeout) is int and 1 <= timeout <= 3600
                and timeout != job["engine"].get("timeout_secs")
                and isinstance(found.get("queued_at"), str)
                and found["queued_at"] != job["message"].get("queued_at"),
                f"invalid deadline retry record for job {job['id']} (page {page}, doc {claim['doc_id']}, "
                f"message {claim['message']})")
        # The retry's scan is pushed and checked again: a cut-short mark of the old attempt goes.
        self.claims[page] = {**{key: value for key, value in self.claims[page].items()
                                if key != "export_truncated"},
                             "message": claim["message"], "supersedes": job["id"]}
        self.state.setdefault("deadline_retries", {})[job["id"]] = {
            "page": page, "message": claim["message"], "queued_at": found["queued_at"],
            "timeout_secs": timeout}
        try:
            print(json.dumps({
                "event": "boinc_batch_deadline_retry", "schema_version": 1, "page": page,
                "failed_job": job["id"], "message_sha256": claim["message"],
                "timeout_secs": timeout}, sort_keys=True), file=sys.stderr, flush=True)
        except (OSError, ValueError):
            pass
        return True

    def retry_releases(self):
        pages = sorted(page for page, claim in self.claims.items() if claim.get("release"))
        if pages:
            self.transport.release([self.claims[page]["doc_id"] for page in pages])
            for page in pages:
                del self.claims[page]
            self.persist()

    def settle_export_truncated(self, truncated, selected, now, journaled=None):
        """Pages whose scan export found cut short when it pushed them again (#1457), `truncated`
        being {page: reason}: dead-lettered and listed as a first push's are, and dropped from
        `selected` so they aren't waited for. A page this plan already has a journal for (a
        deadline retry's failed job, or a prepared one) is not settled: its allocation has to stay
        for the journal's ownership check. It is left claimed, not exported, and logged."""
        journaled = journaled or {}
        kept = sorted(page for page in truncated if page in journaled)
        # A failed journal (a deadline retry's) is settled work and counts for nothing. A prepared
        # one would count as backlog for good: stop for an operator instead of holding it.
        live = [page for page in kept if journaled[page]["stage"] != "failed"]
        require(not live, f"export found {', '.join(live)} cut short, but the plan holds a live journal "
                "for it; reconcile the journal before the next tick (#1457)")
        for page in kept:
            # Kept in the state, so later ticks pass it over and it doesn't take export slots.
            self.claims[page]["export_truncated"] = truncated[page]
        if kept:
            self.persist()
            self.restore()
            try:
                print(json.dumps({"event": "boinc_batch_export_truncated_kept", "schema_version": 1,
                                  "pages": kept, "message": "cut short at LoC, but the plan holds a journal "
                                  "for them: not exported, left claimed until the lease runs out"},
                                 sort_keys=True), file=sys.stderr, flush=True)
            except (OSError, ValueError):
                pass
        truncated = {page: reason for page, reason in truncated.items() if page not in journaled}
        if not truncated:
            return
        for page, reason in truncated.items():
            claim = self.claims.get(page)
            require(claim is not None, f"export found {page} cut short, and it has no claim")
            claim["source_truncated"] = reason
            claim["scan"] = None
            selected.pop(page, None)
        self.persist()
        self.retry_truncated(now)
        self.restore()

    def retry_truncated(self, now):
        """Dead-letter the claimed pages whose JP2 LoC holds cut short (#1457): released, the
        page would be claimed again, and LoC's copy is no better to fetch again. Each claim is
        kept until its dead letter went, then listed in the state's `source_truncated`. Every
        tick tries, draining or not, so a failed step doesn't leave the lease to run out."""
        pages = sorted(page for page, claim in self.claims.items() if "source_truncated" in claim)
        if not pages:
            return
        require(all(isinstance(self.claims[page]["message"], str) and ID.fullmatch(self.claims[page]["message"])
                    for page in pages), "a source-truncated claim has no message sha256 to settle it by")
        for group in chunks(pages):  # the step takes STEP_IDS doc ids at most
            self.transport.dead_letter_truncated(
                [(self.claims[page]["doc_id"], self.claims[page]["message"]) for page in group])
        listed = self.state.setdefault("source_truncated", {})
        for page in pages:
            claim = self.claims.pop(page)
            listed[page] = {"doc_id": claim["doc_id"], "batch": claim["batch"], "scan": claim["scan_base"],
                            "reason": claim["source_truncated"], "at": int(now)}
        self.persist()


def _select_cohort_entry(mapping, keys, *, allow_single_default=False):
    for key in keys:
        if key in mapping:
            return key, mapping[key]
    if allow_single_default and len(mapping) == 1:
        return next(iter(mapping.items()))
    return None, {}


def resolve_watermarks(config, snapshot, status=None, *,
                       low_watermark=None, high_watermark=None, max_global_pending=None,
                       other_app_budget=None, native_slots=None):
    cohorts = config.get("cohorts") or {}
    app = normalize_cohort_apps(config.get("app", OCR_APP_NAME))
    platform = normalize_cohort_platform(config.get("platform", "all"))
    resource = normalize_cohort_resource(config.get("resource", "cpu"))
    explicit_cohort = config.get("cohort")
    cohort_name = explicit_cohort or f"{app}:{resource}"
    matched_key, cohort_cfg = _select_cohort_entry(
        cohorts,
        (cohort_name,) if explicit_cohort is not None else (cohort_name, app, resource),
        allow_single_default=(explicit_cohort is None),
    )
    _validate_cohort_spec(cohort_cfg)
    app = normalize_cohort_apps(cohort_cfg.get("app", app))
    platform = normalize_cohort_platform(cohort_cfg.get("platform", platform))
    resource = normalize_cohort_resource(cohort_cfg.get("resource", resource))
    cohort_name = explicit_cohort or matched_key or f"{app}:{resource}"
    pages_per_host = cohort_cfg.get("pages_per_host", config["pages_per_host"])
    status_cohorts = (status or {}).get("cohorts") or {}
    _, status_cohort = _select_cohort_entry(
        status_cohorts,
        (cohort_name,) if explicit_cohort is not None else (cohort_name, app, resource),
    )
    require(isinstance(status_cohort, dict), "invalid status cohort entry")
    active_hosts = snapshot.get("cohort_active_hosts",
                                status_cohort.get("active_hosts", snapshot["active_hosts"]))
    if native_slots is not None:
        # A native plan (#1780): only its own hosts with a unit's room count.
        active_hosts = sum(slots > 0 for slots in native_slots.values())
    _nonneg_int(active_hosts, MAX_GLOBAL_PENDING, "invalid cohort active_hosts")

    explicit_high = (high_watermark if high_watermark is not None
                     else cohort_cfg.get("high_watermark", config.get("high_watermark")))
    explicit_low = (low_watermark if low_watermark is not None
                    else cohort_cfg.get("low_watermark", config.get("low_watermark")))
    explicit_global = (max_global_pending if max_global_pending is not None
                       else config.get("max_global_pending"))
    # A native plan always admits by its own work (#1780): every app's would stop it for good.
    # It refills as soon as a host has room (low watermark one under the high), to keep CPUs busy.
    cohort_mode = (status is not None or explicit_high is not None or explicit_low is not None or
                   explicit_global is not None or other_app_budget is not None or
                   "cohorts" in config or "cohort" in config or native_slots is not None)

    if explicit_high is not None:
        require(type(explicit_high) is int and 1 <= explicit_high <= MAX_JOBS,
                "invalid high_watermark")
    if explicit_low is not None:
        require(type(explicit_low) is int and 0 <= explicit_low <= MAX_JOBS,
                "invalid low_watermark")
        if explicit_high is not None:
            require(explicit_low <= explicit_high, "low_watermark cannot exceed high_watermark")
        else:
            require(explicit_low <= config["max_pending"], "low_watermark cannot exceed max_pending")
    if explicit_global is not None:
        require(type(explicit_global) is int and 1 <= explicit_global <= MAX_GLOBAL_PENDING,
                "invalid max_global_pending")

    if active_hosts == 0:
        high_target = 0
        low_target = 0
    else:
        if native_slots is not None:
            # CPU fill: each host's units, at most its CPUs and pages_per_host, within max_pending
            # (and a high watermark, when one is set).
            high_target = min(config["max_pending"], sum(native_slots.values()),
                              MAX_JOBS if explicit_high is None else explicit_high)
        elif explicit_high is not None:
            high_target = explicit_high
        else:
            high_target = min(config["max_pending"], active_hosts * pages_per_host)
        if explicit_low is not None:
            low_target = min(explicit_low, high_target)
        elif cohort_mode and native_slots is None:
            low_target = max(0, high_target // 2)
        else:
            low_target = max(0, high_target - 1)

    if explicit_global is not None:
        global_cap = explicit_global
    elif cohort_mode:
        global_cap = max(DEFAULT_MAX_GLOBAL_PENDING, high_target, config["max_pending"])
    else:
        global_cap = high_target
    if other_app_budget is not None:
        # Against a cap set explicitly, as main checks it in a dry run. A budget as large as
        # the cap would leave the cohort no share of it.
        require(explicit_global is not None and type(other_app_budget) is int and
                0 <= other_app_budget < min(global_cap, FEEDER_SHMEM_SLOTS),
                "invalid other_app_budget")

    return {
        "app": app,
        "platform": platform,
        "resource": resource,
        "cohort": cohort_name,
        "active_hosts": active_hosts,
        "low_watermark": low_target,
        "high_watermark": high_target,
        "max_global_pending": global_cap,
        "other_app_budget": other_app_budget,
        "global_hard_cap": None if other_app_budget is None else FEEDER_SHMEM_SLOTS,
        "cohort_mode": cohort_mode,
    }


def other_app_workunits(snapshot, app):
    """Other apps' unassimilated workunits, by the server's per-app count, at most all of
    `snapshot`'s. None from a server image without the count: then no work is other apps'
    for the budget, and all of it counts in full."""
    counts = snapshot.get("pending_workunits_by_app")
    if counts is None:
        return None
    require(isinstance(counts, dict) and all(isinstance(name, str) and type(n) is int and n >= 0
                                             for name, n in counts.items()),
            "invalid per-app inventory counts")
    return min(snapshot["pending_workunits"], sum(n for name, n in counts.items() if name != app))


def counted_pending(info, pending, other):
    """The part of `pending` (unassimilated workunits of every app plus retained backlog, `other`
    of them other apps' by the server's per-app count) that max_global_pending counts. With
    other_app_budget, other apps' work counts only up to the budget, so a backlog no host can
    take can't fill the cap and starve the cohort (#1474); without it, all of it counts."""
    budget = info["other_app_budget"]
    return pending if budget is None else pending - max(0, other - budget)


def global_room(info, pending, other):
    """Room under the global caps, negative past one: max_global_pending for the counted part
    and, with a budget, the feeder's slots for all of it, every app's work counted in full."""
    room = info["max_global_pending"] - counted_pending(info, pending, other)
    if info["global_hard_cap"] is not None:
        room = min(room, info["global_hard_cap"] - pending)
    return room


def global_cap_reason(info, pending, other):
    """Which global cap `pending` has reached, if any. The hard one first: raising
    max_global_pending doesn't lift it."""
    if info["global_hard_cap"] is not None and pending >= info["global_hard_cap"]:
        return "global_hard_cap"
    if counted_pending(info, pending, other) >= info["max_global_pending"]:
        return "global_safety_cap"
    return None


def _extract_status(snapshot, transport, server_status=None, *, drain=False):
    try:
        raw = server_status() if callable(server_status) else server_status
        if raw is None:
            for key in ("server_status", "server_status_xml"):
                if snapshot.get(key) is not None:
                    raw = snapshot[key]
                    break
            if raw is None and ("apps" in snapshot or "tasks_by_app" in snapshot):
                raw = snapshot
        if raw is None:
            xml_attr = getattr(transport, "server_status_xml", None)
            if xml_attr is not None:
                raw = xml_attr() if callable(xml_attr) else xml_attr
        if raw is None:
            status_attr = getattr(transport, "server_status", None)
            if status_attr is not None:
                raw = status_attr() if callable(status_attr) else status_attr
        return parse_server_status(raw) if raw is not None else None
    except (ValueError, KeyError, TypeError, OSError, RuntimeError):
        if drain:
            return None
        raise


def surya_engine(config):
    """The engine pins a Surya plan stages its units with."""
    surya = config["surya"]
    return {"SURYA_ENGINE_IMAGE": surya["engine_image"], "SURYA_ENGINE_LLAMA": surya["engine_llama"],
            "SURYA_ENGINE_WEIGHTS": surya["engine_weights"]}


def surya_steps(config, room):
    """The bridge steps of one Surya tick that finds room for `room` more results: cut the crops
    of the quality campaign's staged pages, then submit units for up to `room` results (#968).
    `unsent` counts results and `surya submit` makes SURYA_REPLICAS of them per unit."""
    surya = {**SURYA_DEFAULTS, **config["surya"]}
    return [
        ("surya-stage-crops", {
            "SURYA_CAMPAIGN": surya["campaign"], "QUALITY_CAMPAIGN": surya["quality_campaign"],
            **surya_engine(config), "SURYA_MAX": surya["stage_pages"],
            "SURYA_TIMEOUT_SECS": surya["timeout_secs"]}),
        ("surya-submit", {
            "SURYA_CAMPAIGN": surya["campaign"],
            "SURYA_MAX": min(surya["submit_units"], room // SURYA_REPLICAS)}),
    ]


def surya_tick(config, state, transport, persist, now=None, *, drain=False,
               low_watermark=None, high_watermark=None, server_status=None):
    """Top up the usnm_surya queue: at or below the low watermark, stage crops and submit up to
    the high one; above it, do nothing. No inventory, journal or hand-off: the units are the
    bridge's own, and a second tick finds the queue the first one filled."""
    now = time.time() if now is None else now
    low = config["low_watermark"] if low_watermark is None else low_watermark
    high = config["high_watermark"] if high_watermark is None else high_watermark
    _validate_cohort_spec({"low_watermark": low, "high_watermark": high})
    unsent = None
    ran = []
    reason = "draining"
    campaign = config["surya"]["campaign"]
    engine = surya_engine(config)
    record = state.get("surya") or {}
    if record.get("campaign") != campaign:
        # A new campaign: nothing staged under it carries other pins.
        record = {}
    # Staged units keep the pins they were cut with (surya/crops.rs skips an existing job
    # file), so a pin change would leave a backlog the new engine's workers reject. The pins
    # are immutable within a campaign; a new engine takes a new campaign name.
    require(record.get("engine") in (None, engine),
            f"engine pins changed for campaign {campaign}; stage a new engine under a new campaign")
    record.update(campaign=campaign, engine=engine)
    state["surya"] = record
    # A failed staging step may already have written units with these pins.
    persist()
    if not drain:
        status = _extract_status({}, transport, server_status=server_status)
        require(status is not None and SURYA_APP_NAME in status["apps"],
                f"server status has no {SURYA_APP_NAME} counts; refusing to guess the queue")
        unsent = status["apps"][SURYA_APP_NAME]["unsent"]
        room = high - unsent
        if unsent > low:
            reason = "above_low_watermark"
        elif room < SURYA_REPLICAS:
            reason = "no_room"
        else:
            reason = None
            for step, variables in surya_steps(config, room):
                transport.step("bridge", step, **variables)
                ran.append({"step": step, "vars": variables})
                # What ran is on record even if the next step fails.
                record["last_run"] = {"at": int(now), "unsent": unsent, "steps": ran}
                persist()
        record.update(checked_at=int(now), unsent=unsent)
    persist()
    return with_timings({
        "event": "boinc_surya_tick", "schema_version": 1, "at": int(now), "plan": config["plan"],
        "app": SURYA_APP_NAME, "campaign": config["surya"]["campaign"], "draining": drain,
        "unsent": unsent, "low_watermark": low, "high_watermark": high,
        "ran": [item["step"] for item in ran], "steps": ran, "skipped": reason,
    }, transport)


def with_timings(event, transport):
    """The tick's line, with where its time went when the transport keeps Timings (#902)."""
    timings = getattr(transport, "timings", None)
    if isinstance(timings, Timings):
        event["timings"] = timings.summary()
    return event


def gold_scope(entry):
    return f"{entry['campaign']}/{entry['language']}/{entry['profile']}"


def gold_onboarding(config, state, transport, persist, now, *, held, room):
    """Run each due gold_onboarding scope's gold-onboard and gold-promote bridge steps (#901).

    A scope is due when its interval has passed since its last run, or GOLD_HELD_INTERVAL_SECONDS
    (an hour) while results are held at collection, if that is shorter. The steps are timed as
    the tick's `gold` phase. The run is recorded before its steps start, so a
    crash or a failing step waits for the next due time instead of retrying every tick. A failed
    step is reported, never hidden, and doesn't stop the tick: collection then holds results
    from uncalibrated hosts exactly as before.

    Gold units count against the global safety cap: `room` is what the cap leaves, and each
    gold-onboard pass may issue at most what is left of it. With none left gold-onboard is
    skipped and gold-promote still runs. Returns the runs and the units they may have issued,
    which the tick then counts as in flight before it refills OCR."""
    record = state.setdefault("gold_onboarding", {})
    runs = []
    reserved = 0
    for entry in config.get("gold_onboarding", ()):
        scope = gold_scope(entry)
        interval = entry.get("interval_minutes", GOLD_DEFAULT_INTERVAL_MINUTES) * 60
        if held:
            interval = min(interval, GOLD_HELD_INTERVAL_SECONDS)
        last = record.get(scope, {}).get("at")
        if isinstance(last, (int, float)) and 0 <= now - last < interval:
            continue
        variables = {
            "GOLD_CAMPAIGN": entry["campaign"], "GOLD_LANGUAGE": entry["language"],
            "GOLD_PROFILE": entry["profile"], "GOLD_ENGINE_IMAGE": entry.get("image", config["image"]),
            "GOLD_ENGINE_BASE": entry["engine_base"],
            "GOLD_ENGINE_VERSION": entry.get("engine_version", "5.5.0"),
        }
        issue = min(entry.get("max_hosts", 10), max(0, room - reserved))
        run = {"at": int(now), "held": held, "max_hosts": issue, "steps": {}}
        record[scope] = run
        persist()
        for step in GOLD_STEPS:
            if step == "gold-onboard":
                if issue < 1:
                    run["steps"][step] = GOLD_SKIPPED_AT_CAP
                    persist()
                    continue
                # Counted even if the step fails: it may have issued some units first.
                reserved += issue
            try:
                # Promotion issues nothing; it always takes the configured positive limit.
                limit = issue if step == "gold-onboard" else entry.get("max_hosts", 10)
                timings = getattr(transport, "timings", None)
                with timings.phase("gold") if timings is not None else contextlib.nullcontext():
                    transport.step("bridge", step, **variables, GOLD_MAX_HOSTS=limit)
                run["steps"][step] = "ok"
            except RuntimeError as error:
                run["steps"][step] = str(error)
            persist()
        runs.append({"scope": scope, **run})
    return runs, reserved


def sibling_archived_held(own_dir, dirs):
    """The archived_held entries of other plans' state directories (--check-state-dir): page ->
    every entry ({"job", "message", ...}) any of them holds for it, so no plan's archive is
    dropped when two archived a page under different jobs. Each must hold a readable state.json."""
    own = Path(own_dir).resolve()
    merged = {}
    for directory in dirs:
        directory = Path(directory).resolve()
        require(directory != own, "--check-state-dir names this plan's own state directory")
        entries = json.loads((directory / "state.json").read_text()).get("archived_held", {})
        require(isinstance(entries, dict) and len(entries) <= MAX_ARCHIVED_HELD and
                all(isinstance(entry, dict) and isinstance(entry.get("job"), str) and ID.fullmatch(entry["job"])
                    and isinstance(entry.get("message"), str) and ID.fullmatch(entry["message"])
                    for entry in entries.values()),
                f"{directory}/state.json has no valid archived_held")
        for page, entry in entries.items():
            merged.setdefault(page, []).append(entry)
    return merged


def sibling_claim_messages(own_dir, dirs):
    """The message sha256s of other plans' claims (--check-state-dir): page -> every sha any of
    them recorded when `claim pull` took the page's message, as the queue held it. A claim
    that carries none is left out."""
    own = Path(own_dir).resolve()
    merged = {}
    for directory in dirs:
        directory = Path(directory).resolve()
        require(directory != own, "--check-state-dir names this plan's own state directory")
        claims = json.loads((directory / "state.json").read_text()).get("claims", {})
        require(isinstance(claims, dict) and len(claims) <= MAX_JOBS,
                f"{directory}/state.json has no valid claims")
        for page, claim in claims.items():
            message = claim.get("message") if isinstance(claim, dict) else None
            if isinstance(message, str) and ID.fullmatch(message):
                merged.setdefault(page, set()).add(message)
    return merged


def queue_message_sha256(message):
    """The sha256 of a journal's queue message as the queue holds it: base64 of its compact JSON,
    fields in the journal's order, as usnm-boinc's OcrMessage::encode writes it and `claim pull`
    hashes it. A message that doesn't come out the same (a float written differently) just
    matches nothing."""
    text = base64.b64encode(json.dumps(message, separators=(",", ":"), ensure_ascii=False).encode())
    return hashlib.sha256(text).hexdigest()


def claimed_messages(claims, sibling_claims=None):
    """Page -> the message sha256s `claim pull` recorded for it, in this plan's claims and in the
    other plans' (--check-state-dir): what settled_messages checks a journal's message against."""
    found = {page: set(shas) for page, shas in (sibling_claims or {}).items()}
    for page, claim in (claims or {}).items():
        message = claim.get("message") if isinstance(claim, dict) else None
        if isinstance(message, str) and ID.fullmatch(message):
            found.setdefault(page, set()).add(message)
    return found


def settled_messages(records, claimed=None):
    """Doc id -> the queue message sha256s of journals, of any plan, settled for good with a
    result: published, or kept by a held scope (#835). Such a message, back after its lease, is
    the same message that job ran: the claim step deletes it (held-pages list) rather than claim
    it again and pass it over as settled elsewhere. A failed, pending or in-flight job's message
    is never listed.

    The journal's message is not always the queue's byte for byte: the export parses its score
    as a float and can land one ulp off the number the seeder wrote (13 % of 556 held srp pages,
    11 Nov 2026), so the sha256 written out again matches nothing. `claimed` (claimed_messages)
    holds the shas `claim pull` recorded; when the message's own sha256 isn't among a page's
    claims, a neighbouring score's is tried, and one that is among them is listed too. A sha256
    nobody claimed is never listed, so a newer message of the page stays new work."""
    claimed = claimed or {}
    found = {}
    for record in records:
        if not (record.get("stage") == "published" or kept_by_policy(record)):
            continue
        message = (record.get("job") or {}).get("message")
        page = message.get("page") if isinstance(message, dict) else None
        if isinstance(page, str) and PAGE_KEY.fullmatch(page):
            shas = found.setdefault(page.replace("/", "_"), set())
            own = queue_message_sha256(message)
            shas.add(own)
            wanted = claimed.get(page, set())
            score = message.get("score")
            # A message the page's claims already hold byte for byte needs no neighbour: a claim
            # of an adjacent score is then another message, and stays new work.
            if wanted and own not in wanted and type(score) is float and math.isfinite(score):
                for toward in (math.inf, -math.inf):
                    sha = queue_message_sha256({**message, "score": math.nextafter(score, toward)})
                    if sha in wanted:
                        shas.add(sha)
    return found


def kept_by_policy(record):
    """Collected and kept, not published, because the trust list holds the job's scope (#835):
    nothing is wrong with it, and collecting it again changes nothing until the hold is lifted."""
    return record["stage"] not in ("published", "failed") and bool(record.get("held"))


def settled(record):
    """Nothing more for the controller to do with this job: published, failed for good, or kept
    by a held trust scope."""
    return record["stage"] in ("published", "failed") or kept_by_policy(record)


def retained_backlog(config, records, owned, selected, pending_jobs, passed=()):
    """Work the global cap counts beyond BOINC's pending workunits: journals of any plan that
    are neither settled nor pending on BOINC (uncollected results, recovered reservations),
    and selected pages with no journal yet. `passed` are pages passed over for another plan's
    journal (#1685): not missing work, though their unsettled journals still count."""
    uncollected = sum(
        not settled(record) and record["job"]["id"] not in pending_jobs
        for record in records
    )
    missing = [page for page in config["pages"] if page["page"] in selected and page["page"] not in owned
               and page["page"] not in passed]
    return uncollected, missing


def held_results(state, held_jobs, now):
    """Track since when each result has been held at collection (returned by BOINC, not
    published); returns the count and the oldest one's age in seconds."""
    since = state.setdefault("held_since", {})
    for job_id in [job_id for job_id in since if job_id not in held_jobs]:
        del since[job_id]
    for job_id in held_jobs:
        since.setdefault(job_id, int(now))
    return len(since), int(max((now - at for at in since.values()), default=0))


def native_capacity(config, snapshot):
    """A native plan's room on each of its hosts (#1780): `units_per_cpu` units (default 1) for
    each CPU the host's client last reported (the inventory's host_cpus), at most pages_per_host.
    A host not active in the last day, or an inventory from a server image without host_cpus,
    gets none. A host asks for work only now and then, and backs off when it finds none, so a
    room of one unit per CPU leaves it idle between ticks; units_per_cpu is what makes the room
    last."""
    cpus = snapshot.get("host_cpus", {})
    require(isinstance(cpus, dict) and len(cpus) <= 100000 and
            all(isinstance(host, str) and host.isdecimal() and type(n) is int and n >= 0
                for host, n in cpus.items()), "invalid host CPU inventory")
    per_cpu = config.get("units_per_cpu", 1)
    return {host: min(cpus.get(str(host), 0) * per_cpu, config["pages_per_host"])
            for host in config["native_hosts"]}


def assign_native_hosts(job_ids, slots, outstanding):
    """(job id, host) for as many of `job_ids` as the hosts have room for: each to the host with
    the most room left (the lowest id on a tie), never past a host's slots. `outstanding` (host
    -> its units not yet settled) is updated."""
    units = []
    for job_id in job_ids:
        room = {host: n - outstanding.get(host, 0) for host, n in slots.items()}
        host = max(sorted(room), key=lambda h: room[h], default=None)
        if host is None or room[host] <= 0:
            break
        outstanding[host] = outstanding.get(host, 0) + 1
        units.append((job_id, host))
    return units


def native_submit_untouched(transport, job_ids, clock):
    """True only when a fresh inventory proves the server holds nothing for these jobs: no BOINC
    workunit named usnm_<job> and no bridge journal. Anything unreadable, or a workunit list cut
    at its limit, is not proof. `clock()` is the time now, read after the inventory: the failed
    step may have waited the lock for 20 minutes, so the tick's own start says nothing."""
    try:
        snapshot = transport.inventory()
        limit = snapshot.get("existing_jobs_limit", MAX_JOBS)
        existing, pending, journals = snapshot["existing_jobs"], snapshot["pending_jobs"], snapshot["jobs"]
        if (snapshot["schema_version"] != 1 or not -60 <= clock() - snapshot["created_at"] <= 3600
                or not isinstance(existing, list) or not isinstance(pending, list)
                or not isinstance(journals, dict) or type(limit) is not int or len(existing) >= limit):
            return False
        return not set(job_ids) & (set(existing) | set(pending) | set(journals))
    except (ValueError, KeyError, TypeError, OSError, RuntimeError):
        return False


def tick(config, state, transport, persist, now=None, *, drain=False, dead_letter_failed=False,
         low_watermark=None, high_watermark=None, max_global_pending=None, server_status=None,
         refill_cap=None, other_app_budget=None, sibling_archived=None, sibling_claims=None,
         resume_native=False):
    if config.get("app") == SURYA_APP_NAME:
        require(refill_cap is None, "--refill-cap is for OCR plans")
        require(other_app_budget is None, "--other-app-budget is for OCR plans")
        return surya_tick(config, state, transport, persist, now, drain=drain,
                          low_watermark=low_watermark, high_watermark=high_watermark,
                          server_status=server_status)
    # A native plan (#1780): its units go only to its native_hosts, and a native result it can't
    # publish pauses it rather than stopping the tick.
    native = config.get("app") == NATIVE_APP_NAME
    require(native or not resume_native, f"--resume-native is for {NATIVE_APP_NAME} plans")
    if resume_native:
        cleared = [state.pop("native_paused", None), state.pop("native_lock_retry", None)]
        if any(value is not None for value in cleared):
            persist()
    # Claims, exports and submits per tick (#1447). Collection still takes up to batch_size, so a
    # shorter tick doesn't leave held results that stop the next refill.
    refill = config["batch_size"] if refill_cap is None else min(config["batch_size"], refill_cap)
    feed = (QueueFeed(config, state, transport, persist, sibling_archived)
            if config.get("source") == "queue" else None)
    if feed:
        feed.restore()
    snapshot = transport.inventory()
    now = time.time() if now is None else now
    tick_started = time.monotonic()
    require(snapshot["schema_version"] == 1 and -60 <= now - snapshot["created_at"] <= 3600,
            "inventory is not fresh")
    if feed:
        warn_near_cap("claims", len(feed.claims), MAX_JOBS, now)
        warn_near_cap("archived_held", len(feed.archived_held), MAX_ARCHIVED_HELD, now)
    for key in ("pending_workunits", "active_hosts"):
        require(type(snapshot[key]) is int and snapshot[key] >= 0, "invalid capacity inventory")
    native_slots = native_capacity(config, snapshot) if native else None
    pending_jobs = snapshot["pending_jobs"]
    require(isinstance(pending_jobs, list) and len(pending_jobs) <= MAX_JOBS and
            all(isinstance(job_id, str) and ID.fullmatch(job_id) for job_id in pending_jobs) and
            len(set(pending_jobs)) == len(pending_jobs), "invalid pending job inventory")
    pending_jobs = set(pending_jobs)
    existing_limit = snapshot.get("existing_jobs_limit", MAX_JOBS)
    require(type(existing_limit) is int and 1 <= existing_limit <= MAX_EXISTING_WORKUNITS_CEILING,
            "invalid existing workunit limit")
    existing_jobs = snapshot["existing_jobs"]
    require(isinstance(existing_jobs, list) and len(existing_jobs) <= existing_limit and
            all(isinstance(job_id, str) and ID.fullmatch(job_id) for job_id in existing_jobs) and
            len(set(existing_jobs)) == len(existing_jobs) and pending_jobs <= set(existing_jobs),
            "invalid existing workunit inventory")
    # Workunit rows before bundles become pages; an image from before it reports none (#1483).
    existing_workunits = snapshot.get("existing_workunits", 0)
    require(type(existing_workunits) is int and 0 <= existing_workunits <= existing_limit,
            "invalid existing workunit inventory")
    existing_jobs = set(existing_jobs)
    server = snapshot["jobs"]
    require(isinstance(server, dict) and len(server) <= MAX_JOBS, "invalid server job inventory")
    for job_id, job in server.items():
        require(ID.fullmatch(job_id) and isinstance(job, dict) and job.get("stage") in STAGES
                and (job.get("spool") is None or isinstance(job["spool"], str)),
                "invalid server job record")
    warn_near_cap("bridge_journals", len(server), MAX_JOBS, now)
    # Bundles whose journals are gone count as rows but add no pages, so warn on the larger.
    warn_near_cap("existing_workunits", max(len(existing_jobs), existing_workunits), existing_limit, now)
    spool = snapshot.get("spool_entries"), snapshot.get("spool_entries_limit")
    if all(type(value) is int for value in spool) and spool[1] > 0:
        warn_near_cap("spool_entries", *spool, now)
    # A job of this plan that another plan already ran and archived as held (same page, same
    # worker, so the same job id, whose BOINC row stays): settled as held, never submitted.
    # Marked on every read of the journals, in memory only; nothing writes them back.
    sibling_archived = sibling_archived or {}

    def archived_elsewhere(record):
        # The same page, job and message as an archive of another plan: a job id leaves out
        # queued_at, so a different message is another attempt, never settled here.
        page = record["job"]["message"]["page"]
        message = state.get("claims", {}).get(page, {}).get("message")
        return message is not None and any(
            entry.get("job") == record["job"]["id"] and entry.get("message") == message
            for entry in sibling_archived.get(page, []))
    elsewhere_held = set()

    def read_records():
        found = transport.records()
        for record in found:
            if (record.get("plan") == config["plan"] and record.get("stage") == "reserved"
                    and not record.get("held") and archived_elsewhere(record)):
                record["held"] = "archived as held by another plan with the same worker"
                elsewhere_held.add(record["job"]["id"])
        return found

    records = read_records()
    # Here rather than in journal_inventory, which a tick runs several times.
    warn_near_cap("dispatch_journals", len(records), MAX_JOBS, now)
    # A queue-fed plan passes over a claimed page another plan's journal is for (#1685): it
    # exports, submits and collects nothing for it, and logs it for `archive_history.py
    # reconcile`, so one stranded page doesn't stop the plan. A fixed page list still refuses.
    conflicts = {}
    reported = set()

    def own(records):
        found = {} if feed else None
        owned = owned_records(config, records, state.get("deadline_retries"), found)
        conflicts.clear()
        conflicts.update(found or {})
        fresh = sorted(page for page in set(conflicts) - reported
                       if not conflicts[page]["settled_elsewhere"])
        if fresh:
            reported.update(fresh)
            try:
                print(json.dumps({
                    "event": "boinc_batch_ownership_conflict", "schema_version": 1, "plan": config["plan"],
                    "pages": [{"page": page, "other_plans": conflicts[page]["plans"],
                               "own_stage": (conflicts[page]["own"] or {}).get("stage")} for page in fresh],
                    "message": "passed over: another plan's journal is for these claimed pages; "
                               "reconcile with archive_history.py reconcile (#1685)"},
                    sort_keys=True), file=sys.stderr, flush=True)
            except (OSError, ValueError):
                pass
        return owned

    owned = own(records)
    for record in owned.values():
        job_id = record["job"]["id"]
        require(server.get(job_id, {}).get("stage") != "submitting",
                "uncertain BOINC submission; reconcile explicitly, never automatically resubmit")
        # A held job (#835) is settled like a published one and never submitted again; one
        # restored from the archive has no bridge journal left, but may still have its row.
        require(settled(record) or job_id not in existing_jobs or
                server.get(job_id, {}).get("stage") == "submitted",
                "SQL/journal mismatch; reconcile existing work before any automatic submission")
    selected = state.setdefault("selected", {})
    for page in owned:
        selected.setdefault(page, now)
    held_jobs = {record["job"]["id"] for record in owned.values()
                 if not settled(record)
                 and server.get(record["job"]["id"], {}).get("spool")}

    def settle_failed(owned):
        """A page whose BOINC workunit failed for good stops the tick, naming it, unless
        --dead-letter-failed sends it to ocr-deadletter (#1154): released, it would come back as
        the same job and workunit. A dead-lettered job is recorded and passed over after."""
        letters = state.setdefault("dead_lettered", {})
        failed = {page: record for page, record in owned.items()
                  if record["stage"] == "failed" and record["job"]["id"] not in letters}
        if not failed:
            return
        names = ", ".join(f"{page} (job {record['job']['id'][:12]})" for page, record in sorted(failed.items()))
        require(dead_letter_failed and feed is not None,
                f"terminal BOINC failure: {names}; dead-letter it with --dead-letter-failed "
                "(#1154) or reconcile it by hand")
        jobs = sorted(record["job"]["id"] for record in failed.values())
        for group in chunks(jobs):  # the step takes STEP_IDS job ids at most
            transport.dead_letter(group)
        for record in failed.values():
            letters[record["job"]["id"]] = int(now)
        persist()

    settle_failed(owned)
    if feed:
        feed.retry_truncated(now)
    held_results(state, held_jobs, now)
    persist()
    # Before collection: a host admitted now has its held results published by this tick's
    # collect, which otherwise stops at the first result it must hold.
    gold_runs = []
    gold_ran = False
    # A gold tick doesn't refill, so the tick after one never runs gold: with a short interval
    # (10 minutes) gold would otherwise be due on every tick and the plan would never refill.
    after_gold = bool(state.pop("gold_tick", False))
    if after_gold:
        # Saved now: a collect that fails on a held result would otherwise leave the marker on
        # disk, and every later run would skip gold, the one thing that can admit the host.
        persist()
    if not drain and not after_gold and config.get("gold_onboarding"):
        # The same caps and the same retained backlog the refill below counts, from the same
        # status. Without status there is no room for gold units (promotion still runs); the
        # status read after collection fails the tick as it always has.
        try:
            gold_status = _extract_status(snapshot, transport, server_status=server_status)
            caps = resolve_watermarks(config, snapshot, gold_status, low_watermark=low_watermark,
                                      high_watermark=high_watermark,
                                      max_global_pending=max_global_pending,
                                      other_app_budget=other_app_budget)
            uncollected, missing = retained_backlog(config, records, owned, selected, pending_jobs, conflicts)
            in_flight = max(snapshot["pending_workunits"], ocr_workunit_counts(snapshot, pending_jobs)[0])
            room = global_room(caps, in_flight + uncollected + len(missing),
                               other_app_workunits(snapshot, caps["app"]) or 0)
        except (ValueError, KeyError, TypeError, OSError, RuntimeError):
            room = 0
        gold_runs, reserved = gold_onboarding(config, state, transport, persist, now,
                                              held=bool(held_jobs), room=room)
        # The gold units just issued are in flight too, for this tick's refill.
        snapshot = {**snapshot, "pending_workunits": snapshot["pending_workunits"] + reserved}
        # A gold run takes about 9 minutes on CT 230: with a refill on top, the tick ran past
        # controller_tick's 15-minute limit, which fires while it still runs. The next tick refills.
        gold_ran = any(run["steps"].get("gold-onboard") != GOLD_SKIPPED_AT_CAP for run in gold_runs)
        if gold_ran:
            state["gold_tick"] = True
            persist()
    to_collect = []
    # A gold tick collects one step's worth (STEP_IDS): a slow collect on top of gold (615 s on
    # 10 Nov, with blob-store timeouts) would take it past 15 minutes. The next tick takes the rest.
    collect_limit = min(config["batch_size"], STEP_IDS) if gold_ran else config["batch_size"]
    for page, record in owned.items():
        job_id = record["job"]["id"]
        remote = server.get(job_id)
        require(not remote or remote["stage"] != "submitting",
                "uncertain BOINC submission; reconcile explicitly, never automatically resubmit")
        if record["stage"] == "failed":
            continue  # dead-lettered: settle_failed passed it
        if kept_by_policy(record):
            continue  # its scope is held (#835): collected again only by hand, once lifted
        if record["stage"] != "published" and remote and remote["spool"] and len(to_collect) < collect_limit:
            spool = remote["spool"]
            require(re.fullmatch(r"/[A-Za-z0-9_./-]+/results/[A-Za-z0-9_./-]+", spool)
                    and ".." not in spool, "invalid assimilator spool")
            to_collect.append((job_id, spool))
    collect_error = None

    def report_collected(found):
        """Log the pages this tick's collect published, as soon as collect ends, so the
        throughput collector counts them whether or not the tick finishes (#1784): the tick
        summary's newly_published is only logged by a tick that gets to its end."""
        wanted = {job_id for job_id, _ in to_collect}
        pages = sum(1 for record in found
                    if record["job"]["id"] in wanted and record.get("stage") == "published")
        if pages:
            # `published` is the tick summary's cumulative count as of now, so a reader can tell
            # what a later summary adds beyond the pages collect already reported.
            conflicting = {} if feed else None
            mine = owned_records(config, found, state.get("deadline_retries"), conflicting)
            total = sum(record["stage"] == "published" for record in mine.values()) + sum(
                bool(conflict["own"]) and conflict["own"]["stage"] == "published"
                for conflict in (conflicting or {}).values())
            try:
                print(json.dumps({"event": "boinc_batch_collected", "schema_version": 1,
                                  "at": int(now), "plan": config["plan"], "pages": pages,
                                  "published": total, "previous": state.get("published", 0)}, sort_keys=True), file=sys.stderr, flush=True)
            except (OSError, ValueError):
                pass

    try:
        if hasattr(transport, "collect_batch"):
            transport.collect_batch(to_collect)
        else:
            for job_id, spool in to_collect:
                transport.collect(job_id, spool)
    except RuntimeError as error:
        # Pages collected before the failure count too.
        if to_collect:
            try:
                report_collected(read_records())
            except (RuntimeError, OSError, ValueError, KeyError):
                pass
        # A native plan goes on below, if the failure left a result unpublished.
        if not native:
            raise
        collect_error = error
    count = len(to_collect)
    if count:
        records = read_records()
        owned = own(records)
        if collect_error is None:
            report_collected(records)
    if native:
        # A native result a collect attempt didn't publish (the trust list doesn't admit its host
        # for the native build, or its collect failed), or one kept under a held scope, stays as
        # it is, unpublished. While any is there, this plan stays paused with an alert, whether it
        # came in this tick or before, and --resume-native can't lift that; the tick goes on, and
        # container plans are untouched (#1780). A result not yet tried (past batch_size) is only
        # collection backlog: the attempted ones are kept on record (native_unpublished).
        collected = {job_id for job_id, _ in to_collect}
        unsettled = {record["job"]["id"] for record in owned.values() if not settled(record)}
        unpublished = sorted(unsettled & (collected | set(state.get("native_unpublished", []))))
        state["native_unpublished"] = unpublished
        kept = sorted(record["job"]["id"] for record in owned.values() if kept_by_policy(record))
        if collect_error is not None and not collected & set(unpublished):
            raise collect_error
        if unpublished or kept:
            reasons = (["collect left a native result unpublished"
                        + (f" ({collect_error})" if collect_error is not None else "")] if unpublished else [])
            reasons += ["a native result is kept by a held scope"] if kept else []
            since = (state.get("native_paused") or {}).get("at", int(now))
            state["native_paused"] = {"at": since, "reason": "; ".join(reasons),
                                      "jobs": (unpublished + kept)[:5]}
            persist()
    native_paused = state.get("native_paused") if native else None
    settle_failed(owned)
    status = _extract_status(snapshot, transport, server_status=server_status, drain=drain)
    cohort_info = resolve_watermarks(
        config, snapshot, status,
        low_watermark=low_watermark, high_watermark=high_watermark,
        max_global_pending=max_global_pending, other_app_budget=other_app_budget,
        native_slots=native_slots,
    )
    ocr_app = cohort_info["app"]
    active_hosts = cohort_info["active_hosts"]
    low_target = cohort_info["low_watermark"]
    high_target = cohort_info["high_watermark"]
    global_cap = cohort_info["max_global_pending"]
    cohort_mode = cohort_info["cohort_mode"]
    target = high_target

    uncollected_not_pending, missing = retained_backlog(config, records, owned, selected, pending_jobs, conflicts)
    # Workunits for the global cap and other apps' share; pages for OCR admission. They differ
    # only with OCR bundles (#1059), whose pages share one workunit.
    ocr_workunits, bundled_pages = ocr_workunit_counts(snapshot, pending_jobs)
    global_in_flight = max(snapshot["pending_workunits"], ocr_workunits)
    has_ocr_attribution = status is not None and ocr_app in status["apps"]
    if has_ocr_attribution:
        app_counts = status["apps"][ocr_app]
        ocr_unsent = app_counts["unsent"]
        ocr_in_progress = app_counts["in_progress"]
        ocr_status_results = app_counts["runnable"]
        other_status_results = sum(
            item["runnable"] for name, item in status["apps"].items() if name != ocr_app
        )
        ocr_in_flight = min(global_in_flight, max(ocr_workunits, ocr_status_results))
    else:
        ocr_unsent = global_in_flight
        ocr_in_progress = 0
        ocr_status_results = global_in_flight
        other_status_results = sum(
            item["runnable"] for name, item in (status["apps"].items() if status is not None else ())
            if name != ocr_app
        )
        ocr_in_flight = global_in_flight

    other_pending = max(0, global_in_flight - ocr_in_flight)
    # By the inventory's per-app count, which leaves out this tick's gold units: they count in
    # full, having taken room under the cap.
    other_workunits = other_app_workunits(snapshot, ocr_app)
    budgeted_other = other_workunits or 0
    ocr_in_flight += bundled_pages
    cohort_uncollected = uncollected_not_pending
    if native:
        # Only this plan's own work is the native plan's (#1780): every plan's units are named
        # usnm_<job>, and container work isn't its to count. The global cap still counts it all.
        ocr_in_flight = sum(record["job"]["id"] in pending_jobs for record in owned.values())
        cohort_uncollected = sum(not settled(record) and record["job"]["id"] not in pending_jobs
                                 for record in owned.values())
    cohort_pending = ocr_in_flight + cohort_uncollected + len(missing)
    global_pending = global_in_flight + uncollected_not_pending + len(missing)
    global_pending_counted = counted_pending(cohort_info, global_pending, budgeted_other)
    cohort_pending_results = (max(ocr_workunits, ocr_status_results) + bundled_pages
                              + uncollected_not_pending + len(missing))
    other_app_pending_results = other_status_results
    global_pending_results = cohort_pending_results + other_app_pending_results
    pending = cohort_pending if cohort_mode else global_pending

    uncertain = any(job["stage"] == "submitting" for job in server.values())
    require(not uncertain, "uncertain server submission blocks fresh allocation")
    held = sum(not settled(record) and
               bool(server.get(record["job"]["id"], {}).get("spool")) for record in owned.values())

    if high_target == 0:
        refilling = False
    elif pending <= low_target:
        refilling = True
    elif pending >= high_target:
        refilling = False
    else:
        refilling = bool(state.get("refilling", False))

    cohort_free = max(0, high_target - pending) if refilling else 0
    global_free = max(0, global_room(cohort_info, global_pending, budgeted_other))
    free = min(cohort_free, global_free)
    submitted = 0
    if feed:
        # Every tick, so the list stays current when this one claims nothing (#1754).
        feed.settled_messages = settled_messages(records, claimed_messages(state.get("claims"), sibling_claims))
        feed.publish_settled()
    if target and not held and not drain and not gold_ran and not native_paused:
        if free > 0:
            if feed:
                feed.settled_messages = settled_messages(
                    records, claimed_messages(state.get("claims"), sibling_claims))
                feed.fill(min(refill, free), selected, owned, now)
                # The fresh claims are allocated now: one another plan's journal is for is passed
                # over and reported, even when nothing else is exported (#1685).
                owned = own(records)
            fresh = [page for page in config["pages"] if page["page"] not in selected][
                :min(refill, free)]
            for page in fresh:
                selected[page["page"]] = now
            persist()

        def retried(page):
            # A deadline retry's claim (#1417) whose job isn't exported yet: the page's journal
            # is still the dead-lettered one it supersedes.
            record = owned.get(page["page"])
            return bool(record and record["stage"] == "failed"
                        and page.get("supersedes") == record["job"]["id"])

        # Never a page any other plan's journal is for, a page claimed this tick included (#1685).
        foreign = {record["job"]["message"]["page"] for record in records if record["plan"] != config["plan"]}
        export = [page for page in config["pages"] if page["page"] in selected and
                  not page.get("cut_short") and
                  page["page"] not in foreign and
                  (page["page"] not in owned or retried(page) or
                   (owned[page["page"]]["stage"] == "prepared"
                    and not owned[page["page"]].get("retry")))]
        if export:
            export = export[:refill]
            for group in chunks(export):  # an export step takes STEP_IDS pages and journals
                pages = {page["page"] for page in group}
                journals = [record["job"]["id"] for record in records
                            if record["job"]["message"]["page"] in pages]
                require(len(journals) <= STEP_IDS,
                        "too many existing journals for selected pages; reconcile history")
                truncated = transport.export(group, journals)
                if truncated and feed:
                    feed.settle_export_truncated(truncated, selected, now, owned)
                    export = [page for page in export if page["page"] not in truncated]
            records = read_records()
            owned = own(records)
            stuck = sorted(page["page"] for page in export if retried(page))
            require(not stuck, f"the deadline retry of {', '.join(stuck)} exported no job of its own; "
                    "reconcile it before the next tick")
        # A held job (#835) stays `reserved` once collected. Its bridge journal says `submitted`
        # until it is archived; restored from the archive it has none, and is never submitted.
        candidates = {record["job"]["id"] for record in owned.values()
                      if not record.get("retry") and record["stage"] == "reserved"
                      and not kept_by_policy(record)
                      and record["job"]["id"] not in existing_jobs
                      and server.get(record["job"]["id"], {}).get("stage") in (None, "reserved")}
        backlog = sum(not settled(record) and
                      record["job"]["id"] not in pending_jobs | candidates for record in records)
        backlog += sum(page not in owned and page not in conflicts for page in selected)
        cohort_backlog = backlog
        if native:
            cohort_backlog = sum(not settled(record) and
                                 record["job"]["id"] not in pending_jobs | candidates
                                 for record in owned.values())
            cohort_backlog += sum(page not in owned and page not in conflicts for page in selected)
        cohort_submit_room = max(0, target - ocr_in_flight - cohort_backlog)
        global_submit_room = max(0, global_room(cohort_info, global_in_flight + backlog, budgeted_other))
        submit_limit = min(refill, cohort_submit_room, global_submit_room)
        to_submit = []
        for record in owned.values():
            job_id = record["job"]["id"]
            if not record.get("retry") and record["stage"] == "reserved" and \
                    not kept_by_policy(record) and \
                    server.get(job_id, {}).get("stage") in (None, "reserved") and \
                    len(to_submit) < submit_limit:
                require(job_id not in existing_jobs,
                        "SQL/journal mismatch; reconcile existing work before any automatic submission")
                to_submit.append(job_id)
        if native:
            # Each unit to one of the plan's hosts with room: its CPUs, at most pages_per_host,
            # less its units not yet settled. Recorded before submission, so a tick that stops
            # in it leaves the choice on record; a reserved job is assigned again each tick.
            targets = state.setdefault("native_targets", {})
            open_jobs = {record["job"]["id"] for record in owned.values() if not settled(record)}
            for job_id in [job_id for job_id in targets if job_id not in open_jobs]:
                del targets[job_id]
            # A job still reserved here (this tick's, or one a refused batch left past it) has no
            # unit yet, so takes no host's room; it is assigned again when it is submitted.
            outstanding = {}
            for job_id, host in targets.items():
                if job_id not in candidates:
                    outstanding[host] = outstanding.get(host, 0) + 1
            units = assign_native_hosts(to_submit, native_slots, outstanding)
            for job_id, host in units:
                targets[job_id] = host
            persist()
            to_submit = [job_id for job_id, _ in units]
            if units:
                lock_retry = False
                try:
                    transport.submit_native(units)
                    if state.pop("native_lock_retry", None) is not None:
                        persist()
                except RuntimeError as error:
                    # Refused by trust at submission, or failed: that job and the ones after it
                    # were not created. The plan pauses with an alert (an uncertain one stops
                    # the next tick before any submission, as for every plan).
                    reason = f"native submission failed ({error})"
                    if isinstance(error, StepLockTimeout):
                        # Only the host's step lock kept it from starting. When the server
                        # provably holds nothing for these jobs, they stay reserved and the next
                        # tick submits them again, up to NATIVE_LOCK_RETRY_LIMIT ticks in a row.
                        tries = (state.get("native_lock_retry") or {}).get("count", 0) + 1
                        if tries <= NATIVE_LOCK_RETRY_LIMIT and native_submit_untouched(
                                transport, [job_id for job_id, _ in units],
                                lambda: now + (time.monotonic() - tick_started)):
                            state["native_lock_retry"] = {"at": int(now), "count": tries,
                                                          "jobs": to_submit[:5]}
                            persist()
                            print(json.dumps({
                                "event": "boinc_native_submit_lock_retry", "schema_version": 1,
                                "plan": config["plan"], "count": tries, "jobs": len(units),
                                "message": "step lock timeout, no workunit or bridge journal "
                                           "for these jobs: submitted again next tick"},
                                sort_keys=True), file=sys.stderr, flush=True)
                            lock_retry = True
                        else:
                            reason += (f"; {tries} step-lock timeouts in a row" if tries > NATIVE_LOCK_RETRY_LIMIT
                                       else "; could not prove that no workunit was created")
                    if not lock_retry:
                        state["native_paused"] = {"at": int(now), "reason": reason, "jobs": to_submit[:5]}
                        persist()
                        native_paused = state["native_paused"]
                    to_submit = []
        elif hasattr(transport, "submit_batch"):
            if max(config.get("pages_per_unit", {}).values(), default=1) > 1:
                by_id = {record["job"]["id"]: record for record in owned.values()}
                units = units_of(config, [by_id[j] for j in to_submit])
                if any(len(unit) > 1 for unit in units):
                    # Pinned bundles the scheduler won't send starve the plan (#1687): refuse,
                    # after collection, so controller_tick pages and nothing is submitted.
                    if "ocr_app_versions" not in snapshot:
                        print(json.dumps({
                            "event": "boinc_batch_pin_unchecked", "schema_version": 1,
                            "app_version_num": config["bundle_app_version"],
                            "message": "the inventory has no ocr_app_versions (a server image from "
                                       "before #1687): bundles submitted without checking their pin"},
                            sort_keys=True), file=sys.stderr, flush=True)
                    problem = pinned_version_problem(snapshot, config["bundle_app_version"])
                    require(problem is None, problem or "")
                transport.submit_batch(to_submit, units)
            else:
                transport.submit_batch(to_submit)
        else:
            for job_id in to_submit:
                transport.submit(job_id)
        submitted += len(to_submit)

    post_uncollected_not_pending = sum(
        not settled(record) and record["job"]["id"] not in pending_jobs
        for record in (owned.values() if native else records)
    )
    post_missing = sum(page not in owned and page not in conflicts for page in selected)
    post_cohort_pending = ocr_in_flight + post_uncollected_not_pending + post_missing
    if high_target == 0 or post_cohort_pending >= high_target:
        state["refilling"] = False
    else:
        state["refilling"] = refilling

    global_block = global_cap_reason(cohort_info, global_pending, budgeted_other)
    if drain:
        backpressure = True
        backpressure_reason = "draining"
    elif native_paused:
        backpressure = True
        backpressure_reason = "native_paused"
    elif held > 0:
        backpressure = True
        backpressure_reason = "collection_backlog"
    elif gold_ran:
        backpressure = True
        backpressure_reason = "gold_tick"
    elif target == 0:
        backpressure = True
        backpressure_reason = "no_native_hosts" if native else "no_active_hosts"
    elif cohort_mode and global_block:
        backpressure = True
        backpressure_reason = global_block
    elif pending >= target:
        backpressure = True
        backpressure_reason = "cohort_high_watermark"
    elif not refilling:
        backpressure = True
        backpressure_reason = "cohort_hysteresis"
    elif global_block:
        backpressure = True
        backpressure_reason = global_block
    else:
        backpressure = False
        backpressure_reason = None

    zero_work_requests = max(
        (status or {}).get("zero_work_requests", 0),
        parse_zero_work_replies(
            snapshot.get("zero_work_requests")
            if "zero_work_requests" in snapshot
            else snapshot.get("no_tasks_sent", snapshot.get("zero_work_hosts", snapshot.get("scheduler_log")))
        ),
        parse_zero_work_replies(getattr(transport, "zero_work_requests", None)),
        parse_zero_work_replies(getattr(transport, "scheduler_log", None)),
    )
    effective_unsent = ocr_unsent if has_ocr_attribution else global_in_flight
    cohort_queue_empty_or_low = (effective_unsent == 0 or cohort_pending <= low_target)
    starvation_alert = bool(
        cohort_queue_empty_or_low and (active_hosts > 0 or zero_work_requests > 0)
    )
    alerts = [{
        "code": "zero_work_starvation" if zero_work_requests > 0 else "cohort_starvation",
        "app": ocr_app,
        "cohort": cohort_info["cohort"],
        "active_hosts": active_hosts,
        "unsent": effective_unsent,
        "in_progress": ocr_in_progress,
        "runnable_pending": cohort_pending,
        "low_watermark": low_target,
        "high_watermark": high_target,
        "other_app_pending": other_pending,
        "other_app_pending_results": other_app_pending_results,
        "zero_work_requests": zero_work_requests,
        "refilled": submitted,
        "blocked_by": backpressure_reason,
    }] if starvation_alert else []
    # A page whose work another plan has published or keeps under a held scope is passed over
    # without an alert: nothing is wrong (its returning queue message was claimed again).
    disputed = sorted(page for page, entry in conflicts.items() if not entry["settled_elsewhere"])
    if disputed:
        alerts.append({"code": "ownership_conflict", "pages": len(disputed),
                       "first": [{"page": page, "other_plans": conflicts[page]["plans"]}
                                 for page in disputed[:5]]})
    if native_paused:
        alerts.append({"code": "native_paused", "app": NATIVE_APP_NAME, **native_paused,
                       "message": "native OCR paused; inspect the held results, then --resume-native"})
    for run in gold_runs:
        for step, outcome in run["steps"].items():
            if outcome not in ("ok", GOLD_SKIPPED_AT_CAP):
                alerts.append({"code": "gold_onboarding_failed", "scope": run["scope"], "step": step,
                               "error": outcome})
    held_count, oldest_held = held_results(state, {
        record["job"]["id"] for record in owned.values() if not settled(record)
        and server.get(record["job"]["id"], {}).get("spool")}, now)
    scope_held = sum(kept_by_policy(record) for record in owned.values())

    # A passed-over page's own published journal still counts, or newly_published would count
    # it twice once the conflict is reconciled.
    published = sum(record["stage"] == "published" for record in owned.values()) + sum(
        bool(conflict["own"]) and conflict["own"]["stage"] == "published" for conflict in conflicts.values())
    previous = state.get("published", 0)
    started = state.setdefault("started_at", now)
    state["published"] = published
    persist()
    event = {
        "event": "boinc_batch_tick", "schema_version": 1, "at": int(now), "plan": config["plan"],
        "draining": drain,
        "active_hosts": active_hosts, "target_pending": target,
        "observed_pending": pending, "selected": len(selected), "published": published,
        "collection_backlog": held,
        "collection_held": held_count,
        "oldest_held_seconds": oldest_held,
        "scope_held": scope_held,
        "scope_held_archived": len(state.get("archived_held", {})),
        "scope_held_elsewhere": len(elsewhere_held),
        "ownership_conflicts": len(disputed),
        "settled_elsewhere": len(conflicts) - len(disputed),
        "gold_onboarding": gold_runs,
        "newly_published": max(0, published - previous),
        "pages_per_hour": round(published * 3600 / max(1, now - started), 3),
        "oldest_pending_seconds": int(max((now - at for page, at in selected.items()
                                          if page not in owned or not settled(owned[page])),
                                         default=0)),
        "backpressure": backpressure,
        "backpressure_reason": backpressure_reason,
        "refill_cap": refill,
        "low_watermark": low_target,
        "high_watermark": high_target,
        "cohort_pending": cohort_pending,
        "other_app_pending": other_pending,
        "global_pending": global_pending,
        "global_pending_counted": global_pending_counted,
        "other_app_workunits": other_workunits,
        "cohort_pending_results": cohort_pending_results,
        "other_app_pending_results": other_app_pending_results,
        "global_pending_results": global_pending_results,
        "max_global_pending": global_cap,
        "other_app_budget": cohort_info["other_app_budget"],
        "global_hard_cap": cohort_info["global_hard_cap"],
        "cohort": {
            "name": cohort_info["cohort"],
            "app": ocr_app,
            "platform": cohort_info["platform"],
            "resource": cohort_info["resource"],
            "active_hosts": active_hosts,
            "unsent": effective_unsent,
            "in_progress": ocr_in_progress,
            "runnable_pending": cohort_pending,
            "low_watermark": low_target,
            "high_watermark": high_target,
            "refilling": bool(state.get("refilling", False)),
        },
        "zero_work_requests": zero_work_requests,
        "starvation_alert": starvation_alert,
        "alerts": alerts,
    }
    if native:
        targets = state.get("native_targets", {})
        event["native"] = {
            "hosts": config["native_hosts"],
            "slots": {str(host): n for host, n in native_slots.items()},
            "outstanding": {str(host): sum(h == host for h in targets.values()) for host in config["native_hosts"]},
            "paused": native_paused,
        }
    return with_timings(event, transport)


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--config", required=True, type=Path)
    parser.add_argument("--state-dir", required=True, type=Path)
    parser.add_argument("--apply", action="store_true",
                        help="mutate the explicitly allocated feed; requires serialized operator approval")
    parser.add_argument("--drain", action="store_true",
                        help="collect existing results without selecting, exporting or submitting work")
    parser.add_argument("--low-watermark", type=int, default=None,
                        help="runnable cohort low watermark at or below which refill begins")
    parser.add_argument("--high-watermark", type=int, default=None,
                        help="runnable cohort high watermark target for refill")
    parser.add_argument("--max-global-pending", type=int, default=None,
                        help="global safety cap on total unassimilated work across all BOINC apps")
    parser.add_argument("--refill-cap", type=int, default=None,
                        help="claim, export and submit at most this many pages a tick (1-500, in steps of at most 100; never more than "
                             "batch_size, the default); collection still takes up to batch_size (#1447)")
    parser.add_argument("--other-app-budget", type=int, default=None,
                        help="count at most this many of other apps' unassimilated workunits toward "
                             "--max-global-pending (needed, and above it), and hold all apps' work to the "
                             f"feeder's {FEEDER_SHMEM_SLOTS} slots (#1474); needs the server's per-app "
                             "inventory counts to take effect")
    parser.add_argument("--server-status-url", default=None,
                        help="optional https server_status.php?xml=1 URL for per-app queue counts")
    parser.add_argument("--server-status-file", type=Path, default=None,
                        help="optional local file with server_status.php?xml=1 or structured status")
    parser.add_argument("--batch-submit", action="store_true",
                        help="submit a tick's jobs in one bridge execution (needs a bridge image whose submit-remote takes JOB_IDS)")
    parser.add_argument("--batch-spool-out", action="store_true",
                        help="spool out a tick's results in one bridge execution (needs spool-out with HANDOFF_SPOOLS)")
    parser.add_argument("--batch-collect", action="store_true",
                        help="collect a tick's results in one hand-off execution (needs a hand-off image whose collect takes JOB_IDS)")
    parser.add_argument("--collect-parallel", type=int, default=1,
                        help=f"with --batch-collect, collect up to this many jobs at once (1 to {MAX_COLLECT_PARALLEL}, "
                             "default 1; needs a hand-off image whose collect takes COLLECT_PARALLEL, #1447)")
    parser.add_argument("--dead-letter-failed", action="store_true",
                        help="send a queue-fed page whose BOINC workunit failed for good to ocr-deadletter and go on "
                             "(#1154; needs the hand-off identity's sender role on ocr-deadletter); without it such a "
                             "page stops the tick")
    parser.add_argument("--skip-truncated-scans", action="store_true",
                        help="check each claimed page's JP2 headers before its scan is pushed, and send one LoC "
                             "holds cut short to ocr-deadletter, never to OCR (#1457; needs push-boinc-scan.sh "
                             "--skip-truncated and a hand-off image whose dead-letter step takes "
                             "SOURCE_TRUNCATED_DOCS)")
    parser.add_argument("--batch-export", action="store_true",
                        help="export a tick's pages from all their batches in one hand-off execution "
                             "(needs a hand-off image whose export takes HANDOFF_BATCHES)")
    parser.add_argument("--check-state-dir", action="append", default=[], type=Path, metavar="DIR",
                        help="another plan's state directory, read only: pages it archived as held "
                             "(archived_held) are kept hidden here and their jobs settled as held, "
                             "as for this plan's own (needed when the plans share a worker); repeat")
    parser.add_argument("--claim-language", action="append", default=[], metavar="LANG",
                        help="claim pages in this source language too, beside the configuration's "
                             "claim_languages, without changing the allocation digest; repeat for more. "
                             "Use it for a language whose scope trust.json holds or trusts")
    parser.add_argument("--resume-native", action="store_true",
                        help=f"a {NATIVE_APP_NAME} plan paused on a native result it could not publish, or "
                             "a refused submission (#1780): clear the pause and go on; a result still held "
                             "pauses it again")
    parser.add_argument("--step-lock-wait-secs", type=int, default=0, metavar="SECONDS",
                        help="how long submit-remote and the gold steps wait for the BOINC host's step lock "
                             f"(0 to {MAX_STEP_LOCK_WAIT_SECS}; default 0: the host's own wait). Sent to "
                             "usnm-boinc-step as --lock-wait-secs, so install a launcher that takes it first")
    parser.add_argument("--rest-journals", action="store_true",
                        help="list and fetch journals over the storage REST API with az's token")
    parser.add_argument("--local-lock", action="store_true",
                        help="hold the controller lock as an expiring record on the BOINC host (30 s heartbeat, "
                             "5 min TTL) through step_command, and fence the mutating hand-off steps with it (#1367)")
    parser.add_argument("--onprem-handoff", action="append", choices=sorted(ONPREM_HANDOFF), default=[],
                        help="run this hand-off step (claim, with its release, or collect) on the on-prem BOINC "
                             "host through step_command instead of the Azure hand-off job; repeat for both (#967)")
    args = parser.parse_args(argv)
    timings = None
    state = None
    try:
        config = load_config(args.config)
        if args.low_watermark is not None or args.high_watermark is not None:
            _validate_cohort_spec({
                k: v for k, v in (
                    ("low_watermark", args.low_watermark),
                    ("high_watermark", args.high_watermark or config.get("high_watermark")),
                ) if v is not None
            })
        if args.max_global_pending is not None:
            require(1 <= args.max_global_pending <= MAX_GLOBAL_PENDING, "invalid max_global_pending")
        if args.refill_cap is not None:
            require(1 <= args.refill_cap <= MAX_TICK_PAGES, "invalid --refill-cap")
            require(config.get("app") != SURYA_APP_NAME, "--refill-cap is for OCR plans")
        if args.other_app_budget is not None:
            require(config.get("app") != SURYA_APP_NAME, "--other-app-budget is for OCR plans")
            # As the tick checks it, so a dry run catches it: below a cap set explicitly.
            cap = (args.max_global_pending if args.max_global_pending is not None
                   else config.get("max_global_pending"))
            require(cap is not None and 0 <= args.other_app_budget < min(cap, FEEDER_SHMEM_SLOTS),
                    "invalid other_app_budget")
        if args.server_status_url is not None:
            _validate_status_url(args.server_status_url)
        if args.claim_language:
            require(config.get("source") == "queue" and config.get("claim_languages"),
                    "--claim-language needs a queue plan with claim_languages")
            require(len(set(args.claim_language)) == len(args.claim_language)
                    and not set(args.claim_language) & set(config["claim_languages"])
                    and all(re.fullmatch(r"[a-z]{3}", l) for l in args.claim_language),
                    "--claim-language adds new three-letter codes to the configuration's claim_languages")
        require(not args.resume_native or config.get("app") == NATIVE_APP_NAME,
                f"--resume-native is for {NATIVE_APP_NAME} plans")
        require(1 <= args.collect_parallel <= MAX_COLLECT_PARALLEL, "invalid --collect-parallel")
        require(args.collect_parallel == 1 or args.batch_collect, "--collect-parallel needs --batch-collect")
        surya = config.get("app") == SURYA_APP_NAME
        require(not args.local_lock or (config.get("step_command") and not surya
                                        and set(args.onprem_handoff) == set(ONPREM_HANDOFF)),
                "--local-lock needs a hand-off plan with step_command and --onprem-handoff claim, collect and export")
        require(not args.local_lock or "handoff_root" in config,
                "--local-lock is for a plan with a local handoff_root, not one backed by Azure")
        require(not args.onprem_handoff or (config.get("step_command") and not surya),
                "--onprem-handoff needs a hand-off plan with step_command")
        if not args.apply and surya:
            # What-if: the steps a tick would run when the queue is empty, with the largest
            # SURYA_MAX the submit step can get. Nothing is read or run.
            high = args.high_watermark or config["high_watermark"]
            print(json.dumps({
                "event": "boinc_surya_plan", "apply": False, "plan": config["plan"],
                "app": SURYA_APP_NAME, "draining": args.drain, "config_sha256": digest(config),
                "low_watermark": (config["low_watermark"] if args.low_watermark is None
                                  else args.low_watermark),
                "high_watermark": high,
                "steps_if_at_or_below_low_watermark": [
                    {"step": step, "vars": variables} for step, variables in surya_steps(config, high)],
            }))
            return 0
        if not args.apply:
            plan_event = {
                "event": "boinc_batch_plan", "apply": False, "plan": config["plan"],
                "source": config.get("source", "pages"),
                "draining": args.drain,
                "allocated_pages": len(config["pages"]),
                "max_pending": config["max_pending"], "config_sha256": digest(config),
                "onprem_handoff": sorted(set(args.onprem_handoff)),
                "local_lock": args.local_lock,
                "batch_size": config["batch_size"],
                "refill_cap": (config["batch_size"] if args.refill_cap is None
                               else min(config["batch_size"], args.refill_cap)),
            }
            if args.low_watermark is not None or "low_watermark" in config:
                plan_event["low_watermark"] = (args.low_watermark if args.low_watermark is not None
                                               else config["low_watermark"])
            if args.high_watermark is not None or "high_watermark" in config:
                plan_event["high_watermark"] = (args.high_watermark if args.high_watermark is not None
                                                else config["high_watermark"])
            if args.max_global_pending is not None or "max_global_pending" in config:
                plan_event["max_global_pending"] = (
                    args.max_global_pending if args.max_global_pending is not None
                    else config["max_global_pending"]
                )
            if args.other_app_budget is not None:
                plan_event["other_app_budget"] = args.other_app_budget
            if config.get("claim_languages"):
                plan_event["claim_languages"] = config["claim_languages"] + args.claim_language
            if config.get("app") == NATIVE_APP_NAME:
                plan_event["app"] = NATIVE_APP_NAME
                plan_event["native_hosts"] = config["native_hosts"]
                plan_event["pages_per_host"] = config["pages_per_host"]
                plan_event["units_per_cpu"] = config.get("units_per_cpu", 1)
            print(json.dumps(plan_event))
            return 0
        args.state_dir = args.state_dir.resolve()
        args.state_dir.mkdir(parents=True, exist_ok=True, mode=0o700)
        with (args.state_dir / "controller.lock").open("a") as lock:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
            path = args.state_dir / "state.json"
            state = json.loads(path.read_text()) if path.exists() else {"config_sha256": digest(config)}
            if surya:
                # No allocation to protect: the watermarks and caps may change. The engine
                # pins are held to the campaign by surya_tick itself.
                state["config_sha256"] = digest(config)
            require(state["config_sha256"] == digest(config),
                    "immutable allocation changed; drain/reconcile before an operator migration")
            timings = Timings(report=lambda line: print(json.dumps(line, sort_keys=True),
                                                        file=sys.stderr, flush=True))
            transport = Transport(
                config, args.state_dir,
                status_url=args.server_status_url, status_file=args.server_status_file,
                batch_submit=args.batch_submit, batch_spool_out=args.batch_spool_out,
                batch_collect=args.batch_collect, batch_export=args.batch_export,
                onprem_handoff=tuple(args.onprem_handoff), rest_journals=args.rest_journals,
                collect_parallel=args.collect_parallel, timings=timings,
                skip_truncated=args.skip_truncated_scans,
                extra_claim_languages=tuple(args.claim_language),
                step_lock_wait_secs=args.step_lock_wait_secs, local_lock=args.local_lock,
            )
            tick_kwargs = {"drain": args.drain, "dead_letter_failed": args.dead_letter_failed}
            if args.resume_native:
                tick_kwargs["resume_native"] = True
            if args.low_watermark is not None:
                tick_kwargs["low_watermark"] = args.low_watermark
            if args.high_watermark is not None:
                tick_kwargs["high_watermark"] = args.high_watermark
            if args.max_global_pending is not None:
                tick_kwargs["max_global_pending"] = args.max_global_pending
            if args.refill_cap is not None:
                tick_kwargs["refill_cap"] = args.refill_cap
            if args.other_app_budget is not None:
                tick_kwargs["other_app_budget"] = args.other_app_budget
            with transport.lock():
                # Read under the lock the archive takes too, so the other plans' archives agree
                # with the journals this tick lists.
                if args.check_state_dir:
                    tick_kwargs["sibling_archived"] = sibling_archived_held(args.state_dir, args.check_state_dir)
                    tick_kwargs["sibling_claims"] = sibling_claim_messages(args.state_dir, args.check_state_dir)
                event = tick(config, state, transport, lambda: save(path, state), **tick_kwargs)
            # Again after the lease is released, so the line counts the release too.
            event["timings"] = timings.summary()
            print(json.dumps(event, sort_keys=True))
        return 0
    except (ValueError, KeyError, TypeError, OSError, RuntimeError) as error:
        # No command output, credentials, page contents or exception chains in telemetry.
        message = str(error) if isinstance(error, (ValueError, RuntimeError)) else type(error).__name__
        failure = {"event": "boinc_batch_failure", "schema_version": 1,
                   "at": int(time.time()), "error": message}
        if timings is not None:
            # How far the tick got, and what that cost, before it failed.
            failure["timings"] = timings.summary()
        if isinstance(state, dict) and isinstance(state.get("held_since"), dict):
            # A tick that stops at a held result still says how many are held and for how long,
            # and what the last gold onboarding runs did (#901).
            now = time.time()
            held = state["held_since"]
            failure["collection_held"] = len(held)
            failure["oldest_held_seconds"] = int(max((now - at for at in held.values()), default=0))
            failure["gold_onboarding"] = state.get("gold_onboarding", {})
        print(json.dumps(failure), file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
