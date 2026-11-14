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
        # a transposition differs at both positions, though an alignment sees one letter moved
        self.assertEqual(la.differing_pairs("ᎡᏒ", "ᏒᎡ", {}), [(0, 0, "Ꭱ", "Ꮢ"), (1, 1, "Ꮢ", "Ꭱ")])
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
    def test_a_transposition_is_flagged_at_both_positions(self):
        F = la.Fonts()
        flagged = la.flags("ᎡᏒ", "ᏒᎡ", {}, F, threshold=0.7)
        self.assertEqual([(f["a"], f["a_label"], f["b"], f["b_label"]) for f in flagged], [(0, "e", 0, "sv"), (1, "sv", 1, "e")])

    @need_font
    def test_a_letter_the_font_lacks_is_counted_not_guessed(self):
        F = la.Fonts({"HEBREW": la.BUNDLED["CHEROKEE"]})  # a font with no Hebrew
        self.assertEqual(la.flags("אל", "בל", {}, F, threshold=0.0), [])
        self.assertEqual(F.unmeasured["HEBREW"], 1)

    @need_font
    def test_a_letter_with_a_mark_is_not_measured_without_shaping(self):
        """The basic layout engine sets a mark beside its letter, so such a pair is counted, not measured, without Raqm."""
        F = la.Fonts({"HEBREW": la.BUNDLED["CHEROKEE"]})
        F.raqm = False
        self.assertIsNone(la.measure(F, "אַ", "א"))
        self.assertEqual(F.unshaped["HEBREW"], 1)
        self.assertEqual(F.unmeasured["HEBREW"], 0)
        self.assertTrue(la.has_mark("אַ") and la.has_mark("פּ") and not la.has_mark("א"))
        self.assertIn("NOT checked", la.unshaped_warning(F))
        self.assertEqual(la.unshaped_warning(la.Fonts()), "")
        # letters of one character don't need shaping
        self.assertIsNotNone(la.measure(F, "Ꭱ", "Ꮢ"))

    TNR = "/System/Library/Fonts/Supplemental/Times New Roman.ttf"

    @unittest.skipUnless(HAVE_PIL and os.path.exists(TNR) and HAVE_PIL and __import__("PIL.features").features.check("raqm"), "needs Raqm and macOS's Times New Roman")
    def test_a_letter_with_its_mark_is_compared_as_one_shaped_letter(self):
        F = la.Fonts({"HEBREW": self.TNR})
        near = la.measure(F, "אַ", "אָ")  # patah and qamats under the same alef
        self.assertGreater(near, 0.8)
        self.assertLess(near, 1.0)
        self.assertEqual(F.unshaped["HEBREW"], 0)
        self.assertLess(la.measure(F, "ר", "ד"), 0.6)

    @need_font
    def test_page_size_matches_the_stylesheet(self):
        css = open(os.path.join(HERE, "..", "public", "style.css"), encoding="utf-8").read()
        self.assertRegex(css, r"\.script \{[^}]*font-size: 2rem;")
        self.assertEqual(la.PAGE_PX, 32)


class MarkOnly(unittest.TestCase):
    PATAH, QAMATS, DAGESH, RAFE, SHIN_DOT, SIN_DOT = "\u05b7", "\u05b8", "\u05bc", "\u05bf", "\u05c1", "\u05c2"

    @unittest.skipUnless(HAVE_PIL, "Pillow not installed")
    def test_same_base_differing_only_in_marks(self):
        al, pe = "\u05d0", "\u05e4"
        self.assertEqual(la.mark_only(al + self.PATAH, al + self.QAMATS), (al, self.PATAH, self.QAMATS))
        self.assertEqual(la.mark_only(al, al + self.PATAH), (al, "", self.PATAH))  # one may have none
        self.assertEqual(la.mark_only(pe + self.DAGESH, pe + self.RAFE), (pe, self.DAGESH, self.RAFE))
        # every script, not only Hebrew: a Latin letter with and without a dot above (q has no precomposed form)
        self.assertEqual(la.mark_only("q\u0307", "q"), ("q", "\u0307", ""))
        # a different base letter keeps the overlap check; identical letters are no pair
        self.assertIsNone(la.mark_only(al + self.PATAH, pe + self.PATAH))
        self.assertIsNone(la.mark_only("\u05e8", "\u05d3"))
        self.assertIsNone(la.mark_only(al + self.PATAH, al + self.PATAH))

    @unittest.skipUnless(HAVE_PIL, "Pillow not installed")
    def test_which_marks_differ(self):
        pair = {"a_marks": self.SHIN_DOT + self.DAGESH, "b_marks": self.SIN_DOT + self.DAGESH}
        self.assertEqual(la.differing_marks(pair), [self.SHIN_DOT, self.SIN_DOT])
        self.assertEqual(la.differing_marks({"a_marks": "", "b_marks": self.PATAH}), [self.PATAH])

    @unittest.skipUnless(HAVE_PIL, "Pillow not installed")
    def test_analyse_records_mark_pairs_apart_from_overlap_flags(self):
        F = la.Fonts({"HEBREW": la.BUNDLED["CHEROKEE"]})  # no Hebrew glyphs: a mark pair must not need a font
        alike, marks = la.analyse("\u05e4\u05bc\u05d0\u05b7\u05dc", "\u05e4\u05bf\u05d0\u05b8\u05dc", {}, F)
        self.assertEqual(alike, [])
        self.assertEqual(marks, [{"a": 0, "a_marks": self.DAGESH, "b": 0, "b_marks": self.RAFE, "base": "\u05e4", "differ": self.DAGESH + self.RAFE},
                                 {"a": 1, "a_marks": self.PATAH, "b": 1, "b_marks": self.QAMATS, "base": "\u05d0", "differ": self.PATAH + self.QAMATS}])
        self.assertEqual(list(marks[0]), sorted(marks[0]), "keys in ascending order")
        self.assertEqual(sum(F.unmeasured.values()) + sum(F.unshaped.values()), 0, "mark pairs are not measured")
        # the old call sees only the overlap-flagged pairs
        self.assertEqual(la.flags("\u05e4\u05bc", "\u05e4\u05bf", {}, F), [])

    YI = {"\u05f2\u05b7": "\u05d9\u05d9\u05b7", "\u05f2": "\u05d9\u05d9", "\u05f1": "\u05d5\u05d9", "\u05f0": "\u05d5\u05d5"}

    def yi_analyse(self, a, b):
        eq = bt.parse_equivalents([f"{k}={v}" for k, v in self.YI.items()])
        return la.analyse(a, b, eq, la.Fonts({"HEBREW": la.BUNDLED["CHEROKEE"]}))

    @unittest.skipUnless(HAVE_PIL, "Pillow not installed")
    def test_a_pointed_double_yod_is_labelled_with_its_point(self):
        """ײַ is ייַ, so שײַן against שיַיַן differs in a yud that has a point on one side; the displayed ײַ has a point, so its
        label must not be "no mark" (#1951)."""
        a, b = "\u05e9\u05f2\u05b7\u05df", "\u05e9\u05d9\u05b7\u05d9\u05b7\u05df"
        alike, marks = self.yi_analyse(a, b)
        self.assertEqual(alike, [])
        self.assertEqual(marks, [{"a": 1, "a_marks": self.PATAH, "b": 1, "b_marks": self.PATAH, "base": "\u05d9", "differ": self.PATAH}])

    @unittest.skipUnless(HAVE_PIL, "Pillow not installed")
    def test_a_pair_on_the_displayed_letters_survives_expansion_that_changes_the_length(self):
        """ײַ is two letters expanded but ײָ is not listed, so אײַב against אײָב has readings of different lengths once
        expanded; the displayed letters are one pair of one base letter that differs in its point."""
        a, b = "\u05d0\u05f2\u05b7\u05d1", "\u05d0\u05f2\u05b8\u05d1"
        alike, marks = self.yi_analyse(a, b)
        self.assertEqual(alike, [])
        self.assertEqual(marks, [{"a": 1, "a_marks": self.PATAH, "b": 1, "b_marks": self.QAMATS, "base": "\u05f2", "differ": self.PATAH + self.QAMATS}])
        self.assertEqual(la.differing_pairs(a, b, bt.parse_equivalents([f"{k}={v}" for k, v in self.YI.items()])), [], "the expanded readings pair nothing")

    @unittest.skipUnless(HAVE_PIL, "Pillow not installed")
    def test_other_configured_equivalents_keep_the_displayed_marks(self):
        # a ligature with and without a point (only ײַ is listed as an equivalent, so the others stay whole letters when
        # pointed and expand when not): one pair of one base letter, whichever way the expansion goes
        for lig in ("\u05f0", "\u05f1", "\u05f2"):
            for plain, pointed, want in ((lig, lig + self.PATAH, ("", self.PATAH)), (lig + self.QAMATS, lig, (self.QAMATS, ""))):
                alike, marks = self.yi_analyse("\u05d0" + plain + "\u05d1", "\u05d0" + pointed + "\u05d1")
                self.assertEqual(alike, [], lig)
                self.assertEqual([(m["a"], m["a_marks"], m["b"], m["b_marks"]) for m in marks], [(1, want[0], 1, want[1])], lig)
        # no equivalent in play: plain pairs, as before
        self.assertEqual(self.yi_analyse("\u05d0\u05b7", "\u05d0")[1], [{"a": 0, "a_marks": self.PATAH, "b": 0, "b_marks": "", "base": "\u05d0", "differ": self.PATAH}])
        # Serbian's equivalents are one letter for one letter and have no marks: unchanged
        sr = bt.parse_equivalents(["C=\u0421"])
        self.assertEqual(la.analyse("C\u0435", "\u0421\u0435", sr, la.Fonts())[1], [])

    @unittest.skipUnless(HAVE_PIL, "Pillow not installed")
    def test_marks_that_survive_nfc_in_any_script_and_precomposed_letters_stay_apart(self):
        F = la.Fonts({"LATIN": la.BUNDLED["CHEROKEE"]})
        _, marks = la.analyse("q\u0307a", "qa", {}, F)
        self.assertEqual([(m["a"], m["a_marks"], m["b_marks"], m["base"]) for m in marks], [(0, "\u0307", "", "q")])
        # a letter the build keeps precomposed (the readings are NFC) is its own base letter, as Serbian's и and й are:
        # it keeps the overlap check, so Serbian and the other precomposed scripts behave as before
        self.assertEqual(la.analyse("\u0439", "\u0438", {}, F)[1], [])
        self.assertEqual(la.analyse("\u00e9", "e", {}, F)[1], [])

    @need_font
    def test_cherokee_and_cyrillic_are_unchanged(self):
        F = la.Fonts()
        self.assertEqual(la.analyse("ᏒᎯ", "ᎡᎯ", {}, F, threshold=0.7)[1], [])
        (flag,) = la.analyse("ᏒᎯ", "ᎡᎯ", {}, F, threshold=0.7)[0]
        self.assertEqual((flag["a"], flag["a_label"], flag["b"], flag["b_label"]), (0, "sv", 0, "e"))
        self.assertEqual(la.analyse("пс", "нс", {}, la.Fonts({"CYRILLIC": la.BUNDLED["CHEROKEE"]}))[1], [])
        self.assertEqual(la.analyse("ᏒᎯ", "ᎡᎯ", {}, F, threshold=0.7)[0], la.flags("ᏒᎯ", "ᎡᎯ", {}, F, threshold=0.7))


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
            self.assertEqual((r["mark_only_tasks"], r["mark_only_pairs"], r["mark_only_by_mark"]), (0, 0, {}))

    @unittest.skipUnless(HAVE_PIL, "Pillow not installed")
    def test_the_command_counts_mark_only_pairs_and_tasks_per_language(self):
        with tempfile.TemporaryDirectory() as d:
            export = os.path.join(d, "export.jsonl")
            rows = [{"id": "yi/t/1", "a": "\u05e4\u05bc\u05d0\u05b7", "b": "\u05e4\u05bf\u05d0\u05b8"},  # two mark pairs
                    {"id": "yi/t/2", "a": "\u05d0\u05b7", "b": "\u05d0"},
                    {"id": "yi/t/3", "a": "\u05e8", "b": "\u05d3"}]
            with open(export, "w", encoding="utf-8") as f:
                f.write("".join(json.dumps(r, ensure_ascii=False) + "\n" for r in rows))
            out = json.loads(subprocess.run([sys.executable, os.path.join(HERE, "lookalike.py"), "--export", export, "--json"],
                                            check=True, capture_output=True, text=True).stdout)["languages"]["yi"]
            self.assertEqual((out["tasks"], out["mark_only_tasks"], out["mark_only_pairs"]), (3, 2, 3))
            self.assertEqual(out["mark_only_by_mark"], {"U+05B7": 2, "U+05B8": 1, "U+05BC": 1, "U+05BF": 1})


if __name__ == "__main__":
    unittest.main()
