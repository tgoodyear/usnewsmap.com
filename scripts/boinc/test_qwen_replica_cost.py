import contextlib
import datetime as dt
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
SPEC = importlib.util.spec_from_file_location("replica_cost", ROOT / "scripts/boinc/qwen-replica-cost.py")
R = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(R)


def result(rid, wu, *, outcome=1, validate=0, received=0, elapsed=0.0, server_state=5, sent=1, host=1):
    return {"id": rid, "workunitid": wu, "server_state": server_state, "outcome": outcome, "validate_state": validate,
            "hostid": host, "sent_time": sent, "received_time": received, "elapsed_time": elapsed, "plan_class": "x"}


def unit(uid, language, version, *, canonical=0, error_mask=0):
    return {"id": uid, "create_time": 1, "canonical_resultid": canonical, "error_mask": error_mask, "target_nresults": 2,
            "max_total_results": 4, "language": language, "prompt_version": version, "pages": 1}


def snapshot():
    units = [
        unit(1, "srp", 1, canonical=11),
        # Valid on the first pair by result id, but the server received the
        # invalid one second: replica 3 resolved it.
        unit(2, "yid", 1, canonical=21),
        unit(3, "yid", 1, error_mask=12),
        unit(4, "yid", 1),
        unit(5, "srp", 1),
        unit(6, "srp", 1, canonical=62),
    ]
    results = [
        result(11, 1, validate=1, received=10, elapsed=100),
        result(12, 1, validate=1, received=20, elapsed=200),
        result(21, 2, validate=1, received=10, elapsed=100),
        result(22, 2, validate=1, received=30, elapsed=100),
        result(23, 2, validate=2, received=20, elapsed=100),
        *(result(30 + i, 3, validate=3, received=10 * i, elapsed=60) for i in range(1, 5)),
        result(41, 4, validate=4, received=10, elapsed=50),
        result(42, 4, validate=4, received=20, elapsed=50),
        result(43, 4, outcome=0, server_state=2, sent=0),
        result(51, 5, received=10, elapsed=40),
        result(52, 5, outcome=0, server_state=4),
        result(61, 6, outcome=3, received=5, elapsed=36),
        result(62, 6, validate=1, received=10, elapsed=10),
        result(63, 6, validate=1, received=20, elapsed=10),
    ]
    return {"snapshot": "2026-11-04T00:00:00Z", "since": "2026-10-21T00:00:00Z", "until": "2026-11-04T00:00:00Z",
            "units": units, "results": results}


class Analyse(unittest.TestCase):
    def setUp(self):
        self.rows = R.analyse(snapshot())

    def test_replica_that_resolved_follows_receipt_order(self):
        units = {u["id"]: u for u in snapshot()["units"]}
        results = [r for r in snapshot()["results"] if r["workunitid"] == 2]
        self.assertEqual(R.classify(units[2], results), ("valid", 3))

    def test_a_third_valid_return_leaves_the_resolver_unknown(self):
        # A late return that agreed after the first pair settled the unit.
        snap = snapshot()
        snap["units"].append(unit(7, "srp", 1, canonical=71))
        snap["results"] += [result(70 + i, 7, validate=1, received=10 * i, elapsed=1) for i in range(1, 4)]
        srp = R.analyse(snap)["srp v1"]
        self.assertEqual((srp["valid"], srp["resolver unknown"], srp["resolved by 3"], srp["disputed"]), (3, 1, 0, 0))

    def test_a_cancelled_unit_is_not_counted_as_capped(self):
        snap = snapshot()
        snap["units"].append(unit(7, "yid", 1, error_mask=16))
        snap["results"] += [result(71, 7, validate=4, received=10, elapsed=30), result(72, 7, validate=4, received=20, elapsed=30)]
        yid = R.analyse(snap)["yid v1"]
        self.assertEqual((yid["no consensus at cap"], yid["disputed at cap"], yid["other failure"], yid["disputed failed"]), (1, 1, 1, 1))
        self.assertAlmostEqual(yid["hours failed"] * 3600, 60)
        self.assertAlmostEqual(yid["hours wasted"] * 3600, 340)

    def test_a_pair_the_validator_has_not_read_is_not_disputed(self):
        snap = snapshot()
        snap["units"].append(unit(7, "srp", 1))
        snap["results"] += [result(71, 7, received=10, elapsed=1), result(72, 7, received=20, elapsed=1)]
        srp = R.analyse(snap)["srp v1"]
        self.assertEqual((srp["disputed"], srp["open: not disputed"]), (0, 2))

    def test_disputed_units_by_how_they_end(self):
        yid = self.rows["yid v1"]
        self.assertEqual((yid["disputed"], yid["resolved by 3"], yid["disputed at cap"], yid["waiting for replica 3"]), (3, 1, 1, 1))
        self.assertEqual((yid["replica 3 returned"], yid["replica 4 returned"], yid["resolved by 4"]), (2, 1, 0))
        self.assertEqual(self.rows["srp v1"]["disputed"], 0)

    def test_errors_and_losing_replicas_resolved_nothing(self):
        yid, srp = self.rows["yid v1"], self.rows["srp v1"]
        self.assertAlmostEqual(yid["hours wasted"] * 3600, 100 + 240)
        self.assertAlmostEqual(yid["hours needed"] * 3600, 200)
        self.assertAlmostEqual(yid["hours disputed open"] * 3600, 100)
        self.assertAlmostEqual(srp["hours wasted"] * 3600, 36)
        self.assertAlmostEqual(srp["hours pending"] * 3600, 40)
        self.assertEqual((yid["replicas wasted"], srp["replicas wasted"]), (5, 1))

    def test_replicas_per_unit(self):
        srp = self.rows["srp v1"]
        self.assertEqual((srp["units"], srp["created"], srp["sent"], srp["unsent"], srp["in progress"]), (3, 7, 7, 0, 1))
        self.assertEqual((self.rows["yid v1"]["unsent"], self.rows["all"]["units"]), (1, 6))


class Report(unittest.TestCase):
    def setUp(self):
        self.dir = ROOT / (".local-replica-cost-tests-" + uuid.uuid4().hex)
        self.dir.mkdir()
        self.addCleanup(shutil.rmtree, self.dir)

    def test_tables(self):
        path = self.dir / "snapshot.json"
        path.write_text(json.dumps(snapshot()))
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            R.main(["report", "--snapshot", str(path), "--min-units", "1"])
        text = out.getvalue()
        self.assertIn("| yid v1 | 1 | 0 | 1 | 0 | 3.00 | 7.00 |", text)
        self.assertIn("| srp v1 | 2 | 2 | 0 | 0 | 2.50 | 2.50 |", text)
        self.assertIn("| yid v1 | 3 | 2 | 1 | 1 | 0 | 1 | 0 | 1 | 0 |", text)


class FakeCursor:
    def __init__(self, log, units, results):
        self.log, self.units, self.results, self.rows, self.description = log, units, results, [], None

    def execute(self, sql, params=None):
        self.log.append((sql, params))
        if "UTC_TIMESTAMP" in sql:
            self.rows = [(dt.datetime(2026, 11, 4, 22, 0, 0),)]
        elif "FROM app" in sql:
            self.rows = [(6,)]
        elif "JOIN result" in sql:
            self.description = [(c,) for c in ("id", "workunitid", "server_state", "outcome", "validate_state", "hostid", "sent_time", "received_time", "elapsed_time", "plan_class")]
            self.rows = self.results
        elif "FROM workunit" in sql:
            self.description = [(c,) for c in ("id", "name", "create_time", "canonical_resultid", "error_mask", "target_nresults", "max_total_results")]
            self.rows = self.units

    def fetchone(self):
        return self.rows[0]

    def fetchall(self):
        return self.rows


class Collect(unittest.TestCase):
    def setUp(self):
        self.dir = ROOT / (".local-replica-cost-tests-" + uuid.uuid4().hex)
        self.addCleanup(shutil.rmtree, self.dir)
        units = self.dir / "qwen/campaign/units"
        (units / "a").mkdir(parents=True)
        (units / "a/qwen.json").write_text(json.dumps({"id": "aa", "language": "yid", "prompt_version": 1}))
        (units / "b").mkdir()
        (units / "b/qwen.json").write_text(json.dumps({"id": "bb", "language": "", "prompt_version": 4,
                                                       "pages": [{"language": "srp"}, {"language": "deu"}, {"language": None}]}))
        (units / "c").mkdir()
        (units / "c/qwen.json").write_text(json.dumps({"id": "dd", "language": "", "prompt_version": 1, "pages": [{}]}))
        (self.dir / "config.xml").write_text(
            "<boinc><config><db_host>h</db_host><db_user>u</db_user><db_passwd>p</db_passwd><db_name>n</db_name></config></boinc>")
        self.log, self.closed = [], []

    def run_collect(self, units, results, limit="10"):
        log, closed = self.log, self.closed

        class Conn:
            def cursor(self):
                return FakeCursor(log, units, results)

            def rollback(self):
                log.append(("ROLLBACK", None))

            def close(self):
                closed.append(True)

        fake = types.ModuleType("MySQLdb")
        fake.connect = lambda **kw: Conn()
        out = io.StringIO()
        with mock.patch.dict(sys.modules, {"MySQLdb": fake}), contextlib.redirect_stdout(out):
            R.main(["collect", "--since", "2026-10-21T00:00:00Z", "--until", "2026-11-04T00:00:00Z", "--config", str(self.dir / "config.xml"),
                    "--qwen-root", str(self.dir / "qwen"), "--download", str(self.dir / "download"), "--limit", limit])
        return json.loads(out.getvalue())

    def test_reads_in_one_read_only_transaction_and_joins_inputs(self):
        units = [(1, "usnm_qwen_aa", 5, 0, 0, 2, 4), (2, "usnm_qwen_bb", 6, 0, 0, 2, 4), (3, "usnm_qwen_cc", 7, 0, 0, 2, 4),
                 (4, "usnm_qwen_dd", 8, 0, 0, 2, 4)]
        results = [(9, 1, 5, 1, 0, 1, 1, 2, 30.0, "x")]
        snap = self.run_collect(units, results)
        sql = [s for s, _ in self.log]
        since, until = (int(dt.datetime(2026, m, d, tzinfo=dt.timezone.utc).timestamp()) for m, d in ((10, 21), (11, 4)))
        query, params = next((s, p) for s, p in self.log if "FROM workunit w JOIN result" in s)
        self.assertIn("w.create_time>=%s AND w.create_time<%s", query)
        self.assertEqual(params, (6, since, until, 1, 4, 80))
        self.assertLess(sql.index("START TRANSACTION READ ONLY"), min(i for i, s in enumerate(sql) if "FROM workunit" in s))
        self.assertEqual(sql[-1], "ROLLBACK")
        self.assertEqual(self.closed, [True])
        self.assertTrue(all("LIMIT" in s for s in sql if "FROM workunit" in s))
        self.assertFalse([s for s in sql if s.split()[0].upper() in ("INSERT", "UPDATE", "DELETE", "REPLACE")])
        langs = {u["id"]: (u["language"], u["prompt_version"], u["pages"]) for u in snap["units"]}
        self.assertEqual(langs, {1: ("yid", 1, 1), 2: ("deu+srp", 4, 3), 3: (None, None, None), 4: (None, 1, 1)})
        self.assertEqual([r["id"] for r in snap["results"]], [9])
        self.assertEqual((snap["snapshot"], snap["since"]), ("2026-11-04T22:00:00Z", "2026-10-21T00:00:00Z"))

    def test_refuses_a_window_at_the_limit_and_still_rolls_back(self):
        with self.assertRaises(SystemExit):
            self.run_collect([(1, "usnm_qwen_aa", 5, 0, 0, 2, 4)], [], limit="1")
        with self.assertRaises(SystemExit):
            self.run_collect([(1, "usnm_qwen_aa", 5, 0, 0, 2, 4)], [(i, 1, 5, 1, 0, 1, 1, 2, 3.0, "x") for i in range(16)], limit="2")
        self.assertEqual([s for s, _ in self.log].count("ROLLBACK"), 2)
        self.assertEqual(self.closed, [True, True])


if __name__ == "__main__":
    unittest.main()
