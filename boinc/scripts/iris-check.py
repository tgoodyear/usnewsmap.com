#!/usr/bin/env python3
"""Validate and retain pilot results against trusted server-staged job records."""
import argparse
import json
import os
from pathlib import Path
import re
import sys
import tempfile

CONTRACT = Path(__file__).resolve().parents[2] / "boinc/apps/iris"
if CONTRACT.is_dir():
    sys.path.insert(0, str(CONTRACT))
from contract import validate_job, validate_result
from bundle import MAX_BUNDLE_JOB_BYTES, MAX_BUNDLE_RESULT_BYTES, validate_bundle_job, validate_bundle_result


def check(result_path, jobs, keep):
    if result_path.stat().st_size > MAX_BUNDLE_RESULT_BYTES:
        raise ValueError("Iris result exceeds the bundle limit")
    raw = result_path.read_bytes()
    result = json.loads(raw)
    identity = result.get("id")
    if not isinstance(identity, str) or not re.fullmatch(r"[0-9a-f]{64}", identity):
        raise ValueError("invalid Iris result job identity")
    uploaded = re.fullmatch(r"usnm_iris_([0-9a-f]{64})_[A-Za-z0-9_.-]+", result_path.name)
    if not uploaded or len(result_path.name) > 200 or uploaded.group(1) != identity:
        raise ValueError("Iris result identity does not match BOINC upload filename")
    job_path = jobs / f"{identity}.json"
    if job_path.stat().st_size > MAX_BUNDLE_JOB_BYTES:
        raise ValueError("registered Iris job exceeds the bundle limit")
    job = json.loads(job_path.read_text())
    bundle = job.get("v") == 2
    if bundle:
        validate_bundle_job(job)
        validate_bundle_result(result, job)
    else:
        if len(raw) > 65536 or job_path.stat().st_size > 65536:
            raise ValueError("Iris result or job exceeds 64 KiB")
        validate_job(job)
        validate_result(result, job)
    keep.mkdir(parents=True, exist_ok=True)
    target = keep / (result_path.name + ".json")
    if target.exists():
        if target.read_bytes() != raw:
            raise ValueError("conflicting retained Iris result")
    else:
        with tempfile.TemporaryDirectory(dir=keep, prefix=".iris-") as directory:
            staged = Path(directory) / "result"
            with staged.open("xb") as stream:
                stream.write(raw)
                stream.flush()
                os.fsync(stream.fileno())
            os.replace(staged, target)
    # A partial bundle is retained (its ok crops are kept) but never validates as a full success.
    if result["status"] != "ok":
        raise ValueError("Iris GPU inference failed or was partial; diagnostic result retained")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--result", type=Path, required=True)
    parser.add_argument("--jobs", type=Path, required=True)
    parser.add_argument("--keep", type=Path, required=True)
    args = parser.parse_args()
    check(args.result, args.jobs, args.keep)


if __name__ == "__main__":
    main()
