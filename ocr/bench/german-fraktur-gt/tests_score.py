#!/usr/bin/env python3
"""Unit tests for score.py: CER, fold, word F1, PAGE-XML and ALTO readers."""
import os, random, sys, tempfile, unittest
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import score

def dp(a, b):
    prev = list(range(len(b) + 1))
    for i, ca in enumerate(a, 1):
        cur = [i]
        for j, cb in enumerate(b, 1):
            cur.append(min(prev[j] + 1, cur[-1] + 1, prev[j - 1] + (ca != cb)))
        prev = cur
    return prev[-1]

PAGE = """<?xml version="1.0"?>
<PcGts xmlns="http://schema.primaresearch.org/PAGE/gts/pagecontent/2019-07-15">
 <Page imageFilename="x.png"><ReadingOrder><OrderedGroup id="g">
  <RegionRefIndexed index="1" regionRef="r2"/><RegionRefIndexed index="0" regionRef="r1"/>
 </OrderedGroup></ReadingOrder>
 <TextRegion id="r1"><TextLine id="l1"><TextEquiv><Unicode>Die Hoſe</Unicode></TextEquiv></TextLine>
  <TextLine id="l2"><TextEquiv><Unicode>des Kaiſers</Unicode></TextEquiv></TextLine></TextRegion>
 <TextRegion id="r2"><TextLine id="l3"><TextEquiv><Unicode>Berlin.</Unicode></TextEquiv></TextLine></TextRegion>
 </Page></PcGts>"""

ALTO = """<alto xmlns="http://www.loc.gov/standards/alto/ns-v4#"><Layout><Page><PrintSpace>
 <TextBlock><TextLine><String CONTENT="Haus"/><SP/><String CONTENT="Kai"/><HYP CONTENT="-"/></TextLine>
 <TextLine><String CONTENT="ser"/></TextLine></TextBlock></PrintSpace></Page></Layout></alto>"""

class T(unittest.TestCase):
    def test_lev_matches_dp(self):
        rnd = random.Random(1)
        for _ in range(300):
            a = "".join(rnd.choice("abc ") for _ in range(rnd.randint(0, 40)))
            b = "".join(rnd.choice("abcd") for _ in range(rnd.randint(0, 40)))
            self.assertEqual(score.lev(a, b), dp(a, b), (a, b))

    def test_cer_simple(self):
        s = score.score_texts("abcd", "abxd")
        self.assertEqual((s["ref_chars"], s["edits_raw"]), (4, 1))
        self.assertAlmostEqual(s["cer_raw"], 0.25)

    def test_whitespace_collapse(self):
        s = score.score_texts("a  b\n\nc", "a b c")
        self.assertEqual(s["edits_raw"], 0)

    def test_fold(self):
        self.assertEqual(score.norm_fold("Kaiſer ʒu"), "Kaiser zu")
        self.assertEqual(score.norm_fold("Kai⸗\nser"), "Kaiser")
        self.assertEqual(score.norm_fold("Kai¬\n ser"), "Kaiser")
        self.assertEqual(score.norm_fold("Kai-\nser"), "Kaiser")
        self.assertEqual(score.norm_fold("a ⸗ b"), "a - b")  # not at line end
        self.assertEqual(score.norm_fold("x-\n—"), "x- —")  # next not a word char
        self.assertEqual(score.norm_raw("Hoſe"), "Hoſe")

    def test_folded_forgives_long_s_and_hyphenation(self):
        s = score.score_texts("Hoſe des Kai⸗\nser", "Hose des Kaiser")
        self.assertEqual(s["edits_folded"], 0)
        self.assertGreater(s["edits_raw"], 0)

    def test_word_f1(self):
        self.assertAlmostEqual(score.word_f1("a b c", "a b c"), 1.0)
        self.assertAlmostEqual(score.word_f1("a b c d", "a b x y"), 0.5)
        self.assertEqual(score.word_f1("", ""), 1.0)
        self.assertEqual(score.word_f1("a", ""), 0.0)

    def test_page_reading_order(self):
        with tempfile.NamedTemporaryFile("w", suffix=".xml", delete=False,
                                         encoding="utf-8") as f:
            f.write(PAGE)
        try:
            self.assertEqual(score.read_reference(f.name),
                             "Die Hoſe\ndes Kaiſers\nBerlin.")
        finally:
            os.unlink(f.name)

    def test_alto(self):
        with tempfile.NamedTemporaryFile("w", suffix=".xml", delete=False) as f:
            f.write(ALTO)
        try:
            t = score.read_reference(f.name)
            self.assertEqual(t, "Haus Kai-\nser")
            self.assertEqual(score.norm_fold(t), "Haus Kaiser")
        finally:
            os.unlink(f.name)

    def test_year_title(self):
        self.assertEqual(score.year_of("x", "dir/1895-03-02_a"), 1895)
        self.assertEqual(score.title_of("chronicling", "Vossische_1895-03-02_p1"), "Vossische")

    def test_cmd(self):
        c = score.build_cmd(score.DEFAULT_CMD, "/a b/p.png", "frak2021", "/t")
        self.assertEqual(c[:2], ["tesseract", "/a b/p.png"])
        self.assertIn("frak2021", c)

if __name__ == "__main__":
    unittest.main(verbosity=1)
