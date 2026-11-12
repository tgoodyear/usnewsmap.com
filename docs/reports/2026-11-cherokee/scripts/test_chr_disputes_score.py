"""Tests for chr_disputes_score.py: python3 -m unittest discover -s docs/reports/2026-11-cherokee/scripts."""
import json
import os
import subprocess
import sys
import tempfile
import unittest

sys.path.insert(0, os.path.dirname(__file__))
import chr_disputes_score as cds  # noqa: E402


def man(stem, page, kind="news", loc=("ᎠᎴ", "ᎣᏂᏃ", "ᎡᎯ"), ours=("ᎠᎴ", "ᎣᏂᏗ", "ᎡᎯ"), k=1):
    return {"id": stem, "page": page, "kind": kind, "loc": list(loc), "ours": list(ours), "words": [k]}


def row(stem, outcome, settled=True, counts=None, k=1, a="ᎣᏂᏃ", b="ᎣᏂᏗ", a_is="loc", b_is="chr_phoenix"):
    c = {"a": 0, "b": 0, "neither": 0, "unsure": 0, "notext": 0}
    c.update(counts or {})
    return {"id": f"chr-lines/disputes-v1/{stem}~{k}", "a": a, "b": b, "a_is": a_is, "b_is": b_is, "counts": c, "settled": settled, "outcome": outcome}


class Score(unittest.TestCase):
    def setUp(self):
        self.manifest = [man("l1", "p1"), man("l2", "p1"), man("l3", "p2", "scripture"), man("l4", "p2"), man("l5", "p3")]
        self.export = [
            row("l1", "b", counts={"b": 3}),                                  # the re-read was right
            row("l2", "a", counts={"a": 3}),                                  # LoC was right
            row("l3", "b", counts={"b": 2, "a": 1}),                          # the re-read was right
            row("l4", "neither", counts={"neither": 3}),                      # settled without a winner
            row("l5", None, settled=False, counts={"a": 1}),                  # waiting
        ]

    def test_states_and_errors(self):
        recs = {r["id"]: r for r in cds.line_states(self.manifest, self.export)}
        self.assertEqual({k: r["state"] for k, r in recs.items()}, {"l1": "resolved", "l2": "resolved", "l3": "resolved", "l4": "open", "l5": "pending"})
        self.assertEqual(recs["l1"]["errors"], {"chr_phoenix": 0, "loc": 1})
        self.assertEqual(recs["l2"]["errors"], {"chr_phoenix": 1, "loc": 0})
        self.assertEqual(recs["l1"]["letters"], 7)
        self.assertEqual(recs["l5"]["answers"], 1)

    def test_report_counts_and_progress(self):
        rep = cds.report(self.manifest, self.export, target=10, draws=200, seed=1)
        p = rep["progress"]
        self.assertEqual((p["resolved_lines"], p["lines_open"], p["lines_pending"], p["still_needed"], p["answers"]), (3, 1, 1, 7, 13))
        self.assertEqual(p["answers_per_resolved_line"], 4.33)
        self.assertEqual(p["resolved_share_of_settled_lines"], 0.75)
        self.assertEqual(p["expected_resolved_if_all_loaded_lines_settle"], 4)
        self.assertFalse(p["loaded_lines_enough"])
        self.assertEqual(p["answers_still_needed"], 31)
        self.assertEqual(rep["words"], {"chr_phoenix_right": 2, "loc_right": 1, "neither": 1, "unsure": 0, "notext": 0, "disputed": 0})
        s = rep["score"]["resolved"]
        self.assertEqual((s["lines"], s["letters"]), (3, 21))
        self.assertAlmostEqual(s["chr_phoenix"], 1 / 21)
        self.assertAlmostEqual(s["loc"], 2 / 21)
        self.assertAlmostEqual(s["difference"], -1 / 21)
        lo, hi = s["interval_95"]["difference"]
        self.assertLessEqual(lo, s["difference"] + 1e-9)
        self.assertGreaterEqual(hi, s["difference"] - 1e-9)
        # the "neither" line counts one wrong letter for each reading in the bound
        b = rep["score"]["with_neither_lines"]
        self.assertEqual(b["lines"], 4)
        self.assertAlmostEqual(b["chr_phoenix"], 2 / 28)
        self.assertEqual(rep["by_kind"]["scripture"]["resolved"], 1)
        self.assertEqual(rep["pages_with_resolved_lines"], 2)
        self.assertIn("3 of 10 held-out lines resolved", rep["summary"])

    def test_no_answers_yet(self):
        rep = cds.report(self.manifest, [], target=300, draws=10, seed=1)
        self.assertEqual(rep["progress"]["resolved_lines"], 0)
        self.assertIsNone(rep["progress"]["answers_still_needed"])
        self.assertIsNone(rep["score"]["resolved"]["difference"])

    def test_edit_distance(self):
        self.assertEqual(cds.edit_distance("ᎠᎴᎣ", "ᎠᎣ"), 1)
        self.assertEqual(cds.edit_distance("", "ᎠᎣ"), 2)

    def test_command_line(self):
        with tempfile.TemporaryDirectory() as d:
            for name, rows in (("m.jsonl", self.manifest), ("e.jsonl", self.export)):
                with open(os.path.join(d, name), "w", encoding="utf-8") as f:
                    f.write("".join(json.dumps(r, ensure_ascii=False) + "\n" for r in rows))
            out = subprocess.run([sys.executable, os.path.join(os.path.dirname(__file__), "chr_disputes_score.py"), "--export", os.path.join(d, "e.jsonl"),
                                  "--manifest", os.path.join(d, "m.jsonl"), "--target", "10", "--draws", "50"], check=True, capture_output=True, text=True).stdout
        self.assertEqual(json.loads(out)["progress"]["resolved_lines"], 3)


if __name__ == "__main__":
    unittest.main()
