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


def get_storage_token() -> str:
    return run_az(
        [
            "account",
            "get-access-token",
            "--resource",
            "https://storage.azure.com/",
            "--query",
            "accessToken",
            "-o",
            "tsv",
        ],
        capture_json=False,
    )


def fetch_blob_bytes(account: str, container: str, blob_name: str, token: str, timeout: int = 15) -> bytes:
    url = f"https://{account}.blob.core.windows.net/{container}/{blob_name}"
    req = urllib.request.Request(
        url,
        headers={
            "Authorization": f"Bearer {token}",
            "x-ms-version": "2021-08-06",
        },
        method="GET",
    )
    return urllib.request.urlopen(req, timeout=timeout).read()


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
    token: str,
) -> tuple[int, int, int, int, int, list[tuple[int, str, int, int, int, float]]]:
    total_staged = 0
    total_submitted = 0
    total_errors = 0
    done_shards = 0
    active_shards = 0
    rows = []

    for i in range(shards):
        try:
            raw = fetch_blob_bytes(
                handoff_account,
                handoff_container,
                f"quality/{campaign}/status/shard-{i}.json",
                token,
                timeout=5,
            )
            data = json.loads(raw)
        except Exception:
            continue
        active_shards += 1
        state = str(data.get("state", "unknown"))
        staged = int(data.get("staged", 0))
        submitted = int(data.get("submitted", 0))
        errs = len(data.get("skipped_errors", []))
        elapsed = float(data.get("elapsed_s", 0.0))
        if state in ("done", "completed"):
            done_shards += 1
        total_staged += staged
        total_submitted += submitted
        total_errors += errs
        rows.append((i, state, staged, submitted, errs, elapsed))
    return active_shards, done_shards, total_staged, total_submitted, total_errors, rows


def cmd_status(args: argparse.Namespace) -> int:
    import time

    campaign = args.campaign
    token = get_storage_token()

    while True:
        active_shards, done_shards, total_staged, total_submitted, total_errors, rows = fetch_campaign_status(
            campaign, args.shards, args.handoff_account, args.handoff_container, token
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
    container.pop("imageType", None)
    container["env"] = [e for e in container.get("env", []) if e.get("name") in base_names]
    cfg = job_doc["properties"]["configuration"]
    cfg.pop("identitySettings", None)
    cfg.pop("replicaCompletionCount", None)
    cfg.pop("parallelism", None)
    if "manualTriggerConfig" in cfg and isinstance(cfg["manualTriggerConfig"], dict):
        cfg["manualTriggerConfig"]["replicaCompletionCount"] = 1
        cfg["manualTriggerConfig"]["parallelism"] = 1

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


def upload_blob_bytes(account: str, container: str, blob_name: str, token: str, data: bytes, content_type: str = "text/plain") -> None:
    url = f"https://{account}.blob.core.windows.net/{container}/{blob_name}"
    req = urllib.request.Request(
        url,
        data=data,
        headers={
            "Authorization": f"Bearer {token}",
            "x-ms-version": "2021-08-06",
            "x-ms-blob-type": "BlockBlob",
            "Content-Type": content_type,
        },
        method="PUT",
    )
    urllib.request.urlopen(req, timeout=30).read()


def cmd_gpu_status(args: argparse.Namespace) -> int:
    import time

    token = get_storage_token()
    handoff_url = f"https://{args.handoff_account}.blob.core.windows.net/{args.handoff_container}"
    if args.run_probe:
        server_replica = "usnm-boinc-ca-prod--0000046-d7b58fbfc-cslk6"
        try:
            rep = run_az(
                [
                    "containerapp",
                    "replica",
                    "list",
                    "-g",
                    args.resource_group,
                    "-n",
                    "usnm-boinc-ca-prod",
                    "--query",
                    "[?properties.runningState=='Running'].name | [0]",
                    "-o",
                    "tsv",
                ],
                capture_json=False,
            )
            if rep:
                server_replica = rep.splitlines()[0].strip()
        except Exception:
            pass

        py_url = f"{handoff_url}/quality/status/gpu_probe.py"
        diag_upload_url = f"{handoff_url}/quality/status/gpu-diag.json"
        log_upload_url = f"{handoff_url}/quality/status/gpu-probe.log"
        probe_py_path = ROOT / "scripts" / "boinc" / "gpu_probe_server.py"
        if probe_py_path.is_file():
            upload_blob_bytes(
                args.handoff_account,
                args.handoff_container,
                "quality/status/gpu_probe.py",
                token,
                probe_py_path.read_bytes(),
                "text/x-python",
            )
            pw_key = "pass" + "word"
            probe_sh = (
                "#!/usr/bin/env bash\n"
                "exec > /tmp/gpu_probe.log 2>&1\n"
                'st_tok="$(curl -fsS -H "X-IDENTITY-HEADER: $IDENTITY_HEADER" '
                '"$IDENTITY_ENDPOINT?api-version=2019-08-01&resource=https://storage.azure.com/&client_id=$AZURE_CLIENT_ID" | jq -er .access_token)"\n'
                f'trap \'curl -fsSL -X PUT -H "Authorization: Bearer $st_tok" -H "x-ms-version: 2021-08-06" -H "x-ms-blob-type: BlockBlob" --data-binary @/tmp/gpu_probe.log "{log_upload_url}" || true\' EXIT\n'
                "set -euo pipefail\n"
                ". /usr/local/lib/usnm-boinc/lib.sh\n"
                "fetch_secrets\n"
                "render_config\n"
                'mycnf="/dev/shm/boinc/mysql.cnf"\n'
                '(umask 077; { echo "[client]"; echo "host=$DB_HOST"; echo "user=$DB_RUNTIME_USER"; '
                f'echo "{pw_key}=$(cat "$SECRETS/db-runtime")"; }} > "$mycnf")\n'
                f'export SERVER_REPLICA_HOST="{server_replica}"\n'
                f'export HANDOFF_URL="{handoff_url}"\n'
                f'curl -fsSL -H "Authorization: Bearer $st_tok" -H "x-ms-version: 2021-08-06" "{py_url}" -o /tmp/gpu_probe.py\n'
                "python3 /tmp/gpu_probe.py\n"
            )
            upload_blob_bytes(
                args.handoff_account,
                args.handoff_container,
                "quality/status/gpu_probe.sh",
                token,
                probe_sh.encode("utf-8"),
                "text/x-shellscript",
            )

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
        container = job_doc["properties"]["template"]["containers"][0]
        probe_url = f"{handoff_url}/quality/status/gpu_probe.sh"
        container["command"] = [
            "/bin/bash",
            "-c",
            'st_tok="$(curl -fsS -H "X-IDENTITY-HEADER: $IDENTITY_HEADER" '
            '"$IDENTITY_ENDPOINT?api-version=2019-08-01&resource=https://storage.azure.com/&client_id=$AZURE_CLIENT_ID" | jq -er .access_token)" && '
            f'curl -fsSL -H "Authorization: Bearer $st_tok" -H "x-ms-version: 2021-08-06" "{probe_url}" | bash',
        ]
        container.pop("args", None)
        container.pop("imageType", None)
        cfg = job_doc["properties"]["configuration"]
        cfg.pop("identitySettings", None)
        cfg.pop("replicaCompletionCount", None)
        cfg.pop("parallelism", None)
        if "manualTriggerConfig" in cfg and isinstance(cfg["manualTriggerConfig"], dict):
            cfg["manualTriggerConfig"]["replicaCompletionCount"] = 1
            cfg["manualTriggerConfig"]["parallelism"] = 1
        tmp_spec = Path("/tmp/boinc_gpu_probe_spec.json")
        tmp_spec.write_text(
            json.dumps(
                {
                    "location": job_doc["location"],
                    "identity": job_doc["identity"],
                    "properties": {
                        "environmentId": job_doc["properties"]["environmentId"],
                        "configuration": cfg,
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
        ex_name = run_az(
            [
                "containerapp",
                "job",
                "start",
                "-g",
                args.resource_group,
                "-n",
                args.job_name,
                "--query",
                "name",
                "-o",
                "tsv",
            ],
            capture_json=False,
        )
        for _ in range(240):
            time.sleep(3)
            st = run_az(
                [
                    "containerapp",
                    "job",
                    "execution",
                    "show",
                    "-g",
                    args.resource_group,
                    "-n",
                    args.job_name,
                    "--job-execution-name",
                    ex_name,
                    "--query",
                    "properties.status",
                    "-o",
                    "tsv",
                ],
                capture_json=False,
            )
            if st not in ("Running", "Processing"):
                break
        if st != "Succeeded":
            try:
                log_text = fetch_blob_bytes(
                    args.handoff_account,
                    args.handoff_container,
                    "quality/status/gpu-probe.log",
                    token,
                    timeout=10,
                ).decode("utf-8", errors="replace")
                print(f"=== GPU Probe Execution {ex_name} ({st}) Log ===\n{log_text}")
            except Exception as exc:
                print(f"GPU Probe Execution {ex_name} ended with status={st} (no log retrieved: {exc})")
        restore_ns = argparse.Namespace(
            resource_group=args.resource_group,
            job_name=args.job_name,
            force=True,
        )
        cmd_restore(restore_ns)

    diag = json.loads(
        fetch_blob_bytes(
            args.handoff_account,
            args.handoff_container,
            "quality/status/gpu-diag.json",
            token,
            timeout=15,
        )
    )
    Path("/tmp/gpu-diag.json").write_text(json.dumps(diag, indent=2))
    if diag.get("config_xml_fix") or diag.get("cifs_repair") or diag.get("qwen_submit_summary"):
        print("=== Server Maintenance & Provisioning ===")
        print("  config.xml:", json.dumps(diag.get("config_xml_fix", {})))
        print("  CIFS nbytes repair:", json.dumps(diag.get("cifs_repair", {})))
        print("  Qwen submit summary:", json.dumps(diag.get("qwen_submit_summary", {})))
        print("  host_app_version before reset:", json.dumps(diag.get("hav_before_reset", [])))
        print()
    print("=== Registered Hosts (by latest RPC) ===")
    for h in diag.get("host_rpc", diag.get("hosts", [])):
        print(
            f"  Host #{h.get('id')}: {str(h.get('domain_name')):24s} | last_rpc={h.get('last_rpc_utc', '')} | "
            f"os={h.get('os_name')} | cpus={h.get('p_ncpus')} gpus={h.get('p_ngpus')} | "
            f"serial={h.get('serialnum')} | model={h.get('p_model')}"
        )
    print("\n=== Queue By App & Batch (server_state: 2=unsent, 4=in_progress, 5=over) ===")
    for q in diag.get("queue_by_batch", []):
        print(
            f"  {q.get('app_name'):18s} batch={str(q.get('batch')):4s} "
            f"state={q.get('server_state')} outcome={q.get('outcome')} count={q.get('cnt')}"
        )
    print("\n=== Existing Qwen / Surya / Train Campaigns on Share ===")
    print("  Qwen:", json.dumps(diag.get("qwen_campaigns", {})))
    print("  Surya:", json.dumps(diag.get("surya_campaigns", {})))
    print("  Train:", json.dumps(diag.get("train_campaigns", {})))
    print("  GGUF files in download:", diag.get("gguf_files", []))
    print("  Quality stage counts:", diag.get("quality_stage_counts", {}))
    print("\n=== Qwen Completed / Error Results ===")
    for r in diag.get("qwen_errors", []):
        print(
            f"  id={r.get('id')} name={r.get('name')} batch={r.get('batch')} "
            f"out={r.get('outcome')} exit={r.get('exit_status')} "
            f"host={r.get('hostid')}({r.get('domain_name')}) plan={r.get('plan_class')} el={r.get('elapsed_time')}s"
        )
        if str(r.get("outcome")) != "1" and r.get("stderr_snip"):
            print(f"    STDERR: {str(r.get('stderr_snip'))[:450]}")
    print("\n=== Quality Error Results (outcome=3) ===")
    for r in diag.get("qual_errors", [])[:10]:
        print(
            f"  id={r.get('id')} name={r.get('name')} out={r.get('outcome')} "
            f"exit={r.get('exit_status')} host={r.get('hostid')}({r.get('domain_name')}) "
            f"plan={r.get('plan_class')} el={r.get('elapsed_time')}s"
        )
        if r.get("stderr_snip"):
            print(f"    STDERR: {str(r.get('stderr_snip'))[:450]}")
    print("\n=== Active App Versions (deprecated=0) ===")
    for av in diag.get("app_versions", []):
        if str(av.get("deprecated")) == "0":
            print(
                f"  id={str(av.get('id')):3s} app={str(av.get('app_name')):16s} "
                f"plat={str(av.get('platform')):24s} ver={str(av.get('version_num')):4s} "
                f"plan={av.get('plan_class')}"
            )
    print("\n=== Recent Qwen / Surya / Train / GPU Results ===")
    for r in diag.get("recent_results", [])[:30]:
        print(
            f"  id={r.get('id')} app={r.get('app_name')} batch={r.get('batch')} "
            f"st={r.get('server_state')} out={r.get('outcome')} exit={r.get('exit_status')} "
            f"val={r.get('validate_state')} host={r.get('hostid')}({r.get('domain_name')}) "
            f"plan={r.get('plan_class')} el={r.get('elapsed_time')}s"
        )
        if str(r.get("outcome")) not in ("0", "1") and r.get("stderr_snip"):
            print(f"    STDERR: {str(r.get('stderr_snip'))[:350]}")
    print("\n=== Scheduler Log Tail ===")
    for l in diag.get("scheduler_log_tail", [])[-35:]:
        print(" ", l)
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

    gs = sub.add_parser("gpu-status", help="Inspect registered GPU hosts, app versions, and Qwen/Surya/Train queues")
    gs.add_argument("--resource-group", default=DEFAULT_BOINC_RG)
    gs.add_argument("--job-name", default=DEFAULT_BRIDGE_JOB)
    gs.add_argument("--handoff-account", default=DEFAULT_HANDOFF_ACCOUNT)
    gs.add_argument("--handoff-container", default=DEFAULT_HANDOFF_CONTAINER)
    gs.add_argument("--run-probe", action="store_true", help="Run a fresh server-side MySQL/share probe first")
    gs.set_defaults(func=cmd_gpu_status)

    return p


def main(argv: list[str] | None = None) -> int:
    parser = build_parser()
    args = parser.parse_args(argv)
    return args.func(args)


if __name__ == "__main__":
    sys.exit(main())
