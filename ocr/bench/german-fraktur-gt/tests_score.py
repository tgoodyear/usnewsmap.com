#!/usr/bin/env python3
"""Unit tests for score.py and analyze.py: CER, fold, word F1, PAGE-XML and ALTO
readers, page discovery, resume checks, year window and completeness."""
import csv, json, os, random, sys, tempfile, unittest
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import analyze, score

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

NESTED = """<?xml version="1.0"?>
<PcGts xmlns="http://schema.primaresearch.org/PAGE/gts/pagecontent/2019-07-15"><Page>
 <ReadingOrder><OrderedGroup id="root">
  <OrderedGroupIndexed index="1" id="gB">
   <RegionRefIndexed index="1" regionRef="B1"/><RegionRefIndexed index="0" regionRef="B0"/></OrderedGroupIndexed>
  <OrderedGroupIndexed index="0" id="gA">
   <RegionRefIndexed index="1" regionRef="A1"/><RegionRefIndexed index="0" regionRef="A0"/></OrderedGroupIndexed>
 </OrderedGroup></ReadingOrder>
 <TextRegion id="B0"><TextLine><TextEquiv><Unicode>B0</Unicode></TextEquiv></TextLine></TextRegion>
 <TextRegion id="B1"><TextLine><TextEquiv><Unicode>B1</Unicode></TextEquiv></TextLine></TextRegion>
 <TextRegion id="A0"><TextLine><TextEquiv><Unicode>A0</Unicode></TextEquiv></TextLine></TextRegion>
 <TextRegion id="A1"><TextLine><TextEquiv><Unicode>A1</Unicode></TextEquiv></TextLine></TextRegion>
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

    def test_page_nested_ordered_groups(self):
        with tempfile.TemporaryDirectory() as d:
            p = os.path.join(d, "n.xml")
            with open(p, "w", encoding="utf-8") as f:
                f.write(NESTED)
            self.assertEqual(score.read_reference(p), "A0\nA1\nB0\nB1")

    def test_fold_nfc_after_replacement(self):
        self.assertEqual(score.norm_fold("a\u0364"), "\u00e4")
        s = score.score_texts("M\u00e4nner", "Ma\u0364nner")
        self.assertEqual(s["edits_folded"], 0)
        self.assertEqual(s["word_f1"], 1.0)

    def test_year_ignores_range(self):
        self.assertIsNone(score.year_of("x", "data/reichsanzeiger-1820-1939/GT-PAGE/x"))
        self.assertEqual(score.year_of("1891_1_0001", "data/reichsanzeiger-1820-1939/a"), 1891)

    def _touch(self, root, rel, text="<x/>"):
        p = os.path.join(root, rel)
        os.makedirs(os.path.dirname(p), exist_ok=True)
        with open(p, "w") as f:
            f.write(text)

    def test_find_pages_prunes_and_dedupes(self):
        with tempfile.TemporaryDirectory() as d:
            for base in ("data", ".clone/data"):
                self._touch(d, base + "/1891_1_0001.xml")
                self._touch(d, base + "/1891_1_0001.jpg", "")
            pages, _ = score.find_pages("reichsanzeiger", d, {}, (1850, 1920), False)
            self.assertEqual([p["page_id"] for p in pages], ["1891_1_0001"])
            self.assertNotIn(".clone", pages[0]["xml"])

    def test_nzz_only_groundtruth(self):
        with tempfile.TemporaryDirectory() as d:
            for sub in ("NZZ_groundtruth", "NZZ_abbyy"):
                self._touch(d, sub + "/1900_a.xml")
            self._touch(d, "img/1900_a.png", "")
            pages, _ = score.find_pages("nzz", d, {}, (1850, 1920), False)
            self.assertEqual(len(pages), 1)
            self.assertIn("NZZ_groundtruth", pages[0]["xml"])

    def test_year_8_digit_date(self):
        self.assertEqual(score.year_of("nzz_18910302_0_0_a1_p1_1"), 1891)
        self.assertIsNone(score.year_of("nzz_18911302_0"))

    def test_nzz_image_matching(self):
        with tempfile.TemporaryDirectory() as d:
            self._touch(d, "xml/NZZ_groundtruth/nzz_18910302_0_0_a1_p1_1.xml")
            self._touch(d, "xml/NZZ_groundtruth/nzz_18910303_0_0_a1_p1_1.xml")
            self._touch(d, "img/0012_nzz_18910302_0_0_a1_p1_1.tif", "")
            self._touch(d, "img/nzz_18910303_0_0_a1_p1_1.tif", "")
            pages, _ = score.find_pages("nzz", d, {}, (1850, 1920), False)
            self.assertEqual(len(pages), 2)
            self.assertTrue(all(p["year"] == 1891 for p in pages))

    def test_chronicling_skips_and_meta(self):
        with tempfile.TemporaryDirectory() as d:
            self._touch(d, "data/annotations/Voss_1895-03-02_0003.xml")
            self._touch(d, "data/images/Voss_1895-03-02_0003.jpg", "")
            self._touch(d, "work_in_progress/Old_1895-03-02_0009.xml")
            self._touch(d, "work_in_progress/Old_1895-03-02_0009.jpg", "")
            self._touch(d, "meta.csv", "page_id,title,year,stratum\n"
                        "Voss_1895-03-02_0003,Voss,1895,ood\n")
            meta = score.load_meta(os.path.join(d, "meta.csv"))
            pages, _ = score.find_pages("chronicling", d, meta, (1850, 1920), False)
            self.assertEqual([(p["page_id"], p["stratum"]) for p in pages],
                             [("Voss_1895-03-02_0003", "ood")])

    def test_cgroup_cpu(self):
        with tempfile.TemporaryDirectory() as d:
            self._touch(d, "cpu.stat", "usage_usec 2500000\nuser_usec 1\n")
            self.assertAlmostEqual(score.cgroup_cpu(d), 2.5)

    def test_resume_problems(self):
        old = {"arms": {"frak2021": "aa"}, "cmd": "c", "tesseract_version": "5.5"}
        new = {"arms": {"frak2021": "aa", "deu_latf": None}, "cmd": "c",
               "tesseract_version": "5.5"}
        self.assertEqual(score.resume_problems(old, new, {"frak2021"}), [])
        self.assertTrue(score.resume_problems(old, {**new, "cmd": "d"}, set()))
        self.assertTrue(score.resume_problems(old, {**new, "tesseract_version": "5.4"}, set()))
        self.assertTrue(score.resume_problems(
            old, {**new, "arms": {"frak2021": "bb"}}, set()))
        self.assertTrue(score.resume_problems(old, new, {"deu_latf"}))

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

class A(unittest.TestCase):
    FIELDS = score.FIELDS

    def write(self, d, rows, expected=None, limit=0, name="r.csv"):
        p = os.path.join(d, name)
        with open(p, "w", newline="") as f:
            w = csv.DictWriter(f, self.FIELDS)
            w.writeheader()
            for set_, pid, arm, year, edits in rows:
                w.writerow(dict(set=set_, stratum="test", page_id=pid, title="t",
                                year=year, arm=arm, ref_chars=1000, edits_raw=edits,
                                edits_folded=edits, cer_raw=0, cer_folded=0,
                                word_f1=1, cpu_s=1, gpu_study_flag=0))
        if expected is not None:
            with open(p + ".meta.json", "w") as f:
                json.dump({"expected": expected, "limit": limit}, f)
        return p

    def run_analyze(self, p, *extra):
        import io, contextlib
        buf = io.StringIO()
        with contextlib.redirect_stdout(buf):
            rc = analyze.main(["--csv", p, "--n-boot", "200"] + list(extra))
        return rc, buf.getvalue()

    def rows(self, year="1900"):
        return [(s, "p1", arm, year, 0) for s in ("chronicling", "reichsanzeiger")
                for arm in ("frak2021", "deu_latf")]

    EXP = {arm: {"chronicling": ["p1"], "reichsanzeiger": ["p1"]}
           for arm in ("frak2021", "deu_latf")}

    def test_complete_lift(self):
        with tempfile.TemporaryDirectory() as d:
            rc, out = self.run_analyze(self.write(d, self.rows(), self.EXP))
            self.assertEqual(rc, 0)
            self.assertIn("**LIFT**", out)

    def test_incomplete_never_lifts(self):
        with tempfile.TemporaryDirectory() as d:
            rows = self.rows()[:1]       # one chronicling page, nothing else
            rc, out = self.run_analyze(self.write(d, rows, self.EXP))
            self.assertEqual(rc, 3)
            self.assertIn("INCOMPLETE", out)
            self.assertNotIn("**LIFT**", out)
            rc, out = self.run_analyze(self.write(d, self.rows(), self.EXP, limit=5))
            self.assertIn("--limit", out)
            self.assertNotIn("**LIFT**", out)
            rc, out = self.run_analyze(self.write(d, self.rows(), None))
            self.assertIn("INCOMPLETE", out)

    def test_year_window(self):
        with tempfile.TemporaryDirectory() as d:
            for y in ("", "1930", "1840"):
                rc, out = self.run_analyze(self.write(d, self.rows(y), self.EXP))
                self.assertNotIn("**LIFT**", out, y)
                self.assertIn("Year window", out)

    def test_duplicate_keys_error(self):
        with tempfile.TemporaryDirectory() as d:
            p = self.write(d, self.rows() + self.rows()[:1], self.EXP)
            with self.assertRaises(SystemExit):
                analyze.load([p])

if __name__ == "__main__":
    unittest.main(verbosity=1)
