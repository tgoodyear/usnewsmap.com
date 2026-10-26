import importlib.util
from pathlib import Path
import unittest

spec = importlib.util.spec_from_file_location(
    "ghn_gap_survey", Path(__file__).with_name("ghn-gap-survey.py")
)
survey = importlib.util.module_from_spec(spec)
spec.loader.exec_module(survey)


class ClassificationTests(unittest.TestCase):
    def test_missing_cached_record_is_unknown_not_exclusive_to_ghn(self):
        rows = survey.classify([{"lccn": "sn90052158"}], {}, set())
        self.assertEqual(rows[0]["bucket"], "ca_unknown")

    def test_cached_title_without_published_pages_needs_investigation(self):
        rows = survey.classify(
            [{"lccn": "sn90052158"}], {"sn90052158": {"directory_only": True}}, set()
        )
        self.assertEqual(rows[0]["bucket"], "ca_not_ours")

    def test_published_pages_take_precedence_even_if_cache_lacks_title(self):
        rows = survey.classify([{"lccn": "sn90052158"}], {}, {"sn90052158"})
        self.assertEqual(rows[0]["bucket"], "ours")


if __name__ == "__main__":
    unittest.main()
