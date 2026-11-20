#!/usr/bin/env python3
"""Read-only steps over the hand-off root on the BOINC host (#1367), for a controller on another
host. onprem-step.sh runs these for `--target handoff --step read-object|read-journals`.

    handoff_read.py --root DIR read-object --object scheduler/inventory.json [--optional]
    handoff_read.py --root DIR read-journals

read-object prints the JSON document (at most 16 MiB), or `null` when --optional and it is absent.
read-journals prints {"journals": {name: {"sha256": ..., "record": ...}}} for every
dispatch/jobs/<64 hex>/record.json, listed and checked again after reading. A round that finds one
changed reads again only the journals that are new or no longer the file read before (6 rounds in
all, then refused), so thousands of journals settle while other plans' collects rewrite a few.
"""

import argparse
import hashlib
import json
import os
import re
import stat
import sys
import time
from pathlib import Path

MAX_OBJECT = 16 * 1024 * 1024
MAX_JOURNAL = 131072
MAX_JOBS = 10000
# A collect or export on the host rewrites a journal now and then while a snapshot is read; the
# read checks again a few times before it gives up (#1367). A round after the first reads only
# what changed, so it costs a listing and a stat of each journal, not a read of all of them.
SNAPSHOT_ATTEMPTS = 6
SNAPSHOT_PAUSE = 0.25
OBJECT = re.compile(r"[A-Za-z0-9][A-Za-z0-9._-]*(/[A-Za-z0-9][A-Za-z0-9._-]*)*")
JOB = re.compile(r"[0-9a-f]{64}")


class ReadError(Exception):
    pass


class ChangedError(ReadError):
    """The journals changed while the snapshot was read; reading again can succeed."""


def safe(root, name):
    if not OBJECT.fullmatch(name) or any(part in (".", "..") for part in name.split("/")):
        raise ReadError("invalid object path")
    path = root / name
    return path


def read_with_stat(path, limit):
    """The bytes of a regular file and the stat of the file they came from, opened once without
    following a link and read at most limit + 1 bytes, so a file swapped in after a size check
    cannot get past the limit."""
    try:
        fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    except OSError:
        raise ReadError("missing or unsafe object") from None
    with os.fdopen(fd, "rb") as handle:
        info = os.fstat(handle.fileno())
        if not stat.S_ISREG(info.st_mode):
            raise ReadError("missing or unsafe object")
        data = handle.read(limit + 1)
    if len(data) > limit:
        raise ReadError("object exceeds limit")
    return data, info


def read_bounded(path, limit):
    return read_with_stat(path, limit)[0]


def identity(info):
    """Which file a journal is: a write makes a new file (renamed or linked into place, as
    LocalStore writes), and a change in place moves its size or times, so the same identity
    is the same bytes. The archive's ETag is the same device, inode, size and mtime."""
    return (info.st_dev, info.st_ino, info.st_size, info.st_mtime_ns, info.st_ctime_ns)


def current(path):
    """The identity of the file at path now, or None when it is gone."""
    try:
        return identity(path.lstat())
    except FileNotFoundError:
        return None


def check_root(root):
    """The same root the runner demands before it mounts one: canonical, owned by the runner's
    user and mode 0700, so no other account can plant what the controller then trusts."""
    info = root.stat()
    if (not root.is_absolute() or root.resolve() != root or info.st_uid != os.geteuid()
            or info.st_mode & 0o777 != 0o700):
        raise ReadError("hand-off root must be canonical, owned by the runner user and mode 0700")


def read_object(root, name, optional=False):
    path = safe(root, name)
    if not path.exists() and not path.is_symlink():
        if optional:
            return "null"
        raise ReadError("missing or unsafe object")
    if path.resolve() != path:
        raise ReadError("missing or unsafe object")
    text = read_bounded(path, MAX_OBJECT).decode()
    json.loads(text)
    return text.strip()


def listing(root):
    jobs = root / "dispatch/jobs"
    if not jobs.is_dir() or jobs.resolve() != jobs:
        raise ReadError("dispatch inventory is missing or unsafe")
    names = []
    for entry in jobs.iterdir():
        if not JOB.fullmatch(entry.name):
            continue
        try:
            info = entry.lstat()
        except FileNotFoundError:
            # Gone since the directory was listed (the archive removes emptied job directories).
            raise ChangedError("job directory removed during listing") from None
        if not stat.S_ISDIR(info.st_mode):
            raise ReadError("unsafe job directory")
        path = entry / "record.json"
        try:
            info = path.lstat()
        except FileNotFoundError:
            continue
        if not stat.S_ISREG(info.st_mode):
            raise ReadError("unsafe journal")
        names.append(f"dispatch/jobs/{entry.name}/record.json")
    if len(names) > MAX_JOBS:
        raise ReadError("dispatch inventory limit; reconcile drained journals")
    return sorted(names)


def read_journal(root, name):
    path = root / name
    try:
        data, info = read_with_stat(path, MAX_JOURNAL)
    except ReadError:
        # Only a journal that is gone changed; an oversized or unsafe one is refused as it is.
        if path.exists() or path.is_symlink():
            raise ReadError("invalid/oversized journal metadata") from None
        raise ChangedError("journal changed during read") from None
    record = json.loads(data)
    if record["job"]["id"] != name.split("/")[2]:
        raise ReadError("journal filename/identity mismatch")
    return {"identity": identity(info), "sha256": hashlib.sha256(data).hexdigest(), "record": record}


def snapshot_round(root, held):
    """One round over the journals: list them, read each that is new or no longer the file held
    from an earlier round, then list again and check every one is still the file read. Raises
    ChangedError when anything moved; `held` keeps what was read for the next round."""
    names = listing(root)
    for name in set(held) - set(names):
        del held[name]
    for name in names:
        if name not in held or current(root / name) != held[name]["identity"]:
            held[name] = read_journal(root, name)
    if listing(root) != names:
        raise ChangedError("journal inventory changed during read")
    for name in names:
        if current(root / name) != held[name]["identity"]:
            raise ChangedError("journal changed during read")
    journals = {name: {"sha256": held[name]["sha256"], "record": held[name]["record"]} for name in names}
    return json.dumps({"journals": journals}, sort_keys=True, separators=(",", ":"))


def read_journals(root, attempts=SNAPSHOT_ATTEMPTS, pause=SNAPSHOT_PAUSE, sleep=time.sleep):
    """snapshot_round, again while a journal changed during the read, at most `attempts` rounds
    in all with a growing pause; any other failure, and the last change, is raised. A journal
    unchanged since an earlier round is not read again, so a later round costs a listing and a
    stat of each journal, and only the few another plan rewrote meanwhile are read."""
    held = {}
    for attempt in range(1, attempts + 1):
        try:
            return snapshot_round(root, held)
        except ChangedError:
            if attempt == attempts:
                raise
            sleep(pause * attempt)


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--root", type=Path, required=True)
    sub = parser.add_subparsers(dest="command", required=True)
    one = sub.add_parser("read-object")
    one.add_argument("--object", required=True)
    one.add_argument("--optional", action="store_true")
    sub.add_parser("read-journals")
    args = parser.parse_args(argv)
    try:
        if not args.root.is_dir() or args.root.is_symlink():
            raise ReadError("hand-off root is not a directory")
        check_root(args.root)
        if args.command == "read-object":
            print(read_object(args.root, args.object, args.optional))
        else:
            print(read_journals(args.root))
    except (ReadError, OSError, ValueError, KeyError, TypeError) as error:
        print(f"error: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
