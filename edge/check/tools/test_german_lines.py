"""Tests for german_lines.py: python3 -m unittest discover -s edge/check/tools."""
import contextlib
import io
import json
import os
import sys
import tempfile
import unittest

sys.path.insert(0, os.path.dirname(__file__))
import german_lines as gl  # noqa: E402
from test_serbian_lines import alto  # noqa: E402

HERE = os.path.dirname(__file__)


def line(y, words):
    """A LoC-style line of (text, x) words, 100 wide each, 40 high, from y."""
    return ((0, y, 100 * len(words), 40), [(t, (100 * i, y, 90, 40)) for i, (t, _) in enumerate(words)])


class Share(unittest.TestCase):
    def test_latin_share_counts_letters_only(self):
        self.assertEqual(gl.latin_share("Straße 22"), 1.0)
        self.assertEqual(gl.latin_share("ſchöne Grüße"), 1.0)
        self.assertEqual(gl.latin_share("12 - 3"), 0.0)
        self.assertAlmostEqual(gl.latin_share("abc Привет"), 3 / 9)
        self.assertEqual(gl.latin_share("אבג"), 0.0)

    def test_is_german_needs_ninety_percent(self):
        self.assertTrue(gl.is_german("die Zeitung erſcheint täglich"))
        self.assertTrue(gl.is_german("aaaaaaaaaя"))  # 9 of 10
        self.assertFalse(gl.is_german("aaaaaaaaяя"))  # 8 of 10
        self.assertFalse(gl.is_german("1906"))

    def test_fractions_are_not_letters(self):
        self.assertEqual(gl.latin_share("Nur ½ Mark"), 1.0)
        self.assertTrue(gl.is_german("Preis 2½ Cents"))

    def test_candidates_need_words_and_latin(self):
        lines = [{"words": [{"text": t} for t in words]} for words in
                 (["Die", "Stadt", "Ohio"], ["Страна", "Нови", "Сад"], ["Nr.", "22"])]
        self.assertEqual(gl.candidates(lines, 3, accept=gl.is_german), [0])


class Run(unittest.TestCase):
    def test_end_to_end_without_network(self):
        try:
            from PIL import Image
        except ImportError:
            self.skipTest("Pillow not installed")
        loc_lines = [line(10, [("Die", 0), ("Stadt", 0), ("Ohio", 0)]),
                     line(70, [("Страна", 0), ("Нови", 0), ("Сад", 0)]),
                     line(130, [("Nr.", 0), ("22", 0)])]
        ours_lines = [line(10, [("Dic", 0), ("Stadt", 0), ("Ohio", 0)]),
                      line(70, [("Страна", 0), ("Нови", 0), ("Сад", 0)])]
        with tempfile.TemporaryDirectory() as tmp:
            ours, work, scans = (os.path.join(tmp, d) for d in ("ours", "work", "scans"))
            for d in (ours, work, scans):
                os.makedirs(d)
            doc, page = "sn1_1910-01-02_ed-1_seq-3", "sn1/1910-01-02/ed-1/seq-3"
            with open(os.path.join(ours, doc + ".xml"), "w", encoding="utf-8") as f:
                f.write(alto(2000, 400, [((x, y * 2, w * 2, h * 2), [(t, (bx * 2, by * 2, bw * 2, bh * 2)) for t, (bx, by, bw, bh) in ws])
                                         for (x, y, w, h), ws in ours_lines]))
            os.makedirs(os.path.join(work, "loc"))
            loc = os.path.join(work, "loc", doc + ".xml")
            with open(loc, "w", encoding="utf-8") as f:
                f.write(alto(1000, 200, loc_lines))
            with open(loc + ".url", "w") as f:  # the cached download, so nothing is fetched
                f.write("https://tile.loc.gov/x/001.xml")
            Image.new("L", (500, 100), 200).save(os.path.join(scans, "s.png"))
            pages = os.path.join(tmp, "pages.jsonl")
            with open(pages, "w", encoding="utf-8") as f:
                f.write(json.dumps({"doc_id": doc, "page": page, "alto": "https://tile.loc.gov/x/001.xml",
                                    "title": "Der Beobachter", "scan": "s.png"}) + "\n")
            argv = ["german_lines.py", "--pages", pages, "--ours", ours, "--work", work, "--scans", scans,
                    "--lines-per-page", "4", "--min-page-lines", "99"]
            out = io.StringIO()
            old = sys.argv
            sys.argv = argv
            try:
                with contextlib.redirect_stdout(out):
                    gl.main()
            finally:
                sys.argv = old
            with open(os.path.join(work, "lines.jsonl"), encoding="utf-8") as f:
                rows = [json.loads(r) for r in f]
            self.assertEqual(len(rows), 1)
            self.assertEqual(rows[0]["a"], "Die Stadt Ohio")
            self.assertEqual(rows[0]["b"], "Dic Stadt Ohio")
            self.assertTrue(os.path.exists(os.path.join(work, rows[0]["image"])))
            self.assertIn("1 lines", out.getvalue())

            # a page with no German line says so
            with open(loc, "w", encoding="utf-8") as f:
                f.write(alto(1000, 200, [loc_lines[1]]))
            out = io.StringIO()
            sys.argv = argv
            try:
                with contextlib.redirect_stdout(out):
                    gl.main()
            finally:
                sys.argv = old
            self.assertIn(f"{doc}: no German lines to check", out.getvalue())


class Config(unittest.TestCase):
    def test_equivalents_fold_long_s_and_friends(self):
        import build_tasks as bt
        with open(os.path.join(HERE, "..", "languages", "de.json"), encoding="utf-8") as f:
            config = json.load(f)
        eq = bt.parse_equivalents([f"{k}={v}" for k, v in config["equivalents"].items()])
        self.assertEqual(bt.letter_diff("ſchön", "schön", eq), ([], []))
        self.assertEqual(bt.letter_diff("ſchon", "schön", eq), ([3], [3]))


if __name__ == "__main__":
    unittest.main()
