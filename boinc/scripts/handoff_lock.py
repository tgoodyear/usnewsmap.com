#!/usr/bin/env python3
"""The hand-off controller's local expiring lock (#1367), held on the BOINC host.

One JSON record, scheduler/controller.lock.json under the hand-off root, written atomically
under a short flock on a sidecar file. onprem-step.sh runs this for `--target handoff --step
lock-acquire|lock-renew|lock-release|lock-status` and for the fence check on mutating hand-off
steps. The clock is this host's, so controller and host clock skew cannot shorten a lease.

    handoff_lock.py --root DIR [--plan NAME] acquire --holder ID --host H --pid N [--ttl 300]
                    [--drain-dir DIR --drain-wait SECS]
    handoff_lock.py --root DIR renew   --holder ID --generation N [--ttl 300]
    handoff_lock.py --root DIR release --holder ID --generation N [--clean]
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
import time
from datetime import datetime, timedelta, timezone
from pathlib import Path

DEFAULT_TTL = 300
MAX_TTL = 3600
RECORD = "scheduler/controller.lock.json"
SIDECAR = "scheduler/.controller.lock.flock"
# A plan that ticks beside others (--plan-scope, #1836) has a record of its own, so plans do not
# hold each other out; the plan names match batch_scheduler.py's PLAN_SCOPE_NAME.
PLAN = re.compile(r"[a-z0-9][a-z0-9-]{0,62}")
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


def record_name(plan):
    return f"scheduler/plans/{plan}/controller.lock.json" if plan else RECORD


def sidecar_name(plan):
    return f"scheduler/plans/{plan}/.controller.lock.flock" if plan else SIDECAR


@contextlib.contextmanager
def exclusive(root, plan=None):
    directory = root / sidecar_name(plan).rsplit("/", 1)[0]
    directory.mkdir(mode=0o700, parents=True, exist_ok=True)
    with (root / sidecar_name(plan)).open("a") as sidecar:
        fcntl.flock(sidecar, fcntl.LOCK_EX)
        yield


def read(root, plan=None):
    path = root / record_name(plan)
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


def write(root, record, plan=None):
    path = root / record_name(plan)
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


STEP_LOCK = "usnm-boinc-step.lock"
HANDOFF_LOCK = "usnm-boinc-handoff.lock"
# Writer preference: an exclusive acquirer of the hand-off lock first takes this gate exclusively
# and keeps it while it waits, so no new shared acquirer gets past it and a stream of shared steps
# cannot starve it. A shared acquirer also takes the gate exclusively (a turnstile), but only
# briefly: gate, hand-off lock shared, gate released at once, before any holder recording. Order everywhere: gate, hand-off lock, plan lock, step lock.
GATE = HANDOFF_LOCK + ".gate"
HOLDER_SUFFIX = ".holder"
POLL_FIRST = 0.02
POLL_MAX = 0.5


def holder_path(directory, name, pid=None, shared=False):
    base = Path(directory) / (name + HOLDER_SUFFIX)
    return Path(f"{base}.{pid}") if shared else base


def note_holder(directory, name, step, plan=None, shared=False, pid=None):
    """Record who holds the lock `name` (pid, step, plan, acquired_at) beside it, atomically.
    Shared holders each get a file of their own, named by pid. Returns the path."""
    pid = os.getpid() if pid is None else pid
    path = holder_path(directory, name, pid, shared)
    info = {"pid": pid, "step": step, "plan": plan, "acquired_at": stamp(now()),
            "mode": "shared" if shared else "exclusive"}
    with tempfile.NamedTemporaryFile("w", dir=path.parent, prefix=".holder-", delete=False) as out:
        temporary = Path(out.name)
        json.dump(info, out, sort_keys=True)
    os.replace(temporary, path)
    return path


def clear_holder(path):
    with contextlib.suppress(OSError):
        Path(path).unlink()


def _alive(pid):
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return False
    except OSError:
        return True
    return True


def holders(directory, name):
    """The recorded holders of lock `name` whose process still runs, as short texts."""
    found = []
    for path in sorted(Path(directory).glob(name + HOLDER_SUFFIX + "*")):
        try:
            info = json.loads(path.read_text())
            pid = int(info["pid"])
        except (OSError, ValueError, KeyError, TypeError):
            continue
        if not _alive(pid):
            continue
        what = f"pid {pid} step {info.get('step')}"
        if info.get("plan"):
            what += f" plan {info['plan']}"
        found.append(f"{what} since {info.get('acquired_at')} ({info.get('mode')})")
    return found


def holder_text(directory, name):
    found = holders(directory, name)
    return f"{name} held by " + "; ".join(found) if found else f"{name} holder unknown"


def lock_until(handle, mode, deadline, directory, name):
    """flock `handle` in `mode`, polling with a short backoff until the monotonic `deadline`.
    On timeout the error names the recorded holders of lock `name`."""
    delay = POLL_FIRST
    while True:
        try:
            fcntl.flock(handle, mode | fcntl.LOCK_NB)
            return
        except BlockingIOError:
            left = deadline - time.monotonic()
            if left <= 0:
                raise LockError(f"an earlier step still holds the step lock: "
                                f"{holder_text(directory, name)}") from None
            time.sleep(min(delay, left))
            delay = min(delay * 2, POLL_MAX)


def drain(directory, wait, plan=None, step="lock-acquire"):
    """Take each host lock a fenced step runs under, once, and let it go (#1897). A mutating step
    that passed its fence check holds its lock until it ends, so returning from here means no step
    of an earlier generation is still writing. The locks, as onprem-step.sh takes them: the
    hand-off lock (hand-off steps, spool-out, batch-inventory, write-held-pages), taken shared
    and followed by the plan's own with a plan scope, and the step lock (submit-remote).
    The hand-off lock goes through its gate, so a waiting exclusive drain is not starved by a
    stream of shared steps. Raises LockError, naming the holder, if one is still held after `wait`
    seconds in all."""
    directory = Path(directory)
    deadline = time.monotonic() + max(0, wait)
    groups = [[(HANDOFF_LOCK, fcntl.LOCK_SH)] if plan else [(HANDOFF_LOCK, fcntl.LOCK_EX)]]
    if plan:
        groups[0].append((f"usnm-boinc-handoff.plan-{plan}.lock", fcntl.LOCK_EX))
    groups.append([(STEP_LOCK, fcntl.LOCK_EX)])
    for group in groups:
        with contextlib.ExitStack() as held:
            gate = None
            if group[0][0] == HANDOFF_LOCK:
                # Turnstile: always exclusive. Exclusive taker keeps it to the end of the group;
                # a shared taker lets it go as soon as it holds the hand-off lock.
                gate = held.enter_context(open(directory / GATE, "a"))
                lock_until(gate, fcntl.LOCK_EX, deadline, directory, GATE)
            for name, mode in group:
                handle = held.enter_context(open(directory / name, "a"))
                lock_until(handle, mode, deadline, directory, name)
                if gate is not None and plan:
                    fcntl.flock(gate, fcntl.LOCK_UN)
                    gate = None
                path = note_holder(directory, name, step, plan, shared=mode == fcntl.LOCK_SH)
                held.callback(clear_holder, path)


def acquire(root, holder, host, pid, ttl=DEFAULT_TTL, drain_dir=None, drain_wait=0, plan=None):
    """Take the lock. A takeover, which is a new generation over a record its holder never released,
    then drains the host's step locks when drain_dir is given; if that times out, the new
    generation is let go again (still marked as needing a drain) so the next tick can try, and
    the error is raised. After a
    release there is nothing to drain, so the usual tick never waits."""
    fresh, takeover = _acquire(root, holder, host, pid, ttl, plan)
    if drain_dir is not None and takeover:
        try:
            drain(drain_dir, drain_wait, plan)
        except (LockError, OSError):
            release(root, holder, fresh["generation"], plan, clean=False)
            raise
    return fresh


def _acquire(root, holder, host, pid, ttl, plan):
    with exclusive(root, plan):
        at = now()
        record = read(root, plan)
        if record is not None and not expired(record, at):
            raise LockError(f"lock is held: {describe(record, at)}")
        fresh = {"holder": holder, "host": host, "pid": pid,
                 "generation": (record["generation"] if record else 0) + 1,
                 "acquired_at": stamp(at), "renewed_at": stamp(at),
                 "expires_at": stamp(at + timedelta(seconds=ttl))}
        write(root, fresh, plan)
        return fresh, record is not None and not record.get("released")


def check(record, holder, generation, at, *, allow_expired):
    if record is None or record["holder"] != holder or record["generation"] != generation:
        raise LockError(f"lock moved on: {describe(record, at)}")
    if expired(record, at) and not allow_expired:
        raise LockError(f"lock expired: {describe(record, at)}")


def renew(root, holder, generation, ttl=DEFAULT_TTL, plan=None):
    with exclusive(root, plan):
        at = now()
        record = read(root, plan)
        check(record, holder, generation, at, allow_expired=False)
        record["renewed_at"] = stamp(at)
        record["expires_at"] = stamp(at + timedelta(seconds=ttl))
        write(root, record, plan)
        return record


def release(root, holder, generation, plan=None, clean=True):
    with exclusive(root, plan):
        at = now()
        record = read(root, plan)
        if record is None or record["holder"] != holder or record["generation"] != generation:
            return None
        # Kept, expired now, so the generation keeps counting up. "released" tells the next
        # acquire the holder ended on purpose, so it has no step of this holder to drain.
        record["expires_at"] = stamp(at)
        # Not for a takeover that failed to drain: the old holder's step may still run, so the next
        # acquire must drain too.
        if clean:
            record["released"] = True
        write(root, record, plan)
        return record


def fence(root, holder, generation, plan=None):
    with exclusive(root, plan):
        at = now()
        check(read(root, plan), holder, generation, at, allow_expired=False)


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--root", type=Path, required=True)
    parser.add_argument("--plan", help="the plan's own record, for --plan-scope controllers")
    sub = parser.add_subparsers(dest="command", required=True)
    for name in ("acquire", "renew", "release", "fence"):
        one = sub.add_parser(name)
        one.add_argument("--holder", required=True)
        if name == "acquire":
            one.add_argument("--host", required=True)
            one.add_argument("--pid", type=int, required=True)
            one.add_argument("--drain-dir", type=Path)
            one.add_argument("--drain-wait", type=int, default=0)
        else:
            one.add_argument("--generation", type=int, required=True)
        if name == "release":
            one.add_argument("--clean", action="store_true",
                             help="every fenced step of this holder is known to have ended")
        if name in ("acquire", "renew"):
            one.add_argument("--ttl", type=int, default=DEFAULT_TTL)
    sub.add_parser("status")
    one = sub.add_parser("note-holder", help="record who holds a host lock (onprem-step.sh)")
    one.add_argument("--lock-dir", type=Path, required=True)
    one.add_argument("--lock", required=True)
    one.add_argument("--step", required=True)
    one.add_argument("--pid", type=int, required=True)
    one.add_argument("--shared", action="store_true")
    one = sub.add_parser("holders", help="print the live recorded holders of a host lock")
    one.add_argument("--lock-dir", type=Path, required=True)
    one.add_argument("--lock", required=True)
    args = parser.parse_args(argv)
    if args.command in ("note-holder", "holders"):
        if not re.fullmatch(r"usnm-boinc-[A-Za-z0-9._-]+\.lock(\.gate)?", args.lock):
            print("error: invalid lock name", file=sys.stderr)
            return 1
        if args.command == "holders":
            print(holder_text(args.lock_dir, args.lock))
            return 0
        if args.plan is not None and not PLAN.fullmatch(args.plan):
            print("error: invalid plan", file=sys.stderr)
            return 1
        try:
            note_holder(args.lock_dir, args.lock, args.step, args.plan, args.shared, args.pid)
        except OSError as error:
            print(f"error: {error}", file=sys.stderr)
            return 1
        return 0
    try:
        if not args.root.is_dir() or args.root.is_symlink():
            raise LockError("hand-off root is not a directory")
        if args.plan is not None and not PLAN.fullmatch(args.plan):
            raise LockError("invalid plan")
        if args.command != "status" and not HOLDER.fullmatch(args.holder):
            raise LockError("invalid holder")
        if args.command == "acquire" and not HOST.fullmatch(args.host):
            raise LockError("invalid host")
        if args.command in ("acquire", "renew") and not 1 <= args.ttl <= MAX_TTL:
            raise LockError("invalid ttl")
        if args.command == "acquire":
            result = acquire(args.root, args.holder, args.host, args.pid, args.ttl,
                             args.drain_dir, args.drain_wait, args.plan)
        elif args.command == "renew":
            result = renew(args.root, args.holder, args.generation, args.ttl, args.plan)
        elif args.command == "release":
            result = release(args.root, args.holder, args.generation, args.plan, clean=args.clean)
        elif args.command == "fence":
            fence(args.root, args.holder, args.generation, args.plan)
            result = {"fenced": "ok"}
        else:
            record = read(args.root, args.plan)
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
