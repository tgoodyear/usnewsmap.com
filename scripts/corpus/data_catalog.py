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
import tempfile
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
DECISIONS = ("accepted", "rejected", "revoked", "unchanged_baseline")
PAGE_STATE_OPT = {
    "import_id", "release_id", "index_id", "decision", "decision_reason",
    "selected_source", "selected_engine", "committed_at", "decided_at",
    "published_at", "verified_at", "search_receipt",
}
SEARCH_RECEIPT_FIELDS = (
    "index_version", "hit_present", "ocr_source", "ocr_engine",
    "response_sha256", "verified_at",
)
ENGINE_SHORT_RE = re.compile(
    r"([a-z0-9][a-z0-9_.+-]{0,63})/([0-9A-Za-z][0-9A-Za-z.+-]{0,63}) ([a-z0-9][a-z0-9_.+-]{0,63})\Z"
)
ENGINE_LEGACY_RE = re.compile(
    r"([a-z0-9][a-z0-9_.+]{0,63})-([0-9A-Za-z][0-9A-Za-z.+-]{0,63})/([a-z0-9][a-z0-9_.+-]{0,63})\Z"
)
INDEXED_KEY_SCOPE_VERSION = 2

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


def source_family(ocr_source):
    if isinstance(ocr_source, str) and ocr_source and ocr_source != "loc":
        return ocr_source[5:] if ocr_source.startswith("usnm-") else ocr_source
    return None


def canonical_engine_key(value, family=None):
    """Normalize a 13 §13.8.1 tag, short key, or legacy <family>-<ver>/<profile> tag to <family>/<ver> <profile>."""
    require(isinstance(value, str) and bool(value.strip()), "engine identity cannot be blank")
    short_text(value, 256)
    short_m = ENGINE_SHORT_RE.fullmatch(value)
    if short_m is not None:
        fam, version, profile = short_m.groups()
        return f"{fam}/{version} {profile}"
    if " " not in value and "/" in value and isinstance(family, str) and family:
        engine, profile = value.split("/", 1)
        if engine.startswith(family + "-"):
            version = engine[len(family) + 1:].lstrip("-")
            candidate = f"{family}/{version} {profile}"
            if ENGINE_SHORT_RE.fullmatch(candidate) is not None:
                return candidate
    legacy_m = ENGINE_LEGACY_RE.fullmatch(value)
    if legacy_m is not None:
        fam, version, profile = legacy_m.groups()
        return f"{fam}/{version} {profile}"
    parts = value.split(" ")
    if len(parts) > 1 and any("=" in field for field in parts[1:]):
        head = parts[0]
        require("/" in head, "structured ocr_engine tag requires <family>/<version>")
        fam, version = head.split("/", 1)
        seen = {}
        for field in parts[1:]:
            require("=" in field, "structured ocr_engine field must be key=value")
            k, v = field.split("=", 1)
            require(k not in seen, "duplicate structured ocr_engine field")
            seen[k] = v
        require("profile" in seen, "structured ocr_engine tag requires profile")
        profile = seen["profile"]
        require(ENGINE_SHORT_RE.fullmatch(f"{fam}/{version} {profile}") is not None,
                "invalid structured ocr_engine family/version/profile")
        return f"{fam}/{version} {profile}"
    return value


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
    selected_engine = f"{record['family']}/{record['engine_version']} {record['profile']}"
    if "/" in record["ocr_engine"] or " " in record["ocr_engine"]:
        require(canonical_engine_key(record["ocr_engine"], family=record["family"]) == selected_engine,
                "RunRecord ocr_engine does not match family/engine_version/profile")
    config = {key: record[key] for key in (
        "engine_id", "engine_version", "image_digest", "models_digest", "models_json",
        "profile", "options_json", "scale", "tiles", "route_id", "routing_version",
        "routing_digest", "worker_version", "contract")}
    result = {key: record[key] for key in (
        "run_id", "engine_id", "engine_version", "family", "profile", "route_id",
        "status", "selected", "candidate_rank", "image_sha256", "image_digest",
        "models_digest", "language", "seconds_wall", "seconds_cpu", "peak_mem_mb",
        "oom", "mean_conf", "damage_before", "damage_after")}
    result.update(selected_engine=selected_engine,
                  config_sha256=digest(config),
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
        require(isinstance(p, dict), "page_state payload must be an object")
        obj({k: v for k, v in p.items() if k not in PAGE_STATE_OPT},
            ("stage", "state", "run_id"))
        require(p["stage"] in STAGES, "invalid independent page stage")
        require(p["state"] in ("present", "missing", "blocked"), "invalid page state")
        if p["run_id"] is not None:
            token(p["run_id"])
        data = dict(p)
        for key in ("import_id", "release_id", "index_id", "decision_reason", "selected_source"):
            if data.get(key) is not None:
                token(data[key])
        if data.get("selected_engine") is not None:
            require(isinstance(data["selected_engine"], str) and data["selected_engine"].strip(),
                    "selected_engine cannot be blank")
            short_text(data["selected_engine"], 256)
        for key in ("committed_at", "decided_at", "published_at", "verified_at"):
            if data.get(key) is not None:
                data[key] = timestamp(data[key])
        if data.get("decision") is not None:
            require(data["stage"] in ("accepted", "indexed"),
                    "decision is only valid for accepted or indexed stages")
            require(data["decision"] in DECISIONS, "invalid release/publication decision")
            if data["decision"] == "accepted":
                require(data["state"] == "present", "accepted decision requires present state")
                require(data["run_id"] is not None, "accepted decision requires run_id")
            elif data["decision"] in ("rejected", "unchanged_baseline"):
                require(data["state"] == "missing",
                        "rejected or unchanged_baseline decision requires missing state")
            elif data["decision"] == "revoked":
                require(data["state"] in ("blocked", "missing"),
                        "revoked decision requires blocked or missing state")
                require(data["run_id"] is not None, "revoked decision requires run_id")
        if data.get("search_receipt") is not None:
            require(data["stage"] == "indexed", "search_receipt is only valid on indexed stage")
            receipt = data["search_receipt"]
            obj(receipt, SEARCH_RECEIPT_FIELDS)
            token(receipt["index_version"])
            require(receipt["hit_present"] is True,
                    "search_receipt requires a matched target hit (hit_present=true)")
            if receipt["ocr_source"] is not None:
                token(receipt["ocr_source"])
            if receipt["ocr_engine"] is not None:
                require(isinstance(receipt["ocr_engine"], str) and receipt["ocr_engine"].strip(),
                        "search_receipt ocr_engine cannot be blank")
                short_text(receipt["ocr_engine"], 256)
            sha(receipt["response_sha256"])
            receipt = dict(receipt, verified_at=timestamp(receipt["verified_at"]))
            if not receipt["hit_present"] or receipt["ocr_source"] is None:
                require(data["state"] != "present" and data.get("decision") != "accepted",
                        "search_receipt without replacement ocr_source cannot claim present/accepted indexed state")
            if data.get("index_id") is not None:
                require(data["index_id"] == receipt["index_version"],
                        "search_receipt index_version must match index_id")
            data["search_receipt"] = receipt
        # Current publication state in search is page-scoped per source so a newer baseline
        # or replacement verification supersedes earlier run-scoped indexed state in latest.
        identity = ([event["page_id"], p["stage"]] if p["stage"] == "indexed"
                    else [event["page_id"], p["stage"], p["run_id"]])
        invariant = ""
    else:
        raise CatalogError("unsupported observation kind")
    return {"key": digest([event["source"], kind, identity]), "invariant": invariant,
            "kind": kind, "source": event["source"], "page_id": event["page_id"],
            "event_id": event["event_id"], "observed_at": observed, "data": data,
            "event_hash": digest({k: v for k, v in event.items() if k != "batch_id"})}


DDL = """
CREATE TABLE meta(version INTEGER NOT NULL);
INSERT INTO meta VALUES(1);
PRAGMA user_version=2;
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


def migrate_indexed_keys(conn):
    """One-time writable upgrade: collapse legacy run-scoped indexed keys to page-scoped keys."""
    with conn:
        conn.execute("BEGIN IMMEDIATE")
        found = conn.execute(
            "SELECT key, source, page_id, observed_at, event_id FROM latest "
            "WHERE kind='page_state' AND stage='indexed'"
        ).fetchall()
        groups = {}
        for row in found:
            expected_key = digest([row["source"], "page_state", [row["page_id"], "indexed"]])
            groups.setdefault((row["source"], row["page_id"], expected_key), []).append(row)
        for (_source, _page_id, expected_key), group in groups.items():
            if len(group) == 1 and group[0]["key"] == expected_key:
                continue
            winner_ref = max(group, key=lambda r: (r["observed_at"], r["event_id"]))
            winner = conn.execute("SELECT * FROM latest WHERE key=?", (winner_ref["key"],)).fetchone()
            for row in group:
                conn.execute("DELETE FROM latest WHERE key=?", (row["key"],))
            conn.execute(
                "INSERT INTO latest VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?)",
                (expected_key, winner["invariant"], winner["kind"], winner["source"],
                 winner["page_id"], winner["event_id"], winner["observed_at"],
                 winner["data"], winner["engine"], winner["status"],
                 winner["run_id"], winner["stage"], winner["state"]),
            )
        conn.execute(f"PRAGMA user_version={INDEXED_KEY_SCOPE_VERSION}")


def connect(path, writable=False):
    path = Path(path)
    require(not path.is_symlink(), "catalog cannot be a symlink")
    require(writable or path.is_file(), "catalog not found; ingest evidence first")
    conn = sqlite3.connect(path.resolve().as_uri() + ("?mode=rwc" if writable else "?mode=ro"),
                           uri=True, timeout=2)
    conn.row_factory = sqlite3.Row
    deadline = time.monotonic() + QUERY_SECONDS
    conn.set_progress_handler(lambda: int(time.monotonic() > deadline), 1000)
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
        user_version = conn.execute("PRAGMA user_version").fetchone()[0]
        require(user_version in (0, INDEXED_KEY_SCOPE_VERSION),
                "unsupported catalog schema (rebuild from evidence)")
        if writable:
            # Rollback journal permits truly read-only queries without WAL/SHM creation.
            conn.execute("PRAGMA journal_mode=DELETE")
            conn.execute("PRAGMA synchronous=FULL")
            if user_version != INDEXED_KEY_SCOPE_VERSION:
                migrate_indexed_keys(conn)
        else:
            require(conn.execute("PRAGMA journal_mode").fetchone()[0] == "delete",
                    "catalog must use DELETE journaling; reopen with init to migrate")
            require(user_version == INDEXED_KEY_SCOPE_VERSION,
                    "catalog has legacy run-scoped indexed keys; reopen with init or ingest to migrate")
            conn.execute("PRAGMA query_only=ON")
        conn.set_progress_handler(None, 0)
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
                    engine=excluded.engine,status=excluded.status,
                    run_id=excluded.run_id,stage=excluded.stage,state=excluded.state""",
                             (event["key"], event["invariant"], event["kind"], event["source"],
                              event["page_id"], event["event_id"], event["observed_at"], canonical(data),
                              data.get("engine_id"), data.get("status") or data.get("decision"),
                              data.get("run_id"), data.get("stage"), data.get("state")))
            conn.execute("INSERT INTO batches VALUES(?,?,?)", (batch_id, batch_hash, len(parsed)))
        return {"batch_id": batch_id, "inserted": inserted, "replayed": False}
    finally:
        conn.close()


def protect_catalog_output(path, db):
    require(Path(path).resolve() not in {
        Path(str(db) + suffix).resolve() for suffix in ("", "-journal", "-wal", "-shm")},
        "output must not overwrite catalog or SQLite sidecars")


def ingest_cached_scans(db, index, scan_root, evidence_out, batch_id, source, cache_set):
    """Bounded index adapter: local stat is presence evidence, never checksum verification."""
    write_cached_scan_evidence(db, index, scan_root, evidence_out, batch_id, source, cache_set)
    return ingest(db, evidence_out)


def write_cached_scan_evidence(db, index, scan_root, evidence_out, batch_id, source, cache_set):
    """Build one bounded evidence file without importing it."""
    token(batch_id)
    token(source)
    require(cache_set in ("keep", "buffer"), "explicit cache_set keep or buffer required")
    root = Path(scan_root).resolve(strict=True)
    require(root.is_dir(), "scan root must be a directory")
    out = Path(evidence_out)
    protect_catalog_output(out, db)
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
            require(isinstance(entry["batch"], str) and
                    re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9_.-]{0,127}", entry["batch"]),
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


def write_atomic_evidence(evidence_out, db, events):
    require(0 < len(events) <= MAX_EVENTS, "batch exceeds 10000 observations")
    raw = "".join(events).encode()
    require(0 < len(raw) <= MAX_BATCH, "projected batch exceeds 16 MiB")
    out = Path(evidence_out)
    protect_catalog_output(out, db)
    require(not out.exists() and not out.is_symlink(), "evidence output must be a new file")
    with tempfile.NamedTemporaryFile(dir=out.parent, prefix="." + out.name + ".",
                                     suffix=".pending") as stream:
        os.chmod(stream.name, 0o600)
        stream.write(raw)
        stream.flush()
        os.fsync(stream.fileno())
        os.link(stream.name, out)
        directory = os.open(out.parent, os.O_RDONLY | os.O_DIRECTORY)
        try:
            os.fsync(directory)
        finally:
            os.close(directory)
    return out


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
    return ingest(db, write_atomic_evidence(evidence_out, db, events))


IMPORT_PART_RE = re.compile(
    r"pages/([A-Za-z0-9-]+_\d{4}-\d\d-\d\d_ed-[1-9]\d*)\.([A-Za-z0-9-]{1,32})\.(\d{8}T\d{12}Z)\.parquet\Z"
)
IMPORT_ALTO_RE = re.compile(
    r"alto/([A-Za-z0-9-]+_\d{4}-\d\d-\d\d_ed-[1-9]\d*_seq-[1-9]\d*)\.([A-Za-z0-9-]{1,32})\.(\d{8}T\d{12}Z)\.xml\Z"
)
IMPORT_RUNS_RE = re.compile(
    r"runs/([A-Za-z0-9-]+_\d{4}-\d\d-\d\d_ed-[1-9]\d*)\.([A-Za-z0-9-]{1,32})\.(\d{8}T\d{12}Z)\.parquet\Z"
)
HITS_RESPONSE_FIELDS = {
    "index_version", "synthetic", "place", "title", "total", "days", "items", "next_cursor",
}


def ingest_import(db, input_path, evidence_out, batch_id, source):
    """ocr-sync committed ImportRecord JSON -> structural imported page_state evidence (never acceptance)."""
    token(batch_id)
    token(source)
    with Path(input_path).open("rb") as stream:
        require(stat.S_ISREG(os.fstat(stream.fileno()).st_mode), "import input must be a regular file")
        raw = stream.read(MAX_BATCH + 1)
    require(0 < len(raw) <= MAX_BATCH, "import record exceeds 16 MiB")
    record = load_json(raw)
    require(isinstance(record, dict), "import record must be an object")
    obj({k: v for k, v in record.items() if k != "outcomes"},
        ("run", "at", "index_version", "sources", "pages"))
    import_id = token(record["run"])
    committed_at = timestamp(record["at"])
    token(record["index_version"])
    outcomes_ref = record.get("outcomes")
    if outcomes_ref is not None:
        safe_reference(outcomes_ref)
        require(re.fullmatch(
            r"ocr-local/outcomes/\d{4}-\d\d-\d\d/" + re.escape(import_id) + r"\.parquet",
            outcomes_ref) is not None, "invalid ImportRecord outcomes path")
    pages = record["pages"]
    require(isinstance(pages, list) and len(pages) <= MAX_EVENTS,
            "invalid ImportRecord pages list")
    page_lccns = set()
    page_path_re = re.compile(r"ocr-local/pages/([A-Za-z0-9-]+)/" + re.escape(import_id) + r"\.parquet\Z")
    for page_ref in pages:
        safe_reference(page_ref)
        m = page_path_re.fullmatch(page_ref)
        require(m is not None and m[1] not in page_lccns, "invalid or duplicate ImportRecord pages entry")
        page_lccns.add(m[1])
    sources = record["sources"]
    require(isinstance(sources, list) and 0 < len(sources) <= MAX_EVENTS,
            "ImportRecord requires 1..10000 sources")
    imported_parts, imported_altos, imported_order, imported_runs, seen_paths = {}, {}, [], set(), set()
    for entry in sources:
        require(isinstance(entry, dict), "ImportRecord source entry must be an object")
        obj({k: v for k, v in entry.items() if k != "reason"}, ("path", "etag", "outcome"))
        path = safe_reference(entry["path"])
        require(path not in seen_paths, "duplicate source path in ImportRecord")
        seen_paths.add(path)
        require(isinstance(entry["etag"], str) and bool(entry["etag"].strip()),
                "ImportRecord source etag cannot be blank")
        short_text(entry["etag"], 256)
        outcome = entry["outcome"]
        require(outcome in ("imported", "quarantined"), "invalid ImportRecord source outcome")
        if outcome == "quarantined":
            require(entry.get("reason") is not None, "quarantined source requires reason")
            token(entry["reason"])
            continue
        require(entry.get("reason") is None, "imported source must not have quarantine reason")
        part_m = IMPORT_PART_RE.fullmatch(path)
        alto_m = IMPORT_ALTO_RE.fullmatch(path)
        runs_m = IMPORT_RUNS_RE.fullmatch(path)
        if part_m is not None:
            issue, node, stamp = part_m.groups()
            require((node, stamp) not in imported_parts, "duplicate imported part for run")
            imported_parts[(node, stamp)] = issue
        elif alto_m is not None:
            doc_id, node, stamp = alto_m.groups()
            require((node, stamp) not in imported_altos, "duplicate imported ALTO for run")
            imported_altos[(node, stamp)] = doc_id
            imported_order.append((node, stamp))
        elif runs_m is not None:
            issue, node, stamp = runs_m.groups()
            require((issue, node, stamp) not in imported_runs, "duplicate imported runs record for run")
            imported_runs.add((issue, node, stamp))
        else:
            raise CatalogError("unrecognized imported source path in ImportRecord")
    require(bool(imported_order) and set(imported_parts) == set(imported_altos),
            "ImportRecord requires matching imported part and ALTO sources")
    for issue, node, stamp in imported_runs:
        require(imported_parts.get((node, stamp)) == issue,
                "imported runs source does not match imported part")
    imported_lccns = set()
    events = []
    for node, stamp in imported_order:
        issue = imported_parts[(node, stamp)]
        doc_id = token(imported_altos[(node, stamp)])
        require(doc_id.startswith(issue + "_seq-"),
                "imported ALTO doc_id does not match part issue")
        imported_lccns.add(issue.split("_", 1)[0])
        run_id = token(node + "." + stamp)
        payload = {"stage": "imported", "state": "present", "run_id": run_id,
                   "import_id": import_id, "committed_at": committed_at}
        event = {"schema_version": 1, "batch_id": batch_id,
                 "event_id": "import-" + digest([import_id, doc_id, run_id]),
                 "kind": "page_state", "source": source, "observed_at": committed_at,
                 "page_id": "loc:" + doc_id, "payload": payload}
        validate_event(event)
        require(len(canonical(event).encode()) <= MAX_LINE, "projected import event exceeds 64 KiB")
        events.append(canonical(event) + "\n")
    require(imported_lccns == page_lccns,
            "ImportRecord pages list does not match imported source LCCNs")
    require(outcomes_ref is not None, "ImportRecord with imported pages requires outcomes path")
    return ingest(db, write_atomic_evidence(evidence_out, db, events))


def ingest_release(db, input_path, evidence_out, batch_id, source):
    """Authoritative release selection/rejection/revocation decisions -> accepted-stage page_state evidence."""
    token(batch_id)
    token(source)
    with Path(input_path).open("rb") as stream:
        require(stat.S_ISREG(os.fstat(stream.fileno()).st_mode), "release input must be a regular file")
        raw = stream.read(MAX_BATCH + 1)
    require(0 < len(raw) <= MAX_BATCH, "release decision file exceeds 16 MiB")
    record = load_json(raw)
    require(isinstance(record, dict), "release record must be an object")
    obj(record, ("schema", "release_id", "decided_at", "decisions"))
    require(type(record["schema"]) is int and record["schema"] == 1, "unsupported release schema")
    release_id = token(record["release_id"])
    default_decided_at = timestamp(record["decided_at"])
    decisions = record["decisions"]
    require(isinstance(decisions, list) and 0 < len(decisions) <= MAX_EVENTS,
            "release record requires 1..10000 page decisions")
    events = []
    allowed_entry = {"doc_id", "page_id", "run_id", "import_id", "committed_at", "decided_at",
                     "decision", "decision_reason", "selected_source", "selected_engine"}
    for entry in decisions:
        require(isinstance(entry, dict) and set(entry) <= allowed_entry, "invalid release decision entry")
        for req in ("run_id", "decision", "decision_reason"):
            require(req in entry, f"release decision entry missing {req}")
        require(("doc_id" in entry) ^ ("page_id" in entry),
                "release decision entry requires exactly one of doc_id or page_id")
        page_id = entry["page_id"] if "page_id" in entry else "loc:" + token(entry["doc_id"])
        token(page_id)
        decision = entry["decision"]
        require(decision in DECISIONS, "invalid release decision")
        reason = token(entry["decision_reason"])
        decided_at = timestamp(entry["decided_at"]) if entry.get("decided_at") is not None else default_decided_at
        committed_at = timestamp(entry["committed_at"]) if entry.get("committed_at") is not None else None
        import_id = token(entry["import_id"]) if entry.get("import_id") is not None else None
        run_id = token(entry["run_id"]) if entry["run_id"] is not None else None
        if decision == "accepted":
            state = "present"
            require(run_id is not None, "accepted release decision requires run_id")
            selected_source = token(entry.get("selected_source"))
            require(selected_source != "loc", "accepted replacement decision requires non-loc selected_source")
            selected_engine = entry.get("selected_engine")
            require(isinstance(selected_engine, str) and selected_engine.strip(),
                    "accepted replacement decision requires selected_engine")
            short_text(selected_engine, 256)
        else:
            if decision == "revoked":
                require(run_id is not None, "revoked release decision requires run_id")
            state = "blocked" if decision == "revoked" else "missing"
            selected_source = token(entry.get("selected_source") or "loc")
            selected_engine = entry.get("selected_engine")
            require(selected_engine is None,
                    "non-accepted release decision must not claim a replacement selected_engine")
        payload = {"stage": "accepted", "state": state, "run_id": run_id,
                   "release_id": release_id, "import_id": import_id,
                   "decision": decision, "decision_reason": reason,
                   "selected_source": selected_source, "selected_engine": selected_engine,
                   "decided_at": decided_at, "committed_at": committed_at}
        event = {"schema_version": 1, "batch_id": batch_id,
                 "event_id": "release-" + digest([release_id, page_id, run_id, decision, decided_at]),
                 "kind": "page_state", "source": source, "observed_at": decided_at,
                 "page_id": page_id, "payload": payload}
        validate_event(event)
        require(len(canonical(event).encode()) <= MAX_LINE, "projected release event exceeds 64 KiB")
        events.append(canonical(event) + "\n")
    return ingest(db, write_atomic_evidence(evidence_out, db, events))


def ingest_search_provenance(db, input_path, evidence_out, batch_id, source, page_id, verified_at,
                             run_id=None, release_id=None, import_id=None, published_at=None):
    """Public /v1/hits JSON response -> indexed-stage search provenance without persisting text."""
    token(batch_id)
    token(source)
    token(page_id)
    require(re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9_.-]{0,63}:.+", page_id) is not None,
            "page_id must be source-qualified, e.g. loc:sn..._seq-1")
    verified = timestamp(verified_at)
    if run_id is not None:
        token(run_id)
    if release_id is not None:
        token(release_id)
    if import_id is not None:
        token(import_id)
    published = timestamp(published_at) if published_at is not None else None
    with Path(input_path).open("rb") as stream:
        require(stat.S_ISREG(os.fstat(stream.fileno()).st_mode), "search input must be a regular file")
        raw = stream.read(MAX_LINE + 1)
    require(0 < len(raw) <= MAX_LINE, "search response exceeds 64 KiB")
    response_sha256 = hashlib.sha256(raw).hexdigest()
    body = load_json(raw)
    require(isinstance(body, dict) and set(body) <= HITS_RESPONSE_FIELDS and
            "index_version" in body and isinstance(body.get("items"), list),
            "invalid /v1/hits search response")
    if "synthetic" in body:
        require(type(body["synthetic"]) is bool, "invalid /v1/hits synthetic flag")
        require(body["synthetic"] is False,
                "synthetic /v1/hits responses cannot record publication provenance")
    index_version = token(body["index_version"])
    doc_id = page_id.split(":", 1)[1]
    matches = []
    for hit in body["items"]:
        require(isinstance(hit, dict) and isinstance(hit.get("doc_id"), str), "invalid search hit")
        token(hit["doc_id"])
        if hit["doc_id"] == doc_id:
            matches.append(hit)
    require(len(matches) == 1, "/v1/hits response must contain exactly one matching item for page_id")
    hit = matches[0]
    hit_present = True
    ocr = hit.get("ocr")
    if ocr is not None:
        obj(ocr, ("source", "engine"))
        ocr_source = token(ocr["source"])
        require(ocr_source != "loc", "replacement ocr.source cannot be loc")
        require(isinstance(ocr["engine"], str) and ocr["engine"].strip(),
                "search hit ocr.engine cannot be blank")
        ocr_engine = short_text(ocr["engine"], 256)
        require(run_id is not None, "published replacement OCR provenance requires run_id")
        decision = "accepted"
        decision_reason = "published_replacement"
        state = "present"
        selected_source = ocr_source
        selected_engine = ocr_engine
    else:
        ocr_source = None
        ocr_engine = None
        decision = "unchanged_baseline"
        decision_reason = "unchanged_baseline"
        state = "missing"
        selected_source = "loc"
        selected_engine = None
    receipt = {"index_version": index_version, "hit_present": hit_present,
               "ocr_source": ocr_source, "ocr_engine": ocr_engine,
               "response_sha256": response_sha256, "verified_at": verified}
    payload = {"stage": "indexed", "state": state, "run_id": run_id,
               "release_id": release_id or index_version, "index_id": index_version,
               "import_id": import_id, "decision": decision,
               "decision_reason": decision_reason,
               "selected_source": selected_source, "selected_engine": selected_engine,
               "published_at": published, "verified_at": verified,
               "search_receipt": receipt}
    event = {"schema_version": 1, "batch_id": batch_id,
             "event_id": "search-" + digest([index_version, page_id, run_id, response_sha256, verified]),
             "kind": "page_state", "source": source, "observed_at": verified,
             "page_id": page_id, "payload": payload}
    validate_event(event)
    require(len(canonical(event).encode()) <= MAX_LINE, "projected search event exceeds 64 KiB")
    return ingest(db, write_atomic_evidence(evidence_out, db, [canonical(event) + "\n"]))


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


def trace(db, page_id, run_id=None):
    """Reconcile one page across artifact, execution, import, release decision and search provenance."""
    token(page_id)
    require(re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9_.-]{0,63}:.+", page_id) is not None,
            "page_id must be source-qualified, e.g. loc:sn..._seq-1")
    if run_id is not None:
        token(run_id)

    def read(conn):
        found = conn.execute("""SELECT latest.*,events.batch_id,events.line_number,
                             events.sha256 AS event_sha256 FROM latest
                             JOIN events USING(source,event_id)
                             WHERE page_id=? ORDER BY observed_at,event_id,key LIMIT ?""",
                             (page_id, MAX_LIMIT + 1)).fetchall()
        require(len(found) <= MAX_LIMIT, "page trace exceeds 100 latest observations")
        items = [{k: row[k] for k in ("key", "kind", "source", "page_id", "event_id", "observed_at",
                                      "batch_id", "line_number", "event_sha256")}
                 | {"data": load_json(row["data"])} for row in found]
        artifacts = [i for i in items if i["kind"] == "artifact"]
        artifact_states = {i["data"]["state"] for i in artifacts}
        artifact_state = ("verified" if "verified" in artifact_states else
                          "present" if "present" in artifact_states else
                          "missing" if "missing" in artifact_states else "unknown")

        resolved_run_id = run_id
        if resolved_run_id is None:
            all_accepted = [i for i in items if i["kind"] == "page_state" and i["data"].get("stage") == "accepted"]
            present_accepted = [i for i in all_accepted if i["data"].get("state") == "present"]
            all_imported = [i for i in items if i["kind"] == "page_state" and i["data"].get("stage") == "imported"]
            all_runs = [i for i in items if i["kind"] == "run"]
            all_executions = [i for i in items if i["kind"] == "execution"]
            all_indexed = [i for i in items if i["kind"] == "page_state" and i["data"].get("stage") == "indexed"]
            for group in (present_accepted, all_accepted, all_imported, all_runs, all_executions, all_indexed):
                for candidate in reversed(group):
                    if candidate["data"].get("run_id") is not None:
                        resolved_run_id = candidate["data"]["run_id"]
                        break
                if resolved_run_id is not None:
                    break

        executions = [i for i in items if i["kind"] == "execution" and
                      (resolved_run_id is None or i["data"].get("run_id") == resolved_run_id)]
        runs = [i for i in items if i["kind"] == "run" and
                (resolved_run_id is None or i["data"].get("run_id") == resolved_run_id)]

        def stage_items(stage_name, allow_unscoped_baseline=False):
            matched = []
            for item in items:
                if item["kind"] != "page_state" or item["data"].get("stage") != stage_name:
                    continue
                item_run = item["data"].get("run_id")
                is_baseline = allow_unscoped_baseline and (
                    item_run is None or item["data"].get("decision") == "unchanged_baseline"
                )
                if resolved_run_id is None or item_run == resolved_run_id or is_baseline:
                    matched.append(item)
            return matched

        imported_rows = stage_items("imported")
        accepted_rows = stage_items("accepted", allow_unscoped_baseline=True)
        indexed_rows = stage_items("indexed", allow_unscoped_baseline=True)

        if imported_rows:
            latest_imp = imported_rows[-1]
            imp_data = latest_imp["data"]
            imported_summary = {
                "state": imp_data["state"],
                "imported": imp_data["state"] == "present",
                "run_id": imp_data.get("run_id"),
                "import_id": imp_data.get("import_id"),
                "committed_at": imp_data.get("committed_at"),
                "observed_at": latest_imp["observed_at"],
                "source": latest_imp["source"],
                "items": imported_rows,
            }
        else:
            imported_summary = {
                "state": "unknown",
                "imported": False,
                "run_id": None,
                "import_id": None,
                "committed_at": None,
                "observed_at": None,
                "source": None,
                "items": [],
            }

        if accepted_rows:
            latest_acc = accepted_rows[-1]
            acc_data = latest_acc["data"]
            acc_decision = acc_data.get("decision") or (
                "accepted" if acc_data["state"] == "present" else
                "revoked" if acc_data["state"] == "blocked" else "rejected"
            )
            release_decision = {
                "state": acc_data["state"],
                "decision": acc_decision,
                "accepted": acc_data["state"] == "present" and acc_decision == "accepted",
                "release_id": acc_data.get("release_id"),
                "run_id": acc_data.get("run_id"),
                "import_id": acc_data.get("import_id"),
                "decision_reason": acc_data.get("decision_reason"),
                "selected_source": acc_data.get("selected_source"),
                "selected_engine": acc_data.get("selected_engine"),
                "decided_at": acc_data.get("decided_at"),
                "committed_at": acc_data.get("committed_at"),
                "observed_at": latest_acc["observed_at"],
                "source": latest_acc["source"],
                "items": accepted_rows,
            }
        else:
            release_decision = {
                "state": "unknown",
                "decision": "unknown",
                "accepted": False,
                "release_id": None,
                "run_id": None,
                "import_id": None,
                "decision_reason": None,
                "selected_source": None,
                "selected_engine": None,
                "decided_at": None,
                "committed_at": None,
                "observed_at": None,
                "source": None,
                "items": [],
            }

        if indexed_rows:
            latest_idx = indexed_rows[-1]
            idx_data = latest_idx["data"]
            receipt = idx_data.get("search_receipt")
            idx_decision = idx_data.get("decision") or (
                "accepted" if idx_data["state"] == "present" else
                "revoked" if idx_data["state"] == "blocked" else "unchanged_baseline"
            )
            search_provenance = {
                "state": idx_data["state"],
                "decision": idx_decision,
                "published": idx_data["state"] == "present" and idx_decision == "accepted",
                "release_id": idx_data.get("release_id"),
                "index_id": idx_data.get("index_id") or (receipt or {}).get("index_version"),
                "run_id": idx_data.get("run_id"),
                "import_id": idx_data.get("import_id"),
                "decision_reason": idx_data.get("decision_reason"),
                "selected_source": idx_data.get("selected_source"),
                "selected_engine": idx_data.get("selected_engine"),
                "published_at": idx_data.get("published_at"),
                "verified_at": idx_data.get("verified_at") or (receipt or {}).get("verified_at"),
                "search_receipt": receipt,
                "observed_at": latest_idx["observed_at"],
                "source": latest_idx["source"],
                "items": indexed_rows,
            }
        else:
            search_provenance = {
                "state": "unknown",
                "decision": "unknown",
                "published": False,
                "release_id": None,
                "index_id": None,
                "run_id": None,
                "import_id": None,
                "decision_reason": None,
                "selected_source": None,
                "selected_engine": None,
                "published_at": None,
                "verified_at": None,
                "search_receipt": None,
                "observed_at": None,
                "source": None,
                "items": [],
            }

        def release_matches_index(rel_id, idx_id):
            return (isinstance(rel_id, str) and isinstance(idx_id, str) and
                    (idx_id == rel_id or idx_id == "pages-" + rel_id or rel_id == "pages-" + idx_id))

        receipt = search_provenance["search_receipt"]
        present_artifact_shas = {
            a["data"].get("sha256")
            for a in artifacts
            if a["data"].get("state") in ("present", "verified") and
            isinstance(a["data"].get("sha256"), str)
        }
        rel_family = source_family(release_decision["selected_source"])
        idx_family = source_family(search_provenance["selected_source"])
        rec_family = source_family(receipt.get("ocr_source")) if isinstance(receipt, dict) else None
        rel_engine = (canonical_engine_key(release_decision["selected_engine"], family=rel_family)
                      if isinstance(release_decision["selected_engine"], str) and
                      release_decision["selected_engine"].strip() else None)
        idx_engine = (canonical_engine_key(search_provenance["selected_engine"], family=idx_family)
                      if isinstance(search_provenance["selected_engine"], str) and
                      search_provenance["selected_engine"].strip() else None)
        rec_engine = (canonical_engine_key(receipt["ocr_engine"], family=rec_family or idx_family)
                      if isinstance(receipt, dict) and isinstance(receipt.get("ocr_engine"), str) and
                      receipt.get("ocr_engine").strip() else None)

        def run_matches_proof(r_item):
            r_data = r_item["data"]
            if (r_data.get("run_id") != resolved_run_id or
                    r_data.get("status") != "ok" or
                    r_data.get("selected") is not True):
                return False
            img_sha = r_data.get("image_sha256")
            if not isinstance(img_sha, str) or img_sha not in present_artifact_shas:
                return False
            if not any(
                e["data"].get("run_id") == resolved_run_id and
                e["data"].get("status") == "ok" and
                any(obs.get("status") == "ok" and obs.get("image_sha256") == img_sha
                    for obs in e["data"].get("observations", []))
                for e in executions
            ):
                return False
            fam = r_data.get("family")
            r_engine_raw = r_data.get("selected_engine")
            if isinstance(r_engine_raw, str) and r_engine_raw.strip():
                r_engine = canonical_engine_key(r_engine_raw, family=fam)
            else:
                ver, prof = r_data.get("engine_version"), r_data.get("profile")
                if not all(isinstance(x, str) and x for x in (fam, ver, prof)):
                    return False
                r_engine = f"{fam}/{ver} {prof}"
            if rel_engine is None or r_engine != rel_engine:
                return False
            fam = r_data.get("family")
            sel_src = release_decision["selected_source"]
            if isinstance(fam, str) and isinstance(sel_src, str) and sel_src.startswith("usnm-"):
                if sel_src != "usnm-" + fam:
                    return False
            return True

        has_matching_execution_and_run = any(run_matches_proof(r) for r in runs)
        has_matching_import = (
            imported_summary["imported"] is True and
            imported_summary["run_id"] == resolved_run_id and
            imported_summary["import_id"] is not None and
            imported_summary["committed_at"] is not None
        )
        has_matching_acceptance = (
            release_decision["accepted"] is True and
            release_decision["run_id"] == resolved_run_id and
            release_decision["release_id"] is not None and
            release_decision["import_id"] in (None, imported_summary["import_id"]) and
            release_decision["selected_source"] not in (None, "loc") and
            rel_engine is not None
        )
        has_matching_publication = (
            search_provenance["published"] is True and
            search_provenance["run_id"] == resolved_run_id and
            search_provenance["import_id"] in (None, imported_summary["import_id"]) and
            release_matches_index(release_decision["release_id"], search_provenance["release_id"]) and
            release_matches_index(release_decision["release_id"], search_provenance["index_id"]) and
            release_matches_index(search_provenance["release_id"], search_provenance["index_id"]) and
            isinstance(receipt, dict) and
            receipt.get("hit_present") is True and
            receipt.get("index_version") == search_provenance["index_id"] and
            receipt.get("ocr_source") not in (None, "loc") and
            receipt.get("ocr_source") == release_decision["selected_source"] == search_provenance["selected_source"] and
            rel_engine is not None and
            rel_engine == idx_engine == rec_engine
        )
        publication_proof_complete = bool(
            resolved_run_id is not None and
            has_matching_execution_and_run and
            has_matching_import and
            has_matching_acceptance and
            has_matching_publication
        )

        return {
            "page_id": page_id,
            "run_id": resolved_run_id,
            "accepted": release_decision["accepted"],
            "published": search_provenance["published"],
            "publication_proof_complete": publication_proof_complete,
            "artifact": {"state": artifact_state, "items": artifacts},
            "execution": {"state": "observed" if (executions or runs) else "unknown",
                          "executions": executions, "runs": runs},
            "imported": imported_summary,
            "release_decision": release_decision,
            "search_provenance": search_provenance,
        }

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
                stage_id = stage_id[:191] + "@" + digest([row["source"], stage_id])
            require(stage_id not in {stage["id"] for stage in stages},
                    "aggregate exported stage identity collision")
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
                    (SELECT DISTINCT source,run_id,page_id FROM latest WHERE kind IN ('run','execution'))"""),
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
    protect_catalog_output(path, db)
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
                if uri.path == "/trace":
                    require("page_id" in params and set(params) <= {"page_id", "run_id"},
                            "trace accepts page_id and optional run_id")
                    self.send_json(200, trace(db, params["page_id"], params.get("run_id")))
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
bounded local cached-scan index/stat, committed ocr-sync ImportRecord JSON
(ingest-import), authoritative release selection/rejection/revocation decisions
(ingest-release), public /v1/hits search provenance receipts (ingest-search),
page provenance reconciliation (trace / query trace / GET /trace) and explicit
artifact/page-state/stage evidence.
NOT provided: Parquet/Blob inventory or queue/outcomes Parquet, ocr-local/pages import
ledger, BOINC allocation journal, or live Quickwit index pointer mutation. Export those
authoritative sources independently; do not label a local catalog ingest as page import.
Missing adapters/sources produce unknown coverage (no fabricated BOINC-ready counts).
Mere worker ok/selected or imported status NEVER implies accepted or published search
inclusion. Release decisions keep accepted, rejected, revoked and unchanged_baseline
outcomes explicit alongside release_id, run_id, import_id, selected_source,
selected_engine, decision_reason, original timestamps and /v1/hits search_receipt.
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
required for replay/rebuild. For larger indexes, scripts/corpus/scan_cache_inventory.py creates
bounded durable slices/evidence with resumable checkpoints and partial coverage reports.
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
    p = subs.add_parser("ingest-import", help="import committed ocr-sync ImportRecord JSON as imported stage")
    p.add_argument("--input", required=True)
    p.add_argument("--evidence-out", required=True)
    p.add_argument("--batch-id", required=True)
    p.add_argument("--source", required=True)
    p = subs.add_parser("ingest-release", help="import authoritative release selection/rejection/revocation JSON")
    p.add_argument("--input", required=True)
    p.add_argument("--evidence-out", required=True)
    p.add_argument("--batch-id", required=True)
    p.add_argument("--source", required=True)
    p = subs.add_parser("ingest-search", help="import public /v1/hits JSON response as indexed search provenance")
    p.add_argument("--input", required=True)
    p.add_argument("--evidence-out", required=True)
    p.add_argument("--batch-id", required=True)
    p.add_argument("--source", required=True)
    p.add_argument("--page-id", required=True)
    p.add_argument("--verified-at", required=True)
    p.add_argument("--run-id")
    p.add_argument("--release-id")
    p.add_argument("--import-id")
    p.add_argument("--published-at")
    p = subs.add_parser("trace", help="reconcile page artifact, execution, import, release decision and search provenance")
    p.add_argument("--page-id", required=True)
    p.add_argument("--run-id")
    subs.add_parser("init", help="create an empty local catalog; snapshot coverage remains unknown")
    p = subs.add_parser("query", help="bounded page detail or filtered metadata (keyset pagination)")
    p.add_argument("kind", choices=("page", "trace", "run", "execution", "artifact", "page_state", "stage", "missing"))
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
        elif args.command == "ingest-import":
            result = ingest_import(args.db, args.input, args.evidence_out, args.batch_id, args.source)
        elif args.command == "ingest-release":
            result = ingest_release(args.db, args.input, args.evidence_out, args.batch_id, args.source)
        elif args.command == "ingest-search":
            result = ingest_search_provenance(
                args.db, args.input, args.evidence_out, args.batch_id, args.source,
                args.page_id, args.verified_at, args.run_id, args.release_id,
                args.import_id, args.published_at)
        elif args.command == "trace":
            result = trace(args.db, args.page_id, args.run_id)
        elif args.command == "query":
            filters = {key: getattr(args, key) for key in
                       ("page_id", "source", "engine", "status", "run_id") if getattr(args, key)}
            if args.kind == "trace":
                require("page_id" in filters and set(filters) <= {"page_id", "run_id"},
                        "trace requires --page-id and optional --run-id")
                result = trace(args.db, args.page_id, args.run_id)
            elif args.kind in ("page", "missing"):
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
