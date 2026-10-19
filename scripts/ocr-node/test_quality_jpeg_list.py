"""Tests for quality-jpeg-list.py (#850): python3 -m unittest -v scripts/ocr-node/test_quality_jpeg_list.py"""

import importlib.util
import os
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
spec = importlib.util.spec_from_file_location("qjl", os.path.join(HERE, "quality-jpeg-list.py"))
qjl = importlib.util.module_from_spec(spec)
spec.loader.exec_module(qjl)


def rows(batch, n, ext=".jp2"):
    return [(batch, 1, f"keep/{batch}/l/d/1/{i}{ext}") for i in range(n)]


def rows_n(batch, n):
    return rows(batch, n)


class Order(unittest.TestCase):
    def setUp(self):
        # Runner order: a(4) b(3) c(2) d(1); 2 shards -> shard 0: a, c; shard 1: b, d.
        self.rows = rows("a", 4) + rows("b", 3) + rows("c", 2) + rows("d", 1) + rows("b", 1, ".jpg")

    def test_shards_take_every_nth_batch_by_size(self):
        q = qjl.shard_queues(self.rows, 2, {}, set())
        self.assertEqual({n.split("/")[1] for n in q[0]}, {"a", "c"})
        self.assertEqual({n.split("/")[1] for n in q[1]}, {"b", "d"})

    def test_jpegs_are_left_out(self):
        q = qjl.shard_queues(self.rows, 2, {}, set())
        self.assertFalse(any(n.endswith(".jpg") for s in q for n in s))

    def test_start_after_skips_the_current_batch(self):
        q = qjl.shard_queues(self.rows, 2, qjl.parse_start_after("0=0"), set())
        self.assertEqual({n.split("/")[1] for n in q[0]}, {"c"})
        self.assertEqual(len(q[1]), 4)

    def test_done_pages_are_dropped(self):
        q = qjl.shard_queues(self.rows, 2, {}, {"keep/c/l/d/1/0.jp2"})
        self.assertNotIn("keep/c/l/d/1/0.jp2", q[0])

    def test_tier1_follows_the_manifest_in_the_same_rotation(self):
        damage = ["audit/damage/v2/e_ver01.parquet", "audit/damage/v2/e_ver01.summary.json",
                  "audit/damage/v2/a_ver01.parquet", "audit/damage/v2/f_ver02.parquet",
                  "audit/damage/v2/g_ver01.parquet"]
        keep = ["keep/f/l/1900-01-02/1/10.jp2", "keep/f/l/1900-01-02/1/2.jp2",
                "keep/e/l/1900-01-01/1/1.jp2", "keep/z/l/1900-01-01/1/1.jp2"]
        manifest = {(b, int(v)) for b, v, _ in self.rows}
        t1 = qjl.tier1_batches(manifest, damage, keep)
        # a is in the manifest and g has no archived pages; e and f follow in name order.
        self.assertEqual([k for k, _ in t1], [("e", 1), ("f", 2)])
        self.assertEqual(t1[1][1], ["keep/f/l/1900-01-02/1/10.jp2", "keep/f/l/1900-01-02/1/2.jp2"])
        # Manifest a b c d: shard 0 gets a, c and shard 1 gets b, d; both then list all of tier 1.
        q = qjl.shard_queues(self.rows, 2, {}, set(), t1)
        self.assertEqual(q[0][-3:], ["keep/e/l/1900-01-01/1/1.jp2", *t1[1][1]])
        self.assertEqual(q[1][-3:], q[0][-3:])

    def test_pages_follow_the_runners_doc_id_string_order(self):
        keep = ["keep/f/l/1900-01-02/1/2.jp2", "keep/f/l/1900-01-02/1/10.jp2"]
        t1 = qjl.tier1_batches(set(), ["audit/damage/v2/f_ver01.parquet"], keep)
        # doc_id l_1900-01-02_ed-1_seq-10 sorts before ..._seq-2, as the runner sorts it.
        self.assertEqual(t1[0][1], ["keep/f/l/1900-01-02/1/10.jp2", "keep/f/l/1900-01-02/1/2.jp2"])

    def test_jpeg_only_batches_keep_their_place_in_the_rotation(self):
        damage = ["audit/damage/v2/e_ver01.parquet", "audit/damage/v2/f_ver01.parquet",
                  "audit/damage/v2/g_ver01.parquet"]
        keep = ["keep/e/l/1900-01-01/1/1.jp2", "keep/f/l/1900-01-01/1/1.jpg",
                "keep/g/l/1900-01-01/1/1.jp2"]
        t1 = qjl.tier1_batches(set(), damage, keep)
        self.assertEqual([k for k, _ in t1], [("e", 1), ("f", 1), ("g", 1)])
        ordered = qjl.ordered_batches([], t1)
        # f, with only a JPEG master, still has its place: g follows it, at global index 2.
        self.assertEqual([k for k, _ in ordered], [("e", 1), ("f", 1), ("g", 1)])
        left = qjl.resolve_current(ordered, 2, {1: "f_ver01"}, warn=lambda m: None, n_manifest=0)
        self.assertEqual([k for k, _ in left[1]], [("g", 1)])
        q = qjl.shard_queues([], 2, {}, set(), t1, left)
        self.assertEqual(q[1], ["keep/g/l/1900-01-01/1/1.jp2"])

    def test_current_names_the_batch_and_rejects_unknown_ones(self):
        ordered = qjl.ordered_batches(self.rows)
        left = qjl.resolve_current(ordered, 2, {0: "a_ver01", 1: "b_ver01"}, warn=lambda m: None,
                                   n_manifest=4)
        self.assertEqual([k for k, _ in left[0]], [("c", 1)])
        self.assertEqual([k for k, _ in left[1]], [("d", 1)])
        with self.assertRaises(SystemExit):
            qjl.resolve_current(ordered, 2, {1: "zz_ver01"})

    def test_a_manifest_batch_under_another_shard_warns(self):
        ordered = qjl.ordered_batches(self.rows)
        warnings = []
        qjl.resolve_current(ordered, 2, {1: "a_ver01"}, warn=warnings.append, n_manifest=4)
        self.assertEqual(len(warnings), 1)

    def tier1_case(self, frozen, now, current):
        """Manifest m0, m1 (two shards), then tier 1. The runner froze `frozen`; the listing
        is now `now`. Returns the batches listed, and those pending under the frozen order."""
        manifest = rows("m0", 9) + rows("m1", 8)
        keep = [f"keep/{b}/l/1900-01-01/1/1.jp2" for b in now]
        damage = [f"audit/damage/v2/{b}_ver01.parquet" for b in now]
        t1 = qjl.tier1_batches({(b, int(v)) for b, v, _ in manifest}, damage, keep)
        ordered = qjl.ordered_batches(manifest, t1)
        left = qjl.resolve_current(ordered, 2, current, warn=lambda m: None, n_manifest=2)
        q = qjl.shard_queues(manifest, 2, {}, set(), t1, left)
        listed = {n.split("/")[1] for s in q for n in s}
        full = ["m0", "m1", *frozen]
        pending = set()
        for s, name in current.items():
            pos = full.index(name.rpartition("_ver")[0])
            pending |= {b for i, b in enumerate(full) if i % 2 == s and i > pos}
        return listed, pending

    def test_unequal_progress_with_a_mismatch_keeps_pending_batches(self):
        frozen = ["a", "b", "c", "d", "e", "f", "g", "h"]
        listed, pending = self.tier1_case(frozen, ["a", "b", "c", "cc", "d", "e", "f", "g", "h"],
                                          {0: "c_ver01", 1: "f_ver01"})
        self.assertTrue(pending <= listed, (pending, listed))

    def test_undetected_drift_keeps_pending_batches(self):
        frozen = ["a", "b", "c", "d", "e", "f", "g", "h"]
        # cc and ee inserted: both positions still look like their shards', with no warning.
        listed, pending = self.tier1_case(frozen, ["a", "b", "c", "cc", "d", "e", "ee", "f", "g", "h"],
                                          {0: "c_ver01", 1: "f_ver01"})
        self.assertTrue(pending <= listed, (pending, listed))

    def test_interleave_alternates_shards(self):
        self.assertEqual(qjl.interleave([["a1", "a2", "a3"], ["b1"]]), ["a1", "b1", "a2", "a3"])


if __name__ == "__main__":
    unittest.main()
