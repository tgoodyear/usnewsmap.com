#!/usr/bin/env python3
"""Read-only OCR throughput/resource snapshot before an optional isolated pilot."""
import argparse
from datetime import datetime, timezone
import json
from pathlib import Path
import pwd
import statistics
import subprocess
import sys


def summarize(lines, now, window_seconds):
    cutoff = now.timestamp() - window_seconds
    completed = []
    errors = 0
    failure_messages = {
        "dead-lettered",
        "page failed; it reappears when its visibility runs out",
        "page abandoned: out of time, nothing written; it reappears for another node",
        "receive failed",
        "message was gone at delete; another node may redo it",
    }
    for line in lines:
        try:
            record = json.loads(line)
            if not isinstance(record, dict):
                continue
            timestamp = datetime.fromisoformat(record["timestamp"].replace("Z", "+00:00"))
            if not cutoff <= timestamp.timestamp() <= now.timestamp():
                continue
            fields = record.get("fields", {})
            failed = False
            if fields.get("message") == "page done":
                if fields.get("status") in {"ok", "short", "empty"}:
                    completed.append((timestamp, fields))
                else:
                    failed = True
            if (record.get("level") == "ERROR"
                    or fields.get("message") in failure_messages):
                failed = True
            errors += int(failed)
        except (ValueError, TypeError, KeyError):
            continue
    durations = [float(fields["secs"]) for _, fields in completed if "secs" in fields]
    return {
        "window_seconds": window_seconds,
        "successful_pages": len(completed),
        "pages_per_hour": len(completed) * 3600 / window_seconds,
        "median_page_seconds": statistics.median(durations) if durations else None,
        "last_success_age_seconds": (
            now.timestamp() - max(timestamp.timestamp() for timestamp, _ in completed)
            if completed else None),
        "error_events": errors,
        "workload": sorted({
            (fields.get("queue", ""), fields.get("language", ""), fields.get("engine", ""))
            for _, fields in completed}),
    }


def read_cgroup(group):
    if not group or not group.startswith("/") or group == "/" or ".." in Path(group).parts:
        return None
    root = Path("/sys/fs/cgroup") / group.lstrip("/")
    result = {}
    for filename in ("cpu.stat", "memory.current", "memory.peak", "memory.events", "pids.current"):
        path = root / filename
        if path.is_file():
            result[filename] = path.read_text().strip()
    return result


def journal(unit, now, window_seconds):
    # Absolute, rounded-down cutoff keeps the advertised window intact even if
    # the preceding systemctl query is delayed; summarize applies exact bounds.
    cutoff = int(now.timestamp() - window_seconds)
    return subprocess.check_output(
        ["journalctl", "-u", unit, "--since", f"@{cutoff}",
         "--no-pager", "-o", "cat"], text=True)


def snapshot(window_seconds):
    now = datetime.now(timezone.utc)
    unit = "usnm-ocr-worker.service"
    properties = subprocess.check_output(
        ["systemctl", "show", unit, "-p", "MainPID", "-p", "ActiveState",
         "-p", "ExecMainStartTimestamp", "-p", "ControlGroup"], text=True)
    worker = dict(line.split("=", 1) for line in properties.splitlines() if "=" in line)
    logs = journal(unit, now, window_seconds)
    report = {"observed_at": now.isoformat(), "worker": worker,
              "throughput": summarize(logs.splitlines(), now, window_seconds),
              "worker_cgroup": read_cgroup(worker["ControlGroup"]),
              "host_cpu": Path("/proc/stat").read_text().splitlines()[0],
              "host_memory": Path("/proc/meminfo").read_text(),
              "load": Path("/proc/loadavg").read_text().strip()}
    try:
        engine_uid = pwd.getpwnam("usnmeng").pw_uid
        engine_group = subprocess.check_output(
            ["systemctl", "show", f"user@{engine_uid}.service", "-p", "ControlGroup", "--value"], text=True).strip()
        report["engine_cgroup"] = read_cgroup(engine_group)
    except (subprocess.CalledProcessError, KeyError):
        report["engine_cgroup"] = None
    report["pressure"] = {kind: (Path("/proc/pressure") / kind).read_text()
                          for kind in ("cpu", "memory", "io")
                          if (Path("/proc/pressure") / kind).exists()}
    return report


def baseline_ready(report, min_pages=10, max_age_seconds=300):
    throughput = report["throughput"]
    age = throughput["last_success_age_seconds"]
    return (report["worker"]["ActiveState"] == "active"
            and throughput["successful_pages"] >= min_pages
            and throughput["error_events"] == 0
            and age is not None and age <= max_age_seconds)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--host")
    parser.add_argument("--window-seconds", type=int, default=900)
    parser.add_argument("--out", type=Path)
    args = parser.parse_args()
    if not 60 <= args.window_seconds <= 86400:
        parser.error("--window-seconds must be 60..86400")
    if args.host:
        # The identical reviewed source runs from stdin; no host files are installed.
        result = subprocess.run(
            ["ssh", "-oBatchMode=yes", "-oConnectTimeout=8", "-oStrictHostKeyChecking=yes",
             "--", args.host, f"sudo -n python3 - --window-seconds={args.window_seconds}"],
            input=Path(__file__).read_text(), text=True, capture_output=True, timeout=60, check=True)
        report = json.loads(result.stdout)
    else:
        report = snapshot(args.window_seconds)
        report["baseline_ready"] = baseline_ready(report)
    output = json.dumps(report, indent=2) + "\n"
    if args.out:
        with args.out.open("x") as stream:
            stream.write(output)
    print(output, end="")


if __name__ == "__main__":
    main()
