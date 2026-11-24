import io
import json
import os
import sys
import tarfile
import tempfile
import unittest
from collections import Counter

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import tess_pick as tp  # noqa: E402
import english_pages as ep  # noqa: E402

try:
    import PIL  # noqa: F401
    HAVE_PIL = True
except ImportError:
    HAVE_PIL = False


def w(text, x, y, line, ww=50, h=20):
    return {"text": text, "box": [x, y, ww, h], "line": line, "line_box": [0, y, 1000, y + h], "conf": 90.0}


class Args:
    lang, set, min_iou, max_other, min_page_aligned = "en-read", "t", 0.5, 0.2, 0.5


def page(**kw):
    return dict({"page": "sn1/1900-01-01/ed-1/seq-1", "page_id": "sn1_1900-01-01_ed-1_seq-1", "date": "1900-01-01"}, **kw)


class Normalise(unittest.TestCase):
    def test_as_score2122(self):
        self.assertEqual(tp.normalise("The,"), "the")
        self.assertEqual(tp.normalise("‘Tis"), "tis")
        self.assertEqual(tp.normalise("don’t"), "don't")
        self.assertEqual(tp.normalise("U.S."), "u s")
        self.assertEqual(tp.normalise("$5"), "$5")
        self.assertEqual(tp.normalise("ſo"), "so")  # NFKC: the long s is s
        self.assertEqual(tp.normalise("—"), "")


class Align(unittest.TestCase):
    def test_mutual_best_pair(self):
        pairs, why, unpaired = tp.align([w("the", 0, 0, 0)], [w("tbe", 4, 1, 0)], 0.5, 0.2)
        self.assertEqual(list(pairs), [0])
        self.assertEqual(pairs[0][0], 0)
        self.assertEqual((why, unpaired), ({}, 0))

    def test_split_is_left_out(self):
        loc = [w("together", 0, 0, 0, ww=100)]
        tess = [w("to", 0, 0, 0, ww=45), w("gether", 50, 0, 0, ww=50)]
        pairs, why, _ = tp.align(loc, tess, 0.5, 0.2)
        self.assertEqual((pairs, why), ({}, {0: "split_or_joined"}))

    def test_join_is_left_out(self):
        loc = [w("to", 0, 0, 0, ww=45), w("gether", 50, 0, 0, ww=50)]
        tess = [w("together", 0, 0, 0, ww=100)]
        pairs, why, _ = tp.align(loc, tess, 0.5, 0.2)
        self.assertEqual(pairs, {})
        self.assertEqual(set(why.values()), {"split_or_joined"})

    def test_no_overlap_and_low_overlap(self):
        pairs, why, unpaired = tp.align([w("a", 0, 0, 0), w("b", 500, 0, 0)], [w("b", 530, 0, 0)], 0.5, 0.2)
        self.assertEqual(why, {0: "no_tesseract_word", 1: "low_overlap"})
        self.assertEqual(unpaired, 1)


class Classify(unittest.TestCase):
    def run_page(self, loc, tess, **kw):
        stats = Counter()
        return tp.page_candidates(page(**kw), loc, tess, Args, stats)

    def test_reasons(self):
        loc = [w("The", 0, 0, 0), w("tbe", 60, 0, 0), w("foun-", 120, 0, 0), w("dation", 0, 30, 1),
               w("U.S.", 60, 30, 1), w("—", 120, 30, 1), w("cat,", 180, 30, 1)]
        tess = [w("the", 0, 0, 0), w("the", 60, 0, 0), w("foun", 120, 0, 0), w("datlon", 0, 30, 1),
                w("US", 60, 30, 1), w("a", 120, 30, 1), w("cat.", 180, 30, 1)]
        rows, c = self.run_page(loc, tess)
        self.assertEqual([(r["loc"], r["tesseract"]) for r in rows], [("tbe", "the")])
        rows2, _ = self.run_page([w("a", 0, 0, 0), w("Tbe,", 60, 0, 0)], [w("a", 0, 0, 0), w("the.", 60, 0, 0)])
        self.assertEqual([(r["loc"], r["tesseract"], r["norm"]["loc"]) for r in rows2], [("Tbe,", "the.", "tbe")])  # raw shown
        self.assertEqual((c["agree_after_normalisation"], c["left_out_hyphen_cut"], c["left_out_several_words"],
                          c["left_out_punctuation_only"], c["disagree"]), (2, 2, 1, 1, 1))
        self.assertEqual(rows[0]["norm"], {"loc": "tbe", "tesseract": "the"})

    def test_page_with_boxes_that_do_not_line_up_is_left_out(self):
        loc = [w("a", 0, 0, 0), w("b", 60, 0, 0), w("c", 120, 0, 0)]
        tess = [w("a", 0, 300, 0)]
        rows, c = self.run_page(loc, tess)
        self.assertEqual((rows, c["page_alignment_failed"]), ([], 1))

    def test_gold_page_carries_truth(self):
        loc = [w("tbe", 0, 0, 0), w("cat", 60, 0, 0), w("sbt", 120, 0, 0)]
        tess = [w("the", 0, 0, 0), w("cat", 60, 0, 0), w("sat", 120, 0, 0)]
        regions = [{"id": "r1", "box": [0, 0, 100, 40], "text": "The cat"}]
        rows, c = self.run_page(loc, tess, regions=regions)
        self.assertEqual([(r["loc"], r["truth"], r["truth_word"], r["region"]) for r in rows], [("tbe", "tesseract", "the", "r1")])
        self.assertEqual(c["outside_truth"], 1)

    def test_sample_weights_and_rank(self):
        rows = [{"id": f"en/t/p~{k}"} for k in range(10)]
        got = tp.sample(rows, 3, "2122")
        self.assertEqual(len(got), 3)
        self.assertTrue(all(r["page_candidates"] == 10 and r["page_sampled"] == 3 for r in got))
        self.assertEqual([r["id"] for r in got], [r["id"] for r in tp.sample(list(reversed(rows)), 3, "2122")])

    def test_gold_sample_fills_quotas_round_robin(self):
        rows = {f"p{k}": [{"id": f"en-read/g/p{k}~{i}", "truth": c} for i, c in enumerate(["tesseract"] * 5 + ["loc"] * 5 + ["neither"])]
                for k in range(3)}
        got = tp.gold_sample(rows, tp.parse_quota("tesseract=4,loc=3,neither=1"), "2122")
        flat = [r for v in got.values() for r in v]
        self.assertEqual(Counter(r["truth"] for r in flat), Counter(tesseract=4, loc=3, neither=1))
        self.assertLessEqual(max(sum(r["truth"] == "tesseract" for r in v) for v in got.values()), 2)
        self.assertTrue(all(r["page_candidates"] == 11 for r in flat))
        with self.assertRaises(SystemExit):
            tp.parse_quota("tess=1")

    def test_side_is_balanced_and_keyed(self):
        sides = Counter(tp.side("k", f"en/t/p~{k}") for k in range(400))
        self.assertTrue(150 < sides["tesseract"] < 250)
        self.assertNotEqual([tp.side("k", f"x{k}") for k in range(20)], [tp.side("j", f"x{k}") for k in range(20)])

    def test_hyphen_cut_marks_next_line_start(self):
        ws = [w("a", 0, 0, 0), w("foun-", 60, 0, 0), w("dation", 0, 30, 1), w("x", 60, 30, 1)]
        self.assertEqual(tp.hyphen_cut(ws), {1, 2})


ALTO = """<alto xmlns="http://www.loc.gov/standards/alto/ns-v3#"><Layout><Page WIDTH="400" HEIGHT="200"><PrintSpace><TextBlock>
<TextLine><String CONTENT="The" HPOS="10" VPOS="10" WIDTH="50" HEIGHT="20"/><SP/><String CONTENT="tbe" HPOS="70" VPOS="10" WIDTH="50" HEIGHT="20"/><SP/><String CONTENT="cat" HPOS="130" VPOS="10" WIDTH="50" HEIGHT="20"/></TextLine>
</TextBlock></PrintSpace></Page></Layout></alto>"""
TSV_HEAD = "level\tpage_num\tblock_num\tpar_num\tline_num\tword_num\tleft\ttop\twidth\theight\tconf\ttext\n"


@unittest.skipUnless(HAVE_PIL, "needs Pillow")
class Build(unittest.TestCase):
    def test_build_writes_blind_tasks_and_reports(self):
        from PIL import Image
        with tempfile.TemporaryDirectory() as d:
            Image.new("L", (400, 200), 255).save(os.path.join(d, "p.png"))
            open(os.path.join(d, "p.xml"), "w").write(ALTO)
            rows = [(10, "The"), (70, "the"), (130, "cat")]
            tsv = TSV_HEAD + "".join(f"5\t1\t1\t1\t1\t{k}\t{x + 2}\t11\t48\t19\t91.5\t{t}\n" for k, (x, t) in enumerate(rows, 1))
            open(os.path.join(d, "p.tsv"), "w").write(tsv)
            with open(os.path.join(d, "pages.jsonl"), "w") as f:
                f.write(json.dumps(page(alto="p.xml", image="p.png", tsv="p.tsv", damage_rate=0.3)) + "\n")
            cfg = os.path.join(HERE, "..", "languages", "en-read.json")
            out = os.path.join(d, "out")
            tp.main(["--pages", os.path.join(d, "pages.jsonl"), "--set", "t", "--language-config", cfg, "--out-dir", out])
            tasks = [json.loads(l) for l in open(os.path.join(out, "tasks.jsonl"))]
            self.assertEqual(len(tasks), 1)
            t = tasks[0]
            self.assertEqual(t["id"], "en-read/t/sn1_1900-01-01_ed-1_seq-1~1")
            self.assertEqual({t["a"], t["b"]}, {"tbe", "the"})
            p = t["private"]
            self.assertEqual({p["a_is"]: t["a"], p["b_is"]: t["b"]}, {"loc": "tbe", "tesseract": "the"})
            self.assertEqual((p["page_candidates"], p["page_sampled"], p["strata"]), (1, 1, {"damage_band": "bad", "decade": 1900}))
            for k in ("line", "word"):
                self.assertTrue(os.path.exists(os.path.join(out, "clips", t[k])))
            self.assertTrue(all(0 <= v <= 1 for v in t["box"]))
            rep = json.load(open(os.path.join(out, "report.json")))
            self.assertEqual((rep["disagree"], rep.get("agree_after_normalisation", 0), rep["agree_exact"]), (1, 0, 2))
            pages = [json.loads(l) for l in open(os.path.join(out, "pages.jsonl"))]
            self.assertEqual((pages[0]["disagree"], pages[0]["sampled"]), (1, 1))
            self.assertTrue(os.path.exists(os.path.join(out, "side-key.private")))
            self.assertNotIn("examples", json.load(open(cfg)))


class Prepare(unittest.TestCase):
    def test_member_doc_and_canonical(self):
        self.assertEqual(ep.member_doc("batch/data/sn1/00123/1900010101/0001/../x"), None)
        self.assertEqual(ep.member_doc("./sn83020866/1828/04/03/ed-1/seq-1/ocr.xml"), "sn83020866_1828-04-03_ed-1_seq-1")
        self.assertEqual(ep.member_doc("sn1/1900-01-01/ed-1/seq-2/ocr.xml"), "sn1_1900-01-01_ed-1_seq-2")
        self.assertIsNone(ep.member_doc("sn1/1900/01/01/ed-1/seq-2/ocr.txt"))
        self.assertEqual(ep.canonical("sn1_1900-01-01_ed-01_seq-002"), "sn1_1900-01-01_ed-1_seq-2")

    def test_scan_path_flat_then_held_layout(self):
        with tempfile.TemporaryDirectory() as d:
            row = {"doc_id": "sn1_1900-01-02_ed-1_seq-03", "batch": "x_y", "format": "jp2"}
            self.assertIsNone(ep.scan_path(d, row))
            held = os.path.join(d, "buffer", "x_y", "sn1", "1900-01-02", "1")
            os.makedirs(held)
            open(os.path.join(held, "3.jp2"), "w").close()
            self.assertEqual(ep.scan_path(d, row), os.path.join(held, "3.jp2"))
            open(os.path.join(d, "sn1_1900-01-02_ed-1_seq-03.jp2"), "w").close()
            self.assertEqual(ep.scan_path(d, row), os.path.join(d, "sn1_1900-01-02_ed-1_seq-03.jp2"))

    def test_status_136_as_the_worker_reads_it(self):
        self.assertTrue(ep.is_status_136(136) and ep.is_status_136(-8))
        self.assertFalse(ep.is_status_136(1) or ep.is_status_136(-9))

    def test_read_members_stops_at_the_last_wanted(self):
        with tempfile.TemporaryDirectory() as d:
            arc = os.path.join(d, "x_ver01.tar.bz2")
            with tarfile.open(arc, "w:bz2") as tf:
                for name, body in (("sn1/1900/01/01/ed-1/seq-1/ocr.xml", b"<a/>"), ("sn1/1900/01/01/ed-1/seq-2/ocr.xml", b"<b/>"),
                                   ("sn1/1900/01/01/ed-1/seq-3/ocr.xml", b"<c/>")):
                    ti = tarfile.TarInfo(name)
                    ti.size = len(body)
                    tf.addfile(ti, io.BytesIO(body))
            got = ep.read_members(arc, {"sn1_1900-01-01_ed-1_seq-2"}, d)
            self.assertEqual(list(got), ["sn1_1900-01-01_ed-1_seq-2"])
            with open(os.path.join(d, "sn1_1900-01-01_ed-1_seq-2.xml"), "rb") as f:
                self.assertEqual(f.read(), b"<b/>")
            self.assertFalse(os.path.exists(os.path.join(d, "sn1_1900-01-01_ed-1_seq-3.xml")))


if __name__ == "__main__":
    unittest.main()
