#!/usr/bin/env python3
"""Collect canonical QUALITY results, pull a fresh snapshot and upsert it over certificate auth.

Runs on the preparation host, never on a workstation. Collection holds CT230's bridge
lock; a separate host lock serializes the whole collect/pull/load pass. Failed or timed
out phases never advance to loading an old snapshot. The existing summary refresh timer
continues to publish ranking updates.

With --warm-without-lock the lock is held only to publish. The row parts are computed first,
without it, by `quality collect --warm-only` (parts are keyed on the hashes of their inputs, so
nothing needs keeping still), and the locked collection then finds them cached: it re-keys the
units, checks nothing changed and writes the tables, under a compute budget and a hard timeout
that bound how long the lock is held (#1817).
"""
import argparse
import datetime
import fcntl
import json
import os
from pathlib import Path
import re
import shlex
import signal
import subprocess
import sys
import time
import uuid

HERE = Path(__file__).resolve().parent
LOCK_ACQUIRED = "USNM_QUALITY_STEP_LOCK_ACQUIRED"
LOCK_RELEASED = "USNM_QUALITY_STEP_LOCK_RELEASED"


def stamp(lines, receipt):
    """The UTC time on a receipt line, or None."""
    for line in lines:
        if line.startswith(receipt + " "):
            try:
                return datetime.datetime.strptime(line.split()[1], "%Y-%m-%dT%H:%M:%SZ").replace(
                    tzinfo=datetime.timezone.utc)
            except ValueError:
                return None
    return None


def replay_collection_diagnostics(stderr):
    if isinstance(stderr, bytes):
        stderr = stderr.decode("utf-8", errors="replace")
    if stderr:
        print(stderr, file=sys.stderr, end="", flush=True)
    lines = (stderr or "").splitlines()
    taken, released = stamp(lines, LOCK_ACQUIRED), stamp(lines, LOCK_RELEASED)
    if taken and released:
        # What the other steps waited behind, to the second.
        print(json.dumps({"phase": "step_lock_held", "seconds": (released - taken).total_seconds()}),
              file=sys.stderr, flush=True)
    return any(line.startswith(LOCK_ACQUIRED + " ") for line in lines)


def progress_of(stderr):
    """The collector's own progress line (`quality collect` prints one to stderr), or None."""
    if isinstance(stderr, bytes):
        stderr = stderr.decode("utf-8", errors="replace")
    for line in reversed((stderr or "").splitlines()):
        if line.startswith('{"phase":'):
            try:
                event = json.loads(line)
            except ValueError:
                continue
            if isinstance(event.get("progress"), dict):
                return event["progress"]
    return None


def collect(args, command, limit=1260):
    wait = args.step_lock_wait_secs
    ssh = ["ssh", "-o", "BatchMode=yes", "-o", "ConnectTimeout=15",
           f"root@{args.server_host}", shlex.join(command)]
    if not wait:
        subprocess.run(ssh, check=True, timeout=limit)
        return
    print(json.dumps({"phase": "step_lock_wait", "budget_seconds": wait}), file=sys.stderr, flush=True)
    try:
        result = subprocess.run(ssh, stderr=subprocess.PIPE, text=True, timeout=limit + wait)
    except subprocess.TimeoutExpired as error:
        acquired = replay_collection_diagnostics(error.stderr)
        print(json.dumps({"phase": "collection_transport_timeout",
                          "budget_seconds": limit + wait, "step_lock_acquired": acquired}),
              file=sys.stderr, flush=True)
        raise
    acquired = replay_collection_diagnostics(result.stderr)
    if result.returncode == 75 and not acquired:
        print(json.dumps({"phase": "step_lock_timeout", "budget_seconds": wait}),
              file=sys.stderr, flush=True)
    elif result.returncode:
        progress = progress_of(result.stderr) if acquired else None
        if progress and progress.get("deferred"):
            # More arrived since the parts were warmed than the locked run may compute. Nothing
            # was published or loaded; the next run warms them without the lock.
            print(json.dumps({"phase": "collection_deferred", "dirty": progress.get("dirty")}),
                  file=sys.stderr, flush=True)
            raise subprocess.CalledProcessError(75, result.args, stderr=result.stderr)
        print(json.dumps({"phase": "collection_failed" if acquired else "remote_admission_failed",
                          "exit": result.returncode}), file=sys.stderr, flush=True)
    result.check_returncode()
    if not acquired:
        raise RuntimeError("Missing step-lock acquisition receipt; refusing snapshot transfer")


def transfer(command, timeout=300):
    with subprocess.Popen(command, start_new_session=True) as process:
        try:
            code = process.wait(timeout=timeout)
        except subprocess.TimeoutExpired:
            for sig in (signal.SIGTERM, signal.SIGKILL):
                try:
                    os.killpg(process.pid, sig)
                except ProcessLookupError:
                    pass
                if sig == signal.SIGTERM:
                    try:
                        process.wait(timeout=10)
                    except subprocess.TimeoutExpired:
                        pass
            process.wait()
            raise
        if code:
            raise subprocess.CalledProcessError(code, command)


def warm_parts(args, collect_args):
    """Compute and cache the row parts without the step lock, as many rounds as the budget allows.

    Each round is `quality collect --warm-only`, niced, which computes dirty units for up to
    --warm-round-secs and prints a progress line saying whether any were left. The step lock is
    for the publication that follows. A failed round fails the root, so the lock is never taken
    for a collection already known to fail."""
    ssh = ["ssh", "-o", "BatchMode=yes", "-o", "ConnectTimeout=15", f"root@{args.server_host}"]
    deadline = time.monotonic() + args.warm_budget_secs
    rounds = 0
    while True:
        remaining = deadline - time.monotonic()
        if rounds and remaining < 60:
            print(json.dumps({"phase": "warm_budget_spent", "rounds": rounds}), file=sys.stderr, flush=True)
            return
        compute = max(30, int(min(args.warm_round_secs, remaining - 30)))
        # A unit started before the compute budget finishes, so the kill waits well past it.
        kill_after = compute + 300
        command = [
            "pct", "exec", str(args.server_ct), "--", "podman", "exec", "usnm-boinc-server",
            "nice", "-n", "15", "timeout", "--signal=TERM", "--kill-after=30s", f"{kill_after}s",
            *collect_args, "--warm-only", "--max-dirty-units", "100000",
            "--max-compute-secs", str(compute),
        ]
        print(json.dumps({"phase": "warm_round", "round": rounds + 1, "compute_secs": compute}),
              file=sys.stderr, flush=True)
        result = subprocess.run([*ssh, shlex.join(command)], stderr=subprocess.PIPE, text=True,
                                timeout=kill_after + 60)
        if result.stderr:
            print(result.stderr, file=sys.stderr, end="", flush=True)
        result.check_returncode()
        rounds += 1
        progress = progress_of(result.stderr)
        if progress is None:
            raise RuntimeError("warm round reported no progress; refusing to publish")
        if not progress.get("deferred"):
            return


def run(args):
    root = f"/project/quality/{args.campaign}"
    qwen_root = f"/project/qwen/{args.qwen_campaign}" if args.qwen_campaign else root + "/no-qwen"
    warm = bool(getattr(args, "warm_without_lock", False))
    if warm and not args.incremental:
        raise ValueError("--warm-without-lock needs --incremental: parts are what it warms")
    # The locked collection's whole life: its compute budget, then the remote kill, so the lock
    # is held for at most about locked_timeout seconds however the collector behaves.
    locked_timeout = getattr(args, "locked_timeout_secs", 150) if warm else 20 * 60
    collect_args = [
        args.collector, "quality", "collect", "--campaign", args.campaign,
        "--root", root, "--spool", "/project/usnmocr/results/0",
        "--qwen-root", qwen_root,
        "--qwen-spool", "/project/usnmocr/results/8" if args.qwen_campaign else root + "/no-qwen",
    ]
    crop_states = getattr(args, "qwen_crop_states", None)
    if args.qwen_campaign and crop_states:
        # Only the crops an agreeing crop-consensus-v2 pair vouched for join (#1434).
        collect_args.extend(["--qwen-crop-states", crop_states])
    pages = getattr(args, "qwen_pages", None)
    if args.qwen_campaign and pages:
        # A bundle with no canonical result joins the pages a pair agreed on (#1435).
        collect_args.extend(["--qwen-pages", pages])
    if args.incremental:
        collect_args.extend(["--cache", root + "/collected-parts"])
    if warm:
        warm_parts(args, collect_args)
    command = [
        "pct", "exec", str(args.server_ct), "--",
        "flock",
    ]
    if args.step_lock_wait_secs:
        command += [
            "-w", str(args.step_lock_wait_secs), "-E", "75", "/run/usnm-boinc-step.lock",
            "sh", "-c",
            # Not exec: the shell stays to say when the lock is let go, so the hold is on record.
            'printf "%s %s\\n" ' + LOCK_ACQUIRED + ' "$(date -u +%FT%TZ)" >&2; "$@"; code=$?; '
            'printf "%s %s\\n" ' + LOCK_RELEASED + ' "$(date -u +%FT%TZ)" >&2; exit $code',
            "sh",
        ]
    else:
        command += ["-n", "/run/usnm-boinc-step.lock"]
    command += [
        "podman", "exec", "usnm-boinc-server",
        "timeout", "--signal=TERM", "--kill-after=30s", f"{locked_timeout}s" if warm else "20m",
        *collect_args, "--out-dir", root + "/out",
    ]
    if args.incremental:
        command.extend(["--max-dirty-units", str(args.max_dirty_units)])
        if warm:
            command.extend(["--max-compute-secs", str(args.locked_compute_secs)])
    collect(args, command, limit=locked_timeout + 60 if warm else 1260)
    snapshot = uuid.uuid4().hex
    transfer(["bash", str(HERE / "quality-results-pull.sh"),
              "--campaign", args.campaign, "--server-host", args.server_host,
              "--server-ct", str(args.server_ct), "--ctid", str(args.ctid),
              "--snapshot-only", "--snapshot-id", snapshot])
    # Read the existing certificate-auth DSN only inside the preparation container.
    # A failed load is retried in full next tick; load.py's keys make this idempotent.
    load = [
        "/opt/usnm-quality-feed/venv/bin/python", "/opt/usnm-quality-feed/load.py",
        "results", "--campaign", args.campaign, "--engine", args.engine_image,
        "--file", f"/var/lib/usnm-quality-feed/results/{args.campaign}/snapshots/{snapshot}/out/pages.parquet",
    ]
    script = ('set -eu; exec ' + shlex.join(load) +
              ' --dsn "$(cat /etc/usnm/quality-db/dsn)"')
    subprocess.run(["pct", "exec", str(args.ctid), "--",
                    "timeout", "--signal=TERM", "--kill-after=30s", "10m",
                    "sh", "-c", script], check=True, timeout=660)
    subprocess.run(["pct", "exec", str(args.ctid), "--", "rm", "-rf",
                    f"/var/lib/usnm-quality-feed/results/{args.campaign}/snapshots/{snapshot}"],
                   check=True, timeout=30)
    return {"campaign": args.campaign, "loaded_at": time.time(),
            "snapshot": f"{root}/out/pages.parquet", "qwen_campaign": args.qwen_campaign}


def native_campaign(value):
    name, _, collector = value.partition("=")
    if (not re.fullmatch(r"[a-z0-9][a-z0-9-]{0,40}", name)
            or (collector and not re.fullmatch(r"/[A-Za-z0-9._/-]+", collector))):
        raise argparse.ArgumentTypeError("expected NAME or NAME=/absolute/collector")
    return name, collector


def run_native(args):
    """One pass over each root. A root that fails, or is still warming its cache, doesn't stop
    the next, and the run reports it at the end."""
    failed = []
    for name, collector in args.native_campaign:
        one = argparse.Namespace(**vars(args))
        one.campaign = one.qwen_campaign = name
        one.collector = collector or args.collector
        try:
            print(json.dumps(run(one)), flush=True)
        except (subprocess.CalledProcessError, subprocess.TimeoutExpired, RuntimeError) as error:
            # 75 is the step lock staying held: the next tick reads this root.
            status = "deferred" if getattr(error, "returncode", None) == 75 else "failed"
            print(json.dumps({"campaign": name, "status": status, "error": str(error)[:300]}), flush=True)
            if status == "failed":
                failed.append(name)
    if failed:
        raise SystemExit("native campaigns failed: " + ", ".join(failed))


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--campaign", help="one QUALITY campaign; needs --qwen-campaign to join readings")
    parser.add_argument("--native-campaign", type=native_campaign, action="append", default=[],
                        metavar="NAME[=COLLECTOR]",
                        help="repeatable in place of --campaign: a root whose QUALITY and native Qwen "
                             "results share its name, with its own collector if named")
    parser.add_argument("--engine-image", required=True)
    parser.add_argument("--qwen-campaign", help="join this native Qwen campaign's canonical batch-8 readings")
    parser.add_argument("--qwen-crop-states", metavar="DIR",
                        help="the server's `qwen compare --crop-states` directory, passed to collectors "
                             "joining Qwen readings so only vouched crops join (needs a collector from #1434 on)")
    parser.add_argument("--qwen-pages", metavar="DIR",
                        help="the server's `qwen compare --pages` directory, passed to collectors joining Qwen "
                             "readings so a bundle with no canonical result joins its agreed pages (#1435)")
    parser.add_argument("--incremental", action="store_true",
                        help="reuse native content-addressed rows; incomplete warming never loads")
    parser.add_argument("--max-dirty-units", type=int, default=8)
    parser.add_argument("--step-lock-wait-secs", type=int, default=0,
                        help="bounded collection-only step-lock admission, 0 (nonblocking) to 60 seconds")
    parser.add_argument("--warm-without-lock", action="store_true",
                        help="compute row parts first without the step lock (`quality collect "
                             "--warm-only`, so the collector must be from #1817 on); the lock is then "
                             "held only to publish. Needs --incremental")
    parser.add_argument("--warm-budget-secs", type=int, default=1500,
                        help="most seconds of warming rounds per root before the locked collection")
    parser.add_argument("--warm-round-secs", type=int, default=600,
                        help="compute budget of one warming round; each round also reads every input once")
    parser.add_argument("--locked-compute-secs", type=int, default=45,
                        help="with --warm-without-lock: start no dirty unit under the lock after this long")
    parser.add_argument("--locked-timeout-secs", type=int, default=150,
                        help="with --warm-without-lock: TERM for the collection under the lock, "
                             "and so the longest it holds it (KILL follows 30 s later)")
    parser.add_argument("--collector", default="usnm-boinc",
                        help="native collector executable inside the server (dedicated persistent path in services)")
    parser.add_argument("--server-host", default="pve.tjg.fyi")
    parser.add_argument("--server-ct", type=int, default=230)
    parser.add_argument("--ctid", type=int, default=215)
    parser.add_argument("--lock", type=Path, default=Path("/run/usnm-quality-results.lock"))
    args = parser.parse_args(argv)
    if bool(args.campaign) == bool(args.native_campaign):
        parser.error("give --campaign or --native-campaign")
    if (not re.fullmatch(r"[a-z0-9][a-z0-9-]{0,40}", args.campaign or args.native_campaign[0][0])
            or not re.fullmatch(r"sha256:[a-f0-9]{64}", args.engine_image)
            or not re.fullmatch(r"[a-z0-9][a-z0-9.-]*", args.server_host)
            or min(args.ctid, args.server_ct, args.max_dirty_units) < 1
            or not 0 <= args.step_lock_wait_secs <= 60):
        parser.error("invalid campaign, engine image, server host, container id, dirty-unit or lock-wait bound")
    if args.warm_without_lock and not (args.incremental and args.step_lock_wait_secs):
        parser.error("--warm-without-lock needs --incremental and --step-lock-wait-secs")
    if (not 60 <= args.warm_budget_secs <= 6 * 3600 or not 30 <= args.warm_round_secs <= 3600
            or not 10 <= args.locked_compute_secs <= 600 or not 30 <= args.locked_timeout_secs <= 600):
        parser.error("invalid warm or locked time bound")
    if args.qwen_campaign and not re.fullmatch(r"[a-z0-9][a-z0-9-]{0,40}", args.qwen_campaign):
        parser.error("invalid native Qwen campaign")
    if args.qwen_crop_states and not re.fullmatch(r"/[A-Za-z0-9._/-]+", args.qwen_crop_states):
        parser.error("--qwen-crop-states must be an absolute path")
    if args.qwen_pages and not re.fullmatch(r"/[A-Za-z0-9._/-]+", args.qwen_pages):
        parser.error("--qwen-pages must be an absolute path")
    with args.lock.open("a") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        if args.native_campaign:
            run_native(args)
        else:
            print(json.dumps(run(args)), flush=True)


if __name__ == "__main__":
    main()
