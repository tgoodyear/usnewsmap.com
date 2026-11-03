"""Durable, source-fingerprint-specific decode failures and paced repair requests."""

import datetime as dt
import fcntl
import hashlib
import json
import math
import os
from pathlib import Path
import re
import sqlite3
import stat
import urllib.error
import urllib.parse
import urllib.request

DB_NAME = "decode-quarantine.sqlite3"
FAILURES_BEFORE_QUARANTINE = 3
MAX_PENDING_REFETCHES = 10
USER_AGENT = "usnewsmap-ocr-manifest/1 (+https://usnewsmap.com)"
CAMPAIGN_RE = re.compile(r"[a-z0-9][a-z0-9-]{0,40}")
DOC_RE = re.compile(r"(?P<lccn>[a-z0-9]{1,16})_(?P<date>\d{4}-\d{2}-\d{2})_ed-(?P<ed>\d+)_seq-(?P<seq>\d+)")


def fingerprint(page):
    doc, batch, sha = page["doc_id"], page["batch"], page["sha256"]
    match = DOC_RE.fullmatch(doc) if isinstance(doc, str) else None
    version, size = page.get("batch_version") or 1, page["bytes"]
    if (not match or not isinstance(batch, str) or not re.fullmatch(r"[A-Za-z0-9_]{1,128}", batch)
            or type(version) is not int or not 1 <= version <= 32767
            or type(size) is not int or size < 1
            or not isinstance(sha, str) or not re.fullmatch(r"[a-f0-9]{64}", sha)):
        raise ValueError("invalid scan fingerprint")
    date = dt.date.fromisoformat(match["date"])
    edition, seq = int(match["ed"]), int(match["seq"])
    if (not 1 <= edition <= 32767 or not 1 <= seq <= 32767
            or doc != f"{match['lccn']}_{date.isoformat()}_ed-{edition}_seq-{seq}"):
        raise ValueError("invalid scan page identity")
    return {"doc_id": doc, "batch": batch, "batch_version": version, "bytes": size, "sha256": sha}


def source_id(page):
    raw = json.dumps(fingerprint(page), sort_keys=True, separators=(",", ":")).encode()
    return hashlib.sha256(raw).hexdigest()


def scan_paths(source):
    match = DOC_RE.fullmatch(source["doc_id"])
    stem = f"{source['batch']}/{match['lccn']}/{match['date']}/{int(match['ed'])}/{int(match['seq'])}"
    return {f"{storage}/{stem}.{ext}" for storage in ("keep", "buffer") for ext in ("jpg", "jp2")}


def _database(state, create=False, writable=False):
    path = Path(state) / DB_NAME
    if create:
        path.parent.mkdir(parents=True, exist_ok=True)
        try:
            fd = os.open(path, os.O_CREAT | os.O_EXCL | os.O_RDWR, 0o600)
        except FileExistsError:
            pass
        else:
            os.close(fd)
        connection = sqlite3.connect(path, timeout=30)
        try:
            with connection:
                connection.execute("BEGIN IMMEDIATE")
                version = connection.execute("PRAGMA user_version").fetchone()[0]
                if version == 0:
                    connection.execute("""
                        CREATE TABLE failures (
                            id TEXT PRIMARY KEY, source TEXT NOT NULL, scan TEXT NOT NULL,
                            lang TEXT NOT NULL, damage_rate REAL, attempts INTEGER NOT NULL,
                            first_at TEXT NOT NULL, last_at TEXT NOT NULL,
                            error_type TEXT NOT NULL, error TEXT NOT NULL, decoder TEXT NOT NULL,
                            refetch_state TEXT NOT NULL, manifest_name TEXT,
                            manifest_sha256 TEXT, manifest_body BLOB
                        )
                    """)
                    connection.execute("PRAGMA user_version = 1")
                elif version != 1:
                    raise ValueError("unknown scan-quarantine database version")
        except Exception:
            connection.close()
            raise
    else:
        try:
            path.stat()
        except FileNotFoundError:
            return None
        uri = "file:" + urllib.parse.quote(str(path.resolve())) + ("?mode=rw" if writable else "?mode=ro")
        connection = sqlite3.connect(uri, uri=True, timeout=30)
        if connection.execute("PRAGMA user_version").fetchone()[0] != 1:
            connection.close()
            raise ValueError("unknown scan-quarantine database version")
    connection.row_factory = sqlite3.Row
    return connection


def _now():
    return dt.datetime.now(dt.timezone.utc).isoformat()


def _page_details(page):
    source = fingerprint(page)
    scan, lang, rate = page["scan"], page["lang"], page.get("damage_rate")
    if (scan not in scan_paths(source) or not isinstance(lang, str)
            or not re.fullmatch(r"[a-z]{3}", lang)
            or (rate is not None and (isinstance(rate, bool) or not isinstance(rate, (int, float))
                                      or not math.isfinite(rate)))):
        raise ValueError("invalid scan-quarantine page metadata")
    return source, scan, lang, rate


def record_failure(state, page, error, decoder):
    """Count only a verified fingerprint's known decoder failures, across restarts."""
    source, scan, lang, rate = _page_details(page)
    identity = source_id(source)
    encoded = json.dumps(source, sort_keys=True, separators=(",", ":"))
    connection = _database(state, create=True)
    try:
        with connection:
            connection.execute("BEGIN IMMEDIATE")
            previous = connection.execute("SELECT source FROM failures WHERE id = ?", (identity,)).fetchone()
            if previous is not None and previous["source"] != encoded:
                raise ValueError("scan-quarantine fingerprint collision")
            now = _now()
            connection.execute("""
                INSERT INTO failures
                  (id, source, scan, lang, damage_rate, attempts, first_at, last_at,
                   error_type, error, decoder, refetch_state)
                VALUES (?, ?, ?, ?, ?, 1, ?, ?, ?, ?, ?, 'observing')
                ON CONFLICT(id) DO UPDATE SET
                    attempts = failures.attempts + 1, last_at = excluded.last_at,
                    error_type = excluded.error_type, error = excluded.error, decoder = excluded.decoder
            """, (identity, encoded, scan, lang, rate, now, now, error.original_type,
                  str(error)[:500], json.dumps(decoder, sort_keys=True)))
            connection.execute("""
                UPDATE failures SET refetch_state = 'pending'
                WHERE id = ? AND attempts >= ? AND refetch_state = 'observing'
            """, (identity, FAILURES_BEFORE_QUARANTINE))
            return connection.execute("SELECT attempts FROM failures WHERE id = ?", (identity,)).fetchone()[0]
    finally:
        connection.close()


def record_retryable(state, page, error):
    """Keep transient/unknown failures eligible without counting them as decoder failures."""
    source, scan, lang, rate = _page_details(page)
    identity = source_id(source)
    encoded = json.dumps(source, sort_keys=True, separators=(",", ":"))
    connection = _database(state, create=True)
    try:
        with connection:
            connection.execute("BEGIN IMMEDIATE")
            previous = connection.execute("SELECT source FROM failures WHERE id = ?", (identity,)).fetchone()
            if previous is not None and previous["source"] != encoded:
                raise ValueError("scan-quarantine fingerprint collision")
            now = _now()
            connection.execute("""
                INSERT INTO failures
                  (id, source, scan, lang, damage_rate, attempts, first_at, last_at,
                   error_type, error, decoder, refetch_state)
                VALUES (?, ?, ?, ?, ?, 0, ?, ?, ?, ?, '{}', 'observing')
                ON CONFLICT(id) DO UPDATE SET last_at = excluded.last_at,
                    error_type = excluded.error_type, error = excluded.error
                WHERE failures.refetch_state = 'observing'
            """, (identity, encoded, scan, lang, rate, now, now, type(error).__name__, str(error)[:500]))
    finally:
        connection.close()


def record_success(state, page):
    """A successful preparation clears earlier failures, but cannot clear a quarantine."""
    connection = _database(state, writable=True)
    if connection is None:
        return
    try:
        with connection:
            connection.execute(
                "DELETE FROM failures WHERE id = ? AND refetch_state = 'observing'", (source_id(page),))
    finally:
        connection.close()


def _record(row):
    record = dict(row)
    record["source"] = json.loads(record["source"])
    record["decoder"] = json.loads(record["decoder"])
    source, _, _, _ = _page_details({**record["source"], "scan": record["scan"],
                                     "lang": record["lang"], "damage_rate": record["damage_rate"]})
    states = {"observing", "pending", "prepared", "queued", "observed_same", "observed_new",
              "blocked_keep", "blocked_language"}
    attempts, state = record["attempts"], record["refetch_state"]
    if (source != record["source"] or source_id(source) != record["id"]
            or type(attempts) is not int or attempts < 0 or state not in states
            or not isinstance(record["decoder"], dict)
            or (state == "observing") != (attempts < FAILURES_BEFORE_QUARANTINE)):
        raise ValueError("invalid scan-quarantine record")
    name, digest, body = record["manifest_name"], record["manifest_sha256"], record["manifest_body"]
    if any(value is not None for value in (name, digest, body)):
        if (name != _manifest_name(record) or not isinstance(body, bytes)
                or hashlib.sha256(body).hexdigest() != digest):
            raise ValueError("invalid recorded repair manifest")
    elif state in ("prepared", "queued"):
        raise ValueError("missing recorded repair manifest")
    return record


def quarantined(state):
    connection = _database(state)
    if connection is None:
        return []
    try:
        rows = connection.execute("SELECT * FROM failures ORDER BY first_at, id").fetchall()
        records = [_record(row) for row in rows]
        return [record for record in records if record["refetch_state"] != "observing"]
    finally:
        connection.close()


def quarantined_ids(state):
    return {record["id"] for record in quarantined(state)}


def retryable_ids(state):
    connection = _database(state)
    if connection is None:
        return set()
    try:
        rows = connection.execute("SELECT * FROM failures").fetchall()
        records = [_record(row) for row in rows]
        return {record["id"] for record in records if record["refetch_state"] == "observing"}
    finally:
        connection.close()


def _update(state, identity, old, new, **values):
    connection = _database(state, create=True)
    try:
        with connection:
            assignments = ["refetch_state = ?"] + [f"{key} = ?" for key in values]
            args = [new, *values.values(), identity, old]
            changed = connection.execute(
                f"UPDATE failures SET {', '.join(assignments)} WHERE id = ? AND refetch_state = ?", args).rowcount
            if changed != 1:
                raise RuntimeError("scan-quarantine refetch state changed concurrently")
    finally:
        connection.close()


def _get_optional(url, max_bytes):
    request = urllib.request.Request(url, headers={"User-Agent": USER_AGENT})
    try:
        with urllib.request.urlopen(request, timeout=30) as response:
            data = response.read(max_bytes + 1)
            if len(data) > max_bytes:
                raise ValueError("scan-server response exceeds the repair check's bound")
            return data
    except urllib.error.HTTPError as error:
        if error.code == 404:
            error.close()
            return None
        raise


def _scan_exists(url):
    request = urllib.request.Request(url, headers={"User-Agent": USER_AGENT})
    try:
        with urllib.request.urlopen(request, timeout=30):
            return True
    except urllib.error.HTTPError as error:
        if error.code == 404:
            error.close()
            return False
        raise


def _keep_state(server, source):
    """Observe both possible formats without changing a retained scan or its sidecar."""
    match = DOC_RE.fullmatch(source["doc_id"])
    stem = (f"{server.rstrip('/')}/scans/keep/{source['batch']}/{match['lccn']}/"
            f"{match['date']}/{int(match['ed'])}/{int(match['seq'])}")
    found = []
    occupied = False
    for ext, fmt in (("jp2", "jp2"), ("jpg", "jpeg")):
        url = f"{stem}.{ext}"
        sidecar = _get_optional(url + ".json", 65536)
        scan_exists = _scan_exists(url)
        if sidecar is None:
            occupied |= scan_exists
            continue
        if not scan_exists:
            occupied = True
            continue
        try:
            side = json.loads(sidecar)
            valid = (isinstance(side, dict)
                     and side.get("doc_id") == source["doc_id"] and side.get("batch") == source["batch"]
                     and int(side.get("batch_version") or 1) == source["batch_version"]
                     and side.get("resolution") == "full" and side.get("format", "jpeg") == fmt
                     and type(side.get("bytes")) is int and side["bytes"] > 0
                     and isinstance(side.get("sha256"), str)
                     and re.fullmatch(r"[a-f0-9]{64}", side["sha256"]))
        except (TypeError, ValueError, OverflowError):
            valid = False
        if not valid:
            occupied = True
            continue
        found.append((side["bytes"], side["sha256"]))
    if occupied or len(set(found)) > 1:
        return "occupied"
    if not found:
        return "absent"
    return "same" if found[0] == (source["bytes"], source["sha256"]) else "new"


def _manifest_name(record):
    value = {"source": record["source"], "lang": record["lang"], "damage_rate": record["damage_rate"]}
    digest = hashlib.sha256(json.dumps(value, sort_keys=True, separators=(",", ":"),
                                       allow_nan=False).encode()).hexdigest()
    return "queue-quality-zdecode-" + digest


def _put_manifest(server, record):
    body = record["manifest_body"]
    if (record["manifest_name"] != _manifest_name(record) or not isinstance(body, bytes)
            or hashlib.sha256(body).hexdigest() != record["manifest_sha256"]):
        raise ValueError("prepared repair manifest failed its identity or checksum")
    url = f"{server.rstrip('/')}/manifests/{record['manifest_name']}/manifest.parquet"
    request = urllib.request.Request(url, data=body, method="PUT",
                                     headers={"User-Agent": USER_AGENT,
                                              "Content-Type": "application/octet-stream"})
    with urllib.request.urlopen(request, timeout=120) as response:
        if response.status not in (200, 201):
            raise RuntimeError(f"scan server refused repair manifest: HTTP {response.status}")


def _entries(root):
    for state in sorted(root.iterdir()):
        if not CAMPAIGN_RE.fullmatch(state.name) or not stat.S_ISDIR(state.lstat().st_mode):
            continue
        for record in quarantined(state):
            yield state, record


def quarantined_documents(root):
    root = Path(root)
    if not stat.S_ISDIR(root.stat().st_mode):
        raise ValueError("scan-quarantine root is not a directory")
    return {record["source"]["doc_id"] for _, record in _entries(root)}


def _log(message, state, record, **extra):
    print(json.dumps({"msg": message, "campaign": state.name, "doc_id": record["source"]["doc_id"],
                      "source_sha256": record["source"]["sha256"], **extra}, sort_keys=True), flush=True)


def reconcile(root, server, make_manifest):
    """Queue bounded non-English buffer repairs through the existing scan-fetch queue."""
    root = Path(root)
    if not stat.S_ISDIR(root.stat().st_mode):
        raise ValueError("scan-quarantine root is not a directory")
    with (root / ".decode-refetch.lock").open("a") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        entries = list(_entries(root))
        for state, record in entries:
            if record["refetch_state"] != "queued":
                continue
            observed = _keep_state(server, record["source"])
            if observed in ("same", "new"):
                new = {"same": "observed_same", "new": "observed_new"}[observed]
                _update(state, record["id"], "queued", new)
                _log("repair keep scan observed", state, record, result=new)
            elif observed == "occupied":
                _log("repair keep scan incomplete or conflicting; will check again", state, record)
        entries = list(_entries(root))
        for state, record in entries:
            if record["refetch_state"] == "prepared":
                _put_manifest(server, record)
                _update(state, record["id"], "prepared", "queued")
                _log("repair manifest queued", state, record, manifest=record["manifest_name"])
        entries = list(_entries(root))
        in_flight = sum(record["refetch_state"] in ("prepared", "queued") for _, record in entries)
        for state, record in entries:
            if record["refetch_state"] != "pending":
                continue
            if record["lang"] == "eng":
                _update(state, record["id"], "pending", "blocked_language")
                _log("repair needs operator review", state, record, reason="English buffer source")
                continue
            if not record["scan"].startswith("buffer/"):
                _update(state, record["id"], "pending", "blocked_keep")
                _log("repair needs operator review", state, record, reason="source is a retained keep scan")
                continue
            if in_flight >= MAX_PENDING_REFETCHES:
                _log("repair waiting for outstanding manifests", state, record, in_flight=in_flight)
                break
            observed = _keep_state(server, record["source"])
            if observed != "absent":
                new = "observed_new" if observed == "new" else "blocked_keep"
                _update(state, record["id"], "pending", new)
                _log("repair needs no new manifest" if observed == "new" else "repair needs operator review",
                     state, record, result=new)
                continue
            body = make_manifest(record)
            if not isinstance(body, bytes) or not body:
                raise ValueError("repair manifest encoder returned no bytes")
            name, digest = _manifest_name(record), hashlib.sha256(body).hexdigest()
            _update(state, record["id"], "pending", "prepared", manifest_name=name,
                    manifest_sha256=digest, manifest_body=body)
            prepared = {**record, "manifest_name": name, "manifest_sha256": digest, "manifest_body": body}
            _put_manifest(server, prepared)
            _update(state, record["id"], "prepared", "queued")
            _log("repair manifest queued", state, record, manifest=name)
            in_flight += 1
