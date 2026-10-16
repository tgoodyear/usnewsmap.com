#!/usr/bin/env python3
"""Manage large-scale BOINC OCR quality campaigns (prepare, launch, status, restore).

Provides a single quiet CLI for staging, submitting, monitoring, and restoring
multi-thousand-workunit `usnm_quality` campaigns on Azure Container Apps without
manual shell/SAS/YAML plumbing:

  python3 scripts/boinc-quality-campaign.py status --campaign v1-wave6
  python3 scripts/boinc-quality-campaign.py prepare --campaign v1-wave7 --count 10000 --shards 20
  python3 scripts/boinc-quality-campaign.py launch --campaign v1-wave7 --shards 20 --parallelism 10
  python3 scripts/boinc-quality-campaign.py restore --env prod
"""

from __future__ import annotations

import argparse
import datetime as dt
import io
import json
import os
import re
import subprocess
import sys
import tarfile
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
DEFAULT_HANDOFF_ACCOUNT = "stusnmoffapzy"
DEFAULT_HANDOFF_CONTAINER = "boinc-handoff"
DEFAULT_SCANS_ACCOUNT = "usnmscansstprodffapzy"
DEFAULT_SCANS_CONTAINER = "scans"
DEFAULT_BOINC_RG = "usnm-boinc-rg-prod"
DEFAULT_BRIDGE_JOB = "usnm-boinc-bridge-caj-prod"
DEFAULT_SERVER_STATUS_URL = "https://boinc.usnewsmap.com/server_status.php?counts=1"


def run_az(args: list[str], capture_json: bool = True):
    cmd = ["az", *args, "--only-show-errors"]
    res = subprocess.run(cmd, capture_output=True, text=True, check=False)
    if res.returncode != 0:
        raise RuntimeError(f"az {' '.join(args[:4])} failed: {res.stderr.strip()}")
    if capture_json:
        out = res.stdout.strip()
        return json.loads(out) if out else None
    return res.stdout.strip()


def generate_sas(account: str, container: str, permissions: str = "r", hours: int = 24) -> str:
    expiry = (dt.datetime.now(dt.timezone.utc) + dt.timedelta(hours=hours)).strftime("%Y-%m-%dT%H:%MZ")
    return run_az(
        [
            "storage",
            "container",
            "generate-sas",
            "--account-name",
            account,
            "--name",
            container,
            "--permissions",
            permissions,
            "--expiry",
            expiry,
            "--auth-mode",
            "login",
            "--as-user",
            "-o",
            "tsv",
        ],
        capture_json=False,
    )


def fetch_server_counts(url: str = DEFAULT_SERVER_STATUS_URL) -> dict[str, int]:
    try:
        xml_text = urllib.request.urlopen(url, timeout=10).read().decode("utf-8", errors="replace")
    except Exception:
        return {}
    counts: dict[str, int] = {}
    for tag in (
        "results_ready_to_send",
        "results_in_progress",
        "workunits_waiting_for_validation",
        "workunits_waiting_for_assimilation",
        "workunits_waiting_for_deletion",
    ):
        m = re.search(rf"<{tag}>(\d+)</{tag}>", xml_text)
        if m:
            counts[tag] = int(m.group(1))
    return counts


def fetch_campaign_status(
    campaign: str,
    shards: int,
    handoff_account: str,
    handoff_container: str,
    sas: str,
) -> tuple[int, int, int, int, int, list[tuple[int, str, int, int, int, float]]]:
    total_staged = 0
    total_submitted = 0
    total_errors = 0
    done_shards = 0
    active_shards = 0
    rows = []

    for i in range(shards):
        url = (
            f"https://{handoff_account}.blob.core.windows.net/"
            f"{handoff_container}/quality/{campaign}/status/shard-{i}.json?{sas}"
        )
        try:
            data = json.loads(urllib.request.urlopen(url, timeout=5).read())
        except Exception:
            continue
        active_shards += 1
        state = str(data.get("state", "unknown"))
        staged = int(data.get("staged", 0))
        submitted = int(data.get("submitted", 0))
        errs = len(data.get("skipped_errors", []))
        elapsed = float(data.get("elapsed_s", 0.0))
        if state == "done":
            done_shards += 1
        total_staged += staged
        total_submitted += submitted
        total_errors += errs
        rows.append((i, state, staged, submitted, errs, elapsed))
    return active_shards, done_shards, total_staged, total_submitted, total_errors, rows


def cmd_status(args: argparse.Namespace) -> int:
    import time

    campaign = args.campaign
    sas = args.handoff_sas or generate_sas(args.handoff_account, args.handoff_container, "rl", hours=4)

    while True:
        active_shards, done_shards, total_staged, total_submitted, total_errors, rows = fetch_campaign_status(
            campaign, args.shards, args.handoff_account, args.handoff_container, sas
        )
        counts = fetch_server_counts(args.server_status_url)
        if not args.wait or done_shards >= args.shards:
            break
        print(
            f"[{dt.datetime.now(dt.timezone.utc).strftime('%H:%M:%SZ')}] {campaign}: "
            f"done={done_shards}/{args.shards} staged={total_staged} submitted={total_submitted} "
            f"({total_submitted * 2} tasks) err={total_errors} | "
            f"ready_to_send={counts.get('results_ready_to_send', 0)} in_progress={counts.get('results_in_progress', 0)}",
            flush=True,
        )
        time.sleep(args.poll_seconds)
    if args.json:
        print(
            json.dumps(
                {
                    "campaign": campaign,
                    "active_shards": active_shards,
                    "done_shards": done_shards,
                    "total_shards": args.shards,
                    "total_staged": total_staged,
                    "total_submitted": total_submitted,
                    "total_errors": total_errors,
                    "server": counts,
                    "shards": [
                        {
                            "shard": i,
                            "state": st,
                            "staged": stg,
                            "submitted": sub,
                            "errors": err,
                            "elapsed_s": el,
                        }
                        for i, st, stg, sub, err, el in rows
                    ],
                },
                indent=2,
            )
        )
        return 0

    print(f"=== Campaign: {campaign} ({done_shards}/{args.shards} shards done, {active_shards} active) ===")
    for i, st, stg, sub, err, el in rows:
        print(f"  shard-{i:02d}: state={st:15s} staged={stg:4d} submitted={sub:4d} err={err:2d} ({el:6.1f}s)")
    print(
        f"Totals: staged={total_staged} | submitted={total_submitted} "
        f"({total_submitted * 2} replica tasks) | errors={total_errors}"
    )
    if counts:
        print(
            "Server queue: "
            f"ready_to_send={counts.get('results_ready_to_send', 0)} | "
            f"in_progress={counts.get('results_in_progress', 0)} | "
            f"waiting_validation={counts.get('workunits_waiting_for_validation', 0)} | "
            f"waiting_assimilation={counts.get('workunits_waiting_for_assimilation', 0)}"
        )
    return 0


def cmd_restore(args: argparse.Namespace) -> int:
    execs = run_az(
        [
            "containerapp",
            "job",
            "execution",
            "list",
            "-g",
            args.resource_group,
            "-n",
            args.job_name,
            "--query",
            "[?properties.status=='Running' || properties.status=='Processing'].name",
            "-o",
            "json",
        ]
    )
    if execs and not args.force:
        print(
            f"Refusing to restore {args.job_name} while {len(execs)} execution(s) are still Running: "
            f"{', '.join(execs)}",
            file=sys.stderr,
        )
        return 1

    job_doc = run_az(
        [
            "containerapp",
            "job",
            "show",
            "-g",
            args.resource_group,
            "-n",
            args.job_name,
            "-o",
            "json",
        ]
    )
    base_names = {
        "AZURE_CLIENT_ID",
        "KEY_VAULT_NAME",
        "DB_HOST",
        "DB_NAME",
        "MASTER_HOST",
        "PROJECT_DIR",
        "HANDOFF_DIR",
    }
    container = job_doc["properties"]["template"]["containers"][0]
    container["command"] = ["/usr/local/bin/bridge-job.sh"]
    container.pop("args", None)
    container["env"] = [e for e in container.get("env", []) if e.get("name") in base_names]
    job_doc["properties"]["configuration"]["replicaCompletionCount"] = 1
    job_doc["properties"]["configuration"]["parallelism"] = 1

    tmp_spec = Path("/tmp/boinc_bridge_restore_spec.json")
    tmp_spec.write_text(
        json.dumps(
            {
                "location": job_doc["location"],
                "identity": job_doc["identity"],
                "properties": {
                    "environmentId": job_doc["properties"]["environmentId"],
                    "configuration": job_doc["properties"]["configuration"],
                    "template": job_doc["properties"]["template"],
                },
            }
        )
    )
    try:
        run_az(
            [
                "rest",
                "--method",
                "PATCH",
                "--uri",
                f"{job_doc['id']}?api-version=2024-03-01",
                "--body",
                f"@{tmp_spec}",
                "-o",
                "none",
            ],
            capture_json=False,
        )
    finally:
        tmp_spec.unlink(missing_ok=True)

    print(f"Restored {args.job_name} to canonical single-replica bridge-job.sh configuration.")
    return 0


def cmd_prepare(args: argparse.Namespace) -> int:
    manifest = Path(args.manifest)
    if not manifest.is_file():
        print(f"error: manifest file not found: {manifest}", file=sys.stderr)
        return 1
    out_dir = Path(args.out_dir)
    shards_dir = out_dir / "shards"
    shards_dir.mkdir(parents=True, exist_ok=True)

    rows = [json.loads(line) for line in manifest.read_text().splitlines() if line.strip()]
    if args.count and len(rows) > args.count:
        rows = rows[: args.count]

    buckets: list[list[dict]] = [[] for _ in range(args.shards)]
    for idx, row in enumerate(rows):
        buckets[idx % args.shards].append(row)

    for s_idx, s_rows in enumerate(buckets):
        tar_path = shards_dir / f"shard-s{s_idx}.tar.gz"
        with tarfile.open(tar_path, "w:gz", compresslevel=1) as tf:
            sample_bytes = ("".join(json.dumps(r) + "\n" for r in s_rows)).encode("utf-8")
            ti = tarfile.TarInfo("sample.jsonl")
            ti.size = len(sample_bytes)
            tf.addfile(ti, io.BytesIO(sample_bytes))

    print(
        f"Prepared {len(rows)} pages across {args.shards} shards in {shards_dir} "
        f"(~{len(rows) // max(args.shards, 1)} pages/shard)."
    )
    return 0


def build_parser() -> argparse.ArgumentParser:
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = p.add_subparsers(dest="subcommand", required=True)

    st = sub.add_parser("status", help="Show shard progress and live BOINC queue counts for a campaign")
    st.add_argument("--campaign", required=True, help="Campaign name (e.g. v1-wave6)")
    st.add_argument("--shards", type=int, default=20, help="Number of shards in the campaign (default: 20)")
    st.add_argument("--handoff-account", default=DEFAULT_HANDOFF_ACCOUNT)
    st.add_argument("--handoff-container", default=DEFAULT_HANDOFF_CONTAINER)
    st.add_argument("--handoff-sas", default=os.environ.get("HANDOFF_SAS", ""))
    st.add_argument("--server-status-url", default=DEFAULT_SERVER_STATUS_URL)
    st.add_argument("--wait", action="store_true", help="Poll until all shards reach state=done")
    st.add_argument("--poll-seconds", type=int, default=30, help="Polling interval in seconds when --wait is set")
    st.add_argument("--json", action="store_true", help="Emit machine-readable JSON")
    st.set_defaults(func=cmd_status)

    pr = sub.add_parser("prepare", help="Split a non-overlapping campaign manifest into shard archives")
    pr.add_argument("--campaign", required=True, help="Campaign name (e.g. v1-wave6)")
    pr.add_argument("--manifest", required=True, help="Path to non-overlapping sample.jsonl manifest")
    pr.add_argument("--out-dir", required=True, help="Local directory to write shard archives")
    pr.add_argument("--count", type=int, default=10000, help="Maximum pages to include (default: 10000)")
    pr.add_argument("--shards", type=int, default=20, help="Number of shards (default: 20)")
    pr.set_defaults(func=cmd_prepare)

    rs = sub.add_parser("restore", help="Restore the BOINC bridge job to its canonical single-replica spec")
    rs.add_argument("--env", default="prod", help="Environment name (default: prod)")
    rs.add_argument("--resource-group", default=DEFAULT_BOINC_RG)
    rs.add_argument("--job-name", default=DEFAULT_BRIDGE_JOB)
    rs.add_argument("--force", action="store_true", help="Restore even if executions are still running")
    rs.set_defaults(func=cmd_restore)

    return p


def main(argv: list[str] | None = None) -> int:
    parser = build_parser()
    args = parser.parse_args(argv)
    return args.func(args)


if __name__ == "__main__":
    sys.exit(main())
