import contextlib
import copy
import importlib.util
import io
import json
from pathlib import Path
import shutil
import sys
import types
import unittest
from unittest import mock
import uuid

ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location("tiebreak_policy", ROOT / "scripts/boinc/qwen-tiebreak-policy.py")
P = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(P)

NOW = 1_800_000_000
LATER = NOW + 86400


def unit(uid, language="yid", version=1, *, canonical=0, error_mask=0, need_validate=0, transition_time=LATER, max_total=4):
    return {"id": uid, "name": f"usnm_qwen_job{uid}", "batch": 8, "canonical_resultid": canonical, "error_mask": error_mask,
            "need_validate": need_validate, "transition_time": transition_time, "max_total_results": max_total,
            "language": language, "prompt_version": version}


def result(rid, wu, *, server_state=5, outcome=1, validate=4, elapsed=360.0):
    return {"id": rid, "workunitid": wu, "server_state": server_state, "outcome": outcome, "validate_state": validate,
            "elapsed_time": elapsed}


def unsent(rid, wu):
    return result(rid, wu, server_state=2, outcome=0, validate=0, elapsed=0.0)


def disputed(uid, language="yid", version=1, **kw):
    """A unit whose first pair disagreed and whose tie-breaker is unsent."""
    return unit(uid, language, version, **kw), [result(10 * uid + 1, uid), result(10 * uid + 2, uid), unsent(10 * uid + 3, uid)]


def window():
    units, results = [], []
    for u, rs in (
        disputed(1),
        # Replica 3 disagreed with both; replica 4 unsent.
        (unit(2), [result(21, 2), result(22, 2), result(23, 2), unsent(24, 2)]),
        # The tie-breaker is running.
        (unit(3), [result(31, 3), result(32, 3), result(33, 3, server_state=4, outcome=0, validate=0)]),
        # Replica 3 returned and the validator hasn't read it.
        (unit(4, need_validate=1), [result(41, 4), result(42, 4), result(43, 4, validate=0)]),
        disputed(5, transition_time=NOW),
        # One result failed the check: a replacement, not a tie-breaker.
        (unit(6), [result(61, 6, validate=2), result(62, 6), unsent(63, 6)]),
        disputed(7, error_mask=8),
        disputed(8, "yid", 4),
        disputed(9, "srp"),
        (unit(10, canonical=101), [result(101, 10, validate=1), result(102, 10, validate=1)]),
    ):
        units.append(u)
        results += rs
    return units, results


class Select(unittest.TestCase):
    def test_no_policy_selects_nothing(self):
        units, results = window()
        groups, chosen = P.select(NOW, units, results, [])
        self.assertEqual(chosen, [])
        self.assertEqual(groups["yid v1"]["disputed"], 5)
        self.assertNotIn("settle", groups["yid v1"])
        self.assertFalse(groups["yid v1"]["in policy"])

    def test_policy_settles_only_its_disputed_units_with_nothing_in_flight(self):
        units, results = window()
        groups, chosen = P.select(NOW, units, results, [("yid", 1)])
        self.assertEqual([u["id"] for u in chosen], [1, 2])
        yid = groups["yid v1"]
        self.assertEqual((yid["open"], yid["disputed"], yid["settle"], yid["settle, batch 8"]), (6, 5, 2, 2))
        self.assertEqual((yid["waiting: replica running"], yid["waiting: awaiting validation"], yid["waiting: due for the transitioner"]),
                         (1, 1, 1))
        self.assertEqual((yid["unsent replicas cancelled"], yid["most replicas forgone"]), (2, 3))
        self.assertAlmostEqual(yid["mean hours per return"], 0.1)
        self.assertAlmostEqual(yid["GPU-hours forgone, low"], 0.2)
        self.assertAlmostEqual(yid["GPU-hours forgone, high"], 0.3)
        self.assertEqual((groups["yid v4"]["disputed"], groups["yid v4"].get("settle", 0), groups["srp v1"].get("settle", 0)), (1, 0, 0))
        self.assertFalse(groups["yid v4"]["in policy"])

    def test_a_bundle_label_matches_exactly(self):
        u, rs = disputed(1, "heb+yid", 4)
        self.assertEqual(P.select(NOW, [u], rs, [("yid", 4)])[1], [])
        self.assertEqual(len(P.select(NOW, [u], rs, [("heb+yid", 4)])[1]), 1)


class Policy(unittest.TestCase):
    def setUp(self):
        self.dir = ROOT / (".local-tiebreak-tests-" + uuid.uuid4().hex)
        self.dir.mkdir()
        self.addCleanup(shutil.rmtree, self.dir)

    def args(self, flags=(), path=None):
        return types.SimpleNamespace(stop_tiebreakers=list(flags), policy_file=path)

    def write(self, value):
        path = self.dir / "policy.json"
        path.write_text(json.dumps(value))
        return str(path)

    def test_file_flags_and_missing_default(self):
        path = self.write({"schema": 1, "stop_tiebreakers": [{"language": "yid", "request_version": 1}]})
        self.assertEqual(P.resolve_policy(self.args(path=path)), [("yid", 1)])
        self.assertEqual(P.resolve_policy(self.args([("yid", 4), ("yid", 1), ("yid", 4)])), [("yid", 1), ("yid", 4)])
        with mock.patch.object(P, "DEFAULT_POLICY", str(self.dir / "absent.json")):
            self.assertEqual(P.resolve_policy(self.args()), [])
        with self.assertRaises(SystemExit):
            P.resolve_policy(self.args([("yid", 1)], path))
        with self.assertRaises(OSError):
            P.resolve_policy(self.args(path=str(self.dir / "absent.json")))

    def test_rejects_malformed_policies(self):
        for bad in ({"schema": 2, "stop_tiebreakers": []}, {"stop_tiebreakers": []},
                    {"schema": 1, "stop_tiebreakers": [{"language": "YID", "request_version": 1}]},
                    {"schema": 1, "stop_tiebreakers": [{"language": "yid", "request_version": True}]},
                    {"schema": 1, "stop_tiebreakers": [{"language": "yid", "request_version": 1, "also": 1}]}):
            with self.assertRaises(ValueError):
                P.read_policy(self.write(bad))
        for bad in ("yid", "yid:0", "yid:v1", "Yid:1"):
            with self.assertRaises(Exception):
                P.group_arg(bad)


class FakeDB:
    """Enough of MySQLdb for the script's statements, with transactions."""

    def __init__(self, units, results):
        self.units = {u["id"]: dict(u) for u in units}
        self.results = {r["id"]: dict(r) for r in results}
        self.log, self.saved, self.commits, self.on_lock, self.force_rowcount = [], None, 0, None, None
        self.mariadb = False

    def connect(self, **kw):
        db = self

        class Cursor:
            description, rows, rowcount = None, [], 0

            def execute(self, sql, params=None):
                db.log.append((sql, params))
                self.rows = db.run(self, sql, params)

            def fetchone(self):
                return self.rows[0]

            def fetchall(self):
                return self.rows

        class Conn:
            def cursor(self):
                return Cursor()

            def commit(self):
                db.saved, db.commits = None, db.commits + 1

            def rollback(self):
                db.log.append(("ROLLBACK", None))
                if db.saved:
                    db.units, db.results = db.saved
                db.saved = None

            def close(self):
                pass

        return Conn()

    def run(self, cur, sql, params):
        wu_cols = ("id", "name", "batch", "canonical_resultid", "error_mask", "need_validate", "transition_time", "max_total_results")
        if sql == "START TRANSACTION":
            self.saved = copy.deepcopy((self.units, self.results))
        elif "MAX_EXECUTION_TIME" in sql and self.mariadb:
            raise Exception(1193, "Unknown system variable 'MAX_EXECUTION_TIME'")
        elif sql == "SELECT UNIX_TIMESTAMP()":
            return [(NOW,)]
        elif "FROM app" in sql:
            return [(6,)]
        elif "FOR UPDATE" in sql and "FROM workunit" in sql:
            if self.on_lock:
                self.on_lock(self, params[0])
            u = self.units[params[0]]
            return [tuple(u[c] for c in wu_cols if c not in ("name", "batch")) + (NOW,)]
        elif "FOR UPDATE" in sql:
            return [(r["id"], r["server_state"], r["outcome"], r["validate_state"]) for r in sorted(self.results.values(), key=lambda r: r["id"])
                    if r["workunitid"] == params[0]]
        elif "JOIN result" in sql:
            cols = ("id", "workunitid", "server_state", "outcome", "validate_state", "elapsed_time")
            cur.description = [(c,) for c in cols]
            return [tuple(r[c] for c in cols) for r in self.results.values()]
        elif "FROM workunit" in sql:
            cur.description = [(c,) for c in wu_cols]
            return [tuple(u[c] for c in wu_cols) for u in self.units.values()]
        elif sql.startswith("UPDATE result"):
            state, outcome, wu, *old = params
            hit = [r for r in self.results.values() if r["workunitid"] == wu and r["server_state"] in old]
            for r in hit:
                r["server_state"], r["outcome"] = state, outcome
            cur.rowcount = len(hit) if self.force_rowcount is None else self.force_rowcount
        elif sql.startswith("UPDATE workunit"):
            total, bit, wu, old = params
            u = self.units[wu]
            ok = not u["canonical_resultid"] and not u["error_mask"] and not u["need_validate"] and u["max_total_results"] == old
            if ok:
                u.update(max_total_results=total, error_mask=u["error_mask"] | bit, transition_time=NOW)
            cur.rowcount = int(ok)
        return []


class Settle(unittest.TestCase):
    def setUp(self):
        self.dir = ROOT / (".local-tiebreak-tests-" + uuid.uuid4().hex)
        self.addCleanup(shutil.rmtree, self.dir)
        units = self.dir / "qwen/campaign/units"
        units.mkdir(parents=True)
        for u in window()[0]:
            (units / str(u["id"])).mkdir()
            (units / str(u["id"]) / "qwen.json").write_text(
                json.dumps({"id": f"job{u['id']}", "language": u["language"], "prompt_version": u["prompt_version"]}))
        (self.dir / "config.xml").write_text(
            "<boinc><config><db_host>h</db_host><db_user>u</db_user><db_passwd>p</db_passwd><db_name>n</db_name></config></boinc>")
        self.journal = self.dir / "journal.jsonl"
        units, results = window()
        for u in units:
            del u["language"], u["prompt_version"]
        self.db = FakeDB(units, results)

    def run_main(self, *argv):
        fake = types.ModuleType("MySQLdb")
        fake.connect = self.db.connect
        out = io.StringIO()
        base = ["--since", "2026-10-21T00:00:00Z", "--until", "2026-11-06T00:00:00Z", "--config", str(self.dir / "config.xml"),
                "--qwen-root", str(self.dir / "qwen"), "--download", str(self.dir / "download")]
        with mock.patch.dict(sys.modules, {"MySQLdb": fake}), mock.patch.object(P, "DEFAULT_POLICY", str(self.dir / "absent.json")), \
                contextlib.redirect_stdout(out):
            P.main([argv[0], *base, *argv[1:]])
        return json.loads(out.getvalue())

    def writes(self):
        return [s for s, _ in self.db.log if s.split()[0] == "UPDATE"]

    def events(self):
        return [json.loads(line) for line in self.journal.read_text().splitlines()]

    def test_plan_reads_only(self):
        report = self.run_main("plan", "--stop-tiebreakers", "yid:1", "--list-units")
        self.assertEqual((report["settle"], report["policy"], report["units read"]), ([1, 2], ["yid:1"], 10))
        self.assertEqual(self.writes(), [])
        sql = [s for s, _ in self.db.log]
        self.assertLess(sql.index("START TRANSACTION READ ONLY"), min(i for i, s in enumerate(sql) if "FROM workunit" in s))
        self.assertEqual(sql[-1], "ROLLBACK")

    def test_plan_bounds_statements_on_mariadb_too(self):
        self.db.mariadb = True
        self.assertEqual(self.run_main("plan", "--stop-tiebreakers", "yid:1", "--list-units")["settle"], [1, 2])
        self.assertIn("SET SESSION max_statement_time=60", [s for s, _ in self.db.log])

    def test_without_a_policy_plan_selects_nothing_and_settle_refuses(self):
        self.assertEqual(self.run_main("plan", "--list-units")["settle"], [])
        with self.assertRaises(SystemExit):
            self.run_main("settle", "--apply", "--max-units", "5", "--journal", str(self.journal))
        self.assertEqual(self.writes(), [])

    def test_preview_changes_nothing(self):
        report = self.run_main("settle", "--stop-tiebreakers", "yid:1")
        self.assertEqual(report["would settle"], [{"workunit": 1, "group": "yid v1", "cancel": [13]},
                                                  {"workunit": 2, "group": "yid v1", "cancel": [24]}])
        self.assertEqual(self.writes(), [])
        for flags in (["--max-units", "5"], ["--journal", str(self.journal)], ["--max-units", "0", "--journal", str(self.journal)]):
            with self.assertRaises(SystemExit), contextlib.redirect_stderr(io.StringIO()):
                self.run_main("settle", "--stop-tiebreakers", "yid:1", "--apply", *flags)
        self.assertFalse(self.journal.exists())

    def test_apply_settles_under_row_locks_and_journals(self):
        report = self.run_main("settle", "--stop-tiebreakers", "yid:1", "--apply", "--max-units", "1", "--journal", str(self.journal))
        self.assertEqual(report["outcomes"], {"settled": 1})
        self.assertEqual((self.db.results[13]["server_state"], self.db.results[13]["outcome"]), (5, 5))
        self.assertEqual({k: self.db.units[1][k] for k in ("error_mask", "max_total_results", "transition_time")},
                         {"error_mask": 8, "max_total_results": 3, "transition_time": NOW})
        self.assertEqual((self.db.units[2]["error_mask"], self.db.results[24]["server_state"]), (0, 2))
        self.assertEqual(self.db.commits, 1)
        update_wu = next((s, p) for s, p in self.db.log if s.startswith("UPDATE workunit"))
        self.assertIn("canonical_resultid=0 AND error_mask=0 AND need_validate=0 AND max_total_results=%s", update_wu[0])
        sql = [s for s, _ in self.db.log]
        locks = [i for i, s in enumerate(sql) if "FOR UPDATE" in s]
        self.assertTrue(locks and max(locks) < min(i for i, s in enumerate(sql) if s.startswith("UPDATE")))
        events = self.events()
        self.assertEqual([e["event"] for e in events], ["run", "begin", "settled", "done"])
        self.assertEqual(events[1]["before"]["results"], [[11, 5, 1, 4], [12, 5, 1, 4], [13, 2, 0, 0]])
        self.assertEqual((events[1]["policy"], events[2]["cancelled"]), (["yid:1"], [13]))

    def test_a_replica_sent_before_the_lock_leaves_the_unit_alone(self):
        def sent(db, wu):
            if wu == 1:
                db.results[13]["server_state"] = 4

        self.db.on_lock = sent
        report = self.run_main("settle", "--stop-tiebreakers", "yid:1", "--apply", "--max-units", "5", "--journal", str(self.journal))
        self.assertEqual(report["outcomes"], {"replica running": 1, "settled": 1})
        self.assertEqual((self.db.units[1]["error_mask"], self.db.units[2]["error_mask"]), (0, 8))
        self.assertEqual([e.get("reason") for e in self.events() if e["event"] == "skipped"], ["replica running"])

    def test_a_count_that_does_not_match_rolls_back_and_stops(self):
        self.db.force_rowcount = 2
        with self.assertRaises(RuntimeError):
            self.run_main("settle", "--stop-tiebreakers", "yid:1", "--apply", "--max-units", "5", "--journal", str(self.journal))
        self.assertEqual((self.db.units[1]["error_mask"], self.db.results[13]["server_state"], self.db.commits), (0, 2, 0))
        self.assertEqual([e["event"] for e in self.events()], ["run", "begin", "failed"])

    def test_a_lock_wait_skips_the_unit(self):
        def locked(db, wu):
            if wu == 1:
                raise Exception(1205, "Lock wait timeout exceeded")

        self.db.on_lock = locked
        report = self.run_main("settle", "--stop-tiebreakers", "yid:1", "--apply", "--max-units", "5", "--journal", str(self.journal))
        self.assertEqual(report["outcomes"], {"locked": 1, "settled": 1})
        self.assertEqual(self.db.units[1]["error_mask"], 0)


if __name__ == "__main__":
    unittest.main()
