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
            re.fullmatch(r"\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d(?:\.\d{1,6})?(?:Z|[+-]\d\d:\d\d)", value),
            "expected RFC3339 timestamp with timezone")
    try:
        parsed = dt.datetime.fromisoformat(value.replace("Z", "+00:00"))
        return parsed.astimezone(dt.timezone.utc).isoformat(timespec="microseconds").replace("+00:00", "Z")
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

    try:
        return json.loads(raw, object_pairs_hook=pairs,
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
        obj(p, ("artifact_id", "sha256", "version", "location", "reference", "state"))
        for key in ("artifact_id", "version", "location"):
            token(p[key])
        sha(p["sha256"])
        safe_reference(p["reference"])
        require(p["state"] in ("present", "verified", "missing"), "invalid artifact state")
        identity = [event["page_id"], p["artifact_id"], p["version"], p["location"]]
        invariant = digest([p["sha256"], p["reference"]])
        data = dict(p)
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
            conn.execute("PRAGMA journal_mode=WAL")
            conn.execute("PRAGMA synchronous=FULL")
        if not writable:
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
                    status=excluded.status,state=excluded.state""",
                             (event["key"], event["invariant"], event["kind"], event["source"],
                              event["page_id"], event["event_id"], event["observed_at"], canonical(data),
                              data.get("engine_id"), data.get("status"), data.get("run_id"),
                              data.get("stage"), data.get("state")))
            conn.execute("INSERT INTO batches VALUES(?,?,?)", (batch_id, batch_hash, len(parsed)))
        return {"batch_id": batch_id, "inserted": inserted, "replayed": False}
    finally:
        conn.close()


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
    require(kind in ("run", "artifact", "page_state", "stage"), "unsupported query kind")
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
                total = count("SELECT count(*) FROM latest WHERE kind=?", (kind,))
                n = count("SELECT count(DISTINCT page_id) FROM latest WHERE kind=?", (kind,))
                as_of = count("""SELECT min(t) FROM
                    (SELECT max(observed_at) t FROM latest WHERE kind=? GROUP BY source)""", (kind,))
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
        return {"schema_version": 1, "observed_at": observed, "generated_at": now(),
                "producer": "corpus-catalog", "coverage": coverage, "stages": stages,
                "runs": {"total": count("""SELECT count(*) FROM
                    (SELECT DISTINCT source,run_id FROM latest WHERE kind='run')"""),
                         "pages": count("SELECT count(DISTINCT page_id) FROM latest WHERE kind='run'")},
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
                allowed = {"limit", "after"}
                if uri.path == "/runs":
                    allowed |= {"page_id", "source", "engine", "status", "run_id"}
                elif uri.path == "/page":
                    allowed |= {"page_id"}
                else:
                    self.send_json(404, {"error": "unknown endpoint"})
                    return
                require(set(params) <= allowed, "unsupported query parameter")
                limit = int(params.pop("limit", "50"))
                after = params.pop("after", "")
                if uri.path == "/runs":
                    result = query(db, "run", params, limit, after)
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
RunRecord must have exact v1 fields and full SHA-256 hashes. Free-form JSON, image_ref,
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

Adapters provided: RunRecord JSON projection and explicit artifact/page-state evidence.
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
Do not expose it through an unauthenticated proxy. Local files/exports are trusted
operator inputs; use filesystem access controls, never include text or credentials
in identifiers or references. Retain evidence on durable storage; SQLite is disposable.
"""


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__, epilog=HELP,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--db", required=True, help="local SQLite read model path")
    subs = parser.add_subparsers(dest="command", required=True)
    p = subs.add_parser("ingest", help="transactionally import one immutable evidence JSONL batch")
    p.add_argument("batch")
    subs.add_parser("init", help="create an empty local catalog; snapshot coverage remains unknown")
    p = subs.add_parser("query", help="bounded page detail or filtered metadata (keyset pagination)")
    p.add_argument("kind", choices=("page", "run", "artifact", "page_state", "stage", "missing"))
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
