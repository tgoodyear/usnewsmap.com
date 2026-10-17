"""Offline bounded transport, immutable retention and catalog recovery checks."""

import copy
import hashlib
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

from scripts import data_catalog as c
from scripts import worker_evidence_sync as w


class SyncTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.root = Path(self.tmp.name)
        self.spool = self.root / "spool"
        self.spool.mkdir()
        self.retained = self.root / "retained"
        self.db = self.root / "catalog.sqlite"
        self.fixture = json.loads((Path(__file__).parent /
                                   "fixtures/data_catalog/worker-success.json").read_text())

    def tearDown(self):
        self.tmp.cleanup()

    def capture(self, suffix=""):
        metadata = copy.deepcopy(self.fixture)
        metadata["run_id"] += suffix
        metadata["run_record"]["run_id"] = metadata["run_id"]
        name = hashlib.sha256(metadata["run_id"].encode()).hexdigest() + ".json"
        (self.spool / name).write_text(c.canonical(metadata) + "\n")
        return name

    def test_capture_replay_and_rebuild_are_identical(self):
        self.capture()
        bundle = w.export(self.spool, "")
        first = w.receive(self.db, self.retained, "worker-test", bundle)
        self.assertEqual(first["inserted"], 2)
        self.assertEqual(w.receive(self.db, self.retained, "worker-test", bundle)["inserted"], 0)
        original = c.query(self.db, "run", {})
        rebuilt = self.root / "rebuilt.sqlite"
        w.receive(rebuilt, self.retained, "worker-test", bundle)
        self.assertEqual(original, c.query(rebuilt, "run", {}))
        self.assertEqual(len(list(self.retained.glob("*.json"))), 1)
        self.assertEqual(len(list(self.retained.glob("*.jsonl"))), 1)
        self.assertFalse(any(s["count"] is not None for s in c.snapshot(self.db)["stages"]
                             if s["id"] in c.STAGES))

    def test_pagination_ignores_pending_and_revisits_new_lower_names(self):
        for n in range(5):
            self.capture(str(n))
        (self.spool / ".pending-capture").write_text("incomplete")
        with patch.object(w, "MAX_FILES", 2):
            seen, cursor = [], ""
            while True:
                page = w.export(self.spool, cursor)
                seen.extend(i["name"] for i in page["files"])
                cursor = w.receive(self.db, self.retained, "worker-test", page)["next"]
                if not cursor:
                    break
            self.assertEqual(len(set(seen)), 5)
            self.assertEqual(len(w.export(self.spool, "")["files"]), 2)

    def test_failure_after_evidence_publication_replays(self):
        self.capture()
        bundle = w.export(self.spool, "")
        with patch.object(c, "ingest", side_effect=OSError("interrupted")):
            with self.assertRaises(OSError):
                w.receive(self.db, self.retained, "worker-test", bundle)
        self.assertEqual(w.receive(self.db, self.retained, "worker-test", bundle)["inserted"], 2)

    def test_rejects_corruption_symlinks_and_oversized_captures(self):
        name = self.capture()
        bundle = w.export(self.spool, "")
        broken = copy.deepcopy(bundle)
        broken["files"][0]["sha256"] = "a" * 64
        with self.assertRaises(c.CatalogError):
            w.receive(self.db, self.retained, "worker-test", broken)
        self.assertFalse(self.retained.exists() and list(self.retained.iterdir()))
        path = self.spool / name
        path.unlink()
        path.symlink_to(self.root / "outside")
        with self.assertRaises(OSError):
            w.export(self.spool, "")
        path.unlink()
        path.write_bytes(b"x" * (c.MAX_LINE + 1))
        with self.assertRaises(c.CatalogError):
            w.export(self.spool, "")

    def test_retained_capture_conflicts_fail_explicitly(self):
        self.capture()
        bundle = w.export(self.spool, "")
        w.receive(self.db, self.retained, "worker-test", bundle)
        next(self.retained.glob("*.json")).write_text("changed")
        with self.assertRaises(c.CatalogError):
            w.receive(self.db, self.retained, "worker-test", bundle)

    def test_acknowledgement_is_checksum_checked_idempotent_and_not_deletion(self):
        name = self.capture()
        bundle = w.export(self.spool, "")
        checksum = bundle["files"][0]["sha256"]
        with self.assertRaises(c.CatalogError):
            w.acknowledge(self.spool, [(name, "a" * 64)])
        self.assertTrue((self.spool / name).exists())
        w.receive(self.db, self.retained, "worker-test", bundle)
        self.assertEqual(w.acknowledge(self.spool, [(name, checksum)]), {"acknowledged": 1})
        self.assertEqual(w.export(self.spool, "")["files"], [])
        self.assertEqual((self.spool / "acknowledged" / name).read_bytes(),
                         (self.retained / name).read_bytes())
        self.assertEqual(w.acknowledge(self.spool, [(name, checksum)]), {"acknowledged": 1})


if __name__ == "__main__":
    unittest.main()
