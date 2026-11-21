#!/usr/bin/env python3
"""Raise `result.priority` on stranded `usnm_qwen` siblings so the scheduler sends them first.

`usnm_qwen` runs two replicas per workunit and one result per host per workunit. A host that
takes one copy is barred from the other, and the unsent copy then queues behind fresh first
copies, because every result has priority 0 and the scheduler sends the higher priority first.
This finds unsent results (`server_state` UNSENT) of the app whose workunit has another result
that was sent (IN_PROGRESS or OVER, `sent_time` > 0), once the oldest such sibling was sent at
least `--min-age-hours` ago, and sets their priority to `--priority` where it is lower. It never
lowers a priority and never touches another app. A rerun finds nothing new, so it is idempotent.

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


# One row per unsent result whose priority is below the target, but not negative, and whose
# workunit has a sent sibling. A negative priority is deliberate (Xiaomi batches are created at -1
# so production work enters the feeder first) and is never raised. MIN(sent_time) is when the workunit's first copy went out, so a later tie-breaker
# copy doesn't restart the clock.
# Raised unsent results whose workunit's first copy went out within --give-up-hours. These sit
# ahead of fresh copies in the feeder, so they are bounded (--max-raised) and fresh work keeps its
# room in the app's cache share. One raised longer ago than that has no host taking it (no
# eligible plan class, say): it keeps its priority, which only orders sends, but no longer holds a
# place under the bound, and siblings that old are not raised. Nothing is ever lowered, so a
# priority this step did not set is never touched.
RAISED_SQL = (
    "SELECT COUNT(*) FROM (SELECT r.id FROM result r JOIN result s ON s.workunitid = r.workunitid "
    "AND s.id <> r.id WHERE r.appid = %s AND r.server_state = %s AND r.priority >= %s AND s.sent_time > 0 "
    "GROUP BY r.id HAVING MIN(s.sent_time) > %s) raised"
)
SELECT_SQL = (
    "SELECT r.id, MIN(s.sent_time) AS first_sent FROM result r JOIN result s ON s.workunitid = r.workunitid "
    "AND s.id <> r.id WHERE r.appid = %s AND r.server_state = %s AND r.priority >= 0 AND r.priority < %s "
    "AND s.server_state IN (%s, %s) AND s.sent_time > 0 "
    "GROUP BY r.id HAVING MIN(s.sent_time) <= %s AND MIN(s.sent_time) > %s ORDER BY first_sent, r.id LIMIT %s"
)
# One run at a time, timer or by hand, so two can't each fill the bound: a MySQL named lock, held
# by this connection for the count, selection and update, and freed when it closes.
LOCK_NAME = "usnm-qwen-sibling-priority"


def run(args, out=sys.stdout, connect_db=connect):
    db = connect_db(args)
    raised, picked, updated, locked = 0, [], 0, False
    now = 0
    try:
        cur = db.cursor()
        cur.execute("SELECT GET_LOCK(%s, 0)", (LOCK_NAME,))
        locked = cur.fetchone()[0] == 1
        if locked:
            cur.execute("SELECT UNIX_TIMESTAMP()")
            now = int(cur.fetchone()[0])
            cur.execute("SELECT id FROM app WHERE name=%s", (APP,))
            row = cur.fetchone()
            if row is None:
                sys.exit(f"no app named {APP}")
            appid = row[0]
            cutoff = now - int(args.min_age_hours * 3600)
            give_up = now - int(args.give_up_hours * 3600)
            cur.execute(RAISED_SQL, (appid, UNSENT, args.priority, give_up))
            raised = int(cur.fetchone()[0])
            room = min(args.max, max(0, args.max_raised - raised))
            if room:
                cur.execute(SELECT_SQL, (appid, UNSENT, args.priority, IN_PROGRESS, OVER, cutoff, give_up, room))
                picked = [(int(r[0]), int(r[1])) for r in cur.fetchall()]
            if picked and not args.dry_run:
                ids = [rid for rid, _ in picked]
                marks = ", ".join(["%s"] * len(ids))
                try:
                    # The guards repeat the selection's, so a result sent or raised since is left alone.
                    cur.execute(
                        f"UPDATE result SET priority = %s WHERE appid = %s AND server_state = %s "
                        f"AND priority >= 0 AND priority < %s AND id IN ({marks})",
                        (args.priority, appid, UNSENT, args.priority, *ids),
                    )
                    updated = cur.rowcount
                    db.commit()
                except Exception:
                    db.rollback()
                    raise
    finally:
        db.close()
    oldest = round((now - min(sent for _, sent in picked)) / 3600, 2) if picked else 0.0
    print(json.dumps({"app": APP, "skipped": None if locked else "another run holds the lock",
                      "already_raised": raised, "selected": len(picked), "updated": updated,
                      "oldest_age_hours": oldest, "priority": args.priority, "min_age_hours": args.min_age_hours,
                      "give_up_hours": args.give_up_hours, "dry_run": args.dry_run}, sort_keys=True), file=out)
    return 0


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("--min-age-hours", type=non_negative_float, default=6.0,
                        help="how long ago the sibling was sent before the result is raised (default 6; 0 is allowed)")
    parser.add_argument("--priority", type=positive, default=10, help="priority to set where it is lower (default 10)")
    parser.add_argument("--max", type=positive, default=500, help="most results to raise in one run (default 500)")
    parser.add_argument("--give-up-hours", type=non_negative_float, default=72.0,
                        help="stop raising, and counting under --max-raised, siblings whose workunit's first copy is "
                             "older than this (default 72, the delay bound)")
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
