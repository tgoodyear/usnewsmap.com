#!/usr/bin/env python3
"""What `usnm_qwen` replicas cost: replicas per unit, how units end, and GPU
time on replicas that resolved nothing (#1420). Read-only.

`collect` runs as root inside the BOINC server container (the campaign copies
of the job inputs are root-only). It reads the workunits created in a window
and their results in one READ ONLY transaction of bounded SELECTs, joins each
unit's language and request version from its `qwen.json`, and prints one JSON
snapshot. It writes nothing. `report` reads that snapshot and prints Markdown
tables; it needs no database.

    ssh root@<boinc host> 'pct exec 230 -- podman exec -i usnm-boinc-server python3 - collect \
        --since 2026-10-21T00:00:00Z --until 2026-11-04T00:00:00Z' \
        < scripts/boinc/qwen-replica-cost.py > snapshot.json
    $ scripts/boinc/qwen-replica-cost.py report --snapshot snapshot.json
"""

import argparse
import collections
import datetime as dt
import glob
import hashlib
import json
import sys
import xml.etree.ElementTree as ET

# BOINC result states (`common_defs.h`).
UNSENT, IN_PROGRESS = 2, 4
SUCCESS = 1
ERROR_OUTCOMES = {3, 4, 6, 7}  # client error, no reply, validate error, detached
VALID, INCONCLUSIVE = 1, 4
# Workunit error_mask bits.
TOO_MANY_ERRORS, TOO_MANY_SUCCESS, TOO_MANY_TOTAL, CANCELLED = 2, 4, 8, 16


def utc(text):
    when = dt.datetime.fromisoformat(text.replace("Z", "+00:00"))
    if when.tzinfo is None:
        raise argparse.ArgumentTypeError("give a UTC time, e.g. 2026-10-21T00:00:00Z")
    return when.astimezone(dt.timezone.utc)


def job_facts(job):
    pages = job.get("pages") or []
    langs = sorted({p.get("language") for p in pages}) or [job.get("language")]
    return {"language": "+".join(str(x) for x in langs), "prompt_version": job.get("prompt_version"), "pages": len(pages) or 1}


def download_path(root, name, fanout):
    """BOINC's `dir_hier_path` for a file in the download tree."""
    return f"{root}/{int(hashlib.md5(name.encode()).hexdigest()[1:8], 16) % fanout:x}/{name}"


def read_job(path):
    try:
        with open(path, encoding="utf-8") as f:
            return json.load(f)
    except (OSError, ValueError):
        return None


def collect(args):
    import MySQLdb  # only in the server image

    cfg = ET.parse(args.config).find("config")
    db = MySQLdb.connect(
        host=cfg.findtext("db_host"),
        user=cfg.findtext("db_user"),
        passwd=cfg.findtext("db_passwd"),
        db=cfg.findtext("db_name"),
        ssl={"ca": args.db_ca},
        read_default_file=args.db_defaults,
        read_default_group="client",
    )
    try:
        cur = db.cursor()
        cur.execute("SET SESSION MAX_EXECUTION_TIME=60000")
        cur.execute("START TRANSACTION READ ONLY")
        cur.execute("SELECT UTC_TIMESTAMP()")
        snapshot = cur.fetchone()[0].strftime("%Y-%m-%dT%H:%M:%SZ")
        cur.execute("SELECT id FROM app WHERE name='usnm_qwen'")
        appid = cur.fetchone()[0]
        cur.execute(
            "SELECT id,name,create_time,canonical_resultid,error_mask,target_nresults,max_total_results "
            "FROM workunit WHERE appid=%s AND create_time>=%s AND create_time<%s ORDER BY id LIMIT %s",
            (appid, int(args.since.timestamp()), int(args.until.timestamp()), args.limit),
        )
        cols = [c[0] for c in cur.description]
        units = [dict(zip(cols, row)) for row in cur.fetchall()]
        if len(units) >= args.limit:
            sys.exit(f"--limit {args.limit} reached; narrow the window")
        results = []
        if units:
            cur.execute(
                "SELECT r.id,r.workunitid,r.server_state,r.outcome,r.validate_state,r.hostid,r.sent_time,"
                "r.received_time,r.elapsed_time,av.plan_class FROM result r "
                "LEFT JOIN app_version av ON av.id=r.app_version_id "
                "WHERE r.appid=%s AND r.workunitid BETWEEN %s AND %s LIMIT %s",
                (appid, units[0]["id"], units[-1]["id"], 8 * args.limit),
            )
            cols = [c[0] for c in cur.description]
            rows = cur.fetchall()
            if len(rows) >= 8 * args.limit:
                sys.exit(f"{len(rows)} results reached the result limit; narrow the window")
            ids = {u["id"] for u in units}
            results = [r for r in (dict(zip(cols, row)) for row in rows) if r["workunitid"] in ids]
        db.rollback()
    finally:
        db.close()
    jobs = {}
    for path in glob.glob(f"{args.qwen_root}/*/units/*/qwen.json"):
        job = read_job(path)
        if isinstance(job, dict) and "id" in job:
            jobs[job["id"]] = job_facts(job)
    fanout = int(cfg.findtext("uldl_dir_fanout") or 1024)
    for u in units:
        job_id = u.pop("name").removeprefix("usnm_qwen_")
        if job_id not in jobs:
            job = read_job(download_path(args.download, f"{job_id}_qwen_json", fanout))
            jobs[job_id] = job_facts(job) if isinstance(job, dict) else {"language": None, "prompt_version": None, "pages": None}
        u.update(jobs[job_id])
    json.dump(
        {"snapshot": snapshot, "since": args.since.strftime("%Y-%m-%dT%H:%M:%SZ"), "until": args.until.strftime("%Y-%m-%dT%H:%M:%SZ"), "units": units, "results": results},
        sys.stdout,
        default=float,
    )
    print()


def in_order(results):
    """A unit's successful returns in the order the server received them."""
    return sorted((r for r in results if r["outcome"] == SUCCESS), key=lambda r: (r["received_time"], r["id"]))


def classify(unit, results):
    """A unit's state and, when it ended valid, which return resolved it:
    2 for the first pair, 3 or 4 for a later replica. The resolver is unknown
    (None) unless exactly two returns are valid: a late return that agreed
    after the first pair, and a third that agreed with both of a disputed
    pair, leave the same record."""
    returns = in_order(results)
    mask = unit["error_mask"]
    if unit["canonical_resultid"]:
        valid = [i for i, r in enumerate(returns, 1) if r["validate_state"] == VALID]
        return "valid", (valid[-1] if len(valid) == 2 else None)
    if mask & (TOO_MANY_SUCCESS | TOO_MANY_TOTAL) and not mask & (TOO_MANY_ERRORS | CANCELLED):
        return "no consensus at cap", None
    if mask:
        return "other failure", None
    if any(r["validate_state"] == INCONCLUSIVE for r in returns):
        return "open: disputed", None
    return "open: not disputed", None


def analyse(snapshot):
    """Counters per `<language> v<request version>` and for `all`."""
    by_unit = collections.defaultdict(list)
    for r in snapshot["results"]:
        by_unit[r["workunitid"]].append(r)
    rows = collections.defaultdict(collections.Counter)
    for u in snapshot["units"]:
        rs = by_unit[u["id"]]
        state, rank = classify(u, rs)
        returns = in_order(rs)
        c = collections.Counter(units=1, created=len(rs))
        c["sent"] = sum(bool(r["sent_time"]) for r in rs)
        c["unsent"] = sum(r["server_state"] == UNSENT for r in rs)
        c["in progress"] = sum(r["server_state"] == IN_PROGRESS for r in rs)
        c[state] = 1
        ran = [r for r in rs if r["outcome"] == SUCCESS or r["outcome"] in ERROR_OUTCOMES]
        hours = sum(r["elapsed_time"] or 0 for r in ran) / 3600
        c["hours"] = hours
        if state == "valid":
            disputed = rank is not None and rank > 2
        else:
            disputed = state != "open: not disputed" and len(returns) >= 2
        c["disputed"] = int(disputed)
        if disputed:
            for n in (3, 4):
                if len(returns) >= n and (rank is None or rank >= n):
                    c[f"replica {n} returned"] = 1
        if state == "valid":
            c[f"resolved by {rank}" if rank else "resolver unknown"] = 1
            c["sent on valid"] = c["sent"]
            pair = [r for r in returns if r["validate_state"] == VALID][:2]
            useful = sum(r["elapsed_time"] or 0 for r in pair) / 3600
            c["hours needed"] = useful
            c["hours wasted"] = hours - useful
            c["replicas wasted"] = len(ran) - 2
        elif state == "no consensus at cap":
            c["hours wasted"] = hours
            c["replicas wasted"] = len(ran)
            c["disputed at cap"] = int(disputed)
        elif state == "other failure":
            c["hours failed"] = hours
            c["disputed failed"] = int(disputed)
        elif state == "open: disputed":
            c["hours disputed open"] = hours
            c[f"waiting for replica {len(returns) + 1}"] = 1
        else:
            c["hours pending"] = hours
        for key in ("all", f"{u['language']} v{u['prompt_version']}"):
            rows[key].update(c)
    return rows


def table(header, lines):
    out = ["| " + " | ".join(header) + " |", "|" + "|".join("---" if i == 0 else "--:" for i in range(len(header))) + "|"]
    out += ["| " + " | ".join(str(x) for x in line) + " |" for line in lines]
    return "\n".join(out)


def ratio(a, b):
    return f"{a / b:.2f}" if b else "–"


def report(args):
    with open(args.snapshot, encoding="utf-8") as f:
        snapshot = json.load(f)
    rows = analyse(snapshot)
    order = ["all"] + sorted((k for k in rows if k != "all"), key=lambda k: (-rows[k]["units"], k))
    order = [k for k in order if k == "all" or rows[k]["units"] >= args.min_units]
    rest = [k for k in rows if k != "all" and k not in order]
    if rest:
        rows["other"] = sum((rows[k] for k in rest), collections.Counter())
        order.append("other")
    print(f"Units created {snapshot['since']} to {snapshot['until']}; database read {snapshot['snapshot']}.\n")
    print(table(
        ("Language, request version", "Units", "Created", "Sent", "Unsent now", "Valid", "No consensus at cap", "Other failure", "Open: disputed", "Open: not disputed"),
        [(k, r["units"], ratio(r["created"], r["units"]), ratio(r["sent"], r["units"]), r["unsent"], r["valid"],
          r["no consensus at cap"], r["other failure"], r["open: disputed"], r["open: not disputed"]) for k, r in ((k, rows[k]) for k in order)],
    ))
    print("\nCreated, Sent: replicas per unit. Other failure: cancelled, or too many errors.\n")
    print(table(
        ("Language, request version", "Valid", "On first pair", "By replica 3", "By replica 4", "Sent per valid unit", "Ran per valid unit, settled"),
        [(k, r["valid"], r["resolved by 2"], r["resolved by 3"], r["resolved by 4"], ratio(r["sent on valid"], r["valid"]),
          ratio(r["replicas wasted"] + 2 * r["valid"], r["valid"])) for k, r in ((k, rows[k]) for k in order)],
    ))
    print("\nRan per valid unit, settled: returns and errors on valid and capped units, per valid unit; a unit needs 2.")
    if rows["all"]["resolver unknown"]:
        print(f"{rows['all']['resolver unknown']} valid units don't have exactly two valid returns, so which one resolved them is unknown; "
              "the resolution columns and the disputed counts leave them out.")
    print()
    print(table(
        ("Language, request version", "First pair disagreed", "Replica 3 returned", "It resolved", "Replica 4 returned", "It resolved", "No consensus at cap", "Other failure", "Waiting for 3", "Waiting for 4"),
        [(k, r["disputed"], r["replica 3 returned"], r["resolved by 3"], r["replica 4 returned"], r["resolved by 4"], r["disputed at cap"],
          r["disputed failed"], r["waiting for replica 3"], r["waiting for replica 4"])
         for k, r in ((k, rows[k]) for k in order)],
    ))
    print()
    print(table(
        ("Language, request version", "GPU-hours", "On the valid pair", "Resolved nothing", "Share", "Other failure", "Disputed, still open", "First return, waiting"),
        [(k, f"{r['hours']:.1f}", f"{r['hours needed']:.1f}", f"{r['hours wasted']:.1f}", f"{r['hours wasted'] / r['hours']:.0%}" if r["hours"] else "–",
          f"{r['hours failed']:.1f}", f"{r['hours disputed open']:.1f}", f"{r['hours pending']:.1f}") for k, r in ((k, rows[k]) for k in order)],
    ))
    print("\nResolved nothing: the losing replicas and errors on units that ended valid, and every replica on units that hit the cap.")


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    sub = parser.add_subparsers(dest="command", required=True)
    c = sub.add_parser("collect", help="read the BOINC database and job inputs (server container, root)")
    c.add_argument("--since", type=utc, required=True, help="first workunit create time, UTC")
    c.add_argument("--until", type=utc, required=True, help="workunits created before this, UTC")
    c.add_argument("--config", default="/project/usnmocr/config.xml")
    c.add_argument("--qwen-root", default="/project/qwen", help="campaign directories holding units/*/qwen.json")
    c.add_argument("--download", default="/project/usnmocr/download", help="download tree, for inputs with no campaign copy")
    c.add_argument("--db-ca", default="/usr/local/share/usnm/mysql-ca.pem")
    c.add_argument("--db-defaults", default="/usr/local/share/usnm/mysql-tls.cnf")
    c.add_argument("--limit", type=int, default=20000, help="most workunits to read")
    r = sub.add_parser("report", help="print Markdown tables from a snapshot")
    r.add_argument("--snapshot", required=True)
    r.add_argument("--min-units", type=int, default=20, help="fold smaller language/version groups into 'other'")
    args = parser.parse_args(argv)
    if args.command == "collect":
        if args.until <= args.since:
            parser.error("--until must be after --since")
        collect(args)
    else:
        report(args)


if __name__ == "__main__":
    main()
