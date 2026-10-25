#!/usr/bin/env python3
"""Collect bounded, credential-free OCR/BOINC aggregate evidence, not file inventory."""

import argparse
from datetime import datetime, timedelta, timezone
from http.client import HTTPException
import json
import os
from pathlib import Path
import tempfile
from urllib.parse import urlsplit
from urllib.request import urlopen
import xml.etree.ElementTree as ET

MAX_BYTES = 1024 * 1024
MAX_COUNT = 10**12
# The dashboard rejects a whole snapshot that lists more issues than this (pipeline.rs).
MAX_ISSUES = 128
# A catalog snapshot generated, or whose newest evidence was observed, longer ago than this is still
# joined but flagged `catalog-frozen`. It is above the one to two day gaps the observer tests use;
# --catalog-max-age-days overrides it.
CATALOG_MAX_AGE_DAYS = 3


def timestamp(value):
    if not isinstance(value, str):
        raise ValueError("Missing source timestamp")
    parsed = datetime.fromisoformat(value.replace("Z", "+00:00"))
    if parsed.tzinfo is None:
        raise ValueError("Source timestamp requires a timezone")
    try:
        return parsed.astimezone(timezone.utc)
    except OverflowError as error:
        raise ValueError("Source timestamp is outside the supported range") from error


def utc(value):
    return value.astimezone(timezone.utc).isoformat().replace("+00:00", "Z")


def count(value):
    if isinstance(value, bool) or not isinstance(value, int) or not 0 <= value <= MAX_COUNT:
        raise ValueError("Invalid aggregate count")
    return value


def object_value(value):
    if not isinstance(value, dict):
        raise ValueError("Expected a source object")
    return value


def bounded_text(value, limit):
    if not isinstance(value, str) or not value or len(value) > limit:
        raise ValueError("Invalid catalog text")
    if any(ord(character) < 32 or 127 <= ord(character) < 160 for character in value):
        raise ValueError("Catalog text contains control characters")
    return value


def observed(value, now):
    result = timestamp(value)
    if result > now + timedelta(seconds=60):
        raise ValueError("Future source timestamp")
    return utc(result)


def day_limit(text):
    try:
        days = int(text)
    except ValueError:
        days = 0
    if not 1 <= days <= 365:
        raise argparse.ArgumentTypeError("expected a whole number of days from 1 to 365")
    return days


def day_count(age):
    days = max(age.days, 0)
    return f"{days} day" if days == 1 else f"{days} days"


def frozen_catalog_issue(generated_at, observed_at, now, max_age_days):
    """Flag a snapshot that has stopped moving; the join would otherwise report it as current."""
    stamps = [("generated", generated_at), ("newest evidence observed", observed_at)]
    ages = [(label, value, now - timestamp(value)) for label, value in stamps if value is not None]
    if not any(age > timedelta(days=max_age_days) for _, _, age in ages):
        return None
    described = "; ".join(f"{label} {value} ({day_count(age)} old)" for label, value, age in ages)
    return {"code": "catalog-frozen",
            "detail": f"Catalog snapshot {described}. The limit is {day_count(timedelta(days=max_age_days))}, "
                      "so its counts describe past evidence, not current work."}


def stage(identifier, label, amount, at, detail):
    return {
        "id": identifier, "label": label, "count": count(amount),
        "state": "observed", "observed_at": at, "detail": detail,
    }


def local_stages(payload, now):
    """Consume only existing aggregate counters; never infer artifact presence.

    The fetcher's status describes one run of one manifest, and the fetch queue rewrites it for every
    queued manifest, so the scan stages come from the dashboard's cumulative counts instead: every
    page in the fetch index, and the pages of the queue's manifests (queue-ocr-*, queue-quality-*).
    """
    payload = object_value(payload)
    fetch = object_value(payload["fetch"])
    if fetch.get("index_error") or fetch.get("queue_error"):
        raise ValueError("Local fetch evidence reports an error")
    if fetch.get("queue_configured") is not True:
        raise ValueError("The dashboard reads no fetch queue")
    # The counts are read from the files at each dashboard poll, so they are as old as the view.
    at = observed(payload["generated_at"], now)
    total = object_value(fetch["total"])
    queue = object_value(fetch["queue_summary"])
    fetched_all = count(total["pages"])
    pages, fetched, left = (count(queue[key]) for key in ("pages", "pages_fetched", "pages_left"))
    if fetched > pages or left > pages:
        raise ValueError("Queued counts exceed the queued population")
    result = [
        stage("scan_cached", "Queued pages fetched", fetched, at,
              "Pages of the fetch queue's manifests in the fetch index, by any run; not per-file "
              "checksum or Azure mirror verification."),
        stage("scan_planned", "Pages in fetch-queue manifests", pages, at,
              "Every queue-ocr-* and queue-quality-* manifest on the scan store, fetched or not; "
              "not the entire corpus."),
        stage("scan_remaining", "Queued pages not yet fetched", left, at,
              "In manifests the queue will still run (queued, fetching or cooling off); "
              "not a fetch command or allocation."),
        stage("scan_total", "Scans fetched, all runs", fetched_all, at,
              "Unique pages in the fetch index, from every plan and queued manifest."),
    ]
    nodes = payload["nodes"]
    if not isinstance(nodes, list) or len(nodes) > 100:
        raise ValueError("Invalid worker inventory")
    seen = set()
    for node in nodes:
        node = object_value(node)
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
    if len(body) > MAX_BYTES:
        raise ValueError("Oversized BOINC XML")
    text = body.decode("utf-8")
    if "<!DOCTYPE" in text.upper() or "<!ENTITY" in text.upper():
        raise ValueError("Unsafe or oversized BOINC XML")
    root = ET.fromstring(text)
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


def published_stages(body, now):
    """The serving index's pages whose searchable text is our OCR, from the public API's status."""
    if len(body) > MAX_BYTES:
        raise ValueError("Oversized API status")
    status = object_value(json.loads(body))
    if status.get("stale") is True or status.get("error"):
        raise ValueError("The API reports stale or failed status")
    at = observed(status["generated_at"], now)
    published = object_value(status["published"])
    version = bounded_text(published["index_version"], 64)
    published_at = observed(published["published_at"], now)
    local = object_value(object_value(published["sources"])["ocr_local"])
    pages = count(local["pages"])
    if pages > count(published["pages"]):
        raise ValueError("Inconsistent published page counts")
    return [stage("indexed", "Pages searchable with our OCR", pages, at,
                  f"Serving index {version}, published {published_at}: pages whose searchable text is "
                  "our OCR (sources.ocr_local). Not a count of OCR runs or accepted readings.")]


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


def collect(ocr_url, boinc_url, now=None, api_status_url=None):
    now = now or datetime.now(timezone.utc)
    sources = [
        ("local-ocr", ocr_url, lambda body: local_stages(json.loads(body), now)),
        ("boinc-public-status", boinc_url, lambda body: boinc_stages(body, now)),
    ]
    if api_status_url:
        sources.append(("published-index", api_status_url, lambda body: published_stages(body, now)))
    events, failures = [], []
    for source, url, parse in sources:
        try:
            stages = parse(fetch(url))
            events.extend({"source": source, "stage": item} for item in stages)
        except (OSError, HTTPException, ValueError, KeyError, TypeError, RecursionError, ET.ParseError):
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
        missing = {
            "boinc-public-status-unavailable": [("boinc_unsent", "BOINC unsent result replicas"),
                                                ("boinc_in_progress", "BOINC assigned result replicas")],
            "published-index-unavailable": [],
        }.get(failure["code"], [("scan_cached", "Queued pages fetched")])
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
        if any(item["id"] == identifier for item in stages):
            continue
        stages.append({"id": identifier, "label": label, "count": None,
                       "state": "unknown", "observed_at": None,
                       "detail": "Source adapter not connected; aggregate status cannot establish this stage."})
    mandatory = {"scan_cached", "scan_planned", "scan_remaining", "boinc_unsent",
                 "boinc_in_progress", "artifact_verified", "run_history",
                 "accepted", "imported", "indexed"}
    room = 64 - sum(item["id"] in mandatory for item in stages)
    retained = []
    for item in stages:
        if item["id"] in mandatory:
            retained.append(item)
        elif room:
            retained.append(item)
            room -= 1
    omitted = len(stages) - len(retained)
    stages = retained
    issues = list(failures)
    if omitted:
        issues.append({"code": "stage-detail-truncated",
                       "detail": f"Omitted {omitted} worker/application detail stages to fit the dashboard limit; source totals are retained."})
    return {
        "schema_version": 1, "observed_at": utc(at) if at else None,
        "generated_at": utc(now), "producer": "corpus-catalog",
        "coverage": [
            "Aggregate observer only; no page-level inventory or run catalog loaded.",
            "Local fetch-index and fetch-queue counts, public BOINC result replicas and the serving index's OCR pages.",
            "BOINC source cache age and application/device eligibility are unknown.",
        ],
        "stages": stages, "runs": {"total": 0, "pages": 0},
        "artifacts": {"locations": 0, "verified": 0, "missing": 0},
        "issues": issues + [{"code": "partial-coverage",
                              "detail": "Zero catalog totals mean no page evidence loaded, not an empty corpus."}],
    }


def add_catalog(value, path, now=None, max_age_days=CATALOG_MAX_AGE_DAYS):
    """Join cataloged evidence counts without claiming a complete corpus inventory."""
    now = now or datetime.now(timezone.utc)
    with Path(path).open("rb") as stream:
        body = stream.read(MAX_BYTES + 1)
    if len(body) > MAX_BYTES:
        raise ValueError("Catalog snapshot exceeds 1 MiB")
    catalog = json.loads(body)
    if type(catalog["schema_version"]) is not int or catalog["schema_version"] != 1 or catalog["producer"] != "corpus-catalog":
        raise ValueError("Unsupported catalog snapshot")
    generated = observed(catalog["generated_at"], now)
    at = observed(catalog["observed_at"], now) if catalog["observed_at"] is not None else None
    frozen = frozen_catalog_issue(generated, at, now, max_age_days)
    runs = {key: count(catalog["runs"][key]) for key in ("total", "pages")}
    artifacts = {key: count(catalog["artifacts"][key])
                 for key in ("locations", "verified", "missing")}
    if artifacts["verified"] + artifacts["missing"] > artifacts["locations"]:
        raise ValueError("Inconsistent catalog location counts")
    if runs["pages"] > runs["total"]:
        raise ValueError("Inconsistent catalog run counts")
    if at is None and (any(runs.values()) or any(artifacts.values())):
        raise ValueError("Nonempty catalog has no source timestamp")
    catalog_stages = catalog["stages"]
    if not isinstance(catalog_stages, list) or len(catalog_stages) > 256:
        raise ValueError("Invalid catalog stage inventory")
    evidence = {}
    for candidate in catalog_stages:
        candidate = object_value(candidate)
        identifier = candidate["id"]
        if identifier not in ("runs", "artifacts", "accepted", "imported", "indexed"):
            continue
        if identifier in evidence:
            raise ValueError("Duplicate catalog evidence stage")
        state = candidate["state"]
        if state not in ("observed", "unknown", "blocked"):
            raise ValueError("Invalid catalog evidence state")
        amount = count(candidate["count"]) if candidate["count"] is not None else None
        source_at = observed(candidate["observed_at"], now) if candidate["observed_at"] is not None else None
        total = (runs["total"] if identifier == "runs" else
                 artifacts["locations"] if identifier == "artifacts" else 0)
        if state == "observed" and (amount is None or source_at is None):
            raise ValueError("Observed catalog evidence requires a count and timestamp")
        if state == "unknown" and amount is not None:
            raise ValueError("Unknown catalog evidence cannot have a count")
        count_invalid = False
        if identifier == "runs":
            count_invalid = amount != runs["pages"]
        elif identifier == "artifacts":
            count_invalid = amount is not None and (amount > total or bool(amount) != bool(total))
        if amount is not None and (count_invalid or source_at is None):
            raise ValueError("Inconsistent catalog evidence stage")
        if amount is None and total:
            raise ValueError("Nonempty catalog totals have no stage evidence")
        evidence[identifier] = {"state": state, "count": amount, "observed_at": source_at}
        if identifier in ("accepted", "imported", "indexed"):
            evidence[identifier]["detail"] = bounded_text(candidate["detail"], 512)
    for identifier, expected in (("runs", runs["total"]), ("artifacts", artifacts["locations"])):
        if identifier not in evidence:
            if expected:
                raise ValueError("Nonempty catalog totals have no stage")
            evidence[identifier] = {"state": "unknown", "count": None, "observed_at": None}
    coverage = catalog["coverage"]
    issues = catalog["issues"]
    if not isinstance(coverage, list) or len(coverage) > 32:
        raise ValueError("Invalid catalog coverage")
    coverage = [bounded_text(item, 128) for item in coverage]
    if not isinstance(issues, list) or len(issues) > 128:
        raise ValueError("Invalid catalog issues")
    issues = [{"code": bounded_text(object_value(item)["code"], 64),
               "detail": bounded_text(item["detail"], 512)} for item in issues]
    value["runs"], value["artifacts"] = runs, artifacts
    value["coverage"][0] = "Aggregate observer joined to a partial page-evidence catalog; not a full corpus inventory."
    room = 32 - len(value["coverage"])
    value["coverage"].extend(coverage[:room] if len(coverage) <= room else
                             coverage[:room - 1] + ["Additional catalog coverage omitted; query the catalog directly."])
    value["issues"] = [issue for issue in value["issues"] if issue["code"] != "partial-coverage"]
    value["issues"].append({"code": "partial-catalog",
                            "detail": "Catalog counts cover retained evidence only, not the corpus. Missing acceptance/import/index sources remain unknown."})
    if frozen:
        # Before the catalog's own issues, which are cut to fit, so a full list cannot push it out.
        value["issues"].append(frozen)
    room = MAX_ISSUES - len(value["issues"])
    value["issues"].extend(issues[:room] if len(issues) <= room else
                           issues[:room - 1] + [{"code": "catalog-issues-truncated",
                                                "detail": "Additional catalog issues omitted; query the catalog directly."}])
    replacements = {
        "artifact_verified": ("artifacts", "Cataloged verified locations", artifacts["verified"]),
        "run_history": ("runs", "Cataloged engine runs", runs["total"]),
    }
    for identifier, label in (("accepted", "Cataloged acceptance observations"),
                              ("imported", "Cataloged curated imports"),
                              ("indexed", "Cataloged search inclusion")):
        if identifier in evidence:
            replacements[identifier] = (identifier, label, evidence[identifier]["count"])
    for item in value["stages"]:
        # The serving index's own count is current; the catalog's search-inclusion evidence is not.
        if item["id"] == "indexed" and item["state"] == "observed":
            continue
        if item["id"] in replacements:
            identifier, label, amount = replacements[item["id"]]
            source = evidence[identifier]
            item.update(label=label, count=amount if source["count"] is not None else None,
                        state=source["state"], observed_at=source["observed_at"],
                        detail=source.get("detail",
                            "Imported page evidence only; source age is independent of live aggregate refresh."))
    times = [timestamp(item["observed_at"]) for item in value["stages"]
             if item.get("observed_at") is not None]
    value["observed_at"] = utc(min(times)) if times else None
    return value


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--ocr-url", required=True)
    parser.add_argument("--boinc-url", default="https://boinc.usnewsmap.com/server_status.php?xml=1")
    parser.add_argument("--api-status-url", default="https://api.usnewsmap.com/v1/status",
                        help="The public API's status, for the serving index's pages with our OCR; "
                             "empty to skip")
    parser.add_argument("--snapshot", required=True, type=Path)
    parser.add_argument("--catalog-snapshot", type=Path,
                        help="Optional bounded snapshot of the separate page-evidence catalog")
    parser.add_argument("--catalog-max-age-days", type=day_limit, default=CATALOG_MAX_AGE_DAYS,
                        help="Flag the catalog snapshot as frozen when it was generated, or its newest "
                             f"evidence observed, more than this many days ago (default {CATALOG_MAX_AGE_DAYS})")
    args = parser.parse_args()
    events, failures = collect(args.ocr_url, args.boinc_url, api_status_url=args.api_status_url or None)
    value = snapshot(events, failures)
    if args.catalog_snapshot:
        try:
            add_catalog(value, args.catalog_snapshot, max_age_days=args.catalog_max_age_days)
        except (OSError, ValueError, KeyError, TypeError, RecursionError):
            failures.append({"code": "catalog-unavailable",
                             "detail": "Page-evidence catalog snapshot could not be read or validated."})
            value["issues"].append(failures[-1])
    atomic_json(args.snapshot, value)
    print(json.dumps({"event": "data_flow_observation", "stages": len(events),
                      "failed_sources": len(failures)}))
    return 1 if failures else 0


if __name__ == "__main__":
    raise SystemExit(main())
