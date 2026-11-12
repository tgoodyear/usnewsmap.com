"""Tests for the per-cell gain measure (#1415): no database."""
import contextlib
import csv
import io
import json
import tempfile
import unittest
from pathlib import Path
from unittest import mock
import importlib.util
import sys

HERE = Path(__file__).resolve().parent
sys.path[:0] = [str(HERE), str(HERE.parent / "ocr-feed")]
spec = importlib.util.spec_from_file_location("quality_gain", HERE / "quality_gain.py")
qg = importlib.util.module_from_spec(spec)
spec.loader.exec_module(qg)


def page(i, lang="eng", decade=1900, loc=0.4, ours=0.3, occ=60, reader="ours-tesseract", cer=None):
    return [f"sn{i}_1900-01-01_ed-1_seq-1", reader, lang, decade, occ, loc, ours,
            "" if cer is None else cer[0], "" if cer is None else cer[1]]


class GainTest(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.d = Path(self.tmp.name)

    def run_on(self, rows, *extra):
        src = self.d / "pages.csv"
        with open(src, "w", newline="") as f:
            w = csv.writer(f, lineterminator="\n")
            w.writerow(qg.COLUMNS)
            w.writerows(rows)
        with contextlib.redirect_stdout(io.StringIO()):
            qg.main(["--input", str(src), "--out-dir", str(self.d / "out"), *extra])
        with open(self.d / "out/cells.csv") as f:
            cells = list(csv.DictReader(f))
        return cells, json.loads((self.d / "out/stop.json").read_text())

    def test_a_clear_gain_goes_and_no_gain_stops_by_band(self):
        rows = [page(i, loc=0.40, ours=0.30 + (i % 5) / 100) for i in range(40)]
        rows += [page(100 + i, lang="ger", loc=0.15, ours=0.15 + (i % 3 - 1) / 100) for i in range(40)]
        cells, stop = self.run_on(rows)
        got = {(c["band"], c["lang"]): c["decision"] for c in cells}
        self.assertEqual(got, {("worst", "eng"): "go", ("moderate", "ger"): "stop"})
        self.assertEqual(stop["cells"], [{"band": "moderate", "decade": 1900, "lang": "ger"}])

    def test_a_small_cell_keeps_measuring(self):
        cells, stop = self.run_on([page(i, loc=0.4, ours=0.4) for i in range(10)])
        self.assertEqual(cells[0]["decision"], "measuring")
        self.assertEqual(stop["cells"], [])

    def test_a_full_cell_decides_on_its_mean(self):
        self.assertEqual(qg.decide(374, 0.019, None, 0.02, 374), "stop")
        self.assertEqual(qg.decide(374, 0.02, None, 0.02, 374), "go")
        self.assertEqual(qg.decide(29, -1.0, 0.0001, 0.02, 374), "measuring")

    def test_one_reader_still_measuring_keeps_the_cell_open(self):
        rows = [page(i, loc=0.15, ours=0.15) for i in range(40)]
        rows += [page(200 + i, loc=0.15, ours=0.15, reader="xiaomi-ocr-0") for i in range(5)]
        _, stop = self.run_on(rows, "--readers", "ours-tesseract,xiaomi-ocr-0")
        self.assertEqual(stop["cells"], [])

    def test_a_reader_with_no_pages_in_a_cell_keeps_it_open(self):
        rows = [page(i, loc=0.15, ours=0.15) for i in range(40)]
        _, stop = self.run_on(rows, "--readers", "ours-tesseract,xiaomi-ocr-0")
        self.assertEqual(stop["cells"], [])
        _, stop = self.run_on(rows)
        self.assertEqual(len(stop["cells"]), 1)
        self.assertEqual(stop["filters"], {"min_occurrences": 50, "moderate_above": "0.1", "worst_above": "0.25"})

    def test_plan_weights_carry_the_cell_to_its_band(self):
        # 20 pages from a big title gaining 0.03 (weight 100), 20 from a small one gaining 0 (weight 1).
        rows = [page(i, loc=0.15, ours=0.12) for i in range(20)] + [page(100 + i, loc=0.15, ours=0.15) for i in range(20)]
        plan = self.d / "plan.jsonl"
        plan.write_text("".join(json.dumps({"doc_id": f"sn{i}_1900-01-01_ed-1_seq-1", "weight": 100 if i < 100 else 1}) + "\n"
                                for i in list(range(20)) + list(range(100, 120))))
        cells, _ = self.run_on(rows, "--plan", str(plan))
        self.assertAlmostEqual(float(cells[0]["mean_drop"]), 0.0297, places=4)

    def test_the_latest_output_is_taken_before_unscored_ones_are_dropped(self):
        sql = qg.export_sql(["ours-tesseract"])
        self.assertLess(sql.index("ORDER BY doc_id, reader, ocred_at DESC"), sql.index("WHERE damage_rate IS NOT NULL"))
        self.assertIn("excluded_estimates", sql)

    def test_the_cer_pair_is_reported_when_both_have_one(self):
        cells, _ = self.run_on([page(i, cer=(0.3, 0.25)) for i in range(3)] + [page(9)])
        self.assertEqual(int(cells[0]["cer_pairs"]), 3)
        self.assertAlmostEqual(float(cells[0]["mean_cer_drop"]), 0.05)

    def test_plan_restricts_the_pages(self):
        plan = self.d / "plan.jsonl"
        plan.write_text(json.dumps({"doc_id": "sn1_1900-01-01_ed-1_seq-1"}) + "\n")
        cells, _ = self.run_on([page(i) for i in range(5)], "--plan", str(plan))
        self.assertEqual(int(cells[0]["n"]), 1)

    def test_the_export_reads_only_and_quotes_reader_names(self):
        sql = qg.export_sql(["ours-tesseract", "o'x"])
        self.assertTrue(sql.lstrip().startswith("BEGIN TRANSACTION READ ONLY;"))
        self.assertIn("('o''x')", sql)
        self.assertNotIn("CREATE", sql.upper())

    def test_unknown_readers_stop_the_run(self):
        def fake(dsn_file, sql, out):
            out.write(b"ours-tesseract\nloc\n")
        dsn = self.d / "dsn"
        dsn.write_text("host=x")
        with mock.patch.object(qg, "psql", fake), self.assertRaises(SystemExit) as e:
            qg.main(["--dsn-file", str(dsn), "--out-dir", str(self.d / "o"), "--readers", "ours-tesseract,xiaomi-ocr-0"])
        self.assertIn("xiaomi-ocr-0", str(e.exception))

    def test_bad_reader_names_are_refused(self):
        with contextlib.redirect_stderr(io.StringIO()), self.assertRaises(SystemExit):
            qg.main(["--input", "x", "--out-dir", str(self.d), "--readers", "a b"])


if __name__ == "__main__":
    unittest.main()
