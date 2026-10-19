"""Guards on what the quality wave runner may change: python3 -m unittest -v scripts/boinc/test_quality_corpus_runner.py

The runner shares the BOINC project with every other app, so each database write must be
scoped to usnm_quality, and it must not touch GPU admission (plan_class_spec.xml) at all.
"""

import re
import unittest
from pathlib import Path

SOURCE = (Path(__file__).resolve().parent / "quality_corpus_runner.py").read_text()


class Scope(unittest.TestCase):
    def test_no_gpu_admission_or_other_apps_versions(self):
        self.assertNotIn("plan_class_spec", SOURCE)
        self.assertNotIn("min_wu_id", SOURCE)
        self.assertNotIn("plan_class LIKE", SOURCE)

    def test_every_update_is_scoped_to_quality(self):
        updates = re.findall(r"UPDATE\s+\w+.*?;", SOURCE, flags=re.S)
        self.assertGreaterEqual(len(updates), 10)
        for stmt in updates:
            with self.subTest(stmt=" ".join(stmt.split())[:120]):
                # Named quality units or app versions, or a unit name / id that the runner took
                # from a usnm_quality query ({wu}, {wid}).
                self.assertTrue(any(k in stmt for k in ("usnm_quality", "'{wu}'", "{wid}")), stmt)
        self.assertNotRegex(SOURCE, r"UPDATE host SET")

    def test_ids_it_cancels_come_from_quality_units(self):
        block = SOURCE.split("cancelled_missing_dl = 0")[1].split("{wid}")[0]
        self.assertIn("w.name LIKE 'usnm_quality_%'", block)

    def test_units_are_replaced_only_on_positive_evidence(self):
        self.assertIn("not in_mysql or mysql_wus[wu][1] != 0", SOURCE)


if __name__ == "__main__":
    unittest.main()
