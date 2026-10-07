"""scripts/check-version-snapshots.py: the schema and the checks it can't express."""

import copy
import importlib.util
import json
import tempfile
import unittest
import unittest.mock
from pathlib import Path

HERE = Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location("check", HERE / "check-version-snapshots.py")
check = importlib.util.module_from_spec(spec)
spec.loader.exec_module(check)

GOOD = {
    "base": "https://usnewsmap.com",
    "captured_at": "2026-10-07T06:20:28Z",
    "index_version": "pages-v20261006-2",
    "searches": {
        "bench: lincoln": {
            "query": "q=lincoln&bucket=year", "status": 200, "wall_s": 12.1,
            "total": {"hits": 5, "places": 2, "papers": 2, "days": 4, "baseline_pages": 900,
                      "first_day": 58000, "last_day": 59000},
            "bucket": {"count": 3, "from": "1860-01-01", "to": "1862-12-31", "unit": "year"},
            "series_hits": [1, 0, 4], "languages": {"eng": 5}, "timing_ms": {"backend": 900, "total": 910},
        },
        "ja: 日本": {
            "query": "q=%E6%97%A5%E6%9C%AC", "status": 422, "wall_s": 0.2,
            "error": {"type": "/errors/unsupported", "title": "Not supported"},
        },
        "example: slow": {"query": "q=slow", "status": "gave up", "wall_s": 150.0},
    },
}


class Check(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.validate = staticmethod(check.fastjsonschema.compile(json.loads((check.DIR / "schema.json").read_text())))

    def problems(self, doc, name="pages-v20261006-2.json"):
        with tempfile.TemporaryDirectory() as d:
            path = Path(d) / name
            path.write_text(json.dumps(doc, ensure_ascii=False))
            return check.problems(path, self.validate)

    def broken(self, change):
        doc = copy.deepcopy(GOOD)
        change(doc)
        return self.problems(doc)

    def test_a_good_snapshot_passes(self):
        self.assertEqual(self.problems(GOOD), [])

    def test_the_committed_snapshots_pass(self):
        for path in sorted(check.DIR.glob("pages-*.json")):
            self.assertEqual(check.problems(path, self.validate), [], path.name)

    def test_schema_violations(self):
        def lincoln(doc):
            return doc["searches"]["bench: lincoln"]
        for why, change in [
            ("no searches", lambda d: d.update(searches={})),
            ("unknown top-level field", lambda d: d.update(extra=1)),
            ("bad version name", lambda d: d.update(index_version="v1")),
            ("bad capture time", lambda d: d.update(captured_at="2026-10-07 06:20")),
            ("bad search name", lambda d: d["searches"].update({"lincoln": lincoln(d)})),
            ("status not a number", lambda d: lincoln(d).update(status="200")),
            ("200 without its total", lambda d: lincoln(d).pop("total")),
            ("200 with an error", lambda d: lincoln(d).update(error={"title": "x"})),
            ("an error with results", lambda d: d["searches"]["ja: 日本"].update(series_hits=[1])),
            ("negative hits", lambda d: lincoln(d)["total"].update(hits=-1)),
            ("unknown bucket unit", lambda d: lincoln(d)["bucket"].update(unit="decade")),
            ("timing without backend", lambda d: lincoln(d)["timing_ms"].pop("backend")),
            ("unknown search field", lambda d: lincoln(d).update(note="x")),
            ("gave up with an error", lambda d: d["searches"]["example: slow"].update(error={"title": "x"})),
            ("gave up with results", lambda d: d["searches"]["example: slow"].update(total={"hits": 1})),
        ]:
            got = self.broken(change)
            self.assertEqual(len(got), 1, why)
            self.assertTrue(got[0].startswith("schema: "), (why, got))

    def test_what_the_schema_cant_say(self):
        self.assertEqual(self.problems(GOOD, "pages-v20261003-1.json"),
                         ["named pages-v20261003-1.json but captures pages-v20261006-2"])
        got = self.broken(lambda d: d["searches"]["bench: lincoln"].update(series_hits=[1, 4]))
        self.assertEqual(got, ["bench: lincoln: 2 series values for 3 buckets"])
        got = self.broken(lambda d: d["searches"]["bench: lincoln"].update(series_hits=[1, 1, 4]))
        self.assertEqual(got, ["bench: lincoln: series adds up to 6, total says 5"])

    def test_a_language_with_no_code_keeps_its_loc_name(self):
        got = self.broken(lambda d: d["searches"]["bench: lincoln"]["languages"].update({"pennsylvania german": 1}))
        self.assertEqual(got, [])
        got = self.broken(lambda d: d["searches"]["bench: lincoln"]["languages"].update({" eng": 1}))
        self.assertTrue(got and got[0].startswith("schema: "), got)

    def test_a_non_200_without_a_problem_document_passes(self):
        # A 502 with an HTML body: the capture records no error.
        self.assertEqual(self.broken(lambda d: d["searches"]["ja: 日本"].pop("error")), [])

    def test_every_json_file_is_checked_whatever_its_name(self):
        with tempfile.TemporaryDirectory() as d, unittest.mock.patch.object(check, "DIR", Path(d)):
            Path(d, "schema.json").write_text((HERE.parent / "ops/version-snapshots/schema.json").read_text())
            Path(d, "snapshot.json").write_text(json.dumps(GOOD))
            with unittest.mock.patch.object(check.sys, "argv", ["check"]):
                self.assertEqual(check.main(), 1)  # named snapshot.json, captures pages-v20261006-2

    def test_not_json(self):
        with tempfile.TemporaryDirectory() as d:
            path = Path(d) / "pages-v20261006-2.json"
            path.write_text("{")
            self.assertTrue(check.problems(path, self.validate)[0].startswith("not JSON"))


if __name__ == "__main__":
    unittest.main()
