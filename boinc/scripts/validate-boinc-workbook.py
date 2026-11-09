#!/usr/bin/env python3
"""Execute the nine source workbook panels; emit aggregate-only acceptance receipts."""

import argparse
import datetime
import hashlib
import json
import pathlib
import sys

# The Log Analytics client lives with the other Azure scripts.
sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[2] / "scripts/azure"))
import log_analytics  # noqa: E402


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--workspace", required=True)
    parser.add_argument("--identity", choices=["az", "arc"], default="az")
    parser.add_argument("--subscription")
    parser.add_argument("--host")
    parser.add_argument("--hosts")
    parser.add_argument("--location")
    parser.add_argument("--workbook", type=pathlib.Path,
                        default=pathlib.Path(__file__).resolve().parents[2]
                        / "infra/workbooks/boinc.json")
    parser.add_argument("--timespan", default="P7D")
    args = parser.parse_args()
    panels = [item for item in json.loads(args.workbook.read_text())["items"]
              if item["type"] == 3]
    if len(panels) != 9:
        raise log_analytics.QueryError("Expected exactly nine workbook query panels")
    token = log_analytics.access_token(args.identity, args.subscription)
    hosts = log_analytics.query_hosts(args.workspace, host=args.host, hosts=args.hosts, location=args.location,
                                    identity=args.identity, subscription=args.subscription)
    receipt = {"identityClass": args.identity, "workspace": args.workspace,
               "endpoints": [f"https://{host}" for host in hosts], "audience": log_analytics.AUDIENCE,
               "timespan": args.timespan, "panels": []}
    for panel in [{"name": "positive-control", "content": {"query": "print QueryRouteControl=1"}},
                  *panels]:
        kql = panel["content"]["query"].replace("{TimeRange:grain}", "1h")
        row = {"name": panel["name"], "querySha256": hashlib.sha256(kql.encode()).hexdigest(),
               "startedUtc": datetime.datetime.now(datetime.timezone.utc).isoformat()}
        try:
            body, history = log_analytics.query(args.workspace, kql, args.timespan,
                                                token=token, hosts=hosts)
            if (panel["name"] == "positive-control"
                    and body["tables"][0].get("rows") != [[1]]):
                raise log_analytics.QueryError("Positive control did not return [[1]]", history)
            tables = []
            for table in body["tables"]:
                names = [column["name"] for column in table["columns"]]
                # Never retain raw messages, principal IDs, or arbitrary string cells.
                safe_names = {"Status", "Reason", "Level", "Name", "TimeGenerated"}
                tables.append({"columns": table["columns"], "rowCount": len(table["rows"]),
                               "samples": [{key: value for key, value in zip(names, values)
                                            if value is None or isinstance(value, (int, float, bool))
                                            or key in safe_names}
                                           for values in table["rows"][:20]]})
            row.update(status="success", attempts=history, tables=tables)
        except log_analytics.QueryError as error:
            row.update(status="failed", error=str(error), attempts=error.history)
        row["endedUtc"] = datetime.datetime.now(datetime.timezone.utc).isoformat()
        receipt["panels"].append(row)
        counts = [table["rowCount"] for table in row.get("tables", [])]
        print(f"Workbook {row['name']}: {row['status']}; row counts={counts}", file=sys.stderr)
    print(json.dumps(receipt, indent=2))
    return int(any(panel["status"] != "success" for panel in receipt["panels"]))


if __name__ == "__main__":
    try:
        sys.exit(main())
    except (log_analytics.QueryError, OSError, ValueError, KeyError):
        print("Workbook validation setup failed; check identity and workbook input", file=sys.stderr)
        sys.exit(1)
