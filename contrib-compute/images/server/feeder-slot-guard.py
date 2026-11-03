#!/usr/bin/env python3
"""Report feeder slot-allocation drift without changing the feeder or any work."""

import argparse
import json
import math
import os
from pathlib import Path
import re
import socket
import stat
import struct
import sys
import time
import xml.etree.ElementTree as ET


MAX_APPS = 10  # MAX_APPS in the pinned BOINC sched/sched_shmem.h
MAX_AGE_MS = 180_000
INTERVAL = 300
BOOT_ID = re.compile(r"[0-9a-f]{8}(-[0-9a-f]{4}){3}-[0-9a-f]{12}")
BITS = re.compile(r"[0-9a-f]{16}")
TLS_OPTIONS = Path("/usr/local/share/usnm/mysql-tls.cnf")
TLS_CA = "/usr/local/share/usnm/mysql-ca.pem"


class Unknown(Exception):
    pass


def read_bounded(path, limit=16384, *, owner=None):
    fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    with os.fdopen(fd, "rb") as stream:
        metadata = os.fstat(stream.fileno())
        if not stat.S_ISREG(metadata.st_mode):
            raise Unknown("invalid_status_file")
        if owner is not None and (metadata.st_uid != owner or metadata.st_mode & 0o022):
            raise Unknown("untrusted_snapshot_file")
        data = stream.read(limit + 1)
    if len(data) > limit:
        raise Unknown("invalid_status_file")
    return data


def feeder_identity(project, proc=Path("/proc")):
    host = socket.gethostname().split(".")[0]
    if not host or "/" in host:
        raise Unknown("invalid_hostname")
    directory = project / f"pid_{host}"
    pid_text = read_bounded(directory / "feeder.pid", 32).decode("ascii").strip()
    if not pid_text.isdecimal() or not 0 < int(pid_text) < 2**31:
        raise Unknown("invalid_feeder_pid")
    pid = int(pid_text)
    line = read_bounded(proc / str(pid) / "stat", 8192).decode("ascii")
    end = line.rfind(")")
    if not line.startswith(f"{pid} (") or end < 0:
        raise Unknown("invalid_feeder_identity")
    fields = line[end + 2:].split()
    if len(fields) < 20 or fields[0] in ("Z", "X", "x") or not fields[19].isdecimal():
        raise Unknown("invalid_feeder_identity")
    command = read_bounded(proc / str(pid) / "cmdline", 8192).split(b"\0")
    if not command or os.path.basename(command[0]) != b"feeder" or b"--allapps" not in command:
        raise Unknown("not_allapps_feeder")
    boot = read_bounded(proc / "sys/kernel/random/boot_id", 64).decode("ascii").strip()
    if not BOOT_ID.fullmatch(boot):
        raise Unknown("invalid_boot_id")
    return directory, (pid, fields[19], boot)


def status_path():
    host = socket.gethostname().split(".")[0]
    root = Path(os.environ.get("USNM_PID_ROOT") or "/var/lib/usnm-boinc")
    if not host or "/" in host or not root.is_absolute():
        raise Unknown("invalid_status_directory")
    return root / f"pid_{host}" / "usnm-feeder-slots.json"


def parse_snapshot(raw, identity, now_ms):
    record = json.loads(raw)
    if not isinstance(record, dict) or type(record.get("version")) is not int or record["version"] != 1:
        raise Unknown("invalid_snapshot")
    pid, ticks, boot = identity
    if (type(record.get("pid")) is not int or record["pid"] != pid
            or record.get("start_ticks") != ticks or record.get("boot_id") != boot):
        raise Unknown("snapshot_process_mismatch")
    if (type(record.get("sequence")) is not int or record["sequence"] < 1
            or type(record.get("monotonic_ms")) is not int or record["monotonic_ms"] <= 0):
        raise Unknown("invalid_snapshot")
    age = now_ms - record["monotonic_ms"]
    if not 0 <= age <= MAX_AGE_MS:
        raise Unknown("stale_snapshot")
    if any(type(record.get(key)) is not bool for key in ("drift", "uncertain", "reloading")):
        raise Unknown("invalid_snapshot")
    if record["uncertain"]:
        raise Unknown("feeder_snapshot_uncertain")
    if record["reloading"]:
        raise Unknown("feeder_reload_in_progress")
    rows = record.get("apps")
    if not isinstance(rows, list) or not 1 <= len(rows) <= MAX_APPS:
        raise Unknown("invalid_snapshot")
    apps = {}
    for row in rows:
        if (not isinstance(row, list) or len(row) != 2 or type(row[0]) is not int
                or row[0] <= 0 or row[0] in apps or not isinstance(row[1], str)
                or not BITS.fullmatch(row[1])):
            raise Unknown("invalid_snapshot")
        weight = struct.unpack(">d", bytes.fromhex(row[1]))[0]
        if not math.isfinite(weight) or weight < 0:
            raise Unknown("invalid_snapshot")
        apps[row[0]] = weight
    return record, apps


def database_apps(project):
    try:
        import MySQLdb

        config = ET.parse(project / "config.xml").find("config")
        if config is None or not TLS_OPTIONS.is_file() or not Path(TLS_CA).is_file():
            raise ValueError("missing configuration")
        host, user, password, database = (config.findtext(key) for key in
                                          ("db_host", "db_user", "db_passwd", "db_name"))
        if not all((host, user, password, database)):
            raise ValueError("incomplete configuration")
        port = 0
        if ":" in host:
            host, port_text = host.split(":", 1)
            if not host or not port_text.isdecimal() or not 1 <= int(port_text) <= 65535:
                raise ValueError("invalid database endpoint")
            port = int(port_text)
        db = MySQLdb.connect(
            host=host, port=port, user=user, passwd=password, db=database,
            ssl={"ca": TLS_CA}, read_default_file=str(TLS_OPTIONS), read_default_group="client",
            connect_timeout=5, read_timeout=10, write_timeout=10,
        )
        try:
            cursor = db.cursor()
            try:
                cursor.execute("SHOW SESSION STATUS LIKE 'Ssl_cipher'")
                cipher = cursor.fetchone()
                if not cipher or not cipher[1]:
                    raise ValueError("unencrypted connection")
                cursor.execute("SELECT id, weight FROM app WHERE deprecated=0 ORDER BY id LIMIT 11")
                rows = cursor.fetchall()
            finally:
                cursor.close()
        finally:
            db.close()
        apps = {}
        for app_id, weight in rows:
            if type(app_id) is not int or app_id <= 0 or app_id in apps:
                raise ValueError("invalid app")
            weight = float(weight)
            if not math.isfinite(weight) or weight < 0:
                raise ValueError("invalid weight")
            apps[app_id] = weight
        if len(apps) > MAX_APPS + 1:
            raise ValueError("too many apps")
        return apps
    except Exception as exc:
        raise Unknown("database_unavailable") from exc


def check(project, proc=Path("/proc")):
    if (project / "stop_maintenance").exists():
        return {"status": "maintenance", "reason": "stop_maintenance"}
    try:
        directory, identity = feeder_identity(project, proc)
        path = status_path()
        raw = read_bounded(path, owner=os.geteuid())
        record, allocated = parse_snapshot(raw, identity, int(time.monotonic() * 1000))
        current = None if record["drift"] else database_apps(project)
        # A concurrent feeder restart or reread must not turn a mixed observation into success.
        if feeder_identity(project, proc) != (directory, identity) or \
                read_bounded(path, owner=os.geteuid()) != raw:
            raise Unknown("observation_changed")
        if record["drift"]:
            return {"status": "drift", "reason": "reload_changed_app_order_or_weights"}
        added = sorted(current.keys() - allocated.keys())
        removed = sorted(allocated.keys() - current.keys())
        changed = sorted(app_id for app_id in current.keys() & allocated.keys()
                         if current[app_id] != allocated[app_id])
        if added or removed or changed:
            return {"status": "drift", "reason": "registry_changed",
                    "added_app_ids": added, "removed_app_ids": removed,
                    "weight_changed_app_ids": changed}
        return {"status": "match", "reason": "allocation_matches_registry"}
    except Unknown as exc:
        return {"status": "unknown", "reason": str(exc)}
    except Exception:
        return {"status": "unknown", "reason": "observation_unavailable"}


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    default = Path(os.environ.get("PROJECT_DIR", "/project")) / os.environ.get("PROJECT", "usnmocr")
    parser.add_argument("--project", type=Path, default=default)
    parser.add_argument("--watch", action="store_true", help="report every five minutes")
    args = parser.parse_args(argv)
    while True:
        result = {"event": "boinc_feeder_slot_guard", "schema_version": 1} | check(args.project)
        print(json.dumps(result, sort_keys=True, separators=(",", ":")), flush=True)
        if not args.watch:
            return {"match": 0, "maintenance": 0, "drift": 1, "unknown": 2}[result["status"]]
        time.sleep(INTERVAL)


if __name__ == "__main__":
    sys.exit(main())
