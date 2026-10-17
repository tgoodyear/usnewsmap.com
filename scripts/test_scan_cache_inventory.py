"""Offline bounded inventory and crash recovery tests; no image hashing or services."""

import json
from contextlib import closing
from pathlib import Path
import shutil
import subprocess
import sys
import unittest
from unittest import mock
import uuid

from scripts import data_catalog as c
from scripts import scan_cache_inventory as inv


ROOT = Path(__file__).resolve().parents[1]


class InventoryTests(unittest.TestCase):
    def setUp(self):
        self.root = ROOT / (".inventory-test-" + uuid.uuid4().hex)
        self.root.mkdir(mode=0o700)
        self.scans = self.root / "scans"
        self.scans.mkdir()
        self.index = self.root / "index.jsonl"
        self.db = self.root / "catalog.sqlite"
        self.work = self.root / "inventory"
        self.write_rows(5)

    def tearDown(self):
        shutil.rmtree(self.root)

    def write_rows(self, count, tail=b""):
        rows = []
        for number in range(1, count + 1):
            rows.append(c.canonical({
                "doc_id": f"sn123_1900-01-01_ed-1_seq-{number}", "batch": "test",
                "batch_version": 1, "loc_url": "https://example.invalid/discard",
                "resolution": "full", "fetched_at": "2026-01-01T00:00:00Z",
                "bytes": 4, "sha256": "ab" * 32, "download_ms": 1,
            }) + "\n")
        self.index.write_bytes("".join(rows).encode() + tail)

    def run_import(self, **kwargs):
        return inv.run(self.db, self.index, self.scans, self.work, "test-stat",
                       kwargs.pop("cache_sets", ["keep", "buffer"]),
                       kwargs.pop("max_batches", 10), **kwargs)

    def count(self):
        with closing(c.connect(self.db)) as conn:
            return conn.execute("SELECT count(*) FROM events").fetchone()[0]

    def evidence(self):
        return sorted(self.work.glob("batch-*/*.jsonl"))

    def test_bounded_multiset_resume_and_rebuild(self):
        scan = self.scans / "keep/test/sn123/1900-01-01/1/1.jpg"
        scan.parent.mkdir(parents=True)
        scan.write_bytes(b"test")
        with mock.patch.object(inv, "ROWS", 2):
            first = self.run_import(max_batches=1)
            self.assertEqual((first["rows"], first["batches"], first["finished"]), (2, 1, False))
            result = self.run_import()
        self.assertEqual((result["rows"], result["batches"], result["finished"]), (5, 3, True))
        self.assertEqual(self.count(), 10)
        self.assertFalse(result["checksum_verified"])
        self.assertIn("partial", result["coverage"])
        snapshot = c.snapshot(self.db)
        self.assertEqual(snapshot["artifacts"]["verified"], 0)
        data = c.query(self.db, "artifact", {})["items"]
        self.assertEqual(sum(i["data"]["state"] == "present" for i in data), 1)
        before = {p: p.read_bytes() for p in self.evidence()}
        self.assertTrue(self.run_import()["finished"])
        self.assertEqual(before, {p: p.read_bytes() for p in self.evidence()})
        rebuilt = self.root / "rebuilt.sqlite"
        for path in self.evidence():
            if path.name != "index.jsonl":
                self.assertNotIn(b"https://", path.read_bytes())
                c.ingest(rebuilt, path)
                self.assertTrue(c.ingest(rebuilt, path)["replayed"])
        rebuilt_snapshot = c.snapshot(rebuilt)
        rebuilt_snapshot.pop("generated_at")
        snapshot.pop("generated_at")
        self.assertEqual(rebuilt_snapshot, snapshot)

    def test_crash_after_commit_before_checkpoint_replays_without_stat(self):
        original = inv.save

        def crash(work, state):
            if state["batches"] == 1:
                raise OSError("simulated power loss")
            return original(work, state)

        with mock.patch.object(inv, "save", side_effect=crash), self.assertRaises(OSError):
            self.run_import()
        self.assertEqual(self.count(), 10)
        with mock.patch.object(c, "write_cached_scan_evidence", side_effect=AssertionError("restat")):
            self.assertTrue(self.run_import()["finished"])
        self.assertEqual(self.count(), 10)

    def test_crash_between_sets_and_partial_build(self):
        original = c.write_cached_scan_evidence
        calls = 0

        def crash(*args):
            nonlocal calls
            calls += 1
            if calls == 2:
                Path(args[3]).write_bytes(b'{"incomplete"')
                raise OSError("interrupted write")
            return original(*args)

        with mock.patch.object(c, "write_cached_scan_evidence", side_effect=crash):
            with self.assertRaises(OSError):
                self.run_import()
        self.assertEqual(self.count(), 5)
        first = (self.work / "batch-00000000/buffer.jsonl").read_bytes()
        self.assertTrue(self.run_import()["finished"])
        self.assertEqual(self.count(), 10)
        self.assertEqual((self.work / "batch-00000000/buffer.jsonl").read_bytes(), first)

    def test_crash_after_publication_before_ingestion(self):
        with mock.patch.object(c, "ingest", side_effect=OSError("before transaction")):
            with self.assertRaises(OSError):
                self.run_import(cache_sets=["keep"])
        before = (self.work / "batch-00000000/keep.jsonl").read_bytes()
        original_sync = inv.sync_dir
        original_ingest = c.ingest
        synced = []

        def sync(path):
            synced.append(path)
            original_sync(path)

        def ingest(*args):
            self.assertIn(self.work / "batch-00000000", synced)
            return original_ingest(*args)

        with mock.patch.object(c, "write_cached_scan_evidence", side_effect=AssertionError("restat")), \
                mock.patch.object(inv, "sync_dir", side_effect=sync), \
                mock.patch.object(c, "ingest", side_effect=ingest):
            self.assertTrue(self.run_import(cache_sets=["keep"])["finished"])
        self.assertEqual(self.count(), 5)
        self.assertEqual((self.work / "batch-00000000/keep.jsonl").read_bytes(), before)

    def test_append_frozen_prefix_and_partial_last_line(self):
        self.write_rows(3, b'{"partial":')
        first = self.run_import()
        self.assertTrue(first["finished"])
        self.assertEqual(first["rows"], 3)
        self.assertEqual(first["uncovered_bytes"], len(b'{"partial":'))
        with self.index.open("ab") as stream:
            stream.write(b"true}\n")
        second = self.run_import()
        self.assertEqual(second["rows"], 3)
        self.assertEqual(second["appended_bytes_not_covered"], len(b"true}\n"))

    def test_reject_mutation_truncation_replacement_and_config_change(self):
        with mock.patch.object(inv, "ROWS", 1):
            self.run_import(max_batches=1)
        raw = self.index.read_bytes()
        self.index.write_bytes(raw.replace(b"sn123", b"sn124"))
        with self.assertRaisesRegex(c.CatalogError, "prefix changed"):
            self.run_import()
        self.index.write_bytes(raw[:1])
        with self.assertRaisesRegex(c.CatalogError, "truncated"):
            self.run_import()
        self.index.write_bytes(raw)
        with self.assertRaisesRegex(c.CatalogError, "configuration changed"):
            self.run_import(cache_sets=["keep"])
        replacement = self.root / "replacement"
        replacement.write_bytes(raw)
        replacement.replace(self.index)
        with self.assertRaisesRegex(c.CatalogError, "replaced"):
            self.run_import()

    def test_lost_catalog_requires_explicit_rebuild(self):
        self.run_import()
        self.db.rename(self.root / "old.sqlite")
        with self.assertRaises(c.CatalogError):
            self.run_import()
        c.connect(self.db, writable=True).close()
        with self.assertRaisesRegex(c.CatalogError, "checkpoint missing"):
            self.run_import()

    def test_rebuild_of_only_final_slice_is_rejected(self):
        with mock.patch.object(inv, "ROWS", 2):
            self.run_import()
        self.db.rename(self.root / "complete.sqlite")
        for cache_set in ("keep", "buffer"):
            c.ingest(self.db, self.work / "batch-00000002" / (cache_set + ".jsonl"))
        with self.assertRaisesRegex(c.CatalogError, "checkpoint missing"):
            self.run_import()

    def test_resume_recovers_hot_sqlite_rollback_journal(self):
        with mock.patch.object(inv, "ROWS", 2):
            self.run_import(max_batches=1)
        process = subprocess.run([sys.executable, "-c", """
import os, sqlite3, sys
conn = sqlite3.connect(sys.argv[1])
conn.execute("PRAGMA cache_size=1")
conn.execute("BEGIN IMMEDIATE")
conn.execute("UPDATE latest SET data=printf('%01000000d',1)")
os._exit(0)
""", str(self.db)], cwd=ROOT, capture_output=True, text=True, timeout=10)
        self.assertEqual(process.returncode, 0, process.stderr)
        journal = Path(str(self.db) + "-journal")
        self.assertTrue(journal.is_file())
        self.assertTrue(self.run_import()["finished"])
        self.assertEqual(self.count(), 10)
        self.assertEqual(len(c.query(self.db, "artifact", {})["items"]), 10)

    def test_created_ancestors_are_durable_before_import(self):
        self.work = self.root / "nested" / "inventory"
        synced = []
        original_sync = inv.sync_dir
        original_ingest = c.ingest

        def sync(path):
            synced.append(path)
            original_sync(path)

        def ingest(*args):
            self.assertIn(self.root, synced)
            self.assertIn(self.root / "nested", synced)
            self.assertIn(self.work, synced)
            return original_ingest(*args)

        with mock.patch.object(inv, "sync_dir", side_effect=sync), \
                mock.patch.object(c, "ingest", side_effect=ingest):
            self.assertTrue(self.run_import()["finished"])

    def test_invalid_rows_publish_no_evidence(self):
        for raw in (b"invalid\n", b"x" * (c.MAX_LINE + 1) + b"\n"):
            with self.subTest(raw_size=len(raw)):
                self.index.write_bytes(raw)
                with self.assertRaises(c.CatalogError):
                    self.run_import()
                self.assertFalse(any(p.name in ("keep.jsonl", "buffer.jsonl")
                                     for p in self.evidence()))
                shutil.rmtree(self.work)

    def test_byte_limit_and_lock(self):
        with mock.patch.object(inv, "BYTES", 700):
            result = self.run_import(max_batches=1)
        self.assertLessEqual((self.work / "batch-00000000/index.jsonl").stat().st_size, 700)
        self.assertFalse(result["finished"])
        with inv.lock(self.work), self.assertRaisesRegex(c.CatalogError, "already in use"):
            self.run_import()

    def test_index_exceeds_original_limits_without_oversized_batches(self):
        row = json.loads(self.index.read_text().splitlines()[0])
        row["user_agent"] = "x" * 2000
        raw = (c.canonical(row) + "\n").encode()
        with self.index.open("wb") as stream:
            for _ in range(c.MAX_EVENTS + 1):
                stream.write(raw)
        self.assertGreater(self.index.stat().st_size, c.MAX_BATCH)
        result = self.run_import(max_batches=1)
        self.assertEqual(result["rows"], inv.ROWS)
        self.assertFalse(result["finished"])
        self.assertEqual(self.count(), inv.ROWS * 2)
        for path in self.evidence():
            self.assertLessEqual(path.stat().st_size, c.MAX_BATCH)

    def test_cli(self):
        process = subprocess.run([
            sys.executable, str(ROOT / "scripts/scan_cache_inventory.py"),
            "--db", str(self.db), "--index", str(self.index),
            "--scan-root", str(self.scans), "--work-dir", str(self.work),
            "--source", "test-stat", "--cache-set", "keep", "--max-batches", "1",
        ], cwd=ROOT, capture_output=True, text=True, timeout=10)
        self.assertEqual(process.returncode, 0, process.stderr)
        self.assertEqual(json.loads(process.stdout)["rows"], 5)


if __name__ == "__main__":
    unittest.main()
