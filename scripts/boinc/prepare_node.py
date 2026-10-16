#!/usr/bin/env python3
"""Create ONLY the disabled, dedicated client prerequisites for an isolated OCR-node pilot."""
from __future__ import annotations

import argparse
from datetime import datetime, timezone
import hashlib
import json
import os
from pathlib import Path
import pwd
import shutil
import subprocess

COMMIT = "ae68f3290ac82d08b4d9767acfe0daaccaeeb883"
SERVICE = "usnm-boinc-canary.service"
DATA = Path("/var/lib/usnm-boinc-canary")
INSTALL = Path("/opt/usnm-boinc-canary")
UNIT_PATH = Path("/etc/systemd/system") / SERVICE
MARKER = "# Managed by scripts/boinc/prepare_node.py; isolated dev only."
UNIT = f"""{MARKER}
[Unit]
Description=Optional isolated local-dev BOINC canary (disabled by default)
ConditionPathExists=/etc/systemd/system/{SERVICE}.d/zz-usnm-volunteer.conf

[Service]
User=boinc
Group=boinc
WorkingDirectory={DATA}
ExecStart={INSTALL}/bin/boinc --dir {DATA} --gui_rpc_port 31417
Restart=no
KillSignal=SIGINT
TimeoutStopSec=120
ProtectSystem=strict
ReadWritePaths={DATA}

[Install]
WantedBy=multi-user.target
"""


def run(*args, **kwargs):
    result = subprocess.run(args, capture_output=True, text=True, timeout=180, **kwargs)
    if result.returncode:
        raise RuntimeError(f"{args[0]}: {(result.stderr or result.stdout)[-1500:]}")
    return result.stdout.strip()


def allocation(*tables):
    ends = [1048576]
    for table in tables:
        for line in table.splitlines():
            if not line.strip() or line.lstrip().startswith("#"):
                continue
            owner, start, count = line.split(":")
            start, count = int(start), int(count)
            if owner == "boinc" or start < 0 or count <= 0:
                raise ValueError("Existing BOINC or malformed subordinate-ID allocation")
            ends.append(start + count)
    return ((max(ends) + 65535) // 65536) * 65536


def validate_baseline(baseline, worker_pid, now=None):
    now = now or datetime.now(timezone.utc)
    age = (now - datetime.fromisoformat(baseline["observed_at"])).total_seconds()
    if not baseline.get("baseline_ready") or not 0 <= age <= 120:
        raise ValueError("Need a fresh, ready pre-install baseline (at most 120 seconds old)")
    if baseline["worker"]["MainPID"] != worker_pid or worker_pid == "0":
        raise ValueError("Worker PID changed since baseline; do not modify this host")


def validate_bundle(bundle):
    if (bundle / "source-commit").read_text().strip() != COMMIT:
        raise ValueError("Not the reviewed stable 8.2.13 source")
    expected = {}
    for line in (bundle / "SHA256SUMS").read_text().splitlines():
        digest, relative = line.split()
        if relative in expected:
            raise ValueError("Duplicate client checksum")
        expected[relative] = digest
    if set(expected) != {"install/bin/boinc", "install/bin/boinccmd"}:
        raise ValueError("Unexpected client checksum manifest")
    for relative, digest in expected.items():
        binary = bundle / relative
        if binary.is_symlink() or not binary.is_file():
            raise ValueError("Expected real exported client binaries")
        if hashlib.sha256(binary.read_bytes()).hexdigest() != digest:
            raise ValueError("Client checksum mismatch")
    return expected


def prepare(bundle, baseline, apply=False):
    checksums = validate_bundle(bundle)
    pid = run("systemctl", "show", "usnm-ocr-worker.service", "--property=MainPID", "--value")
    validate_baseline(baseline, pid)
    if os.uname().machine != "x86_64":
        raise ValueError("This exported client is x86_64 only")
    try:
        pwd.getpwnam("boinc")
    except KeyError:
        pass
    else:
        raise ValueError("Existing boinc account: never adopt or replace an existing identity")
    for path in (DATA, INSTALL, UNIT_PATH):
        if os.path.lexists(path):
            raise ValueError(f"Refusing existing path {path}")
    for name in ("podman", "newuidmap", "newgidmap", "loginctl", "systemctl", "useradd", "usermod"):
        if not shutil.which(name):
            raise ValueError(f"Missing existing host prerequisite {name}; no packages installed")
    if not Path("/sys/fs/cgroup/cgroup.controllers").is_file():
        raise ValueError("cgroup v2 is required")
    start = allocation(Path("/etc/subuid").read_text(), Path("/etc/subgid").read_text())
    print(json.dumps({"service": SERVICE, "home": str(DATA), "install": str(INSTALL),
                      "source_commit": COMMIT, "checksums": checksums,
                      "subordinate_ids": [start, start + 65535], "apply": apply,
                      "starts_or_enables_services": False}, indent=2))
    if not apply:
        return
    if os.geteuid() != 0:
        raise ValueError("--apply requires root")
    run("useradd", "--system", "--user-group", "--create-home", "--home-dir", str(DATA),
        "--shell", "/sbin/nologin", "boinc")
    account = pwd.getpwnam("boinc")
    run("usermod", "--add-subuids", f"{start}-{start + 65535}",
        "--add-subgids", f"{start}-{start + 65535}", "boinc")
    (INSTALL / "bin").mkdir(parents=True)
    for name in ("boinc", "boinccmd"):
        target = INSTALL / "bin" / name
        shutil.copyfile(bundle / "install/bin" / name, target)
        target.chmod(0o755)
    DATA.chmod(0o700)
    version = run(str(INSTALL / "bin/boinc"), "--version", user=account.pw_uid,
                  group=account.pw_gid, extra_groups=[], cwd="/",
                  env={"PATH": "/usr/bin:/bin", "HOME": str(DATA)})
    if not version.startswith("8.2.13 "):
        raise ValueError("Actual RHEL client version did not match")
    UNIT_PATH.write_text(UNIT)
    UNIT_PATH.chmod(0o644)
    run("systemctl", "daemon-reload")
    if run("systemctl", "show", SERVICE, "--property=ActiveState", "--value") != "inactive":
        raise ValueError("Unexpected BOINC activation")
    if run("systemctl", "show", SERVICE, "--property=UnitFileState", "--value") != "disabled":
        raise ValueError("Unexpected BOINC enablement")
    if run("systemctl", "show", "usnm-ocr-worker.service", "--property=MainPID", "--value") != pid:
        raise ValueError("Worker PID changed; stop and investigate before continuing")
    print("Actual RHEL version:", version)
    print("Disabled prerequisites only. Apply reviewed setup_volunteer.py --ocr-node before any pilot start.")


def stop():
    if os.geteuid() != 0 or not UNIT_PATH.read_text().startswith(MARKER):
        raise ValueError("Rollback requires root and this helper's managed canary unit")
    account = pwd.getpwnam("boinc")
    if account.pw_uid == 0 or account.pw_dir != str(DATA):
        raise ValueError("Unexpected BOINC identity; refusing rollback")
    run("systemctl", "stop", SERVICE)
    run("systemctl", "disable", SERVICE)
    run("loginctl", "disable-linger", "boinc")
    run("systemctl", "stop", f"user@{account.pw_uid}.service")
    print("Canary/client and dedicated user manager stopped; lingering disabled. Evidence/account retained.")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--bundle", type=Path)
    parser.add_argument("--baseline", type=Path)
    parser.add_argument("--apply", action="store_true")
    parser.add_argument("--stop", action="store_true")
    args = parser.parse_args()
    if args.stop:
        if args.bundle or args.baseline or args.apply:
            parser.error("--stop is exclusive")
        stop()
    else:
        if not args.bundle or not args.baseline:
            parser.error("--bundle and --baseline are required")
        prepare(args.bundle, json.loads(args.baseline.read_text()), args.apply)


if __name__ == "__main__":
    main()
