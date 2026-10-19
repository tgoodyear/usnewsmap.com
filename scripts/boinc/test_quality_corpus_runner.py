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

    def test_a_failed_lookup_is_not_an_empty_database(self):
        lookup = SOURCE.split("def query_mysql_workunits")[1].split("\ndef ")[0]
        self.assertIn("raise", lookup)
        self.assertIn('"skipped": f"workunit lookup failed: {e}"', SOURCE)

    def test_an_existing_healthy_unit_keeps_its_bundle(self):
        self.assertIn("or (in_mysql and mysql_wus[wu][1] == 0)", SOURCE)

    def test_a_failed_submission_never_renames_existing_work(self):
        # The only rename is the repair step's, for a unit the transitioner marked failed.
        self.assertEqual(SOURCE.count("rename_colliding_workunit(wu)") - SOURCE.count("def rename_colliding_workunit(wu)"), 1)
        submit = SOURCE.split("submit.lock")[1].split("transitioner -d 2 --one_pass")[0]
        self.assertNotIn("rename_colliding_workunit", submit)
        self.assertIn("existing and existing[1] == 0", submit)


if __name__ == "__main__":
    unittest.main()


class LocFallback(unittest.TestCase):
    def test_off_unless_asked_for(self):
        # One 5-second pacer for every shard can't fetch a corpus (#914): the LoC pass runs
        # only with QUALITY_LOC_FALLBACK=1, and every pass loop goes through PASSES.
        self.assertIn('LOC_FALLBACK = os.environ.get("QUALITY_LOC_FALLBACK", "0") == "1"', SOURCE)
        self.assertIn('PASSES = ("jpeg_cached", "jpeg_cached", "archived") + (("loc_fallback",) if LOC_FALLBACK else ())', SOURCE)
        self.assertNotRegex(SOURCE, r'for pass_mode in \("')
