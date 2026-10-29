#!/usr/bin/env python3
"""Reconcile a copied single-page Qwen campaign for shadow-ledger ingestion.

Reads metadata only and writes one complete JSON observation to stdout. Never
submits work, reads credentials, hashes images/models, or changes campaign files.
"""
import argparse
import hashlib
import json
from pathlib import Path
import re
import sys


SHA256 = re.compile(r"[0-9a-f]{64}")
MAX_JOB_BYTES = 512 * 1024
MAX_AUDIT_BYTES = 16 * 1024 * 1024
MAX_UNITS = 10000


class ObservationError(ValueError):
    """The snapshot is inconsistent or outside this observer's supported scope."""


def require(condition, message):
    if not condition:
        raise ObservationError(message)


def unique_object(pairs):
    result = {}
    for key, value in pairs:
        require(key not in result, f"duplicate JSON key: {key}")
        result[key] = value
    return result


def reject_constant(value):
    raise ObservationError(f"non-finite JSON value: {value}")


def read_file(path, limit):
    require(not path.is_symlink() and path.is_file(), f"expected regular file: {path}")
    with path.open("rb") as stream:
        raw = stream.read(limit + 1)
    require(len(raw) <= limit, f"metadata size limit exceeded: {path}")
    return raw


def decode(raw):
    return json.loads(raw, object_pairs_hook=unique_object, parse_constant=reject_constant)


def digest(raw):
    return hashlib.sha256(raw).hexdigest()


def marker(unit, name):
    path = unit / name
    require(not path.is_symlink(), f"symlink marker: {path}")
    if not path.exists():
        return None
    return digest(read_file(path, 4096))


def unit_directories(units, max_units):
    require(not units.is_symlink() and units.is_dir(), "missing or symlink units directory")
    result = []
    for path in units.iterdir():
        require(not path.is_symlink() and path.is_dir(), f"unexpected unit entry: {path}")
        result.append(path)
        require(len(result) <= max_units, "unit limit exceeded; use a bounded campaign snapshot")
    return sorted(result)


def observe(root, max_units=MAX_UNITS):
    require(1 <= max_units <= MAX_UNITS, "max-units must be between 1 and 10000")
    require(not root.is_symlink() and root.is_dir(), "missing or symlink campaign root")
    raw_audit = read_file(root / "audit.json", MAX_AUDIT_BYTES)
    audit = decode(raw_audit)
    require(isinstance(audit, dict), "audit must be an object")
    require(len(audit) <= max_units, "audit entry limit exceeded")
    require(all(SHA256.fullmatch(key) for key in audit), "invalid audited job identity")
    directories = unit_directories(root / "units", max_units)
    observations = []
    observed_jobs = set()
    observed_workunits = set()
    documents = set()
    checks = []
    dispositions = []
    for unit in directories:
        job_path = unit / "qwen.json"
        require(not job_path.is_symlink(), f"expected regular file, not symlink: {job_path}")
        before = {name: marker(unit, name) for name in
                  ("submitted", "submitting", "no_crops", "too_large")}
        require(before["submitting"] is None, f"uncertain submission; reconcile first: {unit.name}")
        terminal = [name for name in ("no_crops", "too_large") if before[name] is not None]
        if terminal:
            require(len(terminal) == 1, f"conflicting terminal dispositions: {unit.name}")
            require(not job_path.exists() and before["submitted"] is None,
                    f"conflicting terminal disposition: {unit.name}")
            dispositions.append({"unit": unit.name, "state": terminal[0],
                                 "marker_sha256": before[terminal[0]], "evidence_verified": False})
            checks.append((unit, None, before))
            continue
        raw_job = read_file(job_path, MAX_JOB_BYTES)
        job = decode(raw_job)
        require(isinstance(job, dict), f"job must be an object: {unit.name}")
        job_id = job.get("id")
        require(isinstance(job_id, str) and SHA256.fullmatch(job_id), f"invalid job id: {unit.name}")
        require(job_id not in observed_jobs, f"duplicate staged job identity: {job_id}")
        observed_jobs.add(job_id)
        require(job.get("v") == 1 and type(job.get("v")) is int,
                f"unsupported job schema: {unit.name}")
        require(job.get("pages", []) == [], f"bundle membership not supported yet: {unit.name}")
        doc_id = job.get("doc_id")
        require(isinstance(doc_id, str) and 0 < len(doc_id) <= 256, "missing or oversized doc_id")
        source = job.get("input")
        require(isinstance(source, dict) and isinstance(source.get("sha256"), str)
                and SHA256.fullmatch(source["sha256"]), f"invalid declared source hash: {unit.name}")
        job_sha = digest(raw_job)
        receipt = audit.get(job_id)
        require((receipt is not None) == (before["submitted"] is not None),
                f"submission marker/audit mismatch: {unit.name}")
        if receipt is not None:
            require(isinstance(receipt, dict) and receipt.get("job_sha256") == job_sha,
                    f"audited manifest changed: {unit.name}")
            require(all(type(receipt.get(key)) is int and receipt[key] == expected
                        for key, expected in (("batch", 8), ("min_quorum", 2), ("target_nresults", 2))),
                    f"unexpected native routing/quorum receipt: {unit.name}")
            workunit_id = receipt.get("workunit_id")
            require(type(workunit_id) is int and workunit_id > 0, "invalid audited workunit id")
            require(workunit_id not in observed_workunits, "duplicate audited workunit id")
            observed_workunits.add(workunit_id)
        else:
            workunit_id = None
        documents.add(doc_id)
        observations.append({
            "job_id": job_id,
            "job_sha256": job_sha,
            "doc_id": doc_id,
            "declared_source_sha256": source["sha256"],
            "source_bytes_verified": False,
            "state": "audit_manifest_match" if receipt is not None else "staged_unverified",
            "workunit_id": workunit_id,
            "expected_original_replicas": 2 if receipt is not None else None,
        })
        checks.append((unit, job_sha, before))
    require(set(audit) <= observed_jobs, "audited job missing from campaign snapshot")
    require(unit_directories(root / "units", max_units) == directories, "unit set changed during observation")
    require(read_file(root / "audit.json", MAX_AUDIT_BYTES) == raw_audit, "audit changed during observation")
    for unit, expected, before in checks:
        require({name: marker(unit, name) for name in before} == before,
                f"markers changed during observation: {unit.name}")
        if expected is not None:
            require(digest(read_file(unit / "qwen.json", MAX_JOB_BYTES)) == expected,
                    f"manifest changed during observation: {unit.name}")
        else:
            path = unit / "qwen.json"
            require(not path.exists() and not path.is_symlink(), f"terminal unit changed: {unit.name}")
    observations.sort(key=lambda row: row["job_id"])
    snapshot = {
        "v": 1,
        "mode": "shadow_only",
        "audit_sha256": digest(raw_audit),
        "counts": {
            "manifest_jobs": len(observations),
            "unique_document_ids": len(documents),
            "audited_workunits": len(observed_workunits),
            "expected_original_replicas": 2 * len(observed_workunits),
            "no_crops_dispositions": sum(row["state"] == "no_crops" for row in dispositions),
            "too_large_dispositions": sum(row["state"] == "too_large" for row in dispositions),
        },
        "jobs": observations,
        "dispositions": dispositions,
    }
    encoded = json.dumps(snapshot, sort_keys=True, separators=(",", ":")).encode()
    return {**snapshot, "observation_sha256": digest(encoded)}


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, required=True, help="copied campaign with audit.json and units/")
    parser.add_argument("--max-units", type=int, default=MAX_UNITS)
    args = parser.parse_args(argv)
    try:
        result = observe(args.root, args.max_units)
    except (ObservationError, OSError, UnicodeError, json.JSONDecodeError) as error:
        print(f"qwen shadow inventory failed: {error}", file=sys.stderr)
        return 1
    print(json.dumps(result, sort_keys=True))
    return 0


if __name__ == "__main__":
    sys.exit(main())
