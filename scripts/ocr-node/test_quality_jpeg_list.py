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

    def test_interleave_alternates_shards(self):
        self.assertEqual(qjl.interleave([["a1", "a2", "a3"], ["b1"]]), ["a1", "b1", "a2", "a3"])


if __name__ == "__main__":
    unittest.main()
