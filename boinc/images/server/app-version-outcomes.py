#!/usr/bin/env python3
"""Count one app version's reported results by outcome and validate state since a time. Read-only.

The release window (boinc/scripts/onprem/release-window.py, docs/boinc/apps/app-release.md "Held
versions") holds a version again on its first client error. Client errors never reach the
validator, so its log can't show them; this reads them from the result table with the runtime
user's defaults file, as work-volume.py does, and prints one JSON document.
"""

import argparse
import json
import re
import subprocess
import sys

DATABASE = re.compile(r"[A-Za-z0-9_]+")
# BOINC's RESULT_OUTCOME and VALIDATE_STATE values (boinc_db_types.h).
OUTCOMES = {1: "success", 3: "client_error", 4: "no_reply", 5: "didnt_need", 6: "validate_error",
            7: "client_detached"}
VALIDATE = {0: "init", 1: "valid", 2: "invalid", 3: "no_check", 4: "inconclusive", 5: "too_late"}


def summary(rows, app_version_id, since):
    counts = {"outcome": {}, "validate_state": {}}
    for outcome, validate_state, n in rows:
        o = OUTCOMES.get(outcome, f"other_{outcome}")
        v = VALIDATE.get(validate_state, f"other_{validate_state}")
        counts["outcome"][o] = counts["outcome"].get(o, 0) + n
        counts["validate_state"][v] = counts["validate_state"].get(v, 0) + n
    return {"event": "boinc_app_version_outcomes", "schema_version": 1, "app_version_id": app_version_id,
            "since": since, "client_errors": counts["outcome"].get("client_error", 0),
            "validate_errors": counts["outcome"].get("validate_error", 0),
            "valid": counts["validate_state"].get("valid", 0),
            "invalid": counts["validate_state"].get("invalid", 0), **counts}


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--mysql-defaults-file", required=True)
    parser.add_argument("--database", required=True)
    parser.add_argument("--app-version-id", type=int, required=True)
    parser.add_argument("--since", type=int, required=True, help="Unix time; results received from then")
    args = parser.parse_args(argv)
    if not DATABASE.fullmatch(args.database) or args.app_version_id < 1 or args.since < 0:
        print("usnm-boinc: invalid app-version-outcomes arguments", file=sys.stderr)
        return 2
    result = subprocess.run(
        ["mysql", f"--defaults-extra-file={args.mysql_defaults_file}", "--ssl", "--ssl-verify-server-cert",
         "--ssl-ca=/usr/local/share/usnm/mysql-ca.pem", "--batch", "--raw", "--skip-column-names",
         f"--database={args.database}", "--execute",
         "SELECT outcome, validate_state, COUNT(*) FROM result "
         f"WHERE app_version_id = {args.app_version_id} AND received_time >= {args.since} "
         "AND server_state = 5 GROUP BY outcome, validate_state"],
        check=True, capture_output=True, text=True)
    rows = [tuple(int(x) for x in line.split("\t")) for line in result.stdout.splitlines() if line]
    print(json.dumps(summary(rows, args.app_version_id, args.since), sort_keys=True))
    return 0


if __name__ == "__main__":
    sys.exit(main())
