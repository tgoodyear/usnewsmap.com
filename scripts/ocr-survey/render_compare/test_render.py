"""render_compare/render.py, ink.py, fonts.py: rendering a word into its box (#2103, step 2).

The font tests need the pinned font in the cache: `fonts.py --fetch` (they skip without it)."""

import json
import sys
import tempfile
import unittest
from pathlib import Path

import numpy as np

sys.path.insert(0, str(Path(__file__).resolve().parent))

import paths  # noqa: E402,F401
import fonts  # noqa: E402
import ink  # noqa: E402
import render  # noqa: E402
import render_preview  # noqa: E402
import loader  # noqa: E402
import synthetic  # noqa: E402

FONT = (fonts.resolve("latin") or [None])[0]
needs_font = unittest.skipIf(FONT is None, "the pinned font is not fetched: run fonts.py --fetch")


class InkTest(unittest.TestCase):
    def test_stroke_width_of_bars(self):
        for w in (2, 3, 5, 7):
            m = np.zeros((40, 80), bool)
            m[:, 10:10 + w] = True
            m[:, 40:40 + w] = True
            self.assertAlmostEqual(ink.stroke_width(m), w, delta=1.0, msg=w)
        self.assertIsNone(ink.stroke_width(np.zeros((5, 5), bool)))

    def test_binarise_finds_dark_strokes_and_not_a_flat_crop(self):
        g = np.full((30, 60), 230, np.uint8)
        g[5:25, 20:24] = 20
        m = ink.binarise(g)
        self.assertTrue(m[10, 21])
        self.assertFalse(m[10, 40])
        self.assertFalse(ink.binarise(np.full((30, 60), 128, np.uint8)).any())

    def test_ink_rows_ignore_a_speck(self):
        m = np.zeros((40, 60), bool)
        m[10:30, 10:50] = True
        m[2, 5] = True
        self.assertEqual(ink.ink_rows(m), (10, 30))
        self.assertIsNone(ink.ink_rows(np.zeros((4, 4), bool)))

    def test_ink_cols_and_specks(self):
        m = np.zeros((40, 60), bool)
        m[10:30, 10:50] = True
        m[2, 55] = True
        self.assertEqual(ink.ink_cols(m), (10, 56))  # a speck is a column of ink
        clean = ink.drop_specks(m)
        self.assertEqual(ink.ink_cols(clean), (10, 50))
        self.assertTrue(clean[15, 20])


class CropFrameTest(unittest.TestCase):
    def test_frame_is_white_outside_the_page(self):
        from PIL import Image

        page = Image.new("L", (100, 50), 0)
        frame, pad, (bx, by, bw, bh) = render.crop_frame(page, [0, 0, 20, 10])
        self.assertEqual((bx, by, bw, bh), (pad, pad, 20, 10))
        self.assertEqual(frame.shape, (10 + 2 * pad, 20 + 2 * pad))
        self.assertEqual(frame[0, 0], 255)  # above and left of the page
        self.assertEqual(frame[pad, pad], 0)  # the page's own pixel


@needs_font
class FontTest(unittest.TestCase):
    def test_record_names_the_font_and_hashes_the_file(self):
        rec = FONT.record()
        self.assertEqual(rec["font_family"], "Old Standard TT")
        self.assertEqual(rec["font_licence"], "OFL-1.1")
        self.assertRegex(rec["font_version"], r"\d")
        self.assertEqual(rec["font_sha256"], fonts.LOCK["latin"]["files"][0]["sha256"])
        self.assertEqual(rec["font_package"], "@fontsource/old-standard-tt@5.3.0")

    def test_cherokee_is_skipped_without_the_font(self):
        self.assertEqual(fonts.resolve("chr"), [])
        self.assertIn("not in the repository", fonts.unavailable_reason("chr"))
        self.assertEqual(fonts.script_of_lang("chr"), "chr")
        self.assertEqual(fonts.script_of_lang("eng"), "latin")

    def test_glyph_coverage(self):
        self.assertTrue(render.has_glyphs("bad Clare-mont", FONT.path))
        self.assertFalse(render.has_glyphs("Ꭰ", FONT.path))  # a Cherokee letter, not in the Latin font


@needs_font
class RenderTest(unittest.TestCase):
    def test_ink_fills_the_requested_spans(self):
        mask, meta = render.render_word("hill", (60, 120), (10, 100), (12, 44), FONT.path, None)
        ys, xs = np.nonzero(mask)
        self.assertEqual((xs.min(), xs.max() + 1), (10, 100))
        self.assertEqual((ys.min(), ys.max() + 1), (12, 44))
        self.assertTrue(meta["drawn"])

    def test_stroke_width_is_matched(self):
        widths = []
        for target in (2.0, 3.0, 4.0):
            mask, meta = render.render_word("hill", (40, 90), (10, 80), (10, 30), FONT.path, target)
            widths.append(ink.stroke_width(mask))
            self.assertAlmostEqual(widths[-1], target, delta=0.8, msg=target)
        self.assertEqual(widths, sorted(widths))

    def test_blank_text_draws_nothing(self):
        mask, meta = render.render_word("", (20, 40), (2, 38), (2, 18), FONT.path, 2.0)
        self.assertFalse(mask.any())
        self.assertFalse(meta["drawn"])

    def test_render_for_crop_takes_the_scan_height(self):
        from PIL import Image

        with tempfile.TemporaryDirectory() as d:
            fx = synthetic.make_set(Path(d))
            page = Image.open(fx["alto_dir"] / "page1.jpg").convert("L")
            rows, _ = loader.load_set(fx["set_dir"], fx["alto_dir"])
            r = rows[0]  # "the"
            out = render.render_for_crop(r["ocr_word"], page, r["box"], FONT.path)
        top, bottom = out["meta"]["ink_rows"]
        scan_rows = ink.ink_rows(out["scan_mask"])
        self.assertLessEqual(abs(top - scan_rows[0]), 1)
        self.assertLessEqual(abs(bottom - scan_rows[1]), 1)
        ys, _ = np.nonzero(out["render_mask"])
        self.assertLessEqual(abs(ys.min() - top), 3)  # a thicker stroke can grow the ink a little
        left, right = out["meta"]["ink_cols"]
        cols = ink.ink_cols(out["scan_mask"])
        self.assertLessEqual(abs(left - cols[0]) + abs(right - cols[1]), 2)
        self.assertEqual(out["render_mask"].shape, out["scan"].shape)

    def test_preview_sheet(self):
        with tempfile.TemporaryDirectory() as d:
            fx = synthetic.make_set(Path(d))
            words = Path(d) / "words.jsonl"
            rows, _ = loader.load_set(fx["set_dir"], fx["alto_dir"])
            words.write_text("".join(json.dumps(r) + "\n" for r in rows), encoding="utf-8")
            out = Path(d) / "sheet.png"
            rc = render_preview.main(["--words", str(words), "--alto-dir", str(fx["alto_dir"]), "--out", str(out),
                                      "--limit", "6", "--wrong-first"])
            self.assertEqual(rc, 0)
            self.assertGreater(out.stat().st_size, 500)


if __name__ == "__main__":
    unittest.main()
