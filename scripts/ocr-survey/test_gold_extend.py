"""scripts/ocr-survey/gold_extend.py: the issue order, the English filter, the issue rows (#968)."""

import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import gold_extend as ge  # noqa: E402


def issues(n: int, lccn: str = "sn1", start: int = 1850) -> list[dict]:
    return [{"lang": "pol", "lccn": lccn, "date": f"{start + i * 3}-01-01", "edition": 1, "title": "t"} for i in range(n)]


class Order(unittest.TestCase):
    def test_seeded_and_never_a_frozen_issue(self):
        pool = issues(40) + issues(40, "sn2")
        taken = [{"lccn": "sn1", "date": "1850-01-01", "edition": 1}]
        a = ge.order_issues(pool, taken, 20, 296, "pol")
        self.assertEqual(a, ge.order_issues(pool, taken, 20, 296, "pol"))
        self.assertNotEqual(a, ge.order_issues(pool, taken, 20, 297, "pol"))
        self.assertNotIn(("sn1", "1850-01-01"), {(i["lccn"], i["date"]) for i in a})
        self.assertEqual(len({(i["lccn"], i["date"]) for i in a}), 20)

    def test_spreads_over_decades_and_titles(self):
        pool = issues(60) + issues(60, "sn2")
        a = ge.order_issues(pool, [], 12, 296, "pol")
        self.assertEqual({i["lccn"] for i in a}, {"sn1", "sn2"})
        self.assertGreaterEqual(len({i["date"][:3] for i in a}), 12)

    def test_stops_when_the_pool_runs_out(self):
        self.assertEqual(len(ge.order_issues(issues(3), [], 10, 1, "pol")), 3)


class English(unittest.TestCase):
    def test_share(self):
        self.assertGreater(ge.english_share("the man and the dog went to the town"), ge.ENGLISH_SHARE)
        self.assertLess(ge.english_share("Dziennik Chicagoski wychodzi codziennie w Chicago"), ge.ENGLISH_SHARE)
        self.assertEqual(ge.english_share(""), 0.0)


class IssueRow(unittest.TestCase):
    def test_row_and_missing_fields(self):
        r = {"number_lccn": ["sn83045348"], "number_edition": ["1"], "date": "1892-10-21", "title": "T",
             "partof_title": ["p"], "language": ["czech", "english"], "digitized": True}
        self.assertEqual(ge.issue_row(r, "cze")["languages"], ["czech", "english"])
        self.assertIsNone(ge.issue_row({**r, "date": "189?"}, "cze"))
        self.assertIsNone(ge.issue_row({"date": "1892-10-21"}, "cze"))


if __name__ == "__main__":
    unittest.main()
