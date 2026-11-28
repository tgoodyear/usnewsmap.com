import collections
import os
import sys
import unittest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import select_pages as sp  # noqa: E402


def doc(lccn, year, seq=1):
    return "%s_%d-01-01_ed-1_seq-%d" % (lccn, year, seq)


class Quotas(unittest.TestCase):
    def test_equal_shares_water_filled(self):
        self.assertEqual(sp.quotas({1880: 100, 1890: 100, 1900: 2}, 20), {1880: 9, 1890: 9, 1900: 2})

    def test_remainder_follows_order(self):
        self.assertEqual(sp.quotas({1880: 9, 1890: 9}, 3, order=[1890, 1880]), {1880: 1, 1890: 2})

    def test_short_universe_gives_all(self):
        self.assertEqual(sp.quotas({1880: 2, 1890: 1}, 10), {1880: 2, 1890: 1})


class Allocate(unittest.TestCase):
    def test_floor_then_proportional(self):
        got = sp.allocate({1780: 2, 1900: 1000, 1910: 1000}, 20, {1780: 2, 1900: 1000, 1910: 3000}, 3, [1780, 1900, 1910])
        self.assertEqual(got, {1780: 2, 1900: 6, 1910: 12})

    def test_capacity_spills_over(self):
        got = sp.allocate({1900: 5, 1910: 100}, 20, {1900: 9000, 1910: 1000}, 1, [1900, 1910])
        self.assertEqual(got, {1900: 5, 1910: 15})

    def test_floors_over_n_fall_back_to_equal(self):
        got = sp.allocate({d: 50 for d in (1880, 1890, 1900)}, 4, {1880: 1, 1890: 1, 1900: 99}, 3, [1900, 1880, 1890])
        self.assertEqual(got, {1880: 1, 1890: 1, 1900: 2})


class Draw(unittest.TestCase):
    def test_title_cap_spans_draws_and_decades(self):
        ranked = sorted([doc("sn1", 1880 + 10 * (i % 3), i) for i in range(30)] + [doc("sn2", 1880, i) for i in range(3)],
                        key=lambda d: (sp.key(d), d))
        titles, taken = collections.Counter(), set()
        a, _ = sp.draw(ranked, 4, 5, titles, taken)
        b, _ = sp.draw(ranked, 10, 5, titles, taken)
        self.assertEqual(titles["sn1"], 5)
        self.assertLessEqual(titles["sn2"], 3)
        self.assertFalse(set(a) & set(b))
        self.assertEqual(len(a) + len(b), 8)

    def test_decades_spread_and_rank_order(self):
        ranked = sorted([doc("t%d" % i, 1850 + 10 * (i % 5), 1) for i in range(100)], key=lambda d: (sp.key(d), d))
        got, q = sp.draw(ranked, 10, 5, collections.Counter(), set())
        self.assertEqual(q, {1850: 2, 1860: 2, 1870: 2, 1880: 2, 1890: 2})
        self.assertEqual(got, [d for d in ranked if d in set(got)])
        # each decade's pages are its best-ranked ones
        for dec in q:
            in_dec = [d for d in ranked if sp.decade(d) == dec]
            self.assertEqual({d for d in got if sp.decade(d) == dec}, set(in_dec[:2]))

    def test_key_prefix(self):
        import hashlib
        self.assertEqual(sp.key("x"), hashlib.sha256(b"2122:pick:x").hexdigest())

    def test_paths(self):
        self.assertEqual(sp.path_form("sn1_1900-02-03_ed-1_seq-04"), "sn1/1900-02-03/ed-1/seq-4")
        self.assertEqual(sp.archive_name("ak_albatross", 1), "ak_albatross_ver01.tar.bz2")
        self.assertEqual(sp.path_tail("http://x/pst_davey_ver01/data/sn83045211/00237287678/1917110301/0875.jpg"),
                         ("sn83045211", "00237287678", "1917110301", "0875"))


if __name__ == "__main__":
    unittest.main()
