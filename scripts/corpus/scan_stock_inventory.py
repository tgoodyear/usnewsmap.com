#!/usr/bin/env python3
"""Resumable read-only KEEP/buffer stock observations, not checksum or corpus acceptance."""

import argparse
from contextlib import closing, contextmanager
from datetime import datetime, timezone
import json
import os
from pathlib import Path
import re
import sqlite3
import stat
import sys
import time

try:
    from scripts.corpus import scan_cache_inventory as inventory
except ModuleNotFoundError:
    import scan_cache_inventory as inventory


MAX_DIRECTORY_ENTRIES = 10000


def now():
    return datetime.now(timezone.utc).isoformat()


def page_key(path):
    parts = Path(path).parts
    if len(parts) != 6:
        return None
    match = re.fullmatch(r"([1-9][0-9]*)\.(?:pct[0-9]+\.)?(?:jpg|jp2)", parts[-1])
    return "/".join((*parts[1:-1], match[1])) if match else None


@contextmanager
def directory_fd(root, relative):
    fd = os.open(root, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
    try:
        for part in Path(relative).parts:
            child = os.open(part, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW, dir_fd=fd)
            os.close(fd)
            fd = child
        yield fd
    finally:
        os.close(fd)


def observe(root, relative, parent_fd=None):
    if parent_fd is None:
        with directory_fd(root, Path(relative).parent) as fd:
            return observe(root, relative, fd)
    try:
        info = os.stat(Path(relative).name, dir_fd=parent_fd, follow_symlinks=False)
    except FileNotFoundError:
        return ("vanished", None, None, None, None)
    if stat.S_ISLNK(info.st_mode):
        return ("symlink", None, None, None, None)
    if stat.S_ISDIR(info.st_mode):
        return ("directory", None, str(info.st_dev), str(info.st_ino), None)
    if stat.S_ISREG(info.st_mode) and Path(relative).suffix in (".jpg", ".jp2"):
        return ("scan", info.st_size, str(info.st_dev), str(info.st_ino), page_key(relative))
    return ("other", None, None, None, None)


def increment(conn, name, amount=1, cache_set="*"):
    conn.execute(
        "INSERT INTO totals VALUES (?,?,?) ON CONFLICT(cache_set,name) DO UPDATE "
        "SET value=value+excluded.value", (cache_set, name, amount),
    )


def record(conn, cache_set, observation):
    kind, size, device, inode, page = observation
    increment(conn, "kind:" + kind, cache_set=cache_set)
    if kind != "scan":
        return
    increment(conn, "scan_files", cache_set=cache_set)
    increment(conn, "scan_file_bytes", size, cache_set)
    if page is None:
        increment(conn, "scan_files_without_page_path", cache_set=cache_set)
    elif conn.execute("INSERT OR IGNORE INTO pages VALUES (?,?)", (cache_set, page)).rowcount:
        increment(conn, "distinct_page_paths", cache_set=cache_set)
    previous = conn.execute(
        "SELECT smallest,largest FROM inodes WHERE device=? AND inode=?", (device, inode)
    ).fetchone()
    if previous is None:
        conn.execute("INSERT INTO inodes VALUES (?,?,?,?)", (device, inode, size, size))
        increment(conn, "distinct_observed_inodes")
        increment(conn, "distinct_inode_max_bytes", size)
    else:
        smallest, largest = previous
        increment(conn, "distinct_inode_max_bytes", max(largest, size) - largest)
        if smallest == largest and size != largest:
            increment(conn, "inodes_with_size_disagreement")
        conn.execute("UPDATE inodes SET smallest=?,largest=? WHERE device=? AND inode=?",
                     (min(smallest, size), max(largest, size), device, inode))


def capture_listing(conn, fd, relative):
    # One bounded names-only snapshot; later stat slices consume this durable queue, not readdir.
    names = []
    with os.scandir(fd) as children:
        for entry in children:
            inventory.c.require(len(names) < MAX_DIRECTORY_ENTRIES,
                                f"directory exceeds {MAX_DIRECTORY_ENTRIES} entries: {relative}")
            names.append(entry.name)
    conn.executemany("INSERT INTO pending VALUES (?,?)",
                     [(relative, str(Path(relative) / name)) for name in names])
    conn.execute("UPDATE directories SET listed=1 WHERE path=?", (relative,))


def summary(conn, config, processed, started, observed_until):
    totals = {(cache_set, name): value for cache_set, name, value in conn.execute(
        "SELECT cache_set,name,value FROM totals"
    )}
    value = lambda name, cache_set="*": totals.get((cache_set, name), 0)
    sets = {
        cache_set: {
            "entry_kinds": {name[5:]: amount for (scope, name), amount in totals.items()
                            if scope == cache_set and name.startswith("kind:")},
            **{name: value(name, cache_set) for name in (
                "scan_files", "scan_file_bytes", "distinct_page_paths", "scan_files_without_page_path"
            )},
        } for cache_set in config["cache_sets"]
    }
    pending = value("pending_directories")
    return {
        "schema_version": 2, "coverage": "selected local cache trees, observed over an interval",
        "checksum_verified": False, "corpus_complete": False, "atomic_snapshot": False,
        "finished": pending == 0, "started_at": started, "observed_until": observed_until,
        "generated_at": now(),
        "root": config["root"], "cache_sets": sets, "pending_directories": pending,
        "entries_this_invocation": processed,
        **{name: value(name) for name in ("distinct_observed_inodes", "distinct_inode_max_bytes",
                                        "inodes_with_size_disagreement")},
        "limits": f"at most {MAX_DIRECTORY_ENTRIES} names per directory; metadata calls may block; "
                  "use an external process deadline and idle I/O priority",
    }


def run(scan_root, work, cache_sets, max_entries=1000, max_seconds=20):
    c = inventory.c
    c.require(type(max_entries) is int and 1 <= max_entries <= 100000,
              "max-entries must be between 1 and 100000")
    c.require(type(max_seconds) in (int, float) and 0 < max_seconds <= 300,
              "max-seconds must be greater than zero and at most 300")
    c.require(cache_sets and len(cache_sets) == len(set(cache_sets)) and
              all(value in ("keep", "buffer") for value in cache_sets),
              "choose distinct keep/buffer sets")
    root, work = Path(scan_root).resolve(), Path(work).resolve()
    c.require(root.is_dir(), "scan root must be a directory")
    c.require(not work.is_relative_to(root) and not root.is_relative_to(work),
              "work directory must be separate from scan root")
    info = root.stat()
    config = {"root": str(root), "identity": [info.st_dev, info.st_ino],
              "cache_sets": sorted(cache_sets)}
    for cache_set in cache_sets:
        path = root / cache_set
        c.require(path.is_dir() and not path.is_symlink(), "cache set must be a real directory")
    inventory.durable_mkdir(work)
    with inventory.lock(work):
        db = work / "stock.sqlite"
        c.require(not db.is_symlink(), "checkpoint cannot be a symlink")
        existing = db.exists()
        with closing(sqlite3.connect(db)) as conn:
            c.require(not existing or conn.execute("PRAGMA user_version").fetchone()[0] == 2,
                      "checkpoint schema changed; use a new work directory")
            conn.execute("PRAGMA synchronous=FULL")
            with conn:
                conn.execute("PRAGMA user_version=2")
                conn.execute("CREATE TABLE IF NOT EXISTS metadata "
                             "(config TEXT, started TEXT, observed_until TEXT)")
                conn.execute("CREATE TABLE IF NOT EXISTS directories "
                             "(path TEXT PRIMARY KEY, done INTEGER NOT NULL, device TEXT, inode TEXT,"
                             "listed INTEGER NOT NULL)")
                conn.execute("CREATE INDEX IF NOT EXISTS pending_directories "
                             "ON directories(path) WHERE done=0")
                conn.execute("CREATE TABLE IF NOT EXISTS pending "
                             "(directory TEXT, path TEXT, PRIMARY KEY(directory,path))")
                conn.execute("CREATE TABLE IF NOT EXISTS totals "
                             "(cache_set TEXT, name TEXT, value INTEGER, PRIMARY KEY(cache_set,name))")
                conn.execute("CREATE TABLE IF NOT EXISTS pages "
                             "(cache_set TEXT, page TEXT, PRIMARY KEY(cache_set,page))")
                conn.execute("CREATE TABLE IF NOT EXISTS inodes "
                             "(device TEXT, inode TEXT, smallest INTEGER, largest INTEGER,"
                             "PRIMARY KEY(device,inode))")
                saved = conn.execute("SELECT config,started,observed_until FROM metadata").fetchone()
                if saved is None:
                    started = now()
                    observed_until = None
                    conn.execute("INSERT INTO metadata VALUES (?,?,NULL)", (json.dumps(config), started))
                    for value in sorted(cache_sets):
                        observation = observe(root, value)
                        c.require(observation[0] == "directory", "cache set changed")
                        conn.execute("INSERT INTO directories VALUES (?,0,?,?,0)",
                                     (value, observation[2], observation[3]))
                        increment(conn, "pending_directories")
                else:
                    c.require(json.loads(saved[0]) == config,
                              "inventory configuration/root changed; use a new work directory")
                    started = saved[1]
                    observed_until = saved[2]
            inventory.sync_dir(work)
            processed = 0
            deadline = time.monotonic() + max_seconds
            with conn:
                while processed < max_entries and time.monotonic() < deadline:
                    row = conn.execute(
                        "SELECT path,device,inode,listed FROM directories "
                        "WHERE done=0 ORDER BY path LIMIT 1"
                    ).fetchone()
                    if row is None:
                        break
                    relative = row[0]
                    with directory_fd(root, relative) as fd:
                        info = os.fstat(fd)
                        c.require((str(info.st_dev), str(info.st_ino)) == row[1:3],
                                  f"directory changed: {relative}; use a new inventory")
                        if not row[3]:
                            capture_listing(conn, fd, relative)
                        while processed < max_entries and time.monotonic() < deadline:
                            next_entry = conn.execute(
                                "SELECT path FROM pending WHERE directory=? ORDER BY path LIMIT 1",
                                (relative,),
                            ).fetchone()
                            if next_entry is None:
                                conn.execute("UPDATE directories SET done=1 WHERE path=?", (relative,))
                                increment(conn, "pending_directories", -1)
                                break
                            child = next_entry[0]
                            observation = observe(root, child, fd)
                            record(conn, Path(child).parts[0], observation)
                            if observation[0] == "directory":
                                conn.execute("INSERT INTO directories VALUES (?,0,?,?,0)",
                                             (child, observation[2], observation[3]))
                                increment(conn, "pending_directories")
                            conn.execute("DELETE FROM pending WHERE directory=? AND path=?",
                                         (relative, child))
                            processed += 1
                            observed_until = now()
                if processed:
                    conn.execute("UPDATE metadata SET observed_until=?", (observed_until,))
            result = summary(conn, config, processed, started, observed_until)
            inventory.publish(work / "report.json", (json.dumps(result, indent=2) + "\n").encode())
            return result


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--scan-root", required=True, type=Path)
    parser.add_argument("--work-dir", required=True, type=Path)
    parser.add_argument("--cache-set", required=True, action="append", choices=("keep", "buffer"))
    parser.add_argument("--max-entries", type=int, default=1000)
    parser.add_argument("--max-seconds", type=float, default=20)
    args = parser.parse_args(argv)
    try:
        print(json.dumps(run(args.scan_root, args.work_dir, args.cache_set,
                             args.max_entries, args.max_seconds)))
        return 0
    except (inventory.c.CatalogError, OSError, sqlite3.Error) as exc:
        print(json.dumps({"error": str(exc)}), file=sys.stderr)
        return 2


if __name__ == "__main__":
    sys.exit(main())
