"""scripts/ocr-survey/gold.py: selection, validation and CER (#296)."""

import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import gold  # noqa: E402


def pages(n: int, group: str = "g") -> list[dict]:
    out = []
    for i in range(n):
        pid = f"sn{group}_{1850 + i * 7}-01-01_ed1_seq{i + 1}"
        out.append({**gold.parse_page_id(pid), "page_id": pid, "group": group, "lang": "eng"})
    return out


class Select(unittest.TestCase):
    def test_quotas_share_equally_and_pass_on_the_rest(self):
        self.assertEqual(gold.quotas({"a": 15, "b": 15, "c": 5}, 30), {"a": 13, "b": 12, "c": 5})
        self.assertEqual(gold.quotas({"a": 2, "b": 2}, 10), {"a": 2, "b": 2})
        self.assertEqual(sum(gold.quotas({"a": 9, "b": 9, "c": 9}, 20).values()), 20)

    def test_seeded_and_one_crop_per_page(self):
        ps = pages(20) + pages(20, "h")
        a, b = gold.select(ps, 12, 1), gold.select(ps, 12, 1)
        self.assertEqual(a, b)
        self.assertEqual(len(a), 12)
        self.assertEqual(len({p["page_id"] for p in a}), 12)
        self.assertNotEqual([p["page_id"] for p in a], [p["page_id"] for p in gold.select(ps, 12, 2)])

    def test_picks_cover_the_eras(self):
        picked = gold.select(pages(30), 6, 1)
        self.assertEqual(len({p["era"] for p in picked}), 6)


class Score(unittest.TestCase):
    def test_normalization(self):
        self.assertEqual(gold.normalize("Die Zei-\ntung  ist\nda"), "Die Zeitung ist da")
        self.assertEqual(gold.normalize("a [?b] c[?]d", truth=True), "a b cd")

    def test_cer_and_wer(self):
        r = gold.cer_wer("the cat sat", "the cot sat")
        self.assertEqual((r["ref_chars"], r["char_edits"], r["ref_words"], r["word_edits"]), (11, 1, 3, 1))
        self.assertEqual(gold.cer_wer("abc", "")["char_edits"], 3)


class Validate(unittest.TestCase):
    def row(self, **kw):
        base = {"crop_id": "g-01", "transcription": "line one\nline two", "transcriber": "A", "reviewer": "B",
                "hints": {"texts": {"eng": "line wun\nline too"}}}
        return {**base, **kw}

    def test_complete_row_passes(self):
        self.assertEqual(gold.validate_rows([self.row()])[0], [])

    def test_empty_transcription_fails(self):
        self.assertTrue(gold.validate_rows([self.row(transcription=None)])[0])

    def test_hint_copy_fails_unless_reviewed(self):
        r = self.row(transcription="line wun line too")
        self.assertTrue(gold.validate_rows([r])[0])
        self.assertEqual(gold.validate_rows([{**r, "hint_copy_reviewed": True}])[0], [])

    def test_review_by_a_second_person(self):
        self.assertTrue(gold.validate_rows([self.row(reviewer=None)])[0])
        self.assertTrue(gold.validate_rows([self.row(reviewer="A")])[0])
        self.assertEqual(gold.validate_rows([self.row(reviewer=None)], require_review=False)[0], [])

    def test_unknown_share_and_markers(self):
        self.assertTrue(gold.validate_rows([self.row(transcription="a[?]" * 10)])[0])
        self.assertTrue(gold.validate_rows([self.row(transcription="abc [?de")])[0])


if __name__ == "__main__":
    unittest.main()
