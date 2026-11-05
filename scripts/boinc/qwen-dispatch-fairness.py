#!/usr/bin/env python3
"""Who can take the unsent `usnm_qwen` replicas, and who has been sent work:
per-host sends by day, units no open host can take, and the feeder cache
(#786). Read-only.

`collect` runs inside the BOINC server container. In one READ ONLY transaction
of bounded SELECTs it reads each Qwen host's results by send day, the unsent
Qwen replicas in the feeder's default order, the hosts already holding a
replica of those units, and recent sends. It reads the plan-class workunit
bounds from `plan_class_spec.xml` and, with `--show-shmem`, parses the feeder's
shared-memory cache. It prints one JSON snapshot and writes nothing. `report`
reads that snapshot and prints Markdown tables; it needs no database.

    ssh root@<boinc host> 'pct exec 230 -- podman exec -i usnm-boinc-server python3 - collect \
        --since 2026-10-28T00:00:00Z --show-shmem /project/usnmocr/bin/show_shmem' \
        < scripts/boinc/qwen-dispatch-fairness.py > snapshot.json
    $ scripts/boinc/qwen-dispatch-fairness.py report --snapshot snapshot.json
"""

import argparse
import collections
import datetime as dt
import json
import subprocess
import sys
import xml.etree.ElementTree as ET

APP = "usnm_qwen"
UNSENT, IN_PROGRESS = 2, 4  # result.server_state (`common_defs.h`)
SUCCESS = 1
ERROR_OUTCOMES = (3, 4, 6, 7)  # client error, no reply, validate error, detached
VALID, INVALID, NO_CHECK, INCONCLUSIVE = 1, 2, 3, 4


def utc(text):
    when = dt.datetime.fromisoformat(text.replace("Z", "+00:00"))
    if when.tzinfo is None:
        raise argparse.ArgumentTypeError("give a UTC time, e.g. 2026-10-28T00:00:00Z")
    return when.astimezone(dt.timezone.utc)


def plan_bounds(payload):
    """{plan class: [min_wu_id, max_wu_id], or None when unbounded}."""
    bounds = {}
    for node in ET.fromstring(payload).findall("plan_class"):
        lo, hi = node.findtext("min_wu_id"), node.findtext("max_wu_id")
        bounds[node.findtext("name")] = None if lo is None and hi is None else [int(lo or 0), int(hi or 2**63 - 1)]
    return bounds


def parse_shmem(text, app=APP):
    """The feeder cache from `show_shmem`: slot counts, and each cached replica of `app`
    as [result id, seconds in the cache]. Reserved slots are held by a scheduler."""
    cache = {"slots": 0, "empty": 0, "reserved": 0, "apps": collections.Counter(), "results": []}
    started = False
    for line in text.splitlines():
        t = line.split()
        if t[:1] == ["max_wu_results:"]:
            started = True
        if not started or not t or not t[0].rstrip(":").isdigit():
            continue
        cache["slots"] += 1
        if t[1] == "---":
            cache["empty"] += 1
        elif t[1] == "PID":
            cache["reserved"] += 1
        else:
            # The last five fields follow the free-text keywords: age, size class, reliable, infeasible, version.
            cache["apps"][t[1]] += 1
            if t[1] == app[:12]:
                cache["results"].append([int(t[3]), int(t[-5].rstrip("s"))])
    return cache


def collect(args):
    import MySQLdb  # only in the server image

    cfg = ET.parse(args.config).find("config")
    with open(args.plan_class_spec, "rb") as f:
        bounds = plan_bounds(f.read())
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
        cur.execute("SELECT UNIX_TIMESTAMP(UTC_TIMESTAMP())")
        now = int(cur.fetchone()[0])
        cur.execute("SELECT id FROM app WHERE name=%s", (APP,))
        appid = cur.fetchone()[0]
        since = int(args.since.timestamp())

        def rows(sql, params, limit, what):
            cur.execute(sql + " LIMIT %s", (*params, limit))
            out = [[v if isinstance(v, str) else int(v) for v in row] for row in cur.fetchall()]
            if len(out) >= limit:
                sys.exit(f"{what}: {limit} rows reached the limit; narrow --since or raise --limit")
            return out

        hosts = rows(
            "SELECT r.hostid, av.plan_class, MAX(r.sent_time) FROM result r JOIN app_version av ON av.id=r.app_version_id "
            "WHERE r.appid=%s AND r.sent_time>=%s GROUP BY r.hostid, av.plan_class",
            (appid, since), args.limit, "hosts")
        daily = rows(
            "SELECT r.hostid, FLOOR(r.sent_time/86400), COUNT(*), "
            f"SUM(r.outcome={SUCCESS} AND r.validate_state={VALID}), SUM(r.outcome={SUCCESS} AND r.validate_state={INVALID}), "
            f"SUM(r.outcome={SUCCESS} AND r.validate_state={INCONCLUSIVE}), SUM(r.outcome={SUCCESS} AND r.validate_state={NO_CHECK}), "
            f"SUM(r.outcome={SUCCESS} AND r.validate_state=0), "
            f"SUM(r.outcome IN {ERROR_OUTCOMES}), SUM(r.server_state={IN_PROGRESS}) "
            "FROM result r WHERE r.appid=%s AND r.sent_time>=%s GROUP BY r.hostid, FLOOR(r.sent_time/86400)",
            (appid, since), args.limit, "daily")
        # The feeder's default order (`feeder.cpp`): result priority, then workunit id.
        unsent = rows(
            f"SELECT r.id, r.workunitid, r.priority FROM result r WHERE r.appid=%s AND r.server_state={UNSENT} "
            "ORDER BY r.priority DESC, r.workunitid, r.id",
            (appid,), args.limit, "unsent")
        taken = rows(
            "SELECT DISTINCT x.workunitid, x.hostid FROM result x JOIN result r ON r.workunitid=x.workunitid "
            f"WHERE r.appid=%s AND r.server_state={UNSENT} AND x.hostid>0",
            (appid,), 8 * args.limit, "taken")
        recent = rows(
            "SELECT r.hostid, r.workunitid, r.priority FROM result r WHERE r.appid=%s AND r.sent_time>=%s",
            (appid, now - 3600 * args.recent_hours), args.limit, "recent")
    finally:
        try:
            db.rollback()
        finally:
            db.close()
    plan = {host: plan_class for host, plan_class, _ in sorted(hosts, key=lambda h: h[2])}
    cache = None
    if args.show_shmem:
        shown = subprocess.run([args.show_shmem], cwd=args.project, capture_output=True, text=True, timeout=30, check=True)
        cache = parse_shmem(shown.stdout)
    json.dump({
        "snapshot": now, "since": since, "recent_hours": args.recent_hours,
        "bounds": {c: bounds.get(c, "absent") for c in sorted(set(plan.values()))},
        "hosts": plan, "daily": daily, "unsent": unsent, "taken": taken, "recent": recent, "cache": cache,
    }, sys.stdout)
    print()


def can_take(bounds, wu):
    return bounds is None or (bounds != "absent" and bounds[0] <= wu <= bounds[1])


def analyse(snap):
    hosts = {int(h): c for h, c in snap["hosts"].items()}
    bounds = snap["bounds"]
    taken = collections.defaultdict(set)
    for wu, host in snap["taken"]:
        taken[wu].add(host)
    units = list(dict.fromkeys((-prio, wu) for _, wu, prio in snap["unsent"]))
    takers = {key: [h for h, c in hosts.items() if h not in taken[key[1]] and can_take(bounds[c], key[1])] for key in units}
    blocked = [key for key in units if not takers[key]]
    head = 0
    while head < len(units) and not takers[units[head]]:
        head += 1
    a = {
        "replicas": len(snap["unsent"]), "units": len(units), "blocked": len(blocked), "head": head,
        "can take": collections.Counter(h for key in units for h in takers[key]),
        "only taker": collections.Counter(takers[key][0] for key in units if len(takers[key]) == 1),
        # Hosts holding no replica of a blocked unit: they could take it if their class allowed.
        "free of blocked": collections.Counter(h for key in blocked for h in hosts if h not in taken[key[1]]),
        "recent": collections.Counter(h for h, _, _ in snap["recent"]),
        "past head": collections.Counter(h for h, wu, prio in snap["recent"] if head and (-prio, wu) > units[head - 1]),
    }
    cache = snap.get("cache")
    if cache:
        unsent_ids = {rid for rid, _, _ in snap["unsent"]}
        blocked_units = {wu for _, wu in blocked}
        cached = {rid for rid, _ in cache["results"]}
        a["cache"] = {
            "qwen slots": len(cached), "empty": cache["empty"], "reserved": cache["reserved"], "slots": cache["slots"],
            "unsent cached": len(cached & unsent_ids),
            "blocked cached": sum(1 for rid, wu, _ in snap["unsent"] if rid in cached and wu in blocked_units),
            "oldest": max((age for _, age in cache["results"]), default=None),
        }
    return a


def table(header, lines):
    out = ["| " + " | ".join(header) + " |", "|" + "|".join("---" if i < 3 else "--:" for i in range(len(header))) + "|"]
    out += ["| " + " | ".join(str(x) for x in line) + " |" for line in lines]
    return "\n".join(out)


def stamp(seconds):
    return dt.datetime.fromtimestamp(seconds, dt.timezone.utc).strftime("%Y-%m-%d %H:%M:%SZ")


def bounds_text(bounds):
    if bounds is None:
        return "unbounded"
    if bounds == "absent":
        return "not in policy"
    return "closed" if bounds[0] > bounds[1] else f"{bounds[0]}–{bounds[1]}"


def report(args):
    with open(args.snapshot, encoding="utf-8") as f:
        snap = json.load(f)
    hosts = {int(h): c for h, c in snap["hosts"].items()}
    print(f"Database read {stamp(snap['snapshot'])}; results sent since {stamp(snap['since'])}. "
          "A host's plan class is that of its latest Qwen send.\n")
    print(table(
        ("Host", "Plan class", "Day sent", "Sent", "Valid", "Invalid", "Inconclusive", "Not checked", "Pending", "Errors", "In progress"),
        [(h, hosts.get(h, "?"), dt.date.fromordinal(dt.date(1970, 1, 1).toordinal() + day), *counts)
         for h, day, *counts in sorted(snap["daily"])],
    ))
    print("\nBy the day each replica was sent, as it stands now. Valid to Pending count successful returns; Not checked: "
          "the unit failed first, for example at the replica cap. Errors: client error, no reply, validate error or detached.\n")
    a = analyse(snap)
    n = snap["recent_hours"]
    print(f"{a['replicas']} unsent replicas on {a['units']} units. A host can take a unit when its plan class's "
          f"workunit bounds include it and it holds no replica of it. {a['blocked']} units no listed host can take now; "
          f"the first {a['head']} units in feeder order are among them.\n")
    print(table(
        ("Host", "Plan class", "Bounds", "Can take", "Only taker", "Could take a blocked unit", f"Sent, last {n} h",
         f"Sent past the blocked head, last {n} h"),
        [(h, c, bounds_text(snap["bounds"][c]), a["can take"][h], a["only taker"][h], a["free of blocked"][h], a["recent"][h],
          a["past head"][h] if a["head"] else "–") for h, c in sorted(hosts.items())],
    ))
    print("\nCould take a blocked unit: holds no replica of it, but its plan class's bounds exclude it. "
          "Sent past the blocked head: sent units that come after the blocked head in feeder order.")
    c = a.get("cache")
    if c:
        print(f"\nFeeder cache: {c['qwen slots']} of {c['slots']} slots hold Qwen replicas, {c['empty']} are empty and "
              f"{c['reserved']} are reserved by a scheduler. {c['unsent cached']} of {a['replicas']} unsent replicas are cached, "
              f"{c['blocked cached']} of them on blocked units; the oldest Qwen slot is {c['oldest']} s old.")


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    sub = parser.add_subparsers(dest="command", required=True)
    c = sub.add_parser("collect", help="read the BOINC database, policy and feeder cache (server container)")
    c.add_argument("--since", type=utc, required=True, help="first send time for the per-host series, UTC")
    c.add_argument("--recent-hours", type=int, default=24, help="hours of recent sends to compare with the blocked head")
    c.add_argument("--config", default="/project/usnmocr/config.xml")
    c.add_argument("--plan-class-spec", default="/project/usnmocr/plan_class_spec.xml")
    c.add_argument("--project", default="/project/usnmocr", help="working directory for --show-shmem")
    c.add_argument("--show-shmem", help="the project's bin/show_shmem, to read the feeder cache")
    c.add_argument("--db-ca", default="/usr/local/share/usnm/mysql-ca.pem")
    c.add_argument("--db-defaults", default="/usr/local/share/usnm/mysql-tls.cnf")
    c.add_argument("--limit", type=int, default=20000, help="most rows per query (8x for holders of unsent units)")
    r = sub.add_parser("report", help="print Markdown tables from a snapshot")
    r.add_argument("--snapshot", required=True)
    args = parser.parse_args(argv)
    if args.command == "collect":
        collect(args)
    else:
        report(args)


if __name__ == "__main__":
    main()
