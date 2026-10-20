#!/usr/bin/env python3
"""Read-only, bounded inventory for the opt-in OCR batch controller."""

import argparse
import json
import os
import re
import subprocess
import time
from pathlib import Path


ID = re.compile(r"[0-9a-f]{64}")
STAGES = {"prepared", "reserved", "submitting", "submitted", "published", "failed"}
MAX_JOBS = 10000
MAX_FILES = 100000
QUERY = """
SELECT
 (SELECT COUNT(*) FROM workunit WHERE assimilate_state != 2),
 (SELECT COUNT(*) FROM host WHERE rpc_time > UNIX_TIMESTAMP() - 86400);
SELECT name, assimilate_state FROM workunit
 WHERE name REGEXP '^usnm_[0-9a-f]{64}$'
 ORDER BY name
 LIMIT 10001
"""


SPOOL_FILE = re.compile(
    r"usnm_([0-9a-f]{64})(__file_(result\.json|text\.txt|layout\.xml)|_error|_no_output_files)")


def spool_candidates(results, jobs):
    """The regular files under `results` named as a known job's assimilator output, with their
    name match. Walked with os.scandir: a directory listing carries each entry's type, so the
    tens of thousands of other apps' results (usnm_quality's, mostly) cost no stat each, which
    over the SMB project share made the inventory take minutes (#932). Symlinks are neither
    followed nor yielded, as with rglob, and every entry counts towards MAX_FILES."""
    count = 0
    stack = [results]
    while stack:
        with os.scandir(stack.pop()) as entries:
            for entry in entries:
                count += 1
                if count > MAX_FILES:
                    raise ValueError("inventory spool limit reached; archive drained results")
                if entry.is_dir(follow_symlinks=False):
                    stack.append(entry.path)
                elif entry.is_file(follow_symlinks=False):
                    match = SPOOL_FILE.fullmatch(entry.name)
                    if match and match[1] in jobs:
                        yield Path(entry.path), match


def inventory(root, results, counts, nonce, pending_ids=(), existing_ids=None):
    if not re.fullmatch(r"[0-9a-f]{32}", nonce):
        raise ValueError("invalid inventory nonce")
    if len(counts) != 2 or any(type(n) is not int or n < 0 for n in counts):
        raise ValueError("invalid inventory counts")
    if len(pending_ids) > MAX_JOBS or len(set(pending_ids)) != len(pending_ids) or \
            any(not ID.fullmatch(job_id) for job_id in pending_ids):
        raise ValueError("invalid/oversized pending job inventory")
    existing_ids = pending_ids if existing_ids is None else existing_ids
    if len(existing_ids) > MAX_JOBS or len(set(existing_ids)) != len(existing_ids) or \
            any(not ID.fullmatch(job_id) for job_id in existing_ids) or \
            not set(pending_ids) <= set(existing_ids):
        raise ValueError("invalid/oversized existing workunit inventory")
    if not root.is_dir() or not results.is_dir() or (root / "jobs").is_symlink():
        raise ValueError("missing/unsafe inventory directories")
    jobs = {}
    for path in sorted((root / "jobs").glob("*/record.json")):
        if len(jobs) >= MAX_JOBS:
            raise ValueError("inventory job limit reached; archive drained journals")
        job_id = path.parent.name
        if not ID.fullmatch(job_id) or path.is_symlink() or path.parent.is_symlink() or \
                path.stat().st_size > 131072:
            raise ValueError("invalid inventory journal")
        record = json.loads(path.read_text())
        if record["job"]["id"] != job_id or record["stage"] not in STAGES:
            raise ValueError("invalid inventory record")
        jobs[job_id] = {"stage": record["stage"], "spool": None}
    seen = {}
    locations = {}
    for path, match in spool_candidates(results, jobs):
        if not path.resolve().is_relative_to(results.resolve()):
            raise ValueError("spool escapes results directory")
        locations.setdefault(match[1], set()).add(str(path.parent))
        if len(locations[match[1]]) > 1:
            raise ValueError("multiple assimilator spools for one job, including partial outputs")
        seen.setdefault((match[1], str(path.parent)), set()).add(match[2])
    for (job_id, directory), names in seen.items():
        complete = {"__file_result.json", "__file_text.txt", "__file_layout.xml"} <= names
        if complete or names & {"_error", "_no_output_files"}:
            if jobs[job_id]["spool"] not in (None, directory):
                raise ValueError("multiple assimilator spools for one job")
            jobs[job_id]["spool"] = directory
    return {
        "schema_version": 1,
        "nonce": nonce,
        "created_at": int(time.time()),
        "pending_workunits": counts[0],
        "pending_jobs": list(pending_ids),
        "existing_jobs": list(existing_ids),
        "active_hosts": counts[1],
        "jobs": jobs,
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, required=True)
    parser.add_argument("--results", type=Path, required=True)
    parser.add_argument("--mysql-defaults-file", required=True)
    parser.add_argument("--database", required=True)
    parser.add_argument("--nonce", required=True)
    parser.add_argument("--out", type=Path, required=True)
    args = parser.parse_args()
    if not re.fullmatch(r"[A-Za-z0-9_]+", args.database):
        parser.error("invalid database name")
    output = subprocess.run(
        ["mysql", f"--defaults-extra-file={args.mysql_defaults_file}", "--ssl",
         "--ssl-verify-server-cert", "--ssl-ca=/usr/local/share/usnm/mysql-ca.pem",
         "--batch", "--raw", "--skip-column-names", f"--database={args.database}",
         "--execute", QUERY],
        check=True, text=True, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, timeout=60,
    ).stdout.strip().splitlines()
    counts = output[0].split("\t") if output else []
    if len(counts) != 2 or any(not value.isdecimal() for value in counts) or \
            any(not re.fullmatch(r"usnm_[0-9a-f]{64}\t-?[0-9]+", value) for value in output[1:]):
        raise ValueError("invalid database inventory")
    rows = [value.split("\t") for value in output[1:]]
    payload = inventory(args.root, args.results, [int(value) for value in counts], args.nonce,
                        [name.removeprefix("usnm_") for name, state in rows if int(state) != 2],
                        [name.removeprefix("usnm_") for name, _ in rows])
    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_text(json.dumps(payload, sort_keys=True) + "\n")


if __name__ == "__main__":
    main()
