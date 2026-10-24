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


if __name__ == "__main__":
    unittest.main()
