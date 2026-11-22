#!/usr/bin/env python3
"""Unit tests for score.py and analyze.py: CER, fold, word F1, PAGE-XML and ALTO
readers, page discovery, resume checks, year window and completeness; line_score.py's
line alignment and analyze.py's rule A2."""
import csv, json, os, random, sys, tempfile, unittest
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import analyze, line_score, score

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
            pages, _, _ = score.find_pages("reichsanzeiger", d, {}, (1850, 1920), False)
            self.assertEqual([p["page_id"] for p in pages], ["1891_1_0001"])
            self.assertNotIn(".clone", pages[0]["xml"])

    def test_nzz_only_groundtruth(self):
        with tempfile.TemporaryDirectory() as d:
            for sub in ("NZZ_groundtruth", "NZZ_abbyy"):
                self._touch(d, sub + "/1900_a.xml")
            self._touch(d, "img/1900_a.png", "")
            pages, _, _ = score.find_pages("nzz", d, {}, (1850, 1920), False)
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
            pages, _, _ = score.find_pages("nzz", d, {}, (1850, 1920), False)
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
            pages, _, _ = score.find_pages("chronicling", d, meta, (1850, 1920), False)
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
        old["gpu_study_sha256"] = "x"
        self.assertEqual(score.resume_problems(old, {**new, "gpu_study_sha256": "x"}, set()), [])
        self.assertTrue(score.resume_problems(old, {**new, "gpu_study_sha256": "y"}, set()))

    def test_resume_refuses_limit_mix(self):
        old = {"arms": {"frak2021": "aa"}, "cmd": "c", "tesseract_version": "5.5"}
        self.assertEqual(score.resume_problems(old, {**old, "limit": 0}, set()), [])
        self.assertTrue(score.resume_problems({**old, "limit": 1}, {**old, "limit": 0}, set()))
        self.assertTrue(score.resume_problems({**old, "limit": 1}, {**old, "limit": 1}, set()))
        self.assertTrue(score.resume_problems(old, {**old, "limit": 2}, set()))

    def test_gpu_study_ids_normalised(self):
        gpu = {score.gpu_norm(x) for x in (
            "nzz/nzz_18501112_0_0_a1_p1_1\r\n", "  \ufeffnzz/0007_nzz_18501113_p2.xml ",
            "chronicling/sn83_1900.jpg", "bare_page.gt.xml")}
        self.assertIn("nzz/nzz_18501113_p2", gpu)
        for set_, pid in (("nzz", "nzz_18501112_0_0_a1_p1_1"),
                          ("nzz", "0042_nzz_18501112_0_0_a1_p1_1"),
                          ("nzz", "nzz_18501112_0_0_a1_p1_1.gt"),
                          ("nzz", "0003_nzz_18501113_p2"),
                          ("chronicling", "sn83_1900"), ("reichsanzeiger", "bare_page")):
            self.assertEqual(score.gpu_flag(set_, pid, gpu), 1, pid)
        self.assertEqual(score.gpu_flag("nzz", "nzz_18501199_p9", gpu), 0)
        self.assertEqual(score.gpu_flag("nzz", "sn83_1900", gpu), 0)  # set-qualified

    def test_resume_keeps_missing_and_omitted_arm_expectations(self):
        import io, contextlib
        with tempfile.TemporaryDirectory() as d:
            root = os.path.join(d, "root"); os.makedirs(os.path.join(root, "chronicling"))
            out = os.path.join(d, "o.csv")
            with open(out, "w", newline="") as f:
                w = csv.DictWriter(f, score.FIELDS); w.writeheader()
                w.writerow(dict(set="chronicling", page_id="c1", arm="frak2021"))
            exp = {a: {"chronicling": ["c1"], "reichsanzeiger": ["r1"]}
                   for a in ("frak2021", "deu_latf")}
            with open(out + ".meta.json", "w") as f:
                json.dump({"arms": {"frak2021": None, "deu_latf": None}, "cmd": score.DEFAULT_CMD,
                           "tesseract_version": "v", "limit": 0, "cpu_cgroup": None,
                           "gpu_study_sha256": score.hashlib.sha256(b"").hexdigest(),
                           "expected": exp,
                           "missing_images": {"reichsanzeiger": ["r2"]}}, f)
            args = ["--data-root", root, "--sets", "chronicling", "--arms", "frak2021",
                    "--out", out, "--version-cmd", "echo v"]
            err = io.StringIO()
            with contextlib.redirect_stderr(err):
                rc = score.main(args)                    # unresolved r2 blocks the run
            self.assertEqual(rc, 2)
            with contextlib.redirect_stderr(err):
                rc = score.main(args + ["--allow-missing-images"])
            self.assertEqual(rc, 0, err.getvalue())
            with open(out + ".meta.json") as f:
                m = json.load(f)
            self.assertEqual(m["missing_images"], {"reichsanzeiger": ["r2"]})
            self.assertEqual(m["expected"]["deu_latf"], exp["deu_latf"])
            self.assertEqual(m["expected"]["frak2021"], exp["frak2021"])

    def test_missing_image_after_year_filter(self):
        with tempfile.TemporaryDirectory() as d:
            for n in ("a_1900", "b_1900", "c_1990"):    # c is out of window
                with open(os.path.join(d, n + ".xml"), "w") as f:
                    f.write(PAGE)
            open(os.path.join(d, "a_1900.png"), "w").close()
            pages, skipped, missing = score.find_pages("reichsanzeiger", d, {}, (1850, 1920), False)
            self.assertEqual([p["page_id"] for p in pages], ["a_1900"])
            self.assertEqual(missing, ["b_1900"])
            self.assertEqual(skipped, 1)

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

    def test_provenance_mismatch_refused(self):
        with tempfile.TemporaryDirectory() as d:
            rows = self.rows()
            p1 = self.write(d, rows[:2], self.EXP, name="a.csv")
            p2 = self.write(d, rows[2:], self.EXP, name="b.csv")
            for p, ver in ((p1, "5.5"), (p2, "5.5")):
                with open(p + ".meta.json") as f:
                    m = json.load(f)
                m.update(tesseract_version=ver, cmd="c", arms={"frak2021": "aa"})
                with open(p + ".meta.json", "w") as f:
                    json.dump(m, f)
            rc, out = self.run_two(p1, p2)
            self.assertEqual(rc, 0)
            with open(p2 + ".meta.json") as f:
                m = json.load(f)
            for k, v in (("tesseract_version", "5.4"), ("cmd", "d"),
                         ("arms", {"frak2021": "bb"})):
                with open(p2 + ".meta.json", "w") as f:
                    json.dump({**m, k: v}, f)
                rc, out = self.run_two(p1, p2)
                self.assertEqual(rc, 3, k)
                self.assertIn("differs between", out)
                self.assertNotIn("**LIFT**", out)

    def test_separate_arm_csvs_merge(self):
        with tempfile.TemporaryDirectory() as d:
            rows = self.rows()
            p1 = self.write(d, [r for r in rows if r[2] == "frak2021"], self.EXP, name="a.csv")
            p2 = self.write(d, [r for r in rows if r[2] == "deu_latf"], self.EXP, name="b.csv")
            for p, arms in ((p1, {"frak2021": "aa"}), (p2, {"deu_latf": "bb"})):
                with open(p + ".meta.json") as f:
                    m = json.load(f)
                m.update(tesseract_version="5.5", cmd="c", arms=arms)
                with open(p + ".meta.json", "w") as f:
                    json.dump(m, f)
            rc, out = self.run_two(p1, p2)
            self.assertEqual(rc, 0, out)
            self.assertNotIn("differs between", out)
            with open(p2 + ".meta.json") as f:
                m = json.load(f)
            m["arms"] = {"deu_latf": "bb", "frak2021": "zz"}      # real conflict
            with open(p2 + ".meta.json", "w") as f:
                json.dump(m, f)
            rc, out = self.run_two(p1, p2)
            self.assertEqual(rc, 3)
            self.assertIn("differs between", out)

    def run_two(self, p1, p2):
        import io, contextlib
        buf = io.StringIO()
        with contextlib.redirect_stdout(buf):
            rc = analyze.main(["--csv", p1, p2, "--n-boot", "200"])
        return rc, buf.getvalue()

    def test_completeness_uses_year_filtered_rows(self):
        with tempfile.TemporaryDirectory() as d:
            # reichsanzeiger only has an out-of-window page: no retained pool rows
            rows = [r for r in self.rows() if r[0] == "chronicling"] + \
                   [("reichsanzeiger", "p1", arm, "1930", 0) for arm in ("frak2021", "deu_latf")]
            rc, out = self.run_analyze(self.write(d, rows, self.EXP))
            self.assertEqual(rc, 3)
            self.assertIn("no retained non-GPU-study pages", out)
            self.assertNotIn("**LIFT**", out)
            # an expected page with no row at all is missing
            exp = {arm: {"chronicling": ["p1", "p2"], "reichsanzeiger": ["p1"]}
                   for arm in ("frak2021", "deu_latf")}
            rc, out = self.run_analyze(self.write(d, self.rows(), exp))
            self.assertEqual(rc, 3)
            self.assertIn("expected pages missing", out)
            # an expected page present but outside the window is not expected
            rows = self.rows() + [("chronicling", "p2", arm, "1930", 0)
                                  for arm in ("frak2021", "deu_latf")]
            rc, out = self.run_analyze(self.write(d, rows, exp))
            self.assertEqual(rc, 0)

    def test_flagged_set_holds_before_decision(self):
        with tempfile.TemporaryDirectory() as d:
            rows = self.rows() + [("nzz", "n1", arm, "1900", 300)
                                  for arm in ("frak2021", "deu_latf")]
            exp = {arm: {**self.EXP[arm], "nzz": ["n1"]} for arm in self.EXP}
            rc, out = self.run_analyze(self.write(d, rows, exp))
            self.assertEqual(rc, 0)
            self.assertIn("**HOLD**", out)
            self.assertNotIn("**LIFT**", out)
            self.assertIn("FLAG", out)

    def test_missing_images_make_run_incomplete(self):
        with tempfile.TemporaryDirectory() as d:
            p = self.write(d, self.rows(), self.EXP)
            with open(p + ".meta.json", "w") as f:
                json.dump({"expected": self.EXP, "limit": 0,
                           "missing_images": {"chronicling": ["p9"]}}, f)
            rc, out = self.run_analyze(p)
            self.assertEqual(rc, 3)
            self.assertIn("no image", out)
            self.assertNotIn("**LIFT**", out)

    def test_duplicate_keys_error(self):
        with tempfile.TemporaryDirectory() as d:
            p = self.write(d, self.rows() + self.rows()[:1], self.EXP)
            with self.assertRaises(SystemExit):
                analyze.load([p])

    def test_title_key_merges_spellings(self):
        self.assertEqual(analyze.title_key("Weisseritz-Zeitung"), analyze.title_key("Weisseritz_Zeitung"))
        self.assertEqual(analyze.title_key(" Neue  Zeitung "), "Neue_Zeitung")

    def test_line_alignment_ignores_reading_order(self):
        lines = [(0, 0, 100, 20, "Die ſtadt"), (0, 30, 100, 50, "zweite Zeile"), (0, 60, 100, 80, "leer")]
        # engine coordinates at twice the truth's scale, words out of order
        words = [(100, 64, 180, 96, "Zeile"), (0, 64, 80, 96, "zweite"),
                 (70, 4, 180, 36, "stadt"), (0, 4, 60, 36, "Die")]
        pairs = line_score.align(lines, words, (100, 100), (200, 200))
        self.assertEqual([p[1] for p in pairs], ["Die stadt", "zweite Zeile", ""])
        s = line_score.score_lines(pairs)
        self.assertEqual((s["lines"], s["lost_lines"], s["lost_chars"]), (3, 1, 4))
        self.assertEqual(s["edits_folded"], 4)       # the lost line only; ſ folds to s
        self.assertEqual(s["edits_raw"], 5)
        self.assertEqual(s["ref_chars"], 25)

    def test_gt_lines_page_bbox(self):
        with tempfile.TemporaryDirectory() as d:
            p = os.path.join(d, "x.xml")
            with open(p, "w", encoding="utf-8") as f:
                f.write('''<PcGts xmlns="http://schema.primaresearch.org/PAGE/gts/pagecontent/2019-07-15">
 <Page imageWidth="400" imageHeight="300"><TextRegion id="r"><TextLine id="l">
 <Coords points="10,20 90,18 95,40 12,42"/><TextEquiv><Unicode>Haus</Unicode></TextEquiv>
 </TextLine></TextRegion></Page></PcGts>''')
            lines, size = line_score.gt_lines(p)
        self.assertEqual(size, (400.0, 300.0))
        self.assertEqual(lines, [(10.0, 18.0, 95.0, 42.0, "Haus")])

    def test_rule_a2(self):
        ok = {"page_ci": (0.1, 0.17), "title_ci": (0.1, 0.2), "lost": 0.03}
        self.assertEqual(analyze.rule_a2(ok, []), ("LIFT", []))
        for bad, part in (({"page_ci": (0.1, 0.18)}, "page"), ({"title_ci": (0.1, 0.24)}, "title"),
                          ({"lost": 0.06}, "lost")):
            v, why = analyze.rule_a2({**ok, **bad}, [])
            self.assertEqual(v, "HOLD")
            self.assertIn(part, why[0])
        self.assertEqual(analyze.rule_a2(ok, ["chronicling"])[0], "HOLD")
        self.assertEqual(analyze.rule_a2({k: v for k, v in ok.items() if k != "lost"}, [])[0], "HOLD")

    def test_rule_a2_nan_limits_fail(self):
        nan = float("nan")
        ok = {"page_ci": (0.1, 0.17), "title_ci": (0.1, 0.2), "lost": 0.03}
        for bad in ({"page_ci": (nan, nan)}, {"title_ci": (nan, nan)}, {"lost": nan},
                    {"ref_chars": 0}):
            self.assertEqual(analyze.rule_a2({**ok, **bad}, [])[0], "HOLD", bad)

    def test_zero_reference_pool_never_lifts(self):
        with tempfile.TemporaryDirectory() as d:
            p = os.path.join(d, "z.csv")
            with open(p, "w", newline="") as f:
                w = csv.DictWriter(f, line_score.FIELDS)
                w.writeheader()
                for s in ("chronicling", "reichsanzeiger"):
                    for arm in ("frak2021", "deu_latf"):
                        w.writerow(dict(set=s, stratum="t", page_id="p1", title="t", year="1900",
                                        arm=arm, ref_chars=0, edits_raw=0, edits_folded=0,
                                        cer_raw=0, cer_folded=0, word_f1=1, cpu_s=1,
                                        gpu_study_flag=0, lost_chars=0, lines=0, lost_lines=0))
            with open(p + ".meta.json", "w") as f:
                json.dump({"expected": A.EXP, "limit": 0}, f)
            _, out = A().run_analyze(p, "--rule", "A2")
            self.assertNotIn("**LIFT**", out)

    def test_page_truth_line_without_coords_fails(self):
        with tempfile.TemporaryDirectory() as d:
            p = os.path.join(d, "x.xml")
            with open(p, "w", encoding="utf-8") as f:
                f.write('''<PcGts xmlns="http://schema.primaresearch.org/PAGE/gts/pagecontent/2019-07-15">
 <Page imageWidth="400" imageHeight="300"><TextRegion id="r"><TextLine id="l">
 <TextEquiv><Unicode>Haus</Unicode></TextEquiv></TextLine></TextRegion></Page></PcGts>''')
            with self.assertRaises(ValueError):
                line_score.gt_lines(p)
            row, err = line_score.job(({"xml": p, "img": p, "set": "s", "page_id": "x"},
                                       "m", "false", d, 5, set(), d, {}))
            self.assertIsNone(row)
            self.assertIn("no usable Coords", err)

    def _job_fixture(self, d, body):
        xml = os.path.join(d, "gt.xml")
        with open(xml, "w", encoding="utf-8") as f:
            f.write('''<PcGts xmlns="http://schema.primaresearch.org/PAGE/gts/pagecontent/2019-07-15">
 <Page imageWidth="100" imageHeight="100"><TextLine id="l"><Coords points="0,0 100,0 100,20 0,20"/>
 <TextEquiv><Unicode>Haus</Unicode></TextEquiv></TextLine></Page></PcGts>''')
        img = os.path.join(d, "i.png")
        with open(img, "wb") as f:
            f.write(b"img1")
        fake = os.path.join(d, "fake.py")
        with open(fake, "w") as f:
            f.write("import sys\nopen(%r, 'a').write('x')\nsys.stdout.write(%r)\n"
                    % (os.path.join(d, "calls"), body))
        cmd = "python3 %s {img}" % fake
        alto = os.path.join(d, "alto")
        page = {"xml": xml, "img": img, "set": "s", "page_id": "p", "title": "t",
                "year": "1900", "stratum": "x"}
        calls = lambda: len(open(os.path.join(d, "calls")).read()) if os.path.exists(
            os.path.join(d, "calls")) else 0
        return page, cmd, alto, img, calls

    def test_alto_cache_keyed_by_provenance_and_validated(self):
        good = ALTO.replace("<String CONTENT=\"Haus\"/>", "<String CONTENT=\"Haus\" HPOS=\"1\" VPOS=\"1\" WIDTH=\"5\" HEIGHT=\"5\"/>")
        with tempfile.TemporaryDirectory() as d:
            page, cmd, alto, img, calls = self._job_fixture(d, good)
            def run(prov):
                return line_score.job((page, "m", cmd, d, 30, set(), alto, prov))
            prov = {"model_sha256": "aa", "version": "t 1"}
            self.assertIsNone(run(prov)[1]); self.assertEqual(calls(), 1)
            self.assertIsNone(run(prov)[1]); self.assertEqual(calls(), 1)     # cache hit
            run({**prov, "model_sha256": "bb"}); self.assertEqual(calls(), 2)  # model changed
            run({**prov, "version": "t 2"}); self.assertEqual(calls(), 3)      # engine changed
            with open(img, "wb") as f:
                f.write(b"img2")
            run({**prov, "version": "t 2"}); self.assertEqual(calls(), 4)      # image changed

    def test_unparseable_alto_not_cached(self):
        with tempfile.TemporaryDirectory() as d:
            page, cmd, alto, img, calls = self._job_fixture(d, "<alto><oops")
            row, err = line_score.job((page, "m", cmd, d, 30, set(), alto, {}))
            self.assertIsNone(row)
            self.assertIsNotNone(err)
            self.assertFalse(os.path.exists(os.path.join(alto, "s", "m", "p.xml")))
            self.assertFalse(os.path.exists(os.path.join(alto, "s", "m", "p.xml.json")))

    def test_merge_resume_keeps_ids_and_missing(self):
        old = {"expected": {"a": {"s": ["p1", "p2"]}, "b": {"s": ["p1"]}},
               "missing_images": {"s": ["p3", "p2"]}}
        exp, miss = score.merge_resume(old, {"a": {"s": ["p4"]}}, {})
        self.assertEqual(exp, {"a": {"s": ["p1", "p2", "p4"]}, "b": {"s": ["p1"]}})
        self.assertEqual(miss, {"s": ["p3"]})       # p2 now has an image

    def test_rule_a_rejects_line_aligned_csv(self):
        with tempfile.TemporaryDirectory() as d:
            p = os.path.join(d, "l.csv")
            with open(p, "w", newline="") as f:
                w = csv.DictWriter(f, line_score.FIELDS)
                w.writeheader()
                w.writerow(dict(set="chronicling", stratum="t", page_id="p1", title="t",
                                year="1900", arm="frak2021", ref_chars=10, edits_raw=0,
                                edits_folded=0, cer_raw=0, cer_folded=0, word_f1=1, cpu_s=1,
                                gpu_study_flag=0, lost_chars=0, lines=1, lost_lines=0))
            with self.assertRaises(SystemExit) as cm:
                A().run_analyze(p)          # default rule A
            self.assertNotEqual(cm.exception.code, 0)
            A().run_analyze(p, "--rule", "A2")
            rc, _ = A().run_analyze(A().write(d, A().rows(), A.EXP, name="page.csv"))
            self.assertEqual(rc, 0)         # page CSVs still fine under A

    def test_cache_hit_restores_cpu_and_unknown_is_empty(self):
        good = ALTO.replace("<String CONTENT=\"Haus\"/>", "<String CONTENT=\"Haus\" HPOS=\"1\" VPOS=\"1\" WIDTH=\"5\" HEIGHT=\"5\"/>")
        with tempfile.TemporaryDirectory() as d:
            page, cmd, alto, img, calls = self._job_fixture(d, good)
            prov = {"model_sha256": "aa", "version": "t 1"}
            r1, _ = line_score.job((page, "m", cmd, d, 30, set(), alto, prov))
            man = os.path.join(alto, "s", "m", "p.xml.json")
            with open(man) as f:
                m = json.load(f)
            m["cpu_s"] = 7.5
            with open(man, "w") as f:
                json.dump(m, f)
            r2, _ = line_score.job((page, "m", cmd, d, 30, set(), alto, prov))
            self.assertEqual(calls(), 1)
            self.assertEqual(r2["cpu_s"], 7.5)
            del m["cpu_s"]
            with open(man, "w") as f:
                json.dump(m, f)
            r3, _ = line_score.job((page, "m", cmd, d, 30, set(), alto, prov))
            self.assertEqual(r3["cpu_s"], "")

    def test_cpu_average_skips_unknown(self):
        rows = [dict(ref_chars=10, edits_folded=1, edits_raw=1, word_f1=1.0, title="t",
                     cpu_s=c) for c in (2.0, None, 4.0)]
        self.assertEqual(analyze.summarize("x", rows, 20, 1)["cpu"], 3.0)
        self.assertIsNone(analyze.summarize("x", rows[1:2], 20, 1)["cpu"])

    def test_engine_hyp_attaches_to_previous_word(self):
        alto = (b'<alto xmlns="http://www.loc.gov/standards/alto/ns-v3#"><Layout>'
                b'<Page WIDTH="100" HEIGHT="50"><PrintSpace><TextBlock><TextLine>'
                b'<String CONTENT="Kai" HPOS="10" VPOS="10" WIDTH="20" HEIGHT="10"/>'
                b'<HYP CONTENT="-"/></TextLine></TextBlock></PrintSpace></Page></Layout></alto>')
        words, size = line_score.alto_words(alto)
        self.assertEqual([w[4] for w in words], ["Kai-"])
        self.assertEqual(size, (100.0, 50.0))

if __name__ == "__main__":
    unittest.main(verbosity=1)
