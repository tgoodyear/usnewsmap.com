#!/usr/bin/env python3
"""The hand-off controller's local expiring lock (#1367), held on the BOINC host.

One JSON record, scheduler/controller.lock.json under the hand-off root, written atomically
under a short flock on a sidecar file. onprem-step.sh runs this for `--target handoff --step
lock-acquire|lock-renew|lock-release|lock-status` and for the fence check on mutating hand-off
steps. The clock is this host's, so controller and host clock skew cannot shorten a lease.

    handoff_lock.py --root DIR acquire --holder ID --host H --pid N [--ttl 300]
    handoff_lock.py --root DIR renew   --holder ID --generation N [--ttl 300]
    handoff_lock.py --root DIR release --holder ID --generation N
    handoff_lock.py --root DIR fence   --holder ID --generation N
    handoff_lock.py --root DIR status
"""

import argparse
import contextlib
import fcntl
import json
import os
import re
import sys
import tempfile
from datetime import datetime, timedelta, timezone
from pathlib import Path

DEFAULT_TTL = 300
MAX_TTL = 3600
RECORD = "scheduler/controller.lock.json"
SIDECAR = "scheduler/.controller.lock.flock"
HOLDER = re.compile(r"[A-Za-z0-9][A-Za-z0-9._-]{0,127}")
HOST = re.compile(r"[A-Za-z0-9][A-Za-z0-9._-]{0,254}")


class LockError(Exception):
    pass


def now():
    return datetime.now(timezone.utc).replace(microsecond=0)


def stamp(moment):
    return moment.strftime("%Y-%m-%dT%H:%M:%SZ")


def parse(text):
    return datetime.strptime(text, "%Y-%m-%dT%H:%M:%SZ").replace(tzinfo=timezone.utc)


@contextlib.contextmanager
def exclusive(root):
    directory = root / "scheduler"
    directory.mkdir(mode=0o700, exist_ok=True)
    with (root / SIDECAR).open("a") as sidecar:
        fcntl.flock(sidecar, fcntl.LOCK_EX)
        yield


def read(root):
    path = root / RECORD
    if not path.exists():
        return None
    if path.is_symlink() or not path.is_file() or path.stat().st_size > 65536:
        raise LockError("unsafe lock record")
    record = json.loads(path.read_text())
    if (not isinstance(record, dict) or not isinstance(record.get("generation"), int)
            or not all(isinstance(record.get(key), str) for key in
                       ("holder", "host", "acquired_at", "renewed_at", "expires_at"))):
        raise LockError("malformed lock record")
    return record


def write(root, record):
    path = root / RECORD
    with tempfile.NamedTemporaryFile("w", dir=path.parent, prefix=".lock-", delete=False) as out:
        temporary = Path(out.name)
        json.dump(record, out, sort_keys=True)
        out.write("\n")
        out.flush()
        os.fsync(out.fileno())
    os.replace(temporary, path)
    fd = os.open(path.parent, os.O_RDONLY)
    try:
        os.fsync(fd)
    finally:
        os.close(fd)


def expired(record, at):
    return parse(record["expires_at"]) <= at


def describe(record, at):
    if record is None:
        return "no lock record"
    gap = int((parse(record["expires_at"]) - at).total_seconds())
    when = f"expires in {gap} s" if gap > 0 else f"expired {(-gap) // 60} min ago"
    return (f"holder {record['holder']} on host {record['host']} pid {record.get('pid')} "
            f"generation {record['generation']}, {when}")


def acquire(root, holder, host, pid, ttl=DEFAULT_TTL):
    with exclusive(root):
        at = now()
        record = read(root)
        if record is not None and not expired(record, at):
            raise LockError(f"lock is held: {describe(record, at)}")
        fresh = {"holder": holder, "host": host, "pid": pid,
                 "generation": (record["generation"] if record else 0) + 1,
                 "acquired_at": stamp(at), "renewed_at": stamp(at),
                 "expires_at": stamp(at + timedelta(seconds=ttl))}
        write(root, fresh)
        return fresh


def check(record, holder, generation, at, *, allow_expired):
    if record is None or record["holder"] != holder or record["generation"] != generation:
        raise LockError(f"lock moved on: {describe(record, at)}")
    if expired(record, at) and not allow_expired:
        raise LockError(f"lock expired: {describe(record, at)}")


def renew(root, holder, generation, ttl=DEFAULT_TTL):
    with exclusive(root):
        at = now()
        record = read(root)
        check(record, holder, generation, at, allow_expired=False)
        record["renewed_at"] = stamp(at)
        record["expires_at"] = stamp(at + timedelta(seconds=ttl))
        write(root, record)
        return record


def release(root, holder, generation):
    with exclusive(root):
        at = now()
        record = read(root)
        if record is None or record["holder"] != holder or record["generation"] != generation:
            return None
        # Kept, expired now, so the generation keeps counting up.
        record["expires_at"] = stamp(at)
        write(root, record)
        return record


def fence(root, holder, generation):
    with exclusive(root):
        at = now()
        check(read(root), holder, generation, at, allow_expired=False)


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--root", type=Path, required=True)
    sub = parser.add_subparsers(dest="command", required=True)
    for name in ("acquire", "renew", "release", "fence"):
        one = sub.add_parser(name)
        one.add_argument("--holder", required=True)
        if name == "acquire":
            one.add_argument("--host", required=True)
            one.add_argument("--pid", type=int, required=True)
        else:
            one.add_argument("--generation", type=int, required=True)
        if name in ("acquire", "renew"):
            one.add_argument("--ttl", type=int, default=DEFAULT_TTL)
    sub.add_parser("status")
    args = parser.parse_args(argv)
    try:
        if not args.root.is_dir() or args.root.is_symlink():
            raise LockError("hand-off root is not a directory")
        if args.command != "status" and not HOLDER.fullmatch(args.holder):
            raise LockError("invalid holder")
        if args.command == "acquire" and not HOST.fullmatch(args.host):
            raise LockError("invalid host")
        if args.command in ("acquire", "renew") and not 1 <= args.ttl <= MAX_TTL:
            raise LockError("invalid ttl")
        if args.command == "acquire":
            result = acquire(args.root, args.holder, args.host, args.pid, args.ttl)
        elif args.command == "renew":
            result = renew(args.root, args.holder, args.generation, args.ttl)
        elif args.command == "release":
            result = release(args.root, args.holder, args.generation)
        elif args.command == "fence":
            fence(args.root, args.holder, args.generation)
            result = {"fenced": "ok"}
        else:
            record = read(args.root)
            at = now()
            result = {"lock": record, "now": stamp(at), "summary": describe(record, at),
                      "expired": record is None or expired(record, at)}
            try:
                from zoneinfo import ZoneInfo
                if record:
                    eastern = parse(record["expires_at"]).astimezone(ZoneInfo("America/New_York"))
                    result["expires_at_eastern"] = eastern.strftime("%Y-%m-%d %H:%M:%S %Z")
            except Exception:
                pass
    except (LockError, OSError, ValueError) as error:
        print(f"error: {error}", file=sys.stderr)
        return 1
    print(json.dumps(result, sort_keys=True))
    return 0


if __name__ == "__main__":
    sys.exit(main())
