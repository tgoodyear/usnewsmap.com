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
from pathlib import Path
import re
import shlex
import subprocess
import time

HERE = Path(__file__).resolve().parent


def run(args):
    root = f"/project/quality/{args.campaign}"
    command = [
        "pct", "exec", str(args.server_ct), "--",
        "flock", "-n", "/run/usnm-boinc-step.lock",
        "timeout", "--signal=TERM", "--kill-after=30s", "20m",
        "podman", "exec", "usnm-boinc-server",
        "usnm-boinc", "quality", "collect", "--campaign", args.campaign,
        "--root", root, "--spool", "/project/usnmocr/results/0",
        "--qwen-root", root + "/no-qwen", "--qwen-spool", root + "/no-qwen",
        "--out-dir", root + "/out",
    ]
    subprocess.run(["ssh", "-o", "BatchMode=yes", "-o", "ConnectTimeout=15",
                    f"root@{args.server_host}", shlex.join(command)],
                   check=True, timeout=1260)
    subprocess.run(["bash", str(HERE / "quality-results-pull.sh"),
                    "--campaign", args.campaign, "--server-host", args.server_host,
                    "--server-ct", str(args.server_ct), "--ctid", str(args.ctid),
                    "--snapshot-only"], check=True, timeout=300)
    # Read the existing certificate-auth DSN only inside the preparation container.
    # A failed load is retried in full next tick; load.py's keys make this idempotent.
    load = [
        "/opt/usnm-quality-feed/venv/bin/python", "/opt/usnm-quality-feed/load.py",
        "results", "--campaign", args.campaign, "--engine", args.engine_image,
        "--file", f"/var/lib/usnm-quality-feed/results/{args.campaign}/out/pages.parquet",
    ]
    script = ('set -eu; exec ' + shlex.join(load) +
              ' --dsn "$(cat /etc/usnm/quality-db/dsn)"')
    subprocess.run(["pct", "exec", str(args.ctid), "--",
                    "timeout", "--signal=TERM", "--kill-after=30s", "10m",
                    "sh", "-c", script], check=True, timeout=660)
    return {"campaign": args.campaign, "loaded_at": time.time(),
            "snapshot": f"{root}/out/pages.parquet"}


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--campaign", required=True)
    parser.add_argument("--engine-image", required=True)
    parser.add_argument("--server-host", default="pve.tjg.fyi")
    parser.add_argument("--server-ct", type=int, default=230)
    parser.add_argument("--ctid", type=int, default=215)
    parser.add_argument("--lock", type=Path, default=Path("/run/usnm-quality-results.lock"))
    args = parser.parse_args(argv)
    if (not re.fullmatch(r"[a-z0-9][a-z0-9-]{0,40}", args.campaign)
            or not re.fullmatch(r"sha256:[a-f0-9]{64}", args.engine_image)
            or not re.fullmatch(r"[a-z0-9][a-z0-9.-]*", args.server_host)
            or min(args.ctid, args.server_ct) < 1):
        parser.error("invalid campaign, engine image, server host or container id")
    with args.lock.open("a") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        print(json.dumps(run(args)), flush=True)


if __name__ == "__main__":
    main()
