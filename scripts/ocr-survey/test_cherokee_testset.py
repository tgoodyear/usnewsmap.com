"""scripts/ocr-survey/cherokee_testset.py: the Cherokee test set's alignment and scoring (#1620)."""

import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import cherokee_testset as ts  # noqa: E402

# A passage of the 1860 NT (Matthew 1:2), as pdftotext -raw gives it, with a hyphenated line break.
NT = """2 ᎡᏆᎭᎻ ᎡᏏᎩ ᎤᏕᏁᎴᎢ: ᎡᏏᎩᏃ ᏤᎦᏈ
ᎤᏕᏁᎴᎢ, ᏤᎦᏈᏃ ᏧᏓ ᎠᎴ ᎾᏍᎩ ᏧᏓ ᏧᏂᏙ-
ᎵᏍᏗ ᎤᏕᏁᎴᎢ;
3 ᏧᏓᏃ ᏇᎵᏏ ᎠᎴ ᏎᎳ ᎤᏕᏁᎴᎢ ᏖᎹ ᎤᏪᏥ ᎨᏎᎢ"""


def line(letters_text: str, line_id: str) -> tuple:
    letters = "".join(ts.SYLLABARY.findall(letters_text))
    return (line_id, {"HPOS": 0, "VPOS": 0, "WIDTH": 10, "HEIGHT": 2}, letters, letters_text)


class Stream(unittest.TestCase):
    def test_hyphenated_words_rejoin_and_words_are_marked(self):
        nt, starts = ts.nt_stream(NT)
        self.assertIn("ᏧᏂᏙᎵᏍᏗ", nt)
        self.assertEqual(len(starts), 21)
        self.assertEqual(ts.spaced(nt, starts, 0, len("ᎡᏆᎭᎻᎡᏏᎩ")), "ᎡᏆᎭᎻ ᎡᏏᎩ")


class Alignment(unittest.TestCase):
    def align(self, lines):
        nt, starts = ts.nt_stream(NT)
        return ts.align_block(lines, nt, starts, ts.gram_index(nt))

    def test_misread_letters_keep_the_line_with_the_nt_reference(self):
        rows = self.align([
            line("ᎡᏆᎭᎻ ᎡᏏᎩ ᎤᏕᏁᎴᎢ: ᎡᏏᎩᏃ ᏤᎦᏈ", "L1"),
            # ᎾᏍᎩ read as ᏩᏍᎩ, the commonest Phoenix misreading.
            line("ᎤᏕᏁᎴᎢ, ᏤᎦᏈᏃ ᏧᏓ ᎠᎴ ᏩᏍᎩ ᏧᏓ ᏧᏂᏙ-", "L2"),
            line("ᎵᏍᏗ ᎤᏕᏁᎴᎢ; ᏧᏓᏃ ᏇᎵᏏ ᎠᎴ ᏎᎳ", "L3"),
        ])
        by_id = {r["line_id"]: r for r in rows}
        self.assertIsNone(by_id["L2"]["drop"])
        self.assertEqual(by_id["L2"]["reference"], "ᎤᏕᏁᎴᎢ ᏤᎦᏈᏃ ᏧᏓ ᎠᎴ ᎾᏍᎩ ᏧᏓ ᏧᏂᏙ")
        self.assertEqual(by_id["L2"]["substitutions"], 1)
        self.assertIsNone(by_id["L1"]["drop"])

    def test_a_missing_word_is_a_wording_difference(self):
        rows = self.align([
            line("ᎡᏆᎭᎻ ᎡᏏᎩ ᎤᏕᏁᎴᎢ: ᎡᏏᎩᏃ ᏤᎦᏈ", "L1"),
            # The printing lacks ᎠᎴ ᎾᏍᎩ: two whole words.
            line("ᎤᏕᏁᎴᎢ, ᏤᎦᏈᏃ ᏧᏓ ᏧᏓ ᏧᏂᏙᎵᏍᏗ", "L2"),
            line("ᎤᏕᏁᎴᎢ; ᏧᏓᏃ ᏇᎵᏏ ᎠᎴ ᏎᎳ ᎤᏕᏁᎴᎢ", "L3"),
        ])
        by_id = {r["line_id"]: r for r in rows}
        self.assertIsNotNone(by_id["L2"]["drop"])

    def test_a_word_missing_at_a_line_break_is_counted(self):
        rows = self.align([
            # The printing lacks ᏤᎦᏈ at the end of the first line.
            line("ᎡᏆᎭᎻ ᎡᏏᎩ ᎤᏕᏁᎴᎢ: ᎡᏏᎩᏃ", "L1"),
            line("ᎤᏕᏁᎴᎢ, ᏤᎦᏈᏃ ᏧᏓ ᎠᎴ ᎾᏍᎩ ᏧᏓ ᏧᏂᏙ-", "L2"),
            line("ᎵᏍᏗ ᎤᏕᏁᎴᎢ; ᏧᏓᏃ ᏇᎵᏏ ᎠᎴ ᏎᎳ", "L3"),
        ])
        by_id = {r["line_id"]: r for r in rows}
        self.assertEqual(by_id["L1"]["deletions"], 3)
        self.assertTrue(by_id["L1"]["reference"].endswith("ᏤᎦᏈ"))
        self.assertIsNotNone(by_id["L1"]["drop"])
        self.assertIsNone(by_id["L2"]["drop"])

    def test_a_misread_letter_in_a_split_word_is_not_a_wording_difference(self):
        rows = self.align([
            line("ᎡᏆᎭᎻ ᎡᏏᎩ ᎤᏕᏁᎴᎢ: ᎡᏏᎩᏃ ᏤᎦᏈ", "L1"),
            # ᏧᏂᏙᎵᏍᏗ split after its first letter, which is misread as Ꮪ.
            line("ᎤᏕᏁᎴᎢ, ᏤᎦᏈᏃ ᏧᏓ ᎠᎴ ᎾᏍᎩ ᏧᏓ Ꮪ-", "L2"),
            line("ᏂᏙᎵᏍᏗ ᎤᏕᏁᎴᎢ; ᏧᏓᏃ ᏇᎵᏏ ᎠᎴ ᏎᎳ", "L3"),
        ])
        by_id = {r["line_id"]: r for r in rows}
        self.assertIsNone(by_id["L2"]["drop"])
        self.assertEqual(by_id["L2"]["substitutions"], 1)
        self.assertIsNone(by_id["L3"]["drop"])

    def test_text_that_isnt_scripture_aligns_nothing(self):
        self.assertEqual(self.align([line("ᏣᎳᎩ ᏧᎴᎯᏌᏅᎯ " * 4, "L1")]), [])


class Pieces(unittest.TestCase):
    def test_image_url(self):
        self.assertEqual(
            ts.image_url("sn83020874/1830-04-07/ed-1/seq-3"),
            "https://tile.loc.gov/image-services/iiif/service:ndnp:dlc:batch_dlc_misctopsn83020874_ver02:"
            "data:sn83020874:print:1830040701:0003/full/full/0/default.jpg")

    def test_page_id(self):
        self.assertEqual(ts.page_id("sn83020866/1828/03/06/ed-1/seq-4/ocr.xml"), "sn83020866/1828-03-06/ed-1/seq-4")

    def test_letter_edit_distance(self):
        self.assertEqual(ts.letter_edit_distance("ᎾᏍᎩ", "ᏩᏍᎩ"), 1)
        self.assertEqual(ts.letter_edit_distance("", "ᎠᎴ"), 2)
        self.assertEqual(ts.letter_edit_distance("ᎠᎴ", "ᎠᎴ"), 0)


if __name__ == "__main__":
    unittest.main()
