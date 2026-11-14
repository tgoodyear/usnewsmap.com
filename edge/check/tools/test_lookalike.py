"""Tests for lookalike.py: python3 -m unittest discover -s edge/check/tools. They need Pillow and the bundled Cherokee font."""
import json
import os
import subprocess
import sys
import tempfile
import unittest

sys.path.insert(0, os.path.dirname(__file__))
import build_tasks as bt  # noqa: E402

try:
    from PIL import Image, ImageChops  # noqa: F401
    import lookalike as la
    HAVE_PIL = True
except ImportError:
    HAVE_PIL = False

HERE = os.path.dirname(os.path.abspath(__file__))
PLANTAGENET = "/System/Library/Fonts/Supplemental/PlantagenetCherokee.ttf"  # macOS's Cherokee font: the old fallback


def need_font(test):
    """The bundled font is a woff2: Pillow reads it only when its FreeType was built with Brotli."""
    def run(self):
        if not HAVE_PIL:
            self.skipTest("Pillow not installed")
        try:
            la.Fonts().bitmap(la.BUNDLED["CHEROKEE"], "Ꭰ")
        except OSError as e:
            self.skipTest(f"this Pillow can't read the bundled woff2: {e}")
        test(self)
    return run


class Labels(unittest.TestCase):
    @unittest.skipUnless(HAVE_PIL, "Pillow not installed")
    def test_cherokee_labels_are_the_syllable(self):
        self.assertEqual(la.label("Ꮢ"), "sv")
        self.assertEqual(la.label("Ꭱ"), "e")
        self.assertEqual(la.label("Ꮎ"), "na")

    @unittest.skipUnless(HAVE_PIL, "Pillow not installed")
    def test_other_scripts_use_the_unicode_name(self):
        self.assertEqual(la.label("п"), "cyrillic small letter pe")
        self.assertEqual(la.label("אַ"), "hebrew letter alef + hebrew point patah")
        self.assertEqual(la.label("ꭰ"), "cherokee small letter a")  # not a capital syllable: the full name

    @unittest.skipUnless(HAVE_PIL, "Pillow not installed")
    def test_scripts(self):
        self.assertEqual([la.script_of(x) for x in ("Ꭱ", "אַ", "п", "e", "3", ".")], ["CHEROKEE", "HEBREW", "CYRILLIC", "LATIN", "", ""])


class Pairs(unittest.TestCase):
    @unittest.skipUnless(HAVE_PIL, "Pillow not installed")
    def test_letters_that_differ_at_the_same_position(self):
        self.assertEqual(la.differing_pairs("ᏒᎯ", "ᎡᎯ", {}), [(0, 0, "Ꮢ", "Ꭱ")])
        self.assertEqual(la.differing_pairs("ᎠᏒᎯ", "ᎠᎡᎯ", {}), [(1, 1, "Ꮢ", "Ꭱ")])
        # a letter one reading has and the other lacks is not a pair, and neither are stretches of unequal length
        self.assertEqual(la.differing_pairs("ᎠᏒ", "ᎠᏒᎯ", {}), [])
        self.assertEqual(la.differing_pairs("ᎠᏒᎯ", "ᎠᎡ", {}), [])

    @unittest.skipUnless(HAVE_PIL, "Pillow not installed")
    def test_equivalent_spellings_are_not_pairs_and_marks_stay_with_their_letter(self):
        yi = bt.parse_equivalents(["װ=וו"])
        self.assertEqual(la.differing_pairs("װעט", "וועט", yi), [])
        self.assertEqual(la.differing_pairs("אַל", "אָל", {}), [(0, 0, "אַ", "אָ")])
        self.assertEqual(la.differing_pairs("מ" + "ל", "פּל", {}), [(0, 0, "מ", "פּ")])


class Measure(unittest.TestCase):
    """The table the threshold comes from. Overlap (intersection over union of the inked pixels, best of one-pixel
    shifts) of letter pairs drawn at 32px in the bundled font. Different: the letters differ plainly. Hard: the letters
    the type thesis (docs/reports/2026-11-cherokee/report.md, section 4) lists as told apart only by a serif or a terminal.
    The bounds are loose so another FreeType version doesn't flip them."""

    DIFFERENT = ["ᎠᎡ", "ᎠᎪ", "ᏣᏤ", "ᎣᎤ", "ᏢᏅ", "ᏣᎳ", "ᎣᎠ"]  # each below 0.65 (0.52 and under)
    @need_font
    def test_known_different_pairs_are_far_apart(self):
        F = la.Fonts()
        for p in self.DIFFERENT:
            self.assertLess(la.measure(F, p[0], p[1]), 0.65, p)

    @need_font
    def test_hard_pairs_are_close_but_under_the_threshold(self):
        F = la.Fonts()
        for p in ("ᎡᏒ", "ᏟᏣ"):
            self.assertTrue(0.65 <= la.measure(F, p[0], p[1]) < la.THRESHOLD, p)

    @need_font
    def test_same_picture_is_same(self):
        F = la.Fonts()
        bm = F.bitmap(la.BUNDLED["CHEROKEE"], "Ꮢ")
        self.assertEqual(la.iou(bm, bm), 1.0)
        self.assertEqual(la.iou(bm, ImageChops.offset(bm, 1, 0)), 1.0)  # a one-pixel shift is the same shape
        self.assertEqual(la.measure(F, "Ꮢ", "Ꮢ"), 1.0)
        self.assertLess(la.iou(bm, ImageChops.offset(bm, 3, 0)), 0.9)  # a three-pixel shift is not

    @need_font
    def test_no_pair_of_cherokee_letters_reaches_the_threshold_in_the_bundled_font(self):
        F = la.Fonts()
        letters = [chr(c) for c in range(0x13A0, 0x13F5)]
        top = max((la.measure(F, x, y), x, y) for i, x in enumerate(letters) for y in letters[i + 1:])
        self.assertLess(top[0], la.THRESHOLD, top)

    @unittest.skipUnless(HAVE_PIL and os.path.exists(PLANTAGENET), "needs macOS's Plantagenet Cherokee")
    def test_the_old_fallback_font_draws_sv_and_e_alike(self):
        F = la.Fonts({"CHEROKEE": PLANTAGENET})
        self.assertGreaterEqual(la.measure(F, "Ꭱ", "Ꮢ"), la.THRESHOLD)
        self.assertGreaterEqual(la.measure(F, "Ꮘ", "Ꮱ"), la.THRESHOLD)
        self.assertLess(la.measure(F, "Ꭰ", "Ꭱ"), 0.65)
        self.assertEqual([p["a_label"] + "/" + p["b_label"] for p in la.flags("ᏒᎯ", "ᎡᎯ", {}, F)], ["sv/e"])


class Flags(unittest.TestCase):
    @need_font
    def test_the_real_case(self):
        F = la.Fonts()
        self.assertEqual(la.flags("ᏒᎯ", "ᎡᎯ", {}, F), [])  # the bundled font tells them apart
        (flag,) = la.flags("ᏒᎯ", "ᎡᎯ", {}, F, threshold=0.7)
        self.assertEqual(list(flag), ["a", "a_label", "b", "b_label", "iou"])  # keys in ascending order
        self.assertEqual((flag["a"], flag["a_label"], flag["b"], flag["b_label"]), (0, "sv", 0, "e"))
        self.assertTrue(0.7 <= flag["iou"] < la.THRESHOLD)

    @need_font
    def test_a_letter_the_font_lacks_is_counted_not_guessed(self):
        F = la.Fonts({"HEBREW": la.BUNDLED["CHEROKEE"]})  # a font with no Hebrew
        self.assertEqual(la.flags("אל", "בל", {}, F, threshold=0.0), [])
        self.assertEqual(F.unmeasured["HEBREW"], 1)

    @need_font
    def test_page_size_matches_the_stylesheet(self):
        css = open(os.path.join(HERE, "..", "public", "style.css"), encoding="utf-8").read()
        self.assertRegex(css, r"\.script \{[^}]*font-size: 2rem;")
        self.assertEqual(la.PAGE_PX, 32)


class Commands(unittest.TestCase):
    @need_font
    def test_build_records_flags_and_the_command_counts_them(self):
        with tempfile.TemporaryDirectory() as d:
            Image.new("L", (200, 40), 255).save(os.path.join(d, "l1.png"))
            words = [{"text": "ᏒᎯ", "box": [150, 5, 40, 30]}, {"text": "ᎠᎴ", "box": [60, 5, 80, 30]}]
            line = {"id": "p1_l1", "image": "l1.png", "page": "sn1/1831-01-22/ed-1/seq-3", "a": "ᏒᎯ ᎠᎴ", "a_words": words, "b": "ᎡᎯ ᎠᏗ"}
            for name, body in (("lines.jsonl", json.dumps(line, ensure_ascii=False) + "\n"), ("cfg.json", json.dumps({"name": "Cherokee"}))):
                with open(os.path.join(d, name), "w", encoding="utf-8") as f:
                    f.write(body)
            tasks = {}
            for name, extra in (("default", []), ("strict", ["--lookalike-threshold", "0.7"])):
                out = os.path.join(d, "out-" + name)
                subprocess.run([sys.executable, os.path.join(HERE, "build_tasks.py"), "--lines", os.path.join(d, "lines.jsonl"), "--images", d,
                                "--lang", "chr", "--set", "t1", "--a-is", "loc", "--b-is", "ours", "--language-config", os.path.join(d, "cfg.json"),
                                "--out-dir", out, *extra], check=True, capture_output=True)
                tasks[name] = [json.loads(x) for x in open(os.path.join(out, "tasks.jsonl"), encoding="utf-8")]
            self.assertEqual(["lookalikes" in t for t in tasks["default"]], [False, False])
            self.assertEqual(["lookalikes" in t for t in tasks["strict"]], [True, False])
            self.assertEqual([(p["a"], p["a_label"], p["b"], p["b_label"]) for p in tasks["strict"][0]["lookalikes"]], [(0, "sv", 0, "e")])
            # the same readings through the command that reads an export, with no rebuild
            export = os.path.join(d, "export.jsonl")
            with open(export, "w", encoding="utf-8") as f:
                for t in tasks["strict"]:
                    f.write(json.dumps({"id": t["id"], "a": t["a"], "b": t["b"]}, ensure_ascii=False) + "\n")
            run = lambda *extra: json.loads(subprocess.run([sys.executable, os.path.join(HERE, "lookalike.py"), "--export", export, "--json", "--ids",
                                                           *extra], check=True, capture_output=True, text=True).stdout)
            self.assertEqual(run()["languages"]["chr"]["tasks_flagged"], 0)
            r = run("--threshold", "0.7")["languages"]["chr"]
            self.assertEqual((r["tasks"], r["tasks_flagged"], r["flagged_pairs"], r["pairs"], r["ids"]), (2, 1, 1, {"Ꮢ/Ꭱ": 1}, ["chr/t1/p1_l1~0"]))


if __name__ == "__main__":
    unittest.main()
