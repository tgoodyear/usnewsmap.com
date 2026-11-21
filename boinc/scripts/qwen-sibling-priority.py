#!/usr/bin/env python3
"""Raise `result.priority` on stranded `usnm_qwen` siblings so the scheduler sends them first.

`usnm_qwen` runs two replicas per workunit and one result per host per workunit. A host that
takes one copy is barred from the other, and the unsent copy then queues behind fresh first
copies, because every result has priority 0 and the scheduler sends the higher priority first.
This finds unsent results (`server_state` UNSENT) of the app whose workunit has another result
that was sent (IN_PROGRESS or OVER, `sent_time` > 0), once the oldest such sibling was sent at
least `--min-age-hours` ago, and sets their priority to `--priority` where it is lower. It never
lowers a priority it did not set, and never touches another app: a table in the project database
(`usnm_qwen_sibling_boost`) records each boost and the priority it replaced, and only those are
ever put back. A rerun finds nothing new.

It runs in the BOINC server container (it needs MySQLdb, the project's `config.xml` and the DB
TLS files), and prints one JSON summary line per run:

    podman exec usnm-boinc-server python3 /usr/local/lib/usnm-boinc/qwen-sibling-priority.py --dry-run
"""

import argparse
import json
import sys
import xml.etree.ElementTree as ET

APP = "usnm_qwen"
# BOINC result server states (`common_defs.h`).
UNSENT, IN_PROGRESS, OVER = 2, 4, 5


def connect(args):
    import MySQLdb  # only in the server image

    cfg = ET.parse(args.config).find("config")
    return MySQLdb.connect(
        host=cfg.findtext("db_host"),
        user=cfg.findtext("db_user"),
        passwd=cfg.findtext("db_passwd"),
        db=cfg.findtext("db_name"),
        ssl={"ca": args.db_ca},
        read_default_file=args.db_defaults,
        read_default_group="client",
    )


def non_negative_float(text):
    value = float(text)
    if value < 0:
        raise argparse.ArgumentTypeError("must not be negative")
    return value


def positive(text):
    value = int(text)
    if value < 1:
        raise argparse.ArgumentTypeError("must be at least 1")
    return value


# Unsent results with a sent sibling, below the target but not negative (a negative priority is
# deliberate: Xiaomi batches are created at -1 so production work enters the feeder first), whose
# workunit's first copy went out between --give-up-hours and --min-age-hours ago. MIN(sent_time)
# is when the first copy went out, so a later tie-breaker copy doesn't restart the clock.
SELECT_SQL = (
    "SELECT r.id, r.priority, MIN(s.sent_time) AS first_sent FROM result r JOIN result s "
    "ON s.workunitid = r.workunitid AND s.id <> r.id "
    "WHERE r.appid = %s AND r.server_state = %s AND r.priority >= 0 AND r.priority < %s "
    "AND s.server_state IN (%s, %s) AND s.sent_time > 0 "
    "GROUP BY r.id, r.priority HAVING MIN(s.sent_time) <= %s AND MIN(s.sent_time) > %s "
    "ORDER BY first_sent, r.id LIMIT %s"
)
# One run at a time, timer or by hand, so two can't each fill the bound: a MySQL named lock, held
# by this connection for the whole run and freed when it closes.
LOCK_NAME = "usnm-qwen-sibling-priority"


# This step's own boosts, kept in the project database next to the rows they change, so a boost and
# its record commit or roll back together: the priority it replaced, the one it set, and when its
# workunit's first copy went out. Only rows listed here are ever put back.
JOURNAL_TABLE = "usnm_qwen_sibling_boost"
CREATE_JOURNAL_SQL = (
    f"CREATE TABLE IF NOT EXISTS {JOURNAL_TABLE} (result_id BIGINT NOT NULL PRIMARY KEY, "
    "from_priority INT NOT NULL, to_priority INT NOT NULL, first_sent BIGINT NOT NULL)"
)


def load_journal(cur):
    """result id -> {"from", "to", "first_sent"} for every boost this step has on record."""
    cur.execute(f"SELECT result_id, from_priority, to_priority, first_sent FROM {JOURNAL_TABLE}")
    return {int(r[0]): {"from": int(r[1]), "to": int(r[2]), "first_sent": int(r[3])} for r in cur.fetchall()}


def run(args, out=sys.stdout, connect_db=connect):
    """Each run, under the lock and in one transaction:

    1. Forget boosts that were sent, or whose priority is no longer the one this step set.
    2. Give up on this step's boosts still unsent --give-up-hours after their workunit's first copy
       went out (no host takes them: no eligible plan class, say): put back the priority each had,
       only where it is still the one this step set.
    3. Raise new siblings while this step's outstanding boosts stay within --max-raised, so they
       never crowd fresh copies out of the app's feeder share. Each row is raised only from the
       priority it was selected at, and recorded only if that update took.

    The record lives in a table of the same database, so it can't drift from the rows it describes.
    """
    db = connect_db(args)
    report = {"app": APP, "skipped": None, "forgotten": 0, "gave_up": 0, "restored": 0, "already_raised": 0,
              "selected": 0, "updated": 0, "oldest_age_hours": 0.0, "priority": args.priority,
              "min_age_hours": args.min_age_hours, "give_up_hours": args.give_up_hours, "dry_run": args.dry_run}
    try:
        cur = db.cursor()
        cur.execute("SELECT GET_LOCK(%s, 0)", (LOCK_NAME,))
        if cur.fetchone()[0] != 1:
            report["skipped"] = "another run holds the lock"
            return report_out(report, out)
        if not args.dry_run:
            # DDL commits implicitly in MySQL, so it runs before any change of this run.
            cur.execute(CREATE_JOURNAL_SQL)
            db.commit()
        cur.execute("SELECT UNIX_TIMESTAMP()")
        now = int(cur.fetchone()[0])
        cur.execute("SELECT id FROM app WHERE name=%s", (APP,))
        row = cur.fetchone()
        if row is None:
            sys.exit(f"no app named {APP}")
        appid = row[0]
        cutoff = now - int(args.min_age_hours * 3600)
        give_up = now - int(args.give_up_hours * 3600)
        try:
            journal = load_journal(cur)
        except Exception:
            if not args.dry_run:
                raise
            journal = {}  # a dry run before the first real one: no table yet, so no boosts
        live = set()
        for rid, e in journal.items():
            cur.execute("SELECT 1 FROM result WHERE id = %s AND appid = %s AND server_state = %s AND priority = %s",
                        (rid, appid, UNSENT, e["to"]))
            if cur.fetchone() is not None:
                live.add(rid)
        forgotten = [rid for rid in journal if rid not in live]
        stale = {rid: e for rid, e in journal.items() if rid in live and e["first_sent"] <= give_up}
        kept = [rid for rid in live if rid not in stale]
        report.update(forgotten=len(forgotten), gave_up=len(stale), already_raised=len(kept))
        room = min(args.max, max(0, args.max_raised - len(kept)))
        picked = []
        if room:
            cur.execute(SELECT_SQL, (appid, UNSENT, args.priority, IN_PROGRESS, OVER, cutoff, give_up, room))
            picked = [(int(r[0]), int(r[1]), int(r[2])) for r in cur.fetchall()]
        report["selected"] = len(picked)
        if picked:
            report["oldest_age_hours"] = round((now - min(sent for _, _, sent in picked)) / 3600, 2)
        if args.dry_run or not (stale or picked or forgotten):
            return report_out(report, out)
        try:
            for rid in forgotten:
                cur.execute(f"DELETE FROM {JOURNAL_TABLE} WHERE result_id = %s", (rid,))
            for rid, e in stale.items():
                # Only where it is still this step's priority: a result sent or changed since is left alone.
                cur.execute("UPDATE result SET priority = %s WHERE id = %s AND appid = %s AND server_state = %s "
                            "AND priority = %s", (e["from"], rid, appid, UNSENT, e["to"]))
                report["restored"] += cur.rowcount
                cur.execute(f"DELETE FROM {JOURNAL_TABLE} WHERE result_id = %s", (rid,))
            for rid, prio, sent in picked:
                # Only from the priority it was selected at, so a result sent or changed since is left alone.
                cur.execute("UPDATE result SET priority = %s WHERE id = %s AND appid = %s AND server_state = %s "
                            "AND priority = %s", (args.priority, rid, appid, UNSENT, prio))
                if cur.rowcount == 1:
                    cur.execute(f"INSERT INTO {JOURNAL_TABLE} (result_id, from_priority, to_priority, first_sent) "
                                "VALUES (%s, %s, %s, %s)", (rid, prio, args.priority, sent))
                    report["updated"] += 1
            db.commit()
        except Exception:
            db.rollback()
            raise
    finally:
        db.close()
    return report_out(report, out)


def report_out(report, out):
    print(json.dumps(report, sort_keys=True), file=out)
    return 0


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("--min-age-hours", type=non_negative_float, default=6.0,
                        help="how long ago the sibling was sent before the result is raised (default 6; 0 is allowed)")
    parser.add_argument("--priority", type=positive, default=10, help="priority to set where it is lower (default 10)")
    parser.add_argument("--max", type=positive, default=500, help="most results to raise in one run (default 500)")
    parser.add_argument("--give-up-hours", type=non_negative_float, default=72.0,
                        help="put back the priority of this step's boosts still unsent once their workunit's first "
                             "copy is this old, and raise no siblings older than this (default 72, the delay bound)")
    parser.add_argument("--max-raised", type=positive, default=50,
                        help="most unsent results to keep raised at once, so fresh copies keep their place "
                             "in the feeder (default 50)")
    parser.add_argument("--dry-run", action="store_true", help="select and report; change nothing")
    parser.add_argument("--config", default="/project/usnmocr/config.xml")
    parser.add_argument("--db-ca", default="/usr/local/share/usnm/mysql-ca.pem")
    parser.add_argument("--db-defaults", default="/usr/local/share/usnm/mysql-tls.cnf")
    return run(parser.parse_args(argv))


if __name__ == "__main__":
    sys.exit(main())
