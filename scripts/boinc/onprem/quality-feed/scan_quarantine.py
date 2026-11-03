"""Durable, source-fingerprint-specific decode failures for operator review."""

from contextlib import contextmanager
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
import urllib.parse

DB_NAME = "decode-quarantine.sqlite3"
LOCK_NAME = ".decode-quarantine.lock"
FAILURES_BEFORE_QUARANTINE = 3
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
                    # Reserved manifest columns allow a future, separately reviewed recovery to
                    # read these quarantine records without changing the existing database.
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


@contextmanager
def publication_lock(root, *, create=False):
    root = Path(root)
    if create:
        root.mkdir(parents=True, exist_ok=True)
    if not stat.S_ISDIR(root.stat().st_mode):
        raise ValueError("scan-quarantine root is not a directory")
    with (root / LOCK_NAME).open("a") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        try:
            yield
        finally:
            fcntl.flock(lock, fcntl.LOCK_UN)


def record_failure(state, page, error, decoder):
    """Count verified decoder failures and quarantine repeated failures for review."""
    source, scan, lang, rate = _page_details(page)
    identity = source_id(source)
    encoded = json.dumps(source, sort_keys=True, separators=(",", ":"))
    with publication_lock(Path(state).parent, create=True):
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
                    UPDATE failures SET refetch_state = 'manual_review'
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
    states = {"observing", "manual_review"}
    attempts, state = record["attempts"], record["refetch_state"]
    if (source != record["source"] or source_id(source) != record["id"]
            or type(attempts) is not int or attempts < 0 or state not in states
            or not isinstance(record["decoder"], dict)
            or (state == "observing") != (attempts < FAILURES_BEFORE_QUARANTINE)):
        raise ValueError("invalid scan-quarantine record")
    if any(record[field] is not None for field in ("manifest_name", "manifest_sha256", "manifest_body")):
        raise ValueError("unexpected recovery data in scan-quarantine record")
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
