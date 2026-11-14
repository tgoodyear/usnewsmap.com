import importlib.util
import io
import json
from pathlib import Path
import tempfile
import types
import unittest

ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location("native_stranded", ROOT / "boinc/scripts/native-stranded.py")
S = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(S)

NOW = 1_800_000_000
JOB = ["%064x" % n for n in range(1, 6)]


class Cursor:
    def __init__(self, rows):
        self.rows = rows
        self.sql = []

    def execute(self, sql, params=None):
        self.sql.append(sql)

    def fetchall(self):
        return self.rows

    def fetchone(self):
        return (NOW,)


class Db:
    def __init__(self, rows):
        self.cur = Cursor(rows)

    def cursor(self):
        return self.cur


def row(n, results, exit_status, sigxcpu, age=100000, host=13):
    return (n, "usnm_" + JOB[n - 1], NOW - age, host, results, exit_status, sigxcpu)


class StrandedTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.dir = Path(self.tmp.name)
        for job in JOB:
            (self.dir / "jobs" / job).mkdir(parents=True)
            (self.dir / "jobs" / job / "record.json").write_text(json.dumps({
                "stage": "submitted", "plan": "native-feed-4",
                "job": {"attempt": 0, "message": {"page": "sn1/1870-09-27/ed-1/seq-" + job[-1]},
                        "engine": {"image": "sha256:" + "4" * 64, "timeout_secs": 1800}}}))

    def args(self, command, rows_db, **extra):
        values = dict(command=command, out=str(self.dir / "list.json"), stale_hours=6.0,
                      handoff=str(self.dir), project="/p", config="", db_ca="", db_defaults="",
                      apply=False, max_units=None, confirm_count=None, journal=None)
        values.update(extra)
        return types.SimpleNamespace(**values), (lambda args: rows_db)

    def test_each_case_is_classified_and_a_young_unit_with_no_result_is_not_stranded(self):
        stale = 6 * 3600
        self.assertEqual(S.classify(1, 1, True, 0, stale), "too_slow_sigxcpu")
        self.assertEqual(S.classify(1, 117, False, 0, stale), "too_slow_sigxcpu")
        self.assertEqual(S.classify(1, -226, False, 0, stale), "exit0_loop")
        self.assertEqual(S.classify(1, 1, False, 0, stale), "client_error")
        self.assertEqual(S.classify(0, None, False, stale, stale), "assigned_no_result")
        self.assertIsNone(S.classify(0, None, False, stale - 1, stale))

    def test_a_name_without_a_job_sha_is_never_listed(self):
        self.assertEqual(S.job_of("usnm_" + JOB[0]), JOB[0])
        for name in ("usnm_ocrb_" + JOB[0], "usnm_zz", JOB[0], "usnm_" + "A" * 64):
            self.assertIsNone(S.job_of(name))

    def test_the_plan_counts_all_three_cases_and_writes_the_page_with_its_job_image_and_timeout(self):
        rows = [row(1, 1, 1, 1), row(2, 1, -226, 0, host=16), row(3, 0, None, 0, host=15),
                row(4, 0, None, 0, age=60)]
        args, connect = self.args("plan", Db(rows))
        out = io.StringIO()
        self.assertEqual(S.run(args, out, connect), 0)
        report = json.loads(out.getvalue())
        self.assertEqual(report["candidates"]["total"], 3)
        self.assertEqual({k: v for k, v in report["candidates"]["by_reason"].items() if v},
                         {"too_slow_sigxcpu": 1, "exit0_loop": 1, "assigned_no_result": 1})
        pages = json.loads(Path(args.out).read_text())["pages"]
        entry = pages["usnm_" + JOB[0]]
        self.assertEqual((entry["job"], entry["image"], entry["timeout_secs"], entry["host"], entry["reason"]),
                         (JOB[0], "sha256:" + "4" * 64, 1800, 13, "too_slow_sigxcpu"))
        self.assertTrue(entry["page"].startswith("sn1/"))
        # The count command reads only the file.
        out = io.StringIO()
        S.run(types.SimpleNamespace(command="count", out=args.out), out)
        self.assertEqual(json.loads(out.getvalue())["total"], 3)

    def test_the_list_is_never_shrunk_by_a_later_run(self):
        args, connect = self.args("plan", Db([row(1, 1, 1, 1), row(2, 1, -226, 0)]))
        S.run(args, io.StringIO(), connect)
        args, connect = self.args("plan", Db([row(1, 1, 1, 1)]))
        S.run(args, io.StringIO(), connect)
        self.assertEqual(S.counts(S.load_list(args.out))["total"], 2)

    def test_a_missing_journal_still_lists_the_page_by_its_job(self):
        (self.dir / "jobs" / JOB[0] / "record.json").unlink()
        args, connect = self.args("plan", Db([row(1, 1, 1, 1)]))
        S.run(args, io.StringIO(), connect)
        entry = S.load_list(args.out)["usnm_" + JOB[0]]
        self.assertEqual((entry["job"], entry["image"], entry["timeout_secs"]), (JOB[0], None, None))

    def test_apply_needs_the_confirmed_count_and_lists_each_page_before_cancelling_it(self):
        rows = [row(1, 1, 1, 1), row(2, 1, -226, 0), row(3, 0, None, 0)]
        calls = []

        def canceller(project, name):
            # The page is already on disk when its unit is cancelled.
            self.assertIn(name, S.load_list(str(self.dir / "list.json")))
            calls.append(name)
            return True, "ok"

        journal = str(self.dir / "journal.jsonl")
        for extra in ({}, {"max_units": 1}, {"max_units": 1, "confirm_count": 2, "journal": journal}):
            args, connect = self.args("settle", Db(rows), apply=True, **extra)
            with self.assertRaises(SystemExit):
                S.run(args, io.StringIO(), connect, canceller)
        self.assertEqual(calls, [])
        args, connect = self.args("settle", Db(rows), apply=True, max_units=1, confirm_count=3, journal=journal)
        self.assertEqual(S.run(args, io.StringIO(), connect, canceller), 0)
        self.assertEqual(calls, ["usnm_" + JOB[0]])
        counts = S.counts(S.load_list(args.out))
        self.assertEqual((counts["total"], counts["settled"]), (3, 1))
        self.assertEqual(len(Path(journal).read_text().splitlines()), 1)

    def test_a_failed_cancel_stops_the_run_and_leaves_the_page_listed_unsettled(self):
        rows = [row(1, 1, 1, 1), row(2, 1, -226, 0)]
        args, connect = self.args("settle", Db(rows), apply=True, max_units=2, confirm_count=2,
                                  journal=str(self.dir / "j.jsonl"))
        self.assertEqual(S.run(args, io.StringIO(), connect, lambda p, n: (False, "boom")), 1)
        counts = S.counts(S.load_list(args.out))
        self.assertEqual((counts["total"], counts["settled"]), (2, 0))

    def test_a_preview_changes_no_unit(self):
        args, connect = self.args("settle", Db([row(1, 1, 1, 1)]))
        S.run(args, io.StringIO(), connect, lambda p, n: self.fail("cancelled in a preview"))

    def test_a_list_file_that_is_not_ours_is_not_overwritten(self):
        path = self.dir / "list.json"
        path.write_text('{"something": 1}')
        with self.assertRaises(SystemExit):
            S.load_list(str(path))


if __name__ == "__main__":
    unittest.main()
