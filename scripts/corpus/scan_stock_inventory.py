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


def summary(conn, config, processed, started):
    sets = {}
    for cache_set in config["cache_sets"]:
        rows = conn.execute(
            "SELECT kind,count(*) FROM entries WHERE cache_set=? GROUP BY kind", (cache_set,)
        ).fetchall()
        scans, size, pages, unplaced = conn.execute(
            "SELECT count(*),coalesce(sum(bytes),0),count(DISTINCT page),"
            "coalesce(sum(page IS NULL),0) FROM entries WHERE cache_set=? AND kind='scan'",
            (cache_set,),
        ).fetchone()
        sets[cache_set] = {
            "entry_kinds": dict(rows), "scan_files": scans, "scan_file_bytes": size,
            "distinct_page_paths": pages, "scan_files_without_page_path": unplaced,
        }
    inodes, inode_bytes, changed_sizes = conn.execute(
        "SELECT count(*),coalesce(sum(largest),0),coalesce(sum(smallest!=largest),0) FROM "
        "(SELECT min(bytes) AS smallest,max(bytes) AS largest FROM entries WHERE kind='scan' "
        "GROUP BY device,inode)"
    ).fetchone()
    pending = conn.execute("SELECT count(*) FROM directories WHERE done=0").fetchone()[0]
    return {
        "schema_version": 1, "coverage": "selected local cache trees, observed over an interval",
        "checksum_verified": False, "corpus_complete": False, "atomic_snapshot": False,
        "finished": pending == 0, "started_at": started, "observed_until": now(),
        "root": config["root"], "cache_sets": sets, "pending_directories": pending,
        "entries_this_invocation": processed, "distinct_observed_inodes": inodes,
        "distinct_inode_max_bytes": inode_bytes, "inodes_with_size_disagreement": changed_sizes,
        "limits": "metadata calls may block; use an external process deadline and idle I/O priority",
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
        with closing(sqlite3.connect(db)) as conn:
            conn.execute("PRAGMA synchronous=FULL")
            with conn:
                conn.execute("CREATE TABLE IF NOT EXISTS metadata (config TEXT, started TEXT)")
                conn.execute("CREATE TABLE IF NOT EXISTS directories "
                             "(path TEXT PRIMARY KEY, done INTEGER NOT NULL, device TEXT, inode TEXT)")
                conn.execute("CREATE TABLE IF NOT EXISTS entries "
                             "(path TEXT PRIMARY KEY, cache_set TEXT, kind TEXT, bytes INTEGER,"
                             "device TEXT, inode TEXT, page TEXT)")
                saved = conn.execute("SELECT config,started FROM metadata").fetchone()
                if saved is None:
                    started = now()
                    conn.execute("INSERT INTO metadata VALUES (?,?)", (json.dumps(config), started))
                    for value in sorted(cache_sets):
                        observation = observe(root, value)
                        c.require(observation[0] == "directory", "cache set changed")
                        conn.execute("INSERT INTO directories VALUES (?,0,?,?)",
                                     (value, observation[2], observation[3]))
                else:
                    c.require(json.loads(saved[0]) == config,
                              "inventory configuration/root changed; use a new work directory")
                    started = saved[1]
            inventory.sync_dir(work)
            processed = 0
            deadline = time.monotonic() + max_seconds
            with conn:
                while processed < max_entries and time.monotonic() < deadline:
                    row = conn.execute(
                        "SELECT path,device,inode FROM directories WHERE done=0 ORDER BY path LIMIT 1"
                    ).fetchone()
                    if row is None:
                        break
                    relative = row[0]
                    with directory_fd(root, relative) as fd, os.scandir(fd) as children:
                        info = os.fstat(fd)
                        c.require((str(info.st_dev), str(info.st_ino)) == row[1:],
                                  f"directory changed: {relative}; use a new inventory")
                        for entry in children:
                            if processed >= max_entries or time.monotonic() >= deadline:
                                break
                            child = str(Path(relative) / entry.name)
                            if conn.execute("SELECT 1 FROM entries WHERE path=?", (child,)).fetchone():
                                continue
                            observation = observe(root, child, fd)
                            conn.execute("INSERT INTO entries VALUES (?,?,?,?,?,?,?)",
                                         (child, Path(child).parts[0], *observation))
                            if observation[0] == "directory":
                                conn.execute("INSERT INTO directories VALUES (?,0,?,?)",
                                             (child, observation[2], observation[3]))
                            processed += 1
                        else:
                            conn.execute("UPDATE directories SET done=1 WHERE path=?", (relative,))
            result = summary(conn, config, processed, started)
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
