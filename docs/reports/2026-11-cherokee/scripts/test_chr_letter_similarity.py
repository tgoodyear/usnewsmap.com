"""Tests for chr_letter_similarity.py on synthetic data: python3 -m unittest discover -s docs/reports/2026-11-cherokee/scripts.

Needs numpy (and scikit-image for the split test); skipped without them. No torch."""
import os
import sys
import unittest

try:
    import numpy as np
except ImportError:  # pragma: no cover
    raise unittest.SkipTest("numpy is not installed")

sys.path.insert(0, os.path.dirname(__file__))
import chr_letter_similarity as cls  # noqa: E402


def task(i, a, b, page="p1", settled=False, outcome=None, a_diff=(), b_diff=()):
    return {"id": f"t{i}", "a": a, "b": b, "page": page, "settled": settled, "outcome": outcome, "a_diff": list(a_diff), "b_diff": list(b_diff)}


class Labels(unittest.TestCase):
    def test_agreed_positions_skip_differing_and_unequal(self):
        self.assertEqual(cls.agreed_positions(task(1, "ABCD", "ABXD", a_diff=[2], b_diff=[2])), [0, 1, 3])
        self.assertEqual(cls.agreed_positions(task(2, "ABC", "ABCD")), [])

    def test_tiers(self):
        pool = cls.attestation_pool([task(1, "ABC", "XBC"), task(2, "ABC", "ABD", page="p2"), task(3, "QQQ", "QQR")])
        self.assertEqual(cls.tier_of(task(9, "ABC", "ABZ"), pool), "A")  # reference spelling ABC seen elsewhere
        self.assertEqual(cls.tier_of(task(3, "QQQ", "QQR"), pool), "B")  # only its own task carries it
        win_b = task(8, "ZZZ", "ABC", settled=True, outcome="b")
        self.assertEqual(cls.tier_of(win_b, pool), "A")  # the settled winner is attested
        win_a = task(7, "ZZZ", "ABC", settled=True, outcome="a")
        self.assertEqual(cls.tier_of(win_a, pool), "B")  # the settled winner is not


class Knn(unittest.TestCase):
    def setUp(self):
        rng = np.random.default_rng(1)
        c = {"x": np.array([1.0, 0, 0]), "y": np.array([0, 1.0, 0])}
        self.labels = ["x", "y"] * 10
        e = np.stack([c[l] + 0.05 * rng.normal(size=3) for l in self.labels])
        self.emb = e / np.linalg.norm(e, axis=1, keepdims=True)
        self.groups = [f"g{i // 2}" for i in range(20)]

    def test_folds_keep_groups_together(self):
        f = cls.grouped_folds(self.groups, 4, 7)
        for g in set(self.groups):
            self.assertEqual(len({f[i] for i, x in enumerate(self.groups) if x == g}), 1)
        self.assertEqual(list(f), list(cls.grouped_folds(self.groups, 4, 7)))

    def test_separable_classes_are_found(self):
        f = cls.grouped_folds(self.groups, 4, 7)
        p1, p5, s1, _ = cls.knn_predict(self.emb, self.labels, f)
        self.assertEqual(p1, self.labels)
        self.assertEqual(p5, self.labels)
        self.assertTrue((s1 > 0.9).all())

    def test_threshold_table_and_pairs(self):
        truth = ["Ꮧ", "Ꮨ", "Ꮧ", "Ꮨ", "Ꭰ"]
        pred = ["Ꮧ", "Ꮨ", "Ꮨ", "Ꮨ", "Ꭰ"]
        sim = np.array([0.9, 0.8, 0.3, 0.7, 0.95])
        mask = [True] * 5
        t = cls.threshold_table(pred, truth, sim, mask, coverage_targets=(0.6, 1.0))
        self.assertEqual(t["rows"][0]["n_covered"], 3)
        self.assertEqual(t["rows"][0]["lookalike_accuracy"], 1.0)
        self.assertEqual(t["rows"][1]["lookalike_accuracy"], 0.75)
        self.assertEqual(t["best_98"]["n_covered"], 4)
        p = cls.pair_stats(pred, truth, mask)[1]
        self.assertEqual((p["n"], p["correct"], p["to_other_member"]), (4, 3, 1))


class Segment(unittest.TestCase):
    def test_three_blocks_split_three_ways(self):
        try:
            import skimage  # noqa: F401
        except ImportError:
            self.skipTest("scikit-image is not installed")
        ink = np.zeros((30, 60), dtype=bool)
        for x0 in (5, 25, 45):
            ink[8:24, x0 : x0 + 10] = True
        boxes = cls.split_word(ink, 3)
        self.assertEqual([b[0] for b in boxes], [5, 25, 45])

    def test_touching_pair_is_cut_and_uneven_is_refused(self):
        try:
            import skimage  # noqa: F401
        except ImportError:
            self.skipTest("scikit-image is not installed")
        ink = np.zeros((30, 60), dtype=bool)
        ink[8:24, 5:29] = True
        ink[8:24, 17] = False  # a thin bridge joins the two letters
        ink[14:16, 17] = True
        ink[8:24, 35:41] = True
        self.assertEqual(len(cls.split_word(ink, 3)), 3)
        self.assertIsNone(cls.split_word(ink, 2))


if __name__ == "__main__":
    unittest.main()
