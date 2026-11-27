#!/usr/bin/env python3
"""List, for each of an app's versions, the recently seen hosts that could be sent it. Read-only.

The held-version qualification in docs/boinc/apps/app-release.md ("Held versions") releases a
version only when the test host is the only one that could take it. This answers that question
without anyone reading the database credentials: bridge-job.sh runs it with the runtime user's
defaults file, as it runs work-volume.py, and it prints one JSON document.

A host is a candidate for a version when its last RPC is within --days, its os_name fits the
version's platform, and, for a GPU plan class, one of the GPUs in host.misc's "gpus" list has the
class's gpu_type and fits its min_gpu_ram_mb / max_gpu_ram_mb. The scheduler checks more than
this (drivers, OpenCL, core versions), so the list is a superset of who could get the version,
which is the safe direction for an isolation check. A host whose GPU list can't be read is
listed under "uncertain" for every GPU version of its OS family, never dropped: an isolation
check must see both lists empty apart from its test host.
"""

import argparse
import json
import re
import subprocess
import sys
import xml.etree.ElementTree as ET

MAX_HOSTS = 100000
MAX_VERSIONS = 1000
APP = re.compile(r"[a-z][a-z0-9_]{0,63}")
DATABASE = re.compile(r"[A-Za-z0-9_]+")

# gpu_type in plan_class_spec.xml -> the vendor names BOINC clients report.
GPU_VENDORS = {
    "nvidia": {"nvidia", "cuda"},
    "ati": {"ati", "amd", "cal"},
    "amd": {"ati", "amd", "cal"},
    "intel": {"intel", "intel_gpu"},
    "apple": {"apple", "apple_gpu"},
}


def os_family(name):
    """The OS family a host's os_name or a platform name belongs to, or None."""
    text = (name or "").lower()
    if "windows" in text:
        return "windows"
    if "darwin" in text or "mac" in text or "apple" in text:
        return "darwin"
    if "linux" in text:
        return "linux"
    return None


def plan_classes(xml_text):
    """{name: {"gpu_type", "min_gpu_ram_mb", "max_gpu_ram_mb"}} for every plan class."""
    classes = {}
    for pc in ET.fromstring(xml_text).iter("plan_class"):
        name = (pc.findtext("name") or "").strip()
        if not name:
            continue

        def number(tag):
            value = pc.findtext(tag)
            return float(value) if value and value.strip() else None

        classes[name] = {"gpu_type": (pc.findtext("gpu_type") or "").strip().lower() or None,
                         "min_gpu_ram_mb": number("min_gpu_ram_mb"),
                         "max_gpu_ram_mb": number("max_gpu_ram_mb")}
    return classes


def gpus(raw):
    """The host's GPUs as [{"vendor", "ram_mb"}], or None if the list can't be read."""
    if raw in (None, "", "NULL", "null"):
        return []
    try:
        value = json.loads(raw)
    except ValueError:
        return None
    if value is None:
        return []
    if not isinstance(value, list):
        return None
    out = []
    for gpu in value:
        if not isinstance(gpu, dict):
            return None
        vendor = str(gpu.get("type") or gpu.get("vendor") or "").strip().lower()
        if not vendor:
            return None
        ram = gpu.get("ram_mb", gpu.get("ram"))
        try:
            ram_mb = None if ram is None else float(ram)
        except (TypeError, ValueError):
            return None
        # Some clients report bytes: anything above 1 TB as MB is bytes.
        if ram_mb is not None and ram_mb > 1e6:
            ram_mb /= 1024 * 1024
        out.append({"vendor": vendor, "ram_mb": ram_mb})
    return out


def fits(gpu, spec):
    if gpu["vendor"] not in GPU_VENDORS.get(spec["gpu_type"], {spec["gpu_type"]}):
        return False
    if gpu["ram_mb"] is None:
        return spec["min_gpu_ram_mb"] is None and spec["max_gpu_ram_mb"] is None
    if spec["min_gpu_ram_mb"] is not None and gpu["ram_mb"] < spec["min_gpu_ram_mb"]:
        return False
    if spec["max_gpu_ram_mb"] is not None and gpu["ram_mb"] > spec["max_gpu_ram_mb"]:
        return False
    return True


def eligibility(versions, hosts, classes, days):
    """The JSON document: versions as [(platform, plan_class, version_num, deprecated)], hosts as
    [(id, os_name, gpus_raw, last_rpc_unix)]."""
    out = []
    for platform, plan_class, version_num, deprecated in versions:
        family = os_family(platform)
        spec = classes.get(plan_class) if plan_class else None
        entry = {"platform": platform, "plan_class": plan_class, "version_num": version_num,
                 "held": bool(deprecated), "candidates": [], "uncertain": []}
        if plan_class and spec is None:
            entry["error"] = "plan class not in plan_class_spec.xml"
        for host_id, os_name, raw, last_rpc in hosts:
            if family is None or os_family(os_name) != family:
                continue
            row = {"host": host_id, "os_name": os_name, "last_rpc": last_rpc}
            if spec is None or spec["gpu_type"] is None:
                entry["candidates"].append(row)
                continue
            listed = gpus(raw)
            if listed is None:
                entry["uncertain"].append(row)
            elif any(fits(gpu, spec) for gpu in listed):
                entry["candidates"].append(dict(row, gpus=listed))
        out.append(entry)
    return {"event": "boinc_host_eligibility", "schema_version": 1, "days": days, "versions": out}


def query(defaults_file, database, sql):
    result = subprocess.run(
        ["mysql", f"--defaults-extra-file={defaults_file}", "--ssl", "--ssl-verify-server-cert",
         "--ssl-ca=/usr/local/share/usnm/mysql-ca.pem", "--batch", "--raw", "--skip-column-names",
         f"--database={database}", "--execute", sql],
        check=True, capture_output=True, text=True)
    return [line.split("\t") for line in result.stdout.splitlines() if line]


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--mysql-defaults-file", required=True)
    parser.add_argument("--database", required=True)
    parser.add_argument("--plan-class-spec", required=True)
    parser.add_argument("--app", default="usnm_qwen")
    parser.add_argument("--days", type=int, default=7)
    args = parser.parse_args(argv)
    if not DATABASE.fullmatch(args.database) or not APP.fullmatch(args.app) or not 1 <= args.days <= 30:
        print("usnm-boinc: invalid host-eligibility arguments", file=sys.stderr)
        return 2
    with open(args.plan_class_spec, encoding="utf-8") as f:
        classes = plan_classes(f.read())
    versions = [(r[0], r[1], int(r[2]), int(r[3])) for r in query(
        args.mysql_defaults_file, args.database,
        "SELECT p.name, av.plan_class, av.version_num, av.deprecated FROM app_version av "
        "JOIN app a ON a.id = av.appid JOIN platform p ON p.id = av.platformid "
        f"WHERE a.name = '{args.app}' AND p.deprecated = 0 ORDER BY av.id LIMIT {MAX_VERSIONS + 1}")]
    hosts = [(int(r[0]), r[1], r[2], int(r[3])) for r in query(
        args.mysql_defaults_file, args.database,
        "SELECT id, os_name, IF(JSON_VALID(misc), COALESCE(JSON_EXTRACT(misc, '$.gpus'), 'null'), 'bad'), "
        f"rpc_time FROM host WHERE rpc_time > UNIX_TIMESTAMP() - {args.days * 86400} "
        f"ORDER BY id LIMIT {MAX_HOSTS + 1}")]
    if len(versions) > MAX_VERSIONS or len(hosts) > MAX_HOSTS:
        print("usnm-boinc: host-eligibility inventory too large", file=sys.stderr)
        return 1
    print(json.dumps(eligibility(versions, hosts, classes, args.days), sort_keys=True))
    return 0


if __name__ == "__main__":
    sys.exit(main())
