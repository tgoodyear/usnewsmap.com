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


class Letters(unittest.TestCase):
    def test_marks_stay_with_their_letter(self):
        self.assertEqual(bt.graphemes("פּאַ"), ["פּ", "אַ"])

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

    def test_unlike_words_are_left_out(self):
        self.assertEqual(bt.pair_words(["ניבן"], ["ף"], YI, 0.34), ([], 1))


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
            self.assertEqual(lang, {"name": "Yiddish", "dir": "rtl", "examples": {}})


if __name__ == "__main__":
    unittest.main()
