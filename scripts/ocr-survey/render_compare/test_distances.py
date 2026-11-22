"""render_compare/distances.py: chamfer, pixel and HOG distances of a rendering from its scan crop (#2103, step 3).

Needs the pinned font in the cache for the end-to-end tests: `fonts.py --fetch` (they skip without it)."""

import json
import statistics
import sys
import tempfile
import unittest
from pathlib import Path

import numpy as np

sys.path.insert(0, str(Path(__file__).resolve().parent))

import paths  # noqa: E402,F401
import distances  # noqa: E402
import fonts  # noqa: E402
import gold  # noqa: E402
import loader  # noqa: E402
import synthetic  # noqa: E402

FONT = (fonts.resolve("latin") or [None])[0]
needs_font = unittest.skipIf(FONT is None, "the pinned font is not fetched: run fonts.py --fetch")


def letter_masks():
    a = np.zeros((40, 80), bool)
    a[8:32, 10:14] = True
    a[8:12, 10:40] = True
    a[20:24, 10:35] = True
    return a


class PrimitivesTest(unittest.TestCase):
    def test_same_shape_has_zero_distance(self):
        a = letter_masks()
        moved, dx, dy, c = distances.align(a, a.copy(), 4)
        self.assertEqual((dx, dy, c), (0, 0, 0.0))
        self.assertAlmostEqual(distances.hog_cosine(a, a), 0.0, places=5)

    def test_alignment_recovers_a_shift(self):
        a = letter_masks()
        b = distances._shifted(a, 3, -2, False)
        moved, dx, dy, c = distances.align(a, b, 5)
        self.assertEqual((dx, dy), (-3, 2))
        self.assertEqual(c, 0.0)
        self.assertTrue((moved == a).all())

    def test_a_different_shape_is_further(self):
        a = letter_masks()
        other = np.zeros_like(a)
        other[8:32, 50:54] = True
        other[8:12, 50:75] = True
        other[28:32, 50:75] = True
        near = distances.align(a, distances._shifted(a, 1, 0, False), 4)[3]
        far = distances.align(a, other, 4)[3]
        self.assertGreater(far, near)
        self.assertGreater(distances.hog_cosine(a, other), 0.05)

    def test_empty_masks(self):
        z = np.zeros((40, 80), bool)
        self.assertEqual(distances.hog_cosine(z, z), 0.0)
        self.assertEqual(distances.hog_cosine(z, letter_masks()), 1.0)


@needs_font
class WordTest(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.fx = synthetic.make_set(Path(self.tmp.name))
        self.rows, _ = loader.load_set(self.fx["set_dir"], self.fx["alto_dir"])
        self.words = Path(self.tmp.name) / "words.jsonl"
        gold.write_jsonl(self.words, self.rows)

    def run_main(self, out, *extra):
        return distances.main(["--words", str(self.words), "--alto-dir", str(self.fx["alto_dir"]), "--out", str(out), *extra])

    def test_jsonl_rows_carry_distances_and_the_font(self):
        out = Path(self.tmp.name) / "d.jsonl"
        self.assertEqual(self.run_main(out), 0)
        rows = gold.read_jsonl(out)
        self.assertEqual(len(rows), 10)
        self.assertEqual({r["status"] for r in rows}, {"ok"})
        for r in rows:
            for k in ("chamfer", "pixel", "hog_cos"):
                self.assertGreaterEqual(r[k], 0.0, k)
            self.assertEqual(r["font_family"], "Old Standard TT")
            self.assertEqual(r["font_sha256"], FONT.sha256)
        self.assertEqual(rows[0]["ocr_word"], "the")  # the loader's columns are kept
        self.assertEqual(rows[0]["label"], 1)

    def test_wrong_words_are_further_when_the_page_is_in_the_same_face(self):
        # Printed in the rendering font, a correct word matches its rendering closely. (With the page in another face
        # single words are noisy: that is the step-4 question, not this test's.)
        with tempfile.TemporaryDirectory() as d:
            fx = synthetic.make_set(Path(d), scan_font=FONT.path)
            rows, _ = loader.load_set(fx["set_dir"], fx["alto_dir"])
            words = Path(d) / "w.jsonl"
            gold.write_jsonl(words, rows)
            out = Path(d) / "d.jsonl"
            distances.main(["--words", str(words), "--alto-dir", str(fx["alto_dir"]), "--out", str(out)])
            res = gold.read_jsonl(out)
        right = [r for r in res if r["label"] == 1]
        wrong = [r for r in res if r["label"] == 0]
        self.assertEqual((len(right), len(wrong)), (7, 3))
        for k in ("chamfer", "pixel", "hog_cos"):
            self.assertGreater(statistics.mean(r[k] for r in wrong), statistics.mean(r[k] for r in right), k)

    def test_parquet_output(self):
        import pyarrow.parquet as pq

        out = Path(self.tmp.name) / "d.parquet"
        self.assertEqual(self.run_main(out), 0)
        t = pq.read_table(out)
        self.assertEqual(t.num_rows, 10)
        self.assertIn("hog_cos", t.column_names)
        self.assertIn("font_sha256", t.column_names)

    def test_cherokee_rows_are_skipped_not_drawn_in_a_latin_font(self):
        rows = [dict(self.rows[0], lang="chr", ocr_word="ᎠᎡ")]
        gold.write_jsonl(self.words, rows)
        out = Path(self.tmp.name) / "d.jsonl"
        self.run_main(out)
        r = gold.read_jsonl(out)[0]
        self.assertEqual(r["status"], "no_font")
        self.assertIsNone(r.get("chamfer"))

    def test_a_word_the_font_cannot_draw_is_kept_with_a_status(self):
        gold.write_jsonl(self.words, [dict(self.rows[0], ocr_word="Ꭰ")])
        out = Path(self.tmp.name) / "d.jsonl"
        self.run_main(out)
        self.assertEqual(gold.read_jsonl(out)[0]["status"], "no_glyph")


if __name__ == "__main__":
    unittest.main()
