import json
from pathlib import Path
import sys
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parent))
import ocr_stored_form as S

FIXTURES = Path(__file__).resolve().parents[2] / "crates/usnm-core/tests/fixtures/stored_form.json"


def distance(a, b):
    prev = list(range(len(b) + 1))
    for i, x in enumerate(a, 1):
        cur = [i]
        for j, y in enumerate(b, 1):
            cur.append(min(prev[j] + 1, cur[-1] + 1, prev[j - 1] + (x != y)))
        prev = cur
    return prev[-1]


class StoredForm(unittest.TestCase):
    def test_matches_the_rust_fixtures(self):
        cases = json.loads(FIXTURES.read_text())
        self.assertTrue(cases)
        for c in cases:
            with self.subTest(c["name"]):
                self.assertEqual(S.stored_form(c["raw"], c["engine"], c["language"]), c["stored"])

    def test_full_tag_profile(self):
        tag = "tesseract/5.5.0 profile=frak2021 image=sha256:0123456789ab models=local route=r r=1"
        self.assertTrue(S.is_fraktur(tag, "eng"))
        self.assertFalse(S.is_fraktur(tag.replace("frak2021", "eng"), "eng"))
        self.assertTrue(S.is_fraktur(tag.replace("frak2021", "deu_latf+deu"), "eng"))

    def test_a_hyphenated_fraktur_line_compares_equal_only_in_stored_form(self):
        engine = "tesseract-5.5.1/frak2021"
        reread, published = "Die Zei⸗\ntung kam an.", "Die Zeitung kam an."
        self.assertGreater(distance(reread, published), 0)
        a, b = S.comparable(reread, published, engine, "deu")
        self.assertEqual(a, b)
        self.assertEqual(distance(a, b), 0)

    def test_a_raw_text_and_its_published_form_compare_equal(self):
        raw = "a\ufb01\u0301n"
        published = S.stored_form(raw, "tesseract-5.5.1/eng", "eng")
        a, b = S.comparable(raw, published, "tesseract-5.5.1/eng", "eng")
        self.assertEqual(a, b)

    def test_published_text_is_unchanged(self):
        published = "Die Zeitung kam an.\nZweite Zeile"
        self.assertEqual(S.stored_form(published, "tesseract-5.5.1/frak2021", "deu"), published)


if __name__ == "__main__":
    unittest.main()
