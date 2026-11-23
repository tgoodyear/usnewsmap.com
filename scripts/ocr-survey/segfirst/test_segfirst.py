"""Tests for the #2122 arm C assembly and scorer, on synthetic data only (no truth page is read)."""
import os
import sys
import unittest

import numpy as np

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import columns  # noqa: E402
import score2122 as S  # noqa: E402
import segfirst as F  # noqa: E402


def found_with(gutters, width=1000, height=2000, work_h=400):
    return {"bands": F.band_cores(work_h, height, columns.Params()), "gutters": gutters, "height": height, "width": width}


class Assembly(unittest.TestCase):
    def test_bands_tile_page(self):
        b = F.band_cores(400, 2000, columns.Params())
        self.assertEqual(b[0][2], 0)
        self.assertEqual(b[-1][3], 2000)
        for p, q in zip(b, b[1:]):
            self.assertEqual(p[3], q[2])

    def test_bands_match_finder_segments(self):
        rng = np.random.default_rng(1)
        gray = np.full((900, 600), 235, np.uint8)
        for x0 in (40, 230, 420):  # three text columns of noise ink
            gray[30:870, x0:x0 + 150] = np.where(rng.random((840, 150)) < 0.25, 20, 235)
        p = columns.Params(work_width=600)
        segs = columns.detect(gray, (1200, 1800), "banded", p)
        self.assertTrue(segs)
        cores = {(r[0], r[1]) for r in F.band_cores(900, 1800, p)}
        for g in segs:
            for s in g:
                self.assertIn((s[2], s[3]), cores)

    def test_cells_cut_at_gutter_centres_and_tile(self):
        f = found_with([])
        bands = f["bands"]
        g = [[[300, 340, b[0], b[1]] for b in bands], [[640, 660, b[0], b[1]] for b in bands[2:]]]
        f["gutters"] = g
        regs = F.assemble(f)
        self.assertTrue(F.tiling_ok(regs, 1000, 2000))
        xs = sorted({(c[0], c[1]) for r in regs for c in r["cells"]})
        self.assertIn((0, 320), xs)
        self.assertIn((320, 650), xs)
        self.assertIn((650, 1000), xs)

    def test_duplicate_and_nested_segments_merge(self):
        f = found_with([])
        b = f["bands"][3]
        f["gutters"] = [[[400, 440, b[0], b[1]]], [[400, 440, b[0], b[1]]], [[420, 430, b[0], b[1]]]]
        self.assertEqual([c[:2] for c in F.cells_of(f)[3]], [[0, 420], [420, 1000]])

    def test_join_and_order(self):
        f = found_with([])
        bands = f["bands"]
        # one gutter from band 2 down; bands 0-1 are a full-width banner
        f["gutters"] = [[[495, 505, b[0], b[1]] for b in bands[2:]]]
        regs = F.assemble(f)
        self.assertEqual(len(regs), 3)
        self.assertEqual(regs[0]["box"][:3], [0, 0, 1000])          # banner first: its top is at the page top
        self.assertEqual(regs[1]["box"][0], 0)                      # then the left column
        self.assertEqual(regs[2]["box"][0], 500)                    # then the right column
        self.assertEqual(regs[1]["box"][3], 2000)

    def test_join_tolerance(self):
        cells = [[[0, 500, 0, 10], [500, 1000, 0, 10]], [[0, 509, 10, 20], [509, 1000, 10, 20]],
                 [[0, 530, 20, 30], [530, 1000, 20, 30]]]
        regs = F.join_regions(cells, 1000)
        self.assertEqual(sorted(len(r) for r in regs), [1, 1, 2, 2])

    def test_mid_page_banner_read_where_top_falls(self):
        f = found_with([])
        bands = f["bands"]
        n = len(bands)
        upper, lower = bands[: n // 2 - 1], bands[n // 2 + 1:]
        f["gutters"] = [[[495, 505, b[0], b[1]] for b in upper], [[495, 505, b[0], b[1]] for b in lower]]
        regs = F.assemble(f)
        tops = [(r["box"][0], r["box"][1], r["box"][2]) for r in regs]
        # left column upper, banner (left edge 0, top mid page), left lower, right upper, right lower
        self.assertEqual([t[0] for t in tops], [0, 0, 0, 500, 500])
        self.assertEqual(tops[1][2], 1000)

    def test_tsv_lines(self):
        tsv = ("level\tpage_num\tblock_num\tpar_num\tline_num\tword_num\tleft\ttop\twidth\theight\tconf\ttext\n"
               "5\t1\t1\t1\t1\t1\t10\t20\t30\t10\t90\tThe\n5\t1\t1\t1\t1\t2\t50\t20\t30\t10\t90\tcat\n"
               "5\t1\t1\t1\t2\t1\t10\t40\t30\t10\t90\tsat\n4\t1\t1\t1\t2\t0\t10\t40\t30\t10\t-1\t\n")
        self.assertEqual(F.tsv_lines(tsv, dx=100), [[[110, 20, 140, 30], [150, 20, 180, 30]], [[110, 40, 140, 50]]])


class Scorer(unittest.TestCase):
    def test_normalise(self):
        self.assertEqual(S.normalise("The Gov-\nernor's  “SPEECH”, ﬁne!"), "the governor's speech fine")
        self.assertEqual(S.normalise("'tis rock’n’roll -- U.S. $5"), "tis rock'n'roll u s $5")
        self.assertEqual(S.normalise("well-known\nword"), "well known word")
        self.assertEqual(S.normalise("end¬\nless"), "endless")

    def test_recall_multiplicity(self):
        arts = [{"id": "a", "text": "the cat the cat the cat"}]
        c, _ = S.page_counts(arts, "the cat the cat")
        self.assertEqual(c[2:], [4, 6])        # unigrams: 2 "the" and 2 "cat" found of 3 each
        self.assertEqual(c[1], 4)              # trigrams in truth
        self.assertEqual(c[0], 2)              # "the cat the", "cat the cat" once each in the hyp

    def test_trigrams_do_not_cross_articles(self):
        arts = [{"id": "a", "text": "one two"}, {"id": "b", "text": "three four five"}]
        c, _ = S.page_counts(arts, "one two three four five")
        self.assertEqual(c[:2], [1, 1])

    def test_article_cer_infix(self):
        self.assertEqual(S.article_edits("hello world", "xx yy hello wurld zz"), 1)
        self.assertEqual(S.article_edits("abc", ""), 3)

    def test_crosses(self):
        seg = [[100, 120, 0, 100]]
        self.assertTrue(S.crosses([[10, 10, 50, 20], [150, 10, 200, 20]], seg))
        self.assertFalse(S.crosses([[10, 10, 50, 20], [60, 10, 90, 20]], seg))
        self.assertFalse(S.crosses([[10, 110, 50, 120], [150, 110, 200, 120]], seg))

    def test_overproof_shared_document_id(self):
        def rec(doc_id, text):
            return {"document_metadata": {"document_id": doc_id, "primary_dataset_filename": "x/dataset3/a.txt"},
                    "ground_truth": {"line_offsets": [[0, len(text)]], "transcription_unit": text}}
        did = "sn88074142-8-00280761485-1871122101-0816"
        key = "sn88074142-00280761485-1871122101-0816"
        recs = [("t.jsonl", 0, rec(did, "first article")), ("t.jsonl", 1, rec(did, "second article")),
                ("t.jsonl", 2, rec("sn1-1-2-187101011-0001", "other page"))]
        out = list(S.overproof_articles(recs, {key}))
        self.assertEqual([a["id"] for _, a in out], [did + "@t.jsonl:0", did + "@t.jsonl:1"])
        self.assertEqual([k for k, _ in out], [key, key])
        with self.assertRaises(SystemExit):
            list(S.overproof_articles(recs + [("u.jsonl", 0, rec(did, "first article"))], {key}))

    def test_bootstrap_paired_shape(self):
        a = np.array([[5, 10, 8, 10, 1, 10]] * 4, dtype=float)
        b = np.array([[6, 10, 8, 10, 1, 10]] * 4, dtype=float)
        out = S.bootstrap({"C": b, "L": a, "W": a}, ("C", "L", "W"))
        self.assertAlmostEqual(out["C-W"]["trigram_recall"][0], 0.1)
        self.assertAlmostEqual(out["C"]["unigram_recall"][1], 0.8)


if __name__ == "__main__":
    unittest.main()
