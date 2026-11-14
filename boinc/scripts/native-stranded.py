#!/usr/bin/env python3
"""Settle `usnm_ocr_native` workunits that can no longer produce a result, and keep each
page on a durable list for its retry (#1919).

A native unit is assigned to one host, so BOINC's transitioner never makes a replacement
result for it (`TRANSITION_NO_NEW_RESULTS`). A unit whose only result failed, or whose host
never came back for it, stays open with no error mask for ever: the page has no result, the
controller's `native_targets` keeps its slot, and the host's room shrinks by one for each.

A unit is stranded when it is `usnm_ocr_native`, has the flag, no error mask, no canonical
result, nothing in progress, unsent or successful, and is one of:

- `too_slow_sigxcpu`: its results all failed, one with `SIGXCPU` in its stderr or exit status
  117 (the engine's CPU limit);
- `exit0_loop`: one failed with exit status -226 ("too many exit(0)s", #1928);
- `client_error`: any other failed result;
- `assigned_no_result`: no result at all, `--stale-hours` after it was created.

`plan` (read-only) and `settle` (a preview without `--apply`) print the count by reason. Both
write nothing but `--out`, the durable list. Each entry holds the page, the workunit, the
parent job sha, its image and `engine.timeout_secs` (from the job's journal), the assigned
host, the reason and the result's exit status. `settle --apply` first writes the list, then
cancels each unit with `bin/cancel_jobs --name`, as BOINC does for any cancelled job: the
transitioner marks it `WU_ERROR_CANCELLED` (0x10), the assimilator writes the terminal marker
`collect` reads, the journal fails and the plan's next tick drops the slot. A cancelled unit is
the controller's failed page, which the retry route reads (`--expected-error-mask 16`). Nothing
is deleted, and no error mask is set by hand. `count` reads the list file only.

Run `plan` and `settle` inside the BOINC server container, as root:

    ssh root@<boinc host> 'pct exec 230 -- podman exec -i usnm-boinc-server python3 - plan' \\
        < boinc/scripts/native-stranded.py

`settle --apply` needs `--max-units`, `--confirm-count` (the plan's total) and `--journal`
(an append-only JSON-lines record of every cancel). Do one unit first (`--max-units 1`), and
check that its marker reaches the spool and the plan's journal fails before the rest.
"""

import argparse
import datetime as dt
import json
import os
import subprocess
import sys
import tempfile
import xml.etree.ElementTree as ET

# BOINC result states and outcomes (`common_defs.h`).
INACTIVE, UNSENT, IN_PROGRESS = 1, 2, 4
SUCCESS = 1
TRANSITION_NO_NEW_RESULTS = 2
EXIT_CPU_LIMIT = 117
EXIT_TOO_MANY_EXIT0 = -226
REASONS = ("too_slow_sigxcpu", "exit0_loop", "client_error", "assigned_no_result")
DEFAULT_OUT = "/project/maintenance/native-stranded.json"
DEFAULT_HANDOFF = "/project/handoff"
DEFAULT_PROJECT = "/project/usnmocr"
JOB_ID_LEN = 64

CANDIDATES = """
SELECT w.id, w.name, w.create_time, a.target_id,
       (SELECT COUNT(*) FROM result r WHERE r.workunitid = w.id) AS results,
       (SELECT r.exit_status FROM result r WHERE r.workunitid = w.id ORDER BY r.id DESC LIMIT 1),
       (SELECT COUNT(*) FROM result r WHERE r.workunitid = w.id AND r.stderr_out LIKE '%%SIGXCPU%%')
FROM workunit w
JOIN app p ON p.id = w.appid AND p.name = 'usnm_ocr_native'
LEFT JOIN assignment a ON a.workunitid = w.id
WHERE w.transitioner_flags = %(flag)s AND w.error_mask = 0 AND w.canonical_resultid = 0
  AND NOT EXISTS (SELECT 1 FROM result x WHERE x.workunitid = w.id
                  AND (x.server_state IN (%(inactive)s, %(unsent)s, %(in_progress)s) OR x.outcome = %(success)s))
ORDER BY w.id
"""


def classify(results, exit_status, sigxcpu, age_secs, stale_secs):
    """The reason a unit is stranded, or None when it isn't (yet)."""
    if results == 0:
        return "assigned_no_result" if age_secs >= stale_secs else None
    if sigxcpu or exit_status == EXIT_CPU_LIMIT:
        return "too_slow_sigxcpu"
    if exit_status == EXIT_TOO_MANY_EXIT0:
        return "exit0_loop"
    return "client_error"


def job_of(name):
    """The job sha a workunit name carries (`usnm_<64 hex>`), or None."""
    job = name[len("usnm_"):] if name.startswith("usnm_") else ""
    ok = len(job) == JOB_ID_LEN and all(c in "0123456789abcdef" for c in job)
    return job if ok else None


def read_journal(handoff, job):
    """What the job's journal says of the page, or Nones when it can't be read."""
    try:
        with open(f"{handoff}/jobs/{job}/record.json", encoding="utf-8") as f:
            record = json.load(f)
        engine = record["job"]["engine"]
        return {"page": record["job"]["message"]["page"], "plan": record.get("plan"),
                "stage": record.get("stage"), "attempt": record["job"].get("attempt", 0),
                "image": engine["image"], "timeout_secs": engine["timeout_secs"]}
    except (OSError, ValueError, KeyError, TypeError):
        return {"page": None, "plan": None, "stage": None, "attempt": None, "image": None,
                "timeout_secs": None}


def find(cur, handoff, now, stale_secs):
    """Every stranded unit, as the list file keeps it, oldest first."""
    cur.execute(CANDIDATES, {"flag": TRANSITION_NO_NEW_RESULTS, "inactive": INACTIVE,
                             "unsent": UNSENT, "in_progress": IN_PROGRESS, "success": SUCCESS})
    found = []
    for wid, name, created, host, results, exit_status, sigxcpu in cur.fetchall():
        reason = classify(int(results), None if exit_status is None else int(exit_status),
                          int(sigxcpu or 0) > 0, now - int(created), stale_secs)
        job = job_of(name)
        if reason is None or job is None:
            continue
        found.append({"workunit": name, "workunit_id": int(wid), "job": job, "host": host,
                      "reason": reason, "exit_status": None if exit_status is None else int(exit_status),
                      "created_at": dt.datetime.fromtimestamp(int(created), dt.timezone.utc).isoformat(),
                      **read_journal(handoff, job)})
    return found


def load_list(path):
    try:
        with open(path, encoding="utf-8") as f:
            data = json.load(f)
    except FileNotFoundError:
        return {}
    except ValueError as e:
        sys.exit(f"{path} is not valid JSON ({e}); not overwriting it")
    if not isinstance(data, dict) or data.get("schema") != 1 or not isinstance(data.get("pages"), dict):
        sys.exit(f"{path} is not a native-stranded list (schema 1); not overwriting it")
    return data["pages"]


def write_list(path, pages):
    """The whole list, keys sorted, replaced atomically."""
    os.makedirs(os.path.dirname(path) or ".", exist_ok=True)
    fd, tmp = tempfile.mkstemp(dir=os.path.dirname(path) or ".", prefix=".native-stranded-")
    with os.fdopen(fd, "w", encoding="utf-8") as f:
        json.dump({"schema": 1, "pages": dict(sorted(pages.items()))}, f, indent=2, sort_keys=True)
        f.write("\n")
    os.replace(tmp, path)


def merge(pages, found, now_iso):
    """Add what was found to the durable list. An entry already there keeps its first-seen time
    and any settlement; nothing is ever removed."""
    for entry in found:
        old = pages.get(entry["workunit"], {})
        pages[entry["workunit"]] = {**entry, "first_seen": old.get("first_seen", now_iso),
                                    **({"settled_at": old["settled_at"]} if "settled_at" in old else {})}
    return pages


def counts(pages):
    by_reason = {reason: 0 for reason in REASONS}
    settled = 0
    for entry in pages.values():
        by_reason[entry["reason"]] = by_reason.get(entry["reason"], 0) + 1
        settled += "settled_at" in entry
    return {"total": len(pages), "settled": settled, "unsettled": len(pages) - settled,
            "by_reason": by_reason}


def cancel(project, name):
    result = subprocess.run(["bin/cancel_jobs", "--name", name], cwd=project, capture_output=True,
                            text=True, check=False)
    return result.returncode == 0, (result.stdout + result.stderr).strip()[-500:]


def connect(args):
    import MySQLdb  # only in the server image

    cfg = ET.parse(args.config).find("config")
    db = MySQLdb.connect(host=cfg.findtext("db_host"), user=cfg.findtext("db_user"),
                         passwd=cfg.findtext("db_passwd"), db=cfg.findtext("db_name"),
                         ssl={"ca": args.db_ca}, read_default_file=args.db_defaults,
                         read_default_group="client")
    return db


def now_of(cur):
    cur.execute("SELECT UNIX_TIMESTAMP()")
    return int(cur.fetchone()[0])


def run(args, out=sys.stdout, connect_db=connect, canceller=cancel):
    if args.command == "count":
        report = counts(load_list(args.out))
        print(json.dumps(report, sort_keys=True), file=out)
        return 0
    db = connect_db(args)
    cur = db.cursor()
    cur.execute("SET SESSION TRANSACTION READ ONLY")
    now = now_of(cur)
    found = find(cur, args.handoff, now, int(args.stale_hours * 3600))
    pages = load_list(args.out)
    now_iso = dt.datetime.fromtimestamp(now, dt.timezone.utc).isoformat()
    merge(pages, found, now_iso)
    report = {"candidates": counts({e["workunit"]: e for e in found}), "list": args.out}
    if args.command == "plan" or not args.apply:
        # A preview and a plan write the list too: it is the record of what was found.
        write_list(args.out, pages)
        print(json.dumps(report, sort_keys=True), file=out)
        return 0
    if args.max_units is None or args.confirm_count is None or not args.journal:
        sys.exit("--apply needs --max-units, --confirm-count and --journal")
    total = report["candidates"]["total"]
    if args.confirm_count != total:
        sys.exit(f"--confirm-count {args.confirm_count} is not the {total} the plan found")
    write_list(args.out, pages)  # first: a page is on the list before anything about its unit changes
    done = 0
    for entry in found[: args.max_units]:
        ok, detail = canceller(args.project, entry["workunit"])
        with open(args.journal, "a", encoding="utf-8") as j:
            j.write(json.dumps({"at": now_iso, "workunit": entry["workunit"], "job": entry["job"],
                                "reason": entry["reason"], "cancelled": ok, "detail": detail},
                               sort_keys=True) + "\n")
        if not ok:
            print(f"cancel failed for {entry['workunit']}: {detail}", file=sys.stderr)
            break
        pages[entry["workunit"]]["settled_at"] = now_iso
        write_list(args.out, pages)
        done += 1
    report["cancelled"] = done
    print(json.dumps(report, sort_keys=True), file=out)
    return 0 if done == min(args.max_units, total) else 1


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    sub = parser.add_subparsers(dest="command", required=True)
    for name, text in (("plan", "count the stranded units and write the list; changes no unit"),
                       ("settle", "cancel the stranded units; a preview without --apply"),
                       ("count", "count the durable list file only")):
        p = sub.add_parser(name, help=text)
        p.add_argument("--out", default=DEFAULT_OUT, help=f"the durable list (default {DEFAULT_OUT})")
        if name == "count":
            continue
        p.add_argument("--stale-hours", type=float, default=6.0,
                       help="age at which a unit with no result is stranded (default 6)")
        p.add_argument("--handoff", default=DEFAULT_HANDOFF, help="the bridge's journals")
        p.add_argument("--project", default=DEFAULT_PROJECT)
        p.add_argument("--config", default=f"{DEFAULT_PROJECT}/config.xml")
        p.add_argument("--db-ca", default="/usr/local/share/usnm/mysql-ca.pem")
        p.add_argument("--db-defaults", default="/usr/local/share/usnm/mysql-tls.cnf")
        if name == "settle":
            p.add_argument("--apply", action="store_true", help="cancel the units; without it only preview")
            p.add_argument("--max-units", type=int, help="most units to cancel in this run")
            p.add_argument("--confirm-count", type=int, help="the plan's total")
            p.add_argument("--journal", help="append-only JSON-lines record of every cancel")
    args = parser.parse_args(argv)
    if args.command != "count" and args.stale_hours < 0:
        parser.error("--stale-hours is not negative")
    return run(args)


if __name__ == "__main__":
    sys.exit(main())
