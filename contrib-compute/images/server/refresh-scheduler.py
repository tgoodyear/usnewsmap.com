#!/usr/bin/env python3
"""Refresh the persistent project CGI from the reviewed server image."""
import argparse
import os
from pathlib import Path
import uuid


def refresh(source: Path, target: Path) -> bool:
    if not source.is_file() or not os.access(source, os.X_OK):
        raise ValueError("image scheduler CGI is missing or not executable")
    payload = source.read_bytes()
    if not payload:
        raise ValueError("image scheduler CGI is empty")
    if target.is_file() and os.access(target, os.X_OK) and target.read_bytes() == payload:
        return False
    staged = target.with_name(".usnm-scheduler-" + uuid.uuid4().hex)
    created = False
    try:
        with staged.open("xb") as stream:
            created = True
            stream.write(payload)
            stream.flush()
            os.fsync(stream.fileno())
        staged.chmod(0o755)
        # Rename instead of truncating the executable used by in-flight requests.
        os.replace(staged, target)
    finally:
        if created:
            staged.unlink(missing_ok=True)
    return True


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source", type=Path, required=True)
    parser.add_argument("--target", type=Path, required=True)
    args = parser.parse_args()
    changed = refresh(args.source, args.target)
    print("scheduler CGI refreshed from image" if changed else "scheduler CGI matches image")


if __name__ == "__main__":
    main()
