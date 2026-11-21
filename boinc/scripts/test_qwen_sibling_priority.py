import contextlib
import importlib.util
import io
import itertools
import json
import os
import sqlite3
import tempfile
import types
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location("sibling_priority", ROOT / "boinc/scripts/qwen-sibling-priority.py")
P = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(P)

NOW = 1_800_000_000
HOUR = 3600
UNSENT, IN_PROGRESS, OVER = 2, 4, 5


class Cursor:
    """Runs the script's SQL on in-memory sqlite: %s becomes ?, and the clock is fixed."""

    def __init__(self, conn, lock_free=True):
        self.conn = conn
        self.cur = conn.cursor()
        self.rowcount = 0
        self.lock_free = lock_free

    def execute(self, sql, params=()):
        if sql.startswith("SELECT GET_LOCK("):
            self.cur.execute("SELECT ?", (1 if self.lock_free else 0,))
        elif sql == "SELECT UNIX_TIMESTAMP()":
            self.cur.execute("SELECT ?", (NOW,))
        else:
            self.cur.execute(sql.replace("%s", "?"), params)
        self.rowcount = self.cur.rowcount

    def fetchone(self):
        return self.cur.fetchone()

    def fetchall(self):
        return self.cur.fetchall()


class Db:
    def __init__(self, conn, lock_free=True):
        self.conn = conn
        self.commits = 0
        self.closed = False
        self.lock_free = lock_free

    def cursor(self):
        return Cursor(self.conn, self.lock_free)

    def commit(self):
        self.commits += 1
        self.conn.commit()

    def rollback(self):
        self.conn.rollback()

    def close(self):
        self.closed = True


def database(results):
    conn = sqlite3.connect(":memory:")
    conn.execute("CREATE TABLE app (id INTEGER PRIMARY KEY, name TEXT)")
    conn.execute("CREATE TABLE result (id INTEGER PRIMARY KEY, appid INT, workunitid INT, server_state INT, "
                 "priority INT, sent_time INT)")
    conn.executemany("INSERT INTO app VALUES (?, ?)", [(1, "usnm_qwen"), (2, "usnm_other")])
    conn.executemany("INSERT INTO result VALUES (?, ?, ?, ?, ?, ?)", results)
    conn.commit()
    return conn


def sibling_pair(base, wu, *, sent_ago=10 * HOUR, state=IN_PROGRESS, appid=1, priority=0, sent_priority=0):
    sent = NOW - sent_ago if sent_ago is not None else 0
    return [(base, appid, wu, state, sent_priority, sent), (base + 1, appid, wu, UNSENT, priority, 0)]


JOURNALS = tempfile.TemporaryDirectory()


def args(**over):
    base = dict(min_age_hours=6.0, priority=10, max=500, max_raised=50, give_up_hours=72.0, dry_run=False,
                journal=os.path.join(JOURNALS.name, f"journal-{next(_journal_ids)}.json"))
    base.update(over)
    return types.SimpleNamespace(**base)


_journal_ids = itertools.count()


def journal_file(entries):
    path = os.path.join(JOURNALS.name, f"journal-{next(_journal_ids)}.json")
    with open(path, "w") as f:
        json.dump({"raised": {str(k): v for k, v in entries.items()}}, f)
    return path


def go(rows, **over):
    conn = database(rows)
    db = Db(conn)
    out = io.StringIO()
    P.run(args(**over), out=out, connect_db=lambda _a: db)
    priorities = dict(conn.execute("SELECT id, priority FROM result"))
    return json.loads(out.getvalue()), priorities, db


class SiblingPriorityTests(unittest.TestCase):
    def test_raises_only_stranded_siblings(self):
        rows = [
            *sibling_pair(10, 1),                          # unsent copy, sibling in progress
            *sibling_pair(20, 2, state=OVER),              # sibling already returned
            (30, 1, 3, UNSENT, 0, 0), (31, 1, 3, UNSENT, 0, 0),  # fresh unit: nothing sent
            *sibling_pair(40, 4, sent_ago=None),           # "sibling" in progress but sent_time 0
            (50, 1, 5, 1, 0, 0), (51, 1, 5, 3, 0, NOW - 20 * HOUR),  # sibling INACTIVE/other state
        ]
        report, priorities, db = go(rows)
        self.assertEqual((report["selected"], report["updated"]), (2, 2))
        self.assertEqual([priorities[i] for i in (11, 21)], [10, 10])
        self.assertEqual([priorities[i] for i in (10, 20, 30, 31, 40, 41, 50, 51)], [0] * 8)
        self.assertEqual(report["oldest_age_hours"], 10.0)
        self.assertTrue(db.closed)

    def test_min_age_gate(self):
        rows = [*sibling_pair(10, 1, sent_ago=5 * HOUR), *sibling_pair(20, 2, sent_ago=7 * HOUR)]
        report, priorities, _ = go(rows)
        self.assertEqual((priorities[11], priorities[21]), (0, 10))
        self.assertEqual(report["selected"], 1)
        _, priorities, _ = go(rows, min_age_hours=8)
        self.assertEqual((priorities[11], priorities[21]), (0, 0))
        _, priorities, _ = go(rows, min_age_hours=5)  # boundary: exactly old enough
        self.assertEqual((priorities[11], priorities[21]), (10, 10))

    def test_zero_min_age_takes_any_sent_sibling(self):
        rows = sibling_pair(10, 1, sent_ago=0)
        report, priorities, _ = go(rows, min_age_hours=0)
        self.assertEqual((report["selected"], priorities[11]), (1, 10))

    def test_oldest_sibling_sets_the_age(self):
        rows = [(10, 1, 1, IN_PROGRESS, 0, NOW - HOUR), (11, 1, 1, OVER, 0, NOW - 9 * HOUR),
                (12, 1, 1, UNSENT, 0, 0)]
        report, priorities, _ = go(rows)
        self.assertEqual((priorities[12], report["oldest_age_hours"]), (10, 9.0))

    def test_never_lowers(self):
        rows = [*sibling_pair(10, 1, priority=50), *sibling_pair(20, 2, priority=10), *sibling_pair(30, 3, priority=3)]
        report, priorities, _ = go(rows)
        self.assertEqual([priorities[11], priorities[21], priorities[31]], [50, 10, 10])
        self.assertEqual(report["selected"], 1)
        # a lower --priority leaves higher ones alone too
        _, priorities, _ = go(rows, priority=5)
        self.assertEqual([priorities[11], priorities[21], priorities[31]], [50, 10, 5])

    def test_idempotent(self):
        conn = database(sibling_pair(10, 1))
        journal = journal_file({})
        for expected in (1, 0):
            out = io.StringIO()
            P.run(args(journal=journal), out=out, connect_db=lambda _a: Db(conn))
            self.assertEqual(json.loads(out.getvalue())["updated"], expected)

    def test_other_apps_untouched(self):
        rows = [*sibling_pair(10, 1), *sibling_pair(20, 2, appid=2)]
        report, priorities, _ = go(rows)
        self.assertEqual((priorities[11], priorities[21]), (10, 0))
        self.assertEqual(report["selected"], 1)

    def test_dry_run_changes_nothing(self):
        report, priorities, db = go(sibling_pair(10, 1), dry_run=True)
        self.assertEqual((report["selected"], report["updated"], report["dry_run"]), (1, 0, True))
        self.assertEqual(priorities[11], 0)
        self.assertEqual(db.commits, 0)

    def test_cap_takes_the_oldest_first(self):
        rows = [row for n in range(5) for row in sibling_pair(10 * (n + 1), n + 1, sent_ago=(10 + n) * HOUR)]
        report, priorities, _ = go(rows, max=2)
        self.assertEqual((report["selected"], report["updated"]), (2, 2))
        raised = sorted(i for i in (11, 21, 31, 41, 51) if priorities[i] == 10)
        self.assertEqual(raised, [41, 51])  # the two sent longest ago
        _, priorities, _ = go(rows, max=2, dry_run=True)
        self.assertEqual(set(priorities[i] for i in (11, 21, 31, 41, 51)), {0})

    def test_negative_priority_is_never_raised(self):
        # Batches created below 0 on purpose (Xiaomi at -1) stay behind production work.
        rows = [*sibling_pair(10, 1, priority=-1), *sibling_pair(20, 2)]
        report, priorities, _ = go(rows)
        self.assertEqual((priorities[11], priorities[21]), (-1, 10))
        self.assertEqual(report["selected"], 1)

    def test_raised_backlog_is_bounded(self):
        # Two of this step's boosts are still unsent: with --max-raised 3 only one more is raised.
        rows = [*sibling_pair(10, 1, priority=10), *sibling_pair(20, 2, priority=10),
                *[row for n in range(3) for row in sibling_pair(30 + 10 * n, 3 + n)]]
        boosts = {11: {"from": 0, "first_sent": NOW - 10 * HOUR}, 21: {"from": 0, "first_sent": NOW - 10 * HOUR}}
        report, priorities, _ = go(rows, max_raised=3, journal=journal_file(boosts))
        self.assertEqual((report["already_raised"], report["selected"], report["updated"]), (2, 1, 1))
        self.assertEqual(sum(priorities[i] == 10 for i in (31, 41, 51)), 1)
        # At the bound, nothing more is raised.
        report, _, _ = go(rows, max_raised=2, journal=journal_file(boosts))
        self.assertEqual((report["selected"], report["updated"]), (0, 0))
        # Priority-10 rows this step didn't raise don't count against the bound.
        report, _, _ = go(rows, max_raised=2)
        self.assertEqual((report["already_raised"], report["selected"]), (0, 2))

    def test_gives_up_on_old_boosts_and_puts_back_only_its_own(self):
        rows = [*sibling_pair(10, 1, sent_ago=80 * HOUR, priority=10),   # this step's boost, now too old
                *sibling_pair(20, 2, sent_ago=80 * HOUR, priority=10),   # raised by someone else: left alone
                *sibling_pair(30, 3, sent_ago=10 * HOUR, priority=10),   # this step's boost, still in time
                *sibling_pair(40, 4, sent_ago=80 * HOUR, priority=10),   # this step's, then changed by hand
                *sibling_pair(50, 5, sent_ago=90 * HOUR),                # never raised, too old to start
                *sibling_pair(60, 6), *sibling_pair(70, 7)]              # dispatchable
        boosts = {11: {"from": 3, "first_sent": NOW - 80 * HOUR}, 31: {"from": 0, "first_sent": NOW - 10 * HOUR},
                  41: {"from": 0, "first_sent": NOW - 80 * HOUR}, 999: {"from": 0, "first_sent": NOW - HOUR}}
        rows = [r if r[0] != 41 else (41, 1, 4, UNSENT, 20, 0) for r in rows]
        journal = journal_file(boosts)
        report, priorities, _ = go(rows, max_raised=2, journal=journal, dry_run=True)
        self.assertEqual((report["gave_up"], report["restored"]), (1, 0))
        self.assertEqual(priorities[11], 10)
        report, priorities, _ = go(rows, max_raised=2, journal=journal)
        self.assertEqual((report["forgotten"], report["gave_up"], report["restored"], report["already_raised"]),
                         (2, 1, 1, 1))
        self.assertEqual([priorities[i] for i in (11, 21, 31, 41, 51)], [3, 10, 10, 20, 0])
        self.assertEqual(sum(priorities[i] == 10 for i in (61, 71)), 1)
        self.assertEqual(sorted(P.load_journal(journal)), [31, 61 if priorities[61] == 10 else 71])
        # A one-off catch-up of older siblings passes a larger --give-up-hours.
        _, priorities, _ = go(rows, give_up_hours=1000)
        self.assertEqual(priorities[51], 10)

    def test_a_failed_commit_keeps_the_boosts_it_meant_to_put_back(self):
        conn = database([*sibling_pair(10, 1, sent_ago=80 * HOUR, priority=10), *sibling_pair(20, 2)])
        journal = journal_file({11: {"from": 0, "first_sent": NOW - 80 * HOUR}})

        class Failing(Db):
            def commit(self):
                raise RuntimeError("commit failed")

        with self.assertRaises(RuntimeError):
            P.run(args(journal=journal), out=io.StringIO(), connect_db=lambda _a: Failing(conn))
        self.assertIn(11, P.load_journal(journal))
        out = io.StringIO()
        P.run(args(journal=journal), out=out, connect_db=lambda _a: Db(conn))
        self.assertEqual(json.loads(out.getvalue())["restored"], 1)
        self.assertEqual(sorted(P.load_journal(journal)), [21])

    def test_a_run_that_finds_another_holding_the_lock_does_nothing(self):
        conn = database(sibling_pair(10, 1))
        out = io.StringIO()
        P.run(args(), out=out, connect_db=lambda _a: Db(conn, lock_free=False))
        report = json.loads(out.getvalue())
        self.assertEqual((report["skipped"], report["selected"], report["updated"]),
                         ("another run holds the lock", 0, 0))
        self.assertEqual(dict(conn.execute("SELECT id, priority FROM result"))[11], 0)

    def test_empty_run(self):
        report, _, _ = go([])
        self.assertEqual((report["selected"], report["updated"], report["oldest_age_hours"]), (0, 0, 0.0))

    def test_missing_app_exits(self):
        conn = database([])
        conn.execute("DELETE FROM app")
        with self.assertRaises(SystemExit):
            P.run(args(), out=io.StringIO(), connect_db=lambda _a: Db(conn))

    def test_options(self):
        for bad in (["--min-age-hours", "-1"], ["--max", "0"], ["--max-raised", "0"], ["--priority", "0"],
                    ["--priority", "-1"]):
            with contextlib.redirect_stderr(io.StringIO()), self.assertRaises(SystemExit):
                P.main(bad)


if __name__ == "__main__":
    unittest.main()
