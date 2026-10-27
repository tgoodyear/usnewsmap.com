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

LOCK_EXEC = """import argparse, json, os
parser = argparse.ArgumentParser()
parser.add_argument("--lock-fd", type=int, required=True)
parser.add_argument("--command-json", required=True)
args = parser.parse_args()
command = json.loads(args.command_json)
os.dup2(args.lock_fd, 3, inheritable=True)
if args.lock_fd != 3:
    os.close(args.lock_fd)
os.execvp(command[0], command)
"""


def image_id(value):
    match = re.fullmatch(r"(?:sha256:)?([0-9a-f]{64})", value) if isinstance(value, str) else None
    return match.group(1) if match else None


def execute(args, request, runner=subprocess.run, *, lock_fd):
    if os.geteuid() != 0:
        raise ValueError("on-prem acceptance transport requires the project-host operator")
    expected = image_id(args.expected_image)
    if expected is None:
        raise ValueError("expected image must be an immutable local image ID")
    if (not re.fullmatch(r"/project(?:/[A-Za-z0-9_.-]+)*", args.project)
            or any(part in (".", "..") for part in args.project.split("/"))):
        raise ValueError("project must be below the existing /project mount")
    if len(request) > 65536 or not isinstance(json.loads(request), dict):
        raise ValueError("expected one bounded JSON request")
    inspected = runner(
        ["podman", "container", "inspect", "usnm-boinc-server"],
        check=True, capture_output=True, text=True, timeout=10)
    containers = json.loads(inspected.stdout)
    if (not isinstance(containers, list) or len(containers) != 1
            or not isinstance(containers[0], dict)
            or image_id(containers[0].get("Image")) != expected
            or not isinstance(containers[0].get("State"), dict)
            or containers[0]["State"].get("Running") is not True
            or not isinstance(containers[0].get("Id"), str)
            or not re.fullmatch(r"[0-9a-f]{64}", containers[0]["Id"])):
        raise ValueError("running server does not match the reviewed image ID")
    source = Path(__file__).with_name("acceptance_server.py").read_text()
    command = ["podman", "exec", "--interactive", "--preserve-fds=1", "--user", "1000:1000",
         containers[0]["Id"], "timeout", "--signal=TERM", "--kill-after=5s", "90s",
         "python3", "-c", source,
         "--project", args.project, "--fixtures", args.project + "/acceptance"]
    # crun preserves FD 3 in the exec process: a lost transport cannot release
    # the project lock while timeout/the adapter still owns that descriptor.
    result = runner(
        [sys.executable, "-c", LOCK_EXEC, "--lock-fd", str(lock_fd),
         "--command-json", json.dumps(command)],
        input=request, check=True, capture_output=True, text=True, timeout=105,
        pass_fds=(lock_fd,))
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
            receipt = execute(args, sys.stdin.read(65537), lock_fd=lock.fileno())
        print(json.dumps(receipt))
    except (OSError, ValueError, subprocess.SubprocessError):
        print("on-prem acceptance failed; reconcile the private reservation journal before retrying",
              file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
