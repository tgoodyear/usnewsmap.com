"""Offline bounded transport, immutable retention and catalog recovery checks."""

import copy
import hashlib
import json
import os
from pathlib import Path
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch

from scripts.corpus import data_catalog as c
from scripts.corpus import worker_evidence_sync as w


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

    def test_interrupted_jsonl_write_cannot_publish_partial_evidence(self):
        self.capture()
        bundle = w.export(self.spool, "")
        factory = c.tempfile.NamedTemporaryFile

        class Interrupted:
            def __init__(self, *args, **kwargs):
                self.file = factory(*args, **kwargs)

            def __enter__(self):
                return self

            def __getattr__(self, name):
                return getattr(self.file, name)

            def __exit__(self, *args):
                return self.file.__exit__(*args)

            def write(self, raw):
                self.file.write(raw.splitlines(keepends=True)[0])
                self.file.flush()
                raise OSError("interrupted after execution, before RunRecord")

        with patch.object(c.tempfile, "NamedTemporaryFile", Interrupted):
            with self.assertRaises(OSError):
                w.receive(self.db, self.retained, "worker-test", bundle)
        self.assertEqual(list(self.retained.glob("*.jsonl")), [])
        self.assertEqual(w.receive(self.db, self.retained, "worker-test", bundle)["inserted"], 2)
        self.assertEqual(len(c.query(self.db, "execution", {})["items"]), 1)
        self.assertEqual(len(c.query(self.db, "run", {})["items"]), 1)

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

    def test_same_capture_from_two_sources_has_independent_batch_identity(self):
        self.capture()
        bundle = w.export(self.spool, "")
        w.receive(self.db, self.retained / "first", "first", bundle)
        w.receive(self.db, self.retained / "second", "second", bundle)
        self.assertEqual(len(c.query(self.db, "execution", {})["items"]), 2)
        self.assertEqual(w.receive(self.db, self.retained / "second", "second", bundle)["inserted"], 0)

    def test_directory_durability_failure_never_becomes_success_on_replay(self):
        self.capture()
        bundle = w.export(self.spool, "")
        with patch.object(w, "sync_directory_tree", side_effect=OSError("directory fsync failed")) as barrier:
            for _ in range(3):
                with self.assertRaises(OSError):
                    w.receive(self.db, self.retained, "worker-test", bundle)
            self.assertEqual(barrier.call_count, 3)
        self.assertEqual(w.receive(self.db, self.retained, "worker-test", bundle)["inserted"], 0)

    def test_fifo_is_rejected_before_any_blocking_read(self):
        name = "a" * 64 + ".json"
        os.mkfifo(self.spool / name)
        with self.assertRaises(c.CatalogError):
            w.export(self.spool, "")
        with self.assertRaises(c.CatalogError):
            w.acknowledge(self.spool, [(name, "b" * 64)])

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

    def test_sync_acknowledges_only_after_snapshot_and_replays_on_snapshot_failure(self):
        self.capture()
        bundle = w.export(self.spool, "")
        state = self.root / "sync"
        snapshot = self.root / "snapshot.json"
        args = SimpleNamespace(db=self.db, state_dir=state, source="worker-test",
                               host="root@worker.example.invalid", identity="dedicated-key",
                               snapshot=snapshot)

        def transfer(*_, stdout, **kwargs):
            stdout.write(c.canonical(bundle).encode())

        with patch.object(w.subprocess, "run", side_effect=transfer), \
                patch.object(w.subprocess, "check_output") as ack:
            with patch.object(c, "atomic_snapshot", side_effect=OSError("disk failure")):
                with self.assertRaises(OSError):
                    w.sync(args)
            ack.assert_not_called()
            ack.return_value = '{"acknowledged":1}'
            self.assertEqual(w.sync(args)["inserted"], 0)
            ack.assert_called_once()
            self.assertIn("ack ", ack.call_args.args[0][-1])
            self.assertTrue(snapshot.exists())


if __name__ == "__main__":
    unittest.main()
