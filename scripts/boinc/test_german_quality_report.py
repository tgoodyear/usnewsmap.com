import copy
import importlib.util
import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path


SCRIPT = Path(__file__).with_name("german-quality-report.py")
SPEC = importlib.util.spec_from_file_location("german_quality_report", SCRIPT)
REPORT = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(REPORT)
DOC = "sn84027101_1910-08-16_ed-1_seq-10"
OTHER = "sn85055101_1917-04-03_ed-1_seq-3"


class Report(unittest.TestCase):
    def setUp(self):
        self.sample = [{"doc_id": DOC, "lang": "deu"}, {"doc_id": OTHER, "lang": "deu"}]
        self.pages = [
            {"doc_id": DOC, "reader": "ours", "outcome": "ok",
             "cer_vs_loc": 1.2, "dice_vs_loc": 0.9, "language": "und"},
            {"doc_id": OTHER, "reader": "ours", "outcome": "timeout",
             "cer_vs_loc": 0.0, "dice_vs_loc": 1.0},
            {"doc_id": DOC, "reader": "loc", "outcome": "ok",
             "chars": 10, "words": 2, "language": "und",
             "cer_vs_loc": 0.0, "wer_vs_loc": 0.0, "dice_vs_loc": 1.0},
        ]
        self.crops = [
            {"doc_id": DOC, "crop_id": "crop-1", "reader": "ours",
             "x0": 0, "y0": 0, "x1": 10, "y1": 10, "stratum": "low/top",
             "vlm_text": "reference", "vlm_model": "model-a", "cer_vs_vlm": 0.25},
            {"doc_id": DOC, "crop_id": "crop-2", "reader": "ours",
             "x0": 0, "y0": 10, "x1": 10, "y1": 20, "stratum": "low/bottom",
             "vlm_text": "", "vlm_model": "model-a", "cer_vs_vlm": 0.0},
        ]

    def run_report(self, readers=("ours", "loc")):
        return REPORT.report(self.sample, self.pages, self.crops, readers, 2)

    def test_strata_order_disagreement_and_failures_keep_their_denominators(self):
        result = self.run_report()
        overall = result["overall"]
        self.assertEqual(overall["page_readers"]["ours"]["outcomes"], {"ok": 1, "timeout": 1})
        self.assertEqual(overall["page_readers"]["loc"]["outcomes"], {"ok": 1, "unknown_no_reader_row": 1})
        metrics = overall["page_readers"]["ours"]["metrics_on_successes"]
        self.assertEqual(metrics["cer_vs_loc"]["mean"], 1.2)
        self.assertEqual(metrics["dice_vs_loc"]["mean"], 0.9)
        self.assertEqual(len(result["by_title_year"]), 2)
        self.assertEqual(result["sample_pages"], ["loc:" + DOC, "loc:" + OTHER])
        self.assertEqual(result["reported_reader_row_languages"], {"und": 2, "unreported": 1})
        crops = overall["crop_readers"]["ours"]
        self.assertEqual(crops["empty_reference_rows"], 1)
        self.assertEqual(crops["metrics_with_nonempty_reference"]["cer_vs_vlm"]["mean"], 0.25)
        self.assertEqual(overall["crop_upper_bound"], 4)
        self.assertEqual(overall["unused_crop_capacity"], 2)
        self.assertIsNone(overall["unobserved_staged_crops"])
        self.assertEqual(overall["crop_readers"]["loc"]["missing_rows_for_observed_crops"], 2)
        self.assertEqual(overall["page_readers"]["loc"]["metrics_on_successes"]["cer_vs_loc"]["mean"], 0.0)

    def test_duplicate_aliases_and_outside_sample_are_errors(self):
        for kind in ("sample", "pages", "crops", "outside"):
            with self.subTest(kind=kind):
                saved = copy.deepcopy((self.sample, self.pages, self.crops))
                if kind == "sample":
                    self.sample.append({"doc_id": "loc:" + DOC, "lang": "deu"})
                elif kind == "outside":
                    self.crops[0]["doc_id"] = "sn84027101_1911-08-16_ed-1_seq-10"
                else:
                    rows = getattr(self, kind)
                    rows.append(dict(rows[0], doc_id="loc:" + DOC))
                with self.assertRaises(ValueError):
                    self.run_report()
                self.sample, self.pages, self.crops = saved

    def test_invalid_metrics_and_reference_conflicts_are_errors(self):
        for bad in (float("nan"), float("inf"), True, -0.1, "0.5"):
            with self.subTest(bad=bad):
                self.pages[0]["cer_vs_loc"] = bad
                with self.assertRaises(ValueError):
                    self.run_report()
        self.pages[0]["cer_vs_loc"] = 0.5
        self.crops.append(dict(self.crops[0], reader="vlm", vlm_text="different"))
        with self.assertRaisesRegex(ValueError, "inconsistent VLM"):
            self.run_report()

    def test_zero_crops_and_missing_pages_are_explicit_not_success(self):
        self.pages = []
        self.crops = []
        result = self.run_report()["overall"]
        self.assertEqual(result["observed_crops"], 0)
        self.assertEqual(result["unused_crop_capacity"], 4)
        self.assertIsNone(result["unobserved_staged_crops"])
        self.assertEqual(result["page_readers"]["ours"]["outcomes"], {"unknown_no_reader_row": 2})

    def test_comparator_identity_cannot_be_missing_or_mixed(self):
        for model in (None, "different-model"):
            with self.subTest(model=model):
                crops = copy.deepcopy(self.crops)
                crops[1]["vlm_text"] = "reference"
                crops[1]["vlm_model"] = model
                with self.assertRaises(ValueError):
                    REPORT.report(self.sample, self.pages, crops, ["ours", "loc"], 2)

    def test_native_null_reference_is_missing_not_an_empty_or_successful_reading(self):
        self.crops[1]["vlm_text"] = None
        self.crops[1]["vlm_model"] = None
        result = self.run_report()["overall"]["crop_readers"]["ours"]
        self.assertEqual(result["missing_reference_rows"], 1)
        self.assertEqual(result["empty_reference_rows"], 0)
        self.assertEqual(result["metrics_with_nonempty_reference"]["cer_vs_vlm"]["observed"], 1)
        self.assertEqual(result["metrics_with_nonempty_reference"]["cer_vs_vlm"]["mean"], 0.25)

    def test_sample_and_crop_plan_validation(self):
        for sample in ([], [{"doc_id": DOC, "lang": "eng"}],
                       [{"doc_id": DOC.replace("08-16", "02-31"), "lang": "deu"}]):
            with self.subTest(sample=sample), self.assertRaises(ValueError):
                REPORT.report(sample, [], [], ["ours"], 2)
        with self.assertRaisesRegex(ValueError, "exceed"):
            REPORT.report(self.sample, self.pages, self.crops, ["ours", "loc"], 1)

    def test_numeric_and_long_lccns_follow_the_native_title_contract(self):
        for title in ("2010270504", "sn2001063133", "a" * 16):
            doc = DOC.replace("sn84027101", title)
            with self.subTest(title=title):
                result = REPORT.report([{"doc_id": doc, "lang": "deu"}], [], [], ["ours"], 2)
                self.assertEqual(result["sample_pages"], ["loc:" + doc])
        for doc in (DOC.replace("sn84027101", "a" * 17),
                    DOC.replace("sn84027101", "SN84027101"),
                    DOC.replace("ed-1", "ed-65536"),
                    DOC.replace("seq-10", "seq-65536")):
            with self.subTest(doc=doc), self.assertRaises(ValueError):
                REPORT.report([{"doc_id": doc, "lang": "deu"}], [], [], ["ours"], 2)

    def test_empty_or_unknown_loc_reference_excludes_comparisons_not_timing(self):
        self.pages[0]["total_ms"] = 12
        for count, exclusion in ((0, "empty_reference_rows"), (None, "unknown_reference_rows")):
            with self.subTest(count=count):
                self.pages[2]["chars"] = count
                self.pages[2]["words"] = count
                result = self.run_report()["overall"]["page_readers"]
                for reader in ("ours", "loc"):
                    metrics = result[reader]["metrics_on_successes"]
                    for name in ("cer_vs_loc", "wer_vs_loc", "dice_vs_loc"):
                        self.assertEqual(metrics[name]["observed"], 0)
                        self.assertEqual(metrics[name][exclusion], 1)
                        self.assertIsNone(metrics[name]["mean"])
                self.assertEqual(result["ours"]["metrics_on_successes"]["total_ms"]["mean"], 12)
        self.pages[2].update(chars=2, words=0)
        metrics = self.run_report()["overall"]["page_readers"]["ours"]["metrics_on_successes"]
        self.assertEqual(metrics["cer_vs_loc"]["observed"], 1)
        self.assertEqual(metrics["dice_vs_loc"]["empty_reference_rows"], 1)

    def test_crop_join_binds_exported_rectangle_and_stratum(self):
        for change in ({"x1": 11}, {"stratum": "high/top"}):
            with self.subTest(change=change):
                crops = self.crops + [dict(self.crops[0], reader="vlm", **change)]
                with self.assertRaisesRegex(ValueError, "geometry/stratum"):
                    REPORT.report(self.sample, self.pages, crops, ["ours", "loc"], 2)

    def test_languages_remain_attributed_to_each_page_and_reader(self):
        self.pages[2]["language"] = "deu"
        result = self.run_report()
        self.assertEqual(result["reported_reader_row_languages"], {"deu": 1, "und": 1, "unreported": 1})
        page = next(row for row in result["per_page"] if row["page_id"] == "loc:" + DOC)
        self.assertEqual(page["page_readers"]["ours"]["reported_reader_row_languages"], {"und": 1})
        self.assertEqual(page["page_readers"]["loc"]["reported_reader_row_languages"], {"deu": 1})

    def test_cli_is_deterministic_hash_bound_and_never_overwrites(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            for name, rows in (("sample", self.sample), ("pages", self.pages), ("crops", self.crops)):
                (root / name).write_text("".join(json.dumps(row) + "\n" for row in rows))
            args = [sys.executable, str(SCRIPT), "--sample", str(root / "sample"),
                    "--pages", str(root / "pages"), "--crops", str(root / "crops"),
                    "--reader", "ours", "--reader", "loc", "--max-crops-per-page", "2"]
            subprocess.run([*args, "--out", str(root / "first")], check=True, capture_output=True)
            subprocess.run([*args, "--out", str(root / "second")], check=True, capture_output=True)
            first = (root / "first").read_bytes()
            self.assertEqual(first, (root / "second").read_bytes())
            result = json.loads(first)
            self.assertEqual(set(result["input_sha256"]), {"sample", "pages", "crops", "reporter"})
            failed = subprocess.run([*args, "--out", str(root / "first")], capture_output=True)
            self.assertEqual(failed.returncode, 2)
            self.assertIn(b"error:", failed.stderr)
            self.assertEqual(first, (root / "first").read_bytes())


if __name__ == "__main__":
    unittest.main()
