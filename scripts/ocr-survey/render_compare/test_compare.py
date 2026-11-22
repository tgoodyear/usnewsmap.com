"""render_compare/compare.py: the AUC comparison (#2103, step 4), on made-up rows and on the tiny synthetic set.

These tests prove the script's arithmetic and plumbing. They say nothing about whether render distance works."""

import json
import sys
import tempfile
import unittest
from pathlib import Path

import numpy as np

sys.path.insert(0, str(Path(__file__).resolve().parent))

import paths  # noqa: E402,F401
import compare  # noqa: E402
import distances  # noqa: E402
import fonts  # noqa: E402
import gold  # noqa: E402
import loader  # noqa: E402
import synthetic  # noqa: E402

FONT = (fonts.resolve("latin") or [None])[0]


def fake_rows(lang="eng", pages=12, per_page=40, seed=1, wc_informative=True):
    """Made-up distance rows. Wrong words are further from their scan; WC separates the non-word errors but not the
    real-word errors (LoC is as sure of 'had' as of 'bad'). Odd pages are the damaged ones."""
    rng = np.random.default_rng(seed)
    rows = []
    for p in range(pages):
        damage = 0.05 + 0.4 * (p % 2) + 0.01 * p / pages
        for w in range(per_page):
            wrong = rng.random() < 0.25
            rwe = wrong and rng.random() < 0.4
            shift = 1.0 if wrong else 0.0
            rows.append({
                "page_id": f"p{p}", "crop_id": f"c{p}", "word_index": w, "lang": lang, "status": "ok",
                "label": 0 if wrong else 1, "op": "sub" if wrong else "match", "real_word_error": bool(rwe),
                "page_damage": damage, "ocr_word": "word",
                "chamfer": float(rng.normal(shift * 1.5, 1.0)), "pixel": float(rng.normal(shift * 0.5, 1.0)),
                "hog_cos": float(rng.normal(shift * 1.5, 1.0)),
                "wc": float(np.clip(rng.normal(0.9 - (0.3 if wrong and not rwe and wc_informative else 0.0), 0.08), 0, 1)),
                "font_family": "Old Standard TT", "font_version": "Version 3.000", "font_sha256": "ab", "font_package": "p"})
    return rows


class AucTest(unittest.TestCase):
    def test_auc_matches_sklearn_with_ties(self):
        from sklearn.metrics import roc_auc_score

        rng = np.random.default_rng(0)
        y = rng.integers(0, 2, 200)
        s = np.round(rng.normal(y * 0.7, 1.0), 1)  # coarse, so there are ties
        self.assertAlmostEqual(compare.auc(y, s), roc_auc_score(y, s), places=12)
        self.assertIsNone(compare.auc(np.zeros(5, int), np.arange(5.0)))
        self.assertEqual(compare.auc(np.array([0, 1]), np.array([0.1, 0.9])), 1.0)
        self.assertEqual(compare.auc(np.array([0, 1]), np.array([0.9, 0.1])), 0.0)


class WcRankTest(unittest.TestCase):
    def test_rank_is_within_page_with_mean_ties(self):
        rows = [{"page_id": "a", "wc": w} for w in (0.1, 0.5, 0.5, 0.9)]
        rows += [{"page_id": "b", "wc": 0.7} for _ in range(3)]  # constant WC page
        rows += [{"page_id": "c", "wc": None}, {"page_id": "c", "wc": 0.2}]
        compare.add_wc_rank(rows)
        self.assertEqual([r["alto_wc_rank"] for r in rows[:4]], [0.125, 0.5, 0.5, 0.875])
        self.assertEqual([r["alto_wc_rank"] for r in rows[4:7]], [0.5, 0.5, 0.5])
        self.assertNotIn("alto_wc_rank", rows[7])
        self.assertEqual(rows[8]["alto_wc_rank"], 0.5)

    def test_rank_removes_a_page_level_shift_that_raw_wc_keeps(self):
        # two pages with the same within-page signal but WC shifted by page: ranked WC separates, raw WC does not pool
        rng = np.random.default_rng(5)
        rows = []
        for p, shift in (("p0", 0.0), ("p1", 0.5), ("p2", 0.0), ("p3", 0.5), ("p4", 0.25), ("p5", 0.25)):
            for w in range(60):
                wrong = rng.random() < 0.3
                rows.append({"page_id": p, "crop_id": p, "word_index": w, "lang": "eng", "status": "ok",
                             "label": 0 if wrong else 1, "op": "sub" if wrong else "match", "real_word_error": False,
                             "page_damage": 0.1, "ocr_word": "word", "chamfer": 0.0, "pixel": 0.0, "hog_cos": 0.0,
                             "wc": float(shift + 0.1 * rng.normal() - (0.1 if wrong else 0.0))})
        a = compare.run(rows)["languages"]["eng"]["results"]["all"]["auc"]
        self.assertGreater(a["alto_wc_rank"], a["alto_wc"] + 0.05)

    def test_raw_wc_is_reported_but_in_no_model(self):
        L = compare.run(fake_rows())["languages"]["eng"]
        self.assertIn("alto_wc", L["results"]["all"]["auc"])
        for k in ("logistic:existing", "logistic:existing+render"):
            self.assertNotIn("alto_wc", L["folds"][k]["members"])
            self.assertIn("alto_wc_rank", L["folds"][k]["members"])

    def test_decision_inputs_carry_gain_intervals(self):
        L = compare.run(fake_rows(pages=12, per_page=40), bootstrap=20)["languages"]["eng"]
        d = L["decision_inputs"]
        for k in ("all_words", "low_damage"):
            self.assertEqual(len(d[k]["gain_over_logistic_existing_ci95"]), 2)
            self.assertEqual(len(d[k]["gain_over_best_existing_single_ci95"]), 2)
        a = L["results"]["all"]["auc"]
        self.assertAlmostEqual(d["all_words"]["gain_over_logistic_existing"], a["logistic:existing+render"] - a["logistic:existing"])
        self.assertNotEqual(d["low_damage"]["best_existing_single"]["signal"], "alto_wc")


class RunTest(unittest.TestCase):
    def setUp(self):
        self.rep = compare.run(fake_rows(), low_q=0.34, bootstrap=0)
        self.L = self.rep["languages"]["eng"]

    def test_report_shape(self):
        self.assertEqual(set(self.L["results"]), {"all", "damage_low", "damage_mid", "damage_high", "real_word_errors",
                                                  "non_word_errors"})
        a = self.L["results"]["all"]["auc"]
        for k in ("chamfer", "pixel", "hog_cos", "alto_wc", "alto_wc_rank", "logistic:existing", "logistic:render", "logistic:existing+render"):
            self.assertIn(k, a)
        self.assertEqual(self.L["folds"]["logistic:render"]["cv"], "5-fold by page")
        self.assertEqual(self.rep["fonts"][0]["family"], "Old Standard TT")
        json.dumps(self.rep)  # serialisable

    def test_render_separates_and_wc_does_not_on_real_word_errors(self):
        r = self.L["results"]
        self.assertGreater(r["all"]["auc"]["chamfer"], 0.75)
        self.assertLess(abs(r["real_word_errors"]["auc"]["alto_wc_rank"] - 0.5), 0.12)
        self.assertGreater(r["real_word_errors"]["auc"]["chamfer"], 0.7)
        self.assertGreater(r["non_word_errors"]["auc"]["alto_wc_rank"], 0.75)
        # the real-word split holds only real-word errors and right words
        words = sum(1 for x in fake_rows() if x["label"] == 1 or x["real_word_error"])
        self.assertEqual(r["real_word_errors"]["words"], words)

    def test_damage_strata_are_by_page(self):
        r = self.L["results"]
        self.assertEqual(r["damage_low"]["words"] + r["damage_mid"]["words"] + r["damage_high"]["words"], r["all"]["words"])
        strata = compare.damage_strata(fake_rows(), 0.34)
        pages_by = {}
        for row, s in zip(fake_rows(), strata):
            pages_by.setdefault(row["page_id"], set()).add(s)
        self.assertTrue(all(len(v) == 1 for v in pages_by.values()))  # a page is in one stratum
        self.assertEqual(pages_by["p0"], {"low"})  # even pages are the clean ones
        self.assertEqual(pages_by["p11"], {"high"})

    def test_gain_and_rule(self):
        g = self.L["results"]["all"]["gain"]
        a = self.L["results"]["all"]["auc"]
        best_existing = max(a["alto_wc_rank"], 0.0)
        self.assertAlmostEqual(g["render_alone_over_best_existing"], max(a[k] for k in compare.ABSOLUTE) - best_existing)
        self.assertAlmostEqual(g["combined_over_existing_logistic"], a["logistic:existing+render"] - a["logistic:existing"])
        self.assertTrue(self.L["results"]["real_word_errors"]["rule"]["passes"])

    def test_no_gain_when_the_distances_are_noise(self):
        rows = fake_rows(seed=3)
        rng = np.random.default_rng(9)
        for r in rows:
            for k in compare.ABSOLUTE:
                r[k] = float(rng.normal())
        L = compare.run(rows)["languages"]["eng"]["results"]
        self.assertFalse(L["all"]["rule"]["passes"])
        self.assertLess(L["all"]["auc"]["chamfer"], 0.6)

    def test_relative_scores_join_the_render_signals_when_present(self):
        rows = fake_rows(seed=4)
        for r in rows:
            for k in compare.ABSOLUTE:
                r["rel_" + k] = r[k] - 0.5
        a = compare.run(rows)["languages"]["eng"]
        self.assertEqual(a["signals_unavailable"], [])
        self.assertEqual(a["folds"]["logistic:render"]["members"], compare.ABSOLUTE + compare.RELATIVE)
        for k in compare.RELATIVE:
            self.assertIn(k, a["results"]["all"]["auc"])
        # without the columns they are simply unavailable, and the render members are the three absolute ones
        b = self.L
        self.assertEqual(b["folds"]["logistic:render"]["members"], compare.ABSOLUTE)

    def test_languages_are_reported_apart(self):
        rep = compare.run(fake_rows("eng") + fake_rows("ger", seed=5), bootstrap=0)
        self.assertEqual(set(rep["languages"]), {"eng", "ger"})
        self.assertIn("lex_oov", rep["languages"]["eng"]["results"]["all"]["auc"])  # English has a lexicon
        self.assertNotIn("lex_oov", rep["languages"]["ger"]["results"]["all"]["auc"])

    def test_rows_missing_a_signal_are_dropped_and_counted(self):
        rows = fake_rows()
        for r in rows[:10]:
            r["wc"] = None
        L = compare.run(rows)["languages"]["eng"]
        self.assertEqual(L["dropped_missing_signal"], 10)
        self.assertEqual(L["words"], len(rows) - 10)

    def test_non_ok_rows_and_insertions(self):
        rows = fake_rows()
        rows.append(dict(rows[0], status="no_font", chamfer=None))
        rows.append(dict(rows[0], op="ins", label=0, word_index=999))
        n = len(rows)
        self.assertEqual(compare.run(rows)["languages"]["eng"]["words"], n - 1)
        self.assertEqual(compare.run(rows, substitutions_only=True)["languages"]["eng"]["words"], n - 2)

    def test_one_page_is_scored_in_sample_and_says_so(self):
        L = compare.run(fake_rows(pages=1, per_page=60))["languages"]["eng"]
        self.assertEqual(L["folds"]["logistic:render"]["cv"], "in-sample (one page)")

    def test_a_language_with_one_class_has_no_results(self):
        rows = [r for r in fake_rows() if r["label"] == 1]
        L = compare.run(rows)["languages"]["eng"]
        self.assertEqual(L["results"], {})

    def test_bootstrap_adds_intervals(self):
        L = compare.run(fake_rows(pages=10, per_page=30), bootstrap=30)["languages"]["eng"]["results"]["all"]
        lo, hi = L["ci95"]["chamfer"]
        self.assertLess(lo, L["auc"]["chamfer"] + 1e-9)
        self.assertGreater(hi, L["auc"]["chamfer"] - 1e-9)
        self.assertIn("gain:render_alone", L["ci95"])

    def test_qe_columns_are_merged_and_used(self):
        rows = fake_rows(pages=6, per_page=20)
        with tempfile.TemporaryDirectory() as d:
            qe = Path(d) / "qe.jsonl"
            gold.write_jsonl(qe, [{"crop_id": r["crop_id"], "word_index": r["word_index"],
                                   "charlm_bpc": 3.0 + (1 - r["label"])} for r in rows])
            rep = compare.run(rows, qe, ["charlm_bpc"])
        a = rep["languages"]["eng"]["results"]["all"]["auc"]
        self.assertEqual(a["qe_charlm_bpc"], 1.0)
        self.assertIn("qe_charlm_bpc", rep["signals"]["existing"])


@unittest.skipIf(FONT is None, "the pinned font is not fetched: run fonts.py --fetch")
class TinyFixtureTest(unittest.TestCase):
    def test_pipeline_runs_on_the_synthetic_set(self):
        with tempfile.TemporaryDirectory() as d:
            root = Path(d)
            fx = synthetic.make_set(root, scan_font=FONT.path)
            words, dist, out = root / "w.jsonl", root / "d.parquet", root / "report.json"
            self.assertEqual(loader.main(["--set-dir", str(fx["set_dir"]), "--alto-dir", str(fx["alto_dir"]), "--out", str(words)]), 0)
            self.assertEqual(distances.main(["--words", str(words), "--alto-dir", str(fx["alto_dir"]), "--out", str(dist)]), 0)
            self.assertEqual(compare.main(["--distances", str(dist), "--out", str(out)]), 0)
            rep = json.loads(out.read_text())
        L = rep["languages"]["eng"]
        self.assertEqual((L["words"], L["wrong"], L["real_word_errors"]), (10, 3, 1))
        self.assertEqual(L["folds"]["logistic:render"]["cv"], "in-sample (one page)")
        self.assertIsNotNone(L["results"]["all"]["auc"]["hog_cos"])


if __name__ == "__main__":
    unittest.main()
