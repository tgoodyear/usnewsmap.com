#!/usr/bin/env python3
"""Root-local transport for the reviewed acceptance server; never a controller step."""
import argparse
import fcntl
import json
import os
from pathlib import Path
import re
import subprocess
import sys


def execute(args, request, runner=subprocess.run):
    if os.geteuid() != 0:
        raise ValueError("on-prem acceptance transport requires the project-host operator")
    if not re.fullmatch(r"sha256:[0-9a-f]{64}", args.expected_image):
        raise ValueError("expected image must be an immutable local image ID")
    if (not re.fullmatch(r"/project(?:/[A-Za-z0-9_.-]+)*", args.project)
            or any(part in (".", "..") for part in args.project.split("/"))):
        raise ValueError("project must be below the existing /project mount")
    if len(request) > 65536 or not isinstance(json.loads(request), dict):
        raise ValueError("expected one bounded JSON request")
    inspected = runner(
        ["podman", "container", "inspect", "usnm-boinc-server"],
        check=True, capture_output=True, text=True, timeout=15)
    containers = json.loads(inspected.stdout)
    if (len(containers) != 1 or containers[0].get("Image") != args.expected_image
            or containers[0].get("State", {}).get("Running") is not True
            or not re.fullmatch(r"[0-9a-f]{64}", containers[0].get("Id", ""))):
        raise ValueError("running server does not match the reviewed image ID")
    source = Path(__file__).with_name("acceptance_server.py").read_text()
    result = runner(
        ["podman", "exec", "--interactive", "--user", "1000:1000",
         containers[0]["Id"], "python3", "-c", source,
         "--project", args.project, "--fixtures", args.project + "/acceptance"],
        input=request, check=True, capture_output=True, text=True, timeout=120)
    receipt = json.loads(result.stdout)
    if not isinstance(receipt, dict):
        raise ValueError("invalid server receipt")
    return receipt


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--expected-image", required=True)
    parser.add_argument("--project", default="/project/usnmocr")
    args = parser.parse_args()
    try:
        # Share the installed bridge runner's lock, without waiting behind live work.
        with open("/run/usnm-boinc-step.lock", "a") as lock:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
            receipt = execute(args, sys.stdin.read(65537))
        print(json.dumps(receipt))
    except (OSError, ValueError, subprocess.SubprocessError):
        print("on-prem acceptance failed; reconcile the private reservation journal before retrying",
              file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
