"""Offline replenishment/restart tests; never touch a live archive or converter."""

import importlib.util
import json
from pathlib import Path
import tempfile
import types
import unittest
from unittest import mock


HERE = Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location("refresh", HERE / "quality-jpeg-refresh.py")
refresh = importlib.util.module_from_spec(spec)
spec.loader.exec_module(refresh)


class RefreshTest(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.work = self.root / "state"
        self.scans = self.root / "scans"
        self.scans.mkdir()
        self.work.mkdir()
        self.db = refresh.connect(self.work, "test-recipe", 2)
        self.addCleanup(lambda: self.db.close())
        self.index = self.root / "full.jsonl"
        self.index.touch()
        self.done = self.root / "done.txt"
        self.done.touch()

    def scan(self, batch, seq, ext="jp2", kept=True):
        doc = f"sn1_1900-01-01_ed-1_seq-{seq}"
        path = f"keep/{batch}/sn1/1900-01-01/1/{seq}.{'jpg' if ext == 'jpeg' else ext}"
        if kept:
            target = self.scans / path
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_bytes(b"synthetic master")
        with self.index.open("a") as out:
            out.write(json.dumps({"doc_id": doc, "batch": batch,
                                  "format": ext, "resolution": "full"}) + "\n")
        return path

    def read_index(self, limit=100):
        return refresh.consume(self.db, self.index, limit,
                               lambda raw: refresh.ingest_scan(self.db, raw, self.scans))

    def lists(self):
        refresh.publish(self.db, self.work, 2)
        return [(self.work / f"host-{host}.txt").read_text().splitlines() for host in range(2)]

    def fill(self, manifest, damage=(), current=None, low=0, high=2):
        ordered = refresh.batch_order(self.db, manifest, damage)
        return refresh.replenish(self.db, ordered, len(manifest), 2,
                                 current or {}, 2, low, high)

    def test_orders_like_existing_builder_and_replenishes_after_restart(self):
        names = [self.scan("a", seq) for seq in [2, 10, 3]]
        names += [self.scan("b", seq) for seq in [1, 2]]
        names += [self.scan("c", 1, ext="jpg")]
        self.read_index()
        self.fill([("a", 1, 3), ("b", 1, 2), ("c", 1, 1)])
        before = self.lists()
        self.assertEqual(before, [[names[1], names[0]], [names[3], names[4]]])
        self.assertEqual(set(before[0]) & set(before[1]), set())
        self.assertEqual(self.fill([("a", 1, 3), ("b", 1, 2), ("c", 1, 1)]), 0)
        self.assertEqual(self.lists(), before, "full lists stay stable")
        self.done.write_text("".join(path + "\tuploaded\n" for path in before[0]))
        refresh.consume(self.db, self.done, 100, lambda raw: refresh.ingest_done(self.db, raw))
        self.db.close()
        self.db = refresh.connect(self.work, "test-recipe", 2)
        self.fill([("a", 1, 3), ("b", 1, 2), ("c", 1, 1)])
        after = self.lists()
        self.assertEqual(after[0], [names[2]])
        self.assertEqual(after[1], before[1])
        self.assertEqual(self.read_index(), (0, True), "restart doesn't reread the corpus")

    def test_new_damage_batch_manifest_and_completed_pages(self):
        old = self.scan("old", 1)
        new = self.scan("new", 1)
        self.scan("buffer-only", 1, kept=False)
        self.read_index()
        self.done.write_text(old + "\talready-there\n")
        refresh.consume(self.db, self.done, 100, lambda raw: refresh.ingest_done(self.db, raw))
        self.fill([("old", 1, 1)])
        self.assertEqual(self.lists(), [[], []])
        self.fill([("old", 1, 1)], ["audit/damage/v2/new_ver02.parquet"])
        self.assertEqual(self.lists(), [[new], []])
        fresh = self.scan("new-manifest", 1)
        self.read_index()
        self.fill([("old", 1, 1), ("new-manifest", 1, 1)])
        self.assertIn(fresh, sum(self.lists(), []))
        self.assertNotIn(old, sum(self.lists(), []))

    def test_normal_jpeg_rows_do_not_block_mixed_index_or_rotation(self):
        first = self.scan("a", 1)
        jpeg = self.scan("jpeg-only", 1, ext="jpeg")
        last = self.scan("b", 1)
        self.assertEqual(self.read_index(), (3, True))
        ordered = refresh.batch_order(self.db, [("a", 1, 1)],
                                      ["audit/damage/v2/jpeg-only_ver01.parquet"])
        self.assertIn(("jpeg-only", 1), [key for key, _ in ordered])
        self.fill([("a", 1, 1), ("b", 1, 1)])
        self.assertEqual(set(sum(self.lists(), [])), {first, last})
        self.assertNotIn(jpeg, sum(self.lists(), []))

    def test_manifest_versions_and_separate_damage_version_keep_distinct_membership(self):
        first, second, tier1 = [self.scan("a", seq) for seq in range(1, 4)]
        self.read_index()
        self.db.executemany("INSERT INTO manifest_keys VALUES(?,?)", [("a", 1), ("a", 2)])
        self.db.executemany("INSERT INTO manifest_pages VALUES(?,?,?)",
                            [(first, "a", 1), (second, "a", 2)])
        # Replay indexed rows after the frozen membership has been loaded.
        for row in self.index.read_text().splitlines():
            refresh.ingest_scan(self.db, row, self.scans)
        ordered = refresh.batch_order(self.db, [("a", 1, 1), ("a", 2, 1)], [])
        self.assertEqual(list(refresh.candidates(self.db, ordered, 2, 1,
                                                {0: "a_ver01"}, 10)), [second])
        with_damage = refresh.batch_order(self.db, [("a", 1, 1), ("a", 2, 1)],
                                           ["audit/damage/v2/a_ver03.parquet"])
        actual = list(refresh.candidates(self.db, with_damage, 2, 1,
                                         {0: "a_ver02"}, 10))
        rows = [("a", 1, first), ("a", 2, second)]
        legacy_tier1 = refresh.ordering.tier1_batches(
            [("a", 1), ("a", 2)], ["audit/damage/v2/a_ver03.parquet"],
            [first, second, tier1])
        legacy_order = refresh.ordering.ordered_batches(rows, legacy_tier1)
        expected = refresh.ordering.interleave(refresh.ordering.shard_queues(
            rows, 1, {}, set(), legacy_tier1,
            remaining=refresh.ordering.resolve_current(legacy_order, 1, {0: "a_ver02"},
                                                        n_manifest=2)))
        self.assertEqual(actual, expected)
        self.fill([("a", 1, 1), ("a", 2, 1)], ["audit/damage/v2/a_ver03.parquet"])
        self.assertEqual(self.db.execute("SELECT count(*) FROM manifest_ready").fetchone()[0], 0)
        refresh.ingest_done(self.db, first + "\tuploaded")
        refresh.ingest_scan(self.db, self.index.read_text().splitlines()[0], self.scans)
        self.assertEqual(self.db.execute("SELECT count(*) FROM manifest_ready").fetchone()[0], 0)

    def test_cursor_is_bounded_partial_lines_retry_and_invalid_rows_rollback(self):
        self.scan("a", 1)
        self.scan("a", 2)
        self.assertEqual(self.read_index(limit=1), (1, False))
        self.assertEqual(self.read_index(limit=1), (1, True))
        with self.index.open("a") as out:
            out.write('{"doc_id":')
        self.assertEqual(self.read_index(), (0, False))
        with self.index.open("a") as out:
            out.write('"invalid","batch":"a"}\n')
        cursor = self.db.execute("SELECT offset FROM streams").fetchone()
        with self.assertRaisesRegex(ValueError, "invalid indexed"):
            self.read_index()
        self.assertEqual(self.db.execute("SELECT offset FROM streams").fetchone(), cursor)

    def test_replaced_truncated_and_changed_sources_are_refused(self):
        self.scan("a", 1)
        self.read_index()
        original = self.index.read_bytes()
        for raw in [b"", original.replace(b'"a"', b'"b"')]:
            self.index.write_bytes(raw)
            with self.assertRaisesRegex(ValueError, "replaced/truncated/changed"):
                self.read_index()
        self.index.write_bytes(original)
        replacement = self.index.with_suffix(".replacement")
        replacement.write_bytes(original)
        replacement.replace(self.index)
        with self.assertRaisesRegex(ValueError, "replaced/truncated/changed"):
            self.read_index()

    def test_concurrent_refresh_is_refused_and_atomic_publish_recovers(self):
        with refresh.locked(self.work):
            with self.assertRaisesRegex(ValueError, "another JPEG refresh"):
                with refresh.locked(self.work):
                    self.fail("overlapping refresh acquired lock")
        for seq in range(1, 5):
            self.scan("a", seq)
        self.read_index()
        self.fill([("a", 1, 4)])
        with mock.patch.object(refresh.os, "replace", side_effect=OSError("interrupted publish")):
            with self.assertRaisesRegex(OSError, "interrupted"):
                self.lists()
        self.assertFalse((self.work / "host-0.txt").exists())
        after = self.lists()
        self.assertEqual(len(sum(after, [])), 4)
        self.assertEqual(len(set(sum(after, []))), 4)
        self.assertEqual(self.fill([("a", 1, 4)]), 0, "ownership survived failed publication")

    def test_recipe_change_and_unknown_current_batch_are_refused(self):
        with self.assertRaisesRegex(ValueError, "recipe/host count changed"):
            refresh.connect(self.work, "other-recipe", 2)
        with self.assertRaises(SystemExit):
            self.fill([("a", 1, 4)], current={0: "unknown_ver01"})

    def test_candidate_query_uses_keyed_pending_index(self):
        plan = self.db.execute("""
            EXPLAIN QUERY PLAN SELECT path FROM pending
            WHERE owner IS NULL AND batch=?
            ORDER BY doc,path LIMIT ?
        """, ("batch", 200)).fetchall()
        details = " ".join(row[-1] for row in plan)
        self.assertIn("USING COVERING INDEX pending_order", details)
        self.assertNotIn("TEMP B-TREE", details)
        plan = self.db.execute("""
            EXPLAIN QUERY PLAN SELECT path FROM manifest_ready
            WHERE batch=? AND version=? ORDER BY doc,path LIMIT ?
        """, ("batch", 1, 200)).fetchall()
        details = " ".join(row[-1] for row in plan)
        self.assertIn("USING COVERING INDEX", details)
        self.assertNotIn("TEMP B-TREE", details)

    def test_manifest_is_read_once_until_its_identity_changes(self):
        manifest = self.root / "manifest.parquet"
        manifest.write_bytes(b"synthetic stand-in; query result is injected")
        included = self.scan("a", 1)
        excluded = self.scan("a", 2)
        self.read_index()
        connection = mock.MagicMock()
        connection.__enter__.return_value = connection

        def execute(query, *args):
            cursor = mock.Mock()
            if "count(*)" in query:
                cursor.fetchmany.return_value = [("a", 1, 4)]
            else:
                cursor.fetchmany.side_effect = [[(included, "a", 1)], []]
            return cursor

        connection.execute.side_effect = execute
        module = types.SimpleNamespace(connect=mock.Mock(return_value=connection))
        with mock.patch.dict("sys.modules", {"duckdb": module}):
            self.assertEqual(refresh.manifest_batches(self.db, manifest), [("a", 1, 4)])
            self.assertEqual(refresh.manifest_batches(self.db, manifest), [["a", 1, 4]])
            self.assertEqual(module.connect.call_count, 1)
            self.fill([("a", 1, 4)])
            self.assertEqual(sum(self.lists(), []), [included])
            self.assertNotIn(excluded, sum(self.lists(), []))
            manifest.write_bytes(b"new manifest")
            refresh.manifest_batches(self.db, manifest)
            self.assertEqual(module.connect.call_count, 2)

    def test_main_waits_for_both_completion_cursors_before_publishing(self):
        ledgers = []
        for host in range(2):
            path = self.root / f"converter-{host}" / "done-test-recipe.txt"
            path.parent.mkdir()
            path.write_text(f"keep/a/sn1/1900-01-01/1/{host + 1}.jp2\tuploaded\n"
                            f"keep/a/sn1/1900-01-01/1/{host + 3}.jp2\tuploaded\n")
            ledgers.append(path)
        damage = self.root / "damage.txt"
        damage.write_text("")
        for seq in range(1, 7):
            self.scan("a", seq)
        args = ["--index", str(self.index), "--scans-root", str(self.scans),
                "--manifest", str(self.root / "manifest.parquet"), "--damage-index", str(damage),
                "--done", str(ledgers[0]), "--done", str(ledgers[1]),
                "--recipe", "test-recipe", "--state", str(self.work), "--shards", "2",
                "--rows-per-tick", "1", "--low-water", "0", "--high-water", "2"]
        with mock.patch.object(refresh, "manifest_batches", return_value=[("a", 1, 6)]):
            refresh.main(args)
            self.assertFalse((self.work / "host-0.txt").exists())
            refresh.main(args)
            self.assertFalse((self.work / "host-0.txt").exists())
            for _ in range(6):
                refresh.main(args)
        published = sum(self.lists(), [])
        self.assertEqual(set(published), {
            "keep/a/sn1/1900-01-01/1/5.jp2", "keep/a/sn1/1900-01-01/1/6.jp2"})


if __name__ == "__main__":
    unittest.main()
