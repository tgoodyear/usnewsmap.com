"""Stock counts survive bounded resume without hashing, touching, or trusting scans."""

import json
import os
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

from scripts.corpus import scan_stock_inventory as stock


class StockTests(unittest.TestCase):
    def setUp(self):
        self.root = Path(self.enterContext(tempfile.TemporaryDirectory()))
        self.scans = self.root / "scans"
        for value in ("keep", "buffer"):
            (self.scans / value).mkdir(parents=True)
        self.work = self.root / "work"

    def scan(self, name, body=b"scan"):
        path = self.scans / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(body)
        return path

    def run_stock(self, **kwargs):
        return stock.run(self.scans, self.work, ["keep", "buffer"], **kwargs)

    def test_bounded_resume_counts_unindexed_scans_and_deduplicates_formats_and_hardlinks(self):
        scan = self.scan("keep/batch/sn123/1900-01-01/1/1.jp2")
        other = self.scan("buffer/batch/sn123/1900-01-01/1/1.jpg", b"jpeg")
        os.link(scan, other.with_name("1.jp2"))
        self.scan("buffer/batch/sn123/1900-01-01/1/1.pct50.jpg", b"scaled")
        self.scan("buffer/unplaced.jpg", b"x")
        scan.with_name("1.jp2.json").write_text("{}")
        first = self.run_stock(max_entries=2)
        self.assertFalse(first["finished"])
        self.assertEqual(first["entries_this_invocation"], 2)
        for _ in range(20):
            report = self.run_stock(max_entries=2)
            if report["finished"]:
                break
        self.assertTrue(report["finished"])
        self.assertEqual(report["cache_sets"]["buffer"]["scan_files"], 4)
        self.assertEqual(report["cache_sets"]["buffer"]["distinct_page_paths"], 1)
        self.assertEqual(report["cache_sets"]["buffer"]["scan_files_without_page_path"], 1)
        self.assertEqual(report["distinct_observed_inodes"], 4)
        self.assertEqual(report["distinct_inode_max_bytes"], 15)
        self.assertFalse(report["checksum_verified"])
        self.assertFalse(report["atomic_snapshot"])
        self.assertFalse(report["corpus_complete"])
        with patch.object(stock, "now", return_value="2099-01-01T00:00:00+00:00"):
            no_op = self.run_stock()
        self.assertEqual(no_op["entries_this_invocation"], 0)
        self.assertEqual(no_op["observed_until"], report["observed_until"])
        self.assertEqual(no_op["generated_at"], "2099-01-01T00:00:00+00:00")
        self.assertEqual(json.loads((self.work / "report.json").read_text())["finished"], True)
        self.assertEqual(scan.read_bytes(), b"scan")

    def test_symlinks_are_reported_not_followed(self):
        outside = self.root / "outside"
        outside.mkdir()
        (outside / "not-stock.jpg").write_bytes(b"secret")
        (self.scans / "keep/link").symlink_to(outside, target_is_directory=True)
        report = self.run_stock()
        self.assertEqual(report["cache_sets"]["keep"]["entry_kinds"], {"symlink": 1})
        self.assertEqual(report["distinct_observed_inodes"], 0)

    def test_error_rolls_back_slice_and_retry_does_not_double_count(self):
        self.scan("keep/1.jpg")
        self.scan("keep/2.jpg")
        original = stock.observe

        def fail(root, relative, parent_fd=None):
            if relative.endswith("2.jpg"):
                raise PermissionError("test")
            return original(root, relative, parent_fd)

        with patch.object(stock, "observe", side_effect=fail), self.assertRaises(PermissionError):
            self.run_stock()
        report = self.run_stock()
        self.assertTrue(report["finished"])
        self.assertEqual(report["cache_sets"]["keep"]["scan_files"], 2)

    def test_deadline_checkpoints_and_changed_configuration_refused(self):
        self.scan("keep/1.jpg")
        with patch.object(stock.time, "monotonic", side_effect=[0, 0, 21, 21]):
            report = self.run_stock(max_seconds=20)
        self.assertFalse(report["finished"])
        self.assertEqual(report["entries_this_invocation"], 0)
        self.assertTrue(self.run_stock()["finished"])
        with self.assertRaisesRegex(stock.inventory.c.CatalogError, "configuration"):
            stock.run(self.scans, self.work, ["keep"])

    def test_replaced_pending_directory_fails_without_following_symlink(self):
        directory = self.scans / "keep/batch"
        directory.mkdir()
        self.run_stock(max_entries=1)
        directory.rmdir()
        directory.symlink_to(self.scans / "buffer", target_is_directory=True)
        with self.assertRaises(OSError):
            self.run_stock()

    def test_real_directory_replacement_refused(self):
        directory = self.scans / "keep/batch"
        directory.mkdir()
        self.run_stock(max_entries=1)
        directory.rename(directory.with_name("old"))
        directory.mkdir()
        with self.assertRaisesRegex(stock.inventory.c.CatalogError, "directory changed"):
            self.run_stock()

    def test_wide_directory_resumes_without_relisting_and_reports_without_history_scans(self):
        for number in range(10):
            self.scan(f"keep/{number}.jpg")
        self.run_stock(max_entries=1)
        original_capture = stock.capture_listing

        def capture(conn, fd, relative):
            self.assertNotEqual(relative, "keep", "unfinished directory must not be relisted")
            return original_capture(conn, fd, relative)

        original_summary = stock.summary
        queries = []

        def summary(conn, *args):
            conn.set_trace_callback(queries.append)
            return original_summary(conn, *args)

        with patch.object(stock, "capture_listing", side_effect=capture), \
                patch.object(stock, "summary", side_effect=summary):
            for _ in range(12):
                report = self.run_stock(max_entries=1)
                if report["finished"]:
                    break
        self.assertTrue(report["finished"])
        self.assertEqual(report["cache_sets"]["keep"]["scan_files"], 10)
        self.assertFalse(any("FROM inodes" in query or "FROM pages" in query for query in queries))

    def test_oversized_listing_fails_explicitly_without_partial_counts(self):
        for number in range(3):
            self.scan(f"keep/{number}.jpg")
        with patch.object(stock, "MAX_DIRECTORY_ENTRIES", 2), \
                self.assertRaisesRegex(stock.inventory.c.CatalogError, "directory exceeds"):
            self.run_stock()
        self.assertEqual(self.run_stock()["cache_sets"]["keep"]["scan_files"], 3)

    def test_inode_size_disagreement_updates_totals_once(self):
        scan = self.scan("keep/1.jpg")
        os.link(scan, self.scans / "keep/2.jpg")
        self.run_stock(max_entries=1)
        scan.write_bytes(b"longer")
        report = self.run_stock()
        self.assertEqual(report["distinct_observed_inodes"], 1)
        self.assertEqual(report["distinct_inode_max_bytes"], 6)
        self.assertEqual(report["inodes_with_size_disagreement"], 1)


if __name__ == "__main__":
    unittest.main()
