#!/usr/bin/env python3
"""Audit controller routing telemetry, not authorization to retire Azure resources."""

import argparse
import hashlib
import json
import math
from pathlib import Path
import re
import time

DAY = 24 * 60 * 60
MAX_GAP = 20 * 60
UNIT = "usnm-boinc-priority-controller.service"
STARTING = "7d4958e842da4a758f6c1cdc7b36dcc5"
FINISHED = "39f53479d3a045ac8e11786248231fbf"
REQUIRED_ROUTES = {
    "onprem/handoff/claim", "onprem/handoff/collect", "onprem/handoff/export", "onprem/scans",
}


def number(value):
    return isinstance(value, (int, float)) and not isinstance(value, bool) and math.isfinite(value)


def audit(records, since, until, unit=UNIT):
    failures = set()
    ticks = []
    routes = set()
    plans = set()
    published = 0
    pushed = 0
    invocations = {}
    if not number(since) or not number(until) or until - since < DAY:
        raise ValueError("window must span at least 24 hours")
    for record in records:
        # Read the full journal export, including failures, rather than a success-only selection.
        at = int(record["__REALTIME_TIMESTAMP"]) / 1_000_000
        if not since <= at <= until:
            continue
        associated = any(record.get(key) == unit for key in ("UNIT", "_SYSTEMD_UNIT", "OBJECT_SYSTEMD_UNIT"))
        invocation = None
        if associated:
            invocation = record.get("INVOCATION_ID") or record.get("_SYSTEMD_INVOCATION_ID")
            if not isinstance(invocation, str) or re.fullmatch(r"[0-9a-f]{32}", invocation) is None:
                failures.add("controller journal record lacks a valid invocation identity")
                invocation = None
            else:
                state = invocations.setdefault(invocation, {"starts": 0, "finishes": 0, "summaries": 0})
                if record.get("MESSAGE_ID") == STARTING:
                    state["starts"] += 1
                elif record.get("MESSAGE_ID") == FINISHED:
                    state["finishes"] += 1
            for key in ("UNIT_RESULT", "SERVICE_RESULT"):
                if key in record and record[key] != "success":
                    failures.add("systemd reports a failed controller invocation")
            if ((("EXIT_CODE" in record or "EXIT_STATUS" in record)
                 and (record.get("EXIT_CODE"), record.get("EXIT_STATUS")) != ("exited", "0"))
                    or ("JOB_RESULT" in record and record["JOB_RESULT"] != "done")):
                failures.add("systemd reports a failed controller invocation")
        message = record["MESSAGE"]
        if not isinstance(message, str):
            raise ValueError("journal MESSAGE must be a string")
        if not message.startswith("{"):
            continue
        event = json.loads(message)
        if not isinstance(event, dict):
            raise ValueError("controller event must be an object")
        kind = event.get("event", "")
        if not isinstance(kind, str):
            raise ValueError("controller event kind must be a string")
        if kind.startswith("boinc_batch_") and record.get("_SYSTEMD_UNIT") != unit:
            failures.add("controller event has missing or mismatched journal unit")
        if kind in ("boinc_batch_tick", "boinc_batch_failure") and invocation is not None:
            invocations[invocation]["summaries"] += 1
        if kind == "boinc_batch_failure":
            failures.add("controller failure in window")
        elif kind == "boinc_batch_phase" and event.get("ok") is not True:
            failures.add("failed controller phase in window")
        elif kind == "boinc_batch_tick":
            ticks.append(at)
            plan = event.get("plan")
            if not isinstance(plan, str) or not plan:
                failures.add("tick lacks plan identity")
            else:
                plans.add(plan)
            if (type(event.get("schema_version")) is not int or event["schema_version"] != 1
                    or event.get("draining") is not False
                    or type(event.get("collection_backlog")) is not int or event["collection_backlog"] != 0
                    or type(event.get("collection_held")) is not int or event["collection_held"] != 0
                    or event.get("alerts") != []):
                failures.add("tick is not a healthy non-draining schema-v1 observation")
            new = event.get("newly_published")
            if not isinstance(new, int) or isinstance(new, bool) or new < 0:
                failures.add("tick lacks a valid publication count")
            else:
                published += new
            timing = event.get("timings")
            if not isinstance(timing, dict):
                failures.add("tick lacks command telemetry")
                continue
            observed = timing.get("execution_routes")
            commands = timing.get("commands")
            if not isinstance(observed, dict) or not isinstance(commands, dict):
                failures.add("tick lacks actual execution-route telemetry")
                continue
            routed = {}
            for route, counts in observed.items():
                parts = route.split("/", 1)
                if len(parts) != 2 or parts[0] not in ("onprem", "azure"):
                    failures.add("unknown execution route")
                    continue
                dest, command = parts
                count = counts.get("count") if isinstance(counts, dict) else None
                if (not isinstance(count, int) or isinstance(count, bool) or count <= 0
                        or type(counts.get("failed")) is not int or counts["failed"] != 0):
                    failures.add("failed or invalid routed command")
                    continue
                if dest != "onprem":
                    failures.add("Azure execution or scan push in window")
                routes.add(route)
                label = "cached scan push" if command == "scans" else command
                routed[label] = routed.get(label, 0) + count
            expected = {}
            for label, counts in commands.items():
                count = counts.get("count") if isinstance(counts, dict) else None
                if (not isinstance(count, int) or isinstance(count, bool) or count <= 0
                        or type(counts.get("failed")) is not int or counts["failed"] != 0):
                    failures.add("failed or invalid command")
                    continue
                if label.startswith(("handoff/", "bridge/")) or label == "cached scan push":
                    expected[label] = count
            if expected != routed:
                failures.add("execution routes do not reconcile with command counts")
            scans = timing.get("scan_pages")
            if not isinstance(scans, dict):
                failures.add("tick lacks parsed scan-page evidence")
                continue
            for dest, counts in scans.items():
                if (dest not in ("onprem", "azure") or not isinstance(counts, dict)
                        or any(type(counts.get(key)) is not int or counts[key] < 0
                               for key in ("pushed", "missing"))
                        or f"{dest}/scans" not in observed):
                    failures.add("invalid scan-page evidence")
                elif dest == "onprem":
                    pushed += counts["pushed"]
            if "onprem/scans" in observed and "onprem" not in scans:
                failures.add("on-prem scan command lacks parsed page counts")
    ticks.sort()
    if not ticks or max(b - a for a, b in zip([since, *ticks], [*ticks, until])) > MAX_GAP:
        failures.add("controller observation gap exceeds 20 minutes")
    if len(ticks) != len(set(ticks)):
        failures.add("duplicate tick timestamps")
    if len(plans) != 1:
        failures.add("window must contain exactly one controller plan")
    if not REQUIRED_ROUTES <= routes:
        failures.add("window lacks real on-prem claim/collect/export/scan-push observations")
    if published == 0:
        failures.add("window has no newly published pages")
    if pushed == 0:
        failures.add("window has no actual on-prem pushed pages")
    if not invocations or any(state != {"starts": 1, "finishes": 1, "summaries": 1}
                              for state in invocations.values()):
        failures.add("window contains an unresolved or boundary-crossing controller invocation")
    return {
        "schema_version": 1,
        "scope": "controller-routing-window",
        "unit": unit,
        "since": since, "until": until, "ticks": len(ticks),
        "controller_invocations": len(invocations),
        "plans": sorted(plans), "newly_published": published,
        "onprem_pushed_pages": pushed,
        "observed_routes": sorted(routes),
        "telemetry_window_passed": not failures,
        "failures": sorted(failures),
        "retirement_authorized": False,
        "remaining_evidence": [
            "stable reviewed binary, configuration and unit throughout the window",
            "independent Azure handoff execution history covering the full window",
            "retained local payload checksums, restart and retention acceptance",
            "main-based what-if, live API pin match and maintainer-approved provision",
        ],
    }


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--journal", type=Path, required=True,
                        help="full journalctl -u SERVICE -o json export (do not filter successes)")
    parser.add_argument("--since", type=int, required=True, help="window start, Unix seconds")
    parser.add_argument("--until", type=int, required=True, help="window end, Unix seconds")
    parser.add_argument("--unit", default=UNIT, help="expected controller systemd service")
    args = parser.parse_args(argv)
    try:
        if args.until > time.time():
            raise ValueError("window end must not be in the future")
        raw = args.journal.read_bytes()
        records = [json.loads(line) for line in raw.splitlines() if line.strip()]
        report = audit(records, args.since, args.until, args.unit)
    except (OSError, ValueError, KeyError, TypeError) as error:
        parser.error(str(error))
    report["journal_sha256"] = hashlib.sha256(raw).hexdigest()
    print(json.dumps(report, sort_keys=True))
    return 0 if report["telemetry_window_passed"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
