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

    def test_query_and_bridge_integration_are_read_only(self):
        bridge = Path(__file__).with_name("bridge-job.sh").read_text()
        self.assertIn('if [ "$step" = batch-inventory ]; then', bridge)
        self.assertIn('--prefix scheduler', bridge)
        self.assertNotIn("INSERT", inventory.QUERY)
        self.assertNotIn("UPDATE", inventory.QUERY)
        self.assertIn("rpc_time > UNIX_TIMESTAMP() - 86400", inventory.QUERY)


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
 (2,'done'),(2,'usnm_bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb'),(9,'unknown');
INSERT INTO host VALUES (0),(UNIX_TIMESTAMP()-90000),(UNIX_TIMESTAMP()),(UNIX_TIMESTAMP()-1);
""" + inventory.QUERY + ";",
            text=True, capture_output=True, check=True,
        )
        self.assertEqual(result.stdout.strip(), "3\t2\nusnm_" + "a" * 64)


if __name__ == "__main__":
    unittest.main()
