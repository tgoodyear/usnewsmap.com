"""Persistent OpenVINO compile cache for the usnm_iris worker (#736).

job.toml mounts a named container volume at /cache that outlives the task. Without it every unit
compiles the model cold (about 23 s, and the cgroup reaches its 9 GiB limit); with a warm cache the
load takes about 1.8 s and peaks near 4 GiB (docs/reports/2026-10-iris-model-reuse).

OpenVINO writes the cache itself: the compiled model blobs at load and further kernel binaries
(*.cl_cache) during generation, whenever a new input shape needs one. Its writes are not atomic, so
only files recorded in a manifest after a clean run are trusted:

  /cache/.lock          flock: one user at a time; a second concurrent task runs without the cache
  /cache/<key>/         OpenVINO's CACHE_DIR for one model/runtime/device combination
  /cache/<key>.json     manifest: name, size and mtime of every file a clean run left behind

At the start of a run, any file that is not in the manifest or no longer matches it (left by a
killed run, or changed since) is deleted, as is every other key's directory. A run commits new files
(fsynced, then the manifest replaced atomically) only after it finished without a generate()
exception; such an exception or a failed warm load discards the whole cache so the next run is cold.
Total size and free disk space are bounded; anything unusable means "run without the cache", never
a failed unit.
"""
from __future__ import annotations

import dataclasses
import errno
import fcntl
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import stat
import sys
from typing import Any

FORMAT = 1
LOCK = ".lock"
MANIFEST_SUFFIX = ".json"
# Measured: 3.18 GB and 247 files for this model on Iris Xe.
MAX_CACHE_BYTES = 6 * 2**30
MAX_FILES = 4096
MAX_MANIFEST_BYTES = 1024 * 1024
# A cold run writes about 3.2 GB; a warm one only adds kernels of a few hundred KB each.
MIN_FREE_COLD_BYTES = 8 * 2**30
MIN_FREE_WARM_BYTES = 1 * 2**30
_KEY = re.compile(r"[0-9a-f]{32}")
_NAME = re.compile(r"[A-Za-z0-9_][A-Za-z0-9_.-]{0,127}")


@dataclasses.dataclass
class Lease:
    root: Path
    key: str
    fd: int
    entries: dict[str, list[int]]

    @property
    def directory(self) -> Path:
        return self.root / self.key

    @property
    def manifest(self) -> Path:
        return self.root / (self.key + MANIFEST_SUFFIX)


def key_for(parts: dict[str, Any]) -> str:
    data = json.dumps({"format": FORMAT, **parts}, sort_keys=True, separators=(",", ":"), ensure_ascii=True)
    return hashlib.sha256(data.encode("utf-8")).hexdigest()[:32]


def file_sha256(path: Path) -> str | None:
    try:
        return hashlib.sha256(path.read_bytes()).hexdigest()
    except OSError:
        return None


def log(event: str, **fields: Any) -> None:
    print("usnm_iris_cache " + json.dumps({"event": event, **fields}, sort_keys=True)[:2000], file=sys.stderr, flush=True)


def _remove(path: Path) -> None:
    try:
        st = os.lstat(path)
    except FileNotFoundError:
        return
    if stat.S_ISDIR(st.st_mode):
        shutil.rmtree(path, ignore_errors=True)
    else:
        try:
            os.unlink(path)
        except FileNotFoundError:
            pass


def _load_manifest(path: Path) -> dict[str, list[int]]:
    """The recorded files, or {} if the manifest is missing or not exactly well formed."""
    try:
        st = os.lstat(path)
        if not stat.S_ISREG(st.st_mode) or st.st_size > MAX_MANIFEST_BYTES:
            return {}
        doc = json.loads(path.read_bytes())
    except (OSError, ValueError):
        return {}
    if not isinstance(doc, dict) or doc.get("format") != FORMAT or not isinstance(doc.get("files"), dict):
        return {}
    files = doc["files"]
    if len(files) > MAX_FILES:
        return {}
    out: dict[str, list[int]] = {}
    for name, value in files.items():
        if (not isinstance(name, str) or not _NAME.fullmatch(name) or not isinstance(value, list) or len(value) != 2
                or not all(type(v) is int and v >= 0 for v in value)):
            return {}
        out[name] = value
    return out


def _scan(directory: Path) -> tuple[dict[str, list[int]], list[str]]:
    """Regular files with acceptable names as {name: [size, mtime_ns]}, and every other entry."""
    files: dict[str, list[int]] = {}
    other: list[str] = []
    for entry in os.scandir(directory):
        st = entry.stat(follow_symlinks=False)
        if stat.S_ISREG(st.st_mode) and _NAME.fullmatch(entry.name):
            files[entry.name] = [st.st_size, st.st_mtime_ns]
        else:
            other.append(entry.name)
    return files, other


def _fsync_dir(path: Path) -> None:
    fd = os.open(path, os.O_RDONLY | getattr(os, "O_DIRECTORY", 0))
    try:
        os.fsync(fd)
    finally:
        os.close(fd)


def acquire(root: Path, parts: dict[str, Any], diag: dict[str, Any]) -> Lease | None:
    """A locked, reconciled cache directory for this key, or None (diag says why) to run without one."""
    diag["compile_cache"] = "unavailable"
    try:
        st = os.lstat(root)
    except OSError:
        return None
    if not stat.S_ISDIR(st.st_mode) or not os.access(root, os.W_OK | os.X_OK):
        return None
    key = key_for(parts)
    diag["compile_cache_key"] = key
    fd = -1
    try:
        fd = os.open(root / LOCK, os.O_RDWR | os.O_CREAT | getattr(os, "O_NOFOLLOW", 0) | getattr(os, "O_CLOEXEC", 0), 0o600)
        try:
            fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except OSError as exc:
            if exc.errno in (errno.EWOULDBLOCK, errno.EAGAIN, errno.EACCES):
                diag["compile_cache"] = "busy"
                os.close(fd)
                return None
            raise
        lease = Lease(root, key, fd, {})
        for entry in os.listdir(root):
            if entry not in (LOCK, key, key + MANIFEST_SUFFIX):
                _remove(root / entry)
        directory = lease.directory
        entries = _load_manifest(lease.manifest)
        dropped = 0
        if os.path.lexists(directory) and not stat.S_ISDIR(os.lstat(directory).st_mode):
            _remove(directory)
        if os.path.isdir(directory):
            files, other = _scan(directory)
            for name in other:
                _remove(directory / name)
                dropped += 1
            for name, value in files.items():
                if entries.get(name) != value:
                    _remove(directory / name)
                    dropped += 1
            entries = {name: value for name, value in entries.items() if files.get(name) == value}
        else:
            entries = {}
        total = sum(size for size, _ in entries.values())
        if total > MAX_CACHE_BYTES or len(entries) > MAX_FILES:
            _remove(directory)
            entries, total = {}, 0
        free = shutil.disk_usage(root).free
        if free < (MIN_FREE_WARM_BYTES if entries else MIN_FREE_COLD_BYTES):
            # Without enough room for OpenVINO's writes, keep what is there but run without the cache.
            diag["compile_cache"] = "no_space"
            diag["compile_cache_free_bytes"] = free
            os.close(fd)
            return None
        if not entries:
            _remove(lease.manifest)
        directory.mkdir(mode=0o700, exist_ok=True)
        lease.entries = entries
        diag.update({"compile_cache": "warm" if entries else "cold", "compile_cache_files": len(entries),
                     "compile_cache_bytes": total, "compile_cache_dropped": dropped})
        log("acquire", key=key, state=diag["compile_cache"], files=len(entries), bytes=total, dropped=dropped)
        return lease
    except OSError as exc:
        diag["compile_cache"] = "error"
        diag["compile_cache_error"] = f"{type(exc).__name__}: {exc}"[:500]
        if fd >= 0:
            os.close(fd)
        return None


def commit(lease: Lease, diag: dict[str, Any]) -> None:
    """Record the files a clean run left behind; over the limits, discard the cache instead."""
    try:
        files, other = _scan(lease.directory)
        total = sum(size for size, _ in files.values())
        if other or total > MAX_CACHE_BYTES or len(files) > MAX_FILES:
            diag["compile_cache_commit"] = "over_limit" if not other else "unexpected_entries"
            discard(lease)
            return
        new = [name for name, value in files.items() if lease.entries.get(name) != value]
        for name in new:
            fd = os.open(lease.directory / name, os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0))
            try:
                os.fsync(fd)
            finally:
                os.close(fd)
        _fsync_dir(lease.directory)
        partial = lease.root / (lease.key + MANIFEST_SUFFIX + ".partial")
        data = json.dumps({"format": FORMAT, "key": lease.key, "files": files}, sort_keys=True).encode("utf-8")
        fd = os.open(partial, os.O_WRONLY | os.O_CREAT | os.O_TRUNC | getattr(os, "O_NOFOLLOW", 0), 0o600)
        with os.fdopen(fd, "wb") as fh:
            fh.write(data)
            fh.flush()
            os.fsync(fh.fileno())
        os.replace(partial, lease.manifest)
        _fsync_dir(lease.root)
        lease.entries = files
        diag.update({"compile_cache_commit": "ok", "compile_cache_new_files": len(new)})
        log("commit", key=lease.key, files=len(files), bytes=total, new=len(new))
    except OSError as exc:
        diag["compile_cache_commit"] = f"error: {type(exc).__name__}: {exc}"[:500]
        discard(lease)


def discard(lease: Lease) -> None:
    """Delete this key's cache so the next run compiles cold."""
    _remove(lease.manifest)
    _remove(lease.directory)
    lease.entries = {}
    log("discard", key=lease.key)


def release(lease: Lease | None) -> None:
    if lease is not None and lease.fd >= 0:
        os.close(lease.fd)
        lease.fd = -1
