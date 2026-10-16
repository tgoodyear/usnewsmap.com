#!/usr/bin/env python3
"""Rebuildable, local-only metadata catalog; canonical bulk data stays in Blob/Parquet."""

import argparse
import datetime as dt
import hashlib
import http.server
import json
import math
import os
from pathlib import Path
import re
import sqlite3
import stat
import sys
import time
import urllib.parse
import uuid


MAX_BATCH = 16 * 1024 * 1024
MAX_LINE = 64 * 1024
MAX_EVENTS = 10000
MAX_RESULT = 1024 * 1024
MAX_LIMIT = 100
MAX_AGGREGATES = 24  # Eight built-in stages also fit dashboard coverage's 32-entry cap.
QUERY_SECONDS = 2
TOKEN = re.compile(r"[A-Za-z0-9][A-Za-z0-9_.:/@+-]{0,255}\Z")
SHA = re.compile(r"[0-9a-f]{64}\Z")
STAGES = ("queued", "allocated", "outcome", "accepted", "imported", "indexed")

# RunRecord v1 in crates/usnm-ocr-contract/src/runs.rs, not an engine-run replacement.
RUN_STR = set("""run_id doc_id node worker_version ocr_engine engine_id family
engine_version adapter image_ref image_digest models_digest models_json profile
options_json route_id status models_loaded image_sha256 dpi_source granularity
markup engine_stats_json language reason""".split())
RUN_OPT_STR = set("routing_digest selection error_kind reading_order text_sha256 script".split())
RUN_UINT = set("""contract tiles routing_version candidate_rank instance_restarts
image_width image_height dpi""".split())
RUN_OPT_UINT = set("blocks lines words text_chars".split())
RUN_FLOAT = {"scale", "seconds_wall"}
RUN_OPT_FLOAT = set("""seconds_cpu peak_mem_mb instance_peak_mem_mb model_load_seconds
mean_conf damage_before damage_after""".split())
RUN_BOOL = {"selected", "oom", "cold_start"}
RUN_FIELDS = (RUN_STR | RUN_OPT_STR | RUN_UINT | RUN_OPT_UINT | RUN_FLOAT |
              RUN_OPT_FLOAT | RUN_BOOL | {"queued_at", "node_finished_at"})


class CatalogError(ValueError):
    """An invalid evidence batch, query, or local catalog."""


def require(condition, message):
    if not condition:
        raise CatalogError(message)


def canonical(value):
    return json.dumps(value, sort_keys=True, separators=(",", ":"), allow_nan=False)


def digest(value):
    return hashlib.sha256(canonical(value).encode()).hexdigest()


def obj(value, fields):
    require(isinstance(value, dict) and set(value) == set(fields), "unexpected object fields")


def token(value):
    require(isinstance(value, str) and TOKEN.fullmatch(value) is not None,
            "invalid metadata identifier (max 256 safe characters)")
    require("://" not in value and not any(x in value.lower() for x in
            ("password", "secret", "bearer", "credential", "sig=", "token=")),
            "credential-bearing metadata is forbidden")
    return value


def short_text(value, limit):
    require(isinstance(value, str) and len(value) <= limit and
            not any(ord(c) < 32 for c in value), "invalid short metadata string")
    require(not re.search(r"https?://|[?&](?:sig|token|key)=|bearer |password|secret|credential",
                          value, re.IGNORECASE), "sensitive metadata forbidden")
    return value


def sha(value, prefixed=False):
    if prefixed and isinstance(value, str) and value.startswith("sha256:"):
        value = value[7:]
    require(isinstance(value, str) and SHA.fullmatch(value) is not None,
            "expected full lowercase SHA-256")
    return value


def timestamp(value):
    require(isinstance(value, str) and len(value) <= 40 and
            re.fullmatch(r"\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d(?:\.\d{1,9})?(?:Z|[+-]\d\d:\d\d)", value),
            "expected RFC3339 timestamp with timezone")
    try:
        parsed = dt.datetime.fromisoformat(value.replace("Z", "+00:00"))
        fraction = re.search(r"\.(\d+)", value)
        nanos_tail = (fraction[1].ljust(9, "0")[6:] if fraction else "000")
        utc = parsed.astimezone(dt.timezone.utc).isoformat(timespec="microseconds").replace("+00:00", "Z")
        return utc[:-1] + nanos_tail + "Z"
    except ValueError as exc:
        raise CatalogError("invalid timestamp") from exc


def now():
    return dt.datetime.now(dt.timezone.utc).isoformat(timespec="microseconds").replace("+00:00", "Z")


def safe_reference(value):
    # Only explicit storage coordinates, never URLs, SAS queries, userinfo, or local paths.
    require(isinstance(value, str) and len(value) <= 512 and
            re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9._/-]*", value) is not None and
            all(p not in ("", ".", "..") for p in value.split("/")),
            "reference must be a credential-free relative object key (max 512)")
    require(not any(x in value.lower() for x in ("secret", "password", "credential")),
            "sensitive reference forbidden")
    return value


def load_json(raw):
    def pairs(items):
        result = {}
        for key, value in items:
            require(key not in result, "duplicate JSON key")
            result[key] = value
        return result

    def finite_float(value):
        result = float(value)
        require(math.isfinite(result), "nonfinite JSON number")
        return result

    try:
        return json.loads(raw, object_pairs_hook=pairs,
                          parse_float=finite_float,
                          parse_constant=lambda _: (_ for _ in ()).throw(CatalogError("nonfinite JSON")))
    except (ValueError, RecursionError) as exc:
        raise CatalogError("invalid JSON") from exc


def adapt_run(record, page_id):
    """Accept actual to_json_line RunRecord v1; persist only an allowlisted projection."""
    require(isinstance(record, dict), "run_record must be an object")
    record = dict(record)
    schema = record.pop("schema", None)
    require(type(schema) is int and schema == 1 and
            set(record) == RUN_FIELDS, "expected exact RunRecord JSON fields plus schema")
    for key in RUN_STR | RUN_OPT_STR:
        value = record[key]
        if value is None and key in RUN_OPT_STR:
            continue
        require(isinstance(value, str) and len(value) <= 8192 and
                not any(ord(c) < 32 for c in value), "invalid RunRecord string")
    for key in RUN_UINT | RUN_OPT_UINT:
        value = record[key]
        require((value is None and key in RUN_OPT_UINT) or
                (type(value) is int and 0 <= value <= 4294967295), "invalid RunRecord uint32")
    for key in RUN_FLOAT | RUN_OPT_FLOAT:
        value = record[key]
        if value is None and key in RUN_OPT_FLOAT:
            continue
        require(type(value) in (int, float) and math.isfinite(value) and value >= 0,
                "invalid RunRecord measurement")
        if key in ("mean_conf", "damage_before", "damage_after"):
            require(value <= 1, "RunRecord ratio outside [0,1]")
    for key in RUN_BOOL:
        require(type(record[key]) is bool, "invalid RunRecord boolean")
    require(record["scale"] > 0, "RunRecord scale must be positive")
    timestamp(record["node_finished_at"])
    if record["queued_at"] is not None:
        timestamp(record["queued_at"])
    require(page_id.split(":", 1)[1] == record["doc_id"], "page_id does not match RunRecord doc_id")
    for key in ("models_json", "options_json", "engine_stats_json"):
        require(isinstance(load_json(record[key]), dict), "RunRecord JSON field must be an object")
    for key in ("image_digest", "models_digest", "routing_digest"):
        if record[key] is not None:
            if key in ("image_digest", "models_digest") and record[key] == "local":
                continue  # Existing RunRecord convention: digest unavailable for local adapters.
            require(record[key].startswith("sha256:"), "expected sha256-prefixed digest")
            sha(record[key], prefixed=True)
    for key in ("image_sha256", "text_sha256"):
        if record[key] is not None:
            sha(record[key])
    for key in ("run_id", "engine_id", "engine_version", "family", "profile",
                "route_id", "status", "language"):
        token(record[key])
    require(not record["selected"] or record["status"] == "ok", "selected read must be ok")
    config = {key: record[key] for key in (
        "engine_id", "engine_version", "image_digest", "models_digest", "models_json",
        "profile", "options_json", "scale", "tiles", "route_id", "routing_version",
        "routing_digest", "worker_version", "contract")}
    result = {key: record[key] for key in (
        "run_id", "engine_id", "engine_version", "family", "profile", "route_id",
        "status", "selected", "candidate_rank", "image_sha256", "image_digest",
        "models_digest", "language", "seconds_wall", "seconds_cpu", "peak_mem_mb",
        "oom", "mean_conf", "damage_before", "damage_after")}
    result.update(config_sha256=digest(config),
                  node_finished_at=timestamp(record["node_finished_at"]))
    return result


WORKER_FIELDS = set("""metadata_schema run_id doc_id node worker_version ocr_source language
batch reason damage_before queued_at dpi dpi_source execution_config worker_config
started_at finished_at seconds_wall status error_kind observations run_record""".split())
OBS_FIELDS = set("""image_sha256 image_format engine_started_at engine_finished_at
seconds_wall status error_kind ocr_engine info_source info""".split())
INFO_FIELDS = set("""engine_id adapter image_ref image_digest models_digest models_json
options_json models_loaded cold_start model_load_seconds markup engine_stats_json""".split())


def measurement(value):
    require(value is None or (type(value) in (int, float) and math.isfinite(value) and value >= 0),
            "invalid execution measurement")
    return value


def adapt_worker(metadata, page_id):
    """Worker metadata_schema=1, including failed attempts without a RunRecord."""
    obj(metadata, WORKER_FIELDS)
    require(type(metadata["metadata_schema"]) is int and metadata["metadata_schema"] == 1,
            "unsupported worker metadata_schema")
    for key in ("run_id", "doc_id", "ocr_source", "language", "batch", "reason", "dpi_source"):
        token(metadata[key])
    for key in ("node", "worker_version"):
        short_text(metadata[key], 256)
    require(page_id == "loc:" + metadata["doc_id"], "worker page_id must be loc:<doc_id>")
    require(metadata["status"] in ("ok", "failed", "abandoned"), "worker attempt must be terminal")
    if metadata["error_kind"] is not None:
        token(metadata["error_kind"])
    require(type(metadata["dpi"]) is int and 0 <= metadata["dpi"] <= 4294967295, "invalid worker dpi")
    measurement(metadata["damage_before"])
    require(metadata["damage_before"] is None or metadata["damage_before"] <= 1,
            "worker damage outside [0,1]")
    started, finished = timestamp(metadata["started_at"]), timestamp(metadata["finished_at"])
    require(started <= finished, "worker finish precedes start")
    if metadata["queued_at"] is not None:
        timestamp(metadata["queued_at"])
    measurement(metadata["seconds_wall"])
    require(metadata["execution_config"] is None or isinstance(metadata["execution_config"], dict),
            "invalid execution config")
    require(isinstance(metadata["worker_config"], dict), "invalid worker config")
    require(isinstance(metadata["observations"], list) and len(metadata["observations"]) <= 2,
            "worker observations must contain at most two reads")
    observations = []
    for entry in metadata["observations"]:
        obj(entry, OBS_FIELDS)
        sha(entry["image_sha256"])
        token(entry["image_format"])
        token(entry["status"])
        if entry["error_kind"] is not None:
            token(entry["error_kind"])
        for key in ("engine_started_at", "engine_finished_at"):
            if entry[key] is not None:
                timestamp(entry[key])
        measurement(entry["seconds_wall"])
        require(entry["info_source"] in (None, "planned", "instance", "result"),
                "invalid engine info provenance")
        if entry["ocr_engine"] is not None:
            require(isinstance(entry["ocr_engine"], str) and len(entry["ocr_engine"]) <= 8192,
                    "invalid discarded engine tag")
        projection = {key: entry[key] for key in OBS_FIELDS - {"info", "ocr_engine"}}
        projection["engine_id"] = None
        if entry["info"] is not None:
            info = entry["info"]
            obj(info, INFO_FIELDS)
            token(info["engine_id"])
            projection["engine_id"] = info["engine_id"]
            for key in INFO_FIELDS - {"cold_start", "model_load_seconds"}:
                require(isinstance(info[key], str) and len(info[key]) <= 8192 and
                        not any(ord(ch) < 32 for ch in info[key]), "invalid engine info string")
            require(type(info["cold_start"]) is bool, "invalid engine cold_start")
            measurement(info["model_load_seconds"])
            for key in ("models_json", "options_json", "engine_stats_json"):
                require(isinstance(load_json(info[key]), dict), "invalid engine info JSON")
            for key in ("image_digest", "models_digest"):
                if info[key] != "local":
                    require(isinstance(info[key], str) and info[key].startswith("sha256:"),
                            "invalid engine digest")
                    sha(info[key], prefixed=True)
                projection[key] = info[key]
            projection["config_sha256"] = digest({key: value for key, value in info.items()
                                                   if key not in ("cold_start", "model_load_seconds",
                                                                  "engine_stats_json")})
        observations.append(projection)
    if metadata["run_record"] is not None:
        run = adapt_run(metadata["run_record"], page_id)
        require(run["run_id"] == metadata["run_id"], "worker/RunRecord run_id mismatch")
    return {"run_id": metadata["run_id"], "status": metadata["status"],
            "error_kind": metadata["error_kind"], "language": metadata["language"],
            "started_at": started, "finished_at": finished,
            "seconds_wall": metadata["seconds_wall"], "observations": observations,
            "engine_id": next((entry["engine_id"] for entry in reversed(observations)
                               if entry["engine_id"] is not None), None),
            "config_sha256": digest([metadata["execution_config"], metadata["worker_config"]]),
            "has_run_record": metadata["run_record"] is not None}


def validate_event(event):
    obj(event, ("schema_version", "batch_id", "event_id", "kind", "source",
                "observed_at", "page_id", "payload"))
    require(type(event["schema_version"]) is int and event["schema_version"] == 1,
            "unsupported evidence schema_version")
    for key in ("batch_id", "event_id", "source"):
        token(event[key])
    if event["kind"] == "stage":
        require(event["page_id"] is None, "aggregate stage page_id must be null")
    else:
        token(event["page_id"])
        require(re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9_.-]{0,63}:.+", event["page_id"]),
                "page_id must be source-qualified, e.g. loc:sn..._seq-1")
    observed = timestamp(event["observed_at"])
    kind, p = event["kind"], event["payload"]
    if kind == "stage":
        obj(p, ("stage_id", "label", "count", "state", "detail", "coverage"))
        token(p["stage_id"])
        require(p["stage_id"] not in ("runs", "artifacts") + STAGES,
                "aggregate stage_id must not shadow per-page stages")
        short_text(p["label"], 128)
        short_text(p["detail"], 512)
        short_text(p["coverage"], 128)
        require(p["count"] is None or (type(p["count"]) is int and
                0 <= p["count"] <= 1000000000000), "invalid aggregate count")
        require(p["state"] in ("observed", "unknown", "blocked"), "invalid aggregate state")
        require(p["state"] != "observed" or p["count"] is not None,
                "observed aggregate needs an explicit count")
        require(p["state"] != "unknown" or p["count"] is None,
                "unknown aggregate must not claim a count")
        identity, invariant, data = [p["stage_id"]], "", dict(p)
    elif kind == "artifact":
        require(isinstance(p, dict), "artifact payload must be an object")
        obj({k: v for k, v in p.items() if k not in ("fetched_at", "bytes", "condition", "observed_sha256")},
            ("artifact_id", "sha256", "version", "location", "reference", "state"))
        if "observed_sha256" in p:
            sha(p["observed_sha256"])
            require(p["observed_sha256"] == p["sha256"] and p["state"] == "verified",
                    "observed checksum must match expected checksum for verified evidence")
        if "condition" in p:
            require(p["condition"] in ("absent", "size_mismatch", "non_regular", "size_match"),
                    "invalid artifact condition")
        if "fetched_at" in p:
            timestamp(p["fetched_at"])
        if "bytes" in p:
            require(type(p["bytes"]) is int and 0 <= p["bytes"] <= 10**12,
                    "invalid artifact byte length")
        for key in ("artifact_id", "version", "location"):
            token(p[key])
        sha(p["sha256"])
        safe_reference(p["reference"])
        require(p["state"] in ("present", "verified", "missing"), "invalid artifact state")
        identity = [event["page_id"], p["artifact_id"], p["version"], p["location"]]
        invariant = digest([p["sha256"], p["reference"]])
        data = dict(p)
    elif kind == "execution":
        obj(p, ("worker_metadata",))
        data = adapt_worker(p["worker_metadata"], event["page_id"])
        identity = [event["page_id"], data["run_id"]]
        invariant = digest([data["config_sha256"],
                            [entry["image_sha256"] for entry in data["observations"]]])
    elif kind == "run":
        obj(p, ("run_record",))
        r = p["run_record"]
        require(isinstance(r, dict) and type(r.get("schema")) is int and r["schema"] == 1,
                "unsupported RunRecord schema")
        data = adapt_run(r, event["page_id"])
        identity = [event["page_id"], data["run_id"], data["candidate_rank"], data["engine_id"]]
        invariant = digest([data["config_sha256"], data["image_sha256"]])
    elif kind == "page_state":
        obj(p, ("stage", "state", "run_id"))
        require(p["stage"] in STAGES, "invalid independent page stage")
        require(p["state"] in ("present", "missing", "blocked"), "invalid page state")
        if p["run_id"] is not None:
            token(p["run_id"])
        identity = [event["page_id"], p["stage"], p["run_id"]]
        invariant = ""
        data = dict(p)
    else:
        raise CatalogError("unsupported observation kind")
    return {"key": digest([event["source"], kind, identity]), "invariant": invariant,
            "kind": kind, "source": event["source"], "page_id": event["page_id"],
            "event_id": event["event_id"], "observed_at": observed, "data": data,
            "event_hash": digest({k: v for k, v in event.items() if k != "batch_id"})}


DDL = """
CREATE TABLE meta(version INTEGER NOT NULL);
INSERT INTO meta VALUES(1);
CREATE TABLE batches(batch_id TEXT PRIMARY KEY, sha256 TEXT NOT NULL, events INTEGER NOT NULL);
CREATE TABLE events(source TEXT NOT NULL, event_id TEXT NOT NULL, sha256 TEXT NOT NULL,
                    batch_id TEXT NOT NULL, line_number INTEGER NOT NULL,
                    PRIMARY KEY(source,event_id));
CREATE TABLE latest(
 key TEXT PRIMARY KEY, invariant TEXT NOT NULL, kind TEXT NOT NULL,
 source TEXT NOT NULL, page_id TEXT, event_id TEXT NOT NULL,
 observed_at TEXT NOT NULL, data TEXT NOT NULL,
 engine TEXT, status TEXT, run_id TEXT, stage TEXT, state TEXT);
CREATE INDEX by_page ON latest(page_id,kind,key);
CREATE INDEX by_kind ON latest(kind,key);
CREATE INDEX by_engine ON latest(kind,engine,key);
CREATE INDEX by_status ON latest(kind,status,key);
CREATE INDEX by_source ON latest(kind,source,key);
CREATE INDEX by_run ON latest(kind,run_id,key);
CREATE INDEX by_stage ON latest(kind,stage,state);
"""


def connect(path, writable=False):
    path = Path(path)
    require(not path.is_symlink(), "catalog cannot be a symlink")
    require(writable or path.is_file(), "catalog not found; ingest evidence first")
    conn = sqlite3.connect(path.resolve().as_uri() + ("?mode=rwc" if writable else "?mode=ro"),
                           uri=True, timeout=2)
    conn.row_factory = sqlite3.Row
    try:
        if writable:
            # The schema transaction also makes first creation recoverable.
            if not conn.execute("SELECT 1 FROM sqlite_master WHERE name='meta'").fetchone():
                require(not conn.execute("SELECT 1 FROM sqlite_master LIMIT 1").fetchone(),
                        "not a catalog; choose a new database path")
                conn.executescript("BEGIN IMMEDIATE;\n" + DDL + "\nCOMMIT;")
        versions = conn.execute("SELECT version FROM meta LIMIT 2").fetchall()
        require(len(versions) == 1 and versions[0][0] == 1,
                "unsupported catalog schema (rebuild from evidence)")
        if writable:
            # Rollback journal permits truly read-only queries without WAL/SHM creation.
            conn.execute("PRAGMA journal_mode=DELETE")
            conn.execute("PRAGMA synchronous=FULL")
        if not writable:
            require(conn.execute("PRAGMA journal_mode").fetchone()[0] == "delete",
                    "catalog must use DELETE journaling; reopen with init to migrate")
            conn.execute("PRAGMA query_only=ON")
        return conn
    except Exception:
        conn.close()
        raise


def ingest(db, batch):
    """Validate the entire bounded batch before one durable transaction."""
    with Path(batch).open("rb") as stream:
        file_stat = os.fstat(stream.fileno())
        require(stat.S_ISREG(file_stat.st_mode) and file_stat.st_size <= MAX_BATCH,
                "batch must be a regular file <=16 MiB")
        raw = stream.read(MAX_BATCH + 1)
    require(0 < len(raw) <= MAX_BATCH, "empty or oversized batch")
    lines = raw.splitlines()
    require(0 < len(lines) <= MAX_EVENTS, "batch exceeds 10000 observations")
    parsed, batch_id = [], None
    for line in lines:
        require(0 < len(line) <= MAX_LINE, "empty or oversized JSONL line")
        event = load_json(line)
        projected = validate_event(event)
        if batch_id is None:
            batch_id = event["batch_id"]
        require(event["batch_id"] == batch_id, "mixed batch_id")
        parsed.append(projected)
    batch_hash = hashlib.sha256(raw).hexdigest()
    conn = connect(db, writable=True)
    try:
        with conn:
            conn.execute("BEGIN IMMEDIATE")
            previous = conn.execute("SELECT sha256 FROM batches WHERE batch_id=?", (batch_id,)).fetchone()
            if previous:
                require(previous[0] == batch_hash, "batch replay conflict")
                return {"batch_id": batch_id, "inserted": 0, "replayed": True}
            inserted = 0
            for line_number, event in enumerate(parsed, 1):
                old = conn.execute("SELECT sha256 FROM events WHERE source=? AND event_id=?",
                                   (event["source"], event["event_id"])).fetchone()
                if old:
                    require(old[0] == event["event_hash"], "event replay conflict")
                    continue
                current = conn.execute("SELECT invariant,observed_at,event_id FROM latest WHERE key=?",
                                       (event["key"],)).fetchone()
                require(not current or current["invariant"] == event["invariant"],
                        "immutable artifact/run identity conflict")
                if event["kind"] == "stage" and not current:
                    require(conn.execute("SELECT count(*) FROM latest WHERE kind='stage'").fetchone()[0] < MAX_AGGREGATES,
                            "aggregate stage limit is 24")
                conn.execute("INSERT INTO events VALUES(?,?,?,?,?)",
                             (event["source"], event["event_id"], event["event_hash"],
                              batch_id, line_number))
                inserted += 1
                if current and (event["observed_at"], event["event_id"]) <= (
                        current["observed_at"], current["event_id"]):
                    continue
                data = event["data"]
                conn.execute("""INSERT INTO latest VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?)
                    ON CONFLICT(key) DO UPDATE SET event_id=excluded.event_id,
                    observed_at=excluded.observed_at,data=excluded.data,
                    engine=excluded.engine,status=excluded.status,state=excluded.state""",
                             (event["key"], event["invariant"], event["kind"], event["source"],
                              event["page_id"], event["event_id"], event["observed_at"], canonical(data),
                              data.get("engine_id"), data.get("status"), data.get("run_id"),
                              data.get("stage"), data.get("state")))
            conn.execute("INSERT INTO batches VALUES(?,?,?)", (batch_id, batch_hash, len(parsed)))
        return {"batch_id": batch_id, "inserted": inserted, "replayed": False}
    finally:
        conn.close()


def ingest_cached_scans(db, index, scan_root, evidence_out, batch_id, source, cache_set):
    """Bounded index adapter: local stat is presence evidence, never checksum verification."""
    token(batch_id)
    token(source)
    require(cache_set in ("keep", "buffer"), "explicit cache_set keep or buffer required")
    root = Path(scan_root).resolve(strict=True)
    require(root.is_dir(), "scan root must be a directory")
    out = Path(evidence_out)
    require(not out.exists() and not out.is_symlink(), "evidence output must be a new file")
    rows, total = [], 0
    observed = timestamp(now())
    with Path(index).open("rb") as stream:
        require(stat.S_ISREG(os.fstat(stream.fileno()).st_mode), "index must be a regular file")
        for number, raw in enumerate(iter(lambda: stream.readline(MAX_LINE + 1), b""), 1):
            total += len(raw)
            require(number <= MAX_EVENTS and total <= MAX_BATCH and len(raw) <= MAX_LINE,
                    "index exceeds bounded batch; split locally before importing")
            entry = load_json(raw)
            require(isinstance(entry, dict), "cached scan index row must be an object")
            obj({k: v for k, v in entry.items() if k not in ("iiif_base", "user_agent", "format", "md5")},
                ("doc_id", "batch", "batch_version", "loc_url", "resolution",
                 "fetched_at", "bytes", "sha256", "download_ms"))
            for key in ("loc_url", "iiif_base", "user_agent", "md5"):
                if key in entry:
                    require(entry[key] is None or (isinstance(entry[key], str) and len(entry[key]) <= 8192),
                            "invalid discarded cache metadata")
            scan_format = entry.get("format", "jpeg")
            require(scan_format in ("jpeg", "jp2"), "unsupported cached scan format")
            token(entry["doc_id"])
            require(re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9_.-]{0,127}", entry["batch"]),
                    "invalid cached scan batch")
            require(type(entry["batch_version"]) is int and entry["batch_version"] > 0,
                    "invalid batch_version")
            require(entry["resolution"] == "full", "only full-resolution cache indexes supported")
            sha(entry["sha256"])
            timestamp(entry["fetched_at"])
            require(type(entry["bytes"]) is int and 0 <= entry["bytes"] <= 10**12,
                    "invalid cached scan bytes")
            require(type(entry["download_ms"]) is int and entry["download_ms"] >= 0,
                    "invalid download_ms")
            match = re.fullmatch(r"([A-Za-z0-9-]+)_(\d{4}-\d\d-\d\d)_ed-(\d+)_seq-(\d+)",
                                 entry["doc_id"])
            require(match is not None, "unsupported cached scan doc_id")
            paper, date, edition, sequence = match.groups()
            extension = "jp2" if scan_format == "jp2" else "jpg"
            reference = f"{cache_set}/{entry['batch']}/{paper}/{date}/{int(edition)}/{int(sequence)}.{extension}"
            candidate = root / reference
            require(candidate.resolve().is_relative_to(root), "cached scan escapes scan root")
            require(not any(parent.is_symlink() for parent in
                            (candidate,) + tuple(candidate.parents) if parent != root),
                    "cached scan path must not contain symlinks")
            try:
                file_stat = candidate.stat()
                present = stat.S_ISREG(file_stat.st_mode) and file_stat.st_size == entry["bytes"]
                condition = ("non_regular" if not stat.S_ISREG(file_stat.st_mode) else
                             "size_match" if present else "size_mismatch")
            except FileNotFoundError:
                present = False
                condition = "absent"
            payload = {"artifact_id": "scan", "sha256": entry["sha256"],
                       "version": entry["sha256"],
                       "location": "cached-scans" if cache_set == "keep" else "cached-scans-buffer",
                       "reference": reference, "state": "present" if present else "missing",
                       "condition": condition,
                       "fetched_at": entry["fetched_at"], "bytes": entry["bytes"]}
            event = {"schema_version": 1, "batch_id": batch_id,
                     "event_id": "cached-" + digest([batch_id, number, entry["doc_id"]]),
                     "kind": "artifact", "source": source, "observed_at": observed,
                     "page_id": "loc:" + entry["doc_id"], "payload": payload}
            validate_event(event)
            rows.append(canonical(event) + "\n")
    require(rows, "empty cached scan index")
    encoded = "".join(rows).encode()
    require(len(encoded) <= MAX_BATCH, "projected batch exceeds 16 MiB; split index")
    # Original fetched_at is retained; stat time is the new observation, not byte verification.
    with out.open("xb") as stream:
        os.chmod(out, 0o600)
        stream.write(encoded)
        stream.flush()
        os.fsync(stream.fileno())
    return ingest(db, out)


def ingest_worker(db, input_path, evidence_out, batch_id, source):
    """One real atomic worker metadata JSON file -> immutable catalog evidence batch."""
    token(batch_id)
    token(source)
    with Path(input_path).open("rb") as stream:
        require(stat.S_ISREG(os.fstat(stream.fileno()).st_mode), "worker input must be a regular file")
        raw = stream.read(MAX_LINE + 1)
    require(0 < len(raw) <= MAX_LINE, "worker envelope exceeds adapter limit 64 KiB")
    metadata = load_json(raw)
    require(isinstance(metadata, dict) and isinstance(metadata.get("doc_id"), str),
            "invalid worker metadata")
    page_id = "loc:" + metadata["doc_id"]
    adapt_worker(metadata, page_id)
    events = []
    for kind, payload in (("execution", {"worker_metadata": metadata}),
                          ("run", {"run_record": metadata["run_record"]})):
        if kind == "run" and metadata["run_record"] is None:
            continue
        event = {"schema_version": 1, "batch_id": batch_id,
                 "event_id": "worker-" + digest([metadata["run_id"], kind]),
                 "kind": kind, "source": source, "observed_at": metadata["finished_at"],
                 "page_id": page_id, "payload": payload}
        validate_event(event)
        require(len(canonical(event).encode()) <= MAX_LINE, "projected worker event exceeds 64 KiB")
        events.append(canonical(event) + "\n")
    out = Path(evidence_out)
    require(not out.exists() and not out.is_symlink(), "evidence output must be a new file")
    require(out.resolve() != Path(db).resolve(), "evidence must not overwrite catalog")
    with out.open("xb") as stream:
        os.chmod(out, 0o600)
        stream.write("".join(events).encode())
        stream.flush()
        os.fsync(stream.fileno())
    return ingest(db, out)


def bounded_query(db, action):
    conn = connect(db)
    deadline = time.monotonic() + QUERY_SECONDS
    conn.set_progress_handler(lambda: int(time.monotonic() > deadline), 1000)
    try:
        conn.execute("BEGIN")  # Consistent multi-query snapshot, including source freshness.
        result = action(conn)
        require(len(canonical(result).encode()) <= MAX_RESULT, "response exceeds 1 MiB; narrow query")
        return result
    finally:
        conn.close()


def rows(conn, kind, filters, limit=50, after=""):
    require(type(limit) is int and 1 <= limit <= MAX_LIMIT, "limit must be 1..100")
    require(isinstance(after, str) and (not after or SHA.fullmatch(after)), "invalid cursor")
    allowed = {"page_id", "source", "engine", "status", "run_id"}
    require(set(filters) <= allowed, "unsupported filter")
    where, args = ["latest.kind=?", "key>?"], [kind, after]
    for key, value in sorted(filters.items()):
        token(value)
        where.append("latest." + key + "=?")
        args.append(value)
    found = conn.execute("""SELECT latest.*,events.batch_id,events.line_number,
                         events.sha256 AS event_sha256 FROM latest
                         JOIN events USING(source,event_id) WHERE """ + " AND ".join(where) +
                         " ORDER BY key LIMIT ?", (*args, limit + 1)).fetchall()
    items = [{key: row[key] for key in ("key", "source", "page_id", "event_id", "observed_at",
                                      "batch_id", "line_number", "event_sha256")}
             | {"kind": row["kind"], "data": load_json(row["data"])} for row in found[:limit]]
    return {"items": items, "next_cursor": found[limit - 1]["key"] if len(found) > limit else None}


def query(db, kind, filters, limit=50, after=""):
    require(kind in ("run", "execution", "artifact", "page_state", "stage"), "unsupported query kind")
    return bounded_query(db, lambda conn: rows(conn, kind, filters, limit, after))


def page(db, page_id, limit=50, after=""):
    token(page_id)
    # Page detail paginates ALL independent evidence, rather than hiding omitted rows.
    def read(conn):
        require(type(limit) is int and 1 <= limit <= MAX_LIMIT, "limit must be 1..100")
        require(not after or SHA.fullmatch(after), "invalid cursor")
        found = conn.execute("""SELECT latest.*,events.batch_id,events.line_number,
                             events.sha256 AS event_sha256 FROM latest
                             JOIN events USING(source,event_id)
                             WHERE page_id=? AND key>? ORDER BY key LIMIT ?""",
                             (page_id, after, limit + 1)).fetchall()
        items = [{k: row[k] for k in ("key", "kind", "source", "page_id", "event_id", "observed_at",
                                    "batch_id", "line_number", "event_sha256")}
                 | {"data": load_json(row["data"])} for row in found[:limit]]
        return {"page_id": page_id, "items": items,
                "next_cursor": found[limit - 1]["key"] if len(found) > limit else None}
    return bounded_query(db, read)


def snapshot(db):
    def read(conn):
        def count(sql, args=()):
            return conn.execute(sql, args).fetchone()[0]

        observed = count("SELECT max(observed_at) FROM latest")
        stages, coverage, issues = [], [], []
        for stage in ("runs", "artifacts") + STAGES:
            if stage in ("runs", "artifacts"):
                kind = {"runs": "run", "artifacts": "artifact"}[stage]
                kinds = (kind, "execution" if kind == "run" else kind)
                total = count("SELECT count(*) FROM latest WHERE kind IN (?,?)", kinds)
                n = count("SELECT count(DISTINCT page_id) FROM latest WHERE kind IN (?,?)", kinds)
                as_of = count("""SELECT min(t) FROM
                    (SELECT max(observed_at) t FROM latest WHERE kind IN (?,?) GROUP BY source)""", kinds)
                blocked = 0
            else:
                total = count("SELECT count(*) FROM latest WHERE kind='page_state' AND stage=?", (stage,))
                as_of = count("""SELECT min(t) FROM (SELECT max(observed_at) t FROM latest
                    WHERE kind='page_state' AND stage=? GROUP BY source)""", (stage,))
                n = count("""SELECT count(DISTINCT page_id) FROM latest
                    WHERE kind='page_state' AND stage=? AND state='present'""", (stage,))
                blocked = count("""SELECT count(*) FROM latest
                    WHERE kind='page_state' AND stage=? AND state='blocked'""", (stage,))
            coverage.append(stage + (":observed" if total else ":unknown"))
            if not total:
                issues.append({"code": "missing_source", "detail": stage + ": no observations imported"})
            stages.append({"id": stage, "label": stage.capitalize(),
                           "count": n if total else None,
                           "state": "blocked" if blocked else "observed" if total else "unknown",
                           "observed_at": as_of,
                           "detail": "Independent source observations; not inferred from engine success."
                           if total else "No source evidence; adapter/export required."})
        aggregate_rows = conn.execute("SELECT * FROM latest WHERE kind='stage' ORDER BY key LIMIT 25").fetchall()
        require(len(aggregate_rows) <= MAX_AGGREGATES, "aggregate stage limit exceeded")
        ids = [load_json(row["data"])["stage_id"] for row in aggregate_rows]
        for row in aggregate_rows:
            value = load_json(row["data"])
            stage_id = value["stage_id"]
            if ids.count(stage_id) > 1:
                stage_id = stage_id[:243] + "@" + digest(row["source"])[:12]
            provenance = " [source=" + row["source"] + "; observed_at=" + row["observed_at"] + "]"
            stages.append({"id": stage_id, "label": value["label"], "count": value["count"],
                           "state": value["state"],
                           "observed_at": row["observed_at"],
                           "detail": value["detail"][:512 - len(provenance)] + provenance})
            coverage.append(value["coverage"][:128 - len(value["state"]) - 1] + ":" + value["state"])
            if value["state"] != "observed":
                issues.append({"code": "source_" + value["state"],
                               "detail": (stage_id + ": " + value["detail"])[:512]})
        mismatch_count = count("""SELECT count(*) FROM latest WHERE kind='artifact'
            AND json_extract(data,'$.condition')='size_mismatch'""")
        if mismatch_count:
            issues.append({"code": "artifact_size_mismatch",
                           "detail": f"{mismatch_count} cached paths exist but their size differs from index evidence; not verified copies."})
        return {"schema_version": 1, "observed_at": observed, "generated_at": now(),
                "producer": "corpus-catalog", "coverage": coverage, "stages": stages,
                "runs": {"total": count("""SELECT count(*) FROM
                    (SELECT DISTINCT source,run_id FROM latest WHERE kind IN ('run','execution'))"""),
                         "pages": count("SELECT count(DISTINCT page_id) FROM latest WHERE kind IN ('run','execution')")},
                "artifacts": {
                    "locations": count("SELECT count(*) FROM latest WHERE kind='artifact'"),
                    "verified": count("SELECT count(*) FROM latest WHERE kind='artifact' AND state='verified'"),
                    "missing": count("SELECT count(*) FROM latest WHERE kind='artifact' AND state='missing'")},
                "issues": issues}
    return bounded_query(db, read)


def plan_missing(db, page_id, limit=50, after=""):
    # Evidence-only recommendations. No fetch, scheduling, or trust-state writes.
    result = query(db, "artifact", {"page_id": page_id}, limit, after)
    result["items"] = [item for item in result["items"] if item["data"]["state"] == "missing"]
    result["read_only"] = True
    result["detail"] = "Missing-copy observations only; presence elsewhere does not authorize copying."
    return result


def atomic_snapshot(path, value, db):
    path = Path(path)
    require(path.resolve() != Path(db).resolve() and
            path.resolve() not in (Path(str(db) + "-wal").resolve(), Path(str(db) + "-shm").resolve()),
            "snapshot must not overwrite catalog")
    require(not path.is_symlink() and (not path.exists() or path.is_file()), "unsafe snapshot path")
    raw = (canonical(value) + "\n").encode()
    require(len(raw) <= MAX_RESULT, "snapshot exceeds 1 MiB")
    sibling = path.with_name("." + path.name + "." + uuid.uuid4().hex + ".pending")
    try:
        with sibling.open("xb") as stream:
            os.chmod(sibling, 0o600)
            stream.write(raw)
            stream.flush()
            os.fsync(stream.fileno())
        os.replace(sibling, path)
    finally:
        sibling.unlink(missing_ok=True)


def make_server(db, host="127.0.0.1", port=0):
    require(host in ("127.0.0.1", "::1"), "HTTP must bind numeric loopback only")
    # Verify existing schema before listening; handlers open read-only connections.
    conn = connect(db)
    conn.close()

    class Handler(http.server.BaseHTTPRequestHandler):
        timeout = 3

        def log_message(self, *_args):
            pass  # Never log request strings containing accidental credentials.

        def do_GET(self):
            try:
                require(re.fullmatch(r"(?:127\.0\.0\.1|\[::1\])(?::\d{1,5})?",
                                     self.headers.get("Host", "")) is not None,
                        "loopback Host header required")
                require(len(self.path) <= 2048, "request URI too long")
                uri = urllib.parse.urlsplit(self.path)
                params = urllib.parse.parse_qs(uri.query, keep_blank_values=True, max_num_fields=10)
                require(all(len(v) == 1 for v in params.values()), "duplicate query parameter")
                params = {k: v[0] for k, v in params.items()}
                if uri.path == "/health":
                    require(not params, "health does not accept query parameters")
                    result = bounded_query(db, lambda conn: {
                        "schema_version": 1, "catalog": "readable",
                        "observed_at": conn.execute("SELECT max(observed_at) FROM latest").fetchone()[0]})
                    self.send_json(200, result)
                    return
                allowed = {"limit", "after"}
                if uri.path in ("/runs", "/executions"):
                    allowed |= {"page_id", "source", "engine", "status", "run_id"}
                elif uri.path == "/page":
                    allowed |= {"page_id"}
                else:
                    self.send_json(404, {"error": "unknown endpoint"})
                    return
                require(set(params) <= allowed, "unsupported query parameter")
                limit = int(params.pop("limit", "50"))
                after = params.pop("after", "")
                if uri.path in ("/runs", "/executions"):
                    result = query(db, "run" if uri.path == "/runs" else "execution", params, limit, after)
                else:
                    require("page_id" in params, "page_id required")
                    result = page(db, params["page_id"], limit, after)
                self.send_json(200, result)
            except (CatalogError, ValueError):
                self.send_json(400, {"error": "invalid or over-limit query"})
            except sqlite3.Error:
                self.send_json(503, {"error": "catalog unavailable or query deadline exceeded"})

        def send_json(self, code, value):
            raw = (canonical(value) + "\n").encode()
            self.send_response(code)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(raw)))
            self.send_header("Cache-Control", "no-store")
            self.send_header("X-Content-Type-Options", "nosniff")
            self.end_headers()
            self.wfile.write(raw)

    class Server(http.server.HTTPServer):
        request_queue_size = 8  # Single request at a time, no unbounded thread pool.

        def get_request(self):
            sock, address = super().get_request()
            sock.settimeout(3)
            return sock, address

    if host == "::1":
        import socket
        Server.address_family = socket.AF_INET6
    return Server((host, port), Handler)


HELP = """Local rebuildable metadata only; no cloud calls, OCR text, scheduling or trust mutation.
Canonical Blob/Parquet remains authoritative. Bootstrap on a new --db: export bounded,
immutable JSONL evidence batches locally; ingest each once; retain originals for rebuild.
Never overwrite an evidence batch: batch_id is bound to exact file bytes, event_id to
canonical event content within source. Preserve original batch_id when replaying.
Overlapping batches may repeat unchanged event_ids: batch_id is not event content.
Queries return source/event time, event hash and original batch_id/line_number.

Every JSONL line has exactly:
{"schema_version":1,"batch_id":"export-1","event_id":"obs-1","kind":"artifact",
 "source":"blob-inventory","observed_at":"2026-10-16T12:00:00Z",
 "page_id":"loc:sn123_1900-01-01_ed-1_seq-1","payload":
 {"artifact_id":"scan","sha256":"<64 lowercase hex>","version":"v1",
  "location":"archive","reference":"container/path/scan.jp2","state":"verified"}}
Artifact state: present|verified|missing. Verification is source evidence, not a fetch.
kind=run payload={"run_record":<actual RunRecord JSON with schema:1 from runs.rs>}.
kind=execution payload={"worker_metadata":<actual worker metadata_schema:1 JSON object>}.
ingest-worker --input FILE --evidence-out NEW.jsonl --batch-id ID --source ID adapts one
atomic worker capture file. Terminal success, failure and abandonment are stored as
execution observations; optional nonnull run_record is imported separately as an engine
read. Upload failure may coexist with engine status ok; neither implies acceptance.
Known runtime configs are fingerprinted, not served; engine attempts retain status,
timestamps, input hashes and provenance (planned/instance/result), no text/free-form tags.
Worker adapter limit: 64 KiB including projected event; larger captures fail explicitly.
GET /executions (same filters/pagination as /runs) lists worker attempts separately.
RunRecord must have exact v1 fields. image_sha256/text_sha256 are full bare hashes;
image_digest/models_digest allow the existing 'local' sentinel (digest unavailable),
otherwise digest fields require sha256:<64 lowercase hex>. No hash is fabricated.
Free-form JSON, image_ref,
adapter, node and OCR engine strings are validated/discarded, not served; configuration
is represented by a hash. Source-qualified page suffix must equal RunRecord.doc_id.
kind=page_state payload={"stage":"accepted","state":"present","run_id":null};
stages: queued|allocated|outcome|accepted|imported|indexed; states: present|missing|blocked.
kind=stage has page_id:null and payload exactly:
{"stage_id":"boinc-ready","label":"BOINC ready","count":19850,"state":"observed",
 "detail":"Public task totals; underlying cache age unknown.","coverage":"BOINC tasks"}.
These are explicit aggregate SOURCE observations, never synthesized page/artifact rows.
Max 24 independent source/stage identities; label128/detail512/coverage128 chars,
count <= 10^12; unknown requires null count, observed requires an integer.
Aggregate stage_id must not shadow runs/artifacts or independent per-page stage names.
Snapshot appends aggregate stages with source/time provenance in detail; duplicate
stage_ids from independent sources receive @<source-hash> suffixes, never summed.
These statuses remain independent, per source and optional run_id; ok/selected NEVER
implies accepted/imported/indexed. Event time, then lexicographic event_id breaks ties.
Artifact hash/reference and run config/input are immutable within their identities.
Use new artifact version or run identity for changes. Sources remain independent.

Adapters provided: actual worker metadata capture JSON, RunRecord JSON projection,
bounded local cached-scan index/stat and explicit artifact/page-state/stage evidence.
NOT provided: Parquet/Blob inventory or queue/outcomes Parquet, ocr-local/pages import
ledger, BOINC allocation journal, acceptance or search index exporters. Export those
authoritative sources independently; do not label a local catalog ingest as page import.
Missing adapters/sources produce unknown coverage (no fabricated BOINC-ready counts).
Snapshot observed_at is newest source observation; null with no evidence, never generation
time. Each stage also has observed_at (null if absent); per-page stage freshness is the
oldest latest observation among its independent sources, never a different stage's clock.
Counts are observed metadata, NOT corpus totals or allocation readiness.

Limits: 16 MiB/10000 events per batch, 64 KiB/line, identifiers 256 chars,
references 512 chars (relative object keys ONLY, no URI/credentials), run strings
8192 chars, 100 rows/page, 1 MiB/response or snapshot, SQLite query deadline 2s,
busy timeout 2s, HTTP socket timeout 3s, 2048-char URI, 10 query parameters.
HTTP is optional read-only numeric loopback, single-request bounded backlog; no SQL.
Catalog uses DELETE rollback journaling so read-only connections do not create WAL/SHM.
Migrate older local WAL catalogs by stopping readers and running --db DB init once.
Do not expose it through an unauthenticated proxy. Local files/exports are trusted
operator inputs; use filesystem access controls, never include text or credentials
in identifiers or references. Retain evidence on durable storage; SQLite is disposable.
ingest-cached-scans explicitly adapts bounded index-full.jsonl slices plus actual local
stat of <scan-root>/<cache-set>/<batch>/<paper>/<date>/<edition>/<sequence>.<jpg|jp2>.
--cache-set keep|buffer is REQUIRED because indexes do not encode set membership.
Producer Sidecar fields iiif_base,user_agent,format,md5 are accepted but free-form fields
are discarded; format determines extension (jpeg defaults for legacy rows). Index alone
does NOT establish presence. Regular file and matching size => present, never verified;
absent/size mismatch => missing. Does not fetch or read image bytes. Original nanosecond
fetched_at and byte length are retained; observed_at is adapter stat observation time.
The condition field distinguishes absent, size_mismatch, non_regular and size_match;
snapshot issues report size mismatches explicitly (not ordinary absent copies).
Original loc_url/download timing are discarded. A fresh immutable --evidence-out is
required for replay/rebuild. Split large indexes into <=10000-line/16MiB local batches.
Artifact payload optionally accepts fetched_at, bytes and condition for source provenance.
Verified imports may supply observed_sha256: it must equal expected sha256 and state must
be verified. Both checksums are retained; the catalog NEVER computes or invents them.
GET /health tests catalog readability only, not pipeline readiness or source freshness.
"""


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__, epilog=HELP,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--db", required=True, help="local SQLite read model path")
    subs = parser.add_subparsers(dest="command", required=True)
    p = subs.add_parser("ingest", help="transactionally import one immutable evidence JSONL batch")
    p.add_argument("--batch", required=True)
    p = subs.add_parser("ingest-cached-scans", help="bounded cache index + local stat, NOT verification")
    p.add_argument("--index", required=True)
    p.add_argument("--scan-root", required=True)
    p.add_argument("--cache-set", required=True, choices=("keep", "buffer"))
    p.add_argument("--evidence-out", required=True)
    p.add_argument("--batch-id", required=True)
    p.add_argument("--source", required=True)
    p = subs.add_parser("ingest-worker", help="import actual worker metadata_schema:1 capture JSON")
    p.add_argument("--input", required=True)
    p.add_argument("--evidence-out", required=True)
    p.add_argument("--batch-id", required=True)
    p.add_argument("--source", required=True)
    subs.add_parser("init", help="create an empty local catalog; snapshot coverage remains unknown")
    p = subs.add_parser("query", help="bounded page detail or filtered metadata (keyset pagination)")
    p.add_argument("kind", choices=("page", "run", "execution", "artifact", "page_state", "stage", "missing"))
    for name in ("page-id", "source", "engine", "status", "run-id"):
        p.add_argument("--" + name)
    p.add_argument("--limit", type=int, default=50)
    p.add_argument("--after", default="")
    p = subs.add_parser("snapshot", help="source-fresh dashboard summary; atomically replace --out")
    p.add_argument("--out")
    p = subs.add_parser("serve", help="optional read-only GET /page?page_id=... and /runs?engine=...")
    p.add_argument("--host", default="127.0.0.1", choices=("127.0.0.1", "::1"))
    p.add_argument("--port", type=int, default=8764)
    args = parser.parse_args(argv)
    try:
        if args.command == "init":
            conn = connect(args.db, writable=True)
            conn.close()
            result = {"schema_version": 1, "initialized": True}
        elif args.command == "ingest":
            result = ingest(args.db, args.batch)
        elif args.command == "ingest-cached-scans":
            result = ingest_cached_scans(args.db, args.index, args.scan_root,
                                         args.evidence_out, args.batch_id, args.source, args.cache_set)
        elif args.command == "ingest-worker":
            result = ingest_worker(args.db, args.input, args.evidence_out, args.batch_id, args.source)
        elif args.command == "query":
            filters = {key: getattr(args, key) for key in
                       ("page_id", "source", "engine", "status", "run_id") if getattr(args, key)}
            if args.kind in ("page", "missing"):
                require(set(filters) == {"page_id"}, "page/missing requires only --page-id")
                result = (page if args.kind == "page" else plan_missing)(
                    args.db, args.page_id, args.limit, args.after)
            else:
                result = query(args.db, args.kind, filters, args.limit, args.after)
        elif args.command == "snapshot":
            result = snapshot(args.db)
            if args.out:
                atomic_snapshot(args.out, result, args.db)
                return 0
        else:
            require(0 <= args.port <= 65535, "invalid port")
            with make_server(args.db, args.host, args.port) as server:
                print(canonical({"listening": server.server_address[:2]}), flush=True)
                server.serve_forever()
            return 0
        print(canonical(result))
        return 0
    except (CatalogError, OSError, sqlite3.Error) as exc:
        # Do not echo raw source values, SQL, request strings or credential-bearing paths.
        message = str(exc) if isinstance(exc, CatalogError) else "local catalog I/O or query failure"
        print(canonical({"error": message}), file=sys.stderr)
        return 2


if __name__ == "__main__":
    sys.exit(main())
