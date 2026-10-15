"""Offline checks for the issue #329 screening artifact and its denominators."""
import unittest
from unittest.mock import patch

import audit329


class ScreeningTest(unittest.TestCase):
    def test_unresolved_is_not_assumed_to_be_an_error(self):
        result = audit329.summarize(
            [{"language": "cze"}],
            [{"language": "cze", "label": label}
             for label in ("likely_ocr_error", "valid", "unresolved", "unresolved")],
        )["cze"]
        self.assertEqual(result["flagged"], 4)
        self.assertEqual(result["unresolved"], 2)
        self.assertEqual(result["provisional_precision_classified"], 0.5)
        self.assertEqual(result["conditional_precision_bounds_all_flags"], [0.25, 0.75])

    def test_no_classified_flags_has_no_precision(self):
        result = audit329.summarize([], [{"language": "hrv", "label": "unresolved"}])["hrv"]
        self.assertIsNone(result["provisional_precision_classified"])
        self.assertEqual(result["conditional_precision_bounds_all_flags"], [0.0, 1.0])

    def test_occurrence_override_precedes_type_decision(self):
        flag = {"language": "cze", "doc_id": "page", "word_index": 1, "word": "token"}
        rules = {("cze", "token"): {"label": "likely_ocr_error"}}
        occurrences = {("cze", "page", 1): {"label": "valid"}}
        self.assertEqual(audit329.label_flag(flag, rules, occurrences), {"label": "valid"})
        flag["word_index"] = 2
        self.assertEqual(audit329.label_flag(flag, rules, occurrences), {"label": "likely_ocr_error"})
        flag["word"] = "unreviewed"
        self.assertEqual(audit329.label_flag(flag, rules, occurrences)["label"], "unresolved")

    def test_unknown_label_cannot_disappear_from_denominator(self):
        with self.assertRaisesRegex(ValueError, "unknown annotation label"):
            audit329.summarize([], [{"language": "cze", "label": "ocr_error"}])

    def test_unknown_label_rejected_when_loading_decisions(self):
        with patch.object(audit329.json, "loads", return_value=[{"label": "ocr_error"}]):
            with self.assertRaisesRegex(ValueError, "unknown annotation label"):
                audit329.label_maps()

    def test_unknown_confidence_rejected_when_loading_decisions(self):
        decision = {"label": "valid", "confidence": "certain"}
        with patch.object(audit329.json, "loads", return_value=[decision]):
            with self.assertRaisesRegex(ValueError, "unknown annotation confidence"):
                audit329.label_maps()

    def test_committed_artifacts_reconcile(self):
        audit329.check()


if __name__ == "__main__":
    unittest.main()
