"""Tests for the OCR prefetch manifest builder (#1055): the parts with no network or pyarrow."""
import datetime as dt
import importlib.util
import unittest
from pathlib import Path

spec = importlib.util.spec_from_file_location("ocr_manifest", Path(__file__).resolve().parent / "ocr_manifest.py")
m = importlib.util.module_from_spec(spec)
spec.loader.exec_module(m)
UTC = dt.timezone.utc


def q(doc, reason, hour, batch="b"):
    return {"doc_id": doc, "batch": batch, "batch_version": 1, "reason": reason, "language": "eng",
            "queued_at": dt.datetime(2026, 10, 24, hour, tzinfo=UTC)}


class PendingTest(unittest.TestCase):
    def test_users_first_and_finished_pages_left_out(self):
        queued = [q("sn1_1900-01-01_ed-1_seq-1", "bulk", 1), q("sn1_1900-01-01_ed-1_seq-2", "user", 5),
                  q("sn1_1900-01-01_ed-1_seq-3", "user", 2), q("sn1_1900-01-01_ed-1_seq-4", "user", 1)]
        outcomes = [{"doc_id": "sn1_1900-01-01_ed-1_seq-4", "outcome": "imported",
                     "at": dt.datetime(2026, 10, 24, 3, tzinfo=UTC)}]
        got = [x["doc_id"][-5:] for x in m.pending(queued, outcomes, ["user", "bulk"])]
        self.assertEqual(got, ["seq-3", "seq-2", "seq-1"])

    def test_a_page_queued_again_after_its_outcome_counts(self):
        queued = [q("sn1_1900-01-01_ed-1_seq-1", "user", 1), q("sn1_1900-01-01_ed-1_seq-1", "user", 6)]
        outcomes = [{"doc_id": "sn1_1900-01-01_ed-1_seq-1", "outcome": "imported",
                     "at": dt.datetime(2026, 10, 24, 3, tzinfo=UTC)}]
        self.assertEqual(len(m.pending(queued, outcomes, ["user"])), 1)

    def test_later_outcomes_end_a_page_but_an_unroutable_dead_letter_does_not(self):
        at = dt.datetime(2026, 10, 24, 3, tzinfo=UTC)
        queued = [q("sn1_1900-01-01_ed-1_seq-1", "user", 1), q("sn1_1900-01-01_ed-1_seq-2", "user", 1),
                  q("sn1_1900-01-01_ed-1_seq-3", "user", 1)]
        outcomes = [
            {"doc_id": "sn1_1900-01-01_ed-1_seq-1", "outcome": "not_sent", "cause": None, "at": at},
            {"doc_id": "sn1_1900-01-01_ed-1_seq-2", "outcome": "dead_lettered", "cause": "max_dequeues", "at": at},
            {"doc_id": "sn1_1900-01-01_ed-1_seq-3", "outcome": "dead_lettered", "cause": "unroutable", "at": at},
        ]
        got = [x["doc_id"][-5:] for x in m.pending(queued, outcomes, ["user"])]
        self.assertEqual(got, ["seq-3"])

    def test_reasons_not_asked_for_and_bad_ids_are_left_out(self):
        queued = [q("sn1_1900-01-01_ed-1_seq-1", "bulk", 1), q("not a page", "user", 1)]
        self.assertEqual(m.pending(queued, [], ["user"]), [])

    def test_rows_carry_the_manifest_columns(self):
        r = m.row_of(q("sn83030193_1918-01-28_ed-2_seq-1", "user", 1, batch="nn_keddy"),
                     {"sn83030193_1918-01-28_ed-2_seq-1": {"lang": "eng", "decision": "detected", "damage_rate": 0.31}})
        self.assertEqual((r["lccn"], r["edition"], r["seq"], r["damage_rate"]), ("sn83030193", 2, 1, 0.31))
        self.assertEqual(r["item_url"], "https://www.loc.gov/item/sn83030193/1918-01-28/ed-2/")
        r = m.row_of(q("sn1_1900-01-01_ed-1_seq-1", "user", 1), {})
        self.assertEqual((r["lang"], r["decision"], r["damage_rate"]), ("eng", "detected", 0.0))


def ranked(doc, batch, lccn, decade, rank):
    return [doc, batch, "1", "detected", "0.5", lccn, decade, str(rank)]


class PickIssuesTest(unittest.TestCase):
    rows = [
        # Rank 1 of each stratum, worst first: an issue of title a (two pages), one of b.
        ranked("a_1900-01-01_ed-1_seq-1", "x_a_ver01", "a", "1900", 1),
        ranked("a_1900-01-01_ed-1_seq-2", "x_a_ver01", "a", "1900", 1),
        ranked("b_1910-05-01_ed-1_seq-1", "x_b_ver01", "b", "1910", 1),
        # Rank 2: the next issue of each.
        ranked("a_1901-02-02_ed-1_seq-1", "x_a_ver01", "a", "1900", 2),
        ranked("b_1911-05-01_ed-2_seq-1", "x_b_ver01", "b", "1910", 2),
        ranked("b_1911-05-01_ed-2_seq-2", "x_b_ver01", "b", "1910", 2),
    ]

    def docs(self, picked):
        return [r[0] for r in picked]

    def test_whole_issues_round_robin(self):
        got = m.pick_issues(self.rows, set(), set(), strata=1, max_pages=100)
        self.assertEqual(self.docs(got), ["a_1900-01-01_ed-1_seq-1", "a_1900-01-01_ed-1_seq-2",
                                          "b_1910-05-01_ed-1_seq-1"])
        got = m.pick_issues(self.rows, set(), set(), strata=2, max_pages=100)
        self.assertEqual(len(got), 6)

    def test_a_done_issue_gives_way_and_done_pages_are_left_out(self):
        queued = {"b_1910-05-01_ed-1_seq-1"}
        index = {("x_a_ver01", "a_1900-01-01_ed-1_seq-2")}
        got = m.pick_issues(self.rows, queued, index, strata=1, max_pages=100)
        self.assertEqual(self.docs(got), ["a_1900-01-01_ed-1_seq-1", "b_1911-05-01_ed-2_seq-1",
                                          "b_1911-05-01_ed-2_seq-2"])

    def test_an_issue_is_never_split_by_the_cap(self):
        got = m.pick_issues(self.rows, set(), set(), strata=2, max_pages=4)
        # a's first issue (2), b's first (1), then a's second (1) fits; b's second (2) doesn't.
        self.assertEqual(len(got), 4)
        self.assertNotIn("b_1911-05-01_ed-2_seq-1", self.docs(got))
        got = m.pick_issues(self.rows, set(), set(), strata=1, max_pages=1)
        self.assertEqual(self.docs(got), ["a_1900-01-01_ed-1_seq-1", "a_1900-01-01_ed-1_seq-2"])

    def test_batches_file(self):
        import tempfile
        with tempfile.NamedTemporaryFile("w", suffix=".tsv", delete=False) as f:
            f.write("# probe\nak_goat_ver01\t200\t2026-10-25T20:51:39Z\nak_albatross_ver01\t403\tt\n"
                    "curiv_joshuaTree_ver01\n")
        self.assertEqual(m.read_batches(f.name), {"ak_goat_ver01", "curiv_joshuaTree_ver01"})
        with open(f.name, "w") as g:
            g.write("x'; drop table y; --\n")
        with self.assertRaises(SystemExit):
            m.read_batches(f.name)


if __name__ == "__main__":
    unittest.main()
