"""Tests for build_tasks.py: python3 -m unittest discover -s edge/check/tools. The clip test needs Pillow."""
import json
import os
import subprocess
import sys
import tempfile
import unittest

sys.path.insert(0, os.path.dirname(__file__))
import build_tasks as bt  # noqa: E402

YI = bt.parse_equivalents(["װ=וו", "ײ=יי"])


class Punctuation(unittest.TestCase):
    def test_letters_only_drops_punctuation_and_folds(self):
        eq = bt.parse_equivalents(["C=С"])
        self.assertEqual(bt.letters_only('"помоћник,', eq), "помоћник")
        self.assertEqual(bt.letters_only("Cе.", eq), bt.letters_only("Се", eq))
        self.assertNotEqual(bt.letters_only("себс", eq), bt.letters_only("себе.", eq))


class Letters(unittest.TestCase):
    def test_marks_stay_with_their_letter(self):
        self.assertEqual(bt.graphemes("פּאַ"), ["פּ", "אַ"])

    def test_spacing_marks_join_their_letter(self):
        self.assertEqual(bt.graphemes("काम"), ["का", "म"])  # U+093E has combining class 0 but is a mark
        self.assertEqual(bt.letter_diff("कात", "काम", {}), ([1], [1]))

    def test_presentation_forms_and_bidi_marks(self):
        self.assertEqual(bt.clean("אַ‏"), "אַ")  # אַ as one presentation form, then RLM

    def test_point_differs(self):
        self.assertEqual(bt.letter_diff("אַל", "אל", {}), ([0], [0]))

    def test_ligature_equals_its_letters(self):
        self.assertEqual(bt.letter_diff("װעט", "וועט", YI), ([], []))
        self.assertEqual(bt.fold("װעט", YI), bt.fold("וועט", YI))

    def test_letter_next_to_ligature(self):
        # נאַלדשטײין (LoC) has one more yud than גאלדשטיין (the model).
        a, b = bt.letter_diff("נאַלדשטײין", "גאלדשטיין", YI)
        self.assertEqual(a, [0, 1, 7])
        self.assertEqual(b, [0, 1])


class Pairing(unittest.TestCase):
    def test_split_word_is_left_out_and_neighbours_pair(self):
        a = ["TB", "טעקאַנד", "עווע..", "ניו", "יאָרק"]
        b = ["0", "[", "פעקאָנך", "עווע.,", "נין", "יאָרק"]
        pairs, left = bt.pair_words(a, b, YI, 0.34)
        self.assertEqual(pairs, [(1, "פעקאָנך"), (2, "עווע.,"), (3, "נין")])
        self.assertEqual(left, 1)

    def test_equal_after_folding_is_no_task(self):
        self.assertEqual(bt.pair_words(["װעט"], ["וועט"], YI, 0.34), ([], 0))

    def test_split_and_joined_words_are_left_out(self):
        self.assertEqual(bt.pair_words(["שלום"], ["של", "ום"], {}, 0.34), ([], 1))
        self.assertEqual(bt.pair_words(["של", "ום"], ["שלום"], {}, 0.34), ([], 2))
        self.assertEqual(bt.pair_words(["זאָגט", "של", "ום"], ["זאגט", "שלום"], {}, 0.34), ([(0, "זאגט")], 2))

    def test_matched_neighbour_is_not_a_split(self):
        self.assertEqual(bt.pair_words(["איז", "זי"], ["אי", "זי"], {}, 0.34), ([(0, "אי")], 0))

    def test_stray_marks(self):
        self.assertTrue(bt.stray_marks("י", "ע", YI))
        self.assertFalse(bt.stray_marks("ײ", "י", YI))  # shares י; yiddish_lines.py drops such picture lines
        self.assertFalse(bt.stray_marks("אי", "אין", YI))

    def test_unlike_words_are_left_out(self):
        self.assertEqual(bt.pair_words(["ניבן"], ["ף"], YI, 0.34), ([], 1))


class Alto(unittest.TestCase):
    def test_line_end_hyphen_joins_its_word(self):
        import yiddish_lines as yl
        xml = """<alto xmlns="http://www.loc.gov/standards/alto/ns-v2#"><Layout><Page WIDTH="1000" HEIGHT="2000"><PrintSpace>
<TextBlock><TextLine HPOS="100" VPOS="50" WIDTH="400" HEIGHT="40">
<String HPOS="300" VPOS="55" WIDTH="200" HEIGHT="30" CONTENT="דער"/><SP/>
<String HPOS="120" VPOS="52" WIDTH="150" HEIGHT="35" CONTENT="פאר"/><HYP HPOS="100" VPOS="60" WIDTH="14" CONTENT="-"/>
</TextLine></TextBlock></PrintSpace></Page></Layout></alto>"""
        with tempfile.NamedTemporaryFile("w", suffix=".xml", delete=False, encoding="utf-8") as f:
            f.write(xml)
        try:
            w, h, lines = yl.alto_lines(f.name)
        finally:
            os.unlink(f.name)
        self.assertEqual((w, h), (1000, 2000))
        self.assertEqual(lines[0]["words"], [{"text": "דער", "box": [300, 55, 200, 30]}, {"text": "פאר-", "box": [100, 52, 170, 35]}])


class TextLines(unittest.TestCase):
    def test_picture_lines_are_not_text(self):
        import yiddish_lines as yl
        w = lambda t: {"text": t, "box": [0, 0, 1, 1]}
        self.assertFalse(yl.text_like({"words": [w(x) for x in "י י ע א . ייע SR י".split()], "wc": 0.9}))
        self.assertTrue(yl.text_like({"words": [w(x) for x in "צודרינגליך, ער SR מיר דא ערקלע-!".split()], "wc": 0.97}))
        self.assertTrue(yl.text_like({"words": [w(x) for x in ". 3750 תמון e 1922 TAMUZ".split()], "wc": 0.96}))


    def test_picture_hit_does_not_use_a_block(self):
        import yiddish_lines as yl
        def line(block, y, text, wc):
            return {"block": block, "box": [0, y, 100, 10], "words": [{"text": t, "box": [0, y, 1, 1]} for t in text.split()], "wc": wc}
        lines = [line(0, 0, "י י ע . א", 0.9), line(0, 10, "י י ע א", 0.9), line(1, 100, "דער מאן געקומען", 0.97),
                 line(1, 110, "צו זיין שוועסטער", 0.97)]
        self.assertEqual(yl.pick_blocks(lines, [(5, 5), (5, 105)], blocks=1), [2, 3])


class Build(unittest.TestCase):
    def test_end_to_end(self):
        try:
            from PIL import Image
        except ImportError:
            self.skipTest("Pillow not installed")
        with tempfile.TemporaryDirectory() as d:
            Image.new("L", (200, 40), 255).save(os.path.join(d, "l1.png"))
            line = {"id": "p1_l1", "image": "l1.png", "page": "sn1/1913-09-01/ed-1/seq-3", "source": "https://example.org/", "label": "L",
                    "a": "סעם נאַלד .", "a_words": [{"text": "סעם", "box": [150, 5, 40, 30]}, {"text": "נאַלד", "box": [60, 5, 80, 30]},
                                                   {"text": ".", "box": [10, 5, 5, 30]}],
                    "b": "סעם גאלד ,"}
            with open(os.path.join(d, "lines.jsonl"), "w", encoding="utf-8") as f:
                f.write(json.dumps(line, ensure_ascii=False) + "\n")
            cfg = {"name": "Yiddish", "dir": "rtl", "equivalents": {"װ": "וו"}}
            with open(os.path.join(d, "cfg.json"), "w", encoding="utf-8") as f:
                json.dump(cfg, f, ensure_ascii=False)
            out = os.path.join(d, "out")
            subprocess.run([sys.executable, os.path.join(os.path.dirname(__file__), "build_tasks.py"), "--lines", os.path.join(d, "lines.jsonl"),
                            "--images", d, "--lang", "yi", "--set", "t1", "--a-is", "loc", "--b-is", "yidj2",
                            "--language-config", os.path.join(d, "cfg.json"), "--out-dir", out], check=True, capture_output=True)
            tasks = [json.loads(x) for x in open(os.path.join(out, "tasks.jsonl"), encoding="utf-8")]
            self.assertEqual(len(tasks), 1)  # "." vs "," has no letters
            t = tasks[0]
            self.assertEqual((t["id"], t["a"], t["b"], t["a_diff"], t["b_diff"]), ("yi/t1/p1_l1~1", "נאַלד", "גאלד", [0, 1], [0, 1]))
            self.assertEqual(t["box"], [0.3, 0.125, 0.4, 0.75])
            self.assertTrue(os.path.exists(os.path.join(out, "clips", t["line"])))
            self.assertEqual(Image.open(os.path.join(out, "clips", t["word"])).size, (80 + 2 * 7, 40))
            lang = json.load(open(os.path.join(out, "language.json"), encoding="utf-8"))
            self.assertEqual(lang, {"name": "Yiddish", "dir": "rtl", "examples": {}, "equivalents": {"װ": "וו"}})


if __name__ == "__main__":
    unittest.main()
