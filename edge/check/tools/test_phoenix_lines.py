"""Tests for phoenix_lines.py: python3 -m unittest discover -s edge/check/tools. The image test needs Pillow."""
import collections
import os
import sys
import tempfile
import unittest

sys.path.insert(0, os.path.dirname(__file__))
import phoenix_lines as pl  # noqa: E402

NA, NAH, WA = "Ꮎ", "Ꮐ", "Ꮹ"
ME = "Ꮊ"  # Ꮊ


def w(text, box=(10, 10, 40, 20)):
    return {"text": text, "box": list(box)}


def line(loc, ours, lid="P1_TL00001"):
    return {"id": lid, "box": [0, 0, 400, 40], "loc_words": [w(t) for t in loc], "ours_words": ours, "page": "sn1/1828-03-06/ed-1/seq-1", "kind": "news"}


class Letters(unittest.TestCase):
    def test_only_syllabary_letters_stay(self):
        self.assertEqual(pl.letters("19. ᎣᏂᏃ, ab"), "ᎣᏂᏃ")
        self.assertEqual(pl.syllabary_words([w("12."), w("ᎠᎴ,")]), [{"text": "ᎠᎴ", "box": [10, 10, 40, 20], "raw": "ᎠᎴ,"}])

    def test_label_and_stem(self):
        self.assertEqual(pl.label("sn83020866/1828-07-23/ed-1/seq-4"), "Cherokee Phoenix, 23 July 1828, page 4")
        self.assertEqual(pl.stem("sn1/1828-03-06/ed-1/seq-4", "P4_TL00078"), "sn1_1828-03-06_ed-1_seq-4_P4_TL00078")


class Disputes(unittest.TestCase):
    def test_one_differing_word_is_kept(self):
        keep, why = pl.disputes([line(["ᎠᎴ", "ᎣᏂᏃ", "ᎡᎯ", "ᎤᏙᏓ"], ["ᎠᎴ", "ᎣᏂᏃ", "ᎡᎯ", "ᎤᏙᏗ"])])
        self.assertEqual(len(keep), 1)
        self.assertEqual(keep[0]["diff"], [3])
        self.assertEqual(why["kept"], 1)

    def test_other_lines_are_left_out_with_a_reason(self):
        lines = [
            line(["ᎠᎴ", "ᎣᏂᏃ", "ᎡᎯ"], ["ᎠᎴ", "ᎣᏂᏃ", "ᎡᎯ"]),  # same
            line(["ᎠᎴ", "ᎣᏂᏃ", "ᎡᎯ"], ["ᏩᎴ", "ᎣᏂᏗ", "ᎡᎯ"]),  # two words
            line(["ᎠᎴ", "ᎣᏂᏃ", "ᎡᎯ"], ["ᎠᎴ", "ᎣᏂᏃᎡᎯ"]),  # one reading joined two words
            line(["ᎠᎴ", "ᎣᏂᏃ"], ["ᎠᎴ", "ᎣᏂᏗ"]),  # short
            line(["ᎠᎴ", "ᎣᏂᏃ", "ᎡᎯ"], ["ᎠᎴ", "ᎣᏂᏃ", "ᏙᏓᏍᏗᏴ"]),  # nothing alike
            line([WA + "ᏍᎩ", "ᎣᏂᏃ", "ᎡᎯ"], [NA + "ᏍᎩ", "ᎣᏂᏃ", "ᎡᎯ"]),  # the G question
        ]
        keep, why = pl.disputes(lines)
        self.assertEqual(keep, [])
        self.assertEqual(dict(why), {"same": 1, "several_words": 1, "word_count_differs": 1, "short": 1, "unusable_pair": 1, "initial_g": 1})

    def test_specks_are_not_a_pair(self):
        self.assertFalse(pl.usable_pair("Ꭰ", "Ꭱ"))
        self.assertTrue(pl.usable_pair("ᎠᎴ", "ᎠᎵ"))

    def test_tiny_boxes_are_left_out(self):
        ln = line(["ᎠᎴ", "ᎣᏂᏃ", "ᎡᎯ"], ["ᎠᎴ", "ᎣᏂᏃ", "ᎡᏗ"])
        ln["loc_words"][2] = w("ᎡᎯ", (10, 10, 2, 20))
        self.assertEqual(pl.disputes([ln])[1]["tiny_box"], 1)


class Rare(unittest.TestCase):
    known = {"ᎺᎵᏍᎩᏂ", "ᎠᎴ"}

    def test_partners_count_single_letter_swaps(self):
        part = pl.partners([("ᎠᏯ" + ME, "ᎠᏯᎥ"), ("ᎠᎴ", "ᎠᎴ"), ("ᎠᏯ", "ᎠᏯ")])
        self.assertEqual(part[ME]["Ꭵ"], 1)

    def test_word_list_suggests_the_letter(self):
        ln = line(["ᎥᎵᏍᎩᏂ", "ᎠᎴ"], ["ᎥᎵᏍᎩᏂ", "ᎠᎴ"])
        out = pl.rare_candidates([ln], self.known, collections.Counter(), {})
        self.assertEqual([(c["kind"], c["letter"], c["a"], c["b"], c["b_is"]) for c in out], [("word-list", ME, "ᎥᎵᏍᎩᏂ", "ᎺᎵᏍᎩᏂ", "word_list")])

    def test_frequent_words_are_left_alone(self):
        ln = line(["ᎥᎵᏍᎩᏂ", "ᎠᎴ"], ["ᎥᎵᏍᎩᏂ", "ᎠᎴ"])
        self.assertEqual(pl.rare_candidates([ln], self.known, collections.Counter({"ᎥᎵᏍᎩᏂ": 99}), {}), [])

    def test_a_disagreement_with_the_letter(self):
        ln = line(["ᎠᏯ" + ME + "ᎴᎵ"], ["ᎠᏯᎥᎴᎵ"])
        out = pl.rare_candidates([ln], set(), collections.Counter(), {})
        self.assertEqual([(c["kind"], c["a_is"], c["b_is"]) for c in out], [("ours-loc", "loc", "chr_phoenix")])

    def test_both_read_the_letter_and_a_known_swap_exists(self):
        ln = line(["ᎠᏯ" + ME + "ᎴᎵ"], ["ᎠᏯ" + ME + "ᎴᎵ"])
        part = {ME: collections.Counter({"Ꭵ": 3})}
        out = pl.rare_candidates([ln], {"ᎠᏯᎥᎴᎵ"}, collections.Counter(), part)
        self.assertEqual([(c["kind"], c["b"], c["b_is"]) for c in out], [("agree", "ᎠᏯᎥᎴᎵ", "word_list")])

    def test_nah_candidates_need_a_na_spelling_in_the_word_list(self):
        ln = line([WA + "ᏍᎩ", "ᎠᎴ", "ᎦᎸ"], [NA + "ᏍᎩ", "ᎠᎴ", "ᎦᎸ"])
        out = pl.nah_candidates([ln], {NA + "ᏍᎩ"})
        self.assertEqual([(c["a"], c["b"], c["a_is"], c["b_is"], c["read_as"]) for c in out],
                         [(NA + "ᏍᎩ", NAH + "ᏍᎩ", "na", "nah", {"loc": WA + "ᏍᎩ", "chr_phoenix": NA + "ᏍᎩ"})])
        self.assertEqual(pl.nah_candidates([ln], set()), [])

    def test_spread_caps_per_letter_and_page(self):
        items = [{"line": {"page": f"p{i % 3}"}, "letter": ME, "a": "ᎠᎴ", "b": "ᎠᎵ", "k": i} for i in range(9)]
        got = pl.spread(items, 4, 2)
        self.assertEqual(len(got), 4)
        self.assertEqual(collections.Counter(g["line"]["page"] for g in got), {"p0": 2, "p1": 1, "p2": 1})


class Alto(unittest.TestCase):
    LOC = """<alto xmlns="http://www.loc.gov/standards/alto/ns-v2#"><Layout><Page WIDTH="4000" HEIGHT="8000"><PrintSpace><TextBlock>
<TextLine ID="P1_TL00001" HPOS="400" VPOS="800" WIDTH="2000" HEIGHT="160"><String HPOS="400" VPOS="800" WIDTH="800" HEIGHT="160" CONTENT="ᎠᎴ"/><SP/><String HPOS="1300" VPOS="800" WIDTH="900" HEIGHT="160" CONTENT="ᎣᏂᏃ,"/></TextLine>
<TextLine ID="P1_TL00002" HPOS="400" VPOS="1200" WIDTH="2000" HEIGHT="160"><String HPOS="400" VPOS="1200" WIDTH="800" HEIGHT="160" CONTENT="ᎡᎯ"/></TextLine>
</TextBlock></PrintSpace></Page></Layout></alto>"""
    PROD = """<alto xmlns="http://www.loc.gov/standards/alto/ns-v3#"><Layout><Page WIDTH="1000" HEIGHT="2000"><PrintSpace><TextBlock>
<TextLine ID="TL1" HPOS="100" VPOS="200" WIDTH="500" HEIGHT="40"><String HPOS="100" VPOS="200" WIDTH="500" HEIGHT="40" CONTENT="ᎠᎴ ᎣᏂᏗ,"/></TextLine>
<TextLine ID="TL2" HPOS="100" VPOS="300" WIDTH="500" HEIGHT="40"><String HPOS="100" VPOS="300" WIDTH="500" HEIGHT="40" CONTENT="ᎡᎯ"/></TextLine>
<TextLine ID="TL3" HPOS="900" VPOS="900" WIDTH="500" HEIGHT="40"><String HPOS="900" VPOS="900" WIDTH="500" HEIGHT="40" CONTENT="stray"/></TextLine>
</TextBlock></PrintSpace></Page></Layout></alto>"""

    def paths(self, d):
        out = []
        for name, xml in (("loc.xml", self.LOC), ("prod.xml", self.PROD)):
            p = os.path.join(d, name)
            with open(p, "w", encoding="utf-8") as f:
                f.write(xml)
            out.append(p)
        return out

    def test_lines_pair_by_position_not_number(self):
        with tempfile.TemporaryDirectory() as d:
            loc, prod = self.paths(d)
            lines, unmatched = pl.paired_lines(loc, prod)
        self.assertEqual([(ln["id"], [x["text"] for x in ln["loc_words"]], ln["ours_words"]) for ln in lines],
                         [("P1_TL00001", ["ᎠᎴ", "ᎣᏂᏃ,"], ["ᎠᎴ", "ᎣᏂᏗ,"]), ("P1_TL00002", ["ᎡᎯ"], ["ᎡᎯ"])])
        self.assertEqual(unmatched, 1)

    def test_cut_line_scales_boxes_into_the_crop(self):
        try:
            from PIL import Image
        except ImportError:
            self.skipTest("Pillow not installed")
        scan = Image.new("L", (2000, 4000), 255)  # half LoC's 4000-wide page
        img, boxes = pl.cut_line(scan, [400, 800, 2000, 160], [{"text": "x", "box": [1300, 800, 900, 160]}], 4000)
        pad = 12  # 15% of the 80 px line
        self.assertEqual(img.size, (1000 + 2 * pad, 80 + 2 * pad))
        self.assertEqual(boxes, [[650 - 200 + pad, pad, 450, 80]])


if __name__ == "__main__":
    unittest.main()
