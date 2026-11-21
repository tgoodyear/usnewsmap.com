#!/usr/bin/env python3
"""Raise `result.priority` on stranded `usnm_qwen` siblings so the scheduler sends them first.

`usnm_qwen` runs two replicas per workunit and one result per host per workunit. A host that
takes one copy is barred from the other, and the unsent copy then queues behind fresh first
copies, because every result has priority 0 and the scheduler sends the higher priority first.
This finds unsent results (`server_state` UNSENT) of the app whose workunit has another result
that was sent (IN_PROGRESS or OVER, `sent_time` > 0), once the oldest such sibling was sent at
least `--min-age-hours` ago, and sets their priority to `--priority` where it is lower. It never
lowers a priority it did not set, and never touches another app: a journal records each boost and
the priority it replaced, and only those are ever put back. A rerun finds nothing new.

It runs in the BOINC server container (it needs MySQLdb, the project's `config.xml` and the DB
TLS files), and prints one JSON summary line per run:

    podman exec usnm-boinc-server python3 /usr/local/lib/usnm-boinc/qwen-sibling-priority.py --dry-run
"""

import argparse
import json
import os
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


def load_journal(path):
    """This step's own boosts: result id -> the priority it had and its workunit's first send."""
    try:
        with open(path) as f:
            raised = json.load(f)["raised"]
    except FileNotFoundError:
        return {}
    return {int(k): {"from": int(v["from"]), "first_sent": int(v["first_sent"])} for k, v in raised.items()}


def save_journal(path, raised):
    tmp = f"{path}.tmp"
    with open(tmp, "w") as f:
        json.dump({"raised": {str(k): v for k, v in sorted(raised.items())}}, f, sort_keys=True)
        f.flush()
        os.fsync(f.fileno())
    os.replace(tmp, path)


def run(args, out=sys.stdout, connect_db=connect):
    """Each run, under the lock:

    1. Forget journal entries that were sent, or whose priority someone else changed since.
    2. Give up on this step's boosts still unsent --give-up-hours after their workunit's first copy
       went out (no host takes them: no eligible plan class, say): put back the priority each had,
       only where it is still the one this step set.
    3. Raise new siblings while this step's outstanding boosts stay within --max-raised, so they
       never crowd fresh copies out of the app's feeder share.

    The journal is written before the database commit with every entry this run touches, and again
    after it without the ones put back, so a failed commit or write leaves only entries that step 1
    forgets or step 2 retries next run; nothing is lowered that this step did not set.
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
        cur.execute("SELECT UNIX_TIMESTAMP()")
        now = int(cur.fetchone()[0])
        cur.execute("SELECT id FROM app WHERE name=%s", (APP,))
        row = cur.fetchone()
        if row is None:
            sys.exit(f"no app named {APP}")
        appid = row[0]
        cutoff = now - int(args.min_age_hours * 3600)
        give_up = now - int(args.give_up_hours * 3600)
        journal = load_journal(args.journal)
        live = {}
        if journal:
            ids = list(journal)
            marks = ", ".join(["%s"] * len(ids))
            cur.execute(f"SELECT id FROM result WHERE appid = %s AND server_state = %s AND priority = %s "
                        f"AND id IN ({marks})", (appid, UNSENT, args.priority, *ids))
            live = {int(r[0]) for r in cur.fetchall()}
        report["forgotten"] = len(journal) - len(live)
        stale = {rid: e for rid, e in journal.items() if rid in live and e["first_sent"] <= give_up}
        kept = {rid: e for rid, e in journal.items() if rid in live and rid not in stale}
        report["gave_up"] = len(stale)
        report["already_raised"] = len(kept)
        room = min(args.max, max(0, args.max_raised - len(kept)))
        picked = []
        if room:
            cur.execute(SELECT_SQL, (appid, UNSENT, args.priority, IN_PROGRESS, OVER, cutoff, give_up, room))
            picked = [(int(r[0]), int(r[1]), int(r[2])) for r in cur.fetchall()]
        report["selected"] = len(picked)
        if picked:
            report["oldest_age_hours"] = round((now - min(sent for _, _, sent in picked)) / 3600, 2)
        if args.dry_run or not (stale or picked or report["forgotten"]):
            return report_out(report, out)
        after = dict(kept)
        after.update({rid: {"from": prio, "first_sent": sent} for rid, prio, sent in picked})
        try:
            for rid, e in stale.items():
                # Only where it is still this step's priority: a result sent or changed since is left alone.
                cur.execute("UPDATE result SET priority = %s WHERE id = %s AND appid = %s AND server_state = %s "
                            "AND priority = %s", (e["from"], rid, appid, UNSENT, args.priority))
                report["restored"] += cur.rowcount
            if picked:
                ids = [rid for rid, _, _ in picked]
                marks = ", ".join(["%s"] * len(ids))
                # The guards repeat the selection's, so a result sent or raised since is left alone.
                cur.execute(
                    f"UPDATE result SET priority = %s WHERE appid = %s AND server_state = %s "
                    f"AND priority >= 0 AND priority < %s AND id IN ({marks})",
                    (args.priority, appid, UNSENT, args.priority, *ids),
                )
                report["updated"] = cur.rowcount
            # Before the commit, the journal lists everything this run may leave raised; after it, only
            # what stays raised. Either way an entry that isn't at this step's priority is forgotten.
            save_journal(args.journal, {**stale, **after})
            db.commit()
        except Exception:
            db.rollback()
            raise
        if stale:
            save_journal(args.journal, after)
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
    parser.add_argument("--journal", default="/project/usnmocr/qwen/sibling-priority-journal.json",
                        help="this step's own boosts and the priority each had, so only those are ever put back")
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
