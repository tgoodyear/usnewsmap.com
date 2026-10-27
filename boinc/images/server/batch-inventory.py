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
 WHERE name REGEXP '^usnm_(ocrb_)?[0-9a-f]{64}$'
 ORDER BY name
 LIMIT 10001
"""


SPOOL_FILE = re.compile(
    r"usnm_([0-9a-f]{64})(__file_(result\.json|text\.txt|layout\.xml)|_error|_no_output_files)")
# An OCR bundle's result (#1059): one workunit, usnm_ocrb_<bundle>, for several pages.
BUNDLE_SPOOL_FILE = re.compile(
    r"usnm_ocrb_([0-9a-f]{64})(__file_(results\.json|texts|layouts)|_error|_no_output_files)")
BUNDLE_PREFIX = "ocrb_"


def spool_candidates(results, jobs, bundles=()):
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
                    match = BUNDLE_SPOOL_FILE.fullmatch(entry.name)
                    if match and match[1] in bundles:
                        yield Path(entry.path), match


def _ids(values, message):
    if len(values) > MAX_JOBS or len(set(values)) != len(values) or \
            any(not ID.fullmatch(value) for value in values):
        raise ValueError(message)


def inventory(root, results, counts, nonce, pending_ids=(), existing_ids=None,
              pending_bundles=(), existing_bundles=None):
    """pending_ids/existing_ids are single-page workunits' job ids; pending_bundles and
    existing_bundles are OCR bundle workunits' bundle ids (#1059), reported as their pages,
    which the server's journals name (record["bundle"])."""
    if not re.fullmatch(r"[0-9a-f]{32}", nonce):
        raise ValueError("invalid inventory nonce")
    if len(counts) != 2 or any(type(n) is not int or n < 0 for n in counts):
        raise ValueError("invalid inventory counts")
    if len(pending_ids) > MAX_JOBS or len(set(pending_ids)) != len(pending_ids) or \
            any(not ID.fullmatch(job_id) for job_id in pending_ids):
        raise ValueError("invalid/oversized pending job inventory")
    existing_ids = pending_ids if existing_ids is None else existing_ids
    existing_bundles = pending_bundles if existing_bundles is None else existing_bundles
    _ids(pending_bundles, "invalid/oversized pending bundle inventory")
    _ids(existing_bundles, "invalid/oversized existing bundle inventory")
    if not set(pending_bundles) <= set(existing_bundles):
        raise ValueError("invalid/oversized existing bundle inventory")
    if len(existing_ids) > MAX_JOBS or len(set(existing_ids)) != len(existing_ids) or \
            any(not ID.fullmatch(job_id) for job_id in existing_ids) or \
            not set(pending_ids) <= set(existing_ids):
        raise ValueError("invalid/oversized existing workunit inventory")
    if not root.is_dir() or not results.is_dir() or (root / "jobs").is_symlink():
        raise ValueError("missing/unsafe inventory directories")
    jobs = {}
    members = {}
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
        bundle = record.get("bundle")
        if bundle is not None:
            if not isinstance(bundle, str) or not ID.fullmatch(bundle):
                raise ValueError("invalid inventory record bundle")
            members.setdefault(bundle, []).append(job_id)
        jobs[job_id] = {"stage": record["stage"], "spool": None}
    # A bundle workunit counts as its pages, so the controller sees every page as it does a
    # single-page unit's.
    pending = list(pending_ids) + [j for b in pending_bundles for j in sorted(members.get(b, ()))]
    existing = list(existing_ids) + [j for b in existing_bundles for j in sorted(members.get(b, ()))]
    if len(set(pending)) != len(pending) or len(set(existing)) != len(existing) or \
            len(existing) > MAX_JOBS:
        raise ValueError("a page is in more than one workunit")
    seen = {}
    locations = {}
    for path, match in spool_candidates(results, jobs, members):
        if not path.resolve().is_relative_to(results.resolve()):
            raise ValueError("spool escapes results directory")
        key = (match.re is BUNDLE_SPOOL_FILE, match[1])
        locations.setdefault(key, set()).add(str(path.parent))
        if len(locations[key]) > 1:
            raise ValueError("multiple assimilator spools for one job, including partial outputs")
        seen.setdefault((key, str(path.parent)), set()).add(match[2])
    for ((bundled, name), directory), names in seen.items():
        if bundled:
            complete = {"__file_results.json", "__file_texts", "__file_layouts"} <= names
            owners = members[name]
        else:
            complete = {"__file_result.json", "__file_text.txt", "__file_layout.xml"} <= names
            owners = [name]
        if complete or names & {"_error", "_no_output_files"}:
            for job_id in owners:
                if jobs[job_id]["spool"] not in (None, directory):
                    raise ValueError("multiple assimilator spools for one job")
                jobs[job_id]["spool"] = directory
    return {
        "schema_version": 1,
        "nonce": nonce,
        "created_at": int(time.time()),
        "pending_workunits": counts[0],
        "pending_jobs": pending,
        # Pending OCR workunits, single-page and bundles: pending_jobs counts pages (#1059).
        "pending_ocr_workunits": len(pending_ids) + sum(bool(members.get(b)) for b in pending_bundles),
        "existing_jobs": existing,
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
            any(not re.fullmatch(r"usnm_(ocrb_)?[0-9a-f]{64}\t-?[0-9]+", value) for value in output[1:]):
        raise ValueError("invalid database inventory")
    rows = [value.split("\t") for value in output[1:]]
    pages = [(name.removeprefix("usnm_"), int(state)) for name, state in rows
             if not name.startswith("usnm_" + BUNDLE_PREFIX)]
    bundles = [(name.removeprefix("usnm_" + BUNDLE_PREFIX), int(state)) for name, state in rows
               if name.startswith("usnm_" + BUNDLE_PREFIX)]
    payload = inventory(args.root, args.results, [int(value) for value in counts], args.nonce,
                        [name for name, state in pages if state != 2],
                        [name for name, _ in pages],
                        [name for name, state in bundles if state != 2],
                        [name for name, _ in bundles])
    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_text(json.dumps(payload, sort_keys=True) + "\n")


if __name__ == "__main__":
    main()
