#!/usr/bin/env python3
"""Run a Linux workload until a deadline, then reap every descendant before returning."""

import argparse
import ctypes
import math
import os
import signal
import subprocess
import sys
import time


class Interrupted(Exception):
    def __init__(self, signum):
        self.signum = signum


def enable_subreaper():
    if sys.platform != "linux" or not os.path.isdir("/proc"):
        raise RuntimeError("the deadline supervisor requires Linux with /proc")
    if not hasattr(os, "pidfd_open") or not hasattr(signal, "pidfd_send_signal"):
        raise RuntimeError("the deadline supervisor requires pidfd support")
    fd = os.pidfd_open(os.getpid())
    try:
        signal.pidfd_send_signal(fd, 0)
    finally:
        os.close(fd)
    libc = ctypes.CDLL(None, use_errno=True)
    if libc.prctl(36, 1, 0, 0, 0) != 0:  # PR_SET_CHILD_SUBREAPER
        err = ctypes.get_errno()
        raise OSError(err, os.strerror(err))


def process_info(pid):
    try:
        with open(f"/proc/{pid}/stat", "rb") as stream:
            fields = stream.read().rsplit(b")", 1)[1].split()
        return int(fields[1]), fields[0]
    except (FileNotFoundError, ProcessLookupError, PermissionError, IndexError, ValueError):
        return None


def descendants():
    parents = {}
    with os.scandir("/proc") as entries:
        for entry in entries:
            if entry.name.isdigit():
                pid = int(entry.name)
                info = process_info(pid)
                if info and info[1] not in (b"Z", b"X"):
                    parents[pid] = info[0]
    found = {os.getpid()}
    while True:
        more = {pid for pid, parent in parents.items() if parent in found} - found
        if not more:
            return found - {os.getpid()}
        found.update(more)


def still_ours(pid):
    seen = set()
    while pid not in seen and pid > 1:
        if pid == os.getpid():
            return True
        seen.add(pid)
        info = process_info(pid)
        if not info or info[1] in (b"Z", b"X"):
            return False
        pid = info[0]
    return False


def signal_descendant(pid, signum, warned):
    try:
        fd = os.pidfd_open(pid)
        try:
            if still_ours(pid):
                signal.pidfd_send_signal(fd, signum)
        finally:
            os.close(fd)
    except ProcessLookupError:
        pass
    except OSError as exc:
        if pid not in warned:
            print(f"cannot signal descendant {pid}: {exc}", file=sys.stderr)
            warned.add(pid)


def reap_adopted():
    while True:
        try:
            pid, _ = os.waitpid(-1, os.WNOHANG)
        except ChildProcessError:
            return False
        if not pid:
            return True


def cleanup(process, boinc_until):
    term_until = time.monotonic() + 2
    next_renewal = 0
    warned = set()
    scan_warned = False
    while True:
        if (process is None or process.poll() is not None) and not reap_adopted():
            return
        try:
            children = descendants()
        except OSError as exc:
            if not scan_warned:
                print(f"cannot inspect descendants: {exc}; continuing cleanup", file=sys.stderr)
                scan_warned = True
            children = set()

        # A stuck kernel process must not make the timed BOINC hold expire.
        if boinc_until and time.time() >= boinc_until - 30 and time.monotonic() >= next_renewal:
            started = time.time()
            try:
                renewal = subprocess.run(
                    ["boinccmd", "--set_gpu_mode", "never", "120"],
                    stdin=subprocess.DEVNULL,
                    stdout=subprocess.DEVNULL,
                    stderr=subprocess.DEVNULL,
                    timeout=5,
                    check=False,
                )
                renewed = renewal.returncode == 0
            except (OSError, subprocess.TimeoutExpired):
                renewed = False
            if renewed:
                boinc_until = started + 120
                print("extended BOINC GPU suspension while stopping descendants", file=sys.stderr)
            else:
                print("could not extend BOINC GPU suspension; continuing cleanup", file=sys.stderr)
                next_renewal = time.monotonic() + 5

        signum = signal.SIGTERM if time.monotonic() < term_until else signal.SIGKILL
        for pid in children:
            signal_descendant(pid, signum, warned)
        time.sleep(0.1)


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--until", type=float, required=True, help="last time to run, as epoch seconds")
    parser.add_argument("--boinc-until", type=float, default=0, help="timed BOINC suspension expiry")
    parser.add_argument("command", nargs=argparse.REMAINDER)
    args = parser.parse_args(argv)
    command = args.command[1:] if args.command[:1] == ["--"] else args.command
    if (
        not command
        or not math.isfinite(args.until)
        or not math.isfinite(args.boinc_until)
        or args.until <= 0
        or args.boinc_until < 0
    ):
        parser.error("a command and a positive finite deadline are required")
    if args.until <= time.time():
        return 124
    try:
        enable_subreaper()
    except (OSError, RuntimeError) as exc:
        print(f"cannot supervise native workload: {exc}", file=sys.stderr)
        return 125

    def interrupt(signum, _frame):
        raise Interrupted(signum)

    signals = (signal.SIGHUP, signal.SIGINT, signal.SIGTERM)
    previous = {sig: signal.signal(sig, interrupt) for sig in signals}
    process = None
    result = 125
    try:
        process = subprocess.Popen(command, start_new_session=True)
        result = process.wait(timeout=max(0, args.until - time.time()))
        if result < 0:
            result = 128 - result
    except subprocess.TimeoutExpired:
        print("native workload reached its deadline", file=sys.stderr)
        result = 124
    except Interrupted as exc:
        result = 128 + exc.signum
    except OSError as exc:
        print(f"cannot start native workload: {exc}", file=sys.stderr)
        result = 127
    finally:
        for sig in signals:
            signal.signal(sig, signal.SIG_IGN)
        cleanup(process, args.boinc_until)
        for sig, handler in previous.items():
            signal.signal(sig, handler)
    return result


if __name__ == "__main__":
    sys.exit(main())
