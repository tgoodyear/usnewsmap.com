#!/usr/bin/env python3
"""Whether gold-engine-platforms.json (on stdin) binds a worker image on a platform (#1687).

Prints `bound`, or what is missing (`no_worker_entry`, `no_platform_entry`), and exits 0; a file
that isn't the bindings object exits 1. boinc-app-release.sh reads the running server's copy, as
the scheduler's gold hook (gold-engine-routing.h) does."""
import argparse
import json
import re
import sys

WORKER = re.compile(r"ghcr\.io/tgoodyear/usnm-boinc-worker@(sha256:[0-9a-f]{64})")
DIGEST = re.compile(r"sha256:[0-9a-f]{64}")


def binding(bindings, worker_image, platform):
    match = WORKER.fullmatch(worker_image)
    if not match:
        raise ValueError("--worker-image must be ghcr.io/tgoodyear/usnm-boinc-worker@sha256:<64 hex>")
    if not isinstance(bindings, dict) or not all(
            isinstance(v, dict) and all(isinstance(b, str) for b in v.values()) for v in bindings.values()):
        raise ValueError("not a gold-engine-platforms.json object")
    entry = bindings.get(match[1])
    if entry is None:
        return "no_worker_entry"
    base = entry.get(platform)
    if base is None:
        return "no_platform_entry"
    if not DIGEST.fullmatch(base):
        raise ValueError("invalid engine base in gold-engine-platforms.json")
    return "bound"


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--worker-image", required=True)
    parser.add_argument("--platform", required=True)
    args = parser.parse_args(argv)
    try:
        print(binding(json.loads(sys.stdin.read(1024 * 1024 + 1)), args.worker_image, args.platform))
    except ValueError as error:
        print(f"error: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
