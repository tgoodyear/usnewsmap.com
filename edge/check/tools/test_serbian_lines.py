"""Tests for serbian_lines.py: python3 -m unittest discover -s edge/check/tools."""
import json
import os
import sys
import tempfile
import unittest

sys.path.insert(0, os.path.dirname(__file__))
import build_tasks as bt  # noqa: E402
import serbian_lines as sl  # noqa: E402

HERE = os.path.dirname(__file__)


def alto(width, height, lines):
    """A minimal ALTO page: lines of (box, [(word, box), ...])."""
    out = [f'<alto xmlns="http://www.loc.gov/standards/alto/ns-v2#"><Layout><Page WIDTH="{width}" HEIGHT="{height}">'
           "<PrintSpace><TextBlock>"]
    for (x, y, w, h), words in lines:
        out.append(f'<TextLine HPOS="{x}" VPOS="{y}" WIDTH="{w}" HEIGHT="{h}">')
        for text, (wx, wy, ww, wh) in words:
            out.append(f'<String CONTENT="{text}" HPOS="{wx}" VPOS="{wy}" WIDTH="{ww}" HEIGHT="{wh}" WC="0.9"/>')
        out.append("</TextLine>")
    out.append("</TextBlock></PrintSpace></Page></Layout></alto>")
    return "".join(out)


class Lines(unittest.TestCase):
    def test_cyrillic_share_counts_letters_only(self):
        self.assertEqual(sl.cyrillic_share("Бр. 22"), 1.0)
        self.assertEqual(sl.cyrillic_share("CTPAHA 4"), 0.0)
        self.assertAlmostEqual(sl.cyrillic_share("Па. nog"), 2 / 5)
        self.assertEqual(sl.cyrillic_share("12"), 0.0)

    def test_candidates_need_words_and_cyrillic(self):
        lines = [{"words": [{"text": t} for t in words]} for words in
                 (["седница", "одржати", "у"], ["the", "Serbian", "club"], ["Бр.", "22"])]
        self.assertEqual(sl.candidates(lines, 3), [0])

    def test_words_in_takes_centres_inside_left_to_right(self):
        words = [{"text": "б", "box": [50, 10, 20, 10]}, {"text": "а", "box": [0, 10, 20, 10]},
                 {"text": "below", "box": [0, 40, 20, 10]}, {"text": "edge", "box": [95, 10, 20, 10]}]
        self.assertEqual([w["text"] for w in sl.words_in([0, 5, 100, 20], words)], ["а", "б"])

    def test_pick_is_the_same_for_a_page_and_differs_by_seed(self):
        pool = list(range(40))
        first = sl.pick(pool, 4, "sn1_1906-01-01_ed-1_seq-1", 1)
        self.assertEqual(first, sl.pick(pool, 4, "sn1_1906-01-01_ed-1_seq-1", 1))
        self.assertEqual(len(first), 4)
        self.assertNotEqual(first, sl.pick(pool, 4, "sn1_1906-01-01_ed-1_seq-1", 2))
        self.assertEqual(sl.pick([3, 1], 4, "x", 1), [1, 3])

    def test_our_words_scale_to_loc_units(self):
        with tempfile.TemporaryDirectory() as tmp:
            ours = os.path.join(tmp, "ours.xml")
            with open(ours, "w", encoding="utf-8") as f:
                f.write(alto(2000, 4000, [((0, 0, 400, 40), [("Стубенвил", (100, 20, 200, 40))])]))
            ow, oh, lines = sl.alto_lines(ours)
            words = sl.scaled_words(lines, 1000 / ow, 2000 / oh)
        self.assertEqual(words, [{"text": "Стубенвил", "box": [50.0, 10.0, 100.0, 20.0]}])


class Config(unittest.TestCase):
    def test_latin_lookalikes_compare_equal_to_cyrillic(self):
        with open(os.path.join(HERE, "..", "languages", "sr.json"), encoding="utf-8") as f:
            config = json.load(f)
        eq = {k: list(v) for k, v in config["equivalents"].items()}
        for k, v in config["equivalents"].items():
            self.assertEqual(len(v), 1, k)
            self.assertTrue(sl.CYRILLIC.match(v), k)
        self.assertEqual(bt.letter_diff("CTPAHA", "СТРАНА", eq), ([], []))
        self.assertEqual(bt.letter_diff("Стубснвнл", "Стубенвил", eq), ([4, 7], [4, 7]))


if __name__ == "__main__":
    unittest.main()
