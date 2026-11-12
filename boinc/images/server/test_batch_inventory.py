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
            result = self.payload()
            self.assertEqual((result["spool_entries"], result["spool_entries_limit"]), (4, 4))
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

    def run_main(self, stdout):
        out = self.root / "inventory.json"
        argv = ["batch-inventory.py", "--root", str(self.root), "--results", str(self.results),
                "--mysql-defaults-file", "my.cnf", "--database", "boinc", "--nonce", "b" * 32,
                "--out", str(out)]
        done = subprocess.CompletedProcess(args=[], returncode=0, stdout=stdout)
        with patch.object(inventory.subprocess, "run", return_value=done), patch("sys.argv", argv):
            inventory.main()
        return json.loads(out.read_text())

    def test_per_app_counts_tell_other_apps_work_from_ocrs(self):
        # #1474: the controller budgets other apps' share of the global cap by these counts.
        # The total is their sum, from the same read, so the two can't disagree; work of no known
        # app counts only in the total.
        payload = self.run_main(f"2\napp\t\t1\napp\tusnm_ocr\t3\napp\tusnm_qwen\t2\nusnm_{self.job}\t0\n")
        self.assertEqual(payload["pending_workunits_by_app"], {"usnm_ocr": 3, "usnm_qwen": 2})
        self.assertEqual((payload["pending_workunits"], payload["active_hosts"], payload["pending_jobs"]),
                         (6, 2, [self.job]))
        self.assertEqual(self.run_main("2\n")["pending_workunits"], 0)
        for bad in ("app\tusnm_qwen\t-1", "app\tusnm qwen\t2", "app\tusnm_qwen\t2\napp\tusnm_qwen\t3",
                    "app\t\t1\napp\t\t2"):
            with self.assertRaisesRegex(ValueError, "invalid database inventory"):
                self.run_main(f"2\n{bad}\n")
        with self.assertRaisesRegex(ValueError, "invalid database inventory"):
            self.run_main("5\t2\n")
        with self.assertRaisesRegex(ValueError, "per-app"):
            inventory.inventory(self.root, self.results, [3, 2], "b" * 32, pending_by_app={"usnm_qwen": -1})

    def test_reports_the_ocr_versions_the_feeder_can_load(self):
        # #1687: the controller checks a bundle's pinned version against these before submitting.
        payload = self.run_main("2\nminver\t0\nversion\tarm64-apple-darwin\tdocker\t106\n"
                                "version\tx86_64-pc-linux-gnu\tdocker\t106\nversion\tx86_64-pc-linux-gnu\tdocker\t107\n"
                                "version\tx86_64-pc-linux-gnu\t\t108\n")
        self.assertEqual(payload["ocr_app_versions"], {"min_version": 0, "versions": [
            {"platform": "arm64-apple-darwin", "plan_class": "docker", "version_num": 106},
            {"platform": "x86_64-pc-linux-gnu", "plan_class": "docker", "version_num": 106},
            {"platform": "x86_64-pc-linux-gnu", "plan_class": "docker", "version_num": 107},
            {"platform": "x86_64-pc-linux-gnu", "plan_class": "", "version_num": 108}]})
        self.assertEqual(self.run_main("2\nminver\t106\n")["ocr_app_versions"], {"min_version": 106, "versions": []})
        # No usnm_ocr app row: nothing to report, and the controller says it is unchecked.
        self.assertNotIn("ocr_app_versions", self.run_main("2\n"))
        for bad in ("minver\t-1", "minver\t1\nminver\t2", "version\tlinux\tdocker\t106",
                    "minver\t0\nversion\tlinux\tdocker\t0", "minver\t0\nversion\tlinux docker\t106",
                    "minver\t0\nversion\tlinux\tdocker\t106\nversion\tlinux\tdocker\t106"):
            with self.subTest(bad=bad), self.assertRaisesRegex(ValueError, "app version inventory"):
                self.run_main(f"2\n{bad}\n")
        many = "".join(f"version\tlinux\tdocker\t{n}\n" for n in range(1, inventory.MAX_VERSIONS + 2))
        with self.assertRaisesRegex(ValueError, "app version inventory"):
            self.run_main("2\nminver\t0\n" + many)
        self.assertIn("app_version.deprecated = 0", inventory.QUERY)
        self.assertIn(f"LIMIT {inventory.MAX_VERSIONS + 1};", inventory.query())

    def test_reports_each_active_hosts_cpus(self):
        # #1780: a native OCR plan gives each of its hosts one unit per CPU.
        payload = self.run_main("2\nhost\t3\t2\nhost\t13\t24\nhost\t14\t-1\n")
        self.assertEqual(payload["host_cpus"], {"3": 2, "13": 24, "14": 0})
        self.assertEqual(self.run_main("0\n")["host_cpus"], {})
        for bad in ("host\t0\t4", "host\tx\t4", "host\t3\t2\nhost\t3\t4", "host\t3"):
            with self.subTest(bad=bad), self.assertRaisesRegex(ValueError, "host inventory"):
                self.run_main(f"2\n{bad}\n")
        many = "".join(f"host\t{n}\t1\n" for n in range(1, inventory.MAX_HOSTS + 2))
        with self.assertRaisesRegex(ValueError, "host inventory"):
            self.run_main("2\n" + many)
        self.assertIn(f"LIMIT {inventory.MAX_HOSTS + 1};", inventory.query())
        self.assertNotIn("host_cpus", self.payload())

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
        self.assertEqual(result["pending_ocr_workunits"], 2)
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

    def test_existing_workunits_have_their_own_cap_above_the_journal_cap(self):
        # Finished workunits stay in the database for delete_delay_hours, so more than MAX_JOBS
        # of them is normal (#1483); the cap that applies is the existing-workunit one.
        existing = [f"{n:064x}" for n in range(1, inventory.MAX_JOBS + 2)]
        payload = inventory.inventory(self.root, self.results, [3, 2], "b" * 32, (), existing)
        self.assertEqual(len(payload["existing_jobs"]), inventory.MAX_JOBS + 1)
        self.assertEqual(payload["existing_jobs_limit"], inventory.DEFAULT_MAX_EXISTING_WORKUNITS)
        with self.assertRaisesRegex(ValueError, "existing workunit inventory"):
            inventory.inventory(self.root, self.results, [3, 2], "b" * 32, (), existing,
                                existing_limit=inventory.MAX_JOBS)
        for limit in (0, inventory.MAX_EXISTING_WORKUNITS_CEILING + 1, "50000"):
            with self.assertRaisesRegex(ValueError, "existing workunit limit"):
                inventory.inventory(self.root, self.results, [3, 2], "b" * 32,
                                    existing_limit=limit)

    def test_the_existing_workunit_cap_is_read_from_the_environment(self):
        self.assertEqual(inventory.max_existing_workunits({}), inventory.DEFAULT_MAX_EXISTING_WORKUNITS)
        self.assertEqual(inventory.max_existing_workunits({"USNM_MAX_EXISTING_WORKUNITS": "75000"}), 75000)
        for value in ("0", "-1", "1e5", "abc", str(inventory.MAX_EXISTING_WORKUNITS_CEILING + 1)):
            with self.assertRaises(ValueError):
                inventory.max_existing_workunits({"USNM_MAX_EXISTING_WORKUNITS": value})
        self.assertIn("LIMIT 75001", inventory.query(75000))
        self.assertNotIn("{limit}", inventory.query())

    def test_a_truncated_workunit_read_is_refused_before_bundles_expand(self):
        # Bundles with no journals left add no pages, so the row count, not the page count, shows
        # that the LIMIT cut the read short.
        rows = "".join(f"usnm_ocrb_{n:064x}\t2\n" for n in (1, 2))
        with patch.dict(inventory.os.environ, {"USNM_MAX_EXISTING_WORKUNITS": "1"}):
            with self.assertRaisesRegex(ValueError, "existing workunit inventory"):
                self.run_main(f"2\n{rows}")
            payload = self.run_main(f"2\nusnm_ocrb_{1:064x}\t2\n")
            self.assertEqual((payload["existing_jobs"], payload["existing_workunits"]), ([], 1))

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
CREATE TEMPORARY TABLE workunit (assimilate_state INTEGER NOT NULL, name VARCHAR(254), appid INTEGER NOT NULL);
CREATE TEMPORARY TABLE host (id INTEGER NOT NULL PRIMARY KEY, rpc_time DOUBLE NOT NULL,
 p_ncpus INTEGER NOT NULL DEFAULT 0);
CREATE TEMPORARY TABLE app (id INTEGER NOT NULL PRIMARY KEY, name VARCHAR(254) NOT NULL,
 min_version INTEGER NOT NULL DEFAULT 0);
CREATE TEMPORARY TABLE platform (id INTEGER NOT NULL PRIMARY KEY, name VARCHAR(254) NOT NULL);
CREATE TEMPORARY TABLE app_version (appid INTEGER NOT NULL, platformid INTEGER NOT NULL,
 version_num INTEGER NOT NULL, plan_class VARCHAR(254) NOT NULL, deprecated SMALLINT NOT NULL);
INSERT INTO app (id, name) VALUES (1,'usnm_ocr'),(2,'usnm_qwen'),(3,'usnm_idle');
INSERT INTO platform VALUES (1,'x86_64-pc-linux-gnu');
INSERT INTO app_version VALUES (1,1,106,'docker',0),(1,1,107,'docker',1),(2,1,200,'docker',0);
INSERT INTO workunit VALUES (0,'other-app',2),(1,'usnm_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa',1),
 (2,'done',2),(2,'usnm_bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb',1),(9,'unknown',2),
 (0,'usnm_ocrb_cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc',1),(0,'usnm_ocrb_short',1),(0,'orphan',7);
INSERT INTO host VALUES (1,0,4),(2,UNIX_TIMESTAMP()-90000,8),(13,UNIX_TIMESTAMP(),24),(3,UNIX_TIMESTAMP()-1,2);
""" + inventory.query() + ";",
            text=True, capture_output=True, check=True,
        )
        self.assertEqual(result.stdout.strip(),
                         "2\nhost\t3\t2\nhost\t13\t24\nminver\t0\nversion\tx86_64-pc-linux-gnu\tdocker\t106\napp\t\t1\napp\tusnm_ocr\t3\napp\tusnm_qwen\t2\nusnm_" + "a" * 64 + "\t1\nusnm_"
                         + "b" * 64 + "\t2\nusnm_ocrb_" + "c" * 64 + "\t0")


if __name__ == "__main__":
    unittest.main()
