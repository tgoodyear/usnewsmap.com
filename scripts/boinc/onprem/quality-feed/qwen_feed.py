#!/usr/bin/env python3
"""One bounded replenishment tick on the quality shipper host; schedule with systemd.

Uses the existing quality CT/venv/outbox and shipper. Original ALTO is extracted on
the archive host into its existing read-only locstats export, not downloaded from LoC.
The normal scan fetcher alone handles new corpus manifests through its global pacer.
"""
import argparse
import fcntl
import json
from pathlib import Path
import shlex
import shutil
import subprocess
import time
import urllib.request


def command(args, data=None):
    return subprocess.check_output(args, input=data, timeout=7200)


def remote(host, args, data=None):
    return command(["ssh", "-o", "BatchMode=yes", "-o", "ConnectTimeout=15",
                    "root@" + host, shlex.join(args)], data)


def tick(a):
    state = a.state
    state.mkdir(parents=True, exist_ok=True)
    with (state / "lock").open("a") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        _tick(a)


def _tick(a):
    phase_started = time.monotonic()

    def phase(name, **counts):
        nonlocal phase_started
        now = time.monotonic()
        print(json.dumps({"phase": name, "seconds": now - phase_started, **counts}), flush=True)
        phase_started = now

    ct = ["pct", "exec", str(a.prep_ct), "--"]
    python = ct + ["/opt/usnm-quality-feed/venv/bin/python"]
    helper = "/opt/usnm-quality-feed/"
    inv = json.loads(remote(a.server_host, ["pct", "exec", str(a.server_ct), "--", "podman", "exec",
                                          "usnm-boinc-server", "python3",
                                          "/project/ops/qwen-feed/qwen_submit.py", "--inventory"]))
    pool = json.loads(remote(a.server_host, ["lvs", "--reportformat", "json", "--units", "b", "--nosuffix",
                                            "-o", "lv_size,data_percent", "pve/data"]))["report"][0]["lv"][0]
    pool_free = float(pool["lv_size"]) * (1 - float(pool["data_percent"]) / 100)
    producer_free = shutil.disk_usage(a.outbox).free
    free = min(pool_free, producer_free)
    phase("inventory", fresh_pairs=inv["fresh_pairs"], unresolved_native=inv["unresolved_native"])
    if min(free, inv["free_bytes"]) < a.min_free_gb * 1024**3:
        print(json.dumps({"status": "storage_backpressure", "pool_free_bytes": int(pool_free),
                          "producer_free_bytes": producer_free}))
        return
    camp_dir = a.outbox / a.campaign
    pending = list(camp_dir.glob("c*/READY"))
    if (2 * inv["fresh_pairs"] >= a.max_ready
            or inv["unresolved_native"] >= a.max_unresolved or len(pending) >= a.max_pending):
        print(json.dumps({"status": "queue_backpressure", "ready": inv["ready"],
                          "fresh_pairs": inv["fresh_pairs"], "unresolved_native": inv["unresolved_native"],
                          "pending_chunks": len(pending)}), flush=True)
        return
    # Each new source page creates two independent native GPU replicas.
    count = min(a.chunk_pages, a.max_ready // 2 - inv["fresh_pairs"],
                a.max_unresolved - inv["unresolved_native"],
                max(0, int((min(free, inv["free_bytes"]) - a.min_free_gb * 1024**3)
                           // (a.reserve_mib * 1024**2))))
    if count < 1:
        return
    work = "/var/lib/usnm-quality-feed/work/" + a.campaign
    command(ct + ["mkdir", "-p", work])
    done = command(python + ["-c", (
        "from pathlib import Path; import json; "
        f"p=Path('/var/lib/usnm-quality-feed/{a.campaign}'); "
        "print(json.dumps([line.split('\\t')[0] for n in ('done.txt','skipped.txt') "
        "if (p/n).exists() for line in (p/n).read_text().splitlines() if line]))"
    )])
    deferred_path = a.state / "deferred.json"
    deferred = json.loads(deferred_path.read_text()) if deferred_path.exists() else {}
    seen = set(inv["documents"]) | set(json.loads(done)) | {
        d for d, retry_at in deferred.items() if retry_at > time.time()}
    command(ct + ["tee", work + "/staged.txt"], ("\n".join(sorted(seen)) + "\n").encode())
    with urllib.request.urlopen(a.scan_server.rstrip("/") + "/index/full.jsonl", timeout=300) as r:
        command(ct + ["tee", work + "/index.jsonl"], r.read())
    command(python + [helper + "quality_feed.py", "select", "--index", work + "/index.jsonl",
                     "--staged", work + "/staged.txt", "--curated", "/srv/curated", "--count", str(count),
                     "--out", work + "/selected.jsonl", "--non-english", "--order", "damage",
                     "--damage-dsn-file", "/etc/usnm/quality-db/dsn",
                     "--scan-server", a.scan_server])
    raw = command(ct + ["cat", work + "/selected.jsonl"])
    pages = [json.loads(line) for line in raw.splitlines() if line]
    phase("source_selection", selected=len(pages))
    if not pages:
        print(json.dumps({"status": "waiting_for_crop_sources", "deferred": len(deferred)}))
    else:
        receipt = json.loads(remote(a.archive_host, ["nice", "-n", "19", "ionice", "-c", "3",
                                                    "python3", "/opt/usnm-quality-feed/archive_alto.py",
                                                    "--pages", "-", "--archives", a.archives,
                                                    "--out", a.alto_export], raw))
        phase("original_alto", cached=len(receipt["cached"]), missing=len(receipt["missing"]),
              pending=len(receipt["pending"]))
        for doc in receipt["missing"]:
            deferred[doc] = time.time() + 86400
        tmp = deferred_path.with_suffix(".tmp")
        tmp.write_text(json.dumps(deferred, sort_keys=True))
        tmp.replace(deferred_path)
        cached = set(receipt["cached"])
        usable = [p for p in pages if p["doc_id"] in cached]
        command(ct + ["tee", work + "/pages.jsonl"],
                "".join(json.dumps(p) + "\n" for p in usable).encode())
        if usable:
            command(python + [helper + "quality_feed.py", "run", "--campaign", a.campaign,
                             "--pages", work + "/pages.jsonl", "--curated", "/srv/curated",
                             "--scan-server", a.scan_server, "--outbox", str(a.outbox),
                             "--state", "/var/lib/usnm-quality-feed", "--chunk-pages", str(a.chunk_pages),
                             "--max-pending", str(a.max_pending), "--workers", "2", "--once",
                             "--alto-cache", "/srv/locstats/qwen-alto"])
        phase("prepare_ready", pages=len(usable))
        print(json.dumps({"status": "prepared", "pages": len(usable), "missing_original_alto": receipt["missing"],
                          "pending_original_alto": receipt["pending"],
                          "ready_before": inv["ready"], "highest_damage": pages[0]["damage_rate"]}))


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--campaign", required=True)
    ap.add_argument("--state", type=Path, required=True)
    ap.add_argument("--outbox", type=Path, default=Path("/var/lib/usnm-quality-feed/outbox"))
    ap.add_argument("--prep-ct", type=int, default=215)
    ap.add_argument("--server-host", required=True)
    ap.add_argument("--server-ct", type=int, default=230)
    ap.add_argument("--archive-host", required=True)
    ap.add_argument("--archives", default="/tank/locbulk/ocr")
    ap.add_argument("--alto-export", default="/tank/locstats/qwen-alto")
    ap.add_argument("--scan-server", required=True)
    ap.add_argument("--min-free-gb", type=int, default=50)
    ap.add_argument("--max-ready", type=int, default=1000)
    ap.add_argument("--max-unresolved", type=int, default=2000)
    ap.add_argument("--reserve-mib", type=int, default=64)
    ap.add_argument("--chunk-pages", type=int, default=100)
    ap.add_argument("--max-pending", type=int, default=3)
    a = ap.parse_args()
    import re
    if (not re.fullmatch(r"[a-z0-9][a-z0-9-]{0,40}", a.campaign)
            or not 1 <= a.chunk_pages <= 490 or a.min_free_gb < 1 or a.max_ready < 2 or a.max_pending < 1
            or not 1 <= a.max_unresolved <= 10000 or a.reserve_mib < 64):
        ap.error("invalid campaign or non-positive/out-of-range limits")
    tick(a)


if __name__ == "__main__":
    main()
