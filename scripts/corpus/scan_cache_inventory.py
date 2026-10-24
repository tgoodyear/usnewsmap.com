#!/usr/bin/env python3
"""Bounded, resumable local scan inventory; stat evidence is NOT checksum verification."""

import argparse
import hashlib
import json
import os
from pathlib import Path
import sqlite3
import stat
import sys
from contextlib import contextmanager

try:
    from scripts.corpus import data_catalog as c
except ModuleNotFoundError:
    import data_catalog as c


# At most 256 input rows and 1 MiB per slice, leaving ample room for projection.
ROWS = 256
BYTES = 1024 * 1024


def sync_dir(path):
    fd = os.open(path, os.O_RDONLY)
    try:
        os.fsync(fd)
    finally:
        os.close(fd)


def durable_mkdir(path):
    missing = []
    parent = path
    while not parent.exists():
        missing.append(parent)
        parent = parent.parent
    for directory in reversed(missing):
        directory.mkdir(mode=0o700, exist_ok=True)
        sync_dir(directory.parent)


def publish(path, raw):
    """Only unpublished scratch is replaceable; final evidence is retained."""
    scratch = path.with_name(path.name + ".writing")
    with scratch.open("wb") as stream:
        os.chmod(scratch, 0o600)
        stream.write(raw)
        stream.flush()
        os.fsync(stream.fileno())
    os.replace(scratch, path)
    sync_dir(path.parent)


def save(work, state):
    publish(work / "state.json", (c.canonical(state) + "\n").encode())


@contextmanager
def lock(work):
    # Released by the OS on interruption; no stale PID lock recovery is needed.
    import fcntl
    with (work / "lock").open("a") as stream:
        try:
            fcntl.flock(stream, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            raise c.CatalogError("inventory directory is already in use") from None
        yield


def prefix_hash(stream, length):
    stream.seek(0)
    result = hashlib.sha256()
    remaining = length
    while remaining:
        raw = stream.read(min(remaining, BYTES))
        c.require(raw, "index truncated; use a new inventory directory")
        result.update(raw)
        remaining -= len(raw)
    return result.hexdigest()


def batch_id(state, number, cache_set):
    return "inventory-" + state["id"] + f"-{number}-{cache_set}"


def run(db, index, scan_root, work, source, cache_sets, max_batches=10):
    c.token(source)
    c.require(type(max_batches) is int and 1 <= max_batches <= 10000,
              "max-batches must be between 1 and 10000")
    c.require(cache_sets and len(cache_sets) == len(set(cache_sets)) and
              all(s in ("keep", "buffer") for s in cache_sets), "choose distinct keep/buffer sets")
    cache_sets = sorted(cache_sets)
    c.require(not Path(db).is_symlink(), "catalog cannot be a symlink")
    db, index, scan_root, work = (Path(p).resolve() for p in (db, index, scan_root, work))
    c.require(scan_root.is_dir(), "scan root must be a directory")
    c.require(not work.exists() or work.is_dir(), "inventory directory must be a directory")
    # Output names must not overwrite input, database, or its journal.
    c.require(not index.is_relative_to(work) and not db.is_relative_to(work) and
              not work.is_relative_to(scan_root), "inventory directory must be separate from inputs and catalog")
    durable_mkdir(work)
    with lock(work), index.open("rb") as stream:
        info = os.fstat(stream.fileno())
        c.require(stat.S_ISREG(info.st_mode), "index must be a regular file")
        config = {"db": str(db), "index": str(index), "scan_root": str(scan_root),
                  "source": source, "cache_sets": cache_sets}
        state_file = work / "state.json"
        if state_file.exists():
            c.require(state_file.stat().st_size <= c.MAX_LINE, "oversized inventory state")
            state = c.load_json(state_file.read_bytes())
            c.require(state["version"] == 1 and state["config"] == config,
                      "inventory configuration changed; use a new inventory directory")
            c.require([info.st_dev, info.st_ino] == state["identity"],
                      "index replaced; use a new inventory directory")
            c.require(info.st_size >= state["captured_bytes"], "index truncated")
            c.require(prefix_hash(stream, state["captured_bytes"]) == state["input_sha256"],
                      "captured index prefix changed; use a new inventory directory")
            # Do not silently skip evidence if a database was lost or replaced.
            if state["batches"]:
                c.require(db.is_file(), "catalog checkpoint missing; rebuild by replaying retained evidence")
                conn = c.connect(db, writable=True)
                try:
                    for number in range(state["batches"]):
                        for cache_set in cache_sets:
                            row = conn.execute("SELECT sha256 FROM batches WHERE batch_id=?",
                                               (batch_id(state, number, cache_set),)).fetchone()
                            c.require(row is not None, "catalog checkpoint missing; rebuild by replaying retained evidence")
                finally:
                    conn.close()
        else:
            # Freeze this invocation's input prefix. Appends belong to a NEW inventory.
            captured = info.st_size
            state = {"version": 1, "id": os.urandom(16).hex(), "config": config,
                     "identity": [info.st_dev, info.st_ino], "captured_bytes": captured,
                     "input_sha256": prefix_hash(stream, captured), "offset": 0,
                     "rows": 0, "batches": 0, "pending": None, "finished": False}
            save(work, state)
        imported = 0
        while imported < max_batches and not state["finished"]:
            number = state["batches"]
            directory = work / f"batch-{number:08d}"
            directory.mkdir(mode=0o700, exist_ok=True)
            if state["pending"] is None:
                stream.seek(state["offset"])
                rows = []
                size = 0
                while len(rows) < ROWS and size < BYTES:
                    remaining = state["captured_bytes"] - stream.tell()
                    if not remaining:
                        break
                    raw = stream.readline(min(c.MAX_LINE + 1, remaining))
                    c.require(raw, "index truncated while reading")
                    c.require(len(raw) <= c.MAX_LINE, "oversized index row")
                    # An append in flight may end at a partial JSONL record.
                    if not raw.endswith(b"\n"):
                        break
                    if size + len(raw) > BYTES:
                        break
                    rows.append(raw)
                    size += len(raw)
                if not rows:
                    state["finished"] = True
                    save(work, state)
                    break
                raw = b"".join(rows)
                path = directory / "index.jsonl"
                publish(path, raw)
                state["pending"] = {"end": state["offset"] + size, "rows": len(rows),
                                    "sha256": hashlib.sha256(raw).hexdigest()}
                save(work, state)
            pending = state["pending"]
            index_slice = directory / "index.jsonl"
            c.require(index_slice.stat().st_size <= BYTES and
                      hashlib.sha256(index_slice.read_bytes()).hexdigest() == pending["sha256"],
                      "pending index slice changed")
            for cache_set in cache_sets:
                evidence = directory / (cache_set + ".jsonl")
                if not evidence.exists():
                    # Adapter writes scratch; only a complete fsynced file is published.
                    build = directory / (cache_set + ".building-output")
                    if build.exists():
                        os.replace(build, directory / (cache_set + ".interrupted"))
                    c.write_cached_scan_evidence(db, index_slice, scan_root, build,
                                                 batch_id(state, number, cache_set), source, cache_set)
                    os.replace(build, evidence)
                sync_dir(directory)
                c.ingest(db, evidence)
            state.update(offset=pending["end"], rows=state["rows"] + pending["rows"],
                         batches=number + 1, pending=None,
                         finished=pending["end"] == state["captured_bytes"])
            save(work, state)
            imported += 1
        return {"coverage": "partial local cache inventory; not corpus completeness",
                "checksum_verified": False, "finished": state["finished"],
                "captured_bytes": state["captured_bytes"], "covered_bytes": state["offset"],
                "uncovered_bytes": state["captured_bytes"] - state["offset"],
                "rows": state["rows"], "batches": state["batches"],
                "cache_sets": cache_sets, "batches_this_invocation": imported,
                "appended_bytes_not_covered": max(0, os.fstat(stream.fileno()).st_size -
                                                 state["captured_bytes"])}


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("db", "index", "scan-root", "work-dir", "source"):
        parser.add_argument("--" + name, required=True)
    parser.add_argument("--cache-set", action="append", required=True, choices=("keep", "buffer"))
    parser.add_argument("--max-batches", type=int, default=10)
    args = parser.parse_args(argv)
    try:
        result = run(args.db, args.index, args.scan_root, args.work_dir,
                     args.source, args.cache_set, args.max_batches)
        print(c.canonical(result))
        return 0
    except (c.CatalogError, OSError, sqlite3.Error) as exc:
        message = str(exc) if isinstance(exc, c.CatalogError) else "local inventory I/O failure"
        print(json.dumps({"error": message}), file=sys.stderr)
        return 2


if __name__ == "__main__":
    sys.exit(main())
