#!/usr/bin/env python3
"""Validate and retain pilot results against trusted server-staged job records."""
import argparse
import json
import os
from pathlib import Path
import re
import sys
import tempfile

CONTRACT = Path(__file__).resolve().parents[2] / "ocr/boinc/iris"
if CONTRACT.is_dir():
    sys.path.insert(0, str(CONTRACT))
from contract import validate_job, validate_result


def check(result_path, jobs, keep):
    if result_path.stat().st_size > 65536:
        raise ValueError("Iris result exceeds 64 KiB")
    raw = result_path.read_bytes()
    result = json.loads(raw)
    identity = result.get("id")
    if not isinstance(identity, str) or not re.fullmatch(r"[0-9a-f]{64}", identity):
        raise ValueError("invalid Iris result job identity")
    job_path = jobs / f"{identity}.json"
    if job_path.stat().st_size > 65536:
        raise ValueError("registered Iris job exceeds 64 KiB")
    job = json.loads(job_path.read_text())
    validate_job(job)
    validate_result(result, job)
    if not re.fullmatch(r"[A-Za-z0-9_.-]{1,200}", result_path.name):
        raise ValueError("invalid uploaded result filename")
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
    if result["status"] != "ok":
        raise ValueError("Iris GPU inference failed; diagnostic result retained")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--result", type=Path, required=True)
    parser.add_argument("--jobs", type=Path, required=True)
    parser.add_argument("--keep", type=Path, required=True)
    args = parser.parse_args()
    check(args.result, args.jobs, args.keep)


if __name__ == "__main__":
    main()
