#!/usr/bin/env python3
"""Read-only steps over the hand-off root on the BOINC host (#1367), for a controller on another
host. onprem-step.sh runs these for `--target handoff --step read-object|read-journals`.

    handoff_read.py --root DIR read-object --object scheduler/inventory.json [--optional]
    handoff_read.py --root DIR read-journals

read-object prints the JSON document (at most 16 MiB), or `null` when --optional and it is absent.
read-journals prints {"journals": {name: {"sha256": ..., "record": ...}}} for every
dispatch/jobs/<64 hex>/record.json, listed before and after reading and refused if they changed.
"""

import argparse
import hashlib
import json
import os
import re
import stat
import sys
from pathlib import Path

MAX_OBJECT = 16 * 1024 * 1024
MAX_JOURNAL = 131072
MAX_JOBS = 10000
OBJECT = re.compile(r"[A-Za-z0-9][A-Za-z0-9._-]*(/[A-Za-z0-9][A-Za-z0-9._-]*)*")
JOB = re.compile(r"[0-9a-f]{64}")


class ReadError(Exception):
    pass


def safe(root, name):
    if not OBJECT.fullmatch(name) or any(part in (".", "..") for part in name.split("/")):
        raise ReadError("invalid object path")
    path = root / name
    return path


def read_bounded(path, limit):
    """The bytes of a regular file, opened once without following a link and read at most
    limit + 1 bytes, so a file swapped in after a size check cannot get past the limit."""
    try:
        fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    except OSError:
        raise ReadError("missing or unsafe object") from None
    with os.fdopen(fd, "rb") as handle:
        if not stat.S_ISREG(os.fstat(handle.fileno()).st_mode):
            raise ReadError("missing or unsafe object")
        data = handle.read(limit + 1)
    if len(data) > limit:
        raise ReadError("object exceeds limit")
    return data


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
        if entry.resolve() != entry or not stat.S_ISDIR(entry.stat().st_mode):
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


def read_journals(root):
    names = listing(root)
    journals = {}
    for name in names:
        try:
            data = read_bounded(root / name, MAX_JOURNAL)
        except ReadError:
            raise ReadError("invalid/oversized journal metadata") from None
        record = json.loads(data)
        if record["job"]["id"] != name.split("/")[2]:
            raise ReadError("journal filename/identity mismatch")
        journals[name] = {"sha256": hashlib.sha256(data).hexdigest(), "record": record}
    if listing(root) != names:
        raise ReadError("journal inventory changed during read")
    for name, item in journals.items():
        if hashlib.sha256(read_bounded(root / name, MAX_JOURNAL)).hexdigest() != item["sha256"]:
            raise ReadError("journal changed during read")
    return json.dumps({"journals": journals}, sort_keys=True, separators=(",", ":"))


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
