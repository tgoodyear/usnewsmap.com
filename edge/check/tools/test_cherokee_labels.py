"""Tests for cherokee_labels.py: python3 -m unittest discover -s edge/check/tools."""
import json
import os
import subprocess
import sys
import tempfile
import unicodedata
import unittest

sys.path.insert(0, os.path.dirname(__file__))
import cherokee_labels as cl  # noqa: E402
import lookalike as la  # noqa: E402

HERE = os.path.dirname(os.path.abspath(__file__))
LANGS = os.path.join(HERE, "..", "languages")


class Labels(unittest.TestCase):
    def test_every_syllabary_letter_is_named_from_its_unicode_name(self):
        want = cl.names()
        self.assertEqual(len(want), 86)
        self.assertEqual(list(want), [chr(c) for c in range(0x13A0, 0x13F6)])
        for ch, name in want.items():
            self.assertEqual("CHEROKEE LETTER " + name.upper(), unicodedata.name(ch), ch)
            self.assertEqual(name, la.label(ch), "the same derivation as the label stored with a flagged pair")
        self.assertEqual((want["Ꭰ"], want["Ꭱ"], want["Ꮢ"], want[unicodedata.lookup("CHEROKEE LETTER TLA")], want[unicodedata.lookup("CHEROKEE LETTER QUV")]),
                         ("a", "e", "sv", "tla", "quv"))

    def test_the_language_files_have_exactly_these_names(self):
        for f in cl.FILES:
            with open(os.path.join(LANGS, f), encoding="utf-8") as fh:
                self.assertEqual(json.load(fh)["labels"], cl.names(), f)
        subprocess.run([sys.executable, os.path.join(HERE, "cherokee_labels.py"), "--check"], check=True)

    def test_write_adds_sorted_labels_and_leaves_other_fields(self):
        with tempfile.TemporaryDirectory() as d:
            for f in cl.FILES:
                with open(os.path.join(d, f), "w", encoding="utf-8") as fh:
                    json.dump({"name": "X", "intro": "y", "labels": {"Ꭰ": "wrong"}}, fh)
            self.assertNotEqual(subprocess.run([sys.executable, os.path.join(HERE, "cherokee_labels.py"), "--check", "--languages-dir", d]).returncode, 0)
            subprocess.run([sys.executable, os.path.join(HERE, "cherokee_labels.py"), "--write", "--languages-dir", d], check=True)
            with open(os.path.join(d, "chr.json"), encoding="utf-8") as fh:
                got = json.load(fh)
            self.assertEqual(list(got), ["intro", "labels", "name"])
            self.assertEqual(got["labels"], cl.names())
            subprocess.run([sys.executable, os.path.join(HERE, "cherokee_labels.py"), "--check", "--languages-dir", d], check=True)


if __name__ == "__main__":
    unittest.main()
