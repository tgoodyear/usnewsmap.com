#!/usr/bin/env python3
"""One bounded BOINC OCR tick; offline validation unless explicitly --apply."""

import argparse
import contextlib
import fcntl
import hashlib
import json
import os
import re
import shutil
import subprocess
import sys
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
MAX_GLOBAL_PENDING = 100000
DEFAULT_MAX_GLOBAL_PENDING = 1000
MAX_STATUS_BYTES = 1024 * 1024
OCR_APP_NAME = "usnm_ocr"
SUPPORTED_COHORT_APPS = (OCR_APP_NAME,)
SUPPORTED_COHORT_PLATFORMS = ("all",)
SUPPORTED_COHORT_RESOURCES = ("cpu",)
APP_NAME = re.compile(r"[A-Za-z0-9_-]{1,80}")
ZERO_WORK_LINE = re.compile(r"(?:No tasks sent|Got 0 new tasks|Sending reply to \[HOST#\d+\]: 0 results)")


class ConfigError(ValueError):
    """Invalid scheduler or cohort configuration."""


def require(ok, message):
    if not ok:
        raise ValueError(message)


COMMON = {"schema_version", "env", "account", "plan", "manifest", "image", "push_command",
          "scans_root", "manifests_root", "batch_size", "max_pending", "pages_per_host"}
PAGES_FIELDS = COMMON | {"batch", "pages"}
QUEUE_FIELDS = COMMON | {"source", "claim_queues", "lease_hours"}
OPTIONAL_FIELDS = {
    "app", "platform", "resource", "cohort", "cohorts",
    "low_watermark", "high_watermark", "max_global_pending", "server_status_url",
}
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
            f"unsupported cohort app; Transport.submit() only dispatches {OCR_APP_NAME} workunits"
        )
    return app


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
        return _nonneg_int(raw, MAX_GLOBAL_PENDING, "invalid zero_work_requests count")
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
            return _nonneg_int(int(value), MAX_GLOBAL_PENDING, f"out-of-range BOINC status <{tag}>")

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
            unsent = _nonneg_int(item["unsent"], MAX_GLOBAL_PENDING, "invalid app unsent count")
            in_progress = _nonneg_int(item["in_progress"], MAX_GLOBAL_PENDING,
                                      "invalid app in_progress count")
            apps[name] = {"unsent": unsent, "in_progress": in_progress, "runnable": unsent + in_progress}
    elif isinstance(raw_apps, dict):
        require(len(raw_apps) <= 100, "oversized BOINC app inventory")
        for name, item in raw_apps.items():
            require(isinstance(name, str) and APP_NAME.fullmatch(name) and isinstance(item, dict),
                    "invalid BOINC app entry")
            require("unsent" in item and "in_progress" in item, "missing BOINC app queue counters")
            unsent = _nonneg_int(item["unsent"], MAX_GLOBAL_PENDING, "invalid app unsent count")
            in_progress = _nonneg_int(item["in_progress"], MAX_GLOBAL_PENDING,
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
                                  if ready is None else _nonneg_int(ready)),
        "results_in_progress": (sum(a["in_progress"] for a in apps.values())
                                if in_prog is None else _nonneg_int(in_prog)),
        "zero_work_requests": zero_work,
        "cohorts": cohorts,
    }


parse_server_status_xml = parse_server_status


def load_config(path):
    config = json.loads(path.read_text())
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
    for key, maximum in (("batch_size", 100), ("max_pending", 10000), ("pages_per_host", 100)):
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
    command = config["push_command"]
    require(isinstance(command, list) and command and
            all(isinstance(arg, str) and re.fullmatch(r"[A-Za-z0-9_/@=.:+-]+", arg)
                for arg in command), "push_command must be a trusted, literal argv prefix")
    if queue:
        # BOINC is a second consumer of the work queues, beside the on-prem nodes
        # (docs/boinc-batch-scheduler.md#queue-fed-work). The page list is whatever it claims.
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


def digest(config):
    return hashlib.sha256(json.dumps(config, sort_keys=True).encode()).hexdigest()


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


class ClaimFailed(RuntimeError):
    """The claim step failed after taking messages: `claims` are the ones it did take."""

    def __init__(self, message, claims):
        super().__init__(message)
        self.claims = claims


class Transport:
    def __init__(self, config, directory, *, status_url=None, status_file=None):
        self.config = config
        self.directory = directory
        self.status_url = status_url
        self.status_file = status_file

    def run(self, argv, label, timeout=2100, ok=(0,)):
        try:
            done = subprocess.run(argv, check=False, text=True, stdout=subprocess.PIPE,
                                  stderr=subprocess.PIPE, timeout=timeout, cwd=REPO)
        except (OSError, subprocess.SubprocessError) as error:
            raise RuntimeError(f"{label} failed; inspect the execution privately before retrying") from error
        if done.returncode not in ok:
            raise RuntimeError(f"{label} failed; inspect the execution privately before retrying")
        return done.stdout

    def az(self, *args):
        return self.run(["az", "storage", "blob", *args, "--auth-mode", "login",
                         "--account-name", self.config["account"], "--container-name",
                         "boinc-handoff", "--only-show-errors"], "hand-off storage")

    def blob(self, name):
        path = self.directory / "read.json"
        self.az("download", "--name", name, "--file", str(path), "--overwrite", "true", "-o", "none")
        require(path.stat().st_size <= 16 * 1024 * 1024, "hand-off JSON exceeds limit")
        return json.loads(path.read_text())

    @contextlib.contextmanager
    def lock(self):
        name = "scheduler/controller.lock"
        if self.az("exists", "--name", name, "--query", "exists", "-o", "tsv").strip() != "true":
            marker = self.directory / "lease-marker.json"
            marker.write_text('{"schema_version":1}\n')
            self.az("upload", "--name", name, "--file", str(marker), "--overwrite", "false", "-o", "none")
        lease = uuid.uuid4().hex
        lease = str(uuid.UUID(lease))
        self.az("lease", "acquire", "--blob-name", name, "--lease-duration", "-1",
                "--proposed-lease-id", lease, "-o", "none")
        try:
            yield
        finally:
            self.az("lease", "release", "--blob-name", name, "--lease-id", lease, "-o", "none")

    def step(self, target, step, **variables):
        argv = ["bash", str(REPO / "scripts/start-boinc-bridge.sh"), "--env", self.config["env"],
                "--target", target, "--step", step, "--wait-mins", "30"]
        for key, value in variables.items():
            argv += ["--var", f"{key}={value}"]
        self.run(argv, f"{target}/{step}")

    def inventory(self):
        nonce = uuid.uuid4().hex
        self.step("bridge", "batch-inventory", BATCH_NONCE=nonce,
                  HANDOFF_URL=f"https://{self.config['account']}.blob.core.windows.net/boinc-handoff")
        result = self.blob("scheduler/inventory.json")
        require(result.get("nonce") == nonce, "stale/concurrent inventory; another controller may be active")
        return result

    def records(self):
        listing = json.loads(self.az("list", "--prefix", "dispatch/jobs/", "--num-results", "*",
                                     "--query", "[].name", "-o", "json"))
        names = [name for name in listing if re.fullmatch(r"dispatch/jobs/[0-9a-f]{64}/record.json", name)]
        require(len(names) <= MAX_JOBS and len(set(names)) == len(names),
                "dispatch inventory limit/consistency failure; reconcile drained journals")
        cache = self.directory / "journal-cache"
        if cache.exists():
            shutil.rmtree(cache)
        cache.mkdir(mode=0o700)
        if names:
            pattern = "dispatch/jobs/" + "[0-9a-f]" * 64 + "/record.json"
            self.run([
                "az", "storage", "blob", "download-batch", "--source", "boinc-handoff",
                "--destination", str(cache), "--pattern", pattern, "--overwrite", "true",
                "--account-name", self.config["account"], "--auth-mode", "login",
                "--only-show-errors", "-o", "none",
            ], "batch journal inventory")
        actual = {str(path.relative_to(cache)) for path in cache.rglob("*") if path.is_file()}
        require(actual == set(names), "journal inventory changed during download; retry under the owner lock")
        records = []
        for name in sorted(names):
            path = cache / name
            require(not path.is_symlink() and path.stat().st_size <= 131072,
                    "invalid/oversized journal metadata")
            record = json.loads(path.read_text())
            require(record["job"]["id"] == name.split("/")[2], "journal filename/identity mismatch")
            records.append(record)
        return records

    def push_args(self):
        return self.config["push_command"] + [
            "--account", self.config["account"], "--scans-root", self.config["scans_root"],
            "--manifests-root", self.config["manifests_root"],
            "--manifest", self.config["manifests_root"].rstrip("/") + "/" + self.config["manifest"],
        ]

    def export(self, pages, job_ids=()):
        queue = self.config.get("source") == "queue"
        if not queue:
            argv = self.push_args()
            for page in pages:
                argv += ["--scan", page["scan"]]
            self.run(argv, "cached scan push")
        # The exporter takes one base batch at a time; queued pages come from many.
        batches = {}
        for page in pages:
            batches.setdefault(page.get("batch") or self.config["batch"], []).append(page)
        for batch, group in sorted(batches.items()):
            variables = dict(
                HANDOFF_PAGES=",".join(page["page"] for page in group),
                HANDOFF_BATCH=batch, HANDOFF_PLAN=self.config["plan"],
                HANDOFF_MANIFEST=self.config["manifest"], ENGINE_IMAGE=self.config["image"],
                HANDOFF_SCANS=",".join(page["scan"][len(self.config["scans_root"].rstrip("/")) + 1:]
                                       for page in group),
                HANDOFF_JOBS=",".join(job_ids))
            if queue:
                variables["HANDOFF_CLAIMS"] = "1"
            self.step("handoff", "export", **variables)

    def claim(self, count):
        """Take up to `count` usable pages off the work queues (the hand-off job's claim step)."""
        nonce = uuid.uuid4().hex
        failure = None
        try:
            self.step("handoff", "claim", HANDOFF_MANIFEST=self.config["manifest"], CLAIM_NONCE=nonce,
                      CLAIM_MAX=count, CLAIM_QUEUES=",".join(self.config["claim_queues"]),
                      CLAIM_LEASE_HOURS=self.config["lease_hours"],
                      **({"CLAIM_LANGUAGES": ",".join(self.config["claim_languages"])}
                         if self.config.get("claim_languages") else {}))
        except RuntimeError as error:
            # Messages taken before the failure are hidden now and their claims are written:
            # record them if the job got as far as its summary, then stop the tick.
            failure = error
        try:
            summary = self.blob("scheduler/claims.json")
            require(summary.get("schema_version") == 1 and summary.get("nonce") == nonce,
                    "stale/concurrent claim summary; another controller may be active")
        except (RuntimeError, ValueError):
            if failure:
                raise failure
            raise
        if failure:
            raise ClaimFailed(str(failure), summary["claims"]) from failure
        return summary["claims"]

    def push_pages(self, bases):
        """Push the cached scans of claimed pages, finding each one's format on the scan server.
        Returns ({base: path under scans/}, [bases with no cached scan])."""
        argv = self.push_args()
        for base in bases:
            # The script names a page by where the fetcher files it under keep/, without "keep/".
            require(base.startswith("keep/"), "claimed scan is not under keep/")
            argv += ["--keep-page", base[len("keep/"):]]
        output = self.run(argv, "cached scan push", ok=(0, 3))
        pushed, missing = {}, []
        for line in output.splitlines():
            kind, _, value = line.partition(" ")
            if kind == "pushed":
                pushed[value] = value
            elif kind == "missing":
                missing.append("keep/" + value)
        found = {}
        for base in bases:
            match = [rel for rel in pushed if rel.startswith(f"{base}.")]
            if match:
                found[base] = match[0]
        require(set(found) | set(missing) == set(bases) and not (set(found) & set(missing)),
                "scan push did not account for every claimed page")
        return found, missing

    def release(self, doc_ids):
        self.step("handoff", "release", HANDOFF_DOCS=",".join(doc_ids))

    def submit(self, job_id):
        self.step("bridge", "submit-remote", JOB_ID=job_id,
                  HANDOFF_URL=f"https://{self.config['account']}.blob.core.windows.net/boinc-handoff")

    def collect(self, job_id, spool):
        self.step("bridge", "spool-out", JOB_ID=job_id, HANDOFF_SPOOL=spool,
                  HANDOFF_URL=f"https://{self.config['account']}.blob.core.windows.net/boinc-handoff")
        self.step("handoff", "collect", JOB_ID=job_id)

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


def owned_records(config, records):
    # Each allocated page's base batch: one for the whole cohort, or the claimed page's own.
    wanted = {page["page"]: page.get("batch") or config["batch"] for page in config["pages"]}
    owned = {}
    for record in records:
        require(record["stage"] in STAGES and ID.fullmatch(record["job"]["id"]),
                "invalid dispatch journal")
        job = record["job"]
        page = job["message"]["page"]
        if record["plan"] != config["plan"]:
            require(page not in wanted,
                    "page is already allocated to another plan; reconcile the allocation before export")
            continue
        require(wanted.get(page) == job["message"]["batch"]
                and job["engine"]["image"] == config["image"],
                "plan already exists with a different allocation/image; operator migration required")
        require(page not in owned, "multiple jobs for one allocated page; reconcile before feeding")
        owned[page] = record
    return owned


BATCH = re.compile(r"[a-z]{2,10}_[A-Za-z0-9_]+")


class QueueFeed:
    """Pages claimed off the work queues, kept in the controller's state.

    The hand-off job's `claim` step takes usable messages off `ocr-bulk` (and `ocr-priority`
    when asked), as a second consumer beside the on-prem nodes. Each claimed page is recorded
    here *before* anything else is done for it, because its message is hidden from the nodes
    for the lease and only a later `collect` deletes it. A page whose scan turns out not to be
    on the scan server goes back to the queue. The record also stays after the page is
    published: its journal belongs to this plan, and `owned_records` refuses a journal for a
    page the plan doesn't know."""

    def __init__(self, config, state, transport, persist):
        self.config = config
        self.transport = transport
        self.persist = persist
        self.claims = state.setdefault("claims", {})
        require(len(self.claims) <= MAX_JOBS, "too many claimed pages; archive drained history first")

    def restore(self):
        self.config["pages"] = [
            {"page": page, "batch": claim["batch"], "scan": claim["scan"]}
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

    def fill(self, count, selected):
        """Make `count` more pages ready: claim them (less any already claimed but not yet
        started), push their scans and give back the ones with none."""
        self.retry_releases()
        started = set(selected)
        waiting = [claim for page, claim in self.claims.items() if page not in started]
        need = max(0, count - len(waiting))
        if need:
            try:
                fresh = self.transport.claim(need)
            except ClaimFailed as error:
                # Their messages are hidden now: keep the claims that were made, then stop.
                self.record(error.claims, need)
                raise
            self.record(fresh, need)
        unpushed = {page: claim for page, claim in self.claims.items()
                    if claim.get("scan") is None and not claim.get("release")}
        if unpushed:
            found, missing = self.transport.push_pages(
                sorted(claim["scan_base"] for claim in unpushed.values()))
            root = self.config["scans_root"].rstrip("/")
            for page, claim in unpushed.items():
                if claim["scan_base"] in found:
                    claim["scan"] = f"{root}/{found[claim['scan_base']]}"
                else:
                    claim["release"] = True
            self.persist()
        self.retry_releases()
        self.restore()

    def record(self, fresh, need):
        require(isinstance(fresh, list) and len(fresh) <= need, "claimed more pages than asked")
        for item in fresh:
            page, claim = self.checked(item)
            if page in self.claims:
                # Its lease ran out while the job was pending and the same message came again,
                # now hidden for a fresh lease: the existing claim, its scan and its job stand.
                # A different message for the page is two attempts at once: stop.
                require(claim["message"] is not None and claim["message"] == self.claims[page].get("message"),
                        "claimed a page already in this plan with a different message")
                continue
            self.claims[page] = claim
        self.persist()

    def retry_releases(self):
        pages = sorted(page for page, claim in self.claims.items() if claim.get("release"))
        if pages:
            self.transport.release([self.claims[page]["doc_id"] for page in pages])
            for page in pages:
                del self.claims[page]
            self.persist()


def _select_cohort_entry(mapping, keys, *, allow_single_default=False):
    for key in keys:
        if key in mapping:
            return key, mapping[key]
    if allow_single_default and len(mapping) == 1:
        return next(iter(mapping.items()))
    return None, {}


def resolve_watermarks(config, snapshot, status=None, *,
                       low_watermark=None, high_watermark=None, max_global_pending=None):
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
    _nonneg_int(active_hosts, MAX_GLOBAL_PENDING, "invalid cohort active_hosts")

    explicit_high = (high_watermark if high_watermark is not None
                     else cohort_cfg.get("high_watermark", config.get("high_watermark")))
    explicit_low = (low_watermark if low_watermark is not None
                    else cohort_cfg.get("low_watermark", config.get("low_watermark")))
    explicit_global = (max_global_pending if max_global_pending is not None
                       else config.get("max_global_pending"))
    cohort_mode = (status is not None or explicit_high is not None or explicit_low is not None or
                   explicit_global is not None or "cohorts" in config or "cohort" in config)

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
        if explicit_high is not None:
            high_target = explicit_high
        else:
            high_target = min(config["max_pending"], active_hosts * pages_per_host)
        if explicit_low is not None:
            low_target = min(explicit_low, high_target)
        elif cohort_mode:
            low_target = max(0, high_target // 2)
        else:
            low_target = max(0, high_target - 1)

    if explicit_global is not None:
        global_cap = explicit_global
    elif cohort_mode:
        global_cap = max(DEFAULT_MAX_GLOBAL_PENDING, high_target, config["max_pending"])
    else:
        global_cap = high_target

    return {
        "app": app,
        "platform": platform,
        "resource": resource,
        "cohort": cohort_name,
        "active_hosts": active_hosts,
        "low_watermark": low_target,
        "high_watermark": high_target,
        "max_global_pending": global_cap,
        "cohort_mode": cohort_mode,
    }


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


def tick(config, state, transport, persist, now=None, *, drain=False,
         low_watermark=None, high_watermark=None, max_global_pending=None, server_status=None):
    feed = QueueFeed(config, state, transport, persist) if config.get("source") == "queue" else None
    if feed:
        feed.restore()
    snapshot = transport.inventory()
    now = time.time() if now is None else now
    require(snapshot["schema_version"] == 1 and -60 <= now - snapshot["created_at"] <= 3600,
            "inventory is not fresh")
    for key in ("pending_workunits", "active_hosts"):
        require(type(snapshot[key]) is int and snapshot[key] >= 0, "invalid capacity inventory")
    pending_jobs = snapshot["pending_jobs"]
    require(isinstance(pending_jobs, list) and len(pending_jobs) <= MAX_JOBS and
            all(isinstance(job_id, str) and ID.fullmatch(job_id) for job_id in pending_jobs) and
            len(set(pending_jobs)) == len(pending_jobs), "invalid pending job inventory")
    pending_jobs = set(pending_jobs)
    existing_jobs = snapshot["existing_jobs"]
    require(isinstance(existing_jobs, list) and len(existing_jobs) <= MAX_JOBS and
            all(isinstance(job_id, str) and ID.fullmatch(job_id) for job_id in existing_jobs) and
            len(set(existing_jobs)) == len(existing_jobs) and pending_jobs <= set(existing_jobs),
            "invalid existing workunit inventory")
    existing_jobs = set(existing_jobs)
    server = snapshot["jobs"]
    require(isinstance(server, dict) and len(server) <= MAX_JOBS, "invalid server job inventory")
    for job_id, job in server.items():
        require(ID.fullmatch(job_id) and isinstance(job, dict) and job.get("stage") in STAGES
                and (job.get("spool") is None or isinstance(job["spool"], str)),
                "invalid server job record")
    records = transport.records()
    owned = owned_records(config, records)
    for record in owned.values():
        job_id = record["job"]["id"]
        require(server.get(job_id, {}).get("stage") != "submitting",
                "uncertain BOINC submission; reconcile explicitly, never automatically resubmit")
        require(record["stage"] in ("published", "failed") or job_id not in existing_jobs or
                server.get(job_id, {}).get("stage") == "submitted",
                "SQL/journal mismatch; reconcile existing work before any automatic submission")
    selected = state.setdefault("selected", {})
    for page in owned:
        selected.setdefault(page, now)
    persist()
    count = 0
    for page, record in owned.items():
        job_id = record["job"]["id"]
        remote = server.get(job_id)
        require(not remote or remote["stage"] != "submitting",
                "uncertain BOINC submission; reconcile explicitly, never automatically resubmit")
        require(record["stage"] != "failed", "terminal failed page; operator investigation required")
        if record["stage"] != "published" and remote and remote["spool"] and count < config["batch_size"]:
            spool = remote["spool"]
            require(re.fullmatch(r"/[A-Za-z0-9_./-]+/results/[A-Za-z0-9_./-]+", spool)
                    and ".." not in spool, "invalid assimilator spool")
            transport.collect(job_id, spool)
            count += 1
    if count:
        records = transport.records()
        owned = owned_records(config, records)
    require(not any(record["stage"] == "failed" for record in owned.values()),
            "terminal BOINC failure; no replacement workload will be manufactured")
    status = _extract_status(snapshot, transport, server_status=server_status, drain=drain)
    cohort_info = resolve_watermarks(
        config, snapshot, status,
        low_watermark=low_watermark, high_watermark=high_watermark,
        max_global_pending=max_global_pending,
    )
    ocr_app = cohort_info["app"]
    active_hosts = cohort_info["active_hosts"]
    low_target = cohort_info["low_watermark"]
    high_target = cohort_info["high_watermark"]
    global_cap = cohort_info["max_global_pending"]
    cohort_mode = cohort_info["cohort_mode"]
    target = high_target

    uncollected_not_pending = sum(
        record["stage"] not in ("published", "failed") and record["job"]["id"] not in pending_jobs
        for record in records
    )
    missing = [page for page in config["pages"] if page["page"] in selected and page["page"] not in owned]
    global_in_flight = max(snapshot["pending_workunits"], len(pending_jobs))
    has_ocr_attribution = status is not None and ocr_app in status["apps"]
    if has_ocr_attribution:
        app_counts = status["apps"][ocr_app]
        ocr_unsent = app_counts["unsent"]
        ocr_in_progress = app_counts["in_progress"]
        ocr_status_results = app_counts["runnable"]
        other_status_results = sum(
            item["runnable"] for name, item in status["apps"].items() if name != ocr_app
        )
        ocr_in_flight = min(global_in_flight, max(len(pending_jobs), ocr_status_results))
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
    cohort_pending = ocr_in_flight + uncollected_not_pending + len(missing)
    global_pending = global_in_flight + uncollected_not_pending + len(missing)
    cohort_pending_results = max(len(pending_jobs), ocr_status_results) + uncollected_not_pending + len(missing)
    other_app_pending_results = other_status_results
    global_pending_results = cohort_pending_results + other_app_pending_results
    pending = cohort_pending if cohort_mode else global_pending

    uncertain = any(job["stage"] == "submitting" for job in server.values())
    require(not uncertain, "uncertain server submission blocks fresh allocation")
    held = sum(record["stage"] != "published" and
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
    global_free = max(0, global_cap - global_pending)
    free = min(cohort_free, global_free)
    submitted = 0
    if target and not held and not drain:
        if free > 0:
            if feed:
                feed.fill(min(config["batch_size"], free), selected)
            fresh = [page for page in config["pages"] if page["page"] not in selected][
                :min(config["batch_size"], free)]
            for page in fresh:
                selected[page["page"]] = now
            persist()
        export = [page for page in config["pages"] if page["page"] in selected and
                  (page["page"] not in owned or owned[page["page"]]["stage"] == "prepared")]
        if export:
            export = export[:config["batch_size"]]
            pages = {page["page"] for page in export}
            journals = [record["job"]["id"] for record in records
                        if record["job"]["message"]["page"] in pages]
            require(len(journals) <= 100, "too many existing journals for selected pages; reconcile history")
            transport.export(export, journals)
            records = transport.records()
            owned = owned_records(config, records)
        candidates = {record["job"]["id"] for record in owned.values()
                      if record["stage"] == "reserved" and record["job"]["id"] not in existing_jobs
                      and server.get(record["job"]["id"], {}).get("stage") in (None, "reserved")}
        backlog = sum(record["stage"] not in ("published", "failed") and
                      record["job"]["id"] not in pending_jobs | candidates for record in records)
        backlog += sum(page not in owned for page in selected)
        cohort_submit_room = max(0, target - ocr_in_flight - backlog)
        global_submit_room = max(0, global_cap - global_in_flight - backlog)
        submit_limit = min(config["batch_size"], cohort_submit_room, global_submit_room)
        for record in owned.values():
            job_id = record["job"]["id"]
            if record["stage"] == "reserved" and \
                    server.get(job_id, {}).get("stage") in (None, "reserved") and \
                    submitted < submit_limit:
                require(job_id not in existing_jobs,
                        "SQL/journal mismatch; reconcile existing work before any automatic submission")
                transport.submit(job_id)
                submitted += 1

    post_uncollected_not_pending = sum(
        record["stage"] not in ("published", "failed") and record["job"]["id"] not in pending_jobs
        for record in records
    )
    post_missing = sum(page not in owned for page in selected)
    post_cohort_pending = ocr_in_flight + post_uncollected_not_pending + post_missing
    if high_target == 0 or post_cohort_pending >= high_target:
        state["refilling"] = False
    else:
        state["refilling"] = refilling

    if drain:
        backpressure = True
        backpressure_reason = "draining"
    elif held > 0:
        backpressure = True
        backpressure_reason = "collection_backlog"
    elif target == 0:
        backpressure = True
        backpressure_reason = "no_active_hosts"
    elif cohort_mode and global_pending >= global_cap:
        backpressure = True
        backpressure_reason = "global_safety_cap"
    elif pending >= target:
        backpressure = True
        backpressure_reason = "cohort_high_watermark"
    elif not refilling:
        backpressure = True
        backpressure_reason = "cohort_hysteresis"
    elif global_pending >= global_cap:
        backpressure = True
        backpressure_reason = "global_safety_cap"
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

    published = sum(record["stage"] == "published" for record in owned.values())
    previous = state.get("published", 0)
    started = state.setdefault("started_at", now)
    state["published"] = published
    persist()
    return {
        "event": "boinc_batch_tick", "schema_version": 1, "at": int(now), "plan": config["plan"],
        "draining": drain,
        "active_hosts": active_hosts, "target_pending": target,
        "observed_pending": pending, "selected": len(selected), "published": published,
        "collection_backlog": held,
        "newly_published": max(0, published - previous),
        "pages_per_hour": round(published * 3600 / max(1, now - started), 3),
        "oldest_pending_seconds": int(max((now - at for page, at in selected.items()
                                          if page not in owned or owned[page]["stage"] != "published"),
                                         default=0)),
        "backpressure": backpressure,
        "backpressure_reason": backpressure_reason,
        "low_watermark": low_target,
        "high_watermark": high_target,
        "cohort_pending": cohort_pending,
        "other_app_pending": other_pending,
        "global_pending": global_pending,
        "cohort_pending_results": cohort_pending_results,
        "other_app_pending_results": other_app_pending_results,
        "global_pending_results": global_pending_results,
        "max_global_pending": global_cap,
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
    parser.add_argument("--server-status-url", default=None,
                        help="optional https server_status.php?xml=1 URL for per-app queue counts")
    parser.add_argument("--server-status-file", type=Path, default=None,
                        help="optional local file with server_status.php?xml=1 or structured status")
    args = parser.parse_args(argv)
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
        if args.server_status_url is not None:
            _validate_status_url(args.server_status_url)
        if not args.apply:
            plan_event = {
                "event": "boinc_batch_plan", "apply": False, "plan": config["plan"],
                "source": config.get("source", "pages"),
                "draining": args.drain,
                "allocated_pages": len(config["pages"]),
                "max_pending": config["max_pending"], "config_sha256": digest(config),
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
            print(json.dumps(plan_event))
            return 0
        args.state_dir = args.state_dir.resolve()
        args.state_dir.mkdir(parents=True, exist_ok=True, mode=0o700)
        with (args.state_dir / "controller.lock").open("a") as lock:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
            path = args.state_dir / "state.json"
            state = json.loads(path.read_text()) if path.exists() else {"config_sha256": digest(config)}
            require(state["config_sha256"] == digest(config),
                    "immutable allocation changed; drain/reconcile before an operator migration")
            transport = Transport(
                config, args.state_dir,
                status_url=args.server_status_url, status_file=args.server_status_file,
            )
            tick_kwargs = {"drain": args.drain}
            if args.low_watermark is not None:
                tick_kwargs["low_watermark"] = args.low_watermark
            if args.high_watermark is not None:
                tick_kwargs["high_watermark"] = args.high_watermark
            if args.max_global_pending is not None:
                tick_kwargs["max_global_pending"] = args.max_global_pending
            with transport.lock():
                event = tick(config, state, transport, lambda: save(path, state), **tick_kwargs)
            print(json.dumps(event, sort_keys=True))
        return 0
    except (ValueError, KeyError, TypeError, OSError, RuntimeError) as error:
        # No command output, credentials, page contents or exception chains in telemetry.
        message = str(error) if isinstance(error, (ValueError, RuntimeError)) else type(error).__name__
        print(json.dumps({"event": "boinc_batch_failure", "schema_version": 1,
                          "at": int(time.time()), "error": message}), file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
