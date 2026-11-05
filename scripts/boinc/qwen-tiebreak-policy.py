#!/usr/bin/env python3
"""Stop `usnm_qwen` tie-breaker replicas for chosen language and request
version groups, and settle their disputed units with no consensus (#1477).

The policy names `<language>:<request version>` groups, for example `yid:1`.
It comes from `--stop-tiebreakers` (repeatable) or a JSON policy file,
`--policy-file`, which defaults to `/project/usnmocr/qwen/tiebreak-policy.json`:

    {"schema": 1, "stop_tiebreakers": [{"language": "yid", "request_version": 1}]}

With no flag and no file the policy is empty, nothing is selected and nothing
changes: tie-breakers run as before, up to `max_total_results`. A language is
the unit's language set as `qwen-replica-cost.py` labels it (`yid`, or
`deu+srp` for a bundle), matched exactly.

A unit is settled when it has no canonical result and no error, at least two
successful returns the validator found inconclusive (the first pair
disagreed), no replica running, nothing waiting for the validator, and isn't
due for the transitioner. Settling, in one transaction per unit under row
locks: its unsent replicas become OVER/DIDNT_NEED (as BOINC's `cancel_job`
does), and the unit gets `max_total_results` = its result count,
`error_mask |= 8` (too many total results) and `transition_time` = now. The
transitioner then marks the inconclusive returns NO_CHECK and hands the unit to
the assimilator with no canonical result, as it does for a unit that reaches
its cap. No Qwen reading joins the page, which keeps its Tesseract readings.

`plan` is read-only: one READ ONLY transaction of bounded SELECTs over the
workunits created in a window. `settle` previews the same selection and
changes nothing without `--apply`; with it, it needs `--max-units` and an
append-only `--journal`. Both run as root inside the BOINC server container:

    ssh root@<boinc host> 'pct exec 230 -- podman exec -i usnm-boinc-server python3 - plan \
        --since 2026-10-21T00:00:00Z --until 2026-11-06T00:00:00Z --stop-tiebreakers yid:1' \
        < scripts/boinc/qwen-tiebreak-policy.py
"""

import argparse
import collections
import datetime as dt
import glob
import hashlib
import json
import os
import re
import sys
import xml.etree.ElementTree as ET

# BOINC result states (`common_defs.h`).
INACTIVE, UNSENT, IN_PROGRESS, OVER = 1, 2, 4, 5
SUCCESS, DIDNT_NEED = 1, 5
VALIDATE_INIT, INCONCLUSIVE = 0, 4
TOO_MANY_TOTAL = 8
# MySQL errors after which a unit is left to a later pass.
LOCK_WAIT, DEADLOCK = 1205, 1213
# Units the transitioner is due to handle within this many seconds are left to a later pass.
DUE_MARGIN = 300
DEFAULT_POLICY = "/project/usnmocr/qwen/tiebreak-policy.json"
LANGUAGE = re.compile(r"[a-z]{3}(\+[a-z]{3})*")


def utc(text):
    when = dt.datetime.fromisoformat(text.replace("Z", "+00:00"))
    if when.tzinfo is None:
        raise argparse.ArgumentTypeError("give a UTC time, e.g. 2026-10-21T00:00:00Z")
    return when.astimezone(dt.timezone.utc)


def group_arg(text):
    language, _, version = text.partition(":")
    if not LANGUAGE.fullmatch(language) or not version.isdigit() or int(version) < 1:
        raise argparse.ArgumentTypeError("give LANGUAGE:REQUEST_VERSION, e.g. yid:1")
    return language, int(version)


def read_policy(path):
    with open(path, encoding="utf-8") as f:
        policy = json.load(f)
    if not isinstance(policy, dict) or set(policy) != {"schema", "stop_tiebreakers"} or policy["schema"] != 1:
        raise ValueError(f"{path}: want {{\"schema\": 1, \"stop_tiebreakers\": [...]}}")
    groups = []
    for entry in policy["stop_tiebreakers"]:
        if (not isinstance(entry, dict) or set(entry) != {"language", "request_version"}
                or not isinstance(entry["language"], str) or not LANGUAGE.fullmatch(entry["language"])
                or type(entry["request_version"]) is not int or entry["request_version"] < 1):
            raise ValueError(f"{path}: bad entry {entry!r}")
        groups.append((entry["language"], entry["request_version"]))
    return groups


def resolve_policy(args):
    """The groups to stop, sorted; empty means current behaviour."""
    if args.stop_tiebreakers and args.policy_file:
        sys.exit("give --stop-tiebreakers or --policy-file, not both")
    if args.stop_tiebreakers:
        return sorted(set(args.stop_tiebreakers))
    path = args.policy_file or DEFAULT_POLICY
    if not args.policy_file and not os.path.exists(path):
        return []
    return sorted(set(read_policy(path)))


def job_facts(job):
    pages = job.get("pages") or []
    langs = sorted({p["language"] for p in pages if p.get("language")}) or ([job["language"]] if job.get("language") else [])
    return {"language": "+".join(langs) or None, "prompt_version": job.get("prompt_version")}


def download_path(root, name, fanout):
    """BOINC's `dir_hier_path` for a file in the download tree."""
    return f"{root}/{int(hashlib.md5(name.encode()).hexdigest()[1:8], 16) % fanout:x}/{name}"


def read_job(path):
    try:
        with open(path, encoding="utf-8") as f:
            return json.load(f)
    except (OSError, ValueError):
        return None


def connect(args):
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
    return db, int(cfg.findtext("uldl_dir_fanout") or 1024)


def read_window(db, args, fanout):
    """Units created in the window, their results and the database clock, in
    one READ ONLY transaction; each unit joined to its language and version."""
    cur = db.cursor()
    try:
        cur.execute("SET SESSION MAX_EXECUTION_TIME=60000")
        cur.execute("START TRANSACTION READ ONLY")
        cur.execute("SELECT UNIX_TIMESTAMP()")
        now = int(cur.fetchone()[0])
        cur.execute("SELECT id FROM app WHERE name='usnm_qwen'")
        appid = cur.fetchone()[0]
        window = (appid, int(args.since.timestamp()), int(args.until.timestamp()))
        cur.execute(
            "SELECT id,name,batch,canonical_resultid,error_mask,need_validate,transition_time,max_total_results "
            "FROM workunit WHERE appid=%s AND create_time>=%s AND create_time<%s ORDER BY id LIMIT %s",
            (*window, args.limit),
        )
        cols = [c[0] for c in cur.description]
        units = [dict(zip(cols, row)) for row in cur.fetchall()]
        if len(units) >= args.limit:
            sys.exit(f"--limit {args.limit} reached; narrow the window")
        results = []
        if units:
            cur.execute(
                "SELECT r.id,r.workunitid,r.server_state,r.outcome,r.validate_state,r.elapsed_time "
                "FROM workunit w JOIN result r ON r.workunitid=w.id "
                "WHERE w.appid=%s AND w.create_time>=%s AND w.create_time<%s AND w.id BETWEEN %s AND %s LIMIT %s",
                (*window, units[0]["id"], units[-1]["id"], 8 * args.limit),
            )
            cols = [c[0] for c in cur.description]
            rows = cur.fetchall()
            if len(rows) >= 8 * args.limit:
                sys.exit(f"{len(rows)} results reached the result limit; narrow the window")
            results = [dict(zip(cols, row)) for row in rows]
    finally:
        db.rollback()
    jobs = {}
    for path in glob.glob(f"{args.qwen_root}/*/units/*/qwen.json"):
        job = read_job(path)
        if isinstance(job, dict) and "id" in job:
            jobs[job["id"]] = job_facts(job)
    for u in units:
        job_id = u["name"].removeprefix("usnm_qwen_")
        if job_id not in jobs:
            job = read_job(download_path(args.download, f"{job_id}_qwen_json", fanout))
            jobs[job_id] = job_facts(job) if isinstance(job, dict) else {"language": None, "prompt_version": None}
        u.update(jobs[job_id])
    return now, units, results


def label(unit):
    return f"{unit['language']} v{unit['prompt_version']}"


def verdict(unit, results, now):
    """Why an open unit is or isn't settled now: `settle`, `not disputed`,
    or the reason a disputed unit waits for a later pass."""
    returns = [r for r in results if r["outcome"] == SUCCESS]
    if sum(r["validate_state"] == INCONCLUSIVE for r in returns) < 2:
        return "not disputed"
    if any(r["server_state"] == IN_PROGRESS for r in results):
        return "replica running"
    if unit["need_validate"] or any(r["validate_state"] == VALIDATE_INIT for r in returns):
        return "awaiting validation"
    if unit["transition_time"] <= now + DUE_MARGIN:
        return "due for the transitioner"
    return "settle"


def unsent(results):
    return [r["id"] for r in results if r["server_state"] in (INACTIVE, UNSENT)]


def select(now, units, results, policy):
    """Counters per `<language> v<version>` for open units, and the units to settle."""
    by_unit = collections.defaultdict(list)
    for r in results:
        by_unit[r["workunitid"]].append(r)
    hours = collections.defaultdict(list)
    for u in units:
        hours[label(u)] += [r["elapsed_time"] / 3600 for r in by_unit[u["id"]] if r["outcome"] == SUCCESS and r["elapsed_time"]]
    groups = collections.defaultdict(collections.Counter)
    chosen = []
    stop = set(policy)
    for u in units:
        if u["canonical_resultid"] or u["error_mask"]:
            continue
        rs = by_unit[u["id"]]
        c = groups[label(u)]
        c["open"] += 1
        why = verdict(u, rs, now)
        if why == "not disputed":
            continue
        c["disputed"] += 1
        if (u["language"], u["prompt_version"]) not in stop:
            continue
        if why != "settle":
            c[f"waiting: {why}"] += 1
            continue
        c["settle"] += 1
        c[f"settle, batch {u['batch']}"] += 1
        c["unsent replicas cancelled"] += len(unsent(rs))
        # Replicas the unit could still have run: its unsent ones, and the rest up to the cap.
        c["most replicas forgone"] += len(unsent(rs)) + max(0, u["max_total_results"] - len(rs))
        chosen.append(u)
    out = {}
    for key in sorted(groups):
        c = groups[key]
        mean = sum(hours[key]) / len(hours[key]) if hours[key] else None
        row = {"in policy": any(key == f"{lang} v{v}" for lang, v in policy), **dict(sorted(c.items())),
               "mean hours per return": mean}
        if c["settle"] and mean is not None:
            row["GPU-hours forgone, low"] = c["unsent replicas cancelled"] * mean
            row["GPU-hours forgone, high"] = c["most replicas forgone"] * mean
        out[key] = row
    return out, chosen


def stamp(now):
    return dt.datetime.fromtimestamp(now, dt.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


def plan(args, policy):
    db, fanout = connect(args)
    try:
        now, units, results = read_window(db, args, fanout)
    finally:
        db.close()
    groups, chosen = select(now, units, results, policy)
    report = {"snapshot": stamp(now), "since": stamp(int(args.since.timestamp())), "until": stamp(int(args.until.timestamp())),
              "policy": [f"{lang}:{v}" for lang, v in policy], "units read": len(units), "groups": groups}
    if args.list_units:
        report["settle"] = [u["id"] for u in chosen]
    return report, chosen, units, results


def journal(f, **event):
    f.write(json.dumps({"at": dt.datetime.now(dt.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"), **event}, sort_keys=True) + "\n")
    f.flush()
    os.fsync(f.fileno())


def settle_one(db, unit, policy_text, log):
    """Recheck one unit under row locks and settle it. Returns the outcome."""
    cur = db.cursor()
    try:
        cur.execute("SET SESSION innodb_lock_wait_timeout=5")
        cur.execute("START TRANSACTION")
        cur.execute(
            "SELECT id,canonical_resultid,error_mask,need_validate,transition_time,max_total_results,UNIX_TIMESTAMP() "
            "FROM workunit WHERE id=%s FOR UPDATE", (unit["id"],))
        row = cur.fetchone()
        locked = dict(zip(("id", "canonical_resultid", "error_mask", "need_validate", "transition_time", "max_total_results", "now"), row))
        cur.execute("SELECT id,server_state,outcome,validate_state FROM result WHERE workunitid=%s ORDER BY id FOR UPDATE", (unit["id"],))
        results = [dict(zip(("id", "server_state", "outcome", "validate_state"), r)) for r in cur.fetchall()]
        before = {k: locked[k] for k in ("canonical_resultid", "error_mask", "need_validate", "transition_time", "max_total_results")}
        before["results"] = [[r["id"], r["server_state"], r["outcome"], r["validate_state"]] for r in results]
        why = "closed" if locked["canonical_resultid"] or locked["error_mask"] else verdict(locked, results, locked["now"])
        if why != "settle":
            db.rollback()
            log(event="skipped", workunit=unit["id"], reason=why, before=before)
            return why
        cancel = unsent(results)
        log(event="begin", workunit=unit["id"], name=unit["name"], group=label(unit), policy=policy_text, before=before, cancel=cancel)
        if cancel:
            cur.execute("UPDATE result SET server_state=%s,outcome=%s WHERE workunitid=%s AND server_state IN (%s,%s)",
                        (OVER, DIDNT_NEED, unit["id"], INACTIVE, UNSENT))
            if cur.rowcount != len(cancel):
                raise RuntimeError(f"cancelled {cur.rowcount} replicas, expected {len(cancel)}")
        cur.execute(
            "UPDATE workunit SET max_total_results=%s,error_mask=error_mask|%s,transition_time=UNIX_TIMESTAMP() "
            "WHERE id=%s AND canonical_resultid=0 AND error_mask=0 AND need_validate=0 AND max_total_results=%s",
            (len(results), TOO_MANY_TOTAL, unit["id"], locked["max_total_results"]))
        if cur.rowcount != 1:
            raise RuntimeError(f"workunit update matched {cur.rowcount} rows")
        db.commit()
    except Exception as e:
        db.rollback()
        if getattr(e, "args", (None,))[:1] in ((LOCK_WAIT,), (DEADLOCK,)):
            log(event="skipped", workunit=unit["id"], reason="locked", error=str(e))
            return "locked"
        log(event="failed", workunit=unit["id"], error=str(e))
        raise
    log(event="settled", workunit=unit["id"], cancelled=cancel, max_total_results=len(results))
    return "settled"


def settle(args, policy):
    if not policy:
        sys.exit("the policy is empty: nothing to settle")
    report, chosen, _, results = plan(args, policy)
    chosen = chosen[: args.max_units]
    by_unit = collections.defaultdict(list)
    for r in results:
        by_unit[r["workunitid"]].append(r)
    if not args.apply:
        report["would settle"] = [{"workunit": u["id"], "group": label(u), "cancel": unsent(by_unit[u["id"]])} for u in chosen]
        return report
    policy_text = [f"{lang}:{v}" for lang, v in policy]
    outcomes = collections.Counter()
    db, _ = connect(args)
    try:
        with open(args.journal, "a", encoding="utf-8") as f:
            def log(**event):
                journal(f, **event)

            log(event="run", policy=policy_text, max_units=args.max_units, selected=len(chosen))
            for u in chosen:
                outcomes[settle_one(db, u, policy_text, log)] += 1
            log(event="done", outcomes=dict(outcomes))
    finally:
        db.close()
    report["outcomes"] = dict(outcomes)
    return report


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    sub = parser.add_subparsers(dest="command", required=True)
    for name, text in (("plan", "count what the policy would settle; read-only"),
                       ("settle", "settle the policy's disputed units; a preview without --apply")):
        p = sub.add_parser(name, help=text)
        p.add_argument("--since", type=utc, required=True, help="first workunit create time, UTC")
        p.add_argument("--until", type=utc, required=True, help="workunits created before this, UTC")
        p.add_argument("--stop-tiebreakers", type=group_arg, action="append", default=[], metavar="LANGUAGE:VERSION",
                       help="a group whose disputed units get no more replicas; repeatable")
        p.add_argument("--policy-file", help=f"JSON policy (default {DEFAULT_POLICY}; a missing default file is an empty policy)")
        p.add_argument("--config", default="/project/usnmocr/config.xml")
        p.add_argument("--qwen-root", default="/project/qwen", help="campaign directories holding units/*/qwen.json")
        p.add_argument("--download", default="/project/usnmocr/download", help="download tree, for inputs with no campaign copy")
        p.add_argument("--db-ca", default="/usr/local/share/usnm/mysql-ca.pem")
        p.add_argument("--db-defaults", default="/usr/local/share/usnm/mysql-tls.cnf")
        p.add_argument("--limit", type=int, default=20000, help="most workunits to read")
        p.add_argument("--list-units", action="store_true", help="list the workunit ids to settle")
        if name == "settle":
            p.add_argument("--apply", action="store_true", help="change the database; without it, only preview")
            p.add_argument("--max-units", type=int, help="most units to settle in this run (required with --apply)")
            p.add_argument("--journal", help="append-only JSON-lines record of every change (required with --apply)")
    args = parser.parse_args(argv)
    if args.until <= args.since:
        parser.error("--until must be after --since")
    try:
        policy = resolve_policy(args)
    except (OSError, ValueError) as e:
        parser.error(str(e))
    if args.command == "plan":
        report = plan(args, policy)[0]
    else:
        if args.max_units is not None and args.max_units < 1:
            parser.error("--max-units must be at least 1")
        if args.apply and (args.max_units is None or not args.journal):
            parser.error("--apply needs --max-units and --journal")
        args.max_units = args.max_units or args.limit
        report = settle(args, policy)
    json.dump(report, sys.stdout, indent=1, default=float)
    print()


if __name__ == "__main__":
    main()
