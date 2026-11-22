"""render_compare/templates.py: harvest, letter cuts, templates and the template renderer (#2103).

The cut and renderer tests need the pinned font in the cache: `fonts.py --fetch` (they skip without it)."""

import json
import sys
import tempfile
import unittest
from pathlib import Path

import numpy as np

sys.path.insert(0, str(Path(__file__).resolve().parent))

import paths  # noqa: E402,F401
import distances  # noqa: E402
import fonts  # noqa: E402
import ink  # noqa: E402
import render  # noqa: E402
import templates  # noqa: E402

FONT = (fonts.resolve("latin") or [None])[0]
needs_font = unittest.skipIf(FONT is None, "the pinned font is not fetched: run fonts.py --fetch")

LEX = frozenset({"mound", "round", "found", "ounce", "unit", "hound", "bound", "sound", "mount", "count"})


def word(text, x, y, w=60, h=20, wc=0.95, line=0):
    return {"text": text, "box": [x, y, w, h], "wc": wc, "line": line}


class SelectWordsTest(unittest.TestCase):
    def test_filters(self):
        ws = [word("mound", 0, 0), word("mo", 100, 0), word("m0und", 200, 0), word("mound", 300, 0, wc=0.5),
              word("zzzzz", 400, 0), word("Round", 500, 0), word("found", 600, 0, wc=None)]
        keep, c = templates.select_words(ws, LEX, [], fallback_quantile=None)
        self.assertEqual([w["text"] for w in keep], ["mound", "Round"])
        self.assertEqual(c["not_letters_3plus"], 2)
        self.assertEqual(c["low_wc"], 2)
        self.assertEqual(c["not_in_lexicon"], 1)

    def test_wc_fallback_on_a_page_where_wc_never_reaches_the_cut(self):
        ws = [word("mound", 20 * i, 0, wc=0.1 + 0.01 * i) for i in range(40)]
        keep, c = templates.select_words(ws, LEX, [], fallback_quantile=None)
        self.assertEqual((len(keep), c["wc_mode"]), (0, "absolute"))
        keep, c = templates.select_words(ws, LEX, [])
        self.assertEqual(c["wc_mode"], "page_quantile")
        self.assertEqual(len(keep), 10)  # the top quarter
        flat = [word("mound", 20 * i, 0, wc=0.87) for i in range(40)]  # a page with one constant WC: all tie, all kept
        self.assertEqual(len(templates.select_words(flat, LEX, [])[0]), 40)

    def test_no_harvested_word_lies_in_or_touches_a_gold_crop(self):
        crops = [[100, 100, 400, 300], [1000, 0, 1200, 50]]
        ws = [word("mound", x, y) for x in range(0, 1300, 40) for y in range(0, 400, 20)]
        keep, c = templates.select_words(ws, LEX, crops, fallback_quantile=None)
        self.assertTrue(keep)
        self.assertGreater(c["in_gold_crop"], 0)
        for w in keep:
            for cb in crops:
                self.assertFalse(templates.overlaps(w["box"], cb), (w["box"], cb))
        # a word straddling the crop's edge by one pixel is a leak too
        edge = word("mound", 341, 150, w=60)
        self.assertEqual(templates.select_words([edge], LEX, crops)[0], [])
        touching = word("mound", 400, 150, w=60)  # starts exactly at the crop's right edge: outside
        self.assertEqual(len(templates.select_words([touching], LEX, crops)[0]), 1)

    def test_crop_boxes_by_stem(self):
        m = [{"source_image": "a-1.jpg", "box": [1, 2, 3, 4]}, {"source_image": "a-1.jpg", "box": [5, 6, 7, 8]},
             {"source_image": None, "box": [0, 0, 1, 1]}]
        self.assertEqual(templates.crop_boxes_by_stem(m), {"a-1": [[1, 2, 3, 4], [5, 6, 7, 8]]})


class TemplateTest(unittest.TestCase):
    def test_median_template_is_the_pixel_median(self):
        a = np.zeros((templates.H, 6), np.uint8)
        a[10:20, 2:4] = 255
        noisy = a.copy()
        noisy[30:40, :] = 255
        t = templates.median_template([a, a, a, noisy, noisy.copy() * 0])
        self.assertTrue((t[10:20, 2:4] == 255).all())
        self.assertEqual(int(t[30:40].max()), 0)

    def test_cut_word_on_a_clean_gap(self):
        m = np.zeros((20, 60), bool)
        for x0 in (5, 25, 45):
            m[3:17, x0: x0 + 10] = True
        b, why = templates.cut_word(m, 5, 55, [0, 1 / 3, 2 / 3, 1])
        self.assertEqual(why, "ok")
        self.assertIn(b[1], range(15, 26))
        self.assertIn(b[2], range(35, 46))

    def test_cut_word_rejects_a_cut_through_solid_ink(self):
        m = np.zeros((20, 60), bool)
        m[3:17, 5:55] = True
        b, why = templates.cut_word(m, 5, 55, [0, 1 / 3, 2 / 3, 1])
        self.assertIsNone(b)
        self.assertEqual(why, "cut_through_ink")

    def test_cut_word_rejects_a_letter_too_wide(self):
        m = np.zeros((20, 60), bool)
        m[3:17, 5:12] = True
        m[3:17, 40:55] = True
        b, why = templates.cut_word(m, 5, 55, [0, 0.9, 1])  # the prior makes the last letter a tenth of the word
        self.assertIsNone(b)
        self.assertEqual(why, "letter_width")


def make_page(texts_at, size=(700, 260), px=60):
    """A white page with `texts_at` [(text, x, y_baseline)] drawn in the font; ALTO-style words and the page image."""
    from PIL import Image, ImageDraw

    im = Image.new("L", size, 255)
    d = ImageDraw.Draw(im)
    f = render._font(str(FONT.path), px)
    words = []
    for i, (t, x, base) in enumerate(texts_at):
        d.text((x, base), t, font=f, fill=0, anchor="ls")
        l, tp, r, b = f.getbbox(t, anchor="ls")
        words.append({"text": t, "box": [x + l - 3, base + tp - 3, r - l + 6, b - tp + 6], "wc": 0.97, "line": 0})
    return im, words


@needs_font
class FontBackedTest(unittest.TestCase):
    def setUp(self):
        self.texts = [("mound", 20 + 130 * (i % 5), 90 + 100 * (i // 5)) for i in range(10)]
        self.page, self.words = make_page(self.texts)
        for i, w in enumerate(self.words):
            w["line"] = i // 5

    def test_harvest_cuts_words_and_leaves_crop_words_out(self):
        crop = [0, 0, 700, 130]  # the first line is a gold crop
        h = templates.harvest_page("sn1-p1", self.page, self.words, LEX, [crop], FONT.path)
        self.assertEqual(h["stats"]["in_gold_crop"], 5)
        self.assertEqual(h["stats"]["trusted"], 5)
        self.assertGreaterEqual(h["stats"]["accepted"], 4)
        self.assertEqual(set(h["samples"]), set("mound"))
        self.assertTrue(all(len(v) == h["stats"]["accepted"] for v in h["samples"].values()))
        self.assertTrue(all(a.shape[0] == templates.H for v in h["samples"].values() for a in v))

    def test_store_and_renderer(self):
        h = templates.harvest_page("sn1-p1", self.page, self.words, LEX, [], FONT.path)
        store = templates.TemplateStore.from_harvest({"sn1-p1": h}, min_samples=5)
        self.assertEqual(set(store.pages["sn1-p1"]), set("mound"))
        self.assertEqual(store.meta["pages"]["sn1-p1"]["samples_per_letter"]["o"], h["stats"]["accepted"])
        with tempfile.TemporaryDirectory() as d:
            p = Path(d) / "t.npz"
            store.save(p)
            back = templates.TemplateStore.load(p)
        self.assertEqual(set(back.pages["sn1-p1"]), set("mound"))
        # a word with letters the page has (mound) and one it does not (z): z comes from the font
        bmp, meta = back.word_ink("zound", "sn1-p1", FONT.path)
        self.assertEqual((meta["tpl_page_letters"], meta["tpl_lccn_letters"], meta["tpl_font_letters"]), (4, 0, 1))
        self.assertEqual(bmp.shape[0] <= templates.H, True)
        # same contract as the font: render_for_crop gives masks, and distances take the renderer
        wb = back.renderer("sn1-p1", FONT.path)
        w = self.words[0]
        res = distances.word_distances("mound", self.page, w["box"], FONT.path, wb)
        self.assertEqual(res["status"], "ok")
        self.assertEqual(res["tpl_page_letters"], 5)
        font_res = distances.word_distances("mound", self.page, w["box"], FONT.path)
        self.assertNotIn("tpl_page_letters", font_res)
        # drawn from its own page's letters, the right word is nearer the scan than a wrong one
        wrong = distances.word_distances("wound", self.page, w["box"], FONT.path, wb)
        self.assertLess(res["chamfer"], wrong["chamfer"])

    def test_lccn_fallback(self):
        h = templates.harvest_page("sn1-p1", self.page, self.words, LEX, [], FONT.path)
        store = templates.TemplateStore.from_harvest({"sn1-p1": h, "sn1-p2": {"samples": {}, "gaps": [], "stats": {"trusted": 0}}},
                                                     min_samples=5)
        _, meta = store.word_ink("mound", "sn1-p2", FONT.path)
        self.assertEqual((meta["tpl_page_letters"], meta["tpl_lccn_letters"], meta["tpl_font_letters"]), (0, 5, 0))
        _, meta = store.word_ink("mound", "sn2-p9", FONT.path)
        self.assertEqual(meta["tpl_font_letters"], 5)

    def test_build_keeps_crop_words_out_of_the_templates(self):
        from PIL import Image

        crop = [0, 0, 700, 130]
        with tempfile.TemporaryDirectory() as d:
            d = Path(d)
            alto, st = d / "loc", d / "set"
            alto.mkdir()
            st.mkdir()
            self.page.save(alto / "sn1-p1.jpg")
            strings = "".join(f'<String CONTENT="{w["text"]}" HPOS="{w["box"][0]}" VPOS="{w["box"][1]}" WIDTH="{w["box"][2]}" '
                              f'HEIGHT="{w["box"][3]}" WC="0.97"/>' for w in self.words)
            (alto / "sn1-p1.xml").write_text(f'<alto xmlns="http://www.loc.gov/standards/alto/ns-v2#"><Layout><Page WIDTH="700" '
                                             f'HEIGHT="260"><PrintSpace><TextBlock><TextLine>{strings}</TextLine></TextBlock>'
                                             f'</PrintSpace></Page></Layout></alto>', encoding="utf-8")
            (st / "manifest.jsonl").write_text(json.dumps({"source_image": "sn1-p1.jpg", "box": crop}) + "\n", encoding="utf-8")
            lex = d / "lex.txt.zst"
            import subprocess

            lex_txt = d / "lex.txt"
            lex_txt.write_text("\n".join(sorted(LEX)) + "\n")
            try:
                subprocess.run(["zstd", "-q", str(lex_txt), "-o", str(lex)], check=True)
            except (OSError, subprocess.CalledProcessError):
                self.skipTest("zstd not available")
            store = templates.build(alto, st, d / "t.npz", None, 1, 0.9, 5, None, None, lexicon=lex)
            rec = store.meta["pages"]["sn1-p1"]
            self.assertEqual(rec["in_gold_crop"], 5)
            self.assertEqual(rec["trusted"], 5)
            self.assertTrue((d / "t.json").exists())


if __name__ == "__main__":
    unittest.main()
