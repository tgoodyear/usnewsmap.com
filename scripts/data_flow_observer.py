#!/usr/bin/env python3
"""Collect bounded, credential-free OCR/BOINC aggregate evidence, not file inventory."""

import argparse
from datetime import datetime, timedelta, timezone
import json
import os
from pathlib import Path
import tempfile
from urllib.parse import urlsplit
from urllib.request import urlopen
import xml.etree.ElementTree as ET

MAX_BYTES = 1024 * 1024
MAX_COUNT = 2**53 - 1


def timestamp(value):
    if not isinstance(value, str):
        raise ValueError("Missing source timestamp")
    parsed = datetime.fromisoformat(value.replace("Z", "+00:00"))
    if parsed.tzinfo is None:
        raise ValueError("Source timestamp requires a timezone")
    return parsed.astimezone(timezone.utc)


def utc(value):
    return value.astimezone(timezone.utc).isoformat().replace("+00:00", "Z")


def count(value):
    if isinstance(value, bool) or not isinstance(value, int) or not 0 <= value <= MAX_COUNT:
        raise ValueError("Invalid aggregate count")
    return value


def observed(value, now):
    result = timestamp(value)
    if result > now + timedelta(seconds=60):
        raise ValueError("Future source timestamp")
    return utc(result)


def stage(identifier, label, amount, at, detail):
    return {
        "id": identifier, "label": label, "count": count(amount),
        "state": "observed", "observed_at": at, "detail": detail,
    }


def local_stages(payload, now):
    """Consume only existing aggregate counters; never infer artifact presence."""
    fetch = payload["fetch"]
    if fetch.get("status_error") or fetch.get("plan_error"):
        raise ValueError("Local fetch evidence reports an error")
    at = observed(fetch["status"]["updated_at"], now)
    sets = fetch["sets"]
    keep = [item for item in sets if item["name"] == "keep"]
    if len(keep) != 1:
        raise ValueError("Expected one keep-set aggregate")
    fetched, total = count(keep[0]["fetched"]), count(keep[0]["total"])
    if fetched > total:
        raise ValueError("Fetched count exceeds planned population")
    result = [
        stage("scan_cached", "Keep-set scans cached", fetched, at,
              "Scan-cache aggregate; not per-file checksum or Azure mirror verification."),
        stage("scan_planned", "Keep-set scans planned", total, at,
              "Current prefetch population, not the entire corpus."),
        stage("scan_remaining", "Keep-set scans not yet reported cached", total - fetched, at,
              "Planning difference; not a fetch command or allocation."),
    ]
    nodes = payload["nodes"]
    if not isinstance(nodes, list) or len(nodes) > 100:
        raise ValueError("Invalid worker inventory")
    seen = set()
    for node in nodes:
        name = node["name"]
        if not isinstance(name, str) or not name.replace("-", "").isalnum() or len(name) > 80:
            raise ValueError("Invalid worker identifier")
        if name in seen:
            raise ValueError("Duplicate worker identifier")
        seen.add(name)
        # Retired/missing workers remain visible in the source dashboard, not counted as live.
        if node["ok"] is not True:
            continue
        node_at = observed(node["last_ok"], now)
        result.append(stage(
            f"local_{name}_completed_hour", f"{name}: completed in trailing hour",
            count(node["pages_per_hour"]), node_at,
            "Worker-reported completion counter; not curated import or index inclusion.",
        ))
    return result


def boinc_stages(body, now):
    if len(body) > MAX_BYTES or b"<!DOCTYPE" in body.upper() or b"<!ENTITY" in body.upper():
        raise ValueError("Unsafe or oversized BOINC XML")
    root = ET.fromstring(body)
    if root.tag != "server_status":
        raise ValueError("Expected BOINC status XML")
    database = root.find("database_file_states")
    if database is None:
        raise ValueError("Missing BOINC database counters")

    def number(element, tag):
        text = element.findtext(tag)
        if text is None or not text.isascii() or not text.isdecimal():
            raise ValueError("Missing or invalid BOINC counter")
        return count(int(text))

    at = utc(now)
    note = "Public server-status observation; source cache age unavailable. Result replicas, not pages or GPU eligibility."
    result = [
        stage("boinc_unsent", "BOINC unsent result replicas",
              number(database, "results_ready_to_send"), at, note),
        stage("boinc_in_progress", "BOINC assigned result replicas",
              number(database, "results_in_progress"), at, note),
    ]
    apps = database.findall("tasks_by_app/app")
    if not apps or len(apps) > 100:
        raise ValueError("Missing or oversized BOINC app inventory")
    seen = set()
    for app in apps:
        name = app.findtext("name", "")
        if not name.startswith("usnm_") or not name.replace("_", "").isalnum() or len(name) > 80:
            raise ValueError("Invalid BOINC application")
        if name in seen:
            raise ValueError("Duplicate BOINC application")
        seen.add(name)
        for field, label in [("unsent", "unsent"), ("in_progress", "assigned")]:
            result.append(stage(f"boinc_{name}_{field}", f"{name}: {label} replicas",
                                number(app, field), at, note))
    return result


def fetch(url):
    parts = urlsplit(url)
    if parts.scheme not in ("http", "https") or not parts.hostname or parts.username or parts.password:
        raise ValueError("Observer requires a credential-free HTTP(S) endpoint")
    if parts.fragment or (parts.query and parts.query != "xml=1"):
        raise ValueError("Observer URL cannot contain credentials or arbitrary query parameters")
    with urlopen(url, timeout=15) as response:
        if response.status != 200:
            raise ValueError("Observer endpoint did not return HTTP 200")
        body = response.read(MAX_BYTES + 1)
    if len(body) > MAX_BYTES:
        raise ValueError("Observer response exceeds 1 MiB")
    return body


def collect(ocr_url, boinc_url, now=None):
    now = now or datetime.now(timezone.utc)
    sources = [
        ("local-ocr", ocr_url, lambda body: local_stages(json.loads(body), now)),
        ("boinc-public-status", boinc_url, lambda body: boinc_stages(body, now)),
    ]
    events, failures = [], []
    for source, url, parse in sources:
        try:
            stages = parse(fetch(url))
            events.extend({"source": source, "stage": item} for item in stages)
        except (OSError, ValueError, KeyError, TypeError, ET.ParseError):
            # Keep remote bodies, URL strings and diagnostic details out of telemetry.
            failures.append({"code": f"{source}-unavailable",
                             "detail": f"{source} evidence could not be read or validated."})
    return events, failures


def atomic_json(path, value):
    path = Path(path)
    encoded = (json.dumps(value, sort_keys=True, indent=2) + "\n").encode()
    if len(encoded) > MAX_BYTES:
        raise ValueError("Snapshot exceeds 1 MiB")
    descriptor, temporary = tempfile.mkstemp(prefix=f".{path.name}.", dir=path.parent)
    try:
        with os.fdopen(descriptor, "wb") as stream:
            stream.write(encoded)
            stream.flush()
            os.fsync(stream.fileno())
        os.chmod(temporary, 0o644)
        os.replace(temporary, path)
    finally:
        if os.path.exists(temporary):
            os.unlink(temporary)


def snapshot(events, failures, now=None):
    now = now or datetime.now(timezone.utc)
    stages = [item["stage"] for item in events]
    at = min((timestamp(item["observed_at"]) for item in stages), default=None)
    for failure in failures:
        missing = (
            [("boinc_unsent", "BOINC unsent result replicas"),
             ("boinc_in_progress", "BOINC assigned result replicas")]
            if failure["code"] == "boinc-public-status-unavailable"
            else [("scan_cached", "Keep-set scans cached")]
        )
        for identifier, label in missing:
            stages.append({"id": identifier, "label": label, "count": None,
                           "state": "unknown", "observed_at": None,
                           "detail": failure["detail"]})
    for identifier, label in [
        ("artifact_verified", "Per-file verified locations"),
        ("run_history", "Cataloged engine runs"),
        ("accepted", "Accepted OCR"),
        ("imported", "Curated imports"),
        ("indexed", "Released search inclusion"),
    ]:
        stages.append({"id": identifier, "label": label, "count": None,
                       "state": "unknown", "observed_at": None,
                       "detail": "Source adapter not connected; aggregate status cannot establish this stage."})
    return {
        "schema_version": 1, "observed_at": utc(at) if at else None,
        "generated_at": utc(now), "producer": "corpus-catalog",
        "coverage": [
            "Aggregate observer only; no page-level inventory or run catalog loaded.",
            "Local prefetch/worker counters and public BOINC result replicas.",
            "BOINC source cache age and application/device eligibility are unknown.",
        ],
        "stages": stages, "runs": {"total": 0, "pages": 0},
        "artifacts": {"locations": 0, "verified": 0, "missing": 0},
        "issues": failures + [{"code": "partial-coverage",
                              "detail": "Zero catalog totals mean no page evidence loaded, not an empty corpus."}],
    }


def add_catalog(value, path, now=None):
    """Join cataloged evidence counts without claiming a complete corpus inventory."""
    now = now or datetime.now(timezone.utc)
    with Path(path).open("rb") as stream:
        body = stream.read(MAX_BYTES + 1)
    if len(body) > MAX_BYTES:
        raise ValueError("Catalog snapshot exceeds 1 MiB")
    catalog = json.loads(body)
    if catalog["schema_version"] != 1 or catalog["producer"] != "corpus-catalog":
        raise ValueError("Unsupported catalog snapshot")
    observed(catalog["generated_at"], now)
    at = observed(catalog["observed_at"], now) if catalog["observed_at"] is not None else None
    runs = {key: count(catalog["runs"][key]) for key in ("total", "pages")}
    artifacts = {key: count(catalog["artifacts"][key])
                 for key in ("locations", "verified", "missing")}
    if artifacts["verified"] + artifacts["missing"] > artifacts["locations"]:
        raise ValueError("Inconsistent catalog location counts")
    if runs["pages"] > runs["total"]:
        raise ValueError("Inconsistent catalog run counts")
    if at is None and (any(runs.values()) or any(artifacts.values())):
        raise ValueError("Nonempty catalog has no source timestamp")
    value["runs"], value["artifacts"] = runs, artifacts
    value["coverage"][0] = "Aggregate observer joined to a partial page-evidence catalog; not a full corpus inventory."
    value["issues"] = [issue for issue in value["issues"] if issue["code"] != "partial-coverage"]
    value["issues"].append({"code": "partial-catalog",
                            "detail": "Catalog totals cover imported evidence only. Unconnected validation/import/index stages remain unknown."})
    replacements = {
        "artifact_verified": ("Cataloged verified locations", artifacts["verified"]),
        "run_history": ("Cataloged engine runs", runs["total"]),
    }
    for item in value["stages"]:
        if item["id"] in replacements:
            label, amount = replacements[item["id"]]
            item.update(label=label, count=amount if at else None,
                        state="observed" if at else "unknown", observed_at=at,
                        detail="Imported page evidence only; source age is independent of live aggregate refresh.")
    return value


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--ocr-url", required=True)
    parser.add_argument("--boinc-url", default="https://boinc.usnewsmap.com/server_status.php?xml=1")
    parser.add_argument("--snapshot", required=True, type=Path)
    parser.add_argument("--catalog-snapshot", type=Path,
                        help="Optional bounded snapshot of the separate page-evidence catalog")
    args = parser.parse_args()
    events, failures = collect(args.ocr_url, args.boinc_url)
    value = snapshot(events, failures)
    if args.catalog_snapshot:
        try:
            add_catalog(value, args.catalog_snapshot)
        except (OSError, ValueError, KeyError, TypeError):
            failures.append({"code": "catalog-unavailable",
                             "detail": "Page-evidence catalog snapshot could not be read or validated."})
            value["issues"].append(failures[-1])
    atomic_json(args.snapshot, value)
    print(json.dumps({"event": "data_flow_observation", "stages": len(events),
                      "failed_sources": len(failures)}))
    return 1 if failures else 0


if __name__ == "__main__":
    raise SystemExit(main())
