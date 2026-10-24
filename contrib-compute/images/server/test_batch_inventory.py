import importlib.util
import json
import os
import subprocess
import unittest
import uuid
from pathlib import Path
from unittest.mock import patch


SPEC = importlib.util.spec_from_file_location("batch_inventory", Path(__file__).with_name("batch-inventory.py"))
inventory = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(inventory)


class InventoryTests(unittest.TestCase):
    def setUp(self):
        self.root = Path(__file__).parent / (".inventory-test-" + uuid.uuid4().hex)
        self.root.mkdir()
        self.results = self.root / "results"
        self.results.mkdir()
        self.job = "a" * 64
        directory = self.root / "jobs" / self.job
        directory.mkdir(parents=True)
        (directory / "record.json").write_text(json.dumps({
            "job": {"id": self.job}, "stage": "submitted",
        }))
        self.spool = self.results / "batch1"
        self.spool.mkdir()

    def tearDown(self):
        import shutil
        shutil.rmtree(self.root)

    def payload(self):
        return inventory.inventory(self.root, self.results, [3, 2], "b" * 32)

    def write(self, suffix):
        (self.spool / ("usnm_" + self.job + suffix)).write_text("test")

    def test_requires_complete_outputs_and_counts_all_apps(self):
        self.write("__file_result.json")
        self.assertIsNone(self.payload()["jobs"][self.job]["spool"])
        self.write("__file_text.txt")
        self.write("__file_layout.xml")
        result = self.payload()
        self.assertEqual(result["pending_workunits"], 3)
        self.assertEqual(result["active_hosts"], 2)
        self.assertEqual(result["jobs"][self.job]["spool"], str(self.spool))
        self.assertIn("assimilate_state != 2", inventory.QUERY)
        self.assertNotIn("DELETE", inventory.QUERY)

    def test_terminal_markers_do_not_require_outputs(self):
        self.write("_error")
        self.assertEqual(self.payload()["jobs"][self.job]["spool"], str(self.spool))

    def test_ambiguous_spools_fail_closed(self):
        self.write("_error")
        second = self.results / "batch2"
        second.mkdir()
        (second / ("usnm_" + self.job + "_no_output_files")).write_text("error")
        with self.assertRaisesRegex(ValueError, "multiple"):
            self.payload()

    def test_complete_plus_partial_spools_are_also_ambiguous(self):
        for suffix in ("__file_result.json", "__file_text.txt", "__file_layout.xml"):
            self.write(suffix)
        second = self.results / "partial"
        second.mkdir()
        (second / ("usnm_" + self.job + "__file_result.json")).write_text("partial")
        with self.assertRaisesRegex(ValueError, "multiple"):
            self.payload()

    def test_limits_and_invalid_counts(self):
        with patch.object(inventory, "MAX_JOBS", 0), self.assertRaises(ValueError):
            self.payload()
        with patch.object(inventory, "MAX_FILES", 0), self.assertRaises(ValueError):
            self.payload()
        with self.assertRaises(ValueError):
            inventory.inventory(self.root, self.results, [-1, 2], "b" * 32)
        with self.assertRaises(ValueError):
            inventory.inventory(self.root, self.root / "missing", [1, 2], "b" * 32)

    def test_other_apps_nested_dirs_and_symlinks_as_with_rglob(self):
        # #932: the scandir walk finds a job's spool in a nested directory, ignores other apps'
        # files and unknown jobs, and neither follows nor yields symlinks.
        nested = self.results / "0" / "deeper"
        nested.mkdir(parents=True)
        for suffix in ("__file_result.json", "__file_text.txt", "__file_layout.xml"):
            (nested / ("usnm_" + self.job + suffix)).write_text("ok")
        (self.results / "0" / ("usnm_quality_" + "c" * 64 + "_0")).write_text("other app")
        (self.results / "0" / ("usnm_" + "d" * 64 + "_error")).write_text("unknown job")
        outside = self.root / "outside"
        outside.mkdir()
        (outside / ("usnm_" + self.job + "_error")).write_text("outside")
        (self.results / "link").symlink_to(outside, target_is_directory=True)
        (self.spool / ("usnm_" + self.job + "_error")).symlink_to(outside / ("usnm_" + self.job + "_error"))
        self.assertEqual(self.payload()["jobs"][self.job]["spool"], str(nested))

    def test_every_entry_counts_towards_the_file_limit(self):
        for i in range(3):
            (self.spool / f"usnm_quality_{i}").write_text("other app")
        with patch.object(inventory, "MAX_FILES", 4):  # batch1 and its 3 files
            self.payload()
        with patch.object(inventory, "MAX_FILES", 3), self.assertRaisesRegex(ValueError, "spool limit"):
            self.payload()

    def test_non_matching_files_are_never_statted(self):
        # The walk must not stat entries it doesn't keep: that per-file stat over SMB was the cost.
        for i in range(50):
            (self.spool / f"usnm_quality_{i}").write_text("other app")
        calls = []
        real = Path.stat

        def counting(path, *args, **kwargs):
            calls.append(path.name)
            return real(path, *args, **kwargs)

        with patch.object(Path, "stat", counting):
            self.payload()
        self.assertFalse([name for name in calls if name.startswith("usnm_quality_")])

    def test_query_and_bridge_integration_are_read_only(self):
        bridge = Path(__file__).with_name("bridge-job.sh").read_text()
        self.assertIn('if [ "$step" = batch-inventory ]; then', bridge)
        self.assertIn('--prefix scheduler', bridge)
        self.assertNotIn("INSERT", inventory.QUERY)
        self.assertNotIn("UPDATE", inventory.QUERY)
        self.assertIn("rpc_time > UNIX_TIMESTAMP() - 86400", inventory.QUERY)

    def bundle(self, bundle, pages, stage="submitted"):
        for page in pages:
            directory = self.root / "jobs" / page
            directory.mkdir(parents=True, exist_ok=True)
            (directory / "record.json").write_text(json.dumps({
                "job": {"id": page}, "stage": stage, "bundle": bundle,
            }))

    def test_a_bundle_workunit_counts_as_its_pages_and_spools_for_each(self):
        bundle, pages = "c" * 64, ["d" * 64, "e" * 64]
        self.bundle(bundle, pages)
        result = inventory.inventory(self.root, self.results, [3, 2], "b" * 32,
                                     [self.job], [self.job], [bundle], [bundle])
        self.assertEqual(result["pending_jobs"], [self.job] + pages)
        self.assertEqual(result["existing_jobs"], [self.job] + pages)
        # Partial bundle outputs give no spool; complete ones give every page the bundle's.
        (self.spool / f"usnm_ocrb_{bundle}__file_results.json").write_text("{}")
        payload = inventory.inventory(self.root, self.results, [3, 2], "b" * 32, (), None,
                                      (), [bundle])
        self.assertTrue(all(payload["jobs"][p]["spool"] is None for p in pages))
        self.assertEqual(payload["pending_jobs"], [])
        self.assertEqual(payload["existing_jobs"], pages)
        for name in ("texts", "layouts"):
            (self.spool / f"usnm_ocrb_{bundle}__file_{name}").write_text("x")
        payload = inventory.inventory(self.root, self.results, [3, 2], "b" * 32, (), None,
                                      (), [bundle])
        self.assertEqual({payload["jobs"][p]["spool"] for p in pages}, {str(self.spool)})
        self.assertIsNone(payload["jobs"][self.job]["spool"])

    def test_a_page_requeued_from_its_bundle_leaves_it(self):
        bundle, pages = "c" * 64, ["d" * 64, "e" * 64]
        self.bundle(bundle, pages)
        # The second page went back to reserved, out of the bundle (spool-out's requeue).
        (self.root / "jobs" / pages[1] / "record.json").write_text(json.dumps({
            "job": {"id": pages[1]}, "stage": "reserved", "unbundled": bundle,
        }))
        (self.spool / f"usnm_ocrb_{bundle}_error").write_text("")
        payload = inventory.inventory(self.root, self.results, [3, 2], "b" * 32, (), None,
                                      (), [bundle])
        self.assertEqual(payload["existing_jobs"], pages[:1])
        self.assertEqual(payload["jobs"][pages[0]]["spool"], str(self.spool))
        self.assertIsNone(payload["jobs"][pages[1]]["spool"])
        # The same page in a single-page workunit and a bundle is refused.
        with self.assertRaises(ValueError):
            inventory.inventory(self.root, self.results, [3, 2], "b" * 32, [], [pages[0]],
                                [], [bundle])


@unittest.skipUnless(os.environ.get("USNM_WORK_VOLUME_MYSQL_PORT"),
                     "set USNM_WORK_VOLUME_MYSQL_PORT for the isolated MySQL fixture")
class InventorySQLTests(unittest.TestCase):
    def test_counts_all_unassimilated_work_and_recent_rpc_hosts(self):
        result = subprocess.run(
            ["mysql", "--no-defaults", "--protocol=TCP", "--host=127.0.0.1",
             f"--port={os.environ['USNM_WORK_VOLUME_MYSQL_PORT']}", "--user=root",
             "--database=mysql", "--batch", "--raw", "--skip-column-names"],
            input="""
CREATE TEMPORARY TABLE workunit (assimilate_state INTEGER NOT NULL, name VARCHAR(254));
CREATE TEMPORARY TABLE host (rpc_time DOUBLE NOT NULL);
INSERT INTO workunit VALUES (0,'other-app'),(1,'usnm_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa'),
 (2,'done'),(2,'usnm_bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb'),(9,'unknown'),
 (0,'usnm_ocrb_cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc'),(0,'usnm_ocrb_short');
INSERT INTO host VALUES (0),(UNIX_TIMESTAMP()-90000),(UNIX_TIMESTAMP()),(UNIX_TIMESTAMP()-1);
""" + inventory.QUERY + ";",
            text=True, capture_output=True, check=True,
        )
        self.assertEqual(result.stdout.strip(),
                         "5\t2\nusnm_" + "a" * 64 + "\t1\nusnm_" + "b" * 64 + "\t2\nusnm_ocrb_" + "c" * 64 + "\t0")


if __name__ == "__main__":
    unittest.main()
