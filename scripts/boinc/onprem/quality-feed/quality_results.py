#!/usr/bin/env python3
"""Collect canonical QUALITY results, pull a fresh snapshot and upsert it over certificate auth.

Runs on the preparation host, never on a workstation. Collection holds CT230's bridge
lock; a separate host lock serializes the whole collect/pull/load pass. Failed or timed
out phases never advance to loading an old snapshot. The existing summary refresh timer
continues to publish ranking updates.
"""
import argparse
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


def collect(args, command):
    wait = args.step_lock_wait_secs
    ssh = ["ssh", "-o", "BatchMode=yes", "-o", "ConnectTimeout=15",
           f"root@{args.server_host}", shlex.join(command)]
    if not wait:
        subprocess.run(ssh, check=True, timeout=1260)
        return
    print(json.dumps({"phase": "step_lock_wait", "budget_seconds": wait}), file=sys.stderr, flush=True)
    result = subprocess.run(ssh, stderr=subprocess.PIPE, text=True, timeout=1260 + wait)
    if result.stderr:
        print(result.stderr, file=sys.stderr, end="", flush=True)
    acquired = any(line.startswith(LOCK_ACQUIRED + " ") for line in result.stderr.splitlines())
    if result.returncode == 75 and not acquired:
        print(json.dumps({"phase": "step_lock_timeout", "budget_seconds": wait}),
              file=sys.stderr, flush=True)
    elif result.returncode:
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


def run(args):
    root = f"/project/quality/{args.campaign}"
    qwen_root = f"/project/qwen/{args.qwen_campaign}" if args.qwen_campaign else root + "/no-qwen"
    command = [
        "pct", "exec", str(args.server_ct), "--",
        "flock",
    ]
    if args.step_lock_wait_secs:
        command += [
            "-w", str(args.step_lock_wait_secs), "-E", "75", "/run/usnm-boinc-step.lock",
            "sh", "-c",
            'printf "%s %s\\n" ' + LOCK_ACQUIRED + ' "$(date -u +%FT%TZ)" >&2; exec "$@"',
            "sh",
        ]
    else:
        command += ["-n", "/run/usnm-boinc-step.lock"]
    command += [
        "podman", "exec", "usnm-boinc-server",
        "timeout", "--signal=TERM", "--kill-after=30s", "20m",
        args.collector, "quality", "collect", "--campaign", args.campaign,
        "--root", root, "--spool", "/project/usnmocr/results/0",
        "--qwen-root", qwen_root,
        "--qwen-spool", "/project/usnmocr/results/8" if args.qwen_campaign else root + "/no-qwen",
        "--out-dir", root + "/out",
    ]
    if args.incremental:
        command.extend(["--cache", root + "/collected-parts",
                        "--max-dirty-units", str(args.max_dirty_units)])
    collect(args, command)
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


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--campaign", required=True)
    parser.add_argument("--engine-image", required=True)
    parser.add_argument("--qwen-campaign", help="join this native Qwen campaign's canonical batch-8 readings")
    parser.add_argument("--incremental", action="store_true",
                        help="reuse native content-addressed rows; incomplete warming never loads")
    parser.add_argument("--max-dirty-units", type=int, default=8)
    parser.add_argument("--step-lock-wait-secs", type=int, default=0,
                        help="bounded collection-only step-lock admission, 0 (nonblocking) to 60 seconds")
    parser.add_argument("--collector", default="usnm-boinc",
                        help="native collector executable inside the server (dedicated persistent path in services)")
    parser.add_argument("--server-host", default="pve.tjg.fyi")
    parser.add_argument("--server-ct", type=int, default=230)
    parser.add_argument("--ctid", type=int, default=215)
    parser.add_argument("--lock", type=Path, default=Path("/run/usnm-quality-results.lock"))
    args = parser.parse_args(argv)
    if (not re.fullmatch(r"[a-z0-9][a-z0-9-]{0,40}", args.campaign)
            or not re.fullmatch(r"sha256:[a-f0-9]{64}", args.engine_image)
            or not re.fullmatch(r"[a-z0-9][a-z0-9.-]*", args.server_host)
            or min(args.ctid, args.server_ct, args.max_dirty_units) < 1
            or not 0 <= args.step_lock_wait_secs <= 60):
        parser.error("invalid campaign, engine image, server host, container id, dirty-unit or lock-wait bound")
    if args.qwen_campaign and not re.fullmatch(r"[a-z0-9][a-z0-9-]{0,40}", args.qwen_campaign):
        parser.error("invalid native Qwen campaign")
    with args.lock.open("a") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        print(json.dumps(run(args)), flush=True)


if __name__ == "__main__":
    main()
