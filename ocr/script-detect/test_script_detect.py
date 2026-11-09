"""Tests of the script-detection tools' pure parts (#1666). Run in the tools' image:

    podman run --rm -v "$PWD/ocr/script-detect:/work" usnm-script-detect python3 -m unittest -v test_script_detect
"""
import unittest

import script_detect as S

try:
    import numpy as np
    import sd_features as F
except ImportError:  # the measures need numpy and scipy
    np = F = None


class TokenKind(unittest.TestCase):
    def test_kinds(self):
        vocab = {"ᎠᎴ"}
        self.assertEqual(S.token_kind("ᎠᎴ,", vocab), "syl-attested")
        self.assertEqual(S.token_kind("ᏣᎳᎩ", vocab), "syl-other")
        self.assertEqual(S.token_kind("19.", vocab), "digits")
        self.assertEqual(S.token_kind("(", vocab), "punct")
        self.assertEqual(S.token_kind("John", vocab), "latin")


class Assign(unittest.TestCase):
    def test_each_tesseract_word_goes_to_the_most_overlapped_loc_word(self):
        loc = [{"box": [0, 0, 100, 10]}, {"box": [110, 0, 200, 10]}]
        tess = [{"text": "ab", "conf": 90.0, "box": [0, 0, 60, 10]},
                {"text": "cd", "conf": 50.0, "box": [60, 0, 100, 10]},
                {"text": "ef", "conf": 70.0, "box": [95, 0, 190, 10]}]
        got = S.assign(loc, tess)
        self.assertEqual(got[0][0], "abcd")
        self.assertAlmostEqual(got[0][1], (90 * 60 + 50 * 40) / 100)
        self.assertEqual(got[1], ("ef", 70.0))

    def test_a_word_nothing_landed_on(self):
        self.assertEqual(S.assign([{"box": [0, 0, 10, 10]}], []), [("", -1.0)])


class Bootstrap(unittest.TestCase):
    col = {"eng_english_long": 0, "eng_conf": 1, "eng_n_lower": 2, "block_syllabary": 3}

    def label(self, kind, loc, row, synth=None):
        return S.bootstrap_label({"kind": kind, "loc": loc, "synth": synth}, row, self.col)

    def test_rules(self):
        self.assertEqual(self.label("digits", "19.", [0, 0, 0, 1]), "o")
        self.assertEqual(self.label("syl-attested", "ᎠᎴ", [1, 99, 0, 1]), "s")  # eng reads it "DS"
        self.assertEqual(self.label("syl-other", "ᎦᏙᏃᏍ", [1, 90, 3, 1]), "l")
        self.assertEqual(self.label("syl-other", "ᎦᏙᏃᏍ", [0, 0, 0, 0.95]), "s")
        self.assertIsNone(self.label("syl-other", "ᎦᏙᏃᏍ", [0, 0, 0, 0.5]))
        self.assertEqual(self.label("syl-attested", "", [0, 0, 0, 1], synth="l"), "l")

    def test_the_model_never_sees_locs_reading(self):
        names = ["word_upper", "loc_attested", "left_loc_paren", "block_syllabary", "eng_conf", "prev_loc_syl"]
        self.assertEqual(S.model_columns(names), [0, 4])

    def test_dev_pages_are_a_stable_fifth(self):
        pages = [f"sn83020866/1828-03-{d:02d}/ed-1/seq-{s}" for d in range(1, 29) for s in range(1, 5)]
        dev = [p for p in pages if S.dev_page(p)]
        self.assertTrue(0.1 < len(dev) / len(pages) < 0.3)
        self.assertEqual(dev, [p for p in pages if S.dev_page(p)])


@unittest.skipIf(np is None, "numpy and scipy are in the tools' image")
class Measures(unittest.TestCase):
    def line(self, letter_rows):
        """A white line image 40 rows high with black bars as letters, each from `top` to row 30."""
        g = np.full((40, 200), 230, dtype=np.uint8)
        for i, top in enumerate(letter_rows):
            g[top:30, 10 + 20 * i:20 + 20 * i] = 20
        return g

    def test_body_and_short_letters(self):
        caps = self.line([10] * 8)  # every letter at full height, like the syllabary
        ink = F.binarize(caps)
        top, bot = F.body(ink)
        self.assertLessEqual(abs(top - 10), 1)  # the 3-row smoothing may widen the body by a row
        self.assertLessEqual(abs(bot - 30), 1)
        self.assertEqual(F.shape(ink, top, bot)["short"], 0.0)
        mixed = self.line([10, 18, 18, 18, 10, 18, 18, 18])  # x-height letters with two ascenders
        ink = F.binarize(mixed)
        top, bot = F.body(ink)
        sh = F.shape(ink, top, bot)
        self.assertLessEqual(abs(top - 18), 1)
        self.assertLessEqual(abs(bot - 30), 1)
        self.assertGreater(sh["tall"], 0.2)

    def test_quality_falls_with_contrast(self):
        sharp = self.line([10] * 8)
        faded = (sharp.astype(float) * 0.2 + 150).astype(np.uint8)
        q1 = F.quality(sharp, F.binarize(sharp), 20)
        q2 = F.quality(faded, F.binarize(faded), 20)
        self.assertGreater(q1["contrast"], q2["contrast"])
        self.assertGreater(q1["separability"], 0.5)

    def test_latin_plausibility(self):
        words = {"widow", "window", "willow", "academy", "cherokee", "clay", "ton", "the", "and"}
        self.assertGreater(F.latin_plausibility("widow", words), F.latin_plausibility("GCWY", words))
        self.assertEqual(F.latin_plausibility("19.", words), 0.0)

    def test_word_crop_is_tight_and_scaled(self):
        import sd_cnn
        g = np.full((100, 300), 230, dtype=np.uint8)
        for x in range(50, 90, 8):  # a word of five letters, 40 wide and 20 high, in a looser box
            g[40:60, x:x + 5] = 10
        c = sd_cnn.word_crop(g, [30, 20, 120, 80])
        self.assertEqual(c.shape, (sd_cnn.CROP_H, sd_cnn.CROP_W))
        self.assertGreater(c[:, :4].mean(), 0.9)  # cut tight: the first letter starts at the left edge
        self.assertGreater(c[:, :62].max(axis=0).sum(), 30)  # 37x20 scaled to about 59x32
        self.assertLess(c[:, 64:].max(), 0.1)

    def test_cnn_context_columns(self):
        P = np.array([[0.9, 0.0, 0.1], [0.2, 0.0, 0.8], [0.1, 0.1, 0.8]], dtype=np.float32)
        meta = [{"line": "a"}, {"line": "a"}, {"line": "b"}]
        c = S.cnn_columns(P, meta)
        self.assertEqual(c.shape, (3, len(S.CNN_NAMES)))
        self.assertAlmostEqual(c[0, S.CNN_NAMES.index("right_cnn_l")], 0.2)
        self.assertEqual(c[0, S.CNN_NAMES.index("left_cnn_l")], -1)
        self.assertAlmostEqual(c[1, S.CNN_NAMES.index("line_cnn_l")], 0.55)
        self.assertEqual(c[2, S.CNN_NAMES.index("left_cnn_l")], -1)

    def test_a_blank_box_has_no_ink(self):
        self.assertFalse(F.binarize(np.full((10, 10), 200, dtype=np.uint8)).any())

    def test_names_match_rows(self):
        self.assertEqual(len(F.NAMES), len(set(F.NAMES)))


if __name__ == "__main__":
    unittest.main()
