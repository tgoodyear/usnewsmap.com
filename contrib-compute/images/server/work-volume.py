#!/usr/bin/env python3
"""Emit one bounded aggregate BOINC work-volume snapshot as JSON."""

import argparse
import json
import re
import subprocess
import sys


QUERY = """
SELECT
  w.total, w.need_validate_no, w.need_validate_yes,
  w.assimilate_init, w.assimilate_ready, w.assimilate_done, w.assimilate_other,
  w.file_delete_init, w.file_delete_ready, w.file_delete_done, w.file_delete_error, w.file_delete_other,
  r.total, r.inactive, r.unsent, r.in_progress, r.state_over, r.other,
  u.active_users, h.active_hosts
FROM (
  SELECT COUNT(*) AS total,
    COALESCE(SUM(need_validate = 0), 0) AS need_validate_no,
    COALESCE(SUM(need_validate = 1), 0) AS need_validate_yes,
    COALESCE(SUM(assimilate_state = 0), 0) AS assimilate_init,
    COALESCE(SUM(assimilate_state = 1), 0) AS assimilate_ready,
    COALESCE(SUM(assimilate_state = 2), 0) AS assimilate_done,
    COALESCE(SUM(assimilate_state NOT IN (0, 1, 2)), 0) AS assimilate_other,
    COALESCE(SUM(file_delete_state = 0), 0) AS file_delete_init,
    COALESCE(SUM(file_delete_state = 1), 0) AS file_delete_ready,
    COALESCE(SUM(file_delete_state = 2), 0) AS file_delete_done,
    COALESCE(SUM(file_delete_state = 3), 0) AS file_delete_error,
    COALESCE(SUM(file_delete_state NOT IN (0, 1, 2, 3)), 0) AS file_delete_other
  FROM workunit
) AS w
CROSS JOIN (
  SELECT COUNT(*) AS total,
    COALESCE(SUM(server_state = 1), 0) AS inactive,
    COALESCE(SUM(server_state = 2), 0) AS unsent,
    COALESCE(SUM(server_state = 4), 0) AS in_progress,
    COALESCE(SUM(server_state = 5), 0) AS state_over,
    COALESCE(SUM(server_state NOT IN (1, 2, 4, 5)), 0) AS other
  FROM result
) AS r
CROSS JOIN (
  SELECT COUNT(*) AS active_users FROM `user` WHERE expavg_credit > 1
) AS u
CROSS JOIN (
  SELECT COUNT(*) AS active_hosts FROM host WHERE expavg_credit > 1
) AS h
"""

FIELDS = (
    "total",
    "need_validate_no",
    "need_validate_yes",
    "assimilate_init",
    "assimilate_ready",
    "assimilate_done",
    "assimilate_other",
    "file_delete_init",
    "file_delete_ready",
    "file_delete_done",
    "file_delete_error",
    "file_delete_other",
    "results_total",
    "results_inactive",
    "results_unsent",
    "results_in_progress",
    "results_over",
    "results_other",
    "active_users",
    "active_hosts",
)


def arguments(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--mysql-defaults-file", required=True)
    parser.add_argument("--database", required=True)
    return parser.parse_args(argv)


def payload(values):
    (wu_total, wu_validate_no, wu_validate_yes, assim_init, assim_ready, assim_done, assim_other,
     delete_init, delete_ready, delete_done, delete_error, delete_other,
     result_total, result_inactive, result_unsent, result_progress, result_over,
     result_other, active_users, active_hosts) = values
    return {
        "event": "boinc_work_volume",
        "schema_version": 1,
        "workunits": {
            "total": wu_total,
            "need_validate": {"no": wu_validate_no, "yes": wu_validate_yes},
            "assimilate_state": {
                "init": assim_init,
                "ready": assim_ready,
                "done": assim_done,
                "other": assim_other,
            },
            "file_delete_state": {
                "init": delete_init,
                "ready": delete_ready,
                "done": delete_done,
                "error": delete_error,
                "other": delete_other,
            },
        },
        "results": {
            "total": result_total,
            "server_state": {
                "inactive": result_inactive,
                "unsent": result_unsent,
                "in_progress": result_progress,
                "over": result_over,
                "other": result_other,
            },
        },
        "active_users": active_users,
        "active_hosts": active_hosts,
    }


def main(argv=None):
    args = arguments(argv)
    if not re.fullmatch(r"[A-Za-z0-9_]+", args.database):
        print("usnm-boinc: invalid work-volume database name", file=sys.stderr)
        return 2
    try:
        result = subprocess.run(
            [
                "mysql",
                f"--defaults-extra-file={args.mysql_defaults_file}",
                "--ssl",
                "--ssl-verify-server-cert",
                "--ssl-ca=/usr/local/share/usnm/mysql-ca.pem",
                "--batch",
                "--raw",
                "--skip-column-names",
                f"--database={args.database}",
                "--execute",
                QUERY,
            ],
            check=True,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
        )
        fields = result.stdout.strip().split("\t")
        if len(fields) != len(FIELDS) or any(not field.isdecimal() for field in fields):
            raise ValueError("invalid aggregate result")
        values = [int(field) for field in fields]
    except (OSError, subprocess.CalledProcessError, ValueError):
        print("usnm-boinc: work-volume aggregate query failed", file=sys.stderr)
        return 1
    print(json.dumps(payload(values), separators=(",", ":"), sort_keys=True))
    return 0


if __name__ == "__main__":
    sys.exit(main())
