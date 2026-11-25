import json
import os
import sys
import tempfile
import unittest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import letter_key as lk  # noqa: E402

EQ = {"ſ": "s", "ꝛ": "r"}
GLYPHS = {"a", "b", "c", "d", "e", "h", "n", "s", "t", "ſ", "T"}


def loc_word(text, x, y, line, w=40, h=20):
    return {"text": text, "box": [x, y, w, h], "line": line, "line_box": [0, y, 400, y + h]}


def tess_word(text, x, y, line, conf=99.0, w=40, h=20):
    step = w / max(1, len(text))
    chars = [{"text": ch, "box": [round(x + k * step), y, round(x + (k + 1) * step), y + h], "conf": conf} for k, ch in enumerate(text)]
    return {"text": text, "box": [x, y, w, h], "line": line, "line_box": [0, y - 2, 400, y + h + 2], "band": [y, y + h], "chars": chars}


class Selection(unittest.TestCase):
    def setUp(self):
        # line 0: the task's line; line 1: agreed words; line 2: a disagreement
        self.loc = [loc_word("the", 0, 0, 0), loc_word("cat", 50, 0, 0),
                    loc_word("Haſe", 0, 40, 1), loc_word("and", 50, 40, 1),
                    loc_word("hen", 0, 80, 2)]
        self.tess = [tess_word("the", 0, 0, 0), tess_word("cat", 50, 0, 0),
                     tess_word("Hase", 0, 40, 1), tess_word("and.", 50, 40, 1),
                     tess_word("ben", 0, 80, 2)]

    def found(self, task_words, min_conf=90):
        return lk.instances("p", self.loc, self.tess, task_words, GLYPHS, EQ, 0.5, min_conf)

    def test_agreement_only(self):
        got = self.found([])
        words = {x["word"] for x in got}
        self.assertIn(2, words)  # Haſe = Hase once ſ is s
        self.assertIn(3, words)  # and = and. once punctuation goes
        self.assertNotIn(4, words)  # hen against ben
        self.assertEqual({x["g"] for x in got if x["word"] == 0}, {"t", "h", "e"})

    def test_case_is_kept(self):
        self.tess[0] = tess_word("The", 0, 0, 0)
        self.assertNotIn(0, {x["word"] for x in self.found([])})

    def test_task_line_is_left_out_whole(self):
        got = self.found([1])  # the task's word is "cat" on line 0
        self.assertFalse([x for x in got if x["word"] in (0, 1)], "nothing from the task's line, not even its neighbours")
        self.assertTrue([x for x in got if x["word"] == 2])

    def test_tesseract_line_over_the_task_line_is_left_out(self):
        # Tesseract put "Hase" on the task's line (its line box overlaps the ALTO line of the task)
        self.tess[2] = dict(self.tess[2], line=0)
        got = self.found([1])
        self.assertFalse([x for x in got if x["word"] == 2])

    def test_low_confidence_letters_are_left_out(self):
        self.tess[3]["chars"][1]["conf"] = 50.0
        got = [x for x in self.found([]) if x["word"] == 3]
        self.assertEqual([x["g"] for x in got], ["a", "d"])

    def test_headline_type_is_left_out(self):
        self.loc.append(loc_word("the", 100, 120, 3, h=60))
        big = tess_word("the", 100, 120, 3, h=60)
        self.tess.append(big)
        got = self.found([])
        self.assertTrue(got)
        self.assertFalse([x for x in got if x["word"] == 5], "a line three times the page's usual height is not body type")

    def test_ligature_letters_are_left_out(self):
        self.loc[3] = loc_word("ach", 50, 40, 1)
        self.tess[3] = tess_word("ach", 50, 40, 1)
        got = lk.instances("p", self.loc, self.tess, [], GLYPHS, EQ, 0.5, 90, ligatures=["ch"])
        self.assertEqual([x["g"] for x in got if x["word"] == 3], ["a"])

    def test_only_listed_glyphs(self):
        self.assertFalse([x for x in self.found([]) if x["g"] in (".", "H")])


class Choosing(unittest.TestCase):
    def inst(self, page, g, word):
        return {"g": g, "page": page, "word": word, "char": 0, "box": [0, 0, 5, 5], "band": [0, 5]}

    def test_levels_in_order(self):
        pages = {"t1/d1/ed-1/seq-1": {}, "t1/d1/ed-1/seq-2": {}, "t1/d2/ed-1/seq-1": {}, "t2/d1/ed-1/seq-1": {}}
        pool = {"t1/d1/ed-1/seq-1": {"a": [self.inst("t1/d1/ed-1/seq-1", "a", 1)]},
                "t1/d1/ed-1/seq-2": {"a": [self.inst("t1/d1/ed-1/seq-2", "a", 2)], "b": [self.inst("t1/d1/ed-1/seq-2", "b", 3)]},
                "t1/d2/ed-1/seq-1": {"b": [self.inst("t1/d2/ed-1/seq-1", "b", 4)], "c": [self.inst("t1/d2/ed-1/seq-1", "c", 5)]},
                "t2/d1/ed-1/seq-1": {"d": [self.inst("t2/d1/ed-1/seq-1", "d", 6)]}}
        got = {e["g"]: e for e in lk.choose("t1/d1/ed-1/seq-1", pool, pages, ["a", "b", "c", "d"], 3, "s", "Font")}
        self.assertEqual(got["a"]["level"], "page")
        self.assertEqual([x["word"] for x in got["a"]["picks"]], [1], "the page's own letter, not the issue's too")
        self.assertEqual(got["b"]["level"], "issue")
        self.assertEqual(got["c"]["level"], "title")
        self.assertEqual(got["d"]["level"], "font", "another title is never a source")
        got = {e["g"]: e for e in lk.choose("t1/d1/ed-1/seq-1", pool, pages, ["d"], 3, "s", None)}
        self.assertEqual(got["d"]["level"], "none")

    def test_order_is_sha256_and_capped(self):
        page = "t/d/ed-1/seq-1"
        cand = [self.inst(page, "e", k) for k in range(10)]
        pool, pages = {page: {"e": cand}}, {page: {}}
        a = lk.choose(page, pool, pages, ["e"], 3, "seed", None)[0]["picks"]
        b = lk.choose(page, {page: {"e": list(reversed(cand))}}, pages, ["e"], 3, "seed", None)[0]["picks"]
        self.assertEqual(len(a), 3)
        self.assertEqual(a, b, "the input order doesn't matter")
        self.assertEqual(a, sorted(cand, key=lambda x: lk.rank("seed", x))[:3])
        c = lk.choose(page, pool, pages, ["e"], 3, "other", None)[0]["picks"]
        self.assertNotEqual([x["word"] for x in a], [x["word"] for x in c], "the seed decides")


HOCR = """<html><body><div class='ocr_page' title='bbox 0 0 200 100'>
<span class='ocr_line' id='l1' title='bbox 10 10 120 30; baseline 0 0'>
<span class='ocrx_word' id='w1' title='bbox 10 10 40 30; x_wconf 96'><span class='ocrx_cinfo' title='x_bboxes 10 10 20 30; x_conf 99.5'>T</span><span class='ocrx_cinfo' title='x_bboxes 20 12 30 30; x_conf 80.1'>o</span></span>
<span class='ocrx_word' id='w2' title='bbox 50 10 80 30; x_wconf 96'><span class='ocrx_cinfo' title='x_bboxes 50 12 60 30; x_conf 97'>&amp;</span></span>
</span></div></body></html>"""


class Reading(unittest.TestCase):
    def test_parse_hocr(self):
        words = lk.parse_hocr(HOCR)
        self.assertEqual([w["text"] for w in words], ["To", "&"])
        self.assertEqual(words[0]["box"], [10, 10, 30, 20])
        self.assertEqual(words[0]["line_box"], [10, 10, 120, 30])
        self.assertEqual(words[0]["band"], [10, 30], "from the letters, not the line box")
        self.assertEqual(words[0]["chars"][1], {"text": "o", "box": [20, 12, 30, 30], "conf": 80.1})

    def test_issue_and_title(self):
        self.assertEqual(lk.issue_of("sn1/1900-01-01/ed-1/seq-3"), "sn1/1900-01-01/ed-1")
        self.assertEqual(lk.title_of("sn1/1900-01-01/ed-1/seq-3"), "sn1")


class Build(unittest.TestCase):
    def test_build_patches_bodies_and_writes_keys(self):
        from PIL import Image
        with tempfile.TemporaryDirectory() as d:
            Image.new("RGB", (200, 100), "white").save(os.path.join(d, "p.png"))
            alto = ("<alto><Layout><Page WIDTH='200' HEIGHT='100'><PrintSpace><TextBlock>"
                    "<TextLine><String CONTENT='To' HPOS='10' VPOS='10' WIDTH='30' HEIGHT='20'/></TextLine>"
                    "<TextLine><String CONTENT='Tas' HPOS='10' VPOS='60' WIDTH='30' HEIGHT='20'/></TextLine>"
                    "</TextBlock></PrintSpace></Page></Layout></alto>")
            open(os.path.join(d, "p.xml"), "w").write(alto)
            hocr = os.path.join(d, "hocr")
            os.makedirs(hocr)
            line2 = ("<span class='ocr_line' title='bbox 10 60 120 80'><span class='ocrx_word' title='bbox 10 60 40 80'>"
                     "<span class='ocrx_cinfo' title='x_bboxes 10 60 20 80; x_conf 99'>T</span>"
                     "<span class='ocrx_cinfo' title='x_bboxes 20 62 30 80; x_conf 99'>a</span>"
                     "<span class='ocrx_cinfo' title='x_bboxes 30 62 40 80; x_conf 99'>s</span></span></span>")
            open(os.path.join(hocr, "t_d_ed-1_seq-1.hocr"), "w").write(HOCR.replace("</div>", line2 + "</div>"))
            pages = os.path.join(d, "pages.jsonl")
            open(pages, "w").write(json.dumps({"page": "t/d/ed-1/seq-1", "page_id": "t_d_ed-1_seq-1", "alto": "p.xml", "image": "p.png"}) + "\n")
            bd = os.path.join(d, "set")
            os.makedirs(bd)
            task = {"id": "x~1", "lang": "en-read", "set": "s1", "page": "t/d/ed-1/seq-1", "a": "Tas", "b": "Tes", "private": {"loc_word": 1}}
            open(os.path.join(bd, "tasks.jsonl"), "w").write(json.dumps(task) + "\n")
            open(os.path.join(bd, "language.json"), "w").write("{}")
            open(os.path.join(bd, "side-key.private"), "w").write("secret")
            cfg = os.path.join(d, "en.json")
            json.dump({"glyphs": ["T", "o", "a"], "glyph_font": "IM Fell English"}, open(cfg, "w"))
            out = os.path.join(d, "set-key")
            lk.main(["--pages", pages, "--hocr-dir", hocr, "--build-dir", bd, "--out-dir", out, "--language-config", cfg, "--min-conf", "90"])
            body = json.loads(open(os.path.join(out, "tasks.jsonl")).read())
            self.assertEqual(list(body), ["id", "lang", "set", "page", "a", "b", "key", "private"])
            self.assertEqual(body["key"], "en-read/s1/key-v1/t_d_ed-1_seq-1.json")
            m = json.load(open(os.path.join(out, "clips", body["key"])))
            got = {g["g"]: g for g in m["glyphs"]}
            self.assertEqual(got["T"]["level"], "page")  # from "To", line 0; the task's line (1) is left out
            self.assertEqual(len(got["T"]["crops"]), 1)
            self.assertEqual(got["o"]["level"], "font", "o is under --min-conf")
            self.assertEqual(got["a"]["level"], "font", "a is only on the task's line")
            self.assertTrue(os.path.exists(os.path.join(out, "clips", m["sprite"])))
            self.assertFalse(os.path.exists(os.path.join(out, "side-key.private")), "secrets stay behind")
            rep = json.load(open(os.path.join(out, "letter-key-report.json")))
            self.assertEqual(rep["glyph_levels_per_task"], {"page": 1, "issue": 0, "title": 0, "font": 2, "none": 0})
            with self.assertRaises(SystemExit):
                lk.main(["--pages", pages, "--hocr-dir", hocr, "--build-dir", bd, "--out-dir", out, "--language-config", cfg])


if __name__ == "__main__":
    unittest.main()
