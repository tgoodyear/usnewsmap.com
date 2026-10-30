#!/usr/bin/env python3
"""Durable source requests for the existing singleton Qwen feeder, not a submitter."""
import argparse
from collections import Counter
import fcntl
import hashlib
import json
import os
from pathlib import Path
import re
import sys
import tempfile
import time
import urllib.request


PAGE = re.compile(r"sn\d+_\d{4}-\d{2}-\d{2}_ed-\d+_seq-\d+")
BATCH = re.compile(r"([a-z]+_[a-zA-Z0-9]+)_ver(0[1-9]|[1-9][0-9])")


def specification(source, max_pages, campaign):
    if not isinstance(source, str):
        raise ValueError("source must be a page ID, exact batch_verNN, or sn number")
    if PAGE.fullmatch(source):
        kind = "page"
    elif BATCH.fullmatch(source):
        kind = "batch"
    elif re.fullmatch(r"sn\d+", source):
        kind = "sn"
    else:
        raise ValueError("source must be a page ID, exact batch_verNN, or sn number")
    if type(max_pages) is not int or not 1 <= max_pages <= 10000:
        raise ValueError("max_pages must be between 1 and 10000")
    if not re.fullmatch(r"[a-z0-9][a-z0-9-]{0,40}", campaign):
        raise ValueError("invalid campaign")
    return {"schema": 1, "source": source, "kind": kind, "max_pages": max_pages,
            "campaign": campaign}


def digest(value):
    return hashlib.sha256(json.dumps(value, sort_keys=True, separators=(",", ":")).encode()).hexdigest()


def matches(row, spec):
    if spec["kind"] == "page":
        return row["doc_id"] == spec["source"]
    if spec["kind"] == "sn":
        return row["doc_id"].split("_", 1)[0] == spec["source"]
    batch = BATCH.fullmatch(spec["source"])
    return row["batch"] == batch[1] and int(row.get("batch_version") or 1) == int(batch[2])


def save(path, value):
    with tempfile.NamedTemporaryFile(mode="w", dir=path.parent, prefix=path.name + ".",
                                     suffix=".tmp", delete=False) as stream:
        temporary = Path(stream.name)
        try:
            json.dump(value, stream, sort_keys=True)
            stream.flush()
            os.fsync(stream.fileno())
            temporary.replace(path)
        finally:
            temporary.unlink(missing_ok=True)
    descriptor = os.open(path.parent, os.O_RDONLY | os.O_DIRECTORY)
    try:
        os.fsync(descriptor)
    finally:
        os.close(descriptor)


def read(path):
    record = json.loads(path.read_text())
    spec = record["spec"]
    if (specification(spec["source"], spec["max_pages"], spec["campaign"]) != spec
            or digest(spec) != record["request_id"]
            or digest(record["pages"]) != record["pages_sha256"]
            or not record["pages"]
            or len(record["pages"]) > record["spec"]["max_pages"]
            or len({p["doc_id"] for p in record["pages"]}) != len(record["pages"])
            or any(not matches(p, record["spec"]) for p in record["pages"])):
        raise ValueError("source request manifest integrity failure")
    return record


def priority_index(index, state, campaign, seen, excluded_requests=()):
    """Prioritize frozen request members while retaining normal idle replenishment."""
    rows = [json.loads(line) for line in index.splitlines()]
    indexed = {row["doc_id"]: row for row in rows}
    for path in sorted((state / "requests").glob("*.json"), key=lambda p: p.stat().st_mtime_ns):
        record = read(path)
        if record["spec"]["campaign"] != campaign or record["request_id"] in excluded_requests:
            continue
        wanted = []
        for page in record["pages"]:
            doc = page["doc_id"]
            if doc in seen or doc not in indexed:
                continue
            row = indexed[doc]
            if (page["sha256"] != row["sha256"] or page["bytes"] != int(row["bytes"])
                    or page["batch"] != row["batch"]
                    or int(page["batch_version"]) != int(row.get("batch_version") or 1)):
                raise ValueError("requested source changed in scan index: " + doc)
            wanted.append(row)
        if wanted:
            return b"".join((json.dumps(row) + "\n").encode() for row in wanted), record["request_id"]
    return index, None


def register(args, spec):
    import qwen_feed as feed

    identity = digest(spec)
    root = args.state / "requests"
    root.mkdir(mode=0o700, parents=True, exist_ok=True)
    path = root / (identity + ".json")
    with (root / (identity + ".lock")).open("a") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        if path.exists():
            return read(path)
        with urllib.request.urlopen(args.scan_server.rstrip("/") + "/index/full.jsonl", timeout=300) as r:
            index = b"".join(line for line in r if matches(json.loads(line), spec))
        if not index:
            raise ValueError("source has no indexed scans; source acquisition is required")
        ct = ["pct", "exec", str(args.prep_ct), "--"]
        work = "/var/lib/usnm-quality-feed/work/source-" + identity
        feed.command(ct + ["mkdir", "-p", work])
        feed.command(ct + ["tee", work + "/index.jsonl"], index)
        feed.command(ct + ["tee", work + "/staged.txt"], b"")
        feed.command(ct + ["/opt/usnm-quality-feed/venv/bin/python",
                          "/opt/usnm-quality-feed/quality_feed.py", "select",
                          "--index", work + "/index.jsonl", "--staged", work + "/staged.txt",
                          "--curated", "/srv/curated", "--count", str(spec["max_pages"]),
                          "--out", work + "/selected.jsonl", "--non-english", "--order", "damage",
                          "--damage-dsn-file", "/etc/usnm/quality-db/dsn",
                          "--scan-server", args.scan_server])
        pages = [json.loads(line) for line in feed.command(ct + ["cat", work + "/selected.jsonl"]).splitlines()]
        if not pages:
            raise ValueError("source has no eligible scored non-English real-image pages")
        record = {"request_id": identity, "spec": spec, "pages": pages,
                  "pages_sha256": digest(pages), "registered_at": time.time()}
        save(path, record)
        return read(path)


def inspect(args, record):
    import qwen_feed as feed

    pages = record["pages"]
    sample = b"".join((json.dumps(p) + "\n").encode() for p in pages)
    native = json.loads(feed.remote(args.server_host, [
        "pct", "exec", str(args.server_ct), "--", "podman", "exec", "-i", "usnm-boinc-server",
        "python3", "/project/ops/qwen-feed/qwen_submit.py", "--source-status", "--sample", "-"], sample))
    expected = {p["doc_id"] for p in pages}
    if set(native["pages"]) != expected:
        raise ValueError("native source status does not cover the exact request membership")
    ct = ["pct", "exec", str(args.prep_ct), "--", "/opt/usnm-quality-feed/venv/bin/python"]
    prepared = json.loads(feed.command(ct + ["-c", (
        "from pathlib import Path; import json; "
        f"p=Path('/var/lib/usnm-quality-feed/{args.campaign}'); "
        "print(json.dumps({n:dict(line.split('\\t',1) for line in (p/n).read_text().splitlines() "
        "if '\\t' in line) if (p/n).exists() else {} for n in ('done.txt','skipped.txt')}))"
    )]))
    inventory = json.loads(feed.remote(args.archive_host, [
        "python3", "/opt/usnm-quality-feed/archive_alto.py",
        "--pages", "-", "--archives", "/tank/locbulk/ocr", "--out", "/tank/locstats/qwen-alto"], b""))
    if not isinstance(inventory.get("available_archives"), list):
        raise ValueError("archive helper lacks retained-source inventory")
    retained = set(inventory["available_archives"])
    counts = Counter()
    waiting = []
    for doc, value in native["pages"].items():
        status = value["status"]
        if status == "pending":
            if doc in prepared["skipped.txt"]:
                status = "preparation_rejected"
            elif doc in prepared["done.txt"]:
                status = "awaiting_publication"
            else:
                status = "awaiting_preparation"
                waiting.append(doc)
        counts[status] += 1
    unfinished_sources = sum(counts[k] for k in ("pending_audit", "awaiting_preparation", "awaiting_publication"))
    unresolved_jobs = native["published_workunits"] - native["agreed_workunits"]
    if (native["error_workunits"] or native["history_unavailable_workunits"]
            or native["source_conflict_pages"]):
        state = "needs_science_review"
    elif unfinished_sources:
        state = "preparing_and_publishing"
    elif not native["published_workunits"]:
        state = "needs_science_review"
    elif unresolved_jobs and native["unsent_replicas"] and not native["running_replicas"]:
        state = "awaiting_dispatch_or_admission"
    elif unresolved_jobs:
        state = "awaiting_host_results"
    else:
        state = "native_consensus_complete"
    waiting_set = set(waiting)
    result = {
        "request_id": record["request_id"], "source": record["spec"]["source"],
        "pages_sha256": record["pages_sha256"], "selected_pages": len(pages),
        "selection_cap_reached": len(pages) == record["spec"]["max_pages"],
        "state": state, "source_dispositions": dict(counts),
        "published_workunits": native["published_workunits"],
        "agreed_workunits": native["agreed_workunits"],
        "unsent_replicas": native["unsent_replicas"], "running_replicas": native["running_replicas"],
        "returned_replicas": native["returned_replicas"], "invalid_replicas": native["invalid_replicas"],
        "error_workunits": native["error_workunits"],
        "history_unavailable_workunits": native["history_unavailable_workunits"],
        "source_conflict_pages": native["source_conflict_pages"],
        "reconstructed_source_pages": sum(
            value.get("provenance") == "verified_reconstruction"
            and value["status"] not in ("source_conflict", "source_provenance_unavailable")
            for value in native["pages"].values()),
        "waiting_source_pages": len(waiting),
        "waiting_original_archive_pages": sum(
            p["doc_id"] in waiting_set
            and f"{p['batch']}_ver{int(p['batch_version']):02d}.tar.bz2" not in retained for p in pages),
        "note": "Consensus is not accuracy; existing collectors retain alternatives. "
                "GPU cohort/finite admission and all source/storage/quorum guards remain unchanged.",
    }
    save(args.state / "requests" / (record["request_id"] + ".progress"), result)
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--mode", choices=("register", "inspect"), required=True)
    parser.add_argument("--state", type=Path, default=Path("/var/lib/usnm-qwen-feed"))
    parser.add_argument("--campaign", required=True)
    parser.add_argument("--server-host", required=True)
    parser.add_argument("--server-ct", type=int, default=230)
    parser.add_argument("--prep-ct", type=int, default=215)
    parser.add_argument("--scan-server", required=True)
    parser.add_argument("--archive-host", required=True)
    args = parser.parse_args()
    payload = json.loads(sys.stdin.read(65537))
    spec = specification(payload["source"], payload["max_pages"], args.campaign)
    identity = digest(spec)
    try:
        if args.mode == "register":
            record = register(args, spec)
            result = {"request_id": identity, "selected_pages": len(record["pages"]),
                      "pages_sha256": record["pages_sha256"]}
        else:
            result = inspect(args, read(args.state / "requests" / (identity + ".json")))
    except ValueError as error:
        print(str(error), file=sys.stderr)
        raise SystemExit(2) from error
    print(json.dumps(result, sort_keys=True))


if __name__ == "__main__":
    main()
