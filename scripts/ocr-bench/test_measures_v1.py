import importlib.util
from pathlib import Path
import unittest

spec = importlib.util.spec_from_file_location("measures_v1", Path(__file__).with_name("measures_v1.py"))
measures = importlib.util.module_from_spec(spec)
spec.loader.exec_module(measures)


class MeasuresV1Tests(unittest.TestCase):
    def test_insertions_omissions_and_missing_are_counted(self):
        self.assertEqual(measures.edit_counts("ab", "axb"),
                         {"substitutions": 0, "insertions": 1, "deletions": 0})
        references = [{"id": "a", "reference": "word", "page_id": "p1"},
                      {"id": "b", "reference": "text", "page_id": "p2"},
                      {"id": "c", "reference": "", "page_id": "p3"}]
        outputs = [{"id": "a", "text": ""}, {"id": "b", "status": "truncated", "text": "te"},
                   {"id": "c", "status": "error"}]
        result = measures.compare_transcriptions(references, {"engine": outputs}, bootstrap_samples=5)
        score = result["systems"]["engine"]["transcription"]
        self.assertEqual((score["deletions"], score["truncated"], score["errors"], score["empty_outputs"]),
                         (6, 1, 1, 1))
        self.assertEqual(score["reference_characters"], 8)

    def test_long_page_edit_alignment_uses_exact_counts(self):
        counts = measures.edit_counts("a" * 100 + "b" * 100, "a" * 99 + "x" + "b" * 100 + "z")
        self.assertEqual(counts, {"substitutions": 1, "insertions": 1, "deletions": 0})

    def test_legacy_rule_matches_existing_survey_scorer_fixtures(self):
        refs = [{"id": "control", "reference": " a\x1fb\u3000c\n\n"},
                {"id": "zero-width", "reference": "a\u200bb"}]
        outputs = {"candidate": [{"id": "control", "text": "a b c"},
                                 {"id": "zero-width", "text": "a\u200bb"}]}
        score = measures.compare_transcriptions(refs, outputs, rule="legacy-ws-v1")
        self.assertEqual(score["systems"]["candidate"]["transcription"]["cer"], 0)

    def test_legacy_whitespace_is_separate_from_diplomatic_rule(self):
        refs = [{"id": "one", "reference": "A  B\n"}]
        outputs = {"same": [{"id": "one", "text": "A B"}]}
        self.assertEqual(measures.compare_transcriptions(refs, outputs, rule="legacy-ws-v1")["systems"]["same"]["transcription"]["cer"], 0)
        self.assertGreater(measures.compare_transcriptions(refs, outputs)["systems"]["same"]["transcription"]["cer"], 0)

    def test_region_coverage_segmentation_order_and_contamination(self):
        references = [{"id": "page", "reference": "one two", "page_id": "page", "regions": [
            {"box": [0, 0, 10, 10], "order": 0, "text": "one"},
            {"box": [0, 20, 10, 10], "order": 1, "text": "two"}]}]
        outputs = {"engine": [{"id": "page", "text": "two one", "regions": [
            {"box": [0, 0, 10, 10], "order": 1, "text": "two"},
            {"box": [0, 20, 10, 10], "order": 0, "text": "one"},
            {"box": [30, 0, 5, 5], "order": 2, "text": "noise"}]}]}
        region = measures.compare_transcriptions(references, outputs, bootstrap_samples=5)["systems"]["engine"]["page_regions"]
        self.assertEqual(region["segmentation"]["coverage_recall"], 1)
        self.assertEqual(region["segmentation"]["precision"], 2 / 3)
        self.assertEqual(region["reconstruction"]["reading_order_pairwise_accuracy"], 0)
        self.assertEqual(region["reconstruction"]["cross_region_contamination_count"], 2)

    def test_unmatched_reference_region_counts_as_omission(self):
        refs = [{"id": "p", "reference": "first second", "regions": [
            {"box": [0, 0, 10, 10], "order": 0, "text": "first"},
            {"box": [0, 20, 10, 10], "order": 1, "text": "second"}]}]
        outs = {"engine": [{"id": "p", "text": "first", "regions": [
            {"box": [0, 0, 10, 10], "order": 0, "text": "first"}]}]}
        region = measures.compare_transcriptions(refs, outs, bootstrap_samples=5)["systems"]["engine"]["page_regions"]
        self.assertEqual(region["transcription"]["reference_characters"], 11)
        self.assertEqual(region["transcription"]["deletions"], 6)

    def test_human_adjudication_requires_two_qualified_readers(self):
        refs = [{"id": "x", "reference": "long s", "page_id": "p"}]
        outs = {"engine": [{"id": "x", "text": "long f"}]}
        review = [{"id": "x", "decision": "human-adjudicated", "qualified_readers": [{"id": "r1", "qualification": "historical transcription"}, {"id": "r2", "qualification": "paleography"}],
                   "categories": ["historical-glyph"]}]
        result = measures.compare_transcriptions(refs, outs, adjudications=review, bootstrap_samples=5)
        report = result["human_adjudication"]["engine"]
        self.assertEqual(report["categories"]["historical-glyph"]["count"], 1)
        self.assertEqual(report["error_review_coverage"], 1)
        with self.assertRaisesRegex(ValueError, "two qualified readers"):
            measures.compare_transcriptions(refs, outs, adjudications=[{**review[0], "qualified_readers": [{"id": "r1", "qualification": "historical transcription"}]}], bootstrap_samples=5)

    def test_strata_page_bootstrap_and_good_page_regression(self):
        refs = [{"id": "a", "reference": "good", "page_id": "p1", "title_id": "t1", "language": ["en"]},
                {"id": "b", "reference": "fine", "page_id": "p2", "title_id": "t2", "language": ["de"]}]
        outputs = {"base": [{"id": "a", "text": "good"}, {"id": "b", "text": ""}],
                   "candidate": [{"id": "a", "text": "bad"}, {"id": "b", "text": "fine"}]}
        result = measures.compare_transcriptions(refs, outputs, bootstrap_samples=50, seed=9)
        comparison = result["comparisons"]["candidate"]
        self.assertEqual(comparison["baseline_good_pages"], 1)
        self.assertEqual(comparison["good_page_regression_rate"], 1)
        self.assertEqual(comparison["paired_uncertainty_by_page"]["clusters"], 2)
        self.assertEqual(set(result["systems"]["base"]["strata"]["language"]), {"en", "de"})

    def test_search_metrics_compare_on_same_fixed_queries(self):
        queries = [
            {"id": "name", "kind": "name", "query": "Mercy", "relevance": {"a-noise": 0, "w-date": 0, "y-place": 0, "z-name": 2}},
            {"id": "place", "kind": "place", "query": "Cedar Falls", "relevance": {"a-noise": 0, "w-date": 0, "y-place": 2, "z-name": 0}},
            {"id": "date", "kind": "date", "query": "1898", "relevance": {"a-noise": 0, "w-date": 2, "y-place": 0, "z-name": 0}},
        ]
        baseline = [{"id": "a-noise", "text": "Local history"}, {"id": "w-date", "text": "189B"},
                    {"id": "y-place", "text": "Seedar FalIs"}, {"id": "z-name", "text": "Marcy"}]
        candidate = [{"id": "a-noise", "text": "Local history"}, {"id": "w-date", "text": "1898"},
                     {"id": "y-place", "text": "Cedar Falls"}, {"id": "z-name", "text": "Mercy"}]
        result = measures.score_search({"legacy": baseline, "candidate": candidate}, queries, k=1)
        self.assertEqual(result["results"]["legacy"]["recall@1"], 0)
        self.assertEqual(result["results"]["candidate"]["recall@1"], 1)
        self.assertEqual(set(result["results"]["candidate"]["by_kind"]), {"name", "place", "date"})
        self.assertEqual(result["results"]["candidate"]["analyzer"], "nfkc-casefold-word-v1")

    def test_review_sample_is_stable_and_independent_of_order(self):
        first = measures.deterministic_review_sample(["c", "a", "b"], 2, "fixed-run")
        self.assertEqual(first, measures.deterministic_review_sample(["b", "c", "a"], 2, "fixed-run"))
        self.assertEqual(len(first), 2)

    def test_quality_cost_gate_blocks_quality_regression(self):
        baseline = {"transcription": {"cer": 0.01, "missing": 0, "errors": 0, "samples": 10}}
        candidate = {"transcription": {"cer": 0.04, "missing": 0, "errors": 0, "samples": 10},
                     "good_page_regression_rate": 0}
        decision = measures.quality_cost_decision(baseline, candidate, baseline_cost=2, candidate_cost=1,
            max_cer_delta=0.01, max_missing_rate_delta=0, max_good_page_regression_rate=0.1)
        self.assertFalse(decision["eligible"])
        self.assertFalse(decision["cost_better_after_quality_gates"])

    def test_rejects_outputs_outside_paired_reference_set(self):
        with self.assertRaisesRegex(ValueError, "outside reference set"):
            measures.compare_transcriptions([{"id": "a", "reference": "x"}],
                                             {"engine": [{"id": "other", "text": "x"}]})


if __name__ == "__main__":
    unittest.main()
