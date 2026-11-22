"""render_compare/fitrel.py: the layout-fit numbers on synthetic boxes, and the look-alike variant set V(t) (#2103).

Pure arithmetic and plumbing; no font or scan is needed."""

import sys
import unittest
from pathlib import Path

import numpy as np

sys.path.insert(0, str(Path(__file__).resolve().parent))

import paths  # noqa: E402,F401
import fitrel  # noqa: E402

PAIRS = [("n", "u", 0.84), ("h", "n", 0.78), ("i", "l", 0.7), ("c", "e", 0.65), ("e", "o", 0.62)]


def frame(ink_boxes, shape=(60, 100)):
    m = np.zeros(shape, bool)
    for y0, y1, x0, x1 in ink_boxes:
        m[y0:y1, x0:x1] = True
    return m


BOX = (20, 20, 60, 20)  # x0, y0, w, h in the frame


class FitMetricsTest(unittest.TestCase):
    def test_ink_that_fills_the_box_is_a_perfect_fit(self):
        f = fitrel.fit_from_mask(frame([(20, 40, 20, 80)]), BOX)
        self.assertTrue(f["has_ink"])
        self.assertAlmostEqual(f["outside_share"], 0.0)
        self.assertAlmostEqual(f["area_ratio"], 1.0)
        self.assertAlmostEqual(f["v_offset"], 0.0)

    def test_ink_spilling_out_of_the_box_counts_the_touching_piece_only(self):
        # one bar from the box's left edge 20 px to the left: half of its 40 columns lie outside the box (x < 20)
        f = fitrel.fit_from_mask(frame([(25, 35, 0, 40)]), BOX)
        self.assertAlmostEqual(f["outside_share"], 0.5)
        # a separate piece that does not touch the box is not counted
        g = fitrel.fit_from_mask(frame([(25, 35, 30, 50), (50, 55, 0, 10)]), BOX)
        self.assertAlmostEqual(g["outside_share"], 0.0)

    def test_a_loose_box_has_a_larger_area_ratio(self):
        f = fitrel.fit_from_mask(frame([(25, 35, 30, 50)]), BOX)  # ink 10 x 20 in a box 20 x 60
        self.assertAlmostEqual(f["area_ratio"], (20 * 60) / (10 * 20))
        self.assertAlmostEqual(f["v_offset"], 0.0)  # ink rows 25..35, centre 30; box centre 30

    def test_vertical_offset_is_a_share_of_box_height(self):
        f = fitrel.fit_from_mask(frame([(30, 40, 20, 80)]), BOX)  # ink centre 35, box centre 30, h 20
        self.assertAlmostEqual(f["v_offset"], 5 / 20)
        up = fitrel.fit_from_mask(frame([(20, 30, 20, 80)]), BOX)  # centre 25: the same size, the other way
        self.assertAlmostEqual(up["v_offset"], 5 / 20)

    def test_no_ink_in_the_box(self):
        f = fitrel.fit_from_mask(frame([(50, 55, 0, 10)]), BOX)
        self.assertFalse(f["has_ink"])
        self.assertTrue(all(np.isnan(f[k]) for k in fitrel.FIT_PARTS))


class FitScoreTest(unittest.TestCase):
    def test_score_orders_worse_fits_higher_and_no_ink_last(self):
        parts = {"outside_share": [0.0, 0.2, 0.9, float("nan")], "area_ratio": [1.0, 1.5, 4.0, float("nan")],
                 "v_offset": [0.0, 0.1, 0.4, float("nan")]}
        s = fitrel.fit_score(parts)
        self.assertTrue(s[0] < s[1] < s[2])
        self.assertEqual(s[3], 1.0)
        self.assertTrue(np.all((s > 0) & (s <= 1)))

    def test_tertiles_split_evenly_and_in_order(self):
        s = np.linspace(0, 1, 300)
        cuts = fitrel.tertile_cuts(s)
        t = np.array([fitrel.tertile_of(x, cuts) for x in s])
        self.assertEqual([int((t == k).sum()) for k in range(3)], [100, 100, 100])
        self.assertTrue(cuts[0] < cuts[1])
        self.assertEqual((t[0], t[-1]), (0, 2))


class VariantsTest(unittest.TestCase):
    def test_every_variant_is_one_pair_partner_swap_in_both_directions(self):
        v = fitrel.variants("hunt", PAIRS)
        self.assertEqual(v, sorted(set(v)))
        self.assertNotIn("hunt", v)
        for w in v:
            self.assertEqual(len(w), 4)
            diff = [i for i in range(4) if w[i] != "hunt"[i]]
            self.assertEqual(len(diff), 1)
            a, b = "hunt"[diff[0]], w[diff[0]]
            self.assertTrue((a, b) in {(x, y) for x, y, _ in PAIRS} | {(y, x) for x, y, _ in PAIRS})
        # h->n, u->n, n->u, n->h: both directions of n/u and of h/n appear
        self.assertEqual(set(v), {"nunt", "hnnt", "huut", "huht"})

    def test_text_with_no_pair_letter_has_no_variants(self):
        self.assertEqual(fitrel.variants("zzzz", PAIRS), [])
        self.assertEqual(fitrel.variants("", PAIRS), [])

    def test_cap_and_determinism(self):
        word = "ceeeeeeeeeeeenunnh"
        full = fitrel.variants(word, PAIRS, cap=10_000)
        self.assertGreater(len(full), fitrel.CAP)
        a = fitrel.variants(word, PAIRS)
        self.assertEqual(len(a), fitrel.CAP)
        self.assertEqual(a, fitrel.variants(word, PAIRS))
        self.assertEqual(a, fitrel.variants(word, list(reversed(PAIRS))))  # the order of the pairs does not matter
        self.assertTrue(set(a) <= set(full))
        self.assertEqual(len(set(a)), len(a))
        self.assertNotEqual(a, fitrel.variants(word, PAIRS, seed=7))  # the seed is the draw
        self.assertEqual(len(fitrel.variants(word, PAIRS, cap=5)), 5)

    def test_the_draw_depends_on_the_text_not_on_call_order(self):
        x1 = fitrel.variants("ceeeeeeeeeeeenunnh", PAIRS)
        fitrel.variants("nnnnnnnnnnnnnnnn", PAIRS)
        self.assertEqual(x1, fitrel.variants("ceeeeeeeeeeeenunnh", PAIRS))

    def test_a_swap_is_in_the_variants_of_both_texts(self):
        v = fitrel.variants("hunt", PAIRS)
        self.assertIn("hnnt", v)
        self.assertIn("hunt", fitrel.variants("hnnt", PAIRS))


if __name__ == "__main__":
    unittest.main()
