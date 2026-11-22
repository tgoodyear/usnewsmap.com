"""render_compare/sanity.py: the synthetic look-alike swap check (#2103, step 5). A sanity check only.

Needs the pinned font in the cache: `fonts.py --fetch` (the tests skip without it)."""

import json
import random
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import paths  # noqa: E402,F401
import fonts  # noqa: E402
import gold  # noqa: E402
import loader  # noqa: E402
import sanity  # noqa: E402
import synthetic  # noqa: E402

FONT = (fonts.resolve("latin") or [None])[0]
needs_font = unittest.skipIf(FONT is None, "the pinned font is not fetched: run fonts.py --fetch")

WORDS = ["hunt", "bill", "name", "union", "child", "lunch", "nine", "mouse", "house", "bible", "hello", "plain",
         "fine", "ceded", "honey", "unit", "chain", "hill", "bound", "found", "mound", "nice", "loud", "poll"]
LINES = [[(w, w, 0.95) for w in WORDS[i:i + 4]] for i in range(0, len(WORDS), 4)]


@needs_font
class PairsTest(unittest.TestCase):
    def test_pairs_come_from_the_lookalike_measure(self):
        pairs = sanity.lookalike_pairs(FONT.path, 0.6)
        got = {(a, b) for a, b, _ in pairs}
        for p in (("n", "u"), ("h", "n"), ("i", "l"), ("b", "h"), ("c", "e")):
            self.assertIn(p, got)
        self.assertNotIn(("a", "z"), got)
        self.assertEqual([v for *_, v in pairs], sorted((v for *_, v in pairs), reverse=True))
        self.assertTrue(all(v >= 0.6 for *_, v in pairs))

    def test_swap_changes_exactly_one_letter_to_a_pair_partner(self):
        pairs = [("n", "u", 0.84), ("h", "n", 0.78)]
        rng = random.Random(1)
        for word in ("hunt", "name", "union"):
            new, (i, c, other, ov) = sanity.swap_word(word, pairs, rng)
            self.assertEqual(len(new), len(word))
            self.assertEqual([k for k in range(len(word)) if word[k] != new[k]], [i])
            self.assertEqual((word[i], new[i]), (c, other))
            self.assertIn((c, other) if (c, other, ov) in pairs else (other, c), {(a, b) for a, b, _ in pairs} | {(b, a) for a, b, _ in pairs})
        self.assertIsNone(sanity.swap_word("to", pairs, rng))  # too short
        self.assertIsNone(sanity.swap_word("Hunt", pairs, rng))  # not lower-case
        self.assertIsNone(sanity.swap_word("zzzz", pairs, rng))  # no letter in a pair


@needs_font
class RunTest(unittest.TestCase):
    def build(self, root, scan_font):
        fx = synthetic.make_set(root, lines=LINES, page_size=(900, 20 + 60 * len(LINES) + 20), scan_font=scan_font)
        rows, _ = loader.load_set(fx["set_dir"], fx["alto_dir"])
        words = root / "words.jsonl"
        gold.write_jsonl(words, rows)
        return fx, rows, words

    def test_distances_rise_when_a_lookalike_is_swapped(self):
        with tempfile.TemporaryDirectory() as d:
            root = Path(d)
            fx, rows, words = self.build(root, FONT.path)
            self.assertTrue(all(r["label"] == 1 for r in rows))
            out, swaps = root / "s.json", root / "s.jsonl"
            rc = sanity.main(["--words", str(words), "--alto-dir", str(fx["alto_dir"]), "--out", str(out), "--rows-out", str(swaps)])
            self.assertEqual(rc, 0)
            rep = json.loads(out.read_text())
            n = len(gold.read_jsonl(swaps))
        self.assertTrue(rep["sanity_check_only"])
        self.assertEqual(rep["font"]["font_family"], "Old Standard TT")
        self.assertGreaterEqual(rep["swapped_words"], 15)
        self.assertEqual(rep["swapped_words"], n)
        for m in ("chamfer", "pixel", "hog_cos"):
            e = rep["all"][m]
            self.assertGreater(e["mean_swapped"], e["mean_original"], m)
            self.assertGreater(e["mean_rise"], 0, m)
            self.assertGreaterEqual(e["share_rose"], 0.7, m)
            self.assertGreater(e["auc_swapped_vs_original"], 0.6, m)
            self.assertLess(e["wilcoxon_p_rise"], 0.01, m)

    def test_runs_when_the_page_is_in_another_face_and_reports_it_apart(self):
        with tempfile.TemporaryDirectory() as d:
            root = Path(d)
            fx, rows, words = self.build(root, None)  # a sans page: no assertion on size, only that it runs
            out = root / "s.json"
            self.assertEqual(sanity.main(["--words", str(words), "--alto-dir", str(fx["alto_dir"]), "--out", str(out)]), 0)
            rep = json.loads(out.read_text())
        self.assertIn("overlap_ge_0.7", rep)
        self.assertIn("overlap_lt_0.7", rep)
        self.assertGreater(len(rep["pair_list"]), 5)

    def test_only_right_words_are_swapped(self):
        with tempfile.TemporaryDirectory() as d:
            root = Path(d)
            fx, rows, words = self.build(root, FONT.path)
            rows[0]["label"] = 0  # a wrong word is not swapped
            gold.write_jsonl(words, rows)
            swaps = root / "s.jsonl"
            sanity.main(["--words", str(words), "--alto-dir", str(fx["alto_dir"]), "--out", str(root / "s.json"), "--rows-out", str(swaps)])
            keys = {(r["crop_id"], r["word_index"]) for r in gold.read_jsonl(swaps)}
        self.assertNotIn((rows[0]["crop_id"], rows[0]["word_index"]), keys)


if __name__ == "__main__":
    unittest.main()
