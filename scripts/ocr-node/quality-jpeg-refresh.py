#!/usr/bin/env python3
"""Replenish two recipe-specific JPEG converter lists from an append-only scan index.

Run on the scan host against local, owner-refreshed manifest/damage/status inputs.
No archive walk, LoC request, upload, converter restart or deletion is performed.
"""

import argparse
from contextlib import contextmanager
import fcntl
import hashlib
import importlib.util
import itertools
import json
import os
from pathlib import Path
import re
import sqlite3
import sys


HERE = Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location("jpeg_list", HERE / "quality-jpeg-list.py")
ordering = importlib.util.module_from_spec(spec)
spec.loader.exec_module(ordering)
DOC = re.compile(r"([a-z0-9]+)_(\d{4}-\d\d-\d\d)_ed-([1-9]\d*)_seq-([1-9]\d*)\Z")
BATCH = re.compile(r"[A-Za-z0-9][A-Za-z0-9_-]{0,255}\Z")
LINE_BYTES = 65536
MAX_BATCHES = 50000


@contextmanager
def locked(work):
    work.mkdir(parents=True, exist_ok=True)
    with (work / "refresh.lock").open("a") as stream:
        try:
            fcntl.flock(stream, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            raise ValueError("another JPEG refresh owns this state") from None
        yield


def connect(work, recipe, hosts):
    db = sqlite3.connect(work / "refresh.sqlite")
    db.executescript("""
        PRAGMA cache_size=-4096;
        PRAGMA synchronous=FULL;
        CREATE TABLE IF NOT EXISTS config(recipe TEXT, hosts INTEGER);
        CREATE TABLE IF NOT EXISTS streams(
            path TEXT PRIMARY KEY, device INTEGER, inode INTEGER,
            offset INTEGER, anchor TEXT);
        CREATE TABLE IF NOT EXISTS done(path TEXT PRIMARY KEY);
        CREATE TABLE IF NOT EXISTS archived(batch TEXT PRIMARY KEY);
        CREATE TABLE IF NOT EXISTS manifest_cache(signature TEXT, rows TEXT);
        CREATE TABLE IF NOT EXISTS manifest_pages(
            path TEXT, batch TEXT, version INTEGER, PRIMARY KEY(path,batch,version));
        CREATE TABLE IF NOT EXISTS manifest_keys(
            batch TEXT, version INTEGER, PRIMARY KEY(batch,version));
        CREATE TABLE IF NOT EXISTS manifest_ready(
            batch TEXT, version INTEGER, doc TEXT, path TEXT,
            PRIMARY KEY(batch,version,doc,path));
        CREATE INDEX IF NOT EXISTS manifest_ready_path ON manifest_ready(path);
        CREATE TABLE IF NOT EXISTS inputs(binding TEXT);
        CREATE TABLE IF NOT EXISTS pending(
            path TEXT PRIMARY KEY, batch TEXT, doc TEXT, owner INTEGER, position INTEGER);
        CREATE INDEX IF NOT EXISTS pending_order ON pending(owner,batch,doc,path);
        CREATE INDEX IF NOT EXISTS pending_owner ON pending(owner,position);
    """)
    config = db.execute("SELECT recipe,hosts FROM config").fetchall()
    if not config:
        db.execute("INSERT INTO config VALUES(?,?)", (recipe, hosts))
        db.commit()
    elif config != [(recipe, hosts)]:
        db.close()
        raise ValueError("recipe/host count changed; use separate refresh state")
    return db


def anchor(stream, offset):
    stream.seek(max(0, offset - 4096))
    return hashlib.sha256(stream.read(min(offset, 4096))).hexdigest()


def consume(db, path, limit, accept):
    """Commit a bounded complete-line prefix and its cursor in one transaction."""
    path = Path(path).resolve(strict=True)
    with path.open("rb") as stream, db:
        info = os.fstat(stream.fileno())
        old = db.execute("SELECT device,inode,offset,anchor FROM streams WHERE path=?",
                         (str(path),)).fetchone()
        offset = 0
        if old:
            device, inode, offset, digest = old
            if ((device, inode) != (info.st_dev, info.st_ino) or info.st_size < offset
                    or anchor(stream, offset) != digest):
                raise ValueError(f"{path}: append-only input replaced/truncated/changed")
        stream.seek(offset)
        count = 0
        while count < limit:
            raw = stream.readline(LINE_BYTES + 1)
            if len(raw) > LINE_BYTES:
                raise ValueError(f"{path}: oversized input line")
            if not raw or not raw.endswith(b"\n"):
                break
            accept(raw.decode("utf-8").rstrip("\n"))
            offset = stream.tell()
            count += 1
        db.execute("INSERT OR REPLACE INTO streams VALUES(?,?,?,?,?)",
                   (str(path), info.st_dev, info.st_ino, offset, anchor(stream, offset)))
    return count, offset == info.st_size


def ingest_scan(db, raw, scans):
    row = json.loads(raw)
    if row.get("resolution", "full") != "full":
        return
    doc, batch = row["doc_id"], row["batch"]
    match = DOC.fullmatch(doc)
    if not match or not BATCH.fullmatch(batch):
        raise ValueError("invalid indexed page or batch")
    ext = row.get("format", "jpg")
    if ext == "jpeg":
        ext = "jpg"
    if ext not in ("jp2", "jpg"):
        raise ValueError("unsupported indexed scan format")
    lccn, date, edition, seq = match.groups()
    path = f"keep/{batch}/{lccn}/{date}/{edition}/{seq}.{ext}"
    if not (scans / path).is_file():
        return
    db.execute("INSERT OR IGNORE INTO archived VALUES(?)", (batch,))
    if ext == "jp2":
        db.execute("""
            INSERT OR IGNORE INTO pending(path,batch,doc)
            SELECT ?,?,?
            WHERE NOT EXISTS(SELECT 1 FROM done WHERE path=?)
        """, (path, batch, doc, path))
        db.execute("""
            INSERT OR IGNORE INTO manifest_ready(batch,version,doc,path)
            SELECT m.batch,m.version,p.doc,p.path FROM manifest_pages m
            JOIN pending p ON p.path=m.path AND p.batch=m.batch
            WHERE m.path=? AND p.owner IS NULL
        """, (path,))


def ingest_done(db, raw):
    path = raw.split("\t", 1)[0]
    if not path:
        raise ValueError("empty completion identity")
    db.execute("INSERT OR IGNORE INTO done VALUES(?)", (path,))
    db.execute("DELETE FROM pending WHERE path=?", (path,))
    db.execute("DELETE FROM manifest_ready WHERE path=?", (path,))


def batch_order(db, manifest, damage):
    if len(manifest) > MAX_BATCHES or len(damage) > MAX_BATCHES:
        raise ValueError("too many batch metadata rows")
    if any(not isinstance(b, str) or not BATCH.fullmatch(b)
           or type(v) is not int or v < 1 or type(n) is not int or n < 1
           for b, v, n in manifest):
        raise ValueError("invalid manifest batch metadata")
    keys = [(b, int(v)) for b, v, _ in manifest]
    if len(set(keys)) != len(keys):
        raise ValueError("duplicate manifest batch")
    ordered = [(key, []) for key, _ in sorted(
        zip(keys, [int(n) for _, _, n in manifest]),
        key=lambda item: (-item[1], *item[0]))]
    seen = set(keys)
    for name in sorted(damage):
        match = ordering.DAMAGE.fullmatch(name)
        if not match:
            continue
        key = (match[1], int(match[2]))
        if key not in seen and db.execute(
                "SELECT 1 FROM archived WHERE batch=?", (key[0],)).fetchone():
            ordered.append((key, []))
            seen.add(key)
    return ordered


def candidates(db, ordered, n_manifest, shards, current, limit):
    remaining = ordering.resolve_current(
        ordered, shards, current, n_manifest=n_manifest)

    def pages(shard):
        batches = remaining.get(shard)
        if batches is None:
            batches = ordering.remaining_batches(ordered, n_manifest, shards, shard, -1)
        for (batch, version), _ in batches:
            if db.execute("SELECT 1 FROM manifest_keys WHERE batch=? AND version=?",
                          (batch, version)).fetchone():
                cursor = db.execute("""
                    SELECT path FROM manifest_ready WHERE batch=? AND version=?
                    ORDER BY doc,path LIMIT ?
                """, (batch, version, limit))
            else:
                cursor = db.execute("""
                    SELECT path FROM pending WHERE owner IS NULL AND batch=?
                    ORDER BY doc,path LIMIT ?
                """, (batch, limit))
            for (path,) in cursor:
                yield path

    seen = set()
    for group in itertools.zip_longest(*(pages(s) for s in range(shards))):
        for path in group:
            if path is not None and path not in seen:
                seen.add(path)
                yield path
                if len(seen) >= limit:
                    return


def replenish(db, ordered, n_manifest, shards, current, hosts, low, high):
    if not 0 <= low < high <= 10000 or not 1 <= shards <= 64:
        raise ValueError("require 0 <= low < high <= 10000 and 1 <= shards <= 64")
    if any(s not in range(shards) for s in current):
        raise ValueError("current status names an invalid runner shard")
    need = []
    for host in range(hosts):
        count = db.execute("SELECT count(*) FROM pending WHERE owner=?", (host,)).fetchone()[0]
        need.append(high - count if count <= low else 0)
    total = sum(need)
    if not total:
        return 0
    paths = list(candidates(db, ordered, n_manifest, shards, current, total))
    assigned = 0
    position = max((db.execute("""
        SELECT position FROM pending WHERE owner=? ORDER BY position DESC LIMIT 1
    """, (host,)).fetchone() or (0,))[0] for host in range(hosts))
    with db:
        for path in paths:
            host = min((h for h in range(hosts) if need[h]),
                       key=lambda h: (high - need[h], h))
            position += 1
            db.execute("UPDATE pending SET owner=?,position=? WHERE path=? AND owner IS NULL",
                       (host, position, path))
            db.execute("DELETE FROM manifest_ready WHERE path=?", (path,))
            need[host] -= 1
            assigned += 1
    return assigned


def publish(db, work, hosts):
    for host in range(hosts):
        path = work / f"host-{host}.txt"
        scratch = path.with_suffix(".writing")
        with scratch.open("w") as stream:
            for (name,) in db.execute(
                    "SELECT path FROM pending WHERE owner=? ORDER BY position", (host,)):
                stream.write(name + "\n")
            stream.flush()
            os.fsync(stream.fileno())
        os.replace(scratch, path)
    fd = os.open(work, os.O_RDONLY)
    try:
        os.fsync(fd)
    finally:
        os.close(fd)


def manifest_batches(db, path):
    path = Path(path).resolve(strict=True)
    info = path.stat()
    def identity(value):
        return (value.st_dev, value.st_ino, value.st_size, value.st_mtime_ns, value.st_ctime_ns)

    signature = json.dumps([str(path), *identity(info)])
    cached = db.execute("SELECT signature,rows FROM manifest_cache").fetchone()
    if cached and cached[0] == signature:
        return json.loads(cached[1])
    import duckdb

    with duckdb.connect() as manifest_db, db:
        manifest_db.execute("SET memory_limit='64MB'")
        manifest_db.execute("SET threads=1")
        rows = manifest_db.execute("""
            SELECT batch,batch_version,count(*) FROM read_parquet(?)
            WHERE blob_name IS NOT NULL AND blob_name <> ''
            GROUP BY batch,batch_version
        """, [str(path)]).fetchmany(MAX_BATCHES + 1)
        if len(rows) > MAX_BATCHES:
            raise ValueError("too many manifest batches")
        db.execute("DELETE FROM manifest_pages")
        db.execute("DELETE FROM manifest_keys")
        db.executemany("INSERT OR IGNORE INTO manifest_keys VALUES(?,?)",
                       ((batch, version) for batch, version, _ in rows))
        cursor = manifest_db.execute("""
            SELECT blob_name,batch,batch_version FROM read_parquet(?)
            WHERE blob_name IS NOT NULL AND blob_name <> ''
        """, [str(path)])
        while chunk := cursor.fetchmany(10000):
            db.executemany("INSERT OR IGNORE INTO manifest_pages VALUES(?,?,?)", chunk)
        db.execute("DELETE FROM manifest_ready")
        db.execute("""
            INSERT INTO manifest_ready(batch,version,doc,path)
            SELECT m.batch,m.version,p.doc,p.path FROM manifest_pages m
            JOIN pending p ON p.path=m.path AND p.batch=m.batch
            WHERE p.owner IS NULL
        """)
        if identity(path.stat()) != identity(info):
            raise ValueError("manifest changed while reading")
        db.execute("DELETE FROM manifest_cache")
        db.execute("INSERT INTO manifest_cache VALUES(?,?)", (signature, json.dumps(rows)))
    return rows


def main(argv=None):
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--index", type=Path, required=True)
    p.add_argument("--scans-root", type=Path, required=True)
    p.add_argument("--manifest", required=True)
    p.add_argument("--damage-index", type=Path, required=True)
    p.add_argument("--current", default="")
    p.add_argument("--done", type=Path, action="append", required=True)
    p.add_argument("--recipe", required=True)
    p.add_argument("--state", type=Path, required=True)
    p.add_argument("--shards", type=int, default=8)
    p.add_argument("--rows-per-tick", type=int, default=10000)
    p.add_argument("--low-water", type=int, default=500)
    p.add_argument("--high-water", type=int, default=2000)
    a = p.parse_args(argv)
    if len(a.done) != 2 or not 1 <= a.rows_per_tick <= 100000:
        p.error("require two completion ledgers and 1..100000 rows per tick")
    if (len({path.resolve() for path in a.done}) != 2
            or not a.scans_root.is_dir()
            or not re.fullmatch(r"[a-z0-9][a-z0-9-]{0,40}", a.recipe)):
        p.error("require distinct completion ledgers, a scan directory and a safe recipe")
    if any(path.name != f"done-{a.recipe}.txt" for path in a.done):
        p.error("completion ledger filenames must match the configured recipe")
    for path in [a.index, Path(a.manifest), a.damage_index, *a.done]:
        if path.resolve().is_relative_to(a.state.resolve()):
            p.error("refresh state must be separate from input files")
    if a.state.resolve().is_relative_to(a.scans_root.resolve()):
        p.error("refresh state must be outside the scan archive")

    with locked(a.state):
        db = connect(a.state, a.recipe, 2)
        try:
            binding = json.dumps([str(path.resolve()) for path in
                                  [a.index, a.scans_root, *a.done]])
            old = db.execute("SELECT binding FROM inputs").fetchone()
            if old and old[0] != binding:
                raise ValueError("scan/ledger input paths changed; use separate refresh state")
            if not old:
                with db:
                    db.execute("INSERT INTO inputs VALUES(?)", (binding,))
            for path in a.done:
                _, caught_up = consume(db, path, a.rows_per_tick, lambda raw: ingest_done(db, raw))
                if not caught_up:
                    print(json.dumps({"state": "catching-up-completions", "source": str(path)}))
                    return
            consumed, _ = consume(db, a.index, a.rows_per_tick,
                                  lambda raw: ingest_scan(db, raw, a.scans_root))
            manifest = manifest_batches(db, a.manifest)
            with a.damage_index.open() as stream:
                damage = [line.strip() for line in itertools.islice(stream, MAX_BATCHES + 1)]
            ordered = batch_order(db, manifest, damage)
            assigned = replenish(db, ordered, len(manifest), a.shards,
                                 ordering.parse_current(a.current), 2, a.low_water, a.high_water)
            publish(db, a.state, 2)
            print(json.dumps({"state": "published", "index_rows": consumed, "assigned": assigned}))
        finally:
            db.close()


if __name__ == "__main__":
    try:
        main()
    except (ValueError, OSError, sqlite3.Error) as error:
        sys.exit(f"error: {error}")
