"""render_compare/loader.py: per-word labels from a gold set and LoC's ALTO (#2103, step 1)."""

import random
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import paths  # noqa: E402,F401
import gold  # noqa: E402
import loader  # noqa: E402
import synthetic  # noqa: E402


class AlignTest(unittest.TestCase):
    def test_edit_count_is_golds(self):
        rng = random.Random(2103)
        vocab = ["a", "bad", "had", "the", "tbe", "hill", "dog", "Clare-", "mont", "x"]
        for _ in range(200):
            t = [rng.choice(vocab) for _ in range(rng.randint(0, 9))]
            o = [rng.choice(vocab) for _ in range(rng.randint(0, 9))]
            ops = loader.align_words(t, o)
            self.assertEqual(loader.word_edits(ops), gold.edit_distance(t, o), (t, o))
            self.assertEqual([i for _, i, _ in ops if i is not None], list(range(len(t))))
            self.assertEqual([j for _, _, j in ops if j is not None], list(range(len(o))))

    def test_substitution_pairs_the_similar_word(self):
        # one substitution and one insertion: "ovcr" belongs with "over", not with "the"
        ops = loader.align_words(["over", "the"], ["ovcr", "extra", "the"])
        self.assertEqual(ops, [("sub", 0, 0), ("ins", None, 1), ("match", 1, 2)])

    def test_truth_tokens_keep_line_end_hyphen(self):
        self.assertEqual(loader.truth_tokens("Clare-\nmont [?fa]rm [?] x"), ["Clare-", "mont", "farm", "x"])


class LicenceTest(unittest.TestCase):
    def test_licence_allows(self):
        for ok in ("CC BY-NC-SA 4.0", "CC0-1.0", "CC-BY-4.0 (Zenodo record)", "Public Domain Mark 1.0"):
            self.assertTrue(loader.licence_allows(ok), ok)
        for no in ("CC BY-NC-ND 4.0", None, "", "all rights reserved", "apache-2.0"):
            self.assertFalse(loader.licence_allows(no), no)


class LoadSetTest(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.fx = synthetic.make_set(Path(self.tmp.name))

    def load(self):
        return loader.load_set(self.fx["set_dir"], self.fx["alto_dir"])

    def test_rows_and_labels(self):
        rows, summaries = self.load()
        self.assertEqual([r["ocr_word"] for r in rows],
                         ["the", "had", "dog", "ran", "ovcr", "tbe", "hill", "Clare-", "mont", "farm"])
        by = {r["ocr_word"]: r for r in rows}
        self.assertEqual({r["ocr_word"] for r in rows if r["label"] == 0}, {"had", "ovcr", "tbe"})
        self.assertEqual(by["had"]["truth_word"], "bad")
        self.assertEqual(by["had"]["op"], "sub")
        self.assertEqual(by["the"]["label"], 1)
        self.assertEqual(by["the"]["op"], "match")
        self.assertEqual(by["Clare-"]["label"], 1)  # the hyphen stays on the word, as in the truth's line end
        self.assertEqual(summaries[0]["word_edits"], 3)
        self.assertEqual(summaries[0]["del"], 0)

    def test_real_word_error_flag(self):
        rows, _ = self.load()
        # "had" is in the English damage-words list; "ovcr" and "tbe" are not; right words never carry the flag
        self.assertEqual([r["ocr_word"] for r in rows if r["real_word_error"]], ["had"])

    def test_boxes_are_image_pixels_and_wc_is_kept(self):
        rows, _ = self.load()
        first = rows[0]
        self.assertEqual(first["box"][:2], [30.0, 20.0])  # ALTO units are 2 per image pixel
        self.assertEqual(first["wc"], 0.97)
        self.assertEqual(first["source_license"], synthetic.LICENCE)
        self.assertEqual({r["page_id"] for r in rows}, {"synthetic-page-1"})
        self.assertIn("page_damage", first)

    def test_box_restricts_the_words(self):
        m = gold.read_jsonl(self.fx["set_dir"] / "manifest.jsonl")
        m[0]["box"] = [0, 0, 800, 70]  # the first line only
        gold.write_jsonl(self.fx["set_dir"] / "manifest.jsonl", m)
        rows, summaries = self.load()
        self.assertEqual(len(rows), 4)
        self.assertEqual(summaries[0]["del"], 6)  # the other lines' truth words have no OCR word in the box

    def test_extra_ocr_word_is_an_insertion(self):
        lines = [[("a", "a", 0.9), ("b", "b", 0.9)], [("c", "c", 0.9)]]
        with tempfile.TemporaryDirectory() as d:
            fx = synthetic.make_set(Path(d), lines=lines)
            x = (fx["alto_dir"] / "page1.xml").read_text()
            extra = '<String ID="n" CONTENT="~" HPOS="400" VPOS="40" WIDTH="30" HEIGHT="30" WC="0.1"/>'
            (fx["alto_dir"] / "page1.xml").write_text(x.replace("</TextLine>", extra + "</TextLine>", 1))
            rows, _ = loader.load_set(fx["set_dir"], fx["alto_dir"])
        noise = next(r for r in rows if r["ocr_word"] == "~")
        self.assertEqual((noise["op"], noise["label"], noise["truth_word"], noise["real_word_error"]), ("ins", 0, None, False))

    def test_refuses_unusable_sets(self):
        for licence, scoring in (("CC BY-NC-ND 4.0", True), ("CC BY 4.0", False)):
            with tempfile.TemporaryDirectory() as d:
                fx = synthetic.make_set(Path(d), licence=licence, for_scoring=scoring)
                with self.assertRaises(SystemExit):
                    loader.load_set(fx["set_dir"], fx["alto_dir"])
        with tempfile.TemporaryDirectory() as d:
            fx = synthetic.make_set(Path(d))
            (fx["set_dir"] / "LICENSES" / "LICENSE").unlink()
            with self.assertRaises(SystemExit):
                loader.load_set(fx["set_dir"], fx["alto_dir"])

    def test_cli_writes_jsonl(self):
        out = Path(self.tmp.name) / "words.jsonl"
        rc = loader.main(["--set-dir", str(self.fx["set_dir"]), "--alto-dir", str(self.fx["alto_dir"]), "--out", str(out)])
        self.assertEqual(rc, 0)
        self.assertEqual(len(gold.read_jsonl(out)), 10)


if __name__ == "__main__":
    unittest.main()
